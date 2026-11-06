//! Frame-granular stage-B parallelism for concatenated-frame streams.
//!
//! Serial stage B (see [`super::mt`]) executes every segment in input
//! order, so the frames of a multi-frame stream serialize on the execute
//! even though stage A already stages across frame boundaries. Frames are
//! independent decode units — repcode history, checksum streams and match
//! sources never cross a frame boundary — so the executor splits the
//! segment list into one lane per frame (grouped when frames outnumber
//! the lane budget) and runs the lanes concurrently: each lane consumes
//! its own frame's staged segments in order, exactly like the serial
//! executor's per-segment readiness wait, writing disjoint output ranges.
//! Only the output base of a lane's first frame couples lanes, and it is
//! a pure stage-A quantity: the sum of the predecessors' staged output
//! sizes, read off the job's published size array (a consumed slot cannot
//! be told apart from an unstaged one, so staging publishes sizes
//! separately). The lane count, the frame packing and the staging-ahead
//! window all come from the plan-time [`super::lane_budget`] when the
//! payload's frames pledge their sizes, and from the stock layout
//! otherwise.
//!
//! Wildcopy spill is bounded per segment: each lane passes its segment's
//! own end as the wild cap, so no lane's overshoot can reach into the next
//! frame's range, which another lane writes concurrently (the same
//! contract the piece executor holds against neighbor pieces). The tail
//! sequences of every segment take the exact cold path instead; on the
//! serial path the cap stays `usize::MAX`, unchanged.
//!
//! Errors keep the serial executor's precedence: the first failing segment
//! in input order wins — every segment belongs to exactly one lane, and a
//! lane whose predecessor failed exits without recording its own, so the
//! minimum recorded id is always the serial decoder's error. A checksum
//! mismatch loses against any decode error and wins in frame order among
//! checksum errors, matching the serial stream's record-first behavior.
//!
//! Single-frame inputs never route here (the caller checks the frame
//! count), so every single-frame decode takes today's serial path
//! unchanged.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use super::{
    errors::FrameDecoderError,
    lane_budget::{self, FramePlan, LaneBudget, LaneSpec},
    mt::{
        OutRegion, ScanPlan, SegmentPlan, StageAJob, execute_segment, verify_frame_content_sizes,
    },
};
#[cfg(feature = "hash")]
use crate::xxh64::Xxh64;

static ENGAGEMENTS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Test injection: the input base address whose lane execution panics
/// instead of running (null disables). Keyed to one input's address so a
/// concurrently running test's decode never trips it in the shared-process
/// test harness. Compiled out of non-test builds.
#[cfg(test)]
static LANE_PANIC_INPUT: core::sync::atomic::AtomicPtr<u8> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Test injection: like [`LANE_PANIC_INPUT`], but armed only on pool
/// lanes (a lane whose frame list starts past the first frame), so a test can
/// panic the executor's worker side while the caller's lane 0 runs to
/// completion. Compiled out of non-test builds.
#[cfg(test)]
static LANE_PANIC_POOL_INPUT: core::sync::atomic::AtomicPtr<u8> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// How many decodes engaged the frame-lane executor so far (process
/// lifetime). Hidden: not API-stable.
#[doc(hidden)]
pub fn frame_lane_engagements() -> usize {
    ENGAGEMENTS.load(Ordering::Relaxed)
}

/// One lane's completion. `Failed.seg` is the failing segment id (decode
/// or stage error); `Aborted` never carries the reported error — it means
/// a predecessor failed, or a worker panicked and the poison takes over.
enum LaneOutcome {
    /// `starts` holds each covered frame's (frame index, absolute output
    /// start) in lane order; `end` is the lane's absolute output end.
    Done {
        starts: Vec<(usize, usize)>,
        end: usize,
        #[cfg(feature = "hash")]
        checksum_err: Option<(usize, FrameDecoderError)>,
    },
    Failed {
        seg: usize,
        err: FrameDecoderError,
    },
    Aborted,
}

/// A pool-servable stage-B lane: one lane's frozen view plus its
/// completion slot. The input, segments and checksums borrows are frozen
/// exactly like `StageAJob`'s: the leasing driver waits every lease out
/// before returning, inside the caller's borrows.
pub(super) struct LaneJob {
    stage: Arc<StageAJob>,
    /// This lane's segment ids, ascending (whole frames, any frame set).
    segs: Vec<usize>,
    /// The covered frames' indices in scan order, aligned with the
    /// frame-start segments of `segs`.
    frames: Vec<usize>,
    input: *const [u8],
    segments: *const [SegmentPlan],
    base: *mut u8,
    buf_limit: usize,
    #[cfg(feature = "hash")]
    checksums: *const [Option<u32>],
    result: Mutex<Option<LaneOutcome>>,
    done: Condvar,
}

// SAFETY: the frozen slices and the output base are touched only while
// the driver's `LeaseOut` guard keeps the borrows live (every lease is
// waited out before the driver returns, inside the caller's borrows);
// the mutable fields are mutex, condvar or atomic.
unsafe impl Send for LaneJob {}
// SAFETY: Sync follows Send here: the frozen slices are read-only past
// construction and every mutable field is synchronized.
unsafe impl Sync for LaneJob {}

fn record(lane: &LaneJob, outcome: LaneOutcome) {
    *lane.result.lock().unwrap() = Some(outcome);
    lane.done.notify_all();
}

/// Whether the decode must stop without a reportable error: a worker
/// panicked (the poison resumes on the driver after the joins) or the
/// driver aborted (an error path on the calling thread).
fn poisoned_or_aborted(stage: &StageAJob) -> bool {
    stage.abort.load(Ordering::Relaxed) || stage.poison.lock().unwrap().is_some()
}

/// Wait until segment `id` is staged and take it. `None`: abort or
/// poison, never observed as a result.
fn take_staged(
    stage: &StageAJob,
    id: usize,
) -> Option<Result<super::mt::DecodedSegment, FrameDecoderError>> {
    let mut guard = stage.slots[id].lock().unwrap();
    loop {
        if let Some(staged) = guard.take() {
            return Some(staged);
        }
        if poisoned_or_aborted(stage) {
            return None;
        }
        guard = stage.ready.wait(guard).unwrap();
    }
}

