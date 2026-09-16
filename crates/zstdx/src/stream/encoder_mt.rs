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
//!
//! A reach-probe-eligible frame (see [`reach_probe`]: the Balanced row with
//! its stock reach) donates the probe's keep side to a pool worker at the
//! staging gate (see `post_donation`): the worker runs job zero's first
//! [`reach_probe::PROBE_SPAN`] bytes through the job machinery itself —
//! entropy feedback, LDM and the incompressibility gate included — while
//! the pump keeps filling the buffer, and only the shrink side parses
//! separately. The verdict is consumed lazily, wherever the schedule first
//! needs it (a post coming due, or a flush/finish); a shrunk frame re-grids
//! from offset zero with the shrunk strip the bulk path uses, and a kept
//! frame's job zero continues the donated state past the span (the
//! continuation skips the strip prefill — it would clear the tables the
//! donation built) while keeping the stream strip (see
//! [`MatchGeneratorDriver::stream_overlap_for`] — the chain rows'
//! cross-job LDM carrier, deliberately wider than the bulk strip). A
//! pledge clears the probe's size gate at construction, so the head stages
//! at [`reach_probe::PROBE_SPAN`]; an open-ended stream cannot know its
//! length, so the decision waits for [`reach_probe::PROBE_MIN_FRAME`]
//! streamed bytes. A stream that ends or flushes before the gate keeps the
//! stock reach.

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
        frame_compressor::{
            BlockChecksum as _, CompressState, FrameHasher, compress_job_blocks_inner,
            new_slice_state,
        },
        frame_header::FrameHeader,
        match_generator::MatchGeneratorDriver,
        mt::{MAX_JOB_SIZE, MIN_JOB_SIZE, donate_keep_span, job_size_for, run_job_with},
        reach_probe::{self, ProbeFeedback, ReachChoice},
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

/// The reach probe's donated keep side (see `post_donation`): job zero's
/// parsed span as a continuation-ready state plus its encoded blocks. The
/// verdict is taken at `resolve_probe`; a Shrink discards both — job zero
/// parses undonated, from a pooled state like any other job.
struct Donation {
    state: alloc::boxed::Box<CompressState<MatchGeneratorDriver>>,
    prefix: Vec<u8>,
}

/// The keep-side task's parked result (the verdict needs the shrink side's
/// cost too — see `JobKind::DonateShrink`).
struct KeepOutcome {
    keep_bits: f64,
    donation: Donation,
}

/// One posted job: the frozen source view (see the recycling rules in
/// [`MtEncoderCore`]'s docs — the backing bytes cannot move before this job
/// completes) plus its payload.
struct Job {
    level: Level,
    /// The frame's reach probe decision (see [`reach_probe`]), shared by
    /// every job so the frame's jobs and its header agree.
    choice: ReachChoice,
    shape: crate::InputShape,
    /// The donation tasks' owned copy of the frame's first
    /// [`reach_probe::PROBE_SPAN`] bytes, shared between the two: owned,
    /// not borrowed, so neither holds a buffer view — donations do not
    /// count in `n_incomplete` and never gate buffer recycling.
    head: Option<Arc<[u8]>>,
    kind: JobKind,
}

