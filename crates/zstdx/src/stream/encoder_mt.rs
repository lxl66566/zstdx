//! Multithreaded streaming frame core: the job-queue sibling of the
//! single-threaded [`super::encoder_core`] core.
//!
//! Input accumulates in one contiguous buffer — a strip of already-encoded
//! history followed by the bytes not yet encoded — and every job posts to a
//! persistent worker pool the moment its bytes are complete (a pledged
//! stream holds its final bulk-grid job for `finish` to mark last). Jobs are
//! cut on absolute job-size boundaries, so the frame bytes depend only on
//! the input and not on how it was written; a flush is the one exception,
//! re-gridding early because making data visible early is its purpose. With
//! a pledged size the job size follows the bulk formula, so a stream written
//! exactly to its pledge is byte-identical to the bulk mt output; without a
//! pledge the job size grows along the stream in equal-size epochs (see
//! [`JobGrid::Growing`]).
//!
//! Buffer recycling is the pipelining hinge: the dead prefix wraps in place
//! (an in-buffer move of the still-live tail) and the buffer grows at the
//! same points — both only once every posted job has completed, because
//! incomplete jobs hold resolved pointers into the buffer. The pump
//! therefore stalls on the workers only when it has lapped the buffer's
//! live data, which the epoch-scale sizing bounds to once per epoch.

use alloc::{sync::Arc, vec::Vec};
use core::{
    slice,
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    any::Any,
    collections::VecDeque,
    sync::{Condvar, Mutex},
    thread::JoinHandle,
};

use super::encoder_core::StreamChecksum;
use crate::{
    EncoderOptions, Level,
    blocks::block::BlockType,
    encoding::{
        block_header::BlockHeader,
        frame_compressor::{BlockChecksum as _, CompressState, FrameHasher, new_slice_state},
        frame_header::FrameHeader,
        match_generator::MatchGeneratorDriver,
        mt::{MAX_JOB_SIZE, MIN_JOB_SIZE, job_size_for, run_job_with},
        reach_probe::ReachChoice,
    },
};

/// Job boundary schedule. Both variants cut jobs on absolute stream offsets
/// alone, so without a flush the frame bytes never depend on how the input
/// was written.
enum JobGrid {
    /// Pledged stream: one fixed job size from the shared bulk formula over
    /// the pledge, so an exactly-pledged stream is byte-identical to the
    /// bulk mt output.
    Fixed(usize),
    /// Unpledged stream: jobs quantize into epochs of `burst_jobs` equal
    /// jobs, each epoch doubling the job size (floored at the bulk formula's
    /// floor, capped at its ceiling). Equal sizes keep a burst's barrier
    /// utilization at ~1 (a growing-per-job schedule puts a burst's largest
    /// and smallest job a growth-factor^burst_jobs apart, so the barrier
    /// idled half the workers), the epoch grid still converges toward the
    /// bulk job density on long streams, and the schedule stays a pure
    /// function of absolute offset — jobs post exactly on grid boundaries,
    /// so the queue never straddles an epoch.
    Growing,
}

/// One posted job: the frozen source view (see the recycling rules in
/// [`MtEncoderCore`]'s docs — the backing bytes cannot move before this job
/// completes) plus the slot its encoded blocks land in.
struct Job {
    /// Base and length of the frozen source view shared with the pool.
    src: FrozenSrc,
    /// Index of the job's strip start inside `src`, and the job's own range
    /// relative to it.
    first: usize,
    len: usize,
    /// Every job except the frame's first starts where the decoder's
    /// repcode history is unknown (see the bulk mt path); a flush-rebased
    /// grid starts mid-frame too.
    gate: bool,
    last_frame_block: bool,
    overlap: usize,
    level: Level,
    shape: crate::InputShape,
    out: Mutex<Option<Vec<u8>>>,
    /// Set (Release) once `out` holds the job's bytes: the assembling side
    /// polls this instead of taking the mutex per tick.
    done: core::sync::atomic::AtomicBool,
}

// SAFETY: the pointer is dereferenced only while the posting thread
// guarantees the backing allocation and bytes stable: buffer moves (wrap)
// and reallocations happen only at quiescence, after every job reading the
// buffer has completed.
unsafe impl Send for FrozenSrc {}
unsafe impl Sync for FrozenSrc {}

/// Frozen job source handed to the worker pool.
struct FrozenSrc {
    ptr: *const u8,
}

