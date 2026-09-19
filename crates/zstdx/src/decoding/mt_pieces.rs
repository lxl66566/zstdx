//! Piece-parallel stage B: sequence execution split at the encoder's MT
//! job grid, for frames whose jobs carry the deep-offset ramp guarantee.
//!
//! The serial stage B (see [`mt`]) executes staged segments in order
//! because any sequence may read output arbitrarily close below its
//! segment's start. Job-style encoders reset entropy state at every job
//! boundary, so job starts are detectable restart blocks — and jobs
//! encoded under the deep-offset ramp (`ZSTDX_MT_RAMP_BYTES`, see
//! `encoding::mt`) additionally reject every cross-boundary match whose
//! source reaches shallower than the ramp depth below the job start. On
//! such frames, whole jobs ("pieces") depend only on a thin
//! executed-prefix watermark and stage B parallelizes.
//!
//! Engagement is measured, never trusted: the decoder plans candidate job
//! grids from the job-size formula's lattice (restart-block positions
//! carry no signal — this encoder re-sends entropy tables densely), then
//! re-walks each candidate's staged sequences standalone (the repcode
//! history starts from a sentinel, so any dependence on pre-piece state
//! fails loudly) and keeps a grid only if every piece's external reads
//! stay a fixed fraction of the job size below its start. Frames that
//! fail discovery or validation fall back to the serial stage B
//! unchanged.
//!
//! Validation is fused into staging instead of running as a barrier round
//! after it: each staged segment extends the contiguous staged prefix,
//! grid multiples are checked and pieces cut against that prefix, and
//! idle workers walk pieces as soon as their range is fully staged — the
//! last piece's walk is the only validation left on the critical path.
//! The verdict (first ascending grid that fully validates with a deep
//! enough lag) is deterministic in the staged content, so the fused form
//! picks the same grid the round form would. Worker wakes and caller
//! wakes use separate condvars: phase transitions touch the pool once,
//! and verdict changes wake only the calling thread.
//!
//! Gated on the same env var as the encoder-side ramp (an experiment
//! pair): this path runs stage A to completion before executing (staging
//! must cover the whole frame before the grid is known), losing the serial
//! path's A/B overlap — default frames must not route here. Only single
//! pledged frames engage: the pledge partitions the output up front and
//! bounds staging memory.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex};

use super::{
    errors::FrameDecoderError,
    mt::{
        BlockPlan, DecodedSegment, ScanPlan, SegmentPlan, SegmentScratch, decode_segment,
        execute_blocks, execute_segment,
    },
    sequence_execution::do_offset_history,
};
#[cfg(feature = "hash")]
use crate::xxh64::Xxh64;

/// Below this pledged size the piece machinery cannot pay (the job floor
/// alone is 512 KiB; the encoder's MT path needs 2 MiB anyway).
pub(super) const MIN_PIECE_FRAME: usize = 2 * 1024 * 1024;
/// Above this pledged size staging the whole frame (all segments' literals
/// plus sequences) outweighs the serial path's bounded staging.
pub(super) const MAX_PIECE_FRAME: usize = 64 * 1024 * 1024;
/// Smallest job size worth cutting a piece at (the encoder's own job floor
/// is 1 MiB; 512 KiB admits foreign grids without going sub-piece).
const MIN_PIECE_JOB: usize = 512 * 1024;
/// Engage only when every piece's external reads stay at least
/// `MIN_LAG_NUM / MIN_LAG_DEN` of the job size below its start — a
/// pipeline start-lag of `lag` bounds the speedup at `J / (J - lag)`, so
/// 3/8 demands a ceiling ≥ 1.6x before the extra machinery runs.
const MIN_LAG_NUM: usize = 3;
const MIN_LAG_DEN: usize = 8;

static TEST_ENABLE: AtomicBool = AtomicBool::new(false);
static ENGAGEMENTS: AtomicUsize = AtomicUsize::new(0);

/// Test-only router override (the env var is process-global; tests must
/// not race other tests through `set_var`). Hidden: not API-stable.
#[doc(hidden)]
pub fn set_piece_decode_for_tests(on: bool) {
    TEST_ENABLE.store(on, Ordering::Relaxed);
}

/// How many decodes engaged the piece-parallel executor so far (process
/// lifetime). Hidden: not API-stable.
#[doc(hidden)]
pub fn piece_engagements() -> usize {
    ENGAGEMENTS.load(Ordering::Relaxed)
}

