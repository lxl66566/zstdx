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
    compress_fastest, far_screen,
    frame_compressor::{
        CompressState, JobSpf, compress_job_blocks_inner, new_slice_state, reset_slice_state,
        return_slice_state, take_slice_state,
    },
    frame_header::FrameHeader,
    match_generator::{
        LDM_FULL_WINDOW, LdmArming, LdmPrefixSnapshot, MatchGeneratorDriver, SPF_MIN_PREFIX,
        SPF_SEG, StripSnapshot, ldm_head_parses,
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
/// The capture grid's job-size ceiling (see `capture_grid`): keeps huge
/// capture-class inputs finely gridded instead of growing one job per
/// eighth of the input forever.
const CAPTURE_JOB_CAP: u64 = 16 * 1024 * 1024;

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
    // Capture frames pin the grid at the reach floor (worker-independent
    // — see `capture_grid`); the analysis tools must cut at the grid the
    // encoder actually runs. `head` is the whole input (the screen clamps
    // its own span).
    let fast_window = far_screen::mt_capture_window(level, shape, head);
    capture_grid(len, level, shape, choice, head, fast_window)
        .unwrap_or_else(|| job_size_for(len, workers.max(2), overlap))
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
    // The fast rows' far-class capture window (R21): resolved from the
    // same head sample the frame-continuous entries screen — None for
    // every chain row (strategy-checked before any screen work runs).
    let fast_window = far_screen::mt_capture_window(level, shape, src);
    // The frame's head decides its chain reach (see reach_probe), and with
    // it the strip: a shrunk search domain shrinks the strip to match.
    // Eligible frames donate the probe's keep side into job zero (see
    // `donate_job_zero_prefix`): the first span blocks run as job zero's
    // own blocks on the calling thread through the job machinery, so only
    // a Shrink verdict pays a re-parse. The keep grid must leave job zero
    // work beyond the span for the continuation to take over.
    let mut donation = None;
    // Whether the donated state arms with the capture's arming
    // (see `donation_arming`): the pairing invariant below checks the
    // capture the frame finally runs against it.
    let mut donation_prefix = false;
    // The capture plan a Keep verdict would run, computed before the
    // donation so the donated span can arm for it; reused as the frame's
    // plan when the verdict keeps.
    let mut keep_plan = None;
    let choice;
    let head = &src[..src.len().min(MAX_BLOCK_SIZE as usize)];
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
        // Capture frames pin the job grid at the reach floor instead of
        // the worker-scaled formula: the grid (and with it the frame
        // bytes) stays identical at every worker count, and the floor
        // already dominates the formula wherever the mid-size band
        // engaged before (a 32 MiB capture source grids 4 MiB jobs at
        // any worker count).
        let keep_job_size = capture_grid(
            src.len() as u64,
            level,
            shape,
            reach_probe::ReachChoice::Keep,
            head,
            fast_window,
        )
        .unwrap_or_else(|| job_size_for(src.len() as u64, workers.max(2), keep_strip));
        keep_plan = plan_prefix_ldm(
            src,
            level,
            shape,
            reach_probe::ReachChoice::Keep,
            keep_job_size,
            src.len().div_ceil(keep_job_size),
            fast_window,
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
    let job_size = capture_grid(src.len() as u64, level, shape, choice, head, fast_window)
        .unwrap_or_else(|| job_size_for(src.len() as u64, workers, overlap));
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
            .or_else(|| plan_prefix_ldm(src, level, shape, choice, job_size, n_jobs, fast_window))
    } else {
        None
    };
    // Pairing invariant: a chain-row capture the frame runs is one the
    // donated job zero armed for (the capture's window band keeps the
    // frame probe-eligible, so the gate passed and the plan was computed
    // there). The fast rows' capture (`fast_window`) never donates — the
    // rows are not probe subjects, and their grid is pinned the same way.
    debug_assert!(spf.is_none() || donation_prefix || fast_window.is_some());
    // A captured fast row declares and runs the far window its screen or
    // geometry armed (the chain rows' capture window is already their own
    // row window, so `window` moves only on the fast rows).
    let window = match (&spf, fast_window) {
        (Some(_), Some(far)) => far,
        _ => window,
    };
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
                    // The no-build capture never waits: an above-median
                    // job takes the snapshot-less windowed prefill and
                    // cold-fills its own span, a below-median job the
                    // stock strip fill. Adopters of the shared prefix
                    // fill (the build bands) wait for the calling
                    // thread's build (it runs while the workers encode
                    // the below-boundary jobs); a start the build's
                    // freeze overshot keeps the stock whole-prefix fill,
                    // and the windowed band takes the newest LDM snapshot
                    // at or below its start (the adoption is
                    // byte-invariant in the snapshot's boundary, so which
                    // one lands is pure scheduling).
                    let snapshot = match spf.as_ref() {
                        Some(PrefixPlan::SelfFill { med }) if start as u64 > *med => {
                            JobSpfOwned::Windowed(None)
                        },
                        Some(PrefixPlan::SelfFill { .. }) => JobSpfOwned::None,
                        Some(plan) if start as u64 > plan_med(plan) => match plan {
                            PrefixPlan::Whole { med: _, share } => {
                                match wait_prefix_build(share, &ready, &poison) {
                                    Some((upto, snap)) => {
                                        JobSpfOwned::Whole((start as u64 > upto).then_some(snap))
                                    },
                                    None => {
                                        // Poisoned while waiting: release the
                                        // slot so the ordered assembly drains
                                        // before the panic resumes.
                                        *slots[id].lock().unwrap() = Some(Vec::new());
                                        ready.notify_all();
                                        break;
                                    },
                                }
                            },
                            PrefixPlan::Windowed(w) => {
                                match wait_ldm_snapshot(w, &ready, &poison, start as u64) {
                                    Some(snap) => JobSpfOwned::Windowed(Some(snap)),
                                    None if poison.lock().unwrap().is_some() => {
                                        *slots[id].lock().unwrap() = Some(Vec::new());
                                        ready.notify_all();
                                        break;
                                    },
                                    // The build ended without a usable
                                    // publish: the stock from-zero fill
                                    // (byte-equal to an adoption there).
                                    None => JobSpfOwned::Windowed(None),
                                }
                            },
                            // Intercepted by the no-build arms above.
                            PrefixPlan::SelfFill { .. } => unreachable!(),
                        },
                        _ => JobSpfOwned::None,
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
                                job_overlap,
                                JobSpf::None,
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
                            snapshot.job_spf(),
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
        // place of the tail jobs' redundant whole-prefix fills. The
        // no-build capture skips this entirely — its jobs never wait for
        // a snapshot, so a serial fill here would only extend the wall.
        if let Some(plan) = &spf {
            #[cfg(feature = "job_trace")]
            let trace_spf = std::time::Instant::now();
            // Caller-side span, its own counter (see Snapshot::spf_build_ns):
            // the build is posting-thread work that the per-job prefill
            // spans cannot see.
            match plan {
                PrefixPlan::Whole { med, share } => {
                    let build = build_prefix_snapshot(src, level, shape, *med)
                        .map(|(upto, snapshot)| PrefixBuild { upto, snapshot });
                    *share.lock().unwrap() = build;
                },
                // The windowed band's LDM-only build: one sequential fill
                // of the prefix, publishing a snapshot at every cadence
                // step so each adopting job's own remainder fill stays
                // bounded wherever the build has reached when the job's
                // worker picks it up.
                PrefixPlan::Windowed(w) => build_ldm_prefix_chain(src, level, shape, w, &ready),
                PrefixPlan::SelfFill { .. } => {},
            }
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

/// The bulk path's shared prefix fill plan (the LDM capture classes, see
/// `compress_slice_mt`): jobs starting above the median boundary `med`
/// draw their history from the calling thread's build instead of filling
/// their whole frame prefix from scratch. Two bands share the schedule:
/// the mid-size band builds one whole-strip snapshot to `med`, the
/// full-window band runs an LDM-only chain of snapshots through the last
/// adopter's start. The fast rows' capture takes neither: their encode is
/// too cheap to hide a serial build, so every job cold-fills its own
/// window span on its worker (the no-build schedule).
enum PrefixPlan {
    Whole {
        med: u64,
        share: Arc<Mutex<Option<PrefixBuild>>>,
    },
    Windowed(WindowPlan),
    /// The fast rows' no-build capture (R22): no shared fill, no waits.
    /// Jobs above `med` take the snapshot-less [`JobSpf::Windowed`] and
    /// `windowed_ldm_prefill` cold-fills their clamped window span
    /// (bit-identical to the adoption it replaces below the window bar —
    /// where the whole frame prefix is the span; above it the cold re-arm
    /// reshuffles equal-length tie-breaks, see `windowed_ldm_prefill`);
    /// jobs at or below `med` keep the stock [`JobSpf::None`] strip fill
    /// exactly as the build bands' below-boundary jobs — the same split
    /// the shared build made, so a source inside the window keeps its
    /// bytes untouched.
    SelfFill {
        med: u64,
    },
}

/// The windowed band's plan: an LDM-only build that starts at `med` and
/// publishes snapshots every `cadence` bytes up to `stop`, the last
/// adopting job's start.
struct WindowPlan {
    med: u64,
    stop: u64,
    cadence: u64,
    share: Arc<Mutex<WindowShare>>,
}

/// The windowed build's published state. The snapshot list is never
/// pruned: the cadence spans `med..stop` in at most ~13 steps, and every
/// adopter (start > med) must always find an entry at or below its start
/// whatever the build's progress when its worker arrives.
struct WindowShare {
    publishes: Vec<Arc<LdmPrefixSnapshot>>,
    done: bool,
}

/// The median boundary shared by the build bands' schedules.
fn plan_med(plan: &PrefixPlan) -> u64 {
    match plan {
        PrefixPlan::Whole { med, .. } => *med,
        PrefixPlan::Windowed(w) => w.med,
        PrefixPlan::SelfFill { .. } => 0,
    }
}

/// The job-side artifact of the shared prefix fill: what `run_job` hands
/// down as the job's [`JobSpf`].
enum JobSpfOwned {
    None,
    Whole(Option<Arc<StripSnapshot>>),
    Windowed(Option<Arc<LdmPrefixSnapshot>>),
}

impl JobSpfOwned {
    fn job_spf(&self) -> JobSpf<'_> {
        match self {
            JobSpfOwned::None => JobSpf::None,
            JobSpfOwned::Whole(snap) => match snap {
                Some(s) => JobSpf::Whole(s),
                // A build that overshot this job's start: the stock
                // whole-strip fill.
                None => JobSpf::None,
            },
            JobSpfOwned::Windowed(snap) => JobSpf::Windowed(snap.as_deref()),
        }
    }
}

/// The capture grid's job size for a source of `len` bytes at a chain
/// reach of `reach` (see `capture_grid`): shared by the bulk planner and
/// the pledged stream's re-grid so the two run the same lattice.
pub(crate) fn capture_job_size(len: u64, reach: usize) -> usize {
    let floor = MIN_JOB_SIZE.max(reach);
    len.div_ceil(8).clamp(floor as u64, CAPTURE_JOB_CAP) as usize
}

/// The capture classes' head engagement screen, shared by `capture_grid`
/// and `plan_prefix_ldm`: the chain rows and the fast rows' full band keep
/// the near-repeat collision screen (`ldm_head_parses`) — what keeps a
/// max-entropy head (random) or a low-alphabet one (json-class) out of
/// the capture's per-job LDM machinery — while the fast rows' mid band
/// carries the far screen's own verdict (the content-aligned twin count
/// that armed the window in `far_screen::mt_capture_window`, strictly
/// stronger evidence than the collision this checks for). `head_block` is
/// the frame's first block.
fn capture_head_parses(
    shape: crate::InputShape,
    fast_window: Option<u64>,
    head_block: &[u8],
) -> bool {
    if fast_window.is_some() && shape.len.is_some_and(|n| n < LDM_FULL_WINDOW as u64) {
        return true;
    }
    ldm_head_parses(head_block)
}

/// The capture grid's job size when the frame's class engages the shared
/// prefix fill (see `plan_prefix_ldm`): pinned at the reach floor — the
/// same gates as the plan, so the grid the plan was laid out on is the
/// one the encoder runs, and it is independent of the worker count (the
/// frame bytes cannot depend on how many workers pick the jobs up).
/// `fast_window` is the fast rows' pre-screened capture window
/// (`far_screen::mt_capture_window`; `None` for every other class).
fn capture_grid(
    len: u64,
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    head: &[u8],
    fast_window: Option<u64>,
) -> Option<usize> {
    let window = MatchGeneratorDriver::prefix_ldm_window(level, shape, choice).or(fast_window)?;
    let head = &head[..head.len().min(MAX_BLOCK_SIZE as usize)];
    if !capture_head_parses(shape, fast_window, head) {
        return None;
    }
    let job_size = capture_job_size(
        len,
        MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize,
    );
    // Eight jobs' worth of input, clamped to the [reach floor, 16 MiB]
    // band (see `capture_job_size`): the floor is where the mid-size
    // capture always gridded (a 32 MiB source keeps its exact 4 MiB
    // lattice), and the 16 MiB side keeps huge inputs finely gridded.
    // Below ~3x the reach the per-job LDM refills measurably erode the
    // far class: each small job's exhaustive fill of its recent history
    // floods the 16-deep LDM buckets and evicts older unique twins that a
    // long job's parse-indexed history (matched interiors skipped) would
    // keep (dll100 at 4 MiB jobs: +0.97 MB over st; at 13.1 MiB: +12 KB).
    let n_jobs = len.div_ceil(job_size as u64) as usize;
    let prefix_last = (1..n_jobs)
        .filter(|&i| (i * job_size) as u64 <= window)
        .last()?;
    ((prefix_last * job_size) as u64 >= SPF_MIN_PREFIX).then_some(job_size)
}

/// Whether the bulk path runs a frame on prefix-strip LDM jobs (the LDM
/// capture classes, see `compress_slice_mt`): the row/window/verdict
/// class plus a head that both parses and carries a wide alphabet, and a
/// job grid whose prefix strips are large enough to pay for the shared
/// fill. Deterministic in the input and level alone (the grid is pinned,
/// see `capture_grid`). `fast_window` is the fast rows' pre-screened
/// capture window (`far_screen::mt_capture_window`; `None` for every
/// other class).
fn plan_prefix_ldm(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
    job_size: usize,
    n_jobs: usize,
    fast_window: Option<u64>,
) -> Option<PrefixPlan> {
    let window = MatchGeneratorDriver::prefix_ldm_window(level, shape, choice).or(fast_window)?;
    let head = &src[..src.len().min(MAX_BLOCK_SIZE as usize)];
    if !capture_head_parses(shape, fast_window, head) {
        return None;
    }
    debug_assert_eq!(
        shape
            .len
            .and_then(|len| capture_grid(len, level, shape, choice, head, fast_window)),
        Some(job_size),
        "the caller pinned the capture grid"
    );
    // Tail jobs whose window strip is a whole frame prefix: the source
    // sits inside the clamped window in the mid-size band, but the bound
    // stays explicit — a mid-stream strip keeps the stock fill either way.
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
    // The fast rows' capture runs the no-build schedule (see
    // `PrefixPlan::SelfFill`): their encode cannot hide a serial build, so
    // the engagement conditions above stay the only gates and every
    // above-boundary job owns its fill.
    if fast_window.is_some() {
        return Some(PrefixPlan::SelfFill { med });
    }
    if MatchGeneratorDriver::windowed_ldm_capture(window) {
        // The last adopting job's start: the build needs to reach it (its
        // own remainder fill then covers the rest), and no further.
        let stop = ((n_jobs - 1) * job_size) as u64;
        // At most ~13 publishes span med..stop, so the unpruned snapshot
        // list stays bounded while every adopter can find an entry at or
        // below its start.
        let cadence = ((stop - med) / 12).max(SPF_SEG);
        Some(PrefixPlan::Windowed(WindowPlan {
            med,
            stop,
            cadence,
            share: Arc::new(Mutex::new(WindowShare {
                publishes: Vec::new(),
                done: false,
            })),
        }))
    } else {
        Some(PrefixPlan::Whole {
            med,
            share: Arc::new(Mutex::new(None)),
        })
    }
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
    share: &Arc<Mutex<Option<PrefixBuild>>>,
    ready: &Condvar,
    poison: &Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>>,
) -> Option<(u64, Arc<StripSnapshot>)> {
    let mut guard = share.lock().unwrap();
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

/// The windowed band's adopter wait: the newest published LDM snapshot at
/// or below `start` (which one lands is pure scheduling — the adoption's
/// bytes are invariant in the snapshot's boundary, see
/// `LdmPrefixSnapshot`; the newest just minimizes this job's own
/// remainder fill). `None` once the build is done without a usable
/// publish (a build that found no freeze — the job then fills its whole
/// prefix from zero, byte-equal to an adoption) or the pool is poisoned.
fn wait_ldm_snapshot(
    plan: &WindowPlan,
    ready: &Condvar,
    poison: &Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>>,
    start: u64,
) -> Option<Arc<LdmPrefixSnapshot>> {
    let mut guard = plan.share.lock().unwrap();
    loop {
        if let Some(snap) = guard.publishes.iter().rev().find(|s| s.upto <= start) {
            return Some(Arc::clone(snap));
        }
        if guard.done || poison.lock().unwrap().is_some() {
            return None;
        }
        guard = ready.wait(guard).unwrap();
    }
}

/// The windowed band's caller-side build: one sequential LDM fill of the
/// prefix through `stop`, segmented at batch-freeze points, publishing a
/// snapshot whenever the fill crosses the next cadence target (and at
/// `med`, the first target, which every adopter can use). Publishing
/// wakes the waiting adopters through the shared `ready` condvar.
fn build_ldm_prefix_chain(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
    plan: &WindowPlan,
    ready: &Condvar,
) {
    let mut state = take_slice_state(
        level,
        shape,
        reach_probe::ReachChoice::Keep,
        LdmArming::JobPrefix,
    );
    // The stock prefill's LDM restart (fresh at 0, entry floor 0) — the
    // chain tables stay untouched: this build only feeds the LDM state.
    state.matcher.prefill_window(&[], 0);
    let cap = ((plan.stop + SPF_SEG) as usize).min(src.len());
    let mut upto = 0u64;
    let mut next_pub = plan.med;
    while upto < plan.stop {
        let soft = (upto + SPF_SEG).min(plan.stop);
        let Some(freeze) = state.matcher.ldm_fill_segment(&src[..cap], 0, upto, soft) else {
            break;
        };
        upto = freeze;
        if upto >= next_pub && upto <= plan.stop {
            let snap = Arc::new(state.matcher.ldm_snapshot(upto));
            plan.share.lock().unwrap().publishes.push(snap);
            next_pub = upto + plan.cadence;
            ready.notify_all();
        }
    }
    plan.share.lock().unwrap().done = true;
    return_slice_state(state);
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
/// prefix fill's engagement (see [`JobSpf`]) — the windowed band prefill
/// indexes only the chain's reach tail while the job's window (`overlap`)
/// spans the whole LDM history.
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
    spf: JobSpf<'_>,
) -> Vec<u8> {
    #[cfg(feature = "job_trace")]
    let trace_reset = std::time::Instant::now();
    prepare_job_state(state, level, shape, choice, ldm, job.start as u64);
    #[cfg(feature = "job_trace")]
    super::job_trace::add_reset(trace_reset);
    // The windowed band's chain tables cover the reach tail; every other
    // engagement indexes the whole strip `overlap` names.
    let chain_overlap = if matches!(spf, JobSpf::Windowed(_)) {
        MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize
    } else {
        overlap
    };
    let start = job.start;
    compress_job_blocks_inner(
        state,
        src,
        job,
        overlap,
        is_last_job,
        start,
        Vec::new(),
        chain_overlap,
        spf,
    )
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
    spf: JobSpf<'_>,
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
        capture_grid, compress_slice_mt, donation_arming, job_size_for, plan_prefix_ldm,
        reach_probe, take_slice_state,
    };
    use crate::{
        Level,
        common::MAX_BLOCK_SIZE,
        decoding::FrameDecoder,
        encoding::{
            match_generator::{LDM_FULL_WINDOW, LdmArming, MatchGeneratorDriver},
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
            // The mid-size band: row 9's source-clamped window is W25.
            (24 * mib, 2, None, true),
            (17 * mib, 4, None, true),
            // A forced window inside the band engages on a large source.
            (128 * mib, 4, Some(25), true),
            // The full-window band (window W26) engages too — on the
            // windowed job model (chain tables over the reach tail, LDM
            // through the snapshot ring).
            (64 * mib, 4, None, true),
            // Below the band there are no prefix strips worth sharing.
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
            // The planner pins the capture grid (see `compress_slice_mt`).
            let job_size = capture_grid(len, Level::Balanced, shape, ReachChoice::Keep, &src, None)
                .unwrap_or_else(|| job_size_for(len, workers.max(2), strip));
            let n_jobs = len.div_ceil(job_size as u64) as usize;
            let plan = plan_prefix_ldm(
                &src,
                Level::Balanced,
                shape,
                ReachChoice::Keep,
                job_size,
                n_jobs,
                None,
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
            None,
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

    /// The fast rows' far-class capture (R21): a mid-band far-class frame —
    /// the R20 fixture class, near-field head twins clearing the screen's
    /// cheap-reject bar and 3 MiB unit copies carrying the paying 2-4 MiB
    /// class — grids onto the capture lattice with the far window declared
    /// and the far class riding cross-job LDM, while a same-size random
    /// source stays stock. The capture is deterministic in the input alone
    /// (mt4 == mt8), and the armed multithreaded frame tracks the armed
    /// single-threaded one (the workers-1 fallback runs the same screen).
    #[test]
    fn fast_row_midband_capture_arms_and_random_stays_stock() {
        let mib = 1024 * 1024;
        let head = lcg(256 * 1024);
        let unit = lcg(3 * mib);
        let mut data = Vec::with_capacity(4 * head.len() + 11 * unit.len());
        for (src, copies) in [(&head, 4u32), (&unit, 11u32)] {
            for copy in 0..copies {
                for (i, &b) in src.iter().enumerate() {
                    data.push(if i % 4096 == (copy as usize * 1024) % 4096 {
                        b ^ 0x5a
                    } else {
                        b
                    });
                }
            }
        }
        let random = lcg(data.len());
        for level in [Level::Fastest, Level::Fast] {
            let st = compress_slice_mt(&data, level, true, 1, None);
            let mt4 = compress_slice_mt(&data, level, true, 4, None);
            let mt8 = compress_slice_mt(&data, level, true, 8, None);
            for (name, frame) in [("st", &st), ("mt4", &mt4)] {
                let mut out = vec![0u8; data.len()];
                let mut decoder = FrameDecoder::new();
                let n = decoder
                    .decode_all(frame, &mut out)
                    .unwrap_or_else(|e| panic!("{level:?} {name}: {e}"));
                assert_eq!((n, &out[..n]), (data.len(), &data[..]), "{level:?} {name}");
                let mut libzstd = Vec::new();
                zstd::stream::copy_decode(frame.as_slice(), &mut libzstd).unwrap();
                assert_eq!(libzstd, data, "{level:?} {name} libzstd");
            }
            assert_eq!(mt4, mt8, "{level:?}: mt4 != mt8");
            // The armed frame carries the far window (W26) in its window
            // descriptor byte, at the same offset the single-threaded
            // entry's does.
            assert_eq!(mt4[5], 0x80, "{level:?}: armed window descriptor");
            // The far class rides the cross-job LDM: within the job-split
            // noise of the frame-continuous armed parse, and far under the
            // stock parse a same-size random source takes.
            assert!(
                mt4.len() <= st.len() + st.len() / 20,
                "{level:?}: mt {} vs armed st {}",
                mt4.len(),
                st.len()
            );
            let stock_random = compress_slice_mt(&random, level, true, 4, None);
            assert!(
                mt4.len() * 2 < stock_random.len(),
                "{level:?}: armed {} vs random {}",
                mt4.len(),
                stock_random.len()
            );
            // The random head fails the screen: its frame keeps the stock
            // window — the same header bytes a below-band random source
            // carries (the FCS tail differs; the prefix compared here does
            // not).
            let below_band = compress_slice_mt(&random[..15 * mib], level, true, 4, None);
            assert_eq!(
                &stock_random[..6],
                &below_band[..6],
                "{level:?}: stock header moved"
            );
        }
    }

    /// The fast rows' full-band capture engages on geometry (no screen may
    /// run there), gated by the same head engagement screen the chain rows
    /// use — a max-entropy head keeps the per-job stock parse.
    #[test]
    fn fast_row_fullband_capture_engages_on_geometry() {
        let parses = pattern_head(MAX_BLOCK_SIZE as usize);
        let rejects = lcg(MAX_BLOCK_SIZE as usize);
        let shape = crate::InputShape {
            len: Some(100 * 1024 * 1024),
            window_log: None,
        };
        for level in [Level::Fastest, Level::Fast] {
            for (name, head) in [("parses", &parses), ("random", &rejects)] {
                let fast_window = super::far_screen::mt_capture_window(level, shape, head);
                assert_eq!(
                    fast_window,
                    Some(LDM_FULL_WINDOW as u64),
                    "{level:?} {name}"
                );
                let grid = capture_grid(
                    shape.len.unwrap(),
                    level,
                    shape,
                    ReachChoice::Keep,
                    head,
                    fast_window,
                );
                assert_eq!(grid.is_some(), name == "parses", "{level:?} {name}");
            }
        }
    }

    /// The fast rows' no-build capture (R22): a full-band frame runs every
    /// job on the snapshot-less windowed prefill — the last jobs start
    /// past the W26 window, so their spans cold re-arm at a non-zero
    /// `ldm_base` (the distance-filter equivalence, see
    /// `windowed_ldm_prefill`). The frame stays worker-independent and
    /// deterministic (no scheduling input exists without a shared build)
    /// and the far class rides the per-job LDM within the job-split noise
    /// of the frame-continuous armed parse.
    #[test]
    fn fast_row_fullband_capture_self_fills_midstream_jobs() {
        const MIB: usize = 1024 * 1024;
        let total = 76 * MIB;
        let unit_len = 5 * MIB;
        let mut unit = lcg(unit_len);
        let head_len = 256 * 1024;
        unit[..head_len].copy_from_slice(&pattern_head(head_len));
        let head_copy = unit[..head_len].to_vec();
        unit[MIB..MIB + head_len].copy_from_slice(&head_copy);
        let mut data = Vec::with_capacity(total);
        for copy in 0..u32::MAX {
            if data.len() >= total {
                break;
            }
            let take = unit.len().min(total - data.len());
            let flip = (copy as usize * 1024) % 4096;
            for (i, &b) in unit[..take].iter().enumerate() {
                data.push(if i % 4096 == flip {
                    b ^ 0x5a
                } else {
                    b
                });
            }
        }
        for level in [Level::Fastest, Level::Fast] {
            let st = compress_slice_mt(&data, level, true, 1, None);
            let mt4 = compress_slice_mt(&data, level, true, 4, None);
            let mt8 = compress_slice_mt(&data, level, true, 8, None);
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder
                .decode_all(&mt8, &mut out)
                .unwrap_or_else(|e| panic!("{level:?}: {e}"));
            assert_eq!((n, &out[..n]), (data.len(), &data[..]), "{level:?}");
            let mut decoded = Vec::new();
            zstd::stream::copy_decode(mt8.as_slice(), &mut decoded).unwrap();
            assert_eq!(decoded, data, "{level:?} libzstd");
            assert_eq!(mt4, mt8, "{level:?}: mt4 != mt8");
            assert_eq!(
                mt8,
                compress_slice_mt(&data, level, true, 8, None),
                "{level:?}: run-to-run determinism"
            );
            assert!(
                mt8.len() <= st.len() + st.len() / 20,
                "{level:?}: the far class must survive the self-fill: mt {} vs armed st {}",
                mt8.len(),
                st.len()
            );
        }
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

    /// The windowed capture band (window at the full W26 bar): a wide-
    /// alphabet keep-class frame larger than the window grids pinned
    /// capture jobs whose LDM history spans the whole window — the far
    /// class beyond the chain reach survives the job split at every
    /// worker count, including the mid-stream jobs whose window strips
    /// reach back past the frame prefix (start > window). 76 MiB grids
    /// eight 9.5 MiB jobs, the last starting at 66.5 MiB.
    #[test]
    fn mt_windowed_ldm_capture_is_worker_independent() {
        const MIB: usize = 1024 * 1024;
        let total = 76 * MIB;
        // A 5 MiB unit tiled with per-copy mutations: the unit-to-unit
        // copies sit at 5 MiB periods — beyond the row's W22 chain reach,
        // exactly LDM's class — and the flipped byte per 4 KiB keeps
        // every 64-byte window intact while no copy is exact.
        let unit_len = 5 * MIB;
        let mut unit = lcg(unit_len);
        let head_len = 256 * 1024;
        unit[..head_len].copy_from_slice(&pattern_head(head_len));
        // A 1 MiB re-copy of the head inside the unit: a near repeat the
        // stock W22 reach finds, so the probe's keep parse wins and the
        // frame keeps its LDM (a shrink verdict abandons the far class).
        let head_copy = unit[..head_len].to_vec();
        unit[MIB..MIB + head_len].copy_from_slice(&head_copy);
        let mut data = Vec::with_capacity(total);
        for copy in 0..u32::MAX {
            if data.len() >= total {
                break;
            }
            let take = unit.len().min(total - data.len());
            let flip = (copy as usize * 1024) % 4096;
            for (i, &b) in unit[..take].iter().enumerate() {
                data.push(if i % 4096 == flip {
                    b ^ 0x5a
                } else {
                    b
                });
            }
        }
        let st = compress_slice_mt(&data, Level::Balanced, true, 1, None);
        let mt4 = compress_slice_mt(&data, Level::Balanced, true, 4, None);
        let mt8 = compress_slice_mt(&data, Level::Balanced, true, 8, None);
        // Roundtrip through both decoders.
        let mut out = vec![0u8; data.len()];
        let mut decoder = FrameDecoder::new();
        let n = decoder
            .decode_all(&mt8, &mut out)
            .unwrap_or_else(|e| panic!("windowed decode: {e}"));
        assert_eq!((n, &out[..n]), (data.len(), &data[..]));
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(mt8.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        // The pinned grid keeps the bytes worker-independent, and repeated
        // runs are identical by construction.
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