// Reusable accumulate buffers: a fresh encoder mapping its multi-megabyte
// buffer pays the whole first-touch fault+zero cost again, which on a
// THP=madvise machine with fragmented physical memory falls back to 4 KiB
// pages (~1.5-2 us each — thousands of faults per stream, measured as the
// interleaved-bench bimodality's mechanism). Retaining one buffer per
// thread caps that cost at the first stream. Size-capped so a one-off huge
// stream does not pin memory forever.
std::thread_local! {
    static BUF_POOL: core::cell::RefCell<Vec<Vec<u8>>> =
        const { core::cell::RefCell::new(Vec::new()) };
}
// Largest buffer retained per thread (a 32 MiB stream peaks at ~35 MiB).
const BUF_POOL_KEEP_MAX: usize = 64 * 1024 * 1024;
// Buffers retained per thread (a second one covers size-mismatched pairs).
const BUF_POOL_DEPTH: usize = 2;

// Upper bound on the accumulate buffer's growth; past it the dead-prefix
// wrap alone recycles space (a wait per ~BUF_CAP_MAX bytes of stream is
// negligible). The old burst model reserved epoch-scale windows of the
// same magnitude.
const BUF_CAP_MAX: usize = 256 * 1024 * 1024;

// Take the pooled accumulate buffer, or a fresh one, with room for `want`
// bytes (a larger pooled buffer is kept as-is — its spare capacity only
// helps the growth reserve).
fn take_pooled_buf(want: usize) -> Vec<u8> {
    BUF_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        let best = pool
            .iter()
            .rposition(|b| b.capacity() >= want)
            .or_else(|| pool.iter().rposition(|_| true));
        match best {
            Some(i) => {
                let mut buf = pool.swap_remove(i);
                buf.clear();
                buf
            },
            None => Vec::new(),
        }
    })
}

fn return_pooled_buf(buf: Vec<u8>) {
    if buf.capacity() == 0 || buf.capacity() > BUF_POOL_KEEP_MAX {
        return;
    }
    BUF_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < BUF_POOL_DEPTH {
            pool.push(buf);
        }
    });
}

struct QueueInner {
    shutdown: bool,
    /// Unclaimed posted jobs, in post order (workers claim from the front,
    /// so the oldest job always starts first).
    queue: VecDeque<Arc<Job>>,
    poison: Option<alloc::boxed::Box<dyn Any + Send>>,
}

/// State shared between the encoder and its pool threads.
struct QueueShared {
    inner: Mutex<QueueInner>,
    /// Workers wait for posted jobs; the encoder waits for completions.
    wake: Condvar,
    progressed: Condvar,
    /// Fast-path mirror of `inner.poison` (the hot write path polls it
    /// without taking the queue lock).
    poisoned: core::sync::atomic::AtomicBool,
    /// Posted jobs not yet completed — the wrap/grow guard and the drain
    /// waits key on it reaching zero.
    n_incomplete: AtomicU64,
    /// Monotonic count of completed jobs — the drain waits' progress tick.
    completed: AtomicU64,
    /// Reusable encoder states for the calling thread's inline jobs.
    states: Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
}

/// Pool worker body: claim the oldest posted job, encode it through a
/// worker-local state (reused across jobs — the pooled matcher tables are
/// the expensive part), publish the bytes, and count the completion.
fn pool_worker(shared: Arc<QueueShared>) {
    let mut state: Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>> = None;
    loop {
        let job = {
            let mut inner = shared.inner.lock().unwrap();
            loop {
                if inner.poison.is_some() {
                    // The encoder is unwinding; in-flight jobs run out, but
                    // no new claims (the caller clears the queue itself).
                    return;
                }
                if let Some(job) = inner.queue.pop_front() {
                    break job;
                }
                if inner.shutdown {
                    return;
                }
                inner = shared.wake.wait(inner).unwrap();
            }
        };
        let mut poisoned = false;
        // SAFETY: the posting thread keeps the backing bytes stable for
        // this job's whole lifetime (see FrozenSrc).
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let state = state.get_or_insert_with(|| alloc::boxed::Box::new(new_slice_state()));
            let src = unsafe { slice::from_raw_parts(job.src.ptr, job.len) };
            run_job_with(
                state,
                src,
                job.first..job.len,
                job.overlap,
                job.last_frame_block,
                job.level,
                job.gate,
                job.shape,
                // The stream core keeps the stock reach: its jobs flow
                // before any head is assembled to probe (see
                // reach_probe's engagement notes).
                ReachChoice::Keep,
            )
        }));
        match attempt {
            Ok(bytes) => {
                *job.out.lock().unwrap() = Some(bytes);
                job.done.store(true, Ordering::Release);
            },
            Err(payload) => {
                // The state may be mid-compress garbage: drop it rather
                // than reuse. The slot is filled with an empty block so the
                // ordered assembly can run to completion before the panic
                // is resumed.
                state = None;
                poisoned = true;
                *job.out.lock().unwrap() = Some(Vec::new());
                job.done.store(true, Ordering::Release);
                shared.poisoned.store(true, Ordering::Release);
                shared.inner.lock().unwrap().poison = Some(payload);
            },
        }
        shared.n_incomplete.fetch_sub(1, Ordering::Release);
        shared.completed.fetch_add(1, Ordering::AcqRel);
        shared.progressed.notify_all();
        if poisoned {
            return;
        }
    }
}