fn enabled() -> bool {
    static ENV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    TEST_ENABLE.load(Ordering::Relaxed)
        || *ENV.get_or_init(|| {
            std::env::var("ZSTDX_MT_RAMP_BYTES")
                .is_ok_and(|v| v.trim() != "0" && !v.trim().is_empty())
        })
}

/// Whether `plan` should route to the piece executor: the env gate plus a
/// single pledged frame in the staging bounds. Returns the pledged size.
pub(super) fn gate(plan: &ScanPlan) -> Option<u64> {
    if !enabled() {
        return None;
    }
    let fcs = plan.pledged_single?;
    (MIN_PIECE_FRAME..=MAX_PIECE_FRAME)
        .contains(&(fcs as usize))
        .then_some(fcs)
}

/// A contiguous run of one segment's blocks inside a piece.
#[derive(Clone)]
struct PiecePart {
    seg: usize,
    blocks: Range<usize>,
    /// Absolute output position of the first block in `blocks`.
    start: usize,
}

/// One piece: the output range of one encoder job, as whole staged blocks.
#[derive(Clone)]
struct Piece {
    start: usize,
    end: usize,
    parts: Vec<PiecePart>,
    /// Highest external output position (+16 wildcopy read slack) this
    /// piece reads, measured by validation; 0 when it reads nothing below
    /// its start. Execution waits for the watermark to reach it.
    max_top: usize,
}

/// One block in output order across all staged segments.
struct BlockEntry {
    seg: usize,
    idx: usize,
    start: usize,
}

/// Candidate job sizes to try, ascending: every output of the encoder's
/// shared job formula `ceil(fcs / (2 * workers))` over plausible worker
/// counts (floored at 1 MiB like the formula), plus the overlap-raised
/// sizes deep levels' strips can set (row-9 reach: a 4 MiB strip). Wrong
/// candidates die on their first violating piece, so the reject cost
/// stays near one piece walk per grid.
fn candidate_jobs(fcs: usize) -> Vec<usize> {
    let mut cands: Vec<usize> = (2..=32u32)
        .map(|w| fcs.div_ceil(w as usize * 2).max(1024 * 1024))
        .collect();
    cands.extend([2, 3, 4, 6, 8].map(|m| m * 1024 * 1024));
    cands.retain(|&j| j >= MIN_PIECE_JOB && j * 2 <= fcs);
    cands.sort_unstable();
    cands.dedup();
    cands
}

/// One candidate grid's incremental state, advanced against the
/// contiguous staged prefix: block-start multiples checked, pieces cut
/// once their whole range is staged, and per-piece validation progress.
struct GridState {
    j: usize,
    alive: bool,
    pieces: Vec<Piece>,
    /// Measured max_top per cut piece, in piece order.
    tops: Vec<usize>,
    /// Next piece index to hand to a validator (pieces below are cut).
    next_validate: usize,
    validated: usize,
    /// Block-table cursor: index of the first block not yet consumed by a
    /// cut piece.
    cut_bi: usize,
    /// Next multiple k whose block-start check is still pending.
    next_mult: usize,
}

impl GridState {
    fn new(j: usize) -> Self {
        GridState {
            j,
            alive: true,
            pieces: Vec::new(),
            tops: Vec::new(),
            next_validate: 0,
            validated: 0,
            cut_bi: 0,
            next_mult: 1,
        }
    }
}

