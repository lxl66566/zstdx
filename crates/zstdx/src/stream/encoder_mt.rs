//! Multithreaded streaming frame core: the burst-shaped sibling of the
//! single-threaded [`super::encoder_core`] core.
//!
//! Input accumulates in one contiguous buffer — a strip of already-encoded
//! history followed by the bytes not yet encoded — and once enough
//! job-sized slices are pending, a burst encodes them in parallel through
//! the same pooled-job machinery as the bulk mt path (see
//! [`crate::encoding::mt`]), appending the assembled blocks to the output in
//! order. Jobs are cut on absolute job-size boundaries, so the frame bytes
//! depend only on the input and not on how it was written; a flush is the
//! one exception, re-gridding early because making data visible early is
//! its purpose. With a pledged size the job size follows the bulk formula,
//! so a stream written exactly to its pledge is byte-identical to the bulk
//! mt output; without a pledge the job size grows along the stream in
//! equal-size epochs (see [`JobGrid::Growing`]).

use alloc::{sync::Arc, vec::Vec};
use core::{
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::{
    any::Any,
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
    /// function of absolute offset — bursts fire exactly on epoch
    /// completion, so a burst is one epoch and never a straddle.
    Growing,
}

/// Frozen job source handed to the worker pool: the backing buffer is not
/// mutated, moved or dropped for the burst's lifetime — the posting thread
/// only resumes buffer work after the pool quiesces (every worker has left
/// the burst's job loop), the same freeze thread::scope used to provide.
struct FrozenSrc {
    ptr: *const u8,
    len: usize,
}

// SAFETY: the pointer is dereferenced only while the burst is posted, and
// the posting thread guarantees the freeze above for that whole window.
unsafe impl Send for FrozenSrc {}
unsafe impl Sync for FrozenSrc {}

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

/// One burst of jobs posted to the pool: everything a worker needs to run
/// its share. Slots and the poison slot mirror the former scope-based
/// burst; `exited` counts workers still inside this burst's job loop, and
/// reaching zero is the pool's quiesce point.
struct BurstCtx {
    src: FrozenSrc,
    /// Window index of `job_start` inside `src`.
    first: usize,
    /// Absolute job boundaries, `bounds.len() == n_jobs + 1`.
    bounds: Vec<u64>,
    n_jobs: usize,
    job_start: u64,
    overlap: usize,
    level: Level,
    last_frame_block: bool,
    shape: crate::InputShape,
    slots: Vec<Mutex<Option<Vec<u8>>>>,
    ready: Condvar,
    next_job: AtomicUsize,
    poison: Mutex<Option<alloc::boxed::Box<dyn Any + Send>>>,
    exited: AtomicUsize,
    quiesce: Mutex<()>,
    quiesce_cv: Condvar,
}

struct PoolInner {
    shutdown: bool,
    /// Burst generation: a worker picks the posted ctx up when `generation` moves
    /// past the generation it last served, and parks otherwise.
    generation: u64,
    ctx: Option<Arc<BurstCtx>>,
}

/// State shared between the encoder and its pool threads: burst
/// coordination plus the reusable encoder states (also serving the
/// single-job inline path, exactly like the former plain Vec pool).
struct PoolShared {
    inner: Mutex<PoolInner>,
    wake: Condvar,
    states: Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
}

/// Pool worker body: serve every posted burst until shutdown. The job loop
/// is the former thread::scope body verbatim (pooled state per burst,
/// atomic job pickup, poison check before each job); the only addition is
/// the `exited` countdown that marks the burst quiesced.
fn pool_worker(shared: Arc<PoolShared>) {
    let mut seen = 0u64;
    loop {
        let ctx;
        {
            let mut inner = shared.inner.lock().unwrap();
            while !inner.shutdown && inner.generation == seen {
                inner = shared.wake.wait(inner).unwrap();
            }
            if inner.shutdown {
                return;
            }
            seen = inner.generation;
            ctx = inner.ctx.clone().unwrap();
        }
        let mut state = take_pooled_state(&shared.states);
        let mut poisoned = false;
        loop {
            if ctx.poison.lock().unwrap().is_some() {
                break;
            }
            let id = ctx.next_job.fetch_add(1, Ordering::Relaxed);
            if id >= ctx.n_jobs {
                break;
            }
            let start = ctx.first + (ctx.bounds[id] - ctx.job_start) as usize;
            let end = ctx.first + (ctx.bounds[id + 1] - ctx.job_start) as usize;
            // Every job except the frame's first starts where the decoder's
            // repcode history is unknown (see the bulk mt path); a
            // flush-rebased grid starts mid-frame too.
            let gate = ctx.bounds[id] > 0;
            // SAFETY: the posting thread freezes the source for the whole
            // burst (see FrozenSrc).
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let src = unsafe { slice::from_raw_parts(ctx.src.ptr, ctx.src.len) };
                run_job_with(
                    &mut state,
                    src,
                    start..end,
                    ctx.overlap,
                    ctx.last_frame_block && id + 1 == ctx.n_jobs,
                    ctx.level,
                    gate,
                    ctx.shape,
                    // The stream core keeps the stock reach: its jobs flow
                    // before any head is assembled to probe (see
                    // reach_probe's engagement notes).
                    ReachChoice::Keep,
                )
            }));
            match attempt {
                Ok(bytes) => *ctx.slots[id].lock().unwrap() = Some(bytes),
                Err(payload) => {
                    *ctx.poison.lock().unwrap() = Some(payload);
                    // Release the slot so the ordered assembly below can run
                    // to completion before the panic is resumed. The state
                    // may be mid-compress garbage, so it is dropped rather
                    // than pooled.
                    poisoned = true;
                    *ctx.slots[id].lock().unwrap() = Some(Vec::new());
                    ctx.ready.notify_all();
                    break;
                },
            }
            ctx.ready.notify_all();
        }
        if !poisoned {
            shared.states.lock().unwrap().push(state);
        }
        if ctx.exited.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _g = ctx.quiesce.lock().unwrap();
            ctx.quiesce_cv.notify_all();
        }
    }
}

