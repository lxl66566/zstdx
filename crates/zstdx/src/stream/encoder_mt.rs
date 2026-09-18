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
//!
//! The chain rows' whole-window strip carries exactly one reader beyond
//! the row's search domain: LDM's far class (the chain walk rejects
//! beyond-reach candidates on its own). A frame both far-class screens
//! reject (see [`FarClass`]) caps every job's strip at the row's chain
//! reach and disarms job LDM — the capped bytes had no reader left, so
//! the frame bytes cannot move.

use alloc::{sync::Arc, vec::Vec};
use core::{
    slice,
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    any::Any,
    collections::VecDeque,
    sync::{Condvar, Mutex},
    time::Duration,
};

use super::{
    encoder_core::{StreamChecksum, write_pending_output},
    mt_pool::{
        WorkerSlot, WorkerSlotState, assign_lease, leave_lease, pool_thread, return_pooled_state,
        take_parked_slot, take_pooled_state, wait_leave,
    },
};
use crate::{
    EncoderOptions, Level,
    blocks::block::BlockType,
    common::MAX_BLOCK_SIZE,
    encoding::{
        block_header::BlockHeader,
        checksum::{BlockChecksum as _, FrameHasher},
        frame_compressor::{CompressState, compress_job_blocks_inner, reset_slice_state},
        frame_header::FrameHeader,
        match_generator::{
            LdmArming, MatchGeneratorDriver, SPF_MIN_PREFIX, SPF_SEG, StripSnapshot,
            far_repeat_dominant, ldm_head_parses,
        },
        mt::{
            MAX_JOB_SIZE, MIN_JOB_SIZE, donate_keep_span, job_size_for, prepare_job_state,
            run_job_with,
        },
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

/// The frame's far-class screen verdict (see `MtEncoderCore::far_class`):
/// whether the chain rows' whole-window cross-job strip carries a paying
/// LDM far class at all. The chain walk rejects beyond-reach candidates,
/// so the strip past the row's chain reach has exactly one reader — LDM;
/// a frame both screens reject (the head engagement screen and the
/// far-repeat veto, see `resolve_far_class`) never pays that reader, and
/// it caps every job's strip at the reach with job LDM disarmed.
/// Byte-exact on such frames: the chain candidates the cap drops were
/// beyond-reach (never emittable), LDM candidates never won (a far twin
/// always trailed a within-reach twin the chain found first), and the
/// strip's seed scan resolves nearest-first (a far-dead span's repeats
/// sit far closer than the reach).
enum FarClass {
    /// No strip-defining site has run yet.
    Pending,
    /// The frame shows a paying far class (or the row screens no cap at
    /// all): strips keep the whole window.
    Alive,
    /// Both screens rejected the far class: strips cap at the row's
    /// chain reach and job LDM disarms for the frame.
    Dead,
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

/// The finish tail's shared prefix fill (see `MtEncoderCore::build_spf`):
/// a snapshot of one state's strip fill covering `[0, upto)`.
struct SpfPlan {
    upto: u64,
    /// Largest job start whose strip is the whole prefix `[0, start)`: the
    /// overlap at plan build. A job starting beyond it carries a
    /// mid-stream strip `[start - overlap, start)` the snapshot's fill
    /// does not cover — it keeps the stock path.
    prefix_end: u64,
    snapshot: Arc<StripSnapshot>,
}

impl SpfPlan {
    /// Whether the tail job starting at `start` (strip `[0, start)` when a
    /// prefix) adopts the snapshot instead of filling its whole strip.
    fn covers(&self, start: u64) -> bool {
        start > self.upto && start <= self.prefix_end
    }
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
        last_frame_block: bool,
        overlap: usize,
        /// The job's LDM arming context: `JobPrefix` on the pledged
        /// mid-size capture (see `MtEncoderCore::resolve_probe`),
        /// `FarDead` when the frame's far-class screen rejected the far
        /// class (see `FarClass`), `Job` everywhere else.
        ldm: LdmArming,
        /// The shared prefix fill's snapshot for this job, when the
        /// finish tail engaged one and this job's strip extends it (see
        /// `MtEncoderCore::build_spf`).
        spf: Mutex<Option<Arc<StripSnapshot>>>,
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

// Upper bound on one direct read into the buffer's spare capacity
// (`pump_direct`): bounds how long a single pump step can defer job
// posting past a completed epoch (a grown buffer's whole spare in one read
// would defer it by the read's own memcpy span).
#[cfg(feature = "std")]
const PUMP_READ_MAX: usize = 1024 * 1024;

// Upper bound on the accumulate buffer's growth; past it the dead-prefix
// wrap alone recycles space (a wait per ~BUF_CAP_MAX bytes of stream is
// negligible). The old burst model reserved epoch-scale windows of the
// same magnitude.
const BUF_CAP_MAX: usize = 256 * 1024 * 1024;

// Take the pooled accumulate buffer, or a fresh one, with room for `want`
// bytes (a larger pooled buffer is kept as-is — its spare capacity only
// helps the growth reserve). The returned length is the buffer's
// ever-initialized extent: a returned buffer carries it as its Vec length,
// so the direct pump can hand out spare bytes without re-zeroing them.
fn take_pooled_buf(want: usize) -> (Vec<u8>, usize) {
    BUF_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        let best = pool
            .iter()
            .rposition(|b| b.capacity() >= want)
            .or_else(|| pool.iter().rposition(|_| true));
        match best {
            Some(i) => {
                let mut buf = pool.swap_remove(i);
                let init_len = buf.len().min(buf.capacity());
                buf.clear();
                (buf, init_len)
            },
            None => (Vec::new(), 0),
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
pub(super) struct QueueShared {
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
}

/// Serve one leased queue: claim the oldest posted job, encode it through
/// a worker-local state (reused across jobs — the pooled matcher tables
/// are the expensive part), publish the bytes, and count the completion.
/// Returns whether this thread must retire (its own job panicked); the
/// worker-local state stays with the thread for its next lease (every job
/// clears what it reads, so provenance cannot reach the bytes).
pub(super) fn serve_queue(
    shared: &QueueShared,
    state: &mut Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>,
) -> bool {
    loop {
        let job = {
            let mut inner = shared.inner.lock().unwrap();
            loop {
                if inner.poison.is_some() {
                    // The encoder is unwinding; in-flight jobs run out, but
                    // no new claims (the caller clears the queue itself).
                    // This thread is healthy — it may lease again.
                    return false;
                }
                if let Some(job) = inner.queue.pop_front() {
                    break job;
                }
                if inner.shutdown {
                    // The lease ends here: this worker's jobs all completed
                    // (the encoder drops only after every lease left), so
                    // the state is quiescent and stays for the next lease.
                    return false;
                }
                inner = shared.wake.wait(inner).unwrap();
            }
        };
        let mut poisoned = false;
        // SAFETY: the posting thread keeps the backing bytes stable for
        // this job's whole lifetime (see FrozenSrc).
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_claimed_job(&job, state);
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
                *state = None;
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
            return true;
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
            last_frame_block,
            overlap,
            ldm,
            donation,
            spf,
            out,
            ..
        } => {
            // SAFETY: the posting thread keeps the backing bytes stable for
            // this job's whole lifetime (see FrozenSrc).
            let src = unsafe { slice::from_raw_parts(frozen.ptr, *len) };
            let spf = spf.lock().unwrap().take();
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
                    None,
                );
                crate::encoding::mt::return_donation_state(dstate);
                out
            } else {
                let state = state.get_or_insert_with(take_pooled_state);
                match spf.as_ref() {
                    // Adopt the shared prefix snapshot: reset and gates as
                    // any job's, then continue the fill to this job's own
                    // strip end instead of re-running its whole prefix.
                    Some(snap) => {
                        prepare_job_state(
                            state,
                            job.level,
                            job.shape,
                            job.choice,
                            *ldm,
                            *first as u64,
                        );
                        compress_job_blocks_inner(
                            state,
                            src,
                            *first..*len,
                            *overlap,
                            *last_frame_block,
                            *first,
                            Vec::new(),
                            Some(snap),
                        )
                    },
                    _ => run_job_with(
                        state,
                        src,
                        *first..*len,
                        *overlap,
                        *last_frame_block,
                        job.level,
                        job.shape,
                        job.choice,
                        *ldm,
                        None,
                    ),
                }
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
                // Runs concurrently with the keep donation, so the cap is
                // unknown here; waiting for it would serialize the pair.
                None,
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

// Independent per-stream flags (checksum, pending probe, emitted header,
// finished) that clear at different points, not one state machine.
#[allow(clippy::struct_excessive_bools)]
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
    /// The jobs' LDM arming context: flipped to [`LdmArming::JobPrefix`]
    /// when a pledged keep-class frame's verdict re-grids it onto the bulk
    /// mid-size capture (see [`Self::resolve_probe`]), or to
    /// [`LdmArming::FarDead`] when the far-class screen (see
    /// [`Self::far_class`]) rejects the far class.
    job_ldm: LdmArming,
    /// The frame's far-class screen (see [`FarClass`]): resolved once, at
    /// the first strip-defining site after the probe machinery settles,
    /// over fixed input spans (see [`Self::resolve_far_class`]) — a pure
    /// function of the input, never of the write cadence.
    far_class: FarClass,
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
    /// Ever-initialized extent of `buf` ([0, init_len) all initialized):
    /// the direct pump hands out only previously-written bytes (the Read
    /// contract forbids uninitialized buffers). Monotone under wraps;
    /// clamped to the live length by growth (a realloc copies only the
    /// live bytes); carried across encoders by the buffer pool (the
    /// returned Vec's length).
    init_len: usize,
    /// Total bytes fed.
    pos: u64,
    /// Checksum absorbed up to this absolute offset.
    hashed_end: u64,
    output: Vec<u8>,
    /// Consumed prefix of `output` (see the ST core's field).
    out_read: usize,
    header_emitted: bool,
    finished: bool,
    /// Pool coordination state, shared with the leased worker threads.
    /// Workers are leased lazily at the first post; until then this is
    /// only the state pool.
    shared: Arc<QueueShared>,
    /// Leased worker slots, taken at the first post (parked threads from
    /// the cross-encoder pool first, fresh spawns making up the rest) and
    /// waited out at drop (`wait_leave` — the join equivalent).
    pool_threads: Vec<Arc<WorkerSlot>>,
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
        let (mut buf, mut init_len) = take_pooled_buf(want);
        if buf.capacity() < want {
            buf.reserve_exact(want - buf.len());
            advise_hugepages(&buf);
            // A realloc copies only the live bytes: the new tail starts
            // uninitialized.
            init_len = buf.len();
        }
        Self {
            level: options.level,
            checksum,
            workers: options.workers,
            job_start: 0,
            grid,
            overlap,
            job_ldm: LdmArming::Job,
            far_class: FarClass::Pending,
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
            init_len,
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

    /// Pull once from `source` straight into the buffer's spare capacity:
    /// the Read-side pump's single-copy form. The region handed to `read`
    /// is always previously-initialized bytes (`init_len` tracks the
    /// ever-initialized extent — handing out uninitialized memory violates
    /// the Read contract), extended one bounded zero-fill at a time, so a
    /// source's bytes land in the job buffer without the staging-chunk
    /// round-trip of `write` — the read path's serial memcpy halves.
    /// Recycling, posting and EOF semantics mirror `write` plus
    /// `pump_from`. One bounded read per call, so job posting stays close
    /// behind the buffered bytes (a whole grown spare in one read would
    /// defer an epoch's post by the read's own span).
    #[cfg(feature = "std")]
    pub(crate) fn pump_direct(&mut self, source: &mut impl crate::io::Read) -> crate::Result<()> {
        debug_assert!(!self.finished);
        if self.buf.len() == self.buf.capacity() {
            // A positive want keeps the growth arm engaged for the nothing-
            // to-wrap case (an all-live buffer at the cap).
            self.make_space(1);
        }
        let len = self.buf.len();
        let want = PUMP_READ_MAX.min(self.buf.capacity() - len);
        debug_assert!(want > 0);
        if self.init_len < len + want {
            // Zero-fill the next region once per buffer region: the pool
            // carries the extent across encoders, so steady-state streams
            // pay this only for freshly grown capacity.
            // SAFETY: [len, target) lies inside the allocation, past the
            // live data; writing it initializes the bytes for future reads.
            let target = (len + want).min(self.buf.capacity());
            unsafe { slice::from_raw_parts_mut(self.buf.as_mut_ptr().add(len), target - len) }
                .fill(0);
            self.init_len = target;
        }
        // SAFETY: [len, init_len) is initialized (zero-fill above or the
        // pool's carried extent) and this pump is the sole accessor past
        // the live prefix (workers read only frozen views below `len`).
        let spare = unsafe {
            slice::from_raw_parts_mut(self.buf.as_mut_ptr().add(len), self.init_len - len)
        };
        // Bound before the match: the spare borrow of the buffer must not
        // outlive the scrutinee into the arms below.
        let read = source.read(&mut spare[..want]).map_err(crate::Error::from);
        match read {
            Ok(0) => self.finish(),
            Ok(n) => {
                // SAFETY: `read` wrote n initialized bytes into spare[..n].
                unsafe { self.buf.set_len(len + n) };
                self.pos += n as u64;
                self.post_ready();
            },
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Close the frame: the pending jobs (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.surface_poison();
        if self.pos > self.job_start {
            // Post the tail before waiting: posting first lets workers
            // claim tail jobs the moment earlier ones free them, instead
            // of idling behind the last straggler of a pre-drain.
            self.encode_jobs(self.pos, true);
        } else {
            // Everything fed is already encoded (or nothing was fed):
            // assemble whatever is still pending, then close with an empty
            // raw last block (the shape the single-threaded path emits).
            self.drain_all();
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
        // the output before it returns (encode_jobs drains everything it
        // posts; with nothing new pending, the drain below covers them).
        self.surface_poison();
        if self.pos > self.job_start {
            self.encode_jobs(self.pos, false);
        } else {
            self.drain_all();
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn has_output(&self) -> bool {
        self.out_read < self.output.len()
    }

    /// Hand the encoded bytes to `w`, keeping the output buffer's
    /// allocation. A failed write keeps the undelivered tail for the
    /// caller's retry (see [`write_pending_output`]).
    pub(crate) fn write_output_to(
        &mut self,
        w: &mut impl crate::io::Write,
    ) -> Result<(), crate::io::Error> {
        let res = write_pending_output(&self.output, &mut self.out_read, w);
        if res.is_ok() {
            self.output.clear();
            self.out_read = 0;
        }
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
            self.engage_midsize_capture();
        }
        // Nothing else is in flight at this point (nothing posts while the
        // verdict is unconsumed and the donation tasks just completed), so
        // the growth below needs no quiesce wait.
        self.reserve_decided();
    }

    /// A pledged keep-class frame whose source-clamped window lands in the
    /// bulk mid-size capture's class (`prefix_ldm_window`) re-grids onto
    /// that capture: reach-based job size (the whole-window strip would
    /// floor the frame at one job and idle the workers), whole-prefix
    /// strips (the overlap already is the clamped window) and
    /// [`LdmArming::JobPrefix`] — the clamped window sits below the `Job`
    /// bar, so without the flip the far class disarms and the pledged
    /// stream pays a chain-only parse where bulk-mt captures it (dll32:
    /// 5,460,884 vs 4,390,255, a 24% gap against the pledged-equals-bulk
    /// contract). The head screen mirrors the bulk planner's
    /// (`ldm_head_parses`): low-alphabet heads keep the stock single-job
    /// schedule — their LDM cannot survive the alphabet gate, so only the
    /// job split would move (a pure boundary loss).
    fn engage_midsize_capture(&mut self) {
        if !matches!(self.grid, JobGrid::Fixed(_)) || self.shape.len.is_none() {
            return;
        }
        let Some(window) =
            MatchGeneratorDriver::prefix_ldm_window(self.level, self.shape, ReachChoice::Keep)
        else {
            return;
        };
        let head = &self.buf[..self.buf.len().min(MAX_BLOCK_SIZE as usize)];
        if !ldm_head_parses(head) {
            return;
        }
        let n = self.shape.len.expect("the pledge gate checked len");
        debug_assert_eq!(
            self.overlap as u64, window,
            "the strip is the clamped window"
        );
        let reach = MatchGeneratorDriver::strip_for_choice(self.level, self.shape, self.choice);
        self.grid = JobGrid::Fixed(job_size_for(n, self.workers, reach as usize));
        self.job_ldm = LdmArming::JobPrefix;
    }

    /// Resolve the far-class screen once per frame (see [`FarClass`]).
    /// Runs only after the probe machinery settles (`post_job` and
    /// `run_inline_job` entry — never inside `build_spf`, which overlaps
    /// the donation wait), so the mid-size capture's `JobPrefix` flip has
    /// already happened and cannot be overwritten. Two gates must both
    /// reject the far class: the head screen (`ldm_head_parses`, the
    /// bulk planner's own — a wide alphabet with a repeat at the head
    /// keeps the stock strip outright), then the far-repeat veto over
    /// exactly the span the first post's strips index, clamped at the
    /// first post's grid line so a write overshoot cannot widen it — a
    /// head whose repeats live beyond the reach (shadowed near-periodic
    /// classes like tiled text) passes the first gate but must not pay
    /// for strips until the span shows the class dominating it. A wrapped
    /// buffer (frame head gone) or a short-of-span resolve keeps the
    /// stock strip.
    fn resolve_far_class(&mut self) {
        if !matches!(self.far_class, FarClass::Pending) {
            return;
        }
        self.far_class = FarClass::Alive;
        if self.job_ldm != LdmArming::Job
            || self.choice == ReachChoice::Shrink
            || !MatchGeneratorDriver::spf_strip_fill(self.level, self.shape)
        {
            return;
        }
        let reach =
            MatchGeneratorDriver::strip_for_choice(self.level, self.shape, self.choice) as usize;
        if self.overlap <= reach
            || self.buf_base != 0
            || self.buf.len() < reach_probe::PROBE_SPAN
            || ldm_head_parses(&self.buf[..reach_probe::PROBE_SPAN])
        {
            return;
        }
        let span = self.buf.len().min(self.job_end(0) as usize);
        if !far_repeat_dominant(&self.buf[..span], reach) {
            self.far_class = FarClass::Dead;
            self.job_ldm = LdmArming::FarDead;
        }
    }

    /// The strip each job borrows and indexes: the level's overlap, capped
    /// at the row's chain reach when the far-class screen rejected the
    /// far class (see [`FarClass`]). The cap never feeds the schedule —
    /// job sizes and epochs stay derived from `overlap` — only the jobs'
    /// own history span and the buffer retention floor.
    fn strip_span(&self) -> usize {
        if matches!(self.far_class, FarClass::Dead) {
            self.overlap.min(MatchGeneratorDriver::strip_for_choice(
                self.level,
                self.shape,
                self.choice,
            ) as usize)
        } else {
            self.overlap
        }
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
            // A realloc copies only the live bytes: the new tail starts
            // uninitialized.
            self.init_len = self.buf.len();
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
            self.post_job(self.job_start, end, false, None);
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
                if let Some(n) = self.shape.len
                    && end >= n
                    && self.pos <= n
                {
                    return false;
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
        // The finish tail's shared prefix fill builds before the probe
        // settles: the build (a full strip fill) runs on this thread while
        // the donation tasks finish on the workers — time the settle would
        // spend waiting anyway. A flush never engages it (see build_spf).
        let mut spf = if last_frame_block {
            self.build_spf(hi)
        } else {
            None
        };
        // A flush or finish ahead of the staging gate decides the probe
        // with the stock reach (see the module docs).
        self.settle_probe();
        if spf.is_some() && self.choice == ReachChoice::Shrink {
            // The shrunk re-grid's strips are not the prefixes the build
            // filled; drop the plan unused.
            spf = None;
        }
        // The far-class screen runs after the probe settles (a verdict can
        // re-grid and re-arm); a dead verdict re-bases every tail strip at
        // the row's reach — not the whole prefixes the build filled — so
        // the plan drops unused, exactly like the shrunk one above.
        self.resolve_far_class();
        if spf.is_some() && matches!(self.far_class, FarClass::Dead) {
            spf = None;
        }
        self.emit_header();
        let bounds = self.build_bounds(hi);
        if bounds.len() == 2 && self.pool_threads.is_empty() && spf.is_none() {
            self.run_inline_job(&bounds, hi, last_frame_block);
        } else {
            let last = bounds.len() - 2;
            for (i, w) in bounds.windows(2).enumerate() {
                let spf = spf
                    .as_ref()
                    .filter(|p| p.covers(w[0]))
                    .map(|p| p.snapshot.clone());
                self.post_job(w[0], w[1], last_frame_block && i == last, spf);
            }
            self.job_start = hi;
            self.drain_all();
        }
    }

    /// Build the finish tail's shared prefix fill (see the module docs):
    /// an unpledged whole-window stream (the row-9 keep class) posts
    /// nothing during write — the growing grid's first epoch spans
    /// `burst_jobs` whole windows — so at finish every tail job would
    /// prefill its own whole-prefix strip [0, start): nested prefixes,
    /// redundant fills jobdecomp put at ~84% of the tail's summed job
    /// time. Instead one state fills the median tail boundary's prefix
    /// once, and the tail jobs share it (the median job continues on the
    /// pre-built state; the jobs above adopt a snapshot and fill only
    /// their remainder; the jobs below keep the stock path — their
    /// strips are the small half). Every fill loop is position-ordered
    /// over position-indexed or newest-wins state, so the incremental
    /// fill is bit-identical to the stock per-job fills and the frame
    /// bytes cannot change.
    ///
    /// The build fills in bounded segments, polling the probe donation
    /// between them: a shrink verdict retires the build immediately (its
    /// strips are not these prefixes), and the wasted segments ran inside
    /// the donation wait the caller would spend either way.
    fn build_spf(&mut self, hi: u64) -> Option<SpfPlan> {
        // Eligibility: the growing grid with nothing posted yet (every
        // tail strip is then the prefix [0, start)), buffers still at
        // stream offset zero, a chain-row strip fill (the snapshot's
        // build/adopt machinery is the chain grid + LDM split pass; the
        // opt/btlazy rows fill different tables), and a prefix strip
        // large enough to pay for the machinery. The strategy gate plus
        // the threshold admit exactly the whole-window chain rows: every
        // other row's stream strip is its search domain (<= 8 MiB). A
        // far-dead frame skips the build (its strips cap at the row's
        // reach — no nesting, nothing to share); the screen may still be
        // pending here (the probe settles after this), and `encode_jobs`
        // drops the plan if it lands dead.
        if !matches!(self.grid, JobGrid::Growing)
            || self.job_start != 0
            || self.buf_base != 0
            || self.choice == ReachChoice::Shrink
            || matches!(self.far_class, FarClass::Dead)
            || !MatchGeneratorDriver::spf_strip_fill(self.level, self.shape)
        {
            return None;
        }
        let bounds = self.build_bounds(hi);
        let n_jobs = bounds.len() - 1;
        if n_jobs < 2 {
            return None;
        }
        // Tail jobs (index >= 1) whose strip is the whole prefix; job zero
        // has no strip of its own (the donation's continuation, or empty).
        let prefix_jobs: Vec<usize> = (1..n_jobs)
            .filter(|&i| bounds[i] <= self.overlap as u64)
            .collect();
        let s_max = bounds[*prefix_jobs.last()?];
        if s_max < SPF_MIN_PREFIX {
            return None;
        }
        // Fill to the median prefix boundary (the balance point: the jobs
        // above adopt and fill their remainders in parallel, the jobs
        // below keep stock fills of the smaller half).
        let med = bounds[prefix_jobs[prefix_jobs.len() / 2]];
        let mut state = take_pooled_state();
        reset_slice_state(
            &mut state,
            self.level,
            self.shape,
            ReachChoice::Keep,
            LdmArming::Job,
        );
        // The clears of a stock prefill, then the fill itself segmented at
        // LDM batch-freeze points (the exact boundaries — see
        // `LdmState::fill_to_freeze`), each segment past `upto` bounded by
        // `med` plus slack so the loop cannot run the whole strip.
        state.matcher.prefill_window(&[], 0);
        let cap = med + SPF_SEG;
        let mut upto = 0u64;
        loop {
            if self
                .probe_wait
                .as_ref()
                .is_some_and(|(k, s)| k.is_done() && s.is_done())
            {
                self.resolve_probe();
            }
            if self.choice == ReachChoice::Shrink {
                return_pooled_state(state);
                return None;
            }
            let soft = (upto + SPF_SEG).min(med);
            let Some(freeze) =
                state
                    .matcher
                    .strip_fill_segment(&self.buf[..cap as usize], 0, upto, soft)
            else {
                // No freeze before the cap: not a shape the snapshot can
                // be cut at; the tail jobs keep their stock fills.
                return_pooled_state(state);
                return None;
            };
            upto = freeze;
            if upto >= med {
                break;
            }
        }
        #[cfg(feature = "job_trace")]
        let trace_spf = std::time::Instant::now();
        let snapshot = Arc::new(state.matcher.snapshot_strip_fill(upto));
        #[cfg(feature = "job_trace")]
        {
            let t = trace_spf;
            crate::encoding::job_trace::add_prefill(t);
        }
        return_pooled_state(state);
        Some(SpfPlan {
            upto,
            prefix_end: self.overlap as u64,
            snapshot,
        })
    }

    /// One short tail job (small inputs, a flush, or a finish without a
    /// burst behind it): inline on the calling thread, no pool involved.
    fn run_inline_job(&mut self, bounds: &[u64], hi: u64, last_frame_block: bool) {
        // The shared job view starts at the next job's strip (the previous
        // job's tail), which is exactly what the buffer retained.
        // A donation implies the pool was leased at its post, so this
        // pool-less path never carries one (job zero would parse undonated
        // — byte-identical, but the donation's work would be wasted).
        debug_assert!(self.donation.is_none() || !self.pool_threads.is_empty());
        self.resolve_far_class();
        let strip = self.strip_span();
        let strip_lo = self.job_start.saturating_sub(strip as u64);
        debug_assert!(self.buf_base <= strip_lo);
        let first = (self.job_start - strip_lo) as usize;
        self.hash_to(hi);
        let last_len = (bounds[1] - bounds[0]) as usize;
        let mut state = take_pooled_state();
        let bytes = run_job_with(
            &mut state,
            &self.buf[..(hi - self.buf_base) as usize],
            first..first + last_len,
            strip,
            last_frame_block,
            self.level,
            self.shape,
            self.choice,
            self.job_ldm,
            None,
        );
        return_pooled_state(state);
        self.output.extend_from_slice(&bytes);
        self.job_start = hi;
    }

    /// Post one job [start, end) to the pool. The source view is the
    /// buffer's [strip_lo, end) — the posting thread guarantees those bytes
    /// stable until the job completes (wrap and growth both require
    /// quiescence). `spf` is the job's share of the shared prefix fill,
    /// when the finish tail engaged one.
    fn post_job(
        &mut self,
        start: u64,
        end: u64,
        last_frame_block: bool,
        spf: Option<Arc<StripSnapshot>>,
    ) {
        debug_assert!(self.pos >= end);
        // The far-class screen lands with the first post (the probe
        // machinery has settled by then), so every job of the frame —
        // including this one — sees the same strip and arming.
        self.resolve_far_class();
        // The post cadence is also the assembly and panic-surfacing cadence:
        // per-write calls would burn millions of polls on the pump path.
        self.surface_poison();
        self.assemble_ready();
        self.emit_header();
        let strip_lo = start.saturating_sub(self.strip_span() as u64);
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
                overlap: self.strip_span(),
                ldm: self.job_ldm,
                last_frame_block,
                spf: Mutex::new(spf),
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
                // Aborted jobs (a poisoned worker) assemble as empty and
                // drain like any completion — surfacing only on the
                // quiescent-with-pending branch below would swallow the
                // panic and ship a frame missing the poisoned jobs' bytes.
                self.surface_poison();
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
                .wait_timeout(inner, Duration::from_millis(100))
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
                .wait_timeout(inner, Duration::from_millis(100))
                .unwrap();
            inner = guard;
            if job.is_done() || self.shared.completed.load(Ordering::Acquire) != before {
                return;
            }
        }
    }

    /// Lowest absolute offset the buffer must still serve: the next job to
    /// post borrows [job_start - strip, job_start) as its strip, and the
    /// bytes after it feed that job.
    fn movable_lo(&self) -> u64 {
        self.job_start.saturating_sub(self.strip_span() as u64)
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
        if self.buf.len() + want > self.buf.capacity() || self.buf.capacity() < BUF_CAP_MAX {
            // Doubling below the cap at every recycle point: an epoch-sized
            // buffer wraps (and waits) once per epoch because the growing
            // grid's posting gate holds a whole epoch live — growing past
            // that lets a stream fit without recycling at all. Past the cap
            // the wrap alone recycles space, so growth tracks the live
            // need.
            let target =
                (self.buf.len() + want + 64 * 1024).max((self.buf.capacity() * 2).min(BUF_CAP_MAX));
            self.buf.reserve_exact(target - self.buf.len());
            advise_hugepages(&self.buf);
            // A realloc copies only the live bytes: the new tail starts
            // uninitialized.
            self.init_len = self.buf.len();
        }
    }

    /// Absorb [hashed_end, end) into the frame checksum.
    fn hash_to(&mut self, end: u64) {
        debug_assert!(self.hashed_end <= end && end <= self.pos);
        let range = (self.hashed_end - self.buf_base) as usize..(end - self.buf_base) as usize;
        self.hasher.hash_tail(&self.buf[range]);
        self.hashed_end = end;
    }

    /// Lease the pool workers at the first posted job: parked threads from
    /// the cross-encoder pool first (a handoff costs ~10 us against the
    /// ~130 us a fresh eight-thread spawn+join pays), fresh spawns making
    /// up the rest.
    fn ensure_workers(&mut self) {
        if self.pool_threads.is_empty() {
            for _ in 0..self.workers {
                let slot = if let Some(slot) = take_parked_slot() {
                    assign_lease(&slot, self.shared.clone());
                    slot
                } else {
                    let slot = Arc::new(WorkerSlot {
                        state: Mutex::new(WorkerSlotState::Serving(self.shared.clone())),
                        wake: Condvar::new(),
                    });
                    if let Err(e) = std::thread::Builder::new().spawn({
                        let slot = slot.clone();
                        move || pool_thread(slot)
                    }) {
                        // No thread ever serves this lease: retire the
                        // slot so Drop's leave-wait passes, then surface
                        // the failure like the raw spawn's unwrap did.
                        leave_lease(&slot, true);
                        panic!("worker spawn failed: {e}");
                    }
                    slot
                };
                self.pool_threads.push(slot);
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

impl Drop for MtEncoderCore {
    fn drop(&mut self) {
        if !self.pool_threads.is_empty() {
            // The leases run their queues out (workers claim until the
            // queue drains) and leave at the shutdown flag; the per-slot
            // wait is the join equivalent — the encoder's buffer must not
            // move while any lease can still touch it.
            self.shared.inner.lock().unwrap().shutdown = true;
            self.shared.wake.notify_all();
            for slot in self.pool_threads.drain(..) {
                wait_leave(&slot, &self.shared);
            }
        }
        // The accumulate buffer outlives the encoder in the thread-local
        // pool (see take_pooled_buf); every worker has exited by now, so
        // nothing reads it anymore. Its length carries the ever-initialized
        // extent to the buffer's next encoder.
        let mut buf = core::mem::take(&mut self.buf);
        // SAFETY: [0, init_len) is initialized (the pump's zero-fill and
        // reads); the extent never exceeds the capacity.
        unsafe { buf.set_len(self.init_len.min(buf.capacity())) };
        return_pooled_buf(buf);
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

    /// A stream ending exactly on an epoch boundary has its last jobs
    /// already posted (and possibly still in flight) at finish: the tail
    /// path must not skip their assembly, and a flush at the same point
    /// must still surface them (visibility promise).
    #[test]
    fn epoch_end_finish_assembles_pending() {
        let mib = 1024 * 1024usize;
        // Fastest with 4 workers: the first epoch is exactly 4 MiB.
        let data = textish(4 * mib);
        let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        core.write(&data);
        core.finish();
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder.decode_all(&core.output, &mut out).unwrap();
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));

        let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        core.write(&data);
        core.flush_block();
        assert!(core.has_output());
        core.finish();
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder.decode_all(&core.output, &mut out).unwrap();
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));
    }

    /// Scripted writer: each scripted call accepts n bytes (or errors); past
    /// the script everything is accepted.
    struct ScriptedWriter {
        script: VecDeque<std::io::Result<usize>>,
        received: Vec<u8>,
    }

    impl std::io::Write for ScriptedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self.script.pop_front() {
                Some(Ok(n)) => {
                    let n = n.min(buf.len());
                    self.received.extend_from_slice(&buf[..n]);
                    Ok(n)
                },
                Some(Err(e)) => Err(e),
                None => {
                    self.received.extend_from_slice(buf);
                    Ok(buf.len())
                },
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Regression: a partial underlying write followed by an error must keep
    /// the undelivered tail in the output buffer so the retry resumes there —
    /// clearing on error dropped a frame stretch the writer never received.
    #[test]
    fn write_output_to_preserves_tail_on_error() {
        let data = textish(4 * 1024 * 1024);
        let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        core.write(&data);
        core.flush_block();
        assert!(core.has_output());
        let mut flaky = ScriptedWriter {
            script: [
                Ok(1),
                Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            ]
            .into(),
            received: Vec::new(),
        };
        assert!(core.write_output_to(&mut flaky).is_err());
        assert_eq!(flaky.received.len(), 1);
        assert!(core.has_output());
        // The retry resumes into the same writer; finish closes the frame.
        core.write_output_to(&mut flaky).unwrap();
        core.finish();
        core.write_output_to(&mut flaky).unwrap();
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder.decode_all(&flaky.received, &mut out).unwrap();
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));
    }

    /// Cross-encoder lease reuse through the global thread pool:
    /// sequential encoders (whose workers park between encoders and are
    /// re-leased) stay byte-identical, and many encoders leasing
    /// concurrently all produce the reference bytes.
    #[test]
    fn pool_lease_reuse_deterministic() {
        let data = textish(512 * 1024);
        let mut reference = None;
        for _ in 0..4 {
            let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fast).workers(4));
            core.write(&data);
            core.finish();
            match &reference {
                Some(bytes) => assert_eq!(&core.output, bytes, "sequential reuse"),
                None => reference = Some(core.output.clone()),
            }
        }
        let reference = reference.unwrap();
        let mut handles = Vec::new();
        for t in 0..16usize {
            let data = data.clone();
            let reference = reference.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..4 {
                    let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fast).workers(4));
                    core.write(&data);
                    core.finish();
                    assert_eq!(core.output, reference, "concurrent lease {t}");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Dropping mid-stream retires the leases with jobs possibly still
    /// queued or in flight: no hang, and the pool still serves a full
    /// encode identically afterwards.
    #[test]
    fn pool_drop_midstream() {
        let data = textish(9 * 1024 * 1024);
        for _ in 0..4 {
            let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
            core.write(&data);
            // Drop here: the first epoch's jobs are posted, later bytes
            // are not.
        }
        let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        core.write(&data);
        core.finish();
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder.decode_all(&core.output, &mut out).unwrap();
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));
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
