//! Multithreaded slice compression: one frame assembled from independent
//! overlap jobs.
//!
//! The input is split into jobs; each job compresses its byte range with the
//! pooled slice machinery while borrowing an `overlap`-sized strip of the
//! preceding job as match history (`adopt_window` plus `prefill_window`, so
//! the strip's positions actually sit in the search tables), keeping the
//! assembled output a single regular zstd frame. Two invariants make the
//! jobs independent of each other (mirroring libzstd's zstdmt):
//! - every job starts from reset entropy tables, so its first block is fully self-describing (no
//!   Repeat modes across a job boundary);
//! - every job except the first gates repcode references until three literal-offset sequences have
//!   rewritten the repeated-offset history (see [`MatchGeneratorDriver::gate_repcodes`]): the
//!   decoder's history at a job boundary is unknown, but each literal offset shifts it down one
//!   slot, so after three of them the matcher and decoder agree again.
//!
//! The frame checksum is hashed over the whole input on the calling thread
//! while the jobs run, and the ordered assembly appends each job's blocks as
//! soon as they land, overlapping the final copy with the late jobs.

use alloc::vec::Vec;
use core::{
    ops::Range,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::sync::{Arc, Condvar, Mutex};

use super::{
    Matcher,
    checksum::SliceChecksum,
    compress_fastest,
    frame_compressor::{
        CompressState, compress_job_blocks, compress_job_blocks_inner, new_slice_state,
        reset_slice_state, return_slice_state, take_slice_state,
    },
    frame_header::FrameHeader,
    match_generator::{
        LdmArming, MatchGeneratorDriver, SPF_MIN_PREFIX, SPF_SEG, StripSnapshot, ldm_head_parses,
    },
    reach_probe,
};
use crate::{Level, common::MAX_BLOCK_SIZE};

/// Below this size the thread spawn and the ordered assembly cost more than
/// the parallelism saves.
const MIN_MT_INPUT: usize = 2 * 1024 * 1024;
/// Job size floor: smaller jobs multiply the overlap duplication without
/// improving load balance.
pub(crate) const MIN_JOB_SIZE: usize = 1024 * 1024;
/// Job size ceiling: a huge known input would otherwise scale the job size
/// and with it the streaming burst buffer (which holds a whole burst) into
/// memory failure. libzstd's zstdmt caps its job size the same way.
pub(crate) const MAX_JOB_SIZE: usize = 1024 * 1024 * 1024;

/// Job size for an input of `len` bytes at `workers` threads: twice as many
/// jobs as workers keeps the tail balanced, the floor keeps the overlap
/// duplication negligible and scales with the level's overlap, and the
/// ceiling bounds the buffering. The bulk and streaming mt paths share this
/// so a pledged stream matches the bulk output byte for byte.
pub(crate) fn job_size_for(len: u64, workers: u32, overlap: usize) -> usize {
    debug_assert!(workers >= 2);
    len.div_ceil(workers as u64 * 2)
        .max(MIN_JOB_SIZE.max(overlap) as u64)
        .min(MAX_JOB_SIZE as u64) as usize
}

/// The encoder's exact MT job size for an input, with the overlap the
/// level implies — dev tooling (`emitframe`/`piecepipe`) cuts its analysis
/// pieces at these boundaries, so it must not re-derive the formula.
/// Hidden: not API-stable.
#[doc(hidden)]
pub fn mt_job_size_for(
    len: u64,
    workers: u32,
    level: Level,
    shape: crate::InputShape,
    head: &[u8],
) -> usize {
    // The frame's reach probe result (see reach_probe) decides the strip
    // with it.
    let choice = reach_probe::probe_reach_choice(head, level, shape);
    let overlap = MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize;
    job_size_for(len, workers.max(2), overlap)
}

/// Compress `src` into one frame using up to `workers` threads.
///
/// Falls back to the single-threaded slice path (byte-identical to
/// [`super::compress_slice_to_vec` modulo the checksum flag`) when the input
/// is too small, only one worker is requested, the level stores raw blocks,
/// or the process has a single usable core.
pub fn compress_slice_mt(
    src: &[u8],
    level: Level,
    checksum: bool,
    workers: u32,
    window_log: Option<u32>,
) -> Vec<u8> {
    let checksum = checksum && cfg!(feature = "hash");
    if workers < 2
        || src.len() < MIN_MT_INPUT
        || level == Level::Uncompressed
        || std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
    {
        return super::compress_slice_shaped(src, level, checksum, crate::InputShape {
            len: None,
            window_log,
        });
    }

    // Twice as many jobs as workers keeps the tail balanced; the floor keeps
    // the overlap duplication negligible, and scales with the level's
    // overlap so deep-search levels don't pay it per job.
    let shape = crate::InputShape {
        len: Some(src.len() as u64),
        window_log,
    };
    // The dense matchers' domain as strip: the strip is fully indexed (see
    // `prefill_window`), so matches reach across job borders as far as the
    // dense search does. A shorter strip caps the ratio at repeats that
    // fit inside it — the text corpus's ~800K period against a 1 MiB window
    // collapsed the multithreaded ratio by an order of magnitude. LDM rows
    // decouple the two (a wide frame window for far reach, a narrow strip):
    // a full-window strip would scale the job floor with the LDM reach and
    // starve parallelism.
    let window = MatchGeneratorDriver::window_for_level(level, shape);
    // The frame's head decides its chain reach (see reach_probe), and with
    // it the strip: a shrunk search domain shrinks the strip to match.
    // Eligible frames donate the probe's keep side into job zero (see
    // `donate_job_zero_prefix`): the first span blocks run as job zero's
    // own blocks on the calling thread through the job machinery, so only
    // a Shrink verdict pays a re-parse. The keep grid must leave job zero
    // work beyond the span for the continuation to take over.
    let mut donation = None;
    // Whether the donated state arms with the mid-size capture's arming
    // (see `donation_arming`): the pairing invariant below checks the
    // capture the frame finally runs against it.
    let mut donation_prefix = false;
    // The capture plan a Keep verdict would run, computed before the
    // donation so the donated span can arm for it; reused as the frame's
    // plan when the verdict keeps.
    let mut keep_plan = None;
    let choice;
    if reach_probe::eligible(level, shape)
        && job_size_for(
            src.len() as u64,
            workers.max(2),
            MatchGeneratorDriver::strip_for_choice(level, shape, reach_probe::ReachChoice::Keep)
                as usize,
        ) >= reach_probe::PROBE_SPAN
    {
        let keep_strip =
            MatchGeneratorDriver::strip_for_choice(level, shape, reach_probe::ReachChoice::Keep)
                as usize;
        let keep_job_size = job_size_for(src.len() as u64, workers.max(2), keep_strip);
        keep_plan = plan_prefix_ldm(
            src,
            level,
            shape,
            reach_probe::ReachChoice::Keep,
            keep_job_size,
            src.len().div_ceil(keep_job_size),
        );
        donation_prefix = keep_plan.is_some();
        let (donated_choice, state, prefix) =
            donate_job_zero_prefix(src, level, shape, donation_arming(donation_prefix));
        choice = donated_choice;
        if choice == reach_probe::ReachChoice::Keep {
            donation = Some((state, prefix));
        } else {
            // The shrink verdict discards the donation (and the plan: the
            // shrunk strips are not the prefixes it filled); job zero
            // parses from its own pooled state like an undonated frame.
            return_slice_state(state);
            keep_plan = None;
        }
    } else {
        choice = reach_probe::probe_reach_choice(src, level, shape);
    }
    let donation = Mutex::new(donation);
    let overlap = MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize;
    let job_size = job_size_for(src.len() as u64, workers, overlap);
    let n_jobs = src.len().div_ceil(job_size);
    let threads = (workers as usize).min(n_jobs);

    // The mid-size prefix-LDM class (dll32-shaped frames): jobs borrow the
    // whole clamped window as their strip, so every job's history is a
    // frame prefix and the far class survives the job split exactly as on
    // the frame-continuous path. The grid keeps the reach-based job size —
    // the window would floor jobs at 32 MiB and starve parallelism — and
    // the shared prefix fill bounds the per-job prefix fill's redundancy:
    // one build to the median boundary, tail jobs above it adopt.
    let spf: Option<PrefixPlan> = if choice == reach_probe::ReachChoice::Keep {
        // A kept frame's capture is exactly the plan the donation gate
        // computed (the same pure inputs at Keep); a non-donated frame
        // plans from its final schedule.
        keep_plan
            .take()
            .or_else(|| plan_prefix_ldm(src, level, shape, choice, job_size, n_jobs))
    } else {
        None
    };
    // Pairing invariant: a capture the frame runs is one the donated job
    // zero armed for. The reverse direction is trivially the gate's own
    // computation; a capture without any donation cannot happen (the
    // capture's window band keeps the frame probe-eligible, so the gate
    // passed and the plan was computed there).
    debug_assert!(spf.is_none() || donation_prefix);
    let job_overlap = if spf.is_some() {
        window as usize
    } else {
        overlap
    };
    let job_ldm = if spf.is_some() {
        LdmArming::JobPrefix
    } else {
        LdmArming::Job
    };

    #[cfg(feature = "hash")]
    let mut frame_hash = checksum.then(|| crate::xxh64::Xxh64::new(0));

    let block_overhead = 3 * (src.len() / MAX_BLOCK_SIZE as usize + 1);
    let mut output = Vec::with_capacity(src.len() + block_overhead + 32);
    FrameHeader {
        // The whole input is known here, so pledge it: decoders preallocate
        // exactly and the parallel decode path can partition up front.
        frame_content_size: Some(src.len() as u64),
        single_segment: false,
        content_checksum: checksum,
        dictionary_id: None,
        window_size: Some(window),
    }
    .serialize(&mut output);

    let slots: Vec<Mutex<Option<Vec<u8>>>> = (0..n_jobs).map(|_| Mutex::new(None)).collect();
    let ready = Condvar::new();
    let next_job = AtomicUsize::new(0);
    let poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if poison.lock().unwrap().is_some() {
                        break;
                    }
                    let id = next_job.fetch_add(1, Ordering::Relaxed);
                    if id >= n_jobs {
                        break;
                    }
                    let start = id * job_size;
                    let end = (start + job_size).min(src.len());
                    // Adopters of the shared prefix fill wait for the
                    // calling thread's build (it runs while the workers
                    // encode the below-boundary jobs); a start the build's
                    // freeze overshot keeps the stock whole-prefix fill.
                    let snapshot = match spf.as_ref() {
                        Some(plan) if start as u64 > plan.med => {
                            if let Some((upto, snap)) = wait_prefix_build(plan, &ready, &poison) {
                                (start as u64 > upto).then_some(snap)
                            } else {
                                // Poisoned while waiting: release the
                                // slot so the ordered assembly drains
                                // before the panic resumes.
                                *slots[id].lock().unwrap() = Some(Vec::new());
                                ready.notify_all();
                                break;
                            }
                        },
                        _ => None,
                    };
                    // The first job starts where the decoder's repeated-offset
                    // history is still the format default [1, 4, 8]; the gate
                    // is job.start > 0 (applied inside the state preparation).
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if id == 0
                            && let Some((mut dstate, prefix)) = donation.lock().unwrap().take()
                        {
                            // The donated state already carries job zero's
                            // reset, strip prefill and parsed span — the
                            // continuation emits from where it stopped.
                            let out = compress_job_blocks_inner(
                                &mut dstate,
                                src,
                                start..end,
                                job_overlap,
                                end == src.len(),
                                reach_probe::PROBE_SPAN,
                                prefix,
                                None,
                            );
                            return_slice_state(dstate);
                            return out;
                        }
                        run_job(
                            src,
                            start..end,
                            job_overlap,
                            end == src.len(),
                            level,
                            shape,
                            choice,
                            job_ldm,
                            snapshot.as_deref(),
                        )
                    }));
                    match attempt {
                        Ok(bytes) => {
                            *slots[id].lock().unwrap() = Some(bytes);
                        },
                        Err(payload) => {
                            *poison.lock().unwrap() = Some(payload);
                            // Release the slot so the ordered assembly below can
                            // run to completion before the panic is resumed.
                            *slots[id].lock().unwrap() = Some(Vec::new());
                        },
                    }
                    ready.notify_all();
                }
            });
        }

        // Build the shared prefix fill on the calling thread while the
        // workers encode: one sequential fill to the median boundary in
        // place of the tail jobs' redundant whole-prefix fills.
        if let Some(plan) = &spf {
            #[cfg(feature = "job_trace")]
            let trace_spf = std::time::Instant::now();
            // Caller-side span, its own counter (see Snapshot::spf_build_ns):
            // the build is posting-thread work that the per-job prefill
            // spans cannot see.
            let build = build_prefix_snapshot(src, level, shape, plan.med)
                .map(|(upto, snapshot)| PrefixBuild { upto, snapshot });
            *plan.share.lock().unwrap() = build;
            ready.notify_all();
            #[cfg(feature = "job_trace")]
            super::job_trace::add_spf_build(trace_spf);
        }

        // The frame checksum is independent of the job split; hash it while
        // the workers are still busy.
        #[cfg(feature = "hash")]
        if let Some(hash) = frame_hash.as_mut() {
            hash.write(src);
        }

        // Ordered assembly on the calling thread: each job's blocks append as
        // soon as they land.
        for slot in &slots {
            let mut guard = slot.lock().unwrap();
            while guard.is_none() {
                // A poisoned pool retires its workers with jobs unclaimed —
                // those slots would never fill. Empty them so the assembly
                // completes and the panic resumes below instead of
                // deadlocking this wait. (Lock order slot→poison is
                // one-sided: workers take them sequentially, never nested.)
                if poison.lock().unwrap().is_some() {
                    *guard = Some(Vec::new());
                    break;
                }
                guard = ready.wait(guard).unwrap();
            }
            let bytes = guard.take().unwrap();
            output.extend_from_slice(&bytes);
        }
    });
    if let Some(payload) = poison.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
    #[cfg(feature = "hash")]
    if let Some(hash) = frame_hash.as_mut() {
        output.extend_from_slice(&(hash.finish() as u32).to_le_bytes());
    }
    output
}