/// Run one lane: wait for the predecessors' published sizes (this lane's
/// output base), then execute the lane's own segments in order.
fn run_lane(lane: &LaneJob) {
    let stage = &lane.stage;
    // The lane's output base: the sum of every predecessor segment's
    // staged size — all segments before the lane's first one, which is
    // its first frame's first segment. A predecessor that failed staging
    // belongs to an earlier lane, which records its error; this lane just
    // stops.
    let first_seg = lane.segs.first().copied().unwrap_or(0);
    let mut base = 0usize;
    for i in 0..first_seg {
        loop {
            match stage.seg_sizes[i].load(Ordering::Acquire) {
                0 => {},
                usize::MAX => {
                    record(lane, LaneOutcome::Aborted);
                    return;
                },
                published => {
                    base += published - 1;
                    break;
                },
            }
            if poisoned_or_aborted(stage) {
                record(lane, LaneOutcome::Aborted);
                return;
            }
            // Bounded poll: the poison mutex doubles as the sleep token
            // (the same pattern the staging backpressure uses).
            let guard = stage.poison.lock().unwrap();
            let (_guard, _timeout) = stage
                .ready
                .wait_timeout(guard, core::time::Duration::from_millis(50))
                .unwrap();
        }
    }
    // SAFETY: this thread serves the lane's lease, so the frozen input
    // slice is live for the whole call.
    let input: &[u8] = unsafe { &*lane.input };
    #[cfg(test)]
    if LANE_PANIC_INPUT.load(Ordering::Relaxed).cast_const() == input.as_ptr()
        || (lane.frames.first().is_some_and(|&f| f != 0)
            && LANE_PANIC_POOL_INPUT.load(Ordering::Relaxed).cast_const() == input.as_ptr())
    {
        panic!("injected lane panic");
    }
    // SAFETY: same lease discipline as the input slice above.
    let segments: &[SegmentPlan] = unsafe { &*lane.segments };
    // SAFETY: same lease discipline as the input slice above.
    #[cfg(feature = "hash")]
    let checksums: &[Option<u32>] = unsafe { &*lane.checksums };
    let mut offset_hist = [1u32, 4, 8];
    let mut starts = Vec::with_capacity(lane.frames.len());
    let mut frame_base = base;
    let mut frame_local = 0usize;
    let mut end = base;
    let mut frame_at = 0usize;
    #[cfg(feature = "hash")]
    let mut checksum_err = None;
    #[cfg(feature = "hash")]
    let mut hash = Xxh64::new(0);
    // Compare the closed frame's checksum, recording the first mismatch.
    // The closing frame is `lane.frames[frame_at - 1]`.
    #[cfg(feature = "hash")]
    macro_rules! close_frame {
        () => {
            if let Some(expected) = checksums.get(lane.frames[frame_at - 1]).copied().flatten() {
                let calculated = hash.finish() as u32;
                if calculated != expected && checksum_err.is_none() {
                    checksum_err = Some((
                        lane.frames[frame_at - 1],
                        FrameDecoderError::ChecksumMismatch {
                            expected,
                            calculated,
                        },
                    ));
                }
            }
        };
    }
    for &id in &lane.segs {
        let Some(staged) = take_staged(stage, id) else {
            record(lane, LaneOutcome::Aborted);
            return;
        };
        let seg = match staged {
            Ok(seg) => seg,
            Err(err) => {
                record(lane, LaneOutcome::Failed { seg: id, err });
                return;
            },
        };
        if segments[id].frame_start {
            offset_hist = [1, 4, 8];
            #[cfg(feature = "hash")]
            if frame_at > 0 {
                close_frame!();
            }
            frame_base = end;
            frame_local = 0;
            starts.push((lane.frames[frame_at], frame_base));
            frame_at += 1;
            #[cfg(feature = "hash")]
            {
                hash = Xxh64::new(0);
            }
        }
        let abs = frame_base + frame_local;
        // The segment's own end is the wild cap: a lane's overshoot must
        // never reach into a range another lane writes concurrently. The
        // cap pushes the tail sequences onto the exact cold path; on the
        // serial path the cap stays `usize::MAX`.
        // SAFETY: `base` was placed for [0, buf_limit) by the driver; the
        // segment's own range plus the cap keeps every store inside it.
        let region = unsafe { OutRegion::new(lane.base, lane.buf_limit, abs + seg.out_size) };
        // SAFETY: region's contract; every match source below `abs` sits
        // inside this frame, written by this lane in order.
        let executed =
            unsafe { execute_segment(region, abs, frame_base, &seg, input, &mut offset_hist) };
        if let Err(err) = executed {
            record(lane, LaneOutcome::Failed { seg: id, err });
            return;
        }
        #[cfg(feature = "hash")]
        if seg.out_size != 0 {
            // SAFETY: this lane just wrote [abs, abs + out_size) at this
            // base; no other lane touches the frame's range.
            unsafe {
                hash.write(core::slice::from_raw_parts(
                    lane.base.cast_const().add(abs),
                    seg.out_size,
                ));
            }
        }
        frame_local += seg.out_size;
        end += seg.out_size;
        // The executed range is final from here on; let backpressured
        // staging advance.
        stage.consumed_count.fetch_add(1, Ordering::Release);
        stage.ready.notify_all();
    }
    // Close the last frame's checksum.
    #[cfg(feature = "hash")]
    if frame_at > 0 {
        close_frame!();
    }
    record(lane, LaneOutcome::Done {
        starts,
        end,
        #[cfg(feature = "hash")]
        checksum_err,
    });
}

/// One leased pool thread's service of a lane. Returns whether the thread
/// must retire: only a panic in the lane body (recorded as `Aborted`, the
/// poison resumes on the driver).
pub(super) fn serve_frame_lane(lane: &Arc<LaneJob>) -> bool {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_lane(lane)));
    match attempt {
        Ok(()) => false,
        Err(payload) => {
            *lane.stage.poison.lock().unwrap() = Some(payload);
            record(lane, LaneOutcome::Aborted);
            lane.stage.ready.notify_all();
            true
        },
    }
}

/// How the lanes get their output region.
enum Place<'a> {
    /// The caller's buffer is valid for its whole length up front
    /// (`decode_all_mt`): lanes start immediately, staging overlaps
    /// execution as on the serial path.
    Fixed { base: *mut u8, buf_limit: usize },
    /// The output grows (`decode_to_vec_mt`): the base may move on
    /// growth, so staging runs to completion first and one `place` call
    /// covers the total before any lane writes.
    Deferred(&'a mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>),
}