pub(crate) struct MtEncoderCore {
    level: Level,
    checksum: bool,
    workers: u32,
    /// Absolute job schedule: job boundaries derive from absolute stream
    /// offsets alone (see [`JobGrid`]), so the frame bytes never depend on
    /// how the input is written. A flush re-grids by advancing job_start
    /// to the current end.
    job_start: u64,
    grid: JobGrid,
    /// History strip each job borrows (and fully indexes) as match window:
    /// the level's whole window, like the bulk mt path.
    overlap: usize,
    /// Jobs buffered before a burst fires: at least one full round of
    /// workers, amortizing the thread spawn.
    burst_jobs: usize,
    /// The frame's declared shape (length = the pledge, plus the forced
    /// window log); shared by every job so tables and the header agree.
    shape: crate::InputShape,
    hasher: StreamChecksum,
    header: Vec<u8>,
    /// [strip of already-encoded history for the next job's matches] followed
    /// by the bytes not yet encoded. `buf_base` is buf[0]'s absolute stream
    /// offset.
    buf: Vec<u8>,
    buf_base: u64,
    /// Total bytes fed.
    pos: u64,
    /// Checksum absorbed up to this absolute offset.
    hashed_end: u64,
    output: Vec<u8>,
    /// Consumed prefix of `output` (see the ST core's field).
    out_read: usize,
    header_emitted: bool,
    finished: bool,
    /// Pool coordination state, shared with the worker threads. Workers
    /// spawn lazily at the first post; until then this is only the state
    /// pool.
    shared: Arc<QueueShared>,
    /// Persistent pool workers, spawned once. Joined on drop via shutdown.
    pool_threads: Vec<JoinHandle<()>>,
    /// Posted jobs not yet assembled, in post order.
    pending: VecDeque<Arc<Job>>,
}

impl MtEncoderCore {
    pub(crate) fn new(options: &EncoderOptions) -> Self {
        let checksum = options.checksum && cfg!(feature = "hash");
        // The pledge is the frame's authoritative length; the forced window
        // log rides along so the jobs' tables and the header window agree.
        let shape = crate::InputShape {
            len: options.pledged_size,
            window_log: options.input_shape.window_log,
        };
        let window = MatchGeneratorDriver::window_for_level(options.level, shape);
        // The job history rides the level's stream overlap rule (see
        // `stream_overlap_for`): the whole window where cross-job history
        // carries a far class, the search domain where the LDM restarts
        // per job like the bulk path.
        let overlap = MatchGeneratorDriver::stream_overlap_for(options.level, shape) as usize;
        // A pledge sizes the grid like the bulk path (byte-identical output
        // when the input matches the pledge); an open-ended stream grows
        // its jobs along the stream (see JobGrid).
        let grid = match options.pledged_size {
            Some(n) => JobGrid::Fixed(job_size_for(n, options.workers, overlap)),
            None => JobGrid::Growing,
        };
        let initial_job = match grid {
            JobGrid::Fixed(size) => size,
            JobGrid::Growing => MIN_JOB_SIZE.max(overlap),
        };
        let header = FrameHeader {
            frame_content_size: options.pledged_size,
            single_segment: false,
            content_checksum: checksum,
            dictionary_id: None,
            window_size: Some(window),
        };
        let mut serialized = Vec::with_capacity(18);
        header.serialize(&mut serialized);
        // One epoch-scale window, like the burst model's reserve: enough for
        // a full round of in-flight jobs plus the strip and write-chunk
        // slack. The size also stays inside the buffer pool's keep cap, so
        // consecutive streams reuse the already-faulted allocation.
        let want = (options.workers as usize).max(2) * initial_job + overlap + 64 * 1024;
        let mut buf = take_pooled_buf(want);
        if buf.capacity() < want {
            buf.reserve_exact(want - buf.len());
            advise_hugepages(&buf);
        }
        Self {
            level: options.level,
            checksum,
            workers: options.workers,
            job_start: 0,
            grid,
            overlap,
            burst_jobs: (options.workers as usize).max(2),

            shape,
            hasher: if checksum {
                StreamChecksum::On(FrameHasher::new())
            } else {
                StreamChecksum::Off
            },
            header: serialized,
            buf,
            buf_base: 0,
            pos: 0,
            hashed_end: 0,
            output: Vec::with_capacity(initial_job + 64),
            out_read: 0,
            header_emitted: false,
            finished: false,
            shared: Arc::new(QueueShared {
                inner: Mutex::new(QueueInner {
                    shutdown: false,
                    queue: VecDeque::new(),
                    poison: None,
                }),
                wake: Condvar::new(),
                progressed: Condvar::new(),
                poisoned: core::sync::atomic::AtomicBool::new(false),
                n_incomplete: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                states: Mutex::new(Vec::new()),
            }),
            pool_threads: Vec::new(),
            pending: VecDeque::new(),
        }
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        debug_assert!(!self.finished);
        let mut data = data;
        while !data.is_empty() {
            let space = self.buf.capacity() - self.buf.len();
            if space == 0 {
                self.make_space(data.len());
                continue;
            }
            let n = space.min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.pos += n as u64;
            self.post_ready();
        }
    }