enum JobKind {
    /// Encode `[first, len)` of the frozen source view as the job's own
    /// blocks.
    Encode {
        /// The frozen source view (see the recycling rules in
        /// [`MtEncoderCore`]'s docs — the backing bytes cannot move before
        /// this job completes).
        src: FrozenSrc,
        /// Index of the job's strip start inside `src`, and the job's own
        /// range relative to it.
        first: usize,
        len: usize,
        /// Every job except the frame's first starts where the decoder's
        /// repcode history is unknown (see the bulk mt path); a
        /// flush-rebased grid starts mid-frame too.
        gate: bool,
        last_frame_block: bool,
        overlap: usize,
        /// The probe's donated keep side when this job is the frame's
        /// first: the continuation resumes the donated state at the span
        /// (skipping the strip prefill — it would clear the very tables
        /// the donation built) and emits the encoded prefix verbatim.
        donation: Mutex<Option<Donation>>,
        out: Mutex<Option<Vec<u8>>>,
        /// Set (Release) once `out` holds the job's bytes: the assembling
        /// side polls this instead of taking the mutex per tick.
        done: core::sync::atomic::AtomicBool,
    },
    /// The probe's keep side over the frame's first
    /// [`reach_probe::PROBE_SPAN`] bytes (see `donate_keep_span`) — job
    /// zero's own blocks, parsed and encoded on a pool worker while the
    /// pump keeps filling the buffer.
    DonateKeep {
        outcome: Mutex<Option<KeepOutcome>>,
        done: core::sync::atomic::AtomicBool,
    },
    /// The probe's shrink side: the throwaway W12 measurement parse on a
    /// second worker (see `parse_cost_with`) — the verdict's other input,
    /// run concurrently with the keep side so neither serializes behind
    /// the other (together they are a full keep-side parse plus a
    /// shrink-side parse, more serial time than the frame's tail can hide
    /// on one worker).
    DonateShrink {
        outcome: Mutex<Option<f64>>,
        done: core::sync::atomic::AtomicBool,
    },
}

impl Job {
    /// Mark this job completed without running it (the poison abort path):
    /// encode slots fill empty so the ordered assembly runs to completion
    /// before the panic resumes; a donation simply never lands an outcome.
    fn abort(&self) {
        match &self.kind {
            JobKind::Encode { out, done, .. } => {
                *out.lock().unwrap() = Some(Vec::new());
                done.store(true, Ordering::Release);
            },
            JobKind::DonateKeep { done, .. } | JobKind::DonateShrink { done, .. } => {
                done.store(true, Ordering::Release);
            },
        }
    }

    /// Whether the job has published its result (bytes or measurement).
    fn is_done(&self) -> bool {
        match &self.kind {
            JobKind::Encode { done, .. }
            | JobKind::DonateKeep { done, .. }
            | JobKind::DonateShrink { done, .. } => done.load(Ordering::Acquire),
        }
    }
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
            run_claimed_job(&job, &mut state)
        }));
        match attempt {
            Ok(()) => {
                mark_done(&job);
            },
            Err(payload) => {
                // The state may be mid-compress garbage: drop it rather
                // than reuse. The slot is filled with an empty block so the
                // ordered assembly can run to completion before the panic
                // is resumed.
                state = None;
                poisoned = true;
                job.abort();
                shared.poisoned.store(true, Ordering::Release);
                shared.inner.lock().unwrap().poison = Some(payload);
            },
        }
        if matches!(job.kind, JobKind::Encode { .. }) {
            // Only encode jobs hold buffer views; a donation's completion
            // is a progress tick alone (see `wait_donation`).
            shared.n_incomplete.fetch_sub(1, Ordering::Release);
        }
        shared.completed.fetch_add(1, Ordering::AcqRel);
        shared.progressed.notify_all();
        if poisoned {
            return;
        }
    }
}

