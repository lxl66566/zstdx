//! Plan-time lane budget for the frame-lane dispatcher
//! ([`super::mt_frames`]): how many stage-B lanes to run, how to pack
//! frames into them, and whether stage A may stage ahead of execution.
//!
//! The stock budget hands stage B `min(workers, frames)` lanes, so a
//! payload with more frames than workers leaves stage A a single staging
//! worker that owns the wall. The budget walks the lane count down from
//! the stock value, converting lanes into staging workers, and stops at
//! the first lane count whose execute critical path no longer hides
//! under the staging span: cutting further would make the packed
//! execute path the new wall.
//!
//! All prices are plan-time arithmetic over the pre-scan's per-frame
//! pledged output sizes and staged volumes; no decode-path state carries
//! them. A payload whose frames lack pledges keeps the stock budget, and
//! the walk never drops below two lanes.

use alloc::vec::Vec;

/// One lane's frame assignment: scan-order frame indices, ascending within
/// the lane. Lane 0 (the caller's lane) carries the head frame after
/// packing.
#[derive(Clone)]
pub(super) struct LaneSpec {
    pub frames: Vec<usize>,
}

/// Per-frame pricing input: pledged output bytes and staged volume.
/// A frame whose block headers failed to parse contributes no staged
/// volume (the totals under-count), the same convention the head-ramp
/// gate uses.
pub(super) struct FramePlan {
    pub out: u64,
    pub staged: u64,
}

/// The decided budget.
pub(super) struct LaneBudget {
    /// Stage-B lane count.
    pub lanes: usize,
    /// LPT packing: the heaviest frame into the lightest lane.
    pub lpt: bool,
    /// Stage A may stage ahead of execution without the per-worker
    /// backpressure window (bounded by the plan's total staged volume).
    pub free_window: bool,
}

/// Staged volume one staging worker sustains, in staged KiB per
/// millisecond (probe 2026-11-05: 2.0 to 2.6 across the multi payloads).
const STAGE_RATE_KIB_MS: u128 = 2200;
/// Aggregate staging throughput cap, staged KiB per millisecond
/// (probe 2026-11-05: saturation near 13 KiB/us).
const STAGE_RATE_MAX_KIB_MS: u128 = 13000;
/// Exec rate over staged volume, staged KiB per millisecond: a frame's
/// execute time is the larger of its staged volume at this rate and its
/// output at `EXEC_OUT_KIB_MS`.
const EXEC_STAGED_KIB_MS: u128 = 3500;
/// Exec rate over output bytes, output KiB per millisecond.
const EXEC_OUT_KIB_MS: u128 = 7000;
/// A frame is dominant when its pledged output is at least this share of
/// the payload's total; dominant payloads take LPT packing.
const DOMINANCE_SHARE: u128 = 35;
/// Free-window volume bound: staging ahead is bounded by the plan's total
/// staged volume, and this is the largest total the budget lets stage
/// freely (half the staging pool's retention cap).
const FREE_WINDOW_MAX_BYTES: u128 = 128 * 1024 * 1024;

/// Consecutive grouping: `n_frames` frames split into `lanes` equal-count
/// runs in scan order.
pub(super) fn group_consecutive(n_frames: usize, lanes: usize) -> Vec<LaneSpec> {
    (0..lanes)
        .map(|li| {
            let first = li * n_frames / lanes;
            let last = (li + 1) * n_frames / lanes;
            LaneSpec {
                frames: (first..last).collect(),
            }
        })
        .collect()
}

/// LPT packing over per-frame weights: the heaviest frame lands in the
/// lightest lane, ties resolved by frame index then lane index, so the
/// schedule is a pure function of the weight list. Every lane keeps its
/// frames in scan order, and lane 0 carries the head frame.
pub(super) fn group_lpt(weights: &[u64], lanes: usize) -> Vec<LaneSpec> {
    debug_assert!(lanes >= 1);
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| weights[b].cmp(&weights[a]).then(a.cmp(&b)));
    let mut sums = alloc::vec![0u128; lanes];
    let mut groups: Vec<Vec<usize>> = alloc::vec![Vec::new(); lanes];
    for f in order {
        let li = sums
            .iter()
            .enumerate()
            .min_by_key(|(i, s)| (*s, *i))
            .map(|(i, _)| i)
            .unwrap();
        sums[li] = sums[li].saturating_add(weights[f] as u128);
        groups[li].push(f);
    }
    let mut specs: Vec<LaneSpec> = groups
        .into_iter()
        .map(|mut g| {
            g.sort_unstable();
            LaneSpec { frames: g }
        })
        .collect();
    // Lane 0 is the caller's lane: it carries the stream's head frame.
    specs.sort_by_key(|s| s.frames.first().copied().unwrap_or(usize::MAX));
    specs
}

