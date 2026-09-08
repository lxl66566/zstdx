//! Frame-checksum offload to a sidecar thread. XXH64 is four serial
//! accumulator chains (~8 cycles of latency per 32 bytes), so hashing
//! incompressible blocks inline caps the raw-block copy at hash speed even
//! though the copy itself runs twice as fast. The checksum value is only
//! needed when the frame ends, so the slice path posts each block's bytes to
//! a dedicated worker (single-producer/single-consumer ring) and waits once
//! at `finish`; the posted pointers point into the caller's input, which the
//! slice entry point guarantees stable for the whole call, and `Drop` drains
//! the ring before the handle can outlive the buffer.
//!
//! The worker spins: block cadence on the engaging payloads is a few
//! microseconds, far below any park/wake latency, and the single-core-pinned
//! case never engages (see [`AsyncChecksum::new`]).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::marker::PhantomData;
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::xxh64::Xxh64;

/// Ring capacity in tasks. A 32 MiB frame posts ~260 tasks; the producer
/// backpressures on a full ring (the worker drains it far faster than the
/// main thread produces on every engaging payload).
const RING: usize = 512;

const KIND_ABSORB: usize = 0;
const KIND_RESET: usize = 1;
const KIND_FINISH: usize = 2;

struct Slot {
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
    state_id: AtomicUsize,
    kind: AtomicUsize,
}

struct Shared {
    /// Next sequence number the producer has published.
    head: AtomicU64,
    /// Next sequence number the consumer has completed.
    tail: AtomicU64,
    slots: [Slot; RING],
    running: AtomicBool,
}

struct Producer {
    shared: Arc<Shared>,
    /// One past the last sequence this frame posted; the drain threshold.
    /// NOT used to claim slots: nested frames on this thread interleave into
    /// the same ring, so every post claims its slot from the shared head.
    posted_upto: u64,
    state_id: usize,
    /// The ring tolerates exactly one producer thread (see [`Producer::post`]).
    _not_send: PhantomData<*const ()>,
}

#[inline(always)]
fn spin() {
    core::hint::spin_loop();
}

impl Shared {
    fn wait_free_slot(&self, seq: u64) {
        if seq >= RING as u64 {
            while self.tail.load(Ordering::Acquire) <= seq - RING as u64 {
                spin();
            }
        }
    }
}

impl Producer {
    fn post(&mut self, kind: usize, ptr: *const u8, len: usize) {
        // Claim the slot by reading head: every producer lives on the thread
        // that owns this ring and post is non-reentrant, so the load-return-
        // store sequence is atomic against other producers and nested frames
        // interleave into distinct slots. A producer-local counter would let
        // two live frames claim the same slot and overwrite each other's
        // tasks (observed as a lost RESET and an out-of-bounds worker panic).
        let seq = self.shared.head.load(Ordering::Relaxed);
        self.shared.wait_free_slot(seq);
        let slot = &self.shared.slots[(seq % RING as u64) as usize];
        slot.ptr.store(ptr as *mut u8, Ordering::Relaxed);
        slot.len.store(len, Ordering::Relaxed);
        slot.state_id.store(self.state_id, Ordering::Relaxed);
        // The kind store publishes the payload fields to the consumer that
        // acquires it; the head store publishes the slot itself.
        slot.kind.store(kind, Ordering::Release);
        self.shared.head.store(seq + 1, Ordering::Release);
        self.posted_upto = seq + 1;
    }

    fn drain(&mut self) {
        while self.shared.tail.load(Ordering::Acquire) < self.posted_upto {
            spin();
        }
    }
}

std::thread_local! {
    static WORKER: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

/// Whether the offload should engage: the worker needs a second usable CPU
/// (a process pinned to one core would only add context switches).
fn worker_shared() -> Option<Arc<Shared>> {
    if std::thread::available_parallelism().map_or(true, |n| n.get() < 2) {
        return None;
    }
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            let shared = Arc::new(Shared {
                head: AtomicU64::new(0),
                tail: AtomicU64::new(0),
                slots: [const {
                    Slot {
                        ptr: AtomicPtr::new(core::ptr::null_mut()),
                        len: AtomicUsize::new(0),
                        state_id: AtomicUsize::new(0),
                        kind: AtomicUsize::new(KIND_ABSORB),
                    }
                }; RING],
                running: AtomicBool::new(true),
            });
            let worker_shared = shared.clone();
            std::thread::Builder::new()
                .name("ruzstd-xxh64".into())
                .spawn(move || run(worker_shared))
                .ok()?;
            *w = Some(shared);
        }
        w.clone()
    })
}