/// Encode or donate one claimed job through a worker-local (or donated)
/// state, publishing the bytes (encode) or the parked verdict (donation).
fn run_claimed_job(
    job: &Job,
    state: &mut Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>,
) {
    match &job.kind {
        JobKind::Encode {
            src: frozen,
            first,
            len,
            gate,
            last_frame_block,
            overlap,
            donation,
            out,
            ..
        } => {
            // SAFETY: the posting thread keeps the backing bytes stable for
            // this job's whole lifetime (see FrozenSrc).
            let src = unsafe { slice::from_raw_parts(frozen.ptr, *len) };
            let bytes = if let Some(don) = donation.lock().unwrap().take() {
                // The donated continuation: job zero's state already carries
                // its reset, strip prefill and parsed span — the
                // continuation emits from where the donation stopped. The
                // state returns to the kit pool afterwards (the worker
                // builds its own like any undonated job's would).
                let mut dstate = don.state;
                let out = compress_job_blocks_inner(
                    &mut dstate,
                    src,
                    *first..*len,
                    *overlap,
                    *last_frame_block,
                    reach_probe::PROBE_SPAN,
                    don.prefix,
                );
                crate::encoding::mt::return_donation_state(dstate);
                out
            } else {
                let state = state.get_or_insert_with(|| alloc::boxed::Box::new(new_slice_state()));
                run_job_with(
                    state,
                    src,
                    *first..*len,
                    *overlap,
                    *last_frame_block,
                    job.level,
                    *gate,
                    job.shape,
                    job.choice,
                )
            };
            *out.lock().unwrap() = Some(bytes);
        },
        JobKind::DonateKeep { outcome, .. } => {
            let head = job.head.as_deref().expect("a donation carries the head");
            // The kit pool (see `mt`'s DONATION_KIT docs): ephemeral pool
            // workers carry no warm tables, so the donation's state
            // round-trips a process-global pool instead.
            let mut st = crate::encoding::mt::take_donation_state();
            let (keep_bits, prefix) = donate_keep_span(&mut st, head, job.level, job.shape);
            *outcome.lock().unwrap() = Some(KeepOutcome {
                keep_bits,
                donation: Donation { state: st, prefix },
            });
        },
        JobKind::DonateShrink { outcome, .. } => {
            let head = job.head.as_deref().expect("a donation carries the head");
            let mut probe = crate::encoding::mt::take_donation_probe();
            let shrink = reach_probe::parse_cost_with(
                &mut probe,
                head,
                job.level,
                job.shape,
                ReachChoice::Shrink,
                ProbeFeedback::Approx,
            );
            crate::encoding::mt::return_donation_probe(probe);
            *outcome.lock().unwrap() = Some(shrink);
        },
    }
}

