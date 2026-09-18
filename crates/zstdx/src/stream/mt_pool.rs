//! Pooled worker threads leased to one encoder queue at a time, plus the
//! global pool of reusable compression states they carry between leases.
//!
//! Split out of [`super::encoder_mt`] so the lease/park/retire mechanics
//! (lock order, idle timeout, panic retirement) stand alone from the job
//! semantics; the queue itself stays with the encoder.

use alloc::vec::Vec;
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::encoder_mt::{QueueShared, serve_queue};
use crate::encoding::{
    frame_compressor::{CompressState, new_slice_state},
    match_generator::MatchGeneratorDriver,
};

// Reusable worker states, global across encoders. The thread leases (see
// `THREAD_POOL`) carry their state across encoders now, but a lease's
// thread still starts cold whenever the pool misses or a worker retires,
// and the inline-job path borrows from here — so states pool globally.
// Every job clears what it reads, so state provenance cannot reach the
// bytes. Depth-capped so one-off worker counts do not pin memory forever.
static STATE_POOL: std::sync::OnceLock<
    Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
> = std::sync::OnceLock::new();
const STATE_POOL_DEPTH: usize = 32;

pub(super) fn take_pooled_state() -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    let pool = STATE_POOL.get_or_init(|| Mutex::new(Vec::new()));
    pool.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop()
        .unwrap_or_else(new_slice_state_boxed)
}

pub(super) fn return_pooled_state(state: alloc::boxed::Box<CompressState<MatchGeneratorDriver>>) {
    let pool = STATE_POOL.get_or_init(|| Mutex::new(Vec::new()));
    let mut pool = pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pool.len() < STATE_POOL_DEPTH {
        pool.push(state);
    }
}

fn new_slice_state_boxed() -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    alloc::boxed::Box::new(new_slice_state())
}

/// One parked pool thread's handoff slot: the thread parks here between
/// encoder leases, and the next encoder's `ensure_workers` wakes it with a
/// lease on its queue.
pub(super) struct WorkerSlot {
    pub(super) state: Mutex<WorkerSlotState>,
    pub(super) wake: Condvar,
}

pub(super) enum WorkerSlotState {
    /// Parked in the pool, waiting for the next lease.
    Parked,
    /// Serving the leased queue.
    Serving(Arc<QueueShared>),
    /// The thread is exiting: its lease panicked, the pool was full at
    /// repark, or the idle timeout fired.
    Retiring,
}

// Parked worker threads, global across encoders: a fresh encoder otherwise
// pays the whole worker spawn+join per stream — ~130 us for eight
// default-stack threads on this class of machine (clone plus stack
// guard-page setup dominates; the queue itself is a condvar either way),
// against ~10 us for a slot handoff. Depth-capped like the state pool; a
// parked thread retires after `THREAD_POOL_IDLE`, so a one-off encoder
// does not pin threads into a long-running process. Lock order: the pool
// mutex and a slot's state mutex are never held together, so none of the
// park/assign/retire races can deadlock.
static THREAD_POOL: std::sync::OnceLock<Mutex<Vec<Arc<WorkerSlot>>>> = std::sync::OnceLock::new();
const THREAD_POOL_DEPTH: usize = 32;
const THREAD_POOL_IDLE: Duration = Duration::from_secs(5);

pub(super) fn take_parked_slot() -> Option<Arc<WorkerSlot>> {
    let pool = THREAD_POOL.get_or_init(|| Mutex::new(Vec::new()));
    pool.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop()
}

/// Re-register a served-out slot as parked. False: the pool is at depth.
fn repark_slot(slot: &Arc<WorkerSlot>) -> bool {
    let pool = THREAD_POOL.get_or_init(|| Mutex::new(Vec::new()));
    let mut pool = pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pool.len() < THREAD_POOL_DEPTH && {
        pool.push(slot.clone());
        true
    }
}

/// Withdraw a timed-out slot from the pool. False: an assigner popped it
/// first, so a lease is imminent — the caller re-checks the slot state.
fn unpark_self(slot: &Arc<WorkerSlot>) -> bool {
    let pool = THREAD_POOL.get_or_init(|| Mutex::new(Vec::new()));
    let mut pool = pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match pool.iter().position(|s| Arc::ptr_eq(s, slot)) {
        Some(i) => {
            pool.swap_remove(i);
            true
        },
        None => false,
    }
}