/// The staging span at `stage_workers` staging workers, in nanoseconds.
fn staging_ns(staged_total: u64, stage_workers: usize) -> u128 {
    let rate = (stage_workers as u128 * STAGE_RATE_KIB_MS).min(STAGE_RATE_MAX_KIB_MS);
    (staged_total as u128 / 1024) * 1_000_000 / rate
}

/// One frame's execute span, in nanoseconds: the larger of its staged
/// volume at the staged rate and its output at the copy rate.
fn exec_frame_ns(plan: &FramePlan) -> u128 {
    let staged = (plan.staged as u128 / 1024) * 1_000_000 / EXEC_STAGED_KIB_MS;
    let out = (plan.out as u128 / 1024) * 1_000_000 / EXEC_OUT_KIB_MS;
    staged.max(out)
}

/// The packed execute critical path at `lanes` lanes, in nanoseconds: the
/// largest lane's summed frame spans.
fn exec_ns(plans: &[FramePlan], lanes: usize, lpt: bool) -> u128 {
    let weights: Vec<u64> = plans.iter().map(|p| p.out).collect();
    let groups = if lpt {
        group_lpt(&weights, lanes)
    } else {
        group_consecutive(plans.len(), lanes)
    };
    groups
        .iter()
        .map(|g| g.frames.iter().map(|&f| exec_frame_ns(&plans[f])).sum())
        .max()
        .unwrap_or(0)
}