/// Publish a successfully run job's result (Release-paired with
/// [`Job::is_done`]).
fn mark_done(job: &Job) {
    match &job.kind {
        JobKind::Encode { done, .. }
        | JobKind::DonateKeep { done, .. }
        | JobKind::DonateShrink { done, .. } => {
            done.store(true, Ordering::Release);
        },
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
    /// Whether the reach probe's decision (see [`reach_probe`]) is still
    /// pending: no job posts until the head is staged and decided.
    probe_pending: bool,
    /// The posted donation tasks whose verdict has not been consumed yet
    /// (see `post_donation`/`resolve_probe`): the keep and shrink sides.
    probe_wait: Option<(Arc<Job>, Arc<Job>)>,
    /// The consumed Keep verdict's continuation for job zero (donated
    /// state plus encoded span), parked until job zero posts.
    donation: Option<Donation>,
    /// The frame's reach decision, handed to every job (see
    /// [`reach_probe`]).
    choice: ReachChoice,
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
        // The reach probe (see reach_probe): a probe-eligible frame defers
        // its schedule until the probe's head is staged. A pledge clears
        // the probe's size gate here; an open-ended stream gates at
        // PROBE_MIN_FRAME streamed bytes (see the module docs).
        let probe_pending = match options.pledged_size {
            Some(_) => reach_probe::eligible(options.level, shape),
            None => MatchGeneratorDriver::reach_probe_eligible(options.level, shape),
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
        // consecutive streams reuse the already-faulted allocation. A
        // pending probe reserves only its staging scale (the probe head
        // plus a first shrunk epoch); the decision re-reserves the decided
        // schedule's scale.
        let want = if probe_pending {
            (options.workers as usize).max(2) * MIN_JOB_SIZE + reach_probe::PROBE_SPAN + 64 * 1024
        } else {
            (options.workers as usize).max(2) * initial_job + overlap + 64 * 1024
        };
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
            probe_pending,
            probe_wait: None,
            donation: None,
            choice: ReachChoice::Keep,

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

    /// The probe decision's staging gate (see [`reach_probe`]): a pledged
    /// frame stages its head at [`reach_probe::PROBE_SPAN`] (the pledge
    /// already cleared the probe's size gate), an open-ended stream must
    /// stream [`reach_probe::PROBE_MIN_FRAME`] bytes before the probe's
    /// fixed cost amortizes.
    fn probe_gate(&self) -> u64 {
        if self.shape.len.is_some() {
            reach_probe::PROBE_SPAN as u64
        } else {
            reach_probe::PROBE_MIN_FRAME
        }
    }

    /// Post the probe's keep-side donation (see [`reach_probe`],
    /// `donate_span`): a pool worker runs job zero's first span blocks
    /// through the job machinery and parks the verdict, while the pump
    /// keeps filling the buffer — only a keep-side parse's worth of work
    /// runs at all, and it overlaps the pump instead of stalling it. The
    /// verdict is consumed lazily wherever the schedule needs it (see
    /// `resolve_probe`). Nothing else may post while the probe is pending,
    /// so the donated head still sits at the buffer's start; the donation
    /// counts in `n_incomplete`, so buffer recycling waits it out.
    fn post_donation(&mut self) {
        self.probe_pending = false;
        debug_assert_eq!(self.job_start, 0);
        debug_assert_eq!(self.buf_base, 0);
        debug_assert!(self.pos >= reach_probe::PROBE_SPAN as u64);
        self.ensure_workers();
        // The donation owns its head: no buffer view is held, so the pump's
        // wrap/growth never waits on it (a shrink-class verdict trails the
        // pump by a whole keep-side parse — gating buffer growth on it
        // would stall the pump behind the very work the donation was meant
        // to hide).
        let head: Arc<[u8]> = Arc::from(
            self.buf[..reach_probe::PROBE_SPAN]
                .to_vec()
                .into_boxed_slice(),
        );
        let done = || core::sync::atomic::AtomicBool::new(false);
        let keep = Arc::new(Job {
            level: self.level,
            choice: ReachChoice::Keep,
            shape: self.shape,
            head: Some(head.clone()),
            kind: JobKind::DonateKeep {
                outcome: Mutex::new(None),
                done: done(),
            },
        });
        let shrink = Arc::new(Job {
            level: self.level,
            choice: ReachChoice::Keep,
            shape: self.shape,
            head: Some(head),
            kind: JobKind::DonateShrink {
                outcome: Mutex::new(None),
                done: done(),
            },
        });
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.queue.push_back(keep.clone());
            inner.queue.push_back(shrink.clone());
        }
        self.probe_wait = Some((keep, shrink));
        self.shared.wake.notify_all();
    }

    /// Settle the reach decision at a flush/finish boundary: a frame that
    /// never cleared the staging gate keeps the stock reach (the schedule's
    /// pure-function property — the verdict was never taken), and a posted
    /// donation is waited out and applied.
    fn settle_probe(&mut self) {
        if self.probe_pending {
            self.probe_pending = false;
            self.reserve_decided();
        }
        if self.probe_wait.is_some() {
            self.resolve_probe();
        }
    }

    /// Consume the donation's verdict: a shrunk frame re-grids from offset
    /// zero with the shrunk history strip and the job floor it implies
    /// (job zero parses undonated, from a pooled state), a kept frame parks
    /// the donated state and prefix for job zero's continuation. Every
    /// outcome re-reserves the decided schedule's working scale (the
    /// pending reserve covers only the head).
    fn resolve_probe(&mut self) {
        let Some((keep_job, shrink_job)) = self.probe_wait.take() else {
            return;
        };
        // Wait both sides out (they hold no buffer views and do not count
        // in n_incomplete, so the quiesce-based waits cannot see them). An
        // aborted task lands done without an outcome, and the unwind
        // resumes below.
        while !keep_job.is_done() {
            self.wait_donation(&keep_job);
        }
        while !shrink_job.is_done() {
            self.wait_donation(&shrink_job);
        }
        let keep = match &keep_job.kind {
            JobKind::DonateKeep { outcome, .. } => outcome.lock().unwrap().take(),
            _ => unreachable!("probe_wait holds the donation tasks"),
        };
        let shrink = match &shrink_job.kind {
            JobKind::DonateShrink { outcome, .. } => outcome.lock().unwrap().take(),
            _ => unreachable!("probe_wait holds the donation tasks"),
        };
        let (Some(keep), Some(shrink)) = (keep, shrink) else {
            self.surface_poison();
            return;
        };
        // The verdict runs here on the pump: its feedback escalation (the
        // contested band only) reuses this thread's pooled probe driver,
        // warm across encoders.
        let choice = reach_probe::decide_donated(
            keep.keep_bits,
            shrink,
            &self.buf[..reach_probe::PROBE_SPAN],
            self.level,
            self.shape,
        );
        self.choice = choice;
        if choice == ReachChoice::Shrink {
            crate::encoding::mt::return_donation_state(keep.donation.state);
            self.overlap = reach_probe::SHRINK_REACH;
            if let JobGrid::Fixed(_) = self.grid {
                let n = self.shape.len.expect("the gate saw a pledge");
                self.grid = JobGrid::Fixed(job_size_for(n, self.workers, self.overlap));
            }
        } else {
            self.donation = Some(keep.donation);
        }
        // Nothing else is in flight at this point (nothing posts while the
        // verdict is unconsumed and the donation tasks just completed), so
        // the growth below needs no quiesce wait.
        self.reserve_decided();
    }

    /// Re-reserve the accumulate buffer for the decided schedule's working
    /// scale (see `MtEncoderCore::new`).
    fn reserve_decided(&mut self) {
        let initial_job = match self.grid {
            JobGrid::Fixed(size) => size,
            JobGrid::Growing => MIN_JOB_SIZE.max(self.overlap),
        };
        let want = (self.workers as usize).max(2) * initial_job + self.overlap + 64 * 1024;
        if self.buf.capacity() < want {
            let target = want.max((self.buf.capacity() * 2).min(BUF_CAP_MAX));
            self.buf.reserve_exact(target - self.buf.len());
            advise_hugepages(&self.buf);
        }
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
        if self.probe_pending {
            // Nothing posts until the probe's head is staged and decided
            // (see the module docs) — the schedule below must never see a
            // pending frame. The donation itself posts here and its
            // verdict is consumed lazily, only where the schedule needs it.
            if self.pos < self.probe_gate() {
                return;
            }
            self.post_donation();
        }
        loop {
            // A verdict that already landed is consumed for free here (the
            // finish path then never waits on it); a pending one is only
            // waited out where the schedule needs it — the post below.
            if self
                .probe_wait
                .as_ref()
                .is_some_and(|(k, s)| k.is_done() && s.is_done())
            {
                self.resolve_probe();
            }
            let end = self.job_end(self.job_start);
            if !self.post_due(end) {
                return;
            }
            if self.probe_wait.is_some() {
                // The verdict re-derives the grid (a Shrink re-grids from
                // offset zero) — recompute the schedule before posting.
                self.resolve_probe();
                continue;
            }
            self.post_job(self.job_start, end, false);
            self.job_start = end;
        }
    }

    /// Whether the job starting at `job_start` (ending at `end`) may post
    /// now. The fixed grid's boundaries never depend on the posting cadence
    /// (its build_bounds never re-slices), so each job posts the moment it
    /// completes — except a pledged grid's final job, held back for
    /// `finish` to mark last (unlocked on overshoot, where the bulk path
    /// also stops holding). The growing grid posts an epoch's jobs only
    /// once the whole epoch is buffered — rechecked per job, because a
    /// large write (or an oversized pooled buffer) can carry `pos` past
    /// several epoch ends in one append: greedily posting an incomplete
    /// epoch's jobs would change both the stream-end tail re-slice and the
    /// bytes.
    fn post_due(&self, end: u64) -> bool {
        match self.grid {
            JobGrid::Fixed(_) => {
                if self.pos < end {
                    return false;
                }
                if let Some(n) = self.shape.len {
                    if end >= n && self.pos <= n {
                        return false;
                    }
                }
                true
            },
            JobGrid::Growing => self.pos >= self.growing_epoch_end(self.job_start),
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
        // A flush or finish ahead of the staging gate decides the probe
        // with the stock reach (see the module docs).
        self.settle_probe();
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
        // A donation implies the pool was spawned at its post, so this
        // pool-less path never carries one (job zero would parse undonated
        // — byte-identical, but the donation's work would be wasted).
        debug_assert!(self.donation.is_none() || !self.pool_threads.is_empty());
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
            self.choice,
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
        // Job zero carries the probe's donated keep side when one was
        // parked (see `resolve_probe`); a donation whose continuation
        // cannot take the last-block flag (the job ends at the span, so
        // the donated blocks' non-last emission would stand) is discarded —
        // job zero parses undonated, byte-identical to the stock schedule.
        let mut donation = if start == 0 {
            self.donation.take()
        } else {
            debug_assert!(self.donation.is_none());
            None
        };
        if donation
            .as_ref()
            .is_some_and(|_| last_frame_block && end <= reach_probe::PROBE_SPAN as u64)
        {
            let discarded = donation.take().unwrap();
            crate::encoding::mt::return_donation_state(discarded.state);
        }
        // SAFETY: strip_lo >= buf_base (asserted above); the view is the
        // buffer's [strip_lo, end), so `first` indexes from its head.
        let ptr = unsafe { self.buf.as_ptr().add((strip_lo - self.buf_base) as usize) };
        let job = Arc::new(Job {
            level: self.level,
            choice: self.choice,
            shape: self.shape,
            head: None,
            kind: JobKind::Encode {
                src: FrozenSrc { ptr },
                first,
                len,
                overlap: self.overlap,
                last_frame_block,
                gate: start > 0,
                donation: Mutex::new(donation),
                out: Mutex::new(None),
                done: core::sync::atomic::AtomicBool::new(false),
            },
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
        while self.pending.front().is_some_and(|j| j.is_done()) {
            let job = self.pending.pop_front().unwrap();
            let bytes = match &job.kind {
                JobKind::Encode { out, .. } => out.lock().unwrap().take().unwrap(),
                JobKind::DonateKeep { .. } | JobKind::DonateShrink { .. } => {
                    unreachable!("donations never pend assembly")
                },
            };
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

    /// Wait for the donation's completion tick: the donating worker ticks
    /// `progressed` (and `completed`) when it lands the verdict. Exit
    /// conditions are re-checked on every wake — a completion that lands
    /// between the caller's `is_done` check and the `completed` snapshot
    /// below fires its notify before this wait parks, so the snapshot is
    /// already final and the delta test would sleep on a condition that
    /// never changes again; the done re-check and the bounded timeout keep
    /// the wait correct either way.
    fn wait_donation(&self, job: &Job) {
        let mut inner = self.shared.inner.lock().unwrap();
        loop {
            if inner.poison.is_some() || inner.shutdown || job.is_done() {
                return;
            }
            let before = self.shared.completed.load(Ordering::Acquire);
            let (guard, _) = self
                .shared
                .progressed
                .wait_timeout(inner, core::time::Duration::from_millis(100))
                .unwrap();
            inner = guard;
            if job.is_done() || self.shared.completed.load(Ordering::Acquire) != before {
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
                        job.abort();
                        if matches!(job.kind, JobKind::Encode { .. }) {
                            self.shared.n_incomplete.fetch_sub(1, Ordering::Release);
                        }
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
