//! Bounded cell-level parallelism for the single-threaded bench sections.
//!
//! A section's cells are independent single-thread workloads, so running
//! several at once trades some cross-cell interference for wall time. Each
//! cell keeps its interleaved A/B on one pool thread, so machine drift and
//! co-tenant load still hit both sides of a cell equally and the per-round
//! ratio verdict is unchanged; only the variance of that verdict grows
//! with the interference. Sections whose cells spawn worker pools
//! internally (the mt sections) or whose measurement depends on allocator
//! state (`small`) stay serial.
//!
//! Two bounds keep the pool inside the machine: a maximum width from
//! `--parallel`, and a total peak-memory estimate. The width for a batch
//! of cells is the cap limited by the memory budget over the per-cell
//! estimate, a flat multiple of the payload plus a fixed floor, because
//! deep-tier table allocations scale with the window instead of the input
//! and both sides of a cell are never alive at once. A batch may mix
//! payload sizes; the width then follows the largest payload's estimate.

use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

/// Default maximum number of timing cells running at once. The 2026-11-01
/// calibration on enc-st json: width 6 inflated json.balanced x by 9
/// percent and width 3 read json.ultra at x1.65 against 1.03 serial, while
/// width 2 held every cell within 5 percent of its serial x, so 2 is the
/// default ceiling for sections whose x is the verdict. Wider widths stay
/// available for smoke passes.
pub const DEFAULT_TIMING_WIDTH: usize = 2;

/// Default maximum number of cells running at once for sections whose
/// output carries no timing verdict (dump frames).
pub const DEFAULT_MAX_WIDTH: usize = 6;

/// Default total peak-memory estimate the pool schedules against.
const MEM_BUDGET: u64 = 4 << 30;

/// Fixed floor of the per-cell estimate: table and output allocations that
/// do not scale with the payload.
const EST_FLOOR: u64 = 64 << 20;

/// Estimate multiplier over the payload: both sides' encoder tables and
/// outputs.
const EST_MULTIPLE: u64 = 6;

/// Peak-memory estimate for one cell over a payload of `raw_len` bytes.
pub fn cell_est(raw_len: usize) -> u64 {
    EST_MULTIPLE * raw_len as u64 + EST_FLOOR
}

/// Width for a batch of cells with payloads of `raw_len` bytes: the
/// `--parallel` cap (0 = one per logical core) against the memory budget.
fn batch_width(max_width: usize, raw_len: usize) -> usize {
    let cap = if max_width == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    } else {
        max_width
    };
    let by_mem = (MEM_BUDGET / cell_est(raw_len)).max(1) as usize;
    cap.min(by_mem).max(1)
}

/// Run cells on a bounded pool and return their outputs in input order.
///
/// Each cell is `(progress label, payload bytes for the memory estimate,
/// body)`; the label names the stderr progress line printed when the cell
/// finishes. Cell output is returned in section order regardless of
/// completion order, so stdout stays diffable. A width of 1 runs the cells
/// serially on the caller's thread, the behavior of the harness before the
/// pool existed.
pub fn run_cells<T, F>(cells: Vec<(String, usize, F)>, max_width: usize) -> Vec<T>
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    let width = cells
        .iter()
        .map(|(_, raw_len, _)| batch_width(max_width, *raw_len))
        .min()
        .unwrap_or(1);
    let total = cells.len();
    let done = AtomicUsize::new(0);
    let finish = |name: &str| {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!("[{n}/{total}] {name}");
    };
    if width <= 1 || cells.len() == 1 {
        return cells
            .into_iter()
            .map(|(name, _, run)| {
                let out = run();
                finish(&name);
                out
            })
            .collect();
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(width)
        .build()
        .unwrap()
        .install(|| {
            cells
                .into_par_iter()
                .map(|(name, _, run)| {
                    let out = run();
                    finish(&name);
                    out
                })
                .collect()
        })
}