/// Hand a lease to a parked slot. Only slots popped from the pool are
/// assignable, so the slot is necessarily `Parked` here.
pub(super) fn assign_lease(slot: &Arc<WorkerSlot>, shared: Arc<QueueShared>) {
    {
        let mut state = slot.state.lock().unwrap();
        debug_assert!(matches!(&*state, WorkerSlotState::Parked));
        *state = WorkerSlotState::Serving(shared);
    }
    slot.wake.notify_one();
}

/// The serving thread leaves its lease (Parked to await another, Retiring
/// to exit), waking any dropper parked on the slot (`wait_leave`).
pub(super) fn leave_lease(slot: &Arc<WorkerSlot>, retire: bool) {
    {
        let mut state = slot.state.lock().unwrap();
        *state = if retire {
            WorkerSlotState::Retiring
        } else {
            WorkerSlotState::Parked
        };
    }
    slot.wake.notify_all();
}

/// Wait until the slot's thread has left `shared`'s service: the lease
/// model's join equivalent — once it returns, no thread can still touch
/// this encoder's queue or buffer. A recycled slot may already serve a
/// later encoder by then, hence the pointer check. The bounded wait
/// re-checks its predicate, like `wait_progress`.
pub(super) fn wait_leave(slot: &Arc<WorkerSlot>, shared: &Arc<QueueShared>) {
    let mut state = slot.state.lock().unwrap();
    while matches!(&*state, WorkerSlotState::Serving(s) if Arc::ptr_eq(s, shared)) {
        let (guard, _) = slot
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap();
        state = guard;
    }
}

/// Pool thread body: one leased queue at a time, parking between leases.
// The spawn body must own its slot ('static); it stays alive after the
// guarded inner loop so the panic path can retire the slot.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn pool_thread(slot: Arc<WorkerSlot>) {
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool_thread_inner(&slot)));
    if let Err(payload) = outcome {
        // The lease machinery itself panicked (job panics are caught in
        // `serve_queue`): withdraw the slot so no dropper waits on a dead
        // thread and no future lease lands on it.
        unpark_self(&slot);
        {
            let mut state = slot.state.lock().unwrap();
            *state = WorkerSlotState::Retiring;
        }
        slot.wake.notify_all();
        std::panic::resume_unwind(payload);
    }
}

fn pool_thread_inner(slot: &Arc<WorkerSlot>) {
    let mut state: Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>> = None;
    while let Some(shared) = wait_lease(slot) {
        let retire = serve_queue(&shared, &mut state);
        leave_lease(slot, retire);
        if retire {
            // A job panicked on this thread: never reuse a thread that has
            // seen an unwind. The state (possibly mid-compress garbage) is
            // already dropped.
            return;
        }
        if !repark_slot(slot) {
            leave_lease(slot, true);
            if let Some(state) = state.take() {
                return_pooled_state(state);
            }
            return;
        }
    }
    // Retired (idle timeout): the state is quiescent, pool it.
    if let Some(state) = state.take() {
        return_pooled_state(state);
    }
}

/// Wait for the next lease. None retires the thread.
fn wait_lease(slot: &Arc<WorkerSlot>) -> Option<Arc<QueueShared>> {
    let mut state = slot.state.lock().unwrap();
    loop {
        if let WorkerSlotState::Serving(shared) = &*state {
            return Some(shared.clone());
        }
        if matches!(&*state, WorkerSlotState::Retiring) {
            return None;
        }
        let (guard, timed_out) = slot.wake.wait_timeout(state, THREAD_POOL_IDLE).unwrap();
        state = guard;
        if !timed_out.timed_out() || !matches!(&*state, WorkerSlotState::Parked) {
            continue;
        }
        // Idle: withdraw from the pool before retiring. The pool lock is
        // never taken under the slot lock, so an assigner may have popped
        // the slot in between — the state re-check below catches its lease.
        drop(state);
        if unpark_self(slot) {
            let mut state = slot.state.lock().unwrap();
            if matches!(&*state, WorkerSlotState::Parked) {
                *state = WorkerSlotState::Retiring;
                return None;
            }
        }
        state = slot.state.lock().unwrap();
    }
}