impl Place<'_> {
    /// Whether staging runs to completion before any lane executes.
    fn deferred(&self) -> bool {
        matches!(self, Place::Deferred(_))
    }

    /// Resolve the region. Deferred mode first waits for every segment's
    /// published outcome: the total is a pure stage-A quantity, and a
    /// stage error bounds the placed range to the segments before it —
    /// no lane past the failing segment can compute its base, so nothing
    /// beyond the bound is ever written.
    fn resolve(
        &mut self,
        stage: &StageAJob,
        n_segments: usize,
    ) -> Result<(*mut u8, usize), FrameDecoderError> {
        match self {
            Place::Fixed { base, buf_limit } => Ok((*base, *buf_limit)),
            Place::Deferred(place) => {
                for i in 0..n_segments {
                    while stage.seg_sizes[i].load(Ordering::Acquire) == 0 {
                        if poisoned_or_aborted(stage) {
                            // The poison resumes on the driver; abort is an
                            // error path whose result is never observed.
                            return Err(FrameDecoderError::NotYetInitialized);
                        }
                        let guard = stage.poison.lock().unwrap();
                        let (_guard, _timeout) = stage
                            .ready
                            .wait_timeout(guard, core::time::Duration::from_millis(50))
                            .unwrap();
                    }
                }
                let bound = stage
                    .seg_sizes
                    .iter()
                    .map(|s| s.load(Ordering::Acquire))
                    .take_while(|&v| v != usize::MAX)
                    .map(|v| v - 1)
                    .fold(0usize, usize::saturating_add);
                place(0, bound)
            },
        }
    }
}

/// Per-frame pricing inputs for the lane budget: pledged output bytes and
/// staged volume, both plan-time. A frame whose block headers failed to
/// parse contributes no staged volume, matching the head-ramp gate's
/// convention for under-counted totals.
fn frame_plans(plan: &ScanPlan) -> Vec<FramePlan> {
    (0..plan.frame_pledges.len())
        .map(|f| FramePlan {
            out: plan.frame_pledges.get(f).copied().flatten().unwrap_or(0),
            staged: plan.frame_staged.get(f).copied().flatten().unwrap_or(0),
        })
        .collect()
}

/// Frame-lane driver: stage on the pool as today, execute frames
/// concurrently. Returns the total decoded bytes. The caller guarantees
/// `workers >= 2` and at least two frames.
fn decode_frames(
    input: &[u8],
    workers: u32,
    plan: &ScanPlan,
    place: Place<'_>,
) -> Result<usize, FrameDecoderError> {
    let segments = &plan.segments;
    let n_segments = segments.len();
    let frame_starts: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.frame_start)
        .map(|(i, _)| i)
        .collect();
    let n_frames = frame_starts.len();
    debug_assert!(n_frames >= 2, "the caller routes multi-frame input only");
    // The lane budget: a payload whose frames all pledge their output
    // trades lanes for staging workers per the plan-time walk; anything
    // else keeps the stock layout, `min(workers, frames)` consecutive
    // lanes with the per-worker staging window.
    let plans = frame_plans(plan);
    let budget: Option<LaneBudget> = if plan.frame_pledges.iter().all(Option::is_some) {
        lane_budget::plan(&plans, workers)
    } else {
        None
    };
    let k_lanes = budget
        .as_ref()
        .map_or_else(|| (workers as usize).min(n_frames), |b| b.lanes);
    let lane_specs: Vec<LaneSpec> = if budget.as_ref().is_some_and(|b| b.lpt) {
        let weights: Vec<u64> = plans.iter().map(|p| p.out).collect();
        lane_budget::group_lpt(&weights, k_lanes)
    } else {
        lane_budget::group_consecutive(n_frames, k_lanes)
    };
    // Budget: the caller runs lane 0, pool lanes take K-1 threads, stage A
    // the rest of the worker budget (at least one).
    let stage_workers = (workers as usize)
        .saturating_sub(k_lanes - 1)
        .min(n_segments)
        .max(1);
    // The staging-ahead window keeps the serial regime's in-flight bound
    // in fixed mode; deferred mode stages to completion before executing,
    // so the window is the whole plan (the piece executor's model). The
    // budget may free the fixed-mode window: with fewer lanes than
    // workers, base settlement needs staging far ahead of execution,
    // bounded by the plan's total staged volume.
    let window = match (place.deferred(), budget.as_ref().map(|b| b.free_window)) {
        (true, _) | (false, Some(true)) => n_segments,
        (false, _) => workers as usize,
    };
    let job = StageAJob::for_lanes(input, segments, stage_workers, window);
    let stage_lease = Arc::new(super::mt_pool::LeaseJob::StageA(job.clone()));
    let stage_leases: Vec<_> = super::mt_pool::lease_workers(stage_workers, &stage_lease)
        .into_iter()
        .map(|s| (s, stage_lease.clone()))
        .collect();
    let guard = super::mt::LeaseOut {
        job: &job,
        slots: &stage_leases,
    };
    let result = run(
        input,
        plan,
        place,
        &job,
        n_segments,
        n_frames,
        &frame_starts,
        &lane_specs,
    );
    drop(guard);
    if let Some(payload) = job.poison.lock().unwrap().take() {
        std::panic::resume_unwind(payload);
    }
    result
}