/// How many lane-mode decodes took a budget so far (process lifetime,
/// test-only observation of the wiring).
#[cfg(test)]
static ENGAGED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
#[doc(hidden)]
pub fn engagements() -> usize {
    ENGAGED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Decide the lane budget. `None` keeps the stock layout: `min(workers,
/// frames)` consecutive lanes and the per-worker staging window.
pub(super) fn plan(plans: &[FramePlan], workers: u32) -> Option<LaneBudget> {
    let worker_count = workers as usize;
    if worker_count < 2 || plans.len() < 3 {
        return None;
    }
    let total_out: u128 = plans.iter().map(|p| p.out as u128).sum();
    if total_out == 0 {
        return None;
    }
    let staged_total: u128 = plans.iter().map(|p| p.staged as u128).sum();
    let dominant =
        plans.iter().map(|p| p.out as u128).max().unwrap() * 100 >= DOMINANCE_SHARE * total_out;
    let stage_ns = |lanes: usize| staging_ns(staged_total as u64, worker_count - lanes + 1);
    let exec_at = |lanes: usize| exec_ns(plans, lanes, dominant);
    // Walk the lane count down from the stock value, converting lanes
    // into staging workers, and stop at the first count whose execute
    // critical path no longer hides under the staging span.
    let mut lanes = worker_count.min(plans.len());
    while lanes > 2 {
        let cut = lanes - 1;
        if stage_ns(cut) >= stage_ns(lanes) {
            // The aggregate staging cap binds: no staging worker gained.
            break;
        }
        if exec_at(cut) >= stage_ns(cut) {
            // The packed execute path would own the wall at `cut`.
            break;
        }
        lanes = cut;
    }
    // Pack LPT when the execute path owns the wall: a staging-bound wall
    // hides the packing, and consecutive lanes keep input-order locality.
    let lpt = dominant && exec_at(lanes) >= stage_ns(lanes);
    #[cfg(test)]
    ENGAGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Some(LaneBudget {
        lanes,
        lpt,
        free_window: staged_total <= FREE_WINDOW_MAX_BYTES,
    })
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    /// Payload shapes from the 2026-11-05 probe, (out KiB, staged KiB)
    /// per frame, with the lane counts the walk picked there.
    #[test]
    fn walk_matches_probed_payloads() {
        let multi = [
            (4096, 3267),
            (2048, 394),
            (3072, 4083),
            (16384, 551),
            (3072, 2421),
            (4096, 5423),
        ];
        let balanced = [
            (5632, 394),
            (5632, 4502),
            (5632, 7504),
            (5632, 395),
            (5632, 4464),
            (5632, 7496),
        ];
        let twodom = [
            (12288, 553),
            (12288, 9826),
            (64, 67),
            (96, 0),
            (128, 95),
            (160, 382),
            (192, 201),
            (224, 0),
            (256, 231),
            (320, 296),
        ];
        let small = [
            (64u64, 47u64),
            (80, 76),
            (88, 88),
            (96, 0),
            (112, 82),
            (128, 129),
            (144, 339),
            (160, 0),
            (176, 157),
            (192, 194),
            (208, 509),
            (224, 0),
            (240, 215),
            (256, 243),
            (288, 353),
            (320, 0),
            (352, 265),
            (384, 347),
            (416, 489),
            (448, 0),
            (480, 364),
            (496, 395),
            (512, 659),
            (512, 0),
        ];
        let mk = |v: &[(u64, u64)]| -> Vec<FramePlan> {
            v.iter()
                .map(|&(o, s)| FramePlan {
                    out: o * 1024,
                    staged: s * 1024,
                })
                .collect()
        };
        // (workers, lanes, lpt) per payload, the probe's picks.
        let expect: [(&[(u64, u64)], &[(u32, usize, bool)]); 4] = [
            (multi.as_slice(), &[
                (4, 3, false),
                (8, 6, false),
                (16, 6, true),
            ]),
            (balanced.as_slice(), &[
                (4, 3, false),
                (8, 6, false),
                (16, 6, false),
            ]),
            (twodom.as_slice(), &[
                (4, 4, false),
                (8, 8, false),
                (16, 10, true),
            ]),
            (small.as_slice(), &[
                (4, 3, false),
                (8, 5, false),
                (16, 11, false),
            ]),
        ];
        for (shape, cells) in expect {
            let plans = mk(shape);
            for &(workers, lanes, lpt) in cells {
                let budget = plan(&plans, workers).expect("budget exists");
                assert_eq!(budget.lanes, lanes, "lanes at {workers} workers");
                assert_eq!(budget.lpt, lpt, "packing at {workers} workers");
                assert!(budget.free_window, "staged volume under the bound");
            }
        }
    }

    #[test]
    fn keeps_stock_shape_on_missing_budget() {
        // Fewer than three frames: no lane trade to make.
        let plans = vec![
            FramePlan {
                out: 1 << 20,
                staged: 1 << 20,
            },
            FramePlan {
                out: 1 << 20,
                staged: 1 << 20,
            },
        ];
        assert!(plan(&plans, 8).is_none());
        // One worker: lane mode needs both a caller lane and staging.
        assert!(plan(&plans, 1).is_none());
        // Zero output: nothing to price.
        let zero = vec![
            FramePlan { out: 0, staged: 0 },
            FramePlan { out: 0, staged: 0 },
            FramePlan { out: 0, staged: 0 },
        ];
        assert!(plan(&zero, 4).is_none());
    }

    #[test]
    fn consecutive_splits_equal_counts() {
        let specs = group_consecutive(10, 4);
        let got: Vec<Vec<usize>> = specs.iter().map(|s| s.frames.clone()).collect();
        assert_eq!(got, vec![vec![0, 1], vec![2, 3, 4], vec![5, 6], vec![
            7, 8, 9
        ]]);
        assert!(group_consecutive(5, 5).iter().all(|s| s.frames.len() == 1));
        assert_eq!(group_consecutive(3, 1)[0].frames, [0, 1, 2]);
    }

    #[test]
    fn lpt_balances_and_sorts_lanes() {
        let weights = [24u64, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        let specs = group_lpt(&weights, 2);
        // The dominant frame rides alone; the nine tiny frames share the
        // other lane; lane 0 still carries frame 0.
        assert_eq!(specs[0].frames, [0]);
        assert_eq!(specs[1].frames.len(), 9);
        // Determinism: repeated calls agree.
        assert_eq!(group_lpt(&weights, 2)[1].frames, specs[1].frames.clone());
        // Equal weights fill lanes in index order: the lightest lane is
        // always the lowest-indexed empty one.
        let specs = group_lpt(&[5u64, 5, 5, 5], 2);
        assert_eq!(specs[0].frames, [0, 2]);
        assert_eq!(specs[1].frames, [1, 3]);
    }
}