/// Resolve one piece's sequences standalone and measure its deepest
/// external read. `None` = the piece depends on state the walk cannot
/// reconstruct (a repcode slot never rewritten in-piece, or an offset past
/// the piece's absolute start) — the grid is unusable.
///
/// The sentinel history makes every never-written slot resolve to
/// ~`u32::MAX`, far beyond any legal offset, so such sequences fail the
/// `off > pos` check instead of silently resolving wrong. The +16 in the
/// measured top covers wildcopy read overshoot on crossing matches.
fn validate_piece(segs: &[Arc<DecodedSegment>], piece: &Piece) -> Option<usize> {
    let mut hist = if piece.start == 0 {
        [1u32, 4, 8]
    } else {
        [u32::MAX; 3]
    };
    let mut pos = piece.start;
    let mut max_top = 0usize;
    for part in &piece.parts {
        let seg = &segs[part.seg];
        for block in &seg.plan[part.blocks.clone()] {
            match block {
                BlockPlan::Raw { out, .. } => pos += out,
                BlockPlan::Rle { len, .. } => pos += len,
                BlockPlan::Compressed { lits, seqs, .. } => {
                    let mut lits_used = 0usize;
                    for seq in &seg.sequences[seqs.clone()] {
                        pos += seq.ll as usize;
                        lits_used += seq.ll as usize;
                        let off = do_offset_history(seq.of, seq.ll, &mut hist);
                        if off == 0 || off as usize > pos {
                            return None;
                        }
                        let src = pos - off as usize;
                        if src < piece.start {
                            max_top = max_top.max((src + seq.ml as usize + 16).min(piece.start));
                        }
                        pos += seq.ml as usize;
                    }
                    pos += (lits.end - lits.start) - lits_used;
                },
            }
        }
    }
    debug_assert_eq!(pos, piece.end, "piece validation must tile the piece");
    Some(max_top)
}