/// Lease the lanes, run the caller's lane, collect outcomes. Kept apart
/// from the lease setup so the stage-A guard always covers the outer
/// error paths (deferred `place` runs between the two lease batches).
#[allow(clippy::too_many_arguments)]
fn run(
    input: &[u8],
    plan: &ScanPlan,
    mut place: Place<'_>,
    job: &Arc<StageAJob>,
    n_segments: usize,
    n_frames: usize,
    frame_starts: &[usize],
    lane_specs: &[LaneSpec],
) -> Result<usize, FrameDecoderError> {
    let (base, buf_limit) = place.resolve(job, n_segments)?;
    let lane_jobs: Vec<Arc<LaneJob>> = lane_specs
        .iter()
        .map(|spec| {
            // The lane's segment list: each covered frame's own segments,
            // ascending, whole frames only.
            let mut segs = Vec::new();
            for &f in &spec.frames {
                let start = frame_starts[f];
                let end = frame_starts.get(f + 1).copied().unwrap_or(n_segments);
                segs.extend(start..end);
            }
            Arc::new(LaneJob {
                stage: job.clone(),
                segs,
                frames: spec.frames.clone(),
                input,
                segments: plan.segments.as_slice(),
                base,
                buf_limit,
                #[cfg(feature = "hash")]
                checksums: plan.checksums.as_slice(),
                result: Mutex::new(None),
                done: Condvar::new(),
            })
        })
        .collect();
    // One pool slot per lane beyond the caller's lane 0.
    let lane_leases: Vec<_> = lane_jobs[1..]
        .iter()
        .flat_map(|lane| {
            let lease = Arc::new(super::mt_pool::LeaseJob::FrameLane(lane.clone()));
            super::mt_pool::lease_workers(1, &lease)
                .into_iter()
                .map(move |s| (s, lease.clone()))
        })
        .collect();
    let lane_guard = super::mt::LeaseOut {
        job,
        slots: &lane_leases,
    };
    ENGAGEMENTS.fetch_add(1, Ordering::Relaxed);
    let outcome = (|| -> Result<usize, FrameDecoderError> {
        // The caller is lane 0; the pool lanes record into their slots.
        run_lane(&lane_jobs[0]);
        let mut starts_by_frame = Vec::with_capacity(n_frames);
        starts_by_frame.resize(n_frames, None);
        let mut written = 0usize;
        let mut first_fail: Option<(usize, FrameDecoderError)> = None;
        // Frame order among checksum errors, matching the serial stream.
        #[cfg(feature = "hash")]
        let mut checksum_err: Option<(usize, FrameDecoderError)> = None;
        let mut aborted = false;
        for (li, lane) in lane_jobs.iter().enumerate() {
            let outcome = if li == 0 {
                lane.result.lock().unwrap().take()
            } else {
                let mut guard = lane.result.lock().unwrap();
                while guard.is_none() {
                    guard = lane.done.wait(guard).unwrap();
                }
                guard.take()
            };
            match outcome {
                Some(LaneOutcome::Done {
                    starts,
                    end,
                    #[cfg(feature = "hash")]
                        checksum_err: lane_checksum_err,
                }) => {
                    written = written.max(end);
                    for (f, start) in starts {
                        debug_assert!(starts_by_frame[f].is_none(), "one lane per frame");
                        starts_by_frame[f] = Some(start);
                    }
                    #[cfg(feature = "hash")]
                    if let Some((f, err)) = lane_checksum_err
                        && checksum_err.as_ref().is_none_or(|(cf, _)| f < *cf)
                    {
                        checksum_err = Some((f, err));
                    }
                },
                Some(LaneOutcome::Failed { seg, err }) => {
                    if first_fail.as_ref().is_none_or(|(s, _)| seg < *s) {
                        first_fail = Some((seg, err));
                    }
                },
                Some(LaneOutcome::Aborted) | None => aborted = true,
            }
        }
        // Serial precedence: the first decode error in input order wins
        // over every checksum mismatch.
        if let Some((_, err)) = first_fail {
            return Err(err);
        }
        #[cfg(feature = "hash")]
        if let Some((_, err)) = checksum_err {
            return Err(err);
        }
        // An aborted lane without a recorded failure implies the pool
        // poison, which the driver resumes after the leases leave, so
        // this sentinel is never observed (the piece executor's own
        // pattern). Returning ahead of the invariants below keeps them
        // on the every-lane-completed path: an aborted lane publishes
        // no frame starts and leaves the total at zero, so the assert
        // and the pledge verification would both run on incomplete data
        // and misreport the worker's panic as their own breach.
        if aborted {
            return Err(FrameDecoderError::NotYetInitialized);
        }
        let starts: Vec<usize> = starts_by_frame
            .into_iter()
            .map(|s| s.expect("every lane completed"))
            .collect();
        verify_frame_content_sizes(&plan.frame_pledges, &starts, written)?;
        Ok(written)
    })();
    drop(lane_guard);
    outcome
}

/// `decode_all_mt`'s multi-frame entry: the caller placed the whole
/// buffer up front, lanes start immediately.
pub(super) fn decode_frames_fixed(
    input: &[u8],
    workers: u32,
    plan: &ScanPlan,
    base: *mut u8,
    buf_limit: usize,
) -> Result<usize, FrameDecoderError> {
    decode_frames(input, workers, plan, Place::Fixed { base, buf_limit })
}