pub(crate) struct MtEncoderCore {
    level: Level,
    checksum: bool,
    workers: u32,
    /// Absolute job schedule: job boundaries derive from absolute stream
    /// offsets alone (see [`JobGrid`]), so the frame bytes never depend on
    /// how the input was written. A flush re-grids by advancing job_start
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
    /// Pool coordination state (reusable encoder states included), shared
    /// with the worker threads. Workers spawn lazily at the first
    /// multi-job burst; until then this is only the state pool.
    shared: Arc<PoolShared>,
    /// Persistent burst workers, spawned once (thread::scope used to pay a
    /// spawn+join of the whole set per burst, ~0.5-2 ms each). Joined on
    /// drop via shutdown.
    pool_threads: Vec<JoinHandle<()>>,
    /// The burst posted to the pool and not yet assembled: the calling
    /// thread keeps accumulating (appends confined to the reserved window,
    /// which never reallocs while workers read the buffer) and the burst is
    /// assembled at the next fire, a flush/finish, or when the window
    /// fills.
    inflight: Option<Arc<BurstCtx>>,
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
        let overlap = window as usize;
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
        let mut buf = take_pooled_buf(
            (options.workers as usize).max(2) * (MIN_JOB_SIZE.max(overlap)) + 64 * 1024,
        );
        if let JobGrid::Growing = grid {
            // The first epoch's whole span up front, advised huge: the
            // buffer is touched densely and grows to burst scale, and the
            // kernel only backs THP=madvise mappings when asked.
            buf.reserve_exact(
                (options.workers as usize).max(2) * (MIN_JOB_SIZE.max(overlap)) + 64 * 1024,
            );
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
            shared: Arc::new(PoolShared {
                inner: Mutex::new(PoolInner {
                    shutdown: false,
                    generation: 0,
                    ctx: None,
                }),
                wake: Condvar::new(),
                states: Mutex::new(Vec::new()),
            }),
            pool_threads: Vec::new(),
            inflight: None,
        }
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        debug_assert!(!self.finished);
        self.try_compact();
        let mut data = data;
        while !data.is_empty() {
            // A burst in flight freezes the buffer (its workers read it),
            // so appends stay inside the reserved window; a full window
            // drains the burst, which trims and re-reserves.
            let space = match &self.inflight {
                Some(_) => self.buf.capacity() - self.buf.len(),
                None => data.len(),
            };
            let n = space.min(data.len());
            if n == 0 {
                self.drain_inflight();
                continue;
            }
            self.buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.pos += n as u64;
            if let Some(hi) = self.burst_hi() {
                self.fire_burst(hi);
            }
        }
        // Surface a worker panic at the write that fired the burst — the
        // timing the blocking model used to give.
        if let Some(ctx) = &self.inflight {
            if ctx.poison.lock().unwrap().is_some() {
                self.drain_inflight();
            }
        }
    }

    /// Close the frame: the pending jobs (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.drain_inflight();
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
        // A flush promises visibility: the in-flight burst's blocks belong
        // to the output even when no new bytes are pending.
        self.drain_inflight();
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

    /// Growing grid: end of the epoch containing `o`. Bursts fire here, so
    /// in the steady state one burst encodes exactly one epoch.
    fn growing_epoch_end(&self, o: u64) -> u64 {
        let (lo, size) = self.growing_epoch(o);
        lo + self.burst_jobs as u64 * size
    }

    /// End offset of the next burst, once enough is fed: the fixed grid
    /// fires on `burst_jobs` complete jobs (keeping the pledged final job
    /// held back for finish), the growing grid fires on epoch completion.
    fn burst_hi(&self) -> Option<u64> {
        debug_assert!(self.pos >= self.job_start);
        match self.grid {
            JobGrid::Fixed(size) => {
                let size = size as u64;
                let complete = (self.pos - self.job_start) / size;
                if complete < self.burst_jobs as u64 {
                    return None;
                }
                let jobs = match self.shape.len {
                    Some(n) if self.pos <= n => {
                        let pledged_jobs = (n - self.job_start).div_ceil(size);
                        complete.min(pledged_jobs.saturating_sub(1))
                    },
                    _ => complete,
                };
                Some(self.job_start + jobs * size)
            },
            JobGrid::Growing => {
                let hi = self.growing_epoch_end(self.job_start);
                (self.pos >= hi).then_some(hi)
            },
        }
    }

    /// Encode the jobs covering [job_start, hi) and advance the grid: the job
    /// ending at `hi` carries the frame's last-block flag when
    /// `last_frame_block` is set (only finish passes true). `hi` must not
    /// exceed `pos`; when it lands mid-job (flush, or the finish tail) the
    /// last job is short.
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

    /// Encode the jobs covering [job_start, hi) synchronously: the
    /// flush/finish path, where the caller wants the bytes on return.
    fn encode_jobs(&mut self, hi: u64, last_frame_block: bool) {
        debug_assert!(self.job_start < hi && hi <= self.pos);
        self.drain_inflight();
        self.emit_header();
        let bounds = self.build_bounds(hi);
        if bounds.len() == 2 {
            self.run_inline_job(&bounds, hi, last_frame_block);
        } else {
            self.fire_jobs(bounds, hi, last_frame_block);
            self.drain_inflight();
        }
    }

    /// The steady write path's sibling of [`Self::encode_jobs`]: drains the
    /// previous burst (by now the workers are usually long done), then
    /// posts the new one and returns while they encode — the calling thread
    /// goes straight back to accumulating, overlapping the pump with the
    /// burst instead of serializing behind its barrier. A single short job
    /// still runs inline (no pool spun up for it).
    fn fire_burst(&mut self, hi: u64) {
        debug_assert!(self.job_start < hi && hi <= self.pos);
        self.drain_inflight();
        self.emit_header();
        let bounds = self.build_bounds(hi);
        if bounds.len() == 2 {
            self.run_inline_job(&bounds, hi, false);
        } else {
            self.fire_jobs(bounds, hi, false);
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
        self.trim_buf();
    }

    /// Post a multi-job burst to the pool and return without waiting: the
    /// burst lands in `inflight` and is assembled by
    /// [`Self::drain_inflight`]. The next append window is reserved before
    /// posting — the frozen buffer cannot realloc while the workers read
    /// it, so `write` can keep appending inside the window.
    fn fire_jobs(&mut self, bounds: Vec<u64>, hi: u64, last_frame_block: bool) {
        debug_assert!(self.inflight.is_none());
        let n_jobs = bounds.len() - 1;
        let burst_start = bounds[0];
        let strip_lo = burst_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= strip_lo);
        let first = (burst_start - strip_lo) as usize;
        let pool_size = self.ensure_workers(n_jobs);
        let burst_job_start = self.job_start;
        self.job_start = hi;
        self.reserve_next_window();
        // SAFETY: posted for the burst and frozen until the pool quiesces
        // (see FrozenSrc); the reservation above already happened with the
        // pool idle, so the pointer is stable for the whole burst.
        let src =
            unsafe { slice::from_raw_parts(self.buf.as_ptr(), (hi - self.buf_base) as usize) };
        let ctx = Arc::new(BurstCtx {
            src: FrozenSrc {
                ptr: src.as_ptr(),
                len: src.len(),
            },
            first,
            bounds,
            n_jobs,
            job_start: burst_job_start,
            overlap: self.overlap,
            level: self.level,
            last_frame_block,
            shape: self.shape,
            slots: (0..n_jobs).map(|_| Mutex::new(None)).collect(),
            ready: Condvar::new(),
            next_job: AtomicUsize::new(0),
            poison: Mutex::new(None),
            exited: AtomicUsize::new(pool_size),
            quiesce: Mutex::new(()),
            quiesce_cv: Condvar::new(),
        });
        {
            let mut inner = self.shared.inner.lock().unwrap();
            // The previous burst drained before this one posts, so no
            // worker still holds an older ctx.
            debug_assert!(inner.ctx.is_none());
            inner.generation += 1;
            inner.ctx = Some(ctx.clone());
        }
        self.shared.wake.notify_all();
        // Checksum absorb on the calling thread while the workers encode.
        self.hash_to(hi);
        self.inflight = Some(ctx);
    }

    /// Drop the consumed buffer prefix once the in-flight burst has
    /// quiesced (an acquire load of the exit countdown — the same freeze
    /// guarantee `drain_inflight` waits for). Compacting early keeps the
    /// prefix drain at strip scale; deferring it to the next fire would
    /// move a whole epoch of pending bytes.
    fn try_compact(&mut self) {
        let Some(ctx) = &self.inflight else {
            return;
        };
        if ctx.exited.load(Ordering::Acquire) != 0 {
            return;
        }
        let keep = self.job_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= keep);
        let drop = (keep - self.buf_base) as usize;
        if drop > 0 {
            self.buf.drain(..drop);
            self.buf_base = keep;
        }
    }

    /// Assemble the in-flight burst's blocks in order, wait for the pool to
    /// quiesce (the source buffer unfreezes), trim, and surface a worker
    /// panic if any.
    fn drain_inflight(&mut self) {
        let Some(ctx) = self.inflight.take() else {
            return;
        };
        for slot in &ctx.slots {
            let mut guard = slot.lock().unwrap();
            while guard.is_none() {
                guard = ctx.ready.wait(guard).unwrap();
            }
            self.output.extend_from_slice(&guard.take().unwrap());
        }
        {
            let mut guard = ctx.quiesce.lock().unwrap();
            while ctx.exited.load(Ordering::Acquire) != 0 {
                guard = ctx.quiesce_cv.wait(guard).unwrap();
            }
        }
        self.shared.inner.lock().unwrap().ctx = None;
        self.trim_buf();
        if let Some(payload) = ctx.poison.lock().unwrap().take() {
            std::panic::resume_unwind(payload);
        }
    }

    /// Reserve buffer space for everything the next burst will accumulate
    /// (plus its strip), so appends never realloc while a burst is in
    /// flight. Called with the pool idle (before posting); a fixed grid
    /// bounds the next burst at `burst_jobs` complete jobs plus the held
    /// back one, the growing grid at its epoch end.
    fn reserve_next_window(&mut self) {
        let next_hi = match self.grid {
            JobGrid::Fixed(size) => self.job_start + (self.burst_jobs as u64 + 1) * size as u64,
            JobGrid::Growing => self.growing_epoch_end(self.job_start),
        };
        let want = (next_hi - self.buf_base) as usize + self.overlap + 64 * 1024;
        if self.buf.capacity() < want {
            self.buf.reserve_exact(want - self.buf.len());
            advise_hugepages(&self.buf);
        }
    }

    /// Retain exactly the strip the next job will prefill from, dropping
    /// everything older: the pending bytes slide to the buffer head, no
    /// separate window copy. The buffer is then sized for the whole next
    /// burst in one reservation: epoch-sized growth otherwise arrives as a
    /// long run of Vec-doubling reallocs, whose recopy tail costs more than
    /// the encoding it precedes.
    fn trim_buf(&mut self) {
        let keep = self.job_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= keep);
        let drop = (keep - self.buf_base) as usize;
        if drop > 0 {
            self.buf.drain(..drop);
            self.buf_base = keep;
        }
        if let JobGrid::Growing = self.grid {
            // Space for the strip plus every job of the epoch about to
            // accumulate, plus one write chunk of slack.
            let want = (self.growing_epoch_end(self.job_start) - self.buf_base) as usize
                + self.overlap
                + 64 * 1024;
            if self.buf.capacity() < want {
                self.buf.reserve_exact(want - self.buf.len());
                advise_hugepages(&self.buf);
            }
        }
    }

    /// Absorb [hashed_end, end) into the frame checksum.
    fn hash_to(&mut self, end: u64) {
        debug_assert!(self.hashed_end <= end && end <= self.pos);
        let range = (self.hashed_end - self.buf_base) as usize..(end - self.buf_base) as usize;
        self.hasher.hash_tail(&self.buf[range]);
        self.hashed_end = end;
    }

    /// Spawn the persistent pool workers on the first multi-job burst and
    /// return the live thread count (the `exited` countdown's size).
    fn ensure_workers(&mut self, n_jobs: usize) -> usize {
        if self.pool_threads.is_empty() {
            let n = (self.workers as usize).min(n_jobs).max(1);
            for _ in 0..n {
                let shared = self.shared.clone();
                self.pool_threads.push(
                    std::thread::Builder::new()
                        .spawn(move || pool_worker(shared))
                        .unwrap(),
                );
            }
        }
        self.pool_threads.len()
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
        // pool (see take_pooled_buf).
        let mut buf = core::mem::take(&mut self.buf);
        return_pooled_buf(core::mem::take(&mut buf));
    }
}

/// Advise MADV_HUGEPAGE over the buffer's whole capacity: the accumulate
/// buffer is written densely at burst scale, and on THP=madvise machines its
/// first touch otherwise pays one 4 KiB fault per page — milliseconds at the
/// spans the growing grid reaches. Pure advice: THP=always machines map it
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