    /// Close the frame: the pending jobs (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.surface_poison();
        self.drain_all();
        if self.pos > self.job_start {
            self.encode_jobs(self.pos, true);
        } else {
            // Everything fed is already encoded (or nothing was fed): the
            // frame still needs a last block, and an empty raw one is the
            // shape the single-threaded streaming path emits.
            self.emit_header();
            BlockHeader {
                last_block: true,
                block_type: BlockType::Raw,
                block_size: 0,
            }
            .serialize(&mut self.output);
        }
        debug_assert_eq!(self.hashed_end, self.pos);
        if self.checksum {
            let checksum = self.hasher.finish32();
            self.output.extend_from_slice(&checksum.to_le_bytes());
        }
        self.finished = true;
    }

    /// Emit the pending bytes early as non-last jobs. A no-op when nothing
    /// is pending. The short trailing job re-grids the job boundaries, so
    /// the output of a flushed stream depends on the flush points (making
    /// data visible early is what a flush is for).
    pub(crate) fn flush_block(&mut self) {
        debug_assert!(!self.finished);
        // A flush promises visibility: every posted job's blocks belong to
        // the output before it returns.
        self.surface_poison();
        self.drain_all();
        if self.pos > self.job_start {
            self.encode_jobs(self.pos, false);
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn has_output(&self) -> bool {
        self.out_read < self.output.len()
    }

    /// Hand the encoded bytes to `w`, keeping the output buffer's allocation.
    pub(crate) fn write_output_to(
        &mut self,
        w: &mut impl crate::io::Write,
    ) -> Result<(), crate::io::Error> {
        let res = w.write_all(&self.output[self.out_read..]);
        self.output.clear();
        self.out_read = 0;
        res
    }

    /// Serve encoded bytes without deallocating the output buffer.
    pub(crate) fn split_output(&mut self, buf: &mut [u8]) -> usize {
        let pending = &self.output[self.out_read..];
        let n = buf.len().min(pending.len());
        buf[..n].copy_from_slice(&pending[..n]);
        self.out_read += n;
        if self.out_read == self.output.len() {
            self.output.clear();
            self.out_read = 0;
        }
        n
    }

    /// End offset of the job starting at absolute offset `start`.
    fn job_end(&self, start: u64) -> u64 {
        let size = match self.grid {
            JobGrid::Fixed(size) => size as u64,
            JobGrid::Growing => self.growing_epoch(start).1,
        };
        start + size
    }

    /// Growing grid: start offset and job size of the epoch containing `o`.
    /// Epochs tile from offset 0, each holding `burst_jobs` equal jobs and
    /// doubling the job size per epoch (from the bulk floor to its ceiling).
    fn growing_epoch(&self, o: u64) -> (u64, u64) {
        let mut size = MIN_JOB_SIZE.max(self.overlap) as u64;
        let mut lo = 0u64;
        loop {
            let hi = lo + self.burst_jobs as u64 * size;
            if o < hi {
                return (lo, size);
            }
            lo = hi;
            size = (size * 2).min(MAX_JOB_SIZE as u64);
        }
    }

    /// Growing grid: start offset of the epoch containing `o` (an epoch
    /// boundary when `o` is one).
    fn growing_epoch_floor(&self, o: u64) -> u64 {
        let mut size = MIN_JOB_SIZE.max(self.overlap) as u64;
        let mut lo = 0u64;
        loop {
            let hi = lo + self.burst_jobs as u64 * size;
            if o < hi {
                return lo;
            }
            lo = hi;
            size = (size * 2).min(MAX_JOB_SIZE as u64);
        }
    }

    /// Growing grid: end of the epoch containing `o`.
    fn growing_epoch_end(&self, o: u64) -> u64 {
        let (lo, size) = self.growing_epoch(o);
        lo + self.burst_jobs as u64 * size
    }

    /// Post every job whose bytes are complete, gated so the schedule stays
    /// a pure function of the input: the growing grid posts an epoch only
    /// once fully buffered (a stream end then re-slices the whole pending
    /// span into worker-filling tail jobs — posting the final epoch's big
    /// jobs early would both strand their encode on one worker at EOF and
    /// change the re-slice), while the fixed grid's boundaries never depend
    /// on the posting cadence, so each job posts the moment it completes. A
    /// pledged grid holds its final job back for `finish` to mark last
    /// (unlocked on overshoot, where the bulk path also stops holding).
    fn post_ready(&mut self) {
        loop {
            let end = self.job_end(self.job_start);
            match self.grid {
                JobGrid::Fixed(_) => {
                    // The fixed grid's boundaries never depend on the
                    // posting cadence (its build_bounds never re-slices),
                    // so each job posts the moment it completes.
                    if self.pos < end {
                        return;
                    }
                    if let Some(n) = self.shape.len {
                        if end >= n && self.pos <= n {
                            return;
                        }
                    }
                },
                JobGrid::Growing => {
                    // An epoch's jobs post only once the whole epoch is
                    // buffered — rechecked per job, because a large write
                    // (or an oversized pooled buffer) can carry `pos` past
                    // several epoch ends in one append: greedily posting an
                    // incomplete epoch's jobs would change both the
                    // stream-end tail re-slice and the bytes.
                    if self.pos < self.growing_epoch_end(self.job_start) {
                        return;
                    }
                },
            }
            self.post_job(self.job_start, end, false);
            self.job_start = end;
        }
    }

    /// Job boundaries between job_start and hi, on the absolute schedule.
    // The growing grid re-slices everything past the last epoch boundary
    // into at most `burst_jobs` equal jobs: that range only appears when
    // hi is the stream end or a flush point — a pure function of the
    // input, so chunking-independence holds — and the re-slice lets the
    // tail burst fill the workers instead of idling them behind one
    // schedule-sized job.
    fn build_bounds(&self, hi: u64) -> Vec<u64> {
        let mut bounds = Vec::with_capacity(self.burst_jobs * 3 + 2);
        bounds.push(self.job_start);
        match self.grid {
            JobGrid::Fixed(_) => {
                while *bounds.last().unwrap() < hi {
                    let next = self.job_end(*bounds.last().unwrap()).min(hi);
                    bounds.push(next);
                }
            },
            JobGrid::Growing => {
                let aligned = self.growing_epoch_floor(hi);
                while *bounds.last().unwrap() < aligned {
                    // The clamp fires only on a flush-regridded grid:
                    // job_start then sits mid-epoch, so the job holding it
                    // crosses the epoch floor — the short job ends exactly
                    // on the boundary and the walk re-aligns (a burst must
                    // never straddle an epoch). On an untouched grid the
                    // clamp is a no-op.
                    let next = self.job_end(*bounds.last().unwrap()).min(aligned);
                    bounds.push(next);
                }
                // Whenever the epoch floor lies ahead of job_start the walk
                // lands on it exactly (positive steps, each capped there).
                debug_assert!(aligned <= self.job_start || *bounds.last().unwrap() == aligned);
                let tail = hi - *bounds.last().unwrap();
                if tail > 0 {
                    let lo = *bounds.last().unwrap();
                    let size = self.job_end(lo) - lo;
                    let target = size.min(
                        tail.div_ceil(self.burst_jobs as u64)
                            .max(MIN_JOB_SIZE as u64),
                    );
                    let n = tail.div_ceil(target) as usize;
                    for i in 1..n {
                        bounds.push(lo + tail * i as u64 / n as u64);
                    }
                    bounds.push(hi);
                }
            },
        }
        bounds
    }

    /// Encode the jobs covering [job_start, hi) and wait them out: the
    /// flush/finish path, where the caller wants the bytes on return. A
    /// single short job on a pool-less core still runs inline (small inputs
    /// never spin up the pool).
    fn encode_jobs(&mut self, hi: u64, last_frame_block: bool) {
        debug_assert!(self.job_start < hi && hi <= self.pos);
        self.emit_header();
        let bounds = self.build_bounds(hi);
        if bounds.len() == 2 && self.pool_threads.is_empty() {
            self.run_inline_job(&bounds, hi, last_frame_block);
        } else {
            let last = bounds.len() - 2;
            for (i, w) in bounds.windows(2).enumerate() {
                self.post_job(w[0], w[1], last_frame_block && i == last);
            }
            self.job_start = hi;
            self.drain_all();
        }
    }

    /// One short tail job (small inputs, a flush, or a finish without a
    /// burst behind it): inline on the calling thread, no pool involved.
    fn run_inline_job(&mut self, bounds: &[u64], hi: u64, last_frame_block: bool) {
        // The shared job view starts at the next job's strip (the previous
        // job's tail), which is exactly what the buffer retained.
        let strip_lo = self.job_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= strip_lo);
        let first = (self.job_start - strip_lo) as usize;
        self.hash_to(hi);
        let last_len = (bounds[1] - bounds[0]) as usize;
        let mut state = take_pooled_state(&self.shared.states);
        let bytes = run_job_with(
            &mut state,
            &self.buf[..(hi - self.buf_base) as usize],
            first..first + last_len,
            self.overlap,
            last_frame_block,
            self.level,
            self.job_start > 0,
            self.shape,
            // See the pool worker's note: the stream core keeps the stock
            // reach.
            ReachChoice::Keep,
        );
        self.shared.states.lock().unwrap().push(state);
        self.output.extend_from_slice(&bytes);
        self.job_start = hi;
    }

    /// Post one job [start, end) to the pool. The source view is the
    /// buffer's [strip_lo, end) — the posting thread guarantees those bytes
    /// stable until the job completes (wrap and growth both require
    /// quiescence).
    fn post_job(&mut self, start: u64, end: u64, last_frame_block: bool) {
        debug_assert!(self.pos >= end);
        // The post cadence is also the assembly and panic-surfacing cadence:
        // per-write calls would burn millions of polls on the pump path.
        self.surface_poison();
        self.assemble_ready();
        self.emit_header();
        let strip_lo = start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= strip_lo);
        let first = (start - strip_lo) as usize;
        let len = (end - strip_lo) as usize;
        self.ensure_workers();
        // SAFETY: strip_lo >= buf_base (asserted above); the view is the
        // buffer's [strip_lo, end), so `first` indexes from its head.
        let ptr = unsafe { self.buf.as_ptr().add((strip_lo - self.buf_base) as usize) };
        let job = Arc::new(Job {
            src: FrozenSrc { ptr },
            first,
            len,
            overlap: self.overlap,
            level: self.level,
            last_frame_block,
            gate: start > 0,
            shape: self.shape,
            out: Mutex::new(None),
            done: core::sync::atomic::AtomicBool::new(false),
        });
        // Register before publishing: a worker's completion decrement must
        // never race ahead of the registration (the queue push below is the
        // earliest a worker can see the job).
        self.shared.n_incomplete.fetch_add(1, Ordering::Release);
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.queue.push_back(job.clone());
        }
        self.pending.push_back(job);
        self.shared.wake.notify_all();
        // Checksum absorb on the calling thread while the workers encode.
        self.hash_to(end);
    }

    /// Assemble every completed prefix job's blocks into the output, in post
    /// order. Never blocks; the read path calls it between pulls so output
    /// appears as soon as its jobs complete.
    fn assemble_ready(&mut self) {
        while self
            .pending
            .front()
            .is_some_and(|j| j.done.load(Ordering::Acquire))
        {
            let job = self.pending.pop_front().unwrap();
            let bytes = job.out.lock().unwrap().take().unwrap();
            self.output.extend_from_slice(&bytes);
        }
    }

    /// Wait for every posted job to complete and assemble it: the
    /// flush/finish visibility barrier.
    fn drain_all(&mut self) {
        loop {
            self.assemble_ready();
            if self.pending.is_empty() {
                return;
            }
            if self.quiescent() {
                // Everything completed yet the pending prefix has an
                // un-ready slot: only reachable through the poison abort
                // path.
                self.surface_poison();
                continue;
            }
            self.wait_progress();
        }
    }

    /// Wait until a job completes (or the pool dies). Exit conditions are
    /// re-checked on every wake: a completion burst that lands between the
    /// caller's own predicate check and the `completed` snapshot below
    /// would otherwise fire its notify before this wait parks, leaving the
    /// snapshot already final and the loop sleeping on a condition that
    /// will never change again.
    fn wait_progress(&self) {
        let mut inner = self.shared.inner.lock().unwrap();
        loop {
            if inner.poison.is_some() || inner.shutdown {
                return;
            }
            // Everything completed: the caller's predicate (drain or
            // recycle) is due a re-evaluation regardless of the snapshot.
            if self.shared.n_incomplete.load(Ordering::Acquire) == 0 {
                return;
            }
            let before = self.shared.completed.load(Ordering::Acquire);
            let (guard, _) = self
                .shared
                .progressed
                .wait_timeout(inner, core::time::Duration::from_millis(100))
                .unwrap();
            inner = guard;
            if self.shared.completed.load(Ordering::Acquire) != before {
                return;
            }
        }
    }

    /// Lowest absolute offset the buffer must still serve: the next job to
    /// post borrows [job_start - overlap, job_start) as its strip, and the
    /// bytes after it feed that job.
    fn movable_lo(&self) -> u64 {
        self.job_start.saturating_sub(self.overlap as u64)
    }

    /// Whether every posted job has completed (no resolved pointers into
    /// the buffer remain — the wrap/grow precondition). Completions only
    /// ever lower `n_incomplete`, and only this thread posts, so a zero
    /// read is stable for the caller's next move.
    fn quiescent(&self) -> bool {
        self.shared.n_incomplete.load(Ordering::Acquire) == 0
    }

    /// Make room for `want` more bytes of appends. Recycling the dead
    /// prefix (an in-buffer move of the live tail) and growth (which may
    /// relocate the allocation) both invalidate the resolved pointers
    /// incomplete jobs hold into the buffer, so they wait for the pool to
    /// run out first. With the buffer sized per epoch this stalls the pump
    /// at most once per epoch, behind the jobs it just fed.
    fn make_space(&mut self, want: usize) {
        while !self.quiescent() {
            if self.shared.poisoned.load(Ordering::Acquire) {
                // In-flight jobs still complete under poison; the unwind
                // resumes from there and never returns here.
                self.surface_poison();
            }
            self.wait_progress();
        }
        let keep = self.movable_lo();
        debug_assert!(self.buf_base <= keep);
        let drop_n = (keep - self.buf_base) as usize;
        if drop_n > 0 {
            self.buf.copy_within(drop_n.., 0);
            let live = self.buf.len() - drop_n;
            self.buf.truncate(live);
            self.buf_base = keep;
        }
        if self.buf.len() + want > self.buf.capacity() {
            // Doubling below the cap amortizes the growth; past it the wrap
            // alone recycles space, so growth tracks the live need.
            let target =
                (self.buf.len() + want + 64 * 1024).max((self.buf.capacity() * 2).min(BUF_CAP_MAX));
            self.buf.reserve_exact(target - self.buf.len());
            advise_hugepages(&self.buf);
        }
    }

    /// Absorb [hashed_end, end) into the frame checksum.
    fn hash_to(&mut self, end: u64) {
        debug_assert!(self.hashed_end <= end && end <= self.pos);
        let range = (self.hashed_end - self.buf_base) as usize..(end - self.buf_base) as usize;
        self.hasher.hash_tail(&self.buf[range]);
        self.hashed_end = end;
    }

    /// Spawn the persistent pool workers at the first posted job.
    fn ensure_workers(&mut self) {
        if self.pool_threads.is_empty() {
            for _ in 0..self.workers {
                let shared = self.shared.clone();
                self.pool_threads.push(
                    std::thread::Builder::new()
                        .spawn(move || pool_worker(shared))
                        .unwrap(),
                );
            }
        }
    }

    /// Resume a worker panic at the write/finish that observes it. The
    /// unclaimed queue jobs are aborted (completed empty, so the counts and
    /// the in-order assembly run to completion), the in-flight ones run
    /// out, and the panic surfaces with the frame state settled.
    fn surface_poison(&mut self) {
        if !self.shared.poisoned.load(Ordering::Acquire) {
            return;
        }
        let payload = {
            let mut inner = self.shared.inner.lock().unwrap();
            match inner.poison.take() {
                None => return,
                Some(payload) => {
                    while let Some(job) = inner.queue.pop_front() {
                        *job.out.lock().unwrap() = Some(Vec::new());
                        job.done.store(true, Ordering::Release);
                        self.shared.n_incomplete.fetch_sub(1, Ordering::Release);
                    }
                    payload
                },
            }
        };
        while !self.quiescent() {
            self.wait_progress();
        }
        self.assemble_ready();
        std::panic::resume_unwind(payload);
    }

    fn emit_header(&mut self) {
        if !self.header_emitted {
            self.output.extend_from_slice(&self.header);
            self.header_emitted = true;
        }
    }
}

