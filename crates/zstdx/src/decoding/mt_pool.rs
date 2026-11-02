//! Pooled worker threads leased to one parallel decode at a time.
//!
//! Split out of [`super::mt`] so the lease/park/retire mechanics (lock
//! order, idle timeout, panic retirement) stand alone from the stage-A job
//! semantics; the per-call job state stays with the decode driver. The
//! mirror of the streaming encoder's pool (see [`crate::stream::mt_pool`]):
//! a decode otherwise pays the whole worker spawn+join per call — about 57
//! us at four threads and 192 us at sixteen on this class of machine —
//! against a 8 to 42 us slot handoff.

use alloc::vec::Vec;
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::mt::{SegmentScratch, StageAJob, serve_stagea};

/// One parked pool thread's handoff slot: the thread parks here between
/// decode leases, and the next `decode_parallel` wakes it with a lease on
/// its job.
pub(super) struct WorkerSlot {
    state: Mutex<WorkerSlotState>,
    wake: Condvar,
}

pub(super) enum WorkerSlotState {
    /// Parked in the pool, waiting for the next lease.
    Parked,
    /// Serving the leased job.
    Serving(Arc<StageAJob>),
    /// The thread is exiting: its lease panicked, the pool was full at
    /// repark, or the idle timeout fired.
    Retiring,
}

// Parked worker threads, global across decoders: a decode otherwise pays
// the whole worker spawn+join per call (clone plus stack guard-page setup
// dominates; the stage-A queue itself is a condvar either way). Depth-capped
// like the encoder pool; a parked thread retires after `THREAD_POOL_IDLE`,
// so a one-off decode does not pin threads into a long-running process.
// Lock order: the pool mutex and a slot's state mutex are never held
// together, so none of the park/assign/retire races can deadlock.
static THREAD_POOL: std::sync::OnceLock<Mutex<Vec<Arc<WorkerSlot>>>> = std::sync::OnceLock::new();
const THREAD_POOL_DEPTH: usize = 32;
const THREAD_POOL_IDLE: Duration = Duration::from_secs(5);

fn take_parked_slot() -> Option<Arc<WorkerSlot>> {
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

/// Hand a lease to a parked slot. Only slots exclusively owned by the
/// leasing caller are assignable, so the slot is necessarily `Parked`
/// here.
pub(super) fn assign_lease(slot: &Arc<WorkerSlot>, job: Arc<StageAJob>) {
    {
        let mut state = slot.state.lock().unwrap();
        debug_assert!(matches!(&*state, WorkerSlotState::Parked));
        *state = WorkerSlotState::Serving(job);
    }
    slot.wake.notify_one();
}

/// Mark a slot's thread as leaving (Parked to await another lease, Retiring
/// to exit), waking any waiter parked on the slot (`wait_leave`).
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

/// Wait until the slot's thread has left `job`'s service: the lease model's
/// join equivalent — once it returns, no thread can still touch this
/// decode's frozen input or segment plans. A recycled slot may already
/// serve a later decode by then, hence the pointer check. The bounded wait
/// re-checks its predicate, so a lost wakeup cannot hang the caller.
pub(super) fn wait_leave(slot: &Arc<WorkerSlot>, job: &Arc<StageAJob>) {
    let mut state = slot.state.lock().unwrap();
    while matches!(&*state, WorkerSlotState::Serving(s) if Arc::ptr_eq(s, job)) {
        let (guard, _) = slot
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap();
        state = guard;
    }
}

/// Secure `n` worker slots and hand every one a lease on `job`. Pooled
/// slots first, fresh spawns making up the rest; a fresh spawn parks in
/// `wait_lease` before its first assignment, so leases go out only after
/// every slot exists. A spawn failure retires the already-secured slots
/// (their threads exit at the flag) and panics, mirroring `scope.spawn`'s
/// behavior.
pub(super) fn lease_workers(n: usize, job: &Arc<StageAJob>) -> Vec<Arc<WorkerSlot>> {
    let mut slots = Vec::with_capacity(n);
    for _ in 0..n {
        let slot = take_parked_slot().unwrap_or_else(|| {
            let slot = Arc::new(WorkerSlot {
                state: Mutex::new(WorkerSlotState::Parked),
                wake: Condvar::new(),
            });
            match std::thread::Builder::new()
                .name("zstdx-dec-pool".into())
                .spawn({
                    let slot = slot.clone();
                    move || pool_thread(slot)
                }) {
                Ok(_) => slot,
                Err(e) => {
                    leave_lease(&slot, true);
                    for taken in &slots {
                        leave_lease(taken, true);
                    }
                    panic!("worker spawn failed: {e}");
                },
            }
        });
        slots.push(slot);
    }
    for slot in &slots {
        assign_lease(slot, job.clone());
    }
    slots
}

/// Retire every parked slot (test hook for the no-thread-leak assertion):
/// their threads observe the flag on wake and exit without reparking.
#[cfg(test)]
pub(super) fn retire_all_for_test() {
    let Some(pool) = THREAD_POOL.get() else {
        return;
    };
    let mut pool = pool.lock().unwrap();
    for slot in pool.drain(..) {
        leave_lease(&slot, true);
    }
}

/// Pool thread body: one leased job at a time, parking between leases.
// The spawn body must own its slot ('static); it stays alive after the
// guarded inner loop so the panic path can retire the slot.
fn pool_thread(slot: Arc<WorkerSlot>) {
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool_thread_inner(&slot)));
    if let Err(payload) = outcome {
        // The lease machinery itself panicked (job panics are caught in
        // `serve_stagea`): withdraw the slot so no waiter blocks on a dead
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
    // Worker-local stage-A scratch, carried across leases (the entropy
    // tables are the expensive part; every segment starts at a restart
    // point, so `decode_segment`'s per-segment reset re-initializes all
    // state a job reads).
    let mut scratch: Option<SegmentScratch> = None;
    while let Some(job) = wait_lease(slot) {
        let retire = serve_stagea(&job, &mut scratch);
        leave_lease(slot, retire);
        if retire {
            // A job panicked on this thread: never reuse a thread that has
            // seen an unwind. `serve_stagea` already dropped the scratch
            // (possibly mid-decode garbage).
            return;
        }
        if !repark_slot(slot) {
            leave_lease(slot, true);
            return;
        }
    }
    // Retired (idle timeout or test drain): the scratch drops with the
    // thread.
}

/// Wait for the next lease. None retires the thread.
fn wait_lease(slot: &Arc<WorkerSlot>) -> Option<Arc<StageAJob>> {
    let mut state = slot.state.lock().unwrap();
    loop {
        if let WorkerSlotState::Serving(job) = &*state {
            return Some(job.clone());
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