/// Deep-offset ramp depth for gated jobs (decode-parallelism experiment):
/// `ZSTDX_MT_RAMP_BYTES` env var, parsed once. Zero (unset) keeps the job
/// parse unconstrained; see `match_generator::RampGate` for the semantics.
static RAMP_TEST_OVERRIDE: AtomicU64 = AtomicU64::new(u64::MAX);

/// Test-only ramp override (env parsing is process-global; tests must not
/// race other tests' encodes through `set_var`). Hidden: not API-stable.
#[doc(hidden)]
pub fn set_mt_ramp_depth_for_tests(depth: u64) {
    RAMP_TEST_OVERRIDE.store(depth, Ordering::Relaxed);
}

fn ramp_depth_from_env() -> u64 {
    static DEPTH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let over = RAMP_TEST_OVERRIDE.load(Ordering::Relaxed);
    if over != u64::MAX {
        return over;
    }
    *DEPTH.get_or_init(|| {
        std::env::var("ZSTDX_MT_RAMP_BYTES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// The donated span's LDM arming for a capture engagement verdict (see
/// `compress_slice_mt`'s donation gate): the capture's own arming when it
/// engages — a kept frame's job zero is a `JobPrefix` job, so the donated
/// span must parse with the same bar — the stock job arming otherwise.
fn donation_arming(capture_engages: bool) -> LdmArming {
    if capture_engages {
        LdmArming::JobPrefix
    } else {
        LdmArming::Job
    }
}

/// Run the reach probe's keep side as job zero's own first span blocks
/// through `state`: the state is job-zero-shaped exactly (reset at the job
/// arming, empty-strip prefill with its table clear and head arming), the
/// emit mirrors `compress_job_blocks`, and the matcher accumulates the
/// probe cost while the blocks become the frame's own output. `ldm` is the
/// arming the kept frame's job zero would run with (see `donation_arming`
/// and the stream core's `post_donation`) — the probe measures the kept
/// parse's true cost only when the span parses as that job would. `head`
/// is the frame's first [`reach_probe::PROBE_SPAN`] bytes. Returns the
/// measured keep cost and the encoded span; the caller takes the verdict
/// (the shrink side measures separately — bulk serially on the calling
/// thread, the stream core on a second pool worker) and disposes of the
/// state (a Shrink verdict's returns to its pool, a Keep's becomes job
/// zero's continuation state).
pub(crate) fn donate_keep_span(
    state: &mut CompressState<MatchGeneratorDriver>,
    head: &[u8],
    level: Level,
    shape: crate::InputShape,
    ldm: LdmArming,
) -> (f64, Vec<u8>) {
    debug_assert_eq!(head.len(), reach_probe::PROBE_SPAN);
    reset_slice_state(state, level, shape, reach_probe::ReachChoice::Keep, ldm);
    state.matcher.prefill_job_strip(&head[..0], 0);
    let block_size = state.matcher.block_size();
    let max_window = state.matcher.window_size() as usize;
    let span = reach_probe::PROBE_SPAN;
    let mut output = Vec::with_capacity(span + 3 * (span / block_size + 1) + 8);
    let mut hasher = SliceChecksum::new(0, false);
    state.matcher.begin_probe_stats();
    let mut cursor = 0usize;
    while cursor < span {
        // Pre-split decisions with the block cap and span-end clamp,
        // matching the continuation's grid (see `pre_split`).
        let window_end = (cursor + block_size).min(span);
        let decided = state
            .split
            .block_size(&head[cursor..window_end], state.matcher.pre_split_level());
        let block_end = cursor + decided;
        let hist = cursor.saturating_sub(max_window);
        state
            .matcher
            .adopt_window(&head[hist..block_end], hist as u64);
        state.matcher.set_block(cursor as u64, block_end as u64);
        let before = output.len();
        compress_fastest(state, false, &mut output, &mut hasher);
        state.split.note_block(decided, output.len() - before);
        cursor = block_end;
    }
    let keep = state
        .matcher
        .take_probe_cost()
        .expect("donation began the stats");
    (keep, output)
}

/// [`donate_keep_span`] wrapped for the bulk path: returns the verdict plus the
/// state and encoded prefix for job zero's continuation (a Shrink verdict
/// discards both — the caller returns the state to the pool and job zero
/// parses from its own pooled state).
fn donate_job_zero_prefix(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
    ldm: LdmArming,
) -> (
    reach_probe::ReachChoice,
    alloc::boxed::Box<CompressState<MatchGeneratorDriver>>,
    Vec<u8>,
) {
    let mut state = take_slice_state(level, shape, reach_probe::ReachChoice::Keep, ldm);
    let (keep, output) = donate_keep_span(
        &mut state,
        &src[..reach_probe::PROBE_SPAN],
        level,
        shape,
        ldm,
    );
    let shrink = reach_probe::parse_cost(
        &src[..reach_probe::PROBE_SPAN],
        level,
        shape,
        reach_probe::ReachChoice::Shrink,
        reach_probe::ProbeFeedback::Approx,
        // Same landslide abort as the bulk-ST path.
        Some(keep * reach_probe::KEEP_LANDSLIDE),
    );
    let choice = reach_probe::decide_donated(keep, shrink, src, level, shape);
    (choice, state, output)
}

/// Pooled donation kit for the streaming core: its pool workers are
/// ephemeral (spawned per encoder, joined at drop), so a donation running
/// there inherits no warm tables — a fresh state and probe driver measured
/// 4-5x the warm parse cost (first-touch faults on ~50 MiB of tables, per
/// stream). The kit outlives encoders and hands the donation the same
/// steady-thread warmth the bulk paths get from their thread-local pools;
/// depth one (a frame has one donation, and the retained tables are the
/// price).
#[cfg(feature = "std")]
type DonationKit = (
    Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>,
    Option<alloc::boxed::Box<MatchGeneratorDriver>>,
);

#[cfg(feature = "std")]
static DONATION_KIT: std::sync::OnceLock<Mutex<DonationKit>> = std::sync::OnceLock::new();

#[cfg(feature = "std")]
fn donation_kit() -> &'static Mutex<DonationKit> {
    DONATION_KIT.get_or_init(|| Mutex::new((None, None)))
}

/// Take the kit's state (a fresh one when the pool is cold).
#[cfg(feature = "std")]
pub(crate) fn take_donation_state() -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    donation_kit()
        .lock()
        .unwrap()
        .0
        .take()
        .unwrap_or_else(|| alloc::boxed::Box::new(new_slice_state()))
}

/// Return a donation state to the kit.
#[cfg(feature = "std")]
pub(crate) fn return_donation_state(state: alloc::boxed::Box<CompressState<MatchGeneratorDriver>>) {
    donation_kit().lock().unwrap().0 = Some(state);
}

/// Take the kit's probe driver (a fresh one when the pool is cold).
#[cfg(feature = "std")]
pub(crate) fn take_donation_probe() -> alloc::boxed::Box<MatchGeneratorDriver> {
    donation_kit()
        .lock()
        .unwrap()
        .1
        .take()
        .unwrap_or_else(|| alloc::boxed::Box::new(MatchGeneratorDriver::new_direct()))
}

/// Return a donation probe driver to the kit.
#[cfg(feature = "std")]
pub(crate) fn return_donation_probe(probe: alloc::boxed::Box<MatchGeneratorDriver>) {
    donation_kit().lock().unwrap().1 = Some(probe);
}

/// The shared prefix fill's published build: the boundary the snapshot
/// covers (an LDM batch-freeze point at or past the plan's median) and
/// the snapshot itself, shared by every adopting tail job.
struct PrefixBuild {
    upto: u64,
    snapshot: Arc<StripSnapshot>,
}

/// The bulk path's shared prefix fill plan (the mid-size prefix-LDM
/// class): jobs starting above `med` wait for the calling thread's build
/// and adopt its snapshot when their strip extends it, instead of filling
/// their whole frame prefix from scratch.
struct PrefixPlan {
    med: u64,
    share: Arc<Mutex<Option<PrefixBuild>>>,
}

/// Whether the bulk path runs a frame on prefix-strip LDM jobs (the
/// dll32-class capture, see `compress_slice_mt`): the row/window/verdict
/// class plus a head that both parses and carries a wide alphabet, and a
/// job grid whose prefix strips are large enough to pay for the shared
/// fill. Deterministic in the input, level and worker count alone.
fn plan_prefix_ldm(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    job_size: usize,
    n_jobs: usize,
) -> Option<PrefixPlan> {
    let window = MatchGeneratorDriver::prefix_ldm_window(level, shape, choice)?;
    let head = &src[..src.len().min(MAX_BLOCK_SIZE as usize)];
    if !ldm_head_parses(head) {
        return None;
    }
    // Tail jobs whose window strip is a whole frame prefix: the source
    // sits inside the clamped window in this class, but the bound stays
    // explicit — a mid-stream strip keeps the stock fill either way.
    let prefix_jobs: Vec<usize> = (1..n_jobs)
        .filter(|&i| (i * job_size) as u64 <= window)
        .collect();
    let s_max = (*prefix_jobs.last()? * job_size) as u64;
    if s_max < SPF_MIN_PREFIX {
        return None;
    }
    // The median prefix boundary balances the sequential build against the
    // adopters' parallel remainder fills (the same balance point the
    // streaming core's finish tail picks).
    let med = (prefix_jobs[prefix_jobs.len() / 2] * job_size) as u64;
    Some(PrefixPlan {
        med,
        share: Arc::new(Mutex::new(None)),
    })
}

/// Fill `[0, med)` once on the calling thread through a pooled state,
/// segmented at LDM batch-freeze points (the only exactly-resumable
/// boundaries — see `LdmState::fill_to_freeze`) and snapshotted at the
/// first freeze at or past `med`. `None` when no freeze lands before the
/// boundary: the tail jobs keep their stock whole-prefix fills.
fn build_prefix_snapshot(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
    med: u64,
) -> Option<(u64, Arc<StripSnapshot>)> {
    let mut state = take_slice_state(
        level,
        shape,
        reach_probe::ReachChoice::Keep,
        LdmArming::JobPrefix,
    );
    // The stock prefill's clears and LDM restart, then the fill itself in
    // segments whose slack past `med` stays bounded — the loop cannot run
    // the whole prefix hunting a freeze.
    state.matcher.prefill_window(&[], 0);
    let cap = ((med + SPF_SEG) as usize).min(src.len());
    let mut upto = 0u64;
    loop {
        let soft = (upto + SPF_SEG).min(med);
        let Some(freeze) = state.matcher.strip_fill_segment(&src[..cap], 0, upto, soft) else {
            return_slice_state(state);
            return None;
        };
        upto = freeze;
        if upto >= med {
            break;
        }
    }
    let snapshot = Arc::new(state.matcher.snapshot_strip_fill(upto));
    return_slice_state(state);
    Some((upto, snapshot))
}

/// Block until the calling thread publishes the shared prefix fill's
/// build (or a poisoned pool aborts the wait). Re-checks on every wake:
/// the build publishes through the shared `ready` condvar the job
/// completions already use. Lock order share→poison is one-sided, like
/// the assembly's slot→poison.
fn wait_prefix_build(
    plan: &PrefixPlan,
    ready: &Condvar,
    poison: &Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>>,
) -> Option<(u64, Arc<StripSnapshot>)> {
    let mut guard = plan.share.lock().unwrap();
    loop {
        if let Some(build) = guard.as_ref() {
            return Some((build.upto, Arc::clone(&build.snapshot)));
        }
        if poison.lock().unwrap().is_some() {
            return None;
        }
        guard = ready.wait(guard).unwrap();
    }
}

/// Reset a state for a job and apply the job-start gates: fresh entropy
/// tables, the repcode gate and ramp arm unless the job starts the frame
/// (the decoder's repeated-offset history is the format default only
/// there). The prefix of [`run_job_with`] shared with the streaming
/// core's pre-built prefix-fill state (see `encoder_mt::build_spf`),
/// whose state is prepared before it is handed to a worker.
pub(crate) fn prepare_job_state(
    state: &mut CompressState<MatchGeneratorDriver>,
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    ldm: LdmArming,
    job_start: u64,
) {
    reset_slice_state(state, level, shape, choice, ldm);
    if job_start > 0 {
        state.matcher.gate_repcodes();
        let depth = ramp_depth_from_env();
        if depth > 0 {
            state.matcher.arm_ramp(job_start, depth);
        }
    }
}

/// Compress one job through `state`, resetting it for the job: fresh
/// entropy tables, and the repcode gate unless the job starts the frame
/// (the decoder's repeated-offset history is the format default only there).
/// `shape` is the whole frame's declared shape (length known for bulk,
/// pledge or none for streaming; jobs share it so tables and the header
/// window agree). `ldm` is the job's LDM arming context; `spf` the shared
/// prefix fill's snapshot when the job adopts one instead of filling its
/// strip from scratch (see `StripSnapshot`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_job_with(
    state: &mut CompressState<MatchGeneratorDriver>,
    src: &[u8],
    job: Range<usize>,
    overlap: usize,
    is_last_job: bool,
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    ldm: LdmArming,
    spf: Option<&StripSnapshot>,
) -> Vec<u8> {
    #[cfg(feature = "job_trace")]
    let trace_reset = std::time::Instant::now();
    prepare_job_state(state, level, shape, choice, ldm, job.start as u64);
    #[cfg(feature = "job_trace")]
    super::job_trace::add_reset(trace_reset);
    match spf {
        Some(snap) => {
            let start = job.start;
            compress_job_blocks_inner(
                state,
                src,
                job,
                overlap,
                is_last_job,
                start,
                Vec::new(),
                Some(snap),
            )
        },
        None => compress_job_blocks(state, src, job, overlap, is_last_job),
    }
}

/// Compress one job on the calling (worker) thread through the per-thread
/// pooled state, so steady-state jobs reuse their hash table allocation.
/// Shared by the streaming burst driver.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_job(
    src: &[u8],
    job: Range<usize>,
    overlap: usize,
    is_last_job: bool,
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    ldm: LdmArming,
    spf: Option<&StripSnapshot>,
) -> Vec<u8> {
    let mut state = take_slice_state(level, shape, choice, ldm);
    let output = run_job_with(
        &mut state,
        src,
        job,
        overlap,
        is_last_job,
        level,
        shape,
        choice,
        ldm,
        spf,
    );
    return_slice_state(state);
    output
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use super::{
        compress_slice_mt, donation_arming, job_size_for, plan_prefix_ldm, reach_probe,
        take_slice_state,
    };
    use crate::{
        Level,
        common::MAX_BLOCK_SIZE,
        decoding::FrameDecoder,
        encoding::{
            match_generator::{LdmArming, MatchGeneratorDriver},
            reach_probe::ReachChoice,
        },
    };

    /// Wide-alphabet head built from a 96-pattern pool: 8-byte windows
    /// repeat (the capture head screen's parse evidence) and the alphabet
    /// is wide enough to clear it, unlike random or uniform bytes.
    fn pattern_head(len: usize) -> Vec<u8> {
        let mut patterns = Vec::new();
        let mut state = 0x243f_6a88_85a3_08d3u64;
        for _ in 0..96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            patterns.push(state.to_le_bytes());
        }
        let mut head = Vec::with_capacity(len);
        let mut pick = 0xdead_beef_cafeu64;
        while head.len() < len {
            pick = pick.wrapping_mul(6364136223846793005).wrapping_add(1);
            head.extend_from_slice(&patterns[(pick >> 33) as usize % 96]);
        }
        head
    }

    /// The capture plan (and with it the donation's arming) at the band
    /// edges: only the source-clamped window in [W25, W26) engages — a
    /// `Job`-armed donation would parse the span without LDM there, so the
    /// pairing with `donation_arming` is load-bearing exactly in this band.
    #[test]
    fn donation_arming_pairs_with_the_capture_plan() {
        let src = pattern_head(MAX_BLOCK_SIZE as usize);
        let mib: u64 = 1024 * 1024;
        let cases: &[(u64, u32, Option<u32>, bool)] = &[
            // The band: row 9's source-clamped window is W25.
            (24 * mib, 2, None, true),
            (17 * mib, 4, None, true),
            // A forced window inside the band engages on a large source.
            (128 * mib, 4, Some(25), true),
            // Above the band (window W26) the `Job` bar holds.
            (64 * mib, 4, None, false),
            // Below it there are no prefix strips worth sharing.
            (8 * mib, 4, None, false),
        ];
        for &(len, workers, window_log, engages) in cases {
            let shape = crate::InputShape {
                len: Some(len),
                window_log,
            };
            let strip =
                MatchGeneratorDriver::strip_for_choice(Level::Balanced, shape, ReachChoice::Keep)
                    as usize;
            let job_size = job_size_for(len, workers.max(2), strip);
            let n_jobs = len.div_ceil(job_size as u64) as usize;
            let plan = plan_prefix_ldm(
                &src,
                Level::Balanced,
                shape,
                ReachChoice::Keep,
                job_size,
                n_jobs,
            );
            assert_eq!(
                plan.is_some(),
                engages,
                "len {len} workers {workers} window_log {window_log:?}"
            );
            assert_eq!(
                donation_arming(plan.is_some()),
                if engages {
                    LdmArming::JobPrefix
                } else {
                    LdmArming::Job
                }
            );
        }
        // The head screen: same band, uniform bytes — no capture, and the
        // donation keeps the stock arming.
        let shape = crate::InputShape {
            len: Some(24 * mib),
            window_log: None,
        };
        let strip =
            MatchGeneratorDriver::strip_for_choice(Level::Balanced, shape, ReachChoice::Keep)
                as usize;
        let job_size = job_size_for(shape.len.unwrap(), 2, strip);
        let plan = plan_prefix_ldm(
            &vec![0u8; MAX_BLOCK_SIZE as usize],
            Level::Balanced,
            shape,
            ReachChoice::Keep,
            job_size,
            shape.len.unwrap().div_ceil(job_size as u64) as usize,
        );
        assert_eq!(plan.is_some(), false);
        assert_eq!(donation_arming(plan.is_some()), LdmArming::Job);
    }

    /// Far periodicity whose twins resolve only through LDM inside job
    /// zero's own span: a 4.5 MiB unit (past the row's W22 chain reach)
    /// tiled exactly, so every copy after the first matches 4.5 MiB back.
    /// The unit carries an 8-byte motif every 2 KiB so the incompressibility
    /// gate's repeat probe keeps every block matchable — copy one is then
    /// scanned and LDM-indexed, and copy two's far twins resolve through
    /// the table. The donated job zero must parse with the capture's
    /// arming: a `Job`-armed donation (the arming mismatch this pins)
    /// parses the span without LDM — the capture's clamped window sits
    /// below the `Job` bar — and loses the far twins it holds, diverging
    /// from the single-threaded path by their whole cost.
    #[test]
    fn donated_job_zero_resolves_intra_job_far_twin() {
        const MIB: usize = 1024 * 1024;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let unit_len = 9 * MIB / 2; // 4.5 MiB
        let mut unit: Vec<u8> = (0..unit_len).map(|_| (rand() & 0xff) as u8).collect();
        let word: [u8; 8] = core::array::from_fn(|_| (rand() & 0xff) as u8);
        let mut i = 0;
        while i + 64 <= unit_len {
            for k in 0..8 {
                unit[i + k * 8..i + k * 8 + 8].copy_from_slice(&word);
            }
            i += 2048;
        }
        let mut data = pattern_head(256 * 1024);
        while data.len() < 32 * MIB {
            let take = unit.len().min(32 * MIB - data.len());
            data.extend_from_slice(&unit[..take]);
        }
        // 32 MiB at two workers grids 8 MiB jobs, so job zero holds the
        // head copy plus ~3.25 MiB of copy two; the capture engages (the
        // head above).
        let st = compress_slice_mt(&data, Level::from_zstd(9), true, 1, None);
        let mt = compress_slice_mt(&data, Level::from_zstd(9), true, 2, None);
        for (name, frame) in [("st", &st), ("mt", &mt)] {
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder
                .decode_all(frame, &mut out)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((n, &out[..n]), (data.len(), &data[..]), "{name}");
            let mut libzstd = Vec::new();
            zstd::stream::copy_decode(frame.as_slice(), &mut libzstd).unwrap();
            assert_eq!(libzstd, data, "{name} libzstd");
        }
        assert!(
            mt.len() <= st.len() + st.len() / 50,
            "the intra-job far twin must survive the donated job zero: mt {} vs st {}",
            mt.len(),
            st.len()
        );
    }

    fn lcg(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..len).map(|_| (rand() & 0xff) as u8).collect()
    }

    /// Text-like data: repeating vocabulary with variation, so matches,
    /// repcodes and entropy coding all engage across job boundaries.
    fn textish(len: usize) -> Vec<u8> {
        let words: Vec<&[u8]> = vec![
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

    fn shapes() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("text", textish(5 * 1024 * 1024 + 123)),
            ("zeros", vec![0u8; 4 * 1024 * 1024]),
            ("random", lcg(2 * 1024 * 1024 + 7)),
            ("exact-jobs", textish(4 * 1024 * 1024)),
        ]
    }

    #[test]
    fn mt_frames_decode_identically() {
        for (name, data) in shapes() {
            for workers in [2u32, 4] {
                for checksum in [true, false] {
                    let compressed =
                        compress_slice_mt(&data, Level::Fastest, checksum, workers, None);
                    // our decoder, exact-size output slice
                    let mut out = vec![0u8; data.len()];
                    let mut decoder = FrameDecoder::new();
                    let n = decoder
                        .decode_all(&compressed, &mut out)
                        .unwrap_or_else(|e| panic!("{name}/{workers}/{checksum}: {e}"));
                    assert_eq!(
                        (n, &out[..n]),
                        (data.len(), &data[..]),
                        "{name}/{workers}/{checksum}"
                    );
                    // libzstd decodes the assembled frame too
                    let mut decoded = Vec::new();
                    zstd::stream::copy_decode(compressed.as_slice(), &mut decoded).unwrap();
                    assert_eq!(decoded, data, "{name}/{workers}/{checksum}");
                }
            }
        }
    }

    #[test]
    fn mt_output_is_deterministic() {
        let data = textish(3 * 1024 * 1024);
        let a = compress_slice_mt(&data, Level::Fastest, true, 3, None);
        let b = compress_slice_mt(&data, Level::Fastest, true, 3, None);
        assert_eq!(a, b);
    }

    /// The overlap strip must keep the multithreaded ratio close to the
    /// single-threaded one on data whose repeats fit inside a window.
    #[test]
    fn mt_ratio_stays_close_to_single_thread() {
        let data = textish(6 * 1024 * 1024);
        let st = compress_slice_mt(&data, Level::Fastest, true, 1, None);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4, None);
        assert!(
            mt.len() <= st.len() + st.len() / 50,
            "single {} vs multi {}",
            st.len(),
            mt.len()
        );
    }

    /// The chain levels must ride the job path too: the repcode gate plus a
    /// fresh-table job start interact with the deeper search.
    #[test]
    fn mt_chain_levels_roundtrip() {
        let data = textish(5 * 1024 * 1024);
        for level in [Level::Fast, Level::Balanced] {
            let compressed = compress_slice_mt(&data, level, true, 4, None);
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder.decode_all(&compressed, &mut out).unwrap();
            assert_eq!((n, &out[..n]), (data.len(), &data[..]));
            let st = crate::encoding::compress_slice_to_vec(&data, level);
            assert!(
                compressed.len() <= st.len() + st.len() / 50,
                "mt ratio must stay near single-thread: {} vs {}",
                compressed.len(),
                st.len()
            );
        }
    }

    /// A repeat period that fits the level's window must keep matching
    /// across job borders: the strip is indexed, so the only ratio loss left
    /// is the job-start entropy restarts. Data whose period exceeds the old
    /// un-indexed strip collapsed the ratio by an order of magnitude.
    #[test]
    fn mt_periodic_repeat_matches_across_jobs() {
        let unit = textish(300 * 1024);
        let data: Vec<u8> = (0..24).flat_map(|_| unit.iter().copied()).collect();
        for level in [Level::Fastest, Level::Fast, Level::Balanced] {
            let st = compress_slice_mt(&data, level, true, 1, None);
            let mt = compress_slice_mt(&data, level, true, 4, None);
            assert!(
                mt.len() <= st.len() + st.len() / 50,
                "{level:?}: periodic data must match across jobs, mt {} vs st {}",
                mt.len(),
                st.len()
            );
        }
    }

    /// The mid-size prefix-LDM class (dll32-shaped frames): jobs borrow the
    /// whole clamped window as their strip, so repeats beyond the chain
    /// reach survive the job split — the multithreaded ratio stays near the
    /// frame-continuous one instead of dropping the far class. The corpus
    /// tiles four mutated copies of one 5 MiB unit, so its redundancy sits
    /// at 5 MiB periods: beyond the row's W22 reach, exactly LDM's class.
    #[test]
    fn mt_midsize_prefix_ldm_keeps_far_repeats() {
        let unit_len = 5 * 1024 * 1024;
        let mut unit = lcg(unit_len);
        // A code-like 256 KiB head — instructions drawn from a 96-pattern
        // pool: wide alphabet (the engagement screen's bar) and repeated
        // 8-byte windows (its parse evidence), aperiodic so the probe's
        // keep parse pays no periodic-bucket walks. The head re-copies 1
        // MiB into the unit (a repeat only the stock reach finds), and
        // the unit-to-unit copies below sit at 5 MiB periods — beyond the
        // chain reach, exactly LDM's class.
        let head_len = 256 * 1024;
        let mut patterns = Vec::with_capacity(96);
        let mut state = 0x243f_6a88_85a3_08d3u64;
        for _ in 0..96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            patterns.push(state.to_le_bytes());
        }
        let mut head = Vec::with_capacity(head_len);
        let mut pick = 0xdead_beef_cafeu64;
        while head.len() < head_len {
            pick = pick.wrapping_mul(6364136223846793005).wrapping_add(1);
            head.extend_from_slice(&patterns[(pick >> 33) as usize % 96]);
        }
        unit[..head_len].copy_from_slice(&head);
        unit[1024 * 1024..1024 * 1024 + head_len].copy_from_slice(&head);
        let mut data = Vec::with_capacity(4 * unit_len);
        for copy in 0..4u32 {
            for (i, &b) in unit.iter().enumerate() {
                // One flipped byte per 4 KiB keeps the copies' 64-byte
                // windows intact while no copy is exact.
                data.push(if i % 4096 == (copy as usize * 1024) % 4096 {
                    b ^ 0x5a
                } else {
                    b
                });
            }
        }
        let st = compress_slice_mt(&data, Level::Balanced, true, 1, None);
        let mt4 = compress_slice_mt(&data, Level::Balanced, true, 4, None);
        let mt8 = compress_slice_mt(&data, Level::Balanced, true, 8, None);
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder
            .decode_all(&mt4, &mut out)
            .unwrap_or_else(|e| panic!("midsize decode: {e}"));
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(mt4.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        // Same job grid at this size, so the worker count cannot show in
        // the bytes; and repeated runs are identical by construction.
        if mt4 != mt8 {
            let at = mt4.iter().zip(mt8.iter()).position(|(a, b)| a != b);
            panic!(
                "mt4 {} mt8 {} st {} first-diff {at:?}",
                mt4.len(),
                mt8.len(),
                st.len()
            );
        }
        assert_eq!(
            mt4,
            compress_slice_mt(&data, Level::Balanced, true, 4, None)
        );
        assert!(
            mt4.len() <= st.len() + st.len() / 50,
            "the far class must survive the job split: mt {} vs st {}",
            mt4.len(),
            st.len()
        );
    }

    /// Inputs below the engagement threshold must fall back to the
    /// single-thread path byte-for-byte.
    #[test]
    fn small_inputs_fall_back_identically() {
        let data = textish(64 * 1024);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4, None);
        let st = crate::encoding::compress_slice_to_vec(&data, Level::Fastest);
        assert_eq!(mt, st);
    }

    /// The tree rows (`BtLazy`, `Opt`) never read the ramp gate: arming one
    /// of their jobs would silently run the experiment unconstrained, so
    /// `arm_ramp` refuses. Arming the gate directly needs no env override —
    /// the strategy check is unconditional and the test stays race-free
    /// against the process-global ramp override other lib tests use.
    #[test]
    fn ramp_arm_refuses_tree_rows() {
        for level in [Level::Best, Level::Opt] {
            let mut state = take_slice_state(
                level,
                crate::InputShape::default(),
                reach_probe::ReachChoice::Keep,
                LdmArming::Frame,
            );
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.matcher.arm_ramp(1 << 20, 1 << 20);
            }));
            let payload = match attempt {
                Ok(()) => panic!("{level:?}: arming must refuse, not arm"),
                Err(payload) => payload,
            };
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            assert!(
                message.contains("deep-offset ramp"),
                "{level:?}: unexpected panic message: {message}"
            );
        }
    }

    /// The scan rows (here: the Balanced chain row) still arm, so the guard
    /// cannot widen past the tree rows.
    #[test]
    fn ramp_arm_still_arms_chain_rows() {
        let mut state = take_slice_state(
            Level::Balanced,
            crate::InputShape::default(),
            reach_probe::ReachChoice::Keep,
            LdmArming::Frame,
        );
        state.matcher.arm_ramp(1 << 20, 1 << 20);
    }
}
