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
const MIN_PIECE_FRAME: usize = 2 * 1024 * 1024;
/// Above this pledged size staging the whole frame (all segments' literals
/// plus sequences) outweighs the serial path's bounded staging.
const MAX_PIECE_FRAME: usize = 64 * 1024 * 1024;
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
/// candidates die on the first validated piece (violations abort the
/// round), so the lattice's reject cost stays near one piece walk.
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

/// Candidate grids for the staged frame: `(job_size, pieces)` pairs,
/// ascending job size. A grid is kept when every `k * job_size` lands on a
/// block start (the encoder's jobs end at block edges, so true job
/// boundaries always are; pieces may start at any block — entropy
/// independence is the segment's job, not the piece's).
fn plan_grids(segs: &[Arc<DecodedSegment>], fcs: usize) -> Vec<(usize, Vec<Piece>)> {
    let total: usize = segs.iter().map(|s| s.plan.len()).sum();
    let mut blocks: Vec<BlockEntry> = Vec::with_capacity(total);
    let mut pos = 0usize;
    for (si, seg) in segs.iter().enumerate() {
        for (bi, b) in seg.plan.iter().enumerate() {
            blocks.push(BlockEntry {
                seg: si,
                idx: bi,
                start: pos,
            });
            pos += b.out();
        }
    }
    if pos != fcs {
        // Corrupt pledge: the serial path reports it with its own errors.
        return Vec::new();
    }
    let mut grids = Vec::new();
    'seed: for j in candidate_jobs(fcs) {
        let n_pieces = fcs.div_ceil(j);
        for k in 1..n_pieces {
            let target = k * j;
            if blocks.binary_search_by_key(&target, |b| b.start).is_err() {
                continue 'seed;
            }
        }
        grids.push((j, cut_pieces(&blocks, j, fcs, n_pieces)));
    }
    grids
}

fn cut_pieces(blocks: &[BlockEntry], j: usize, fcs: usize, n_pieces: usize) -> Vec<Piece> {
    let mut pieces = Vec::with_capacity(n_pieces);
    let mut bi = 0usize;
    for k in 0..n_pieces {
        let start = k * j;
        let end = ((k + 1) * j).min(fcs);
        while blocks[bi].start < start {
            bi += 1;
        }
        debug_assert_eq!(blocks[bi].start, start, "grid multiples are block starts");
        let mut parts = Vec::new();
        while bi < blocks.len() && blocks[bi].start < end {
            let seg = blocks[bi].seg;
            let part_start = blocks[bi].start;
            let first = blocks[bi].idx;
            let mut last = first;
            while bi < blocks.len() && blocks[bi].start < end && blocks[bi].seg == seg {
                last = blocks[bi].idx;
                bi += 1;
            }
            parts.push(PiecePart {
                seg,
                blocks: first..last + 1,
                start: part_start,
            });
        }
        pieces.push(Piece {
            start,
            end,
            parts,
            max_top: 0,
        });
    }
    pieces
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
    /// Workers stage segments; no execution decision yet.
    Staging,
    /// A candidate grid is being validated; `round` holds its pieces.
    Validating,
    /// The chosen grid executes as pieces; `pieces` holds them.
    Execute,
    /// Validation failed everywhere: the caller runs the serial stage B.
    Serial,
}

enum Job {
    Stage(usize),
    Validate(usize),
    Exec(usize),
    Quit,
}

struct Driver {
    phase: Phase,
    aborting: bool,
    staged: Vec<Option<Result<Arc<DecodedSegment>, FrameDecoderError>>>,
    staged_count: usize,
    next_stage: usize,
    segs: Vec<Arc<DecodedSegment>>,
    round: Option<Arc<Vec<Piece>>>,
    round_tops: Vec<Arc<AtomicUsize>>,
    next_validate: usize,
    validated: usize,
    violations: usize,
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

/// Shared pool context: the phase machine under one mutex, a broadcast
/// condvar, panic poison, and the immutable inputs.
struct Pool<'a> {
    mtx: &'a Mutex<Driver>,
    cv: &'a Condvar,
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
                    if d.staged_count < self.n_segments {
                        d = self.cv.wait(d).unwrap();
                        continue;
                    }
                    // Staging complete; the caller decides the next phase.
                    d = self.cv.wait(d).unwrap();
                },
                Phase::Validating => {
                    let len = d.round.as_ref().map_or(0, |r| r.len());
                    if d.violations == 0 && d.next_validate < len {
                        let i = d.next_validate;
                        d.next_validate += 1;
                        return Job::Validate(i);
                    }
                    if d.validated < d.next_validate {
                        d = self.cv.wait(d).unwrap();
                        continue;
                    }
                    // Round settled; the caller posts the next one.
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
            Ok(Ok(seg)) => d.staged[id] = Some(Ok(Arc::new(seg))),
            Ok(Err(e)) => d.staged[id] = Some(Err(e)),
            Err(payload) => {
                *self.poison.lock().unwrap() = Some(payload);
                d.aborting = true;
            },
        }
        d.staged_count += 1;
        drop(d);
        self.cv.notify_all();
    }

    fn run_validate(&self, i: usize) {
        let (segs, round, tops) = {
            let d = self.mtx.lock().unwrap();
            let round = d.round.clone().expect("validation job implies a round");
            (d.segs.clone(), round, d.round_tops[i].clone())
        };
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            validate_piece(&segs, &round[i])
        }));
        let mut d = self.mtx.lock().unwrap();
        d.validated += 1;
        match attempt {
            Ok(Some(top)) => tops.store(top, Ordering::Relaxed),
            Ok(None) => d.violations += 1,
            Err(payload) => {
                *self.poison.lock().unwrap() = Some(payload);
                d.aborting = true;
            },
        }
        drop(d);
        self.cv.notify_all();
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
        drop(d);
        self.cv.notify_all();
    }

    fn worker_loop(&self) {
        let mut scratch = SegmentScratch::new();
        loop {
            match self.claim() {
                Job::Quit => return,
                Job::Stage(id) => self.run_stage(id, &mut scratch),
                Job::Validate(i) => self.run_validate(i),
                Job::Exec(i) => self.run_exec(i),
            }
        }
    }
}