/// Take a worker state from the pool, or build a fresh one.
fn take_pooled_state(
    pool: &Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
) -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    pool.lock()
        .unwrap()
        .pop()
        .unwrap_or_else(|| alloc::boxed::Box::new(new_slice_state()))
}

impl Drop for MtEncoderCore {
    fn drop(&mut self) {
        if !self.pool_threads.is_empty() {
            self.shared.inner.lock().unwrap().shutdown = true;
            self.shared.wake.notify_all();
            for handle in self.pool_threads.drain(..) {
                let _ = handle.join();
            }
        }
        // The accumulate buffer outlives the encoder in the thread-local
        // pool (see take_pooled_buf); every worker has exited by now, so
        // nothing reads it anymore.
        return_pooled_buf(core::mem::take(&mut self.buf));
    }
}

/// Advise MADV_HUGEPAGE over the buffer's whole capacity: the accumulate
/// buffer is written densely at burst scale, and on THP=madvise machines its
/// first touch otherwise pays one 4 KiB fault per page — milliseconds at
/// the spans the growing grid reaches. Pure advice: THP=always machines map it
/// huge anyway, THP=never ignores it, and a failing syscall is too.
#[cfg(target_os = "linux")]
fn advise_hugepages(buf: &Vec<u8>) {
    const MADV_HUGEPAGE: u32 = 14;
    unsafe extern "C" {
        fn madvise(addr: *mut u8, len: usize, advice: u32) -> i32;
    }
    let lo = (buf.as_ptr() as usize) & !0xfff;
    let hi = buf.as_ptr() as usize + buf.capacity();
    if hi > lo {
        unsafe { madvise(lo as *mut u8, hi - lo, MADV_HUGEPAGE) };
    }
}