/// Execute one piece into the output. The caller guarantees the executed
/// watermark has reached `piece.max_top` (every external read is final).
///
/// # Safety
/// `base` must be valid for writes over `[piece.start, piece.end)` plus 16
/// bytes of wildcopy slack below `min(buf_limit, piece.end)`, and for
/// reads over the final output below `piece.start`.
unsafe fn execute_piece(
    base: *mut u8,
    input: &[u8],
    segs: &[Arc<DecodedSegment>],
    piece: &Piece,
    buf_limit: usize,
) -> Result<(), FrameDecoderError> {
    let mut hist = if piece.start == 0 {
        [1u32, 4, 8]
    } else {
        [u32::MAX; 3]
    };
    for part in &piece.parts {
        // SAFETY: caller contract above; parts tile the piece and the
        // history carries the true state at each part start (validated).
        unsafe {
            execute_blocks(
                base,
                part.start,
                &segs[part.seg],
                part.blocks.clone(),
                input,
                &mut hist,
                buf_limit,
                piece.end,
            )?;
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Phase {
    /// Workers stage segments and validate every candidate grid's pieces
    /// as their range stages; the verdict is decided on the pool.
    Staging,
    /// The chosen grid executes as pieces; `pieces` holds them.
    Execute,
    /// Validation failed everywhere: the caller runs the serial stage B.
    Serial,
}

enum Job {
    Stage(usize),
    Validate { grid: usize, piece: usize },
    Exec(usize),
    Quit,
}

/// One segment's staging slot. `Ok` segments move into `Driver::segs` when
/// the contiguous prefix reaches them.
enum Slot {
    Pending,
    Ok(Arc<DecodedSegment>),
    Failed,
}

struct Driver {
    phase: Phase,
    aborting: bool,
    n_segments: usize,
    fcs: usize,
    staged: Vec<Slot>,
    staged_count: usize,
    next_stage: usize,
    stage_errors: Vec<(usize, FrameDecoderError)>,
    /// Segments [0, contig_next) are staged and in `segs`, with their
    /// blocks in `blocks` covering [0, staged_pos).
    contig_next: usize,
    staged_pos: usize,
    segs: Vec<Arc<DecodedSegment>>,
    blocks: Vec<BlockEntry>,
    grids: Vec<GridState>,
    winner: Option<usize>,
    all_dead: bool,
    stage_failed: bool,
    pieces: Option<Arc<Vec<Piece>>>,
    next_piece: usize,
    done: Vec<bool>,
    /// Piece index one past the contiguous done prefix.
    watermark_piece: usize,
    /// Absolute end of the contiguous done prefix.
    watermark: usize,
    executed: usize,
    absorb_next: usize,
    exec_err: Option<FrameDecoderError>,
    base: usize,
    buf_limit: usize,
    #[cfg(feature = "hash")]
    hash: Option<Xxh64>,
}

impl Driver {
    /// Extend the contiguous staged prefix over consecutively completed
    /// segments, then advance the grids against it. Returns whether new
    /// validation work appeared.
    fn advance_prefix(&mut self) -> bool {
        while self.contig_next < self.n_segments {
            let seg = match &self.staged[self.contig_next] {
                Slot::Ok(s) => s.clone(),
                _ => break,
            };
            let mut pos = self.staged_pos;
            for (bi, b) in seg.plan.iter().enumerate() {
                self.blocks.push(BlockEntry {
                    seg: self.contig_next,
                    idx: bi,
                    start: pos,
                });
                pos += b.out();
            }
            self.staged_pos = pos;
            self.contig_next += 1;
            self.segs.push(seg);
        }
        self.refresh_grids(self.staged_count == self.n_segments)
    }

    /// Check grid multiples and cut pieces against the current staged
    /// prefix; `complete` marks frame-end staging (every multiple is
    /// decidable and every piece cuttable). Returns whether any new
    /// validation work appeared.
    fn refresh_grids(&mut self, complete: bool) -> bool {
        let mut new_work = false;
        if complete && self.staged_pos != self.fcs {
            // Corrupt pledge: no grid can tile the output, so every grid
            // dies and the serial fallback below verifies the frame's
            // declared size (verify_frame_content_sizes).
            self.grids.iter_mut().for_each(|g| g.alive = false);
            return false;
        }
        for gi in 0..self.grids.len() {
            let j = self.grids[gi].j;
            let n_pieces = self.fcs.div_ceil(j);
            while self.grids[gi].alive && self.grids[gi].next_mult < n_pieces {
                let target = self.grids[gi].next_mult * j;
                if !complete && self.staged_pos <= target {
                    break; // undecided until a block past the target stages
                }
                if self
                    .blocks
                    .binary_search_by_key(&target, |b| b.start)
                    .is_err()
                {
                    self.grids[gi].alive = false;
                    break;
                }
                self.grids[gi].next_mult += 1;
            }
            while self.grids[gi].alive && self.grids[gi].pieces.len() < n_pieces {
                let k = self.grids[gi].pieces.len();
                let start = k * j;
                let end = ((k + 1) * j).min(self.fcs);
                if self.staged_pos < end {
                    break;
                }
                debug_assert_eq!(
                    self.blocks[self.grids[gi].cut_bi].start, start,
                    "grid multiples are block starts"
                );
                let mut bi = self.grids[gi].cut_bi;
                let mut parts = Vec::new();
                while bi < self.blocks.len() && self.blocks[bi].start < end {
                    let seg = self.blocks[bi].seg;
                    let part_start = self.blocks[bi].start;
                    let first = self.blocks[bi].idx;
                    let mut last = first;
                    while bi < self.blocks.len()
                        && self.blocks[bi].start < end
                        && self.blocks[bi].seg == seg
                    {
                        last = self.blocks[bi].idx;
                        bi += 1;
                    }
                    parts.push(PiecePart {
                        seg,
                        blocks: first..last + 1,
                        start: part_start,
                    });
                }
                self.grids[gi].cut_bi = bi;
                self.grids[gi].pieces.push(Piece {
                    start,
                    end,
                    parts,
                    max_top: 0,
                });
                self.grids[gi].tops.push(0);
                new_work = true;
            }
        }
        new_work
    }

    /// Settle the verdict on the first alive grid once it is fully cut and
    /// every piece is validated: lag pass → winner; lag fail → dead and
    /// the next grid is considered. Grids after an alive-but-incomplete
    /// one wait (their pieces are claimed only in staging idle anyway —
    /// ascending priority).
    fn try_settle(&mut self) {
        if self.winner.is_some() {
            return;
        }
        for gi in 0..self.grids.len() {
            let g = &self.grids[gi];
            if !g.alive {
                continue;
            }
            // A grid still being cut is incomplete even when its cut
            // pieces all validated: settling it would leave the uncut
            // tail's max_top at zero and the watermark gate open.
            let n_pieces = self.fcs.div_ceil(g.j);
            if g.pieces.len() < n_pieces || g.validated < g.pieces.len() {
                break;
            }
            let j = g.j;
            let lag_ok = g
                .pieces
                .iter()
                .skip(1)
                .zip(g.tops.iter().skip(1))
                .all(|(p, &top)| p.start.saturating_sub(top) >= j * MIN_LAG_NUM / MIN_LAG_DEN);
            if lag_ok {
                self.winner = Some(gi);
                return;
            }
            self.grids[gi].alive = false;
        }
        self.all_dead = self.grids.iter().all(|g| !g.alive);
    }

    /// Next validation job: the first alive grid (ascending job size)
    /// with an unclaimed cut piece.
    fn claim_validation(&mut self) -> Option<Job> {
        if self.winner.is_some() || self.all_dead || self.stage_failed {
            return None;
        }
        self.grids
            .iter_mut()
            .enumerate()
            .find(|(_, g)| g.alive && g.next_validate < g.pieces.len())
            .map(|(grid, g)| {
                let piece = g.next_validate;
                g.next_validate += 1;
                Job::Validate { grid, piece }
            })
    }

    /// Absorb every completed-but-unabsorbed piece below `upto` (piece
    /// index bound) into the frame checksum, in order. Own-range absorbs
    /// stay on the executing thread's caches; a foreign range is absorbed
    /// by whichever thread unblocks the order.
    fn absorb_through(&mut self, pieces: &[Piece], upto: usize) {
        let mut next = self.absorb_next;
        #[cfg(feature = "hash")]
        while next < upto {
            let p = &pieces[next];
            if let Some(h) = self.hash.as_mut() {
                // SAFETY: base covers the whole frame; the piece range is
                // final (its done flag is set).
                unsafe {
                    h.write(core::slice::from_raw_parts(
                        (self.base as *const u8).add(p.start),
                        p.end - p.start,
                    ));
                }
            }
            next += 1;
        }
        #[cfg(not(feature = "hash"))]
        {
            let _ = pieces;
        }
        self.absorb_next = next;
    }
}

/// Shared pool context: the phase machine under one mutex, worker and
/// caller condvars (verdict changes wake the calling thread alone), panic
/// poison, and the immutable inputs.
struct Pool<'a> {
    mtx: &'a Mutex<Driver>,
    cv: &'a Condvar,
    cv_caller: &'a Condvar,
    poison: &'a Mutex<Option<Box<dyn std::any::Any + Send>>>,
    input: &'a [u8],
    plans: &'a [SegmentPlan],
    n_segments: usize,
}

impl Pool<'_> {
    fn claim(&self) -> Job {
        let mut d = self.mtx.lock().unwrap();
        loop {
            if d.aborting {
                return Job::Quit;
            }
            match d.phase {
                Phase::Staging => {
                    if d.next_stage < self.n_segments {
                        let id = d.next_stage;
                        d.next_stage += 1;
                        return Job::Stage(id);
                    }
                    if let Some(job) = d.claim_validation() {
                        return job;
                    }
                    d = self.cv.wait(d).unwrap();
                },
                Phase::Execute => {
                    let len = d.pieces.as_ref().map_or(0, |p| p.len());
                    if d.next_piece < len {
                        let i = d.next_piece;
                        d.next_piece += 1;
                        return Job::Exec(i);
                    }
                    if d.executed < len {
                        d = self.cv.wait(d).unwrap();
                        continue;
                    }
                    return Job::Quit;
                },
                Phase::Serial => return Job::Quit,
            }
        }
    }

    fn run_stage(&self, id: usize, scratch: &mut SegmentScratch) {
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_segment(self.input, &self.plans[id], scratch)
        }));
        let mut d = self.mtx.lock().unwrap();
        match attempt {
            Ok(Ok(seg)) => d.staged[id] = Slot::Ok(Arc::new(seg)),
            Ok(Err(e)) => {
                d.staged[id] = Slot::Failed;
                d.stage_errors.push((id, e));
                d.stage_failed = true;
                d.grids.iter_mut().for_each(|g| g.alive = false);
            },
            Err(payload) => {
                *self.poison.lock().unwrap() = Some(payload);
                d.aborting = true;
            },
        }
        d.staged_count += 1;
        let mut new_work = false;
        if !d.aborting && !d.stage_failed && id == d.contig_next {
            new_work = d.advance_prefix();
            d.try_settle();
        }
        let settled = d.winner.is_some() || d.all_dead;
        let staged_done = d.staged_count == self.n_segments;
        let aborting = d.aborting;
        drop(d);
        if new_work || aborting {
            self.cv.notify_all();
        }
        // The calling thread parks only once staging is exhausted, so the
        // verdict conditions alone can release it.
        if settled || staged_done || aborting {
            self.cv_caller.notify_one();
        }
    }

    fn run_validate(&self, grid: usize, piece: usize) {
        let (segs, target) = {
            let d = self.mtx.lock().unwrap();
            (d.segs.clone(), d.grids[grid].pieces[piece].clone())
        };
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            validate_piece(&segs, &target)
        }));
        let mut d = self.mtx.lock().unwrap();
        d.grids[grid].validated += 1;
        match attempt {
            Ok(Some(top)) => d.grids[grid].tops[piece] = top,
            Ok(None) => d.grids[grid].alive = false,
            Err(payload) => {
                *self.poison.lock().unwrap() = Some(payload);
                d.aborting = true;
            },
        }
        d.try_settle();
        let settled = d.winner.is_some() || d.all_dead;
        let aborting = d.aborting;
        drop(d);
        // No worker wake otherwise: a validation completion never makes new
        // work claimable (pieces appear only through the staging prefix).
        if aborting {
            self.cv.notify_all();
        }
        if settled || aborting {
            self.cv_caller.notify_one();
        }
    }

    fn run_exec(&self, i: usize) {
        let (segs, pieces, base, buf_limit) = {
            let mut d = self.mtx.lock().unwrap();
            let pieces = d.pieces.clone().expect("exec job implies pieces");
            while d.watermark < pieces[i].max_top && d.exec_err.is_none() && !d.aborting {
                d = self.cv.wait(d).unwrap();
            }
            if d.exec_err.is_some() || d.aborting {
                return;
            }
            (d.segs.clone(), pieces, d.base as *mut u8, d.buf_limit)
        };
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: place() covered [0, fcs) at `base`; the watermark
            // gate made every external read final.
            unsafe { execute_piece(base, self.input, &segs, &pieces[i], buf_limit) }
        }));
        let mut d = self.mtx.lock().unwrap();
        match attempt {
            Ok(Ok(())) => {
                d.done[i] = true;
                d.executed += 1;
                if i == d.watermark_piece {
                    let all = d.pieces.clone().expect("exec job implies pieces");
                    while d.watermark_piece < all.len() && d.done[d.watermark_piece] {
                        d.watermark_piece += 1;
                    }
                    d.watermark = if d.watermark_piece == all.len() {
                        all.last().map_or(0, |p| p.end)
                    } else {
                        all[d.watermark_piece].start
                    };
                    let upto = d.watermark_piece;
                    d.absorb_through(&all, upto);
                }
            },
            Ok(Err(e)) => {
                if d.exec_err.is_none() {
                    d.exec_err = Some(e);
                }
                d.aborting = true;
            },
            Err(payload) => {
                *self.poison.lock().unwrap() = Some(payload);
                d.aborting = true;
            },
        }
        let finished = d.executed == d.pieces.as_ref().map_or(0, |p| p.len());
        let errored = d.exec_err.is_some() || d.aborting;
        drop(d);
        self.cv.notify_all();
        if finished || errored {
            self.cv_caller.notify_one();
        }
    }

    fn worker_loop(&self) {
        let mut scratch = SegmentScratch::new();
        loop {
            match self.claim() {
                Job::Quit => return,
                Job::Stage(id) => self.run_stage(id, &mut scratch),
                Job::Validate { grid, piece } => self.run_validate(grid, piece),
                Job::Exec(i) => self.run_exec(i),
            }
        }
    }
}