/// Abort in-flight phases (a caller error path) and wake the pool.
fn abort_pool(mtx: &Mutex<Driver>, cv: &Condvar) {
    mtx.lock().unwrap().aborting = true;
    cv.notify_all();
}

/// Piece-parallel driver: stage everything, discover and validate job
/// grids, then execute pieces on the pool + caller. Validation failure
/// runs the serial stage B over the same staged segments. `place` is
/// called with the whole pledged range on this thread only.
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
        staged: (0..n_segments).map(|_| None).collect(),
        staged_count: 0,
        next_stage: 0,
        segs: Vec::new(),
        round: None,
        round_tops: Vec::new(),
        next_validate: 0,
        validated: 0,
        violations: 0,
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
    let poison: Mutex<Option<Box<dyn std::any::Any + Send>>> = Mutex::new(None);
    let pool = Pool {
        mtx: &driver,
        cv: &cv,
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

        // Phase A: wait for the whole frame to stage, then take it out of
        // the slots (the first error in segment order wins).
        {
            let mut d = driver.lock().unwrap();
            while d.staged_count < n_segments && !d.aborting {
                d = cv.wait(d).unwrap();
            }
        }
        let mut segs: Vec<Arc<DecodedSegment>> = Vec::with_capacity(n_segments);
        let stage_err = {
            let mut d = driver.lock().unwrap();
            let mut err = None;
            for slot in &mut d.staged {
                match slot.take().expect("staged_count covers every slot") {
                    Ok(seg) => {
                        if err.is_none() {
                            segs.push(seg);
                        }
                    },
                    Err(e) => {
                        if err.is_none() {
                            err = Some(e);
                        }
                    },
                }
            }
            err
        };
        if let Some(e) = stage_err {
            abort_pool(&driver, &cv);
            return Err(e);
        }

        // Grid discovery + validation rounds; the first grid whose pieces
        // all validate with a deep-enough watermark lag wins.
        let grids = plan_grids(&segs, fcs);
        let mut chosen: Option<Vec<Piece>> = None;
        'candidates: for (j, pieces) in grids {
            let n = pieces.len();
            {
                let mut d = driver.lock().unwrap();
                if d.aborting {
                    break 'candidates;
                }
                d.segs.clone_from(&segs);
                d.round = Some(Arc::new(pieces.clone()));
                d.round_tops = (0..n).map(|_| Arc::new(AtomicUsize::new(0))).collect();
                d.next_validate = 0;
                d.validated = 0;
                d.violations = 0;
                d.phase = Phase::Validating;
            }
            cv.notify_all();
            let (violations, tops) = {
                let mut d = driver.lock().unwrap();
                loop {
                    let settled = d.validated == d.next_validate
                        && (d.violations > 0 || d.next_validate == n);
                    if settled || d.aborting {
                        break;
                    }
                    d = cv.wait(d).unwrap();
                }
                if d.aborting {
                    break 'candidates;
                }
                let tops = d
                    .round_tops
                    .iter()
                    .map(|t| t.load(Ordering::Relaxed))
                    .collect::<Vec<_>>();
                (d.violations, tops)
            };
            if violations > 0 {
                continue;
            }
            let lag_ok = pieces
                .iter()
                .skip(1)
                .zip(&tops[1..])
                .all(|(p, &top)| p.start.saturating_sub(top) >= j * MIN_LAG_NUM / MIN_LAG_DEN);
            if lag_ok {
                let mut merged = pieces;
                for (p, &top) in merged.iter_mut().zip(&tops) {
                    p.max_top = top;
                }
                chosen = Some(merged);
                break;
            }
        }

        let Some(pieces) = chosen else {
            // Serial stage B over the staged segments; workers quit.
            driver.lock().unwrap().phase = Phase::Serial;
            cv.notify_all();
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
                abort_pool(&driver, &cv);
                return Err(e);
            },
        };
        let n = pieces.len();
        {
            let mut d = driver.lock().unwrap();
            d.segs = segs;
            d.pieces = Some(Arc::new(pieces));
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
        }
        cv.notify_all();
        ENGAGEMENTS.fetch_add(1, Ordering::Relaxed);

        // The calling thread executes pieces too (in-order claims keep the
        // watermark dependency chain deadlock-free).
        loop {
            match pool.claim() {
                Job::Quit => break,
                Job::Exec(i) => pool.run_exec(i),
                Job::Stage(_) | Job::Validate(_) => {
                    unreachable!("phase counters are exhausted in Execute")
                },
            }
        }
        let verdict = {
            let mut d = driver.lock().unwrap();
            while d.executed < n && d.exec_err.is_none() && !d.aborting {
                d = cv.wait(d).unwrap();
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
            abort_pool(&driver, &cv);
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
    for (id, seg) in segs.iter().enumerate() {
        if plan.segments[id].frame_start {
            offset_hist = [1, 4, 8];
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