#[cfg(not(target_os = "linux"))]
fn advise_hugepages(_buf: &Vec<u8>) {}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::decoding::FrameDecoder;

    fn textish(len: usize) -> Vec<u8> {
        let words: [&[u8]; 13] = [
            b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ",
            b"lorem ", b"ipsum ", b"dolor ", b"sit ", b"amet ",
        ];
        let mut state = 7u64;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = words[((state >> 33) as usize) % words.len()];
            let take = w.len().min(len - out.len());
            out.extend_from_slice(&w[..take]);
        }
        out
    }

    /// The unpledged schedule keeps a burst's jobs equal while a pledged one
    /// keeps the fixed bulk grid.
    #[test]
    fn growing_grid_scales_jobs_with_stream() {
        let unpledged = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        let floor = unpledged.job_end(0);
        assert_eq!(floor, MIN_JOB_SIZE.max(unpledged.overlap) as u64);
        // Epochs of 4 jobs double the size: the job at 60 MiB opens the
        // 16 MiB epoch, and every job it bursts with is equally large.
        let mib = (1024 * 1024) as u64;
        assert_eq!(unpledged.job_end(60 * mib) - 60 * mib, 16 * mib);
        for o in [0, floor, 60 * mib - 1, 60 * mib, 61 * mib] {
            let (lo, size) = unpledged.growing_epoch(o);
            assert_eq!(unpledged.growing_epoch_end(o), lo + 4 * size);
        }

        let pledged = MtEncoderCore::new(
            &EncoderOptions::new(Level::Fastest)
                .workers(4)
                .pledged_size(Some(64 * 1024 * 1024)),
        );
        assert_eq!(
            pledged.job_end(0),
            pledged.job_end(32 * 1024 * 1024) - 32 * 1024 * 1024
        );
    }

    /// A long unpledged stream crosses many growth points and burst/tail
    /// shapes; the frame must decode to the fed bytes, and — no flush
    /// involved — the frame bytes must not depend on how it was written.
    #[test]
    fn growing_grid_roundtrip() {
        let data = textish(20 * 1024 * 1024 + 31);
        let mut reference = None;
        for chunk in [1024 * 1024, 300 * 1024, usize::MAX] {
            let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
            for piece in data.chunks(chunk) {
                core.write(piece);
            }
            core.finish();
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder.decode_all(&core.output, &mut out).unwrap();
            assert_eq!((n, &out[..n]), (data.len(), &data[..]), "chunk {chunk}");
            match &reference {
                Some(bytes) => assert_eq!(&core.output, bytes, "chunk {chunk}"),
                None => reference = Some(core.output.clone()),
            }
        }
    }
}