/// Abort in-flight phases (a caller error path) and wake the pool.
fn abort_pool(mtx: &Mutex<Driver>, cv: &Condvar, cv_caller: &Condvar) {
    mtx.lock().unwrap().aborting = true;
    cv.notify_all();
    cv_caller.notify_one();
}

/// The first staging error in segment order, taken out of the driver.
fn take_stage_error(driver: &Mutex<Driver>) -> Option<FrameDecoderError> {
    let mut d = driver.lock().unwrap();
    let pos = d
        .stage_errors
        .iter()
        .enumerate()
        .min_by_key(|(_, (id, _))| *id)
        .map(|(pos, _)| pos)?;
    Some(d.stage_errors.swap_remove(pos).1)
}

/// Piece-parallel driver: stage and validate fused on the pool (grid
/// pieces are walked as their range stages; the verdict is the first
/// ascending grid that fully validates with a deep-enough watermark lag),
/// then execute pieces on the pool + caller. Validation failure runs the
/// serial stage B over the same staged segments. `place` is called with
/// the whole pledged range on this thread only.
pub(super) fn decode_pieces(
    input: &[u8],
    workers: u32,
    plan: &ScanPlan,
    fcs: u64,
    place: &mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>,
) -> Result<usize, FrameDecoderError> {
    let fcs = fcs as usize;
    let n_segments = plan.segments.len();
    let threads = (workers as usize).min(n_segments);

    let driver = Mutex::new(Driver {
        phase: Phase::Staging,
        aborting: false,
        n_segments,
        fcs,
        staged: (0..n_segments).map(|_| Slot::Pending).collect(),
        staged_count: 0,
        next_stage: 0,
        stage_errors: Vec::new(),
        contig_next: 0,
        staged_pos: 0,
        segs: Vec::new(),
        blocks: Vec::new(),
        grids: candidate_jobs(fcs)
            .into_iter()
            .map(GridState::new)
            .collect(),
        winner: None,
        all_dead: false,
        stage_failed: false,
        pieces: None,
        next_piece: 0,
        done: Vec::new(),
        watermark_piece: 0,
        watermark: 0,
        executed: 0,
        absorb_next: 0,
        exec_err: None,
        base: 0,
        buf_limit: 0,
        #[cfg(feature = "hash")]
        hash: None,
    });
    let cv = Condvar::new();
    let cv_caller = Condvar::new();
    let poison: Mutex<Option<Box<dyn std::any::Any + Send>>> = Mutex::new(None);
    let pool = Pool {
        mtx: &driver,
        cv: &cv,
        cv_caller: &cv_caller,
        poison: &poison,
        input,
        plans: &plan.segments,
        n_segments,
    };

    // The expected trailer word of the single frame, if checksummed.
    #[cfg(feature = "hash")]
    let expected = plan.checksums.first().copied().flatten();

    let result = std::thread::scope(|scope| -> Result<usize, FrameDecoderError> {
        for _ in 0..threads {
            scope.spawn(|| pool.worker_loop());
        }

        // Fused staging + validation runs on the pool, and the calling
        // thread stages segments along (it executes pieces later the same
        // way): between segments it parks on its own condvar until either
        // staging work appears or the verdict is ready (a winning grid, a
        // staging error, or every grid dead). It never claims validation:
        // a caller walking pieces reads the staged set while workers still
        // write it, and measured slower on every shape than staying out
        // (the staging streams are the same either way).
        {
            let mut scratch = SegmentScratch::new();
            let mut d = driver.lock().unwrap();
            loop {
                let ready = d.winner.is_some()
                    || d.aborting
                    || (d.staged_count == n_segments && (d.all_dead || d.stage_failed));
                if ready {
                    break;
                }
                let job: Option<Job> = if d.phase != Phase::Staging || d.next_stage >= n_segments {
                    None
                } else {
                    let id = d.next_stage;
                    d.next_stage += 1;
                    Some(Job::Stage(id))
                };
                match job {
                    Some(Job::Stage(id)) => {
                        drop(d);
                        pool.run_stage(id, &mut scratch);
                        d = driver.lock().unwrap();
                    },
                    Some(Job::Validate { .. } | Job::Exec(_) | Job::Quit) => {
                        unreachable!("the caller claims staging only")
                    },
                    None => d = cv_caller.wait(d).unwrap(),
                }
            }
            if d.aborting {
                // The poison (if any) resumes after the scope joins.
                return Err(FrameDecoderError::NotYetInitialized);
            }
        }
        if let Some(e) = take_stage_error(&driver) {
            abort_pool(&driver, &cv, &cv_caller);
            return Err(e);
        }

        // A plain binding first: a lock guard in the let-else scrutinee
        // would live through the else branch and deadlock the re-lock.
        let winner = driver.lock().unwrap().winner;
        let Some(winner) = winner else {
            // Serial stage B over the staged segments; workers quit.
            driver.lock().unwrap().phase = Phase::Serial;
            cv.notify_all();
            let segs = driver.lock().unwrap().segs.clone();
            #[cfg(feature = "hash")]
            let mut hash = expected.map(|e| (Xxh64::new(0), e));
            #[cfg(feature = "hash")]
            return run_serial(input, &segs, plan, place, &mut hash);
            #[cfg(not(feature = "hash"))]
            return run_serial(input, &segs, plan, place);
        };

        // Piece execution: one place() call covers the whole pledged range.
        let (base, buf_limit) = match place(0, fcs) {
            Ok(ok) => ok,
            Err(e) => {
                abort_pool(&driver, &cv, &cv_caller);
                return Err(e);
            },
        };
        let n = {
            let mut d = driver.lock().unwrap();
            let g = &mut d.grids[winner];
            for (p, &top) in g.pieces.iter_mut().zip(&g.tops) {
                p.max_top = top;
            }
            let pieces = Arc::new(core::mem::take(&mut g.pieces));
            let n = pieces.len();
            d.pieces = Some(pieces);
            d.done = alloc::vec![false; n];
            d.next_piece = 0;
            d.watermark_piece = 0;
            d.watermark = 0;
            d.executed = 0;
            d.absorb_next = 0;
            d.base = base as usize;
            d.buf_limit = buf_limit;
            #[cfg(feature = "hash")]
            {
                d.hash = expected.map(|_| Xxh64::new(0));
            }
            d.phase = Phase::Execute;
            n
        };
        cv.notify_all();
        ENGAGEMENTS.fetch_add(1, Ordering::Relaxed);

        // The calling thread executes pieces too (in-order claims keep the
        // watermark dependency chain deadlock-free).
        loop {
            match pool.claim() {
                Job::Quit => break,
                Job::Exec(i) => pool.run_exec(i),
                Job::Stage(_) | Job::Validate { .. } => {
                    unreachable!("phase counters are exhausted in Execute")
                },
            }
        }
        let verdict = {
            let mut d = driver.lock().unwrap();
            while d.executed < n && d.exec_err.is_none() && !d.aborting {
                d = cv_caller.wait(d).unwrap();
            }
            if let Some(e) = d.exec_err.take() {
                Err(e)
            } else if d.aborting {
                Err(FrameDecoderError::NotYetInitialized)
            } else {
                // Drain the ordered checksum absorb, then finish+compare.
                let all = d.pieces.clone().expect("execute phase implies pieces");
                #[cfg(feature = "hash")]
                let mismatch = {
                    d.absorb_through(&all, n);
                    d.hash.as_mut().zip(expected).and_then(|(h, want)| {
                        let calc = h.finish() as u32;
                        (calc != want).then_some(FrameDecoderError::ChecksumMismatch {
                            expected: want,
                            calculated: calc,
                        })
                    })
                };
                #[cfg(not(feature = "hash"))]
                let mismatch = None;
                mismatch.map_or_else(|| Ok(fcs), Err)
            }
        };
        if verdict.is_err() {
            abort_pool(&driver, &cv, &cv_caller);
        }
        verdict
    });
    if let Some(payload) = poison.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
    result
}