fn run(shared: Arc<Shared>) {
    // One checksum state per concurrent frame; ids come from a global
    // counter and are stable for the process lifetime, so the vector only
    // ever grows.
    let mut states: Vec<Xxh64> = Vec::new();
    let mut done = 0u64;
    while shared.running.load(Ordering::Relaxed) {
        while shared.head.load(Ordering::Acquire) <= done {
            spin();
        }
        let slot = &shared.slots[(done % RING as u64) as usize];
        let kind = slot.kind.load(Ordering::Acquire);
        let state_id = slot.state_id.load(Ordering::Relaxed);
        match kind {
            KIND_RESET => {
                if state_id >= states.len() {
                    states.resize(state_id + 1, Xxh64::new(0));
                }
                states[state_id] = Xxh64::new(0);
            }
            KIND_FINISH => {
                let reply = slot.ptr.load(Ordering::Relaxed);
                // SAFETY: the reply cell outlives this task — the producer
                // stack that owns it spins until `done` flips, which happens
                // only after these stores.
                unsafe {
                    let cell = &*(reply as *const FinishCell);
                    cell.value
                        .store(states[state_id].finish(), Ordering::Relaxed);
                    cell.done.store(true, Ordering::Release);
                }
            }
            _ => {
                let ptr = slot.ptr.load(Ordering::Relaxed);
                let len = slot.len.load(Ordering::Relaxed);
                // SAFETY: the producer guarantees the posted range stays
                // alive until this task completes (the slice input is
                // immutable for the call and Drop drains before release).
                let bytes = unsafe { slice::from_raw_parts(ptr, len) };
                states[state_id].write(bytes);
            }
        }
        done += 1;
        shared.tail.store(done, Ordering::Release);
    }
}

struct FinishCell {
    value: AtomicU64,
    done: AtomicBool,
}

static NEXT_STATE_ID: AtomicUsize = AtomicUsize::new(0);

/// Per-frame handle: mirrors the inline `FrameHasher` contract but every
/// absorb runs on the worker.
pub(crate) struct AsyncChecksum {
    producer: Producer,
}

impl AsyncChecksum {
    /// Returns None when the offload cannot engage (single-core pinning or
    /// spawn failure); the caller then hashes inline.
    pub(crate) fn new() -> Option<Self> {
        let shared = worker_shared()?;
        let state_id = NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed);
        let mut producer = Producer {
            shared,
            posted_upto: 0,
            state_id,
            _not_send: PhantomData,
        };
        producer.post(KIND_RESET, core::ptr::null(), 0);
        Some(Self { producer })
    }

    /// Absorb `bytes` off-thread. Posted pointers must stay valid until the
    /// next [`Self::finish`] or the handle's drop.
    #[inline]
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.producer.post(KIND_ABSORB, bytes.as_ptr(), bytes.len());
    }

    /// Drain the frame's posts and produce the checksum.
    pub(crate) fn finish(&mut self) -> u32 {
        let cell = FinishCell {
            value: AtomicU64::new(0),
            done: AtomicBool::new(false),
        };
        self.producer
            .post(KIND_FINISH, core::ptr::addr_of!(cell) as *const u8, 0);
        while !cell.done.load(Ordering::Acquire) {
            spin();
        }
        cell.value.load(Ordering::Relaxed) as u32
    }
}

impl Drop for AsyncChecksum {
    fn drop(&mut self) {
        // Panic path: no finish was requested, but posted pointers may not
        // be consumed yet; drain before the input buffer can go away.
        self.producer.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::AsyncChecksum;
    use crate::xxh64::Xxh64;
    use alloc::vec::Vec;

    /// The offloaded checksum must equal the inline one for whole writes,
    /// arbitrary splits and multiple frames through the same worker.
    #[test]
    fn offloaded_checksum_matches_inline() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for len in [0usize, 1, 7, 31, 32, 33, 64, 100, 4096, 100_000] {
            let mut data = Vec::with_capacity(len);
            while data.len() < len {
                data.extend_from_slice(&rand().to_le_bytes());
            }
            let mut reference = Xxh64::new(0);
            reference.write(&data);
            let mut offloaded = AsyncChecksum::new().expect("worker engages under test");
            offloaded.write(&data);
            assert_eq!(offloaded.finish(), reference.finish() as u32, "len {len}");

            // arbitrary splits: state continuity across posts
            let mut offloaded = AsyncChecksum::new().unwrap();
            let mut i = 0;
            while i < data.len() {
                let step = 1 + (rand() % 999) as usize;
                let end = (i + step).min(data.len());
                offloaded.write(&data[i..end]);
                i = end;
            }
            assert_eq!(
                offloaded.finish(),
                reference.finish() as u32,
                "splits {len}"
            );
        }

        // two frames through the same worker: states must not leak
        let a: Vec<u8> = (0..4096).map(|i| (i * 7) as u8).collect();
        let b: Vec<u8> = alloc::vec![9u8; 4096];
        let mut ra = Xxh64::new(0);
        ra.write(&a);
        let mut rb = Xxh64::new(0);
        rb.write(&b);
        let mut ha = AsyncChecksum::new().unwrap();
        let mut hb = AsyncChecksum::new().unwrap();
        ha.write(&a);
        hb.write(&b);
        assert_eq!(ha.finish(), ra.finish() as u32);
        assert_eq!(hb.finish(), rb.finish() as u32);
    }
}