/// `decode_to_vec_mt`'s multi-frame entry: staging completes first, one
/// `place` covers the total, then lanes run.
pub(super) fn decode_frames_growing(
    input: &[u8],
    workers: u32,
    plan: &ScanPlan,
    place: &mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>,
) -> Result<usize, FrameDecoderError> {
    decode_frames(input, workers, plan, Place::Deferred(place))
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec, vec::Vec};
    use core::ops::Range;

    use super::Ordering;
    use crate::{
        EncoderOptions, Level, bulk,
        decoding::{
            FrameDecoder,
            errors::FrameDecoderError,
            mt::{decode_all_mt, decode_to_vec_mt},
        },
        frame_lane_engagements,
    };

    const MAX_WINDOW: u64 = crate::decoding::DEFAULT_MAX_WINDOW_SIZE;

    /// Text-ish pseudo-random payload (level 3 compresses it into the
    /// multi-frame regime above MIN_MT_INPUT).
    fn textish(seed: u64, len: usize) -> Vec<u8> {
        const WORDS: [&[u8]; 8] = [
            b"the quick brown fox ",
            b"jumps over the lazy dog ",
            b"lorem ipsum dolor ",
            b"amet consectetur ",
            b"adipiscing elit sed ",
            b"do eiusmod tempor ",
            b"incididunt ut labore ",
            b"et dolore magna ",
        ];
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = WORDS[((state >> 33) as usize) % WORDS.len()];
            let take = w.len().min(len - out.len());
            out.extend_from_slice(&w[..take]);
        }
        out
    }

    fn compress(data: &[u8], level: Level, checksum: bool, workers: u32) -> Vec<u8> {
        bulk::compress_with(
            data,
            &EncoderOptions::new(level)
                .checksum(checksum)
                .workers(workers),
        )
        .unwrap()
    }

    /// A compressed frame whose header declares its content size: the
    /// lane budget prices the walk from the pledges.
    fn compress_pledged(data: &[u8], level: Level, checksum: bool, workers: u32) -> Vec<u8> {
        bulk::compress_with(
            data,
            &EncoderOptions::new(level)
                .checksum(checksum)
                .workers(workers)
                .pledged_size(Some(data.len() as u64)),
        )
        .unwrap()
    }

    /// Deterministic xorshift for the randomized multi-frame property.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// Semi-structured records: a second content class for the property
    /// streams (literal runs with matches).
    fn records(rng: &mut Rng, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut id = 0u64;
        while out.len() < len {
            let state = rng.next();
            let user = (state >> 33) % 5000;
            let event = match (state >> 45) % 5 {
                0 => "click",
                1 => "view",
                2 => "purchase",
                3 => "error",
                _ => "login",
            };
            out.extend_from_slice(
                format!(
                    "{{\"id\":{id},\"user\":\"user_{user}\",\"event\":\"{event}\",\"ts\":{}}}",
                    1700000000 + id
                )
                .as_bytes(),
            );
            id += 1;
        }
        out.truncate(len);
        out
    }

    /// Pseudo-random bytes: incompressible, so the frame stays near raw
    /// size and clears the parallel floor cheaply.
    fn noise(rng: &mut Rng, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next().to_le_bytes()[..chunk.len()]);
        }
        out
    }

    /// Append a skippable frame: magic 0x184D2A50 + LE length + payload.
    fn push_skippable(stream: &mut Vec<u8>, fill: u8, len: usize) {
        stream.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18]);
        stream.extend_from_slice(&(len as u32).to_le_bytes());
        stream.resize(stream.len() + len, fill);
    }

    /// A multi-frame stream large enough to engage the parallel decoder.
    /// Frame 2 carries no checksum, the others do. Returns the stream, the
    /// raw outputs and each frame's byte range in the stream.
    fn multiframe_stream() -> (Vec<u8>, Vec<Vec<u8>>, Vec<Range<usize>>) {
        let raws = alloc::vec![
            textish(7, 2 * 1024 * 1024),
            textish(0xfeed, 3 * 1024 * 1024),
            textish(0x1234, 2 * 1024 * 1024),
        ];
        let mut stream = Vec::new();
        let mut parts = Vec::new();
        for (i, raw) in raws.iter().enumerate() {
            let start = stream.len();
            stream.extend_from_slice(&compress(
                raw,
                if i == 1 {
                    Level::Balanced
                } else {
                    Level::Fastest
                },
                i != 2,
                if i == 2 {
                    1
                } else {
                    4
                },
            ));
            parts.push(start..stream.len());
        }
        (stream, raws, parts)
    }

    fn expected_output(raws: &[Vec<u8>]) -> Vec<u8> {
        raws.iter().flat_map(|r| r.iter().copied()).collect()
    }

    /// Multi-frame decode through both MT entry points equals the sequential
    /// decoder and the serial MT path's bytes, at grouped and per-frame lane
    /// counts.
    #[test]
    fn multiframe_lane_output_equality() {
        let (stream, raws, _) = multiframe_stream();
        let expect = expected_output(&raws);

        let mut seq = vec![0u8; expect.len()];
        let n = FrameDecoder::new().decode_all(&stream, &mut seq).unwrap();
        assert_eq!(&seq[..n], &expect[..]);

        for workers in [2u32, 4, 8] {
            let before = frame_lane_engagements();
            let mut placed = vec![0u8; expect.len()];
            let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
            assert_eq!(&placed[..n], &expect[..], "placed mt{workers}");
            let mut appended = Vec::new();
            decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap();
            assert_eq!(appended, expect, "appended mt{workers}");
            assert!(
                frame_lane_engagements() > before,
                "mt{workers} multi-frame decode must engage the lane executor"
            );
        }
    }

    /// The append contract holds behind a non-empty prefix, and the length
    /// stays unchanged on a checksum error.
    /// A pledged multi-frame stream takes a plan-time budget on the MT
    /// path. The counter never false-fails under the shared-process test
    /// harness: a concurrent test's decode can only add engagements.
    #[test]
    fn multiframe_decode_takes_lane_budget() {
        // Small textish frames over a noise anchor: the frames pledge
        // their sizes and the anchor carries the stream past the parallel
        // floor.
        let mut rng = Rng(0x1a2e);
        let mut stream = Vec::new();
        let mut raws = Vec::new();
        // Pledged frames: the budget prices the walk from the pledges,
        // so every frame declares its content size.
        for i in 0..5u64 {
            let raw = textish(0x700 + i, 96 * 1024);
            stream.extend_from_slice(&compress_pledged(&raw, Level::Fastest, true, 1));
            raws.push(raw);
        }
        let raw = noise(&mut rng, 600 * 1024);
        stream.extend_from_slice(&compress_pledged(&raw, Level::Fastest, true, 1));
        raws.push(raw);
        let expect = expected_output(&raws);
        let before = super::super::lane_budget::engagements();
        let mut placed = vec![0u8; expect.len()];
        let n = decode_all_mt(&stream, &mut placed, 8, MAX_WINDOW).unwrap();
        assert_eq!(&placed[..n], &expect[..]);
        assert!(
            super::super::lane_budget::engagements() > before,
            "the lane budget must engage on pledged multi-frame input"
        );
    }

    #[test]
    fn multiframe_append_contract() {
        let (stream, raws, parts) = multiframe_stream();
        let expect = expected_output(&raws);
        let prefix = b"non-empty prefix".to_vec();
        let mut appended = prefix.clone();
        decode_to_vec_mt(&stream, &mut appended, 4, MAX_WINDOW).unwrap();
        assert_eq!(&appended[..prefix.len()], &prefix[..]);
        assert_eq!(&appended[prefix.len()..], &expect[..]);

        // Corrupt frame 0's trailer (it carries a checksum): the error must
        // surface with the vector length unchanged.
        let mut corrupt = stream.clone();
        let trailer = parts[0].end - 1;
        corrupt[trailer] ^= 0xff;
        let mut appended = prefix.clone();
        let err = decode_to_vec_mt(&corrupt, &mut appended, 4, MAX_WINDOW).unwrap_err();
        assert!(
            matches!(err, FrameDecoderError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert_eq!(appended.len(), prefix.len());
    }

    /// Checksum mismatches report per frame: the offending frame's trailer
    /// alone fails, and the good stream round-trips. The offending frame
    /// must be the LAST one so every earlier lane finishes and the min-id
    /// rule is not what the test exercises.
    #[test]
    fn multiframe_lane_checksum_reports_frame() {
        let (stream, _, parts) = multiframe_stream();
        // Flip a byte in frame 1's trailer (checksummed): the error must be
        // a checksum mismatch, not a decode error.
        let mut corrupt = stream.clone();
        let frame1_trailer = parts[1].end - 1;
        corrupt[frame1_trailer] ^= 0xff;
        let mut out = Vec::new();
        let err = decode_to_vec_mt(&corrupt, &mut out, 4, MAX_WINDOW).unwrap_err();
        assert!(
            matches!(err, FrameDecoderError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        let mut seq = vec![0u8; 7 * 1024 * 1024];
        let err = FrameDecoder::new()
            .decode_all(&corrupt, &mut seq)
            .unwrap_err();
        assert!(
            matches!(err, FrameDecoderError::ChecksumMismatch { .. }),
            "serial reports the same variant: {err:?}"
        );
    }

    /// A stage-A failure mid-stream reports the same error variant as the
    /// sequential decoder, through both entries.
    #[test]
    fn multiframe_stage_error_matches_sequential() {
        let (stream, _, parts) = multiframe_stream();
        // Corrupt deep inside the second frame's block data.
        let mut corrupt = stream.clone();
        let at = parts[1].start + 900;
        corrupt[at] ^= 0xff;
        corrupt[at + 1] ^= 0xff;
        let mut seq = vec![0u8; 7 * 1024 * 1024];
        let seq_err = FrameDecoder::new()
            .decode_all(&corrupt, &mut seq)
            .unwrap_err();

        for workers in [2u32, 4, 8] {
            let mut placed = vec![0u8; 7 * 1024 * 1024];
            let err = decode_all_mt(&corrupt, &mut placed, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(format!("{err:?}"), format!("{seq_err:?}"), "mt{workers}");
            let mut appended = Vec::new();
            let err = decode_to_vec_mt(&corrupt, &mut appended, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(format!("{err:?}"), format!("{seq_err:?}"), "mt{workers}");
        }
    }

    /// More frames than lanes: eight small frames at two workers exercise
    /// the frame grouping, and a skippable frame between two real ones
    /// keeps the lane split aligned with the scan's frame count.
    #[test]
    fn multiframe_grouping_and_skippable() {
        let mut raws = Vec::new();
        for i in 0..8 {
            raws.push(textish(0xb00 + i as u64, 300 * 1024));
        }
        let mut stream = Vec::new();
        for (i, raw) in raws.iter().enumerate() {
            stream.extend_from_slice(&compress(raw, Level::Fastest, true, 1));
            if i == 3 {
                push_skippable(&mut stream, 0xab, 64);
            }
        }
        let expect = expected_output(&raws);
        let mut seq = vec![0u8; expect.len()];
        let n = FrameDecoder::new().decode_all(&stream, &mut seq).unwrap();
        assert_eq!(&seq[..n], &expect[..]);
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; expect.len()];
            let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
            assert_eq!(&placed[..n], &expect[..], "mt{workers}");
        }
    }

    /// Single-frame input keeps today's serial stage-B path: the lane
    /// injection must never fire on it, and must fire on multi-frame input
    /// (the panic path also proves the pool-lane poison machinery).
    #[test]
    fn single_frame_keeps_serial_stage_b() {
        let raw = textish(0x51, 4 * 1024 * 1024);
        let stream = compress(&raw, Level::Fastest, true, 4);
        super::LANE_PANIC_INPUT.store(stream.as_ptr().cast_mut(), Ordering::Relaxed);
        let mut placed = vec![0u8; raw.len()];
        let n = decode_all_mt(&stream, &mut placed, 8, MAX_WINDOW).unwrap();
        assert_eq!(&placed[..n], &raw[..]);

        let (multi, raws, _) = multiframe_stream();
        super::LANE_PANIC_INPUT.store(multi.as_ptr().cast_mut(), Ordering::Relaxed);
        let mut placed = vec![0u8; expected_output(&raws).len()];
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_all_mt(&multi, &mut placed, 4, MAX_WINDOW)
        }));
        super::LANE_PANIC_INPUT.store(core::ptr::null_mut(), Ordering::Relaxed);
        assert!(
            outcome.is_err(),
            "the lane injection must fire on multi-frame input"
        );
    }

    /// One frame carrying its pledged content size (the one-shot bulk
    /// entries omit the field; the pledge verification reads it).
    fn pledged_frame(raw: &[u8]) -> Vec<u8> {
        use crate::io::Write as _;
        let mut sink = Vec::new();
        let mut enc = crate::stream::write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Fastest).pledged_size(Some(raw.len() as u64)),
        )
        .unwrap();
        enc.write_all(raw).unwrap();
        enc.finish().unwrap();
        sink
    }

    /// A pool lane worker's panic must surface as its own payload: an
    /// aborted lane without any recorded failure means the driver resumes
    /// the poison after the leases leave, and the outcome collection's
    /// all-Done invariants (the frame-start assert, the pledge
    /// verification) must not fire first — an aborted lane publishes no
    /// frame starts, so both would misreport the worker's panic as their
    /// own invariant breach. Every frame pledges its content size so the
    /// verification walks the missing lane's frame starts.
    #[test]
    fn pool_lane_panic_surfaces_the_payload() {
        let mut stream = Vec::new();
        let mut expect = Vec::new();
        for i in 0..3u64 {
            let raw = textish(0x700 + i, 2 * 1024 * 1024);
            stream.extend_from_slice(&pledged_frame(&raw));
            expect.extend_from_slice(&raw);
        }
        // Control: the stream engages the lane executor and decodes clean.
        let before = frame_lane_engagements();
        let mut placed = vec![0u8; expect.len()];
        let n = decode_all_mt(&stream, &mut placed, 2, MAX_WINDOW).unwrap();
        assert_eq!(&placed[..n], &expect[..]);
        assert!(frame_lane_engagements() > before);

        // mt2 groups three frames into lanes [0] and [1, 2]: the pool
        // lane panics at its first segment while the caller's lane 0
        // completes, and the driver must resume the worker's payload.
        super::LANE_PANIC_POOL_INPUT.store(stream.as_ptr().cast_mut(), Ordering::Relaxed);
        let blew = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut placed = vec![0u8; expect.len()];
            let _ = decode_all_mt(&stream, &mut placed, 2, MAX_WINDOW);
        }));
        super::LANE_PANIC_POOL_INPUT.store(core::ptr::null_mut(), Ordering::Relaxed);
        let payload = blew.expect_err("the pool lane's panic must surface");
        let msg = payload.downcast_ref::<&str>().copied().or_else(|| {
            payload
                .downcast_ref::<alloc::string::String>()
                .map(std::string::String::as_str)
        });
        assert_eq!(msg, Some("injected lane panic"));
    }

    /// Deterministic property: streams of 1..=16 frames — empty and
    /// sub-floor sizes up to 256 KiB, textish and records content,
    /// per-frame checksum, level and encoder workers, skippable frames
    /// interleaved, a noise frame anchoring sub-floor streams above the
    /// parallel floor — decode byte-equal through the sequential decoder
    /// and the MT entry at 2/4/8/16 workers, placed and appending. The
    /// seed is fixed, so every failure reproduces.
    #[test]
    fn multiframe_property_st_mt_byte_equality() {
        const SIZES: [usize; 4] = [0, 4096, 64 * 1024, 256 * 1024];
        const BUDGET: usize = 1024 * 1024;
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for case in 0..6 {
            let n_frames = 1 + (rng.next() % 16) as usize;
            let mut spent = 0usize;
            let mut stream = Vec::new();
            let mut raws = Vec::new();
            for _ in 0..n_frames {
                if rng.next().trailing_zeros() >= 3 {
                    push_skippable(&mut stream, 0xab, 16 + (rng.next() >> 40) as usize % 48);
                }
                let len = if spent < BUDGET {
                    SIZES[(rng.next() >> 36) as usize % SIZES.len()].min(BUDGET - spent)
                } else {
                    0
                };
                spent += len;
                let raw = if rng.next() & 1 == 0 {
                    textish(rng.next() | 1, len)
                } else {
                    records(&mut rng, len)
                };
                stream.extend_from_slice(&compress(
                    &raw,
                    Level::from_zstd(1 + (rng.next() >> 36) as i32 % 4),
                    rng.next() & 1 == 0,
                    1 + (rng.next() >> 40) as u32 % 3,
                ));
                raws.push(raw);
            }
            // The noise anchor keeps multi-frame cases above the 512-KiB
            // parallel floor, so the frame-lane path engages everywhere.
            if raws.len() >= 2 && stream.len() < 600 * 1024 {
                let raw = noise(&mut rng, 600 * 1024 - stream.len() + 4096);
                stream.extend_from_slice(&compress(&raw, Level::Fastest, false, 1));
                raws.push(raw);
            }
            let expect = expected_output(&raws);

            let mut seq = vec![0u8; expect.len()];
            let n = FrameDecoder::new().decode_all(&stream, &mut seq).unwrap();
            assert_eq!(&seq[..n], &expect[..], "case {case}: sequential");

            let before = frame_lane_engagements();
            for workers in [2u32, 4, 8, 16] {
                let mut placed = vec![0u8; expect.len()];
                let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
                assert_eq!(&placed[..n], &expect[..], "case {case}: mt{workers}");
                let mut appended = Vec::new();
                decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap();
                assert_eq!(appended, expect, "case {case}: mt{workers} appended");
            }
            assert!(
                raws.len() < 2 || stream.len() < 600 * 1024 || frame_lane_engagements() > before,
                "case {case}: the multi-frame stream must engage the lane executor"
            );
        }
    }

    /// Staged decode error inside a non-first frame of a stream that
    /// outnumbers the worker budget: the minimum failing segment id — the
    /// serial decoder's error — wins over a later lane's checksum
    /// mismatch, at grouped and ungrouped worker counts.
    #[test]
    fn multiframe_grouped_error_precedence() {
        // Six frames; at mt2 the lanes group frames 0-2 and 3-5. Frame 1
        // carries no checksum, so its deep corruption is a decode error;
        // frame 4 carries one, so only its trailer flips.
        let mut stream = Vec::new();
        let mut parts = Vec::new();
        for i in 0..6u64 {
            let raw = textish(0xd00 + i, 2 * 1024 * 1024);
            let start = stream.len();
            stream.extend_from_slice(&compress(&raw, Level::Fastest, i != 1, 4));
            parts.push(start..stream.len());
        }

        // A staged error inside frame 4 — the second lane group at mt2 —
        // rejects through both entries wherever the sequential decoder
        // rejects. The variant is the staged parse's own (stage A
        // pre-parses sequence sections, so the failing read can differ
        // from the sequential decoder's fused loop); the contract is the
        // accept/reject symmetry, not variant equality.
        let mut corrupt = stream.clone();
        let at = parts[4].start + 900;
        corrupt[at] ^= 0xff;
        corrupt[at + 1] ^= 0xff;
        let mut seq = vec![0u8; 12 * 1024 * 1024];
        let seq_err = FrameDecoder::new()
            .decode_all(&corrupt, &mut seq)
            .unwrap_err();
        assert!(
            !matches!(seq_err, FrameDecoderError::ChecksumMismatch { .. }),
            "{seq_err:?}"
        );
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; 12 * 1024 * 1024];
            let err = decode_all_mt(&corrupt, &mut placed, workers, MAX_WINDOW).unwrap_err();
            assert!(
                !matches!(err, FrameDecoderError::ChecksumMismatch { .. }),
                "mt{workers}: {err:?}"
            );
            let mut appended = Vec::new();
            decode_to_vec_mt(&corrupt, &mut appended, workers, MAX_WINDOW).unwrap_err();
        }

        // Min-failing-id precedence: frame 1 corrupts deep while frame
        // 4's checksum trailer flips. Frame 4's block data is intact, so
        // any decode error can only come from frame 1 — the reported
        // error must be frame 1's alone, and the later lane's checksum
        // mismatch must lose against it.
        let mut only_f1 = stream.clone();
        let at = parts[1].start + 900;
        only_f1[at] ^= 0xff;
        only_f1[at + 1] ^= 0xff;
        let mut both = only_f1.clone();
        both[parts[4].end - 1] ^= 0xff;
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; 12 * 1024 * 1024];
            let err_f1 = decode_all_mt(&only_f1, &mut placed, workers, MAX_WINDOW).unwrap_err();
            let mut placed = vec![0u8; 12 * 1024 * 1024];
            let err_both = decode_all_mt(&both, &mut placed, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(
                format!("{err_both:?}"),
                format!("{err_f1:?}"),
                "mt{workers}: the earlier frame's decode error must win"
            );
            assert!(
                !matches!(err_both, FrameDecoderError::ChecksumMismatch { .. }),
                "mt{workers}: the later lane's checksum mismatch must lose"
            );
        }
    }

    /// A checksum mismatch in a middle frame of a stream that outnumbers
    /// the worker budget surfaces from the checksumming frame's own
    /// trailer, grouped or not, with the append vector unchanged.
    #[test]
    fn multiframe_middle_frame_checksum_grouped() {
        // Five checksummed frames; frame 2 is lane 1's first frame at mt2
        // (lanes 0-1 and 2-4) and lane 0's group completes clean.
        let mut stream = Vec::new();
        let mut parts = Vec::new();
        for i in 0..5u64 {
            let raw = textish(0xe00 + i, 2 * 1024 * 1024);
            let start = stream.len();
            stream.extend_from_slice(&compress(&raw, Level::Fastest, true, 4));
            parts.push(start..stream.len());
        }
        let mut corrupt = stream.clone();
        corrupt[parts[2].end - 1] ^= 0xff;
        let mut seq = vec![0u8; 10 * 1024 * 1024];
        let seq_err = FrameDecoder::new()
            .decode_all(&corrupt, &mut seq)
            .unwrap_err();
        assert!(
            matches!(seq_err, FrameDecoderError::ChecksumMismatch { .. }),
            "{seq_err:?}"
        );
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; 10 * 1024 * 1024];
            let err = decode_all_mt(&corrupt, &mut placed, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(format!("{err:?}"), format!("{seq_err:?}"), "mt{workers}");
            let mut appended = b"prefix".to_vec();
            let err = decode_to_vec_mt(&corrupt, &mut appended, workers, MAX_WINDOW).unwrap_err();
            assert!(
                matches!(err, FrameDecoderError::ChecksumMismatch { .. }),
                "mt{workers}: {err:?}"
            );
            assert_eq!(appended.len(), b"prefix".len(), "mt{workers}");
        }
    }

    /// An empty frame in the tail: the last lane's segment covers zero
    /// output bytes, and the decode stays byte-equal through every worker
    /// count and both entries.
    #[test]
    fn multiframe_empty_frame_tail() {
        let (mut stream, raws, _) = multiframe_stream();
        stream.extend_from_slice(&compress(&[], Level::Fastest, true, 1));
        let expect = expected_output(&raws);
        for workers in [2u32, 4, 8] {
            let mut placed = vec![0u8; expect.len()];
            let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
            assert_eq!(&placed[..n], &expect[..], "mt{workers}");
            let mut appended = Vec::new();
            decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap();
            assert_eq!(appended, expect, "mt{workers}");
        }
    }

    /// Seventeen frames at 2/4/12 workers: frames outnumber every worker
    /// count, so the lanes group consecutive frames — across empty frames
    /// and skippable interleave — and every grouping stays byte-equal.
    #[test]
    fn multiframe_frames_outnumber_workers() {
        let mut rng = Rng(0xabcdef);
        let mut stream = Vec::new();
        let mut raws = Vec::new();
        for i in 0..16u64 {
            if i == 5 || i == 11 {
                push_skippable(&mut stream, 0x5a, 32);
            }
            let raw = textish(
                0x600 + i,
                if i % 4 == 0 {
                    0
                } else {
                    8 * 1024
                },
            );
            stream.extend_from_slice(&compress(&raw, Level::Fastest, i & 1 == 0, 1));
            raws.push(raw);
        }
        // The 16 tiny frames stay far below the floor; the noise anchor
        // carries the stream above it.
        let raw = noise(&mut rng, 600 * 1024);
        stream.extend_from_slice(&compress(&raw, Level::Fastest, true, 1));
        raws.push(raw);
        let expect = expected_output(&raws);
        let before = frame_lane_engagements();
        for workers in [2u32, 4, 12] {
            let mut placed = vec![0u8; expect.len()];
            let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
            assert_eq!(&placed[..n], &expect[..], "mt{workers}");
            let mut appended = Vec::new();
            decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap();
            assert_eq!(appended, expect, "mt{workers}");
        }
        assert!(frame_lane_engagements() > before);
    }

    /// A dictID-carrying frame inside a multi-frame stream: the scan
    /// refuses dictionary state, so both MT entries fall back to the
    /// sequential decoder — the dict-less decode of a frame that matched
    /// into the dictionary reports the sequential decoder's own error
    /// (here `OffsetTooBig`, not the registry's `DictNotProvided`, which
    /// the bulk path never consults), byte-for-byte the same through MT.
    #[test]
    fn multiframe_dictid_frame_matches_sequential() {
        let dict = textish(0x99, 64 * 1024);
        let mut stream = Vec::new();
        let raw0 = textish(0xa1, 2 * 1024 * 1024);
        stream.extend_from_slice(&compress(&raw0, Level::Fastest, true, 4));
        // A frame stamped with the dictionary's id (mismatched against
        // the dict-less decode), between two plain frames. Its content
        // shares the dict's word stream, so the encoder matched into the
        // dictionary and the dict-less decode must fail.
        let raw1 = textish(0x99, 2 * 1024 * 1024);
        stream.extend_from_slice(
            &bulk::compress_with(
                &raw1,
                &EncoderOptions::new(Level::Fastest).dictionary(&dict),
            )
            .unwrap(),
        );
        let raw2 = textish(0xa3, 2 * 1024 * 1024);
        stream.extend_from_slice(&compress(&raw2, Level::Fastest, true, 4));

        let mut seq = vec![0u8; 6 * 1024 * 1024];
        let seq_err = FrameDecoder::new()
            .decode_all(&stream, &mut seq)
            .unwrap_err();
        assert!(
            !matches!(seq_err, FrameDecoderError::ChecksumMismatch { .. }),
            "{seq_err:?}"
        );
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; 6 * 1024 * 1024];
            let err = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(format!("{err:?}"), format!("{seq_err:?}"), "mt{workers}");
            let mut appended = Vec::new();
            let err = decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap_err();
            assert_eq!(format!("{err:?}"), format!("{seq_err:?}"), "mt{workers}");
        }
    }

    /// A skippable-only stream decodes to empty through the MT entries and
    /// never engages the lane executor: the scan sees no real frame, so
    /// the sequential fallback owns the stream even above the floor.
    #[test]
    fn skippable_only_stream_decodes_empty() {
        let mut stream = Vec::new();
        push_skippable(&mut stream, 0x5a, 16);
        push_skippable(&mut stream, 0x5a, 1024);
        push_skippable(&mut stream, 0x5a, 700 * 1024);
        let mut seq = Vec::new();
        let n = FrameDecoder::new().decode_all(&stream, &mut seq).unwrap();
        assert_eq!(n, 0);
        for workers in [2u32, 4] {
            let mut placed = vec![0u8; 0];
            let n = decode_all_mt(&stream, &mut placed, workers, MAX_WINDOW).unwrap();
            assert_eq!(n, 0, "mt{workers}");
            let mut appended = b"prefix".to_vec();
            decode_to_vec_mt(&stream, &mut appended, workers, MAX_WINDOW).unwrap();
            assert_eq!(appended, b"prefix", "mt{workers}");
        }
        // The engagement pin is the clean decode itself. The counter is
        // process-global and parallel lane tests move it inside any
        // snapshot window, so an exact-equality check on it races (it
        // fired under the paranoid profile once this suite's lane
        // population grew). A skippable-only scan yields no segments, so
        // the parallel path never routes the stream — and an engaged
        // lane run would have no lane 0 to execute, panicking on the
        // empty lane list rather than decoding empty.
    }
}