/// The serial stage B over already-staged segments (the validation
/// fallback): identical semantics to `mt::decode_parallel`'s executor.
/// `hash` is `(stream, expected)` when the frame is checksummed.
#[allow(clippy::type_complexity)]
fn run_serial(
    input: &[u8],
    segs: &[Arc<DecodedSegment>],
    plan: &ScanPlan,
    place: &mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>,
    #[cfg(feature = "hash")] hash: &mut Option<(Xxh64, u32)>,
) -> Result<usize, FrameDecoderError> {
    let mut offset_hist = [1u32, 4, 8];
    let mut written = 0usize;
    // Output offset where each frame began, in scan order.
    let mut frame_starts = Vec::with_capacity(plan.frame_pledges.len());
    for (id, seg) in segs.iter().enumerate() {
        if plan.segments[id].frame_start {
            offset_hist = [1, 4, 8];
            frame_starts.push(written);
        }
        let (base, buf_limit) = place(written, written + seg.out_size)?;
        // SAFETY: place made [0, written + out_size) valid; every match
        // source below `written` is final (in-order execution).
        unsafe {
            execute_segment(base, written, seg, input, &mut offset_hist, buf_limit)?;
        }
        #[cfg(feature = "hash")]
        if let Some((h, _)) = hash.as_mut() {
            // SAFETY: same-thread absorb of just-executed bytes, before the
            // next `place` call may move the output.
            unsafe {
                h.write(core::slice::from_raw_parts(base.add(written), seg.out_size));
            }
        }
        written += seg.out_size;
    }
    // The pledge check precedes the checksum, matching the sequential
    // paths' frame-tail ordering.
    super::mt::verify_frame_content_sizes(&plan.frame_pledges, &frame_starts, written)?;
    #[cfg(feature = "hash")]
    if let Some((h, expected)) = hash.as_ref() {
        let calculated = h.finish() as u32;
        if calculated != *expected {
            return Err(FrameDecoderError::ChecksumMismatch {
                expected: *expected,
                calculated,
            });
        }
    }
    Ok(written)
}
