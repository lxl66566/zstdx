//! btlazy2 strategy: libzstd's `ZSTD_compressBlock_btlazy2` — the
//! lazy-generic selection loop at depth 2 over the DUBT match tree
//! (`dubt.rs`): positions fill at three stores each, and sorting is
//! amortized into the searches' batch sort, so the lazy scan's
//! stepped-over positions and match interiors pay nothing beyond the fill.
//!
//! The selection prices displaced literals at the code lengths fed back by
//! the block encoder (the chain tier's measured win); before the first
//! feedback this reduces bit-for-bit to libzstd's flat `ml*4` raw gain.
//! The pricing runs inside the finder's descent (see `dubt::Found`): the
//! driver only combines the two per-search argmaxes under libzstd's
//! margins, so no candidate list crosses the call.

use alloc::vec::Vec;

use super::{
    SeqWord,
    dubt::{DubtFinder, Found, lazy_price, scale_value},
    ldm::LdmSeq,
    match_generator::{HASH_READ, MIN_MATCH, push_seq_packed},
    opt::{Match, OptKnobs, count_from, read4},
};

/// Displaced-literal value scale of one scan position: the sum of the
/// first four bytes' code lengths fed back by the block encoder, clamped
/// to 6 bits per byte — a local selection must not let cap-priced bytes
/// dominate its decisions. With the default flat lens this is 16,
/// libzstd's raw `ml*4` gain. The clamp is folded into a per-block table
/// so the gather itself is four loads and three adds.
#[inline(always)]
fn lit_scale(win: &[u8], idx: usize, lit_clamp: &[i32; 256]) -> i32 {
    lit_clamp[win[idx] as usize]
        + lit_clamp[win[idx + 1] as usize]
        + lit_clamp[win[idx + 2] as usize]
        + lit_clamp[win[idx + 3] as usize]
}

/// Forward-count a rep0 candidate at `p`: 4 verified bytes plus the
/// extension. `None` when the offset does not resolve or the bytes differ.
/// The caller guarantees 4 readable bytes at `p`.
#[inline]
fn rep_probe(
    win: &[u8],
    win_base: u64,
    block_end_idx: usize,
    p: u64,
    offset: u32,
) -> Option<usize> {
    let cand_abs = p.checked_sub(offset as u64)?;
    if cand_abs < win_base {
        return None;
    }
    let pidx = (p - win_base) as usize;
    let cand = (cand_abs - win_base) as usize;
    if read4(win, cand) != read4(win, pidx) {
        return None;
    }
    Some(count_from(win, pidx, cand, MIN_MATCH, block_end_idx))
}

/// Per-block scratch anchor for the pooled matcher state. The fused
/// selection needs no candidate buffer; the type stays as the bridge's
/// pooling seam (`match_generator` owns the `Option`).
pub(crate) struct LazyScratch;

impl LazyScratch {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// One full block parsed with the btlazy2 strategy. Selection is libzstd's
/// lazy-generic loop at depth 2: each position's repcode and tree
/// candidates compete on value-minus-offset-price, a two-deep lazy walk
/// chases a better start under the alternating depth-1/depth-2 margins,
/// literal-offset matches extend backward into the pending literals, and
/// the offset-2 chain follows every emission.
///
/// Armed LDM turns the block into segments (libzstd's
/// `ZSTD_ldm_blockCompress` for the lazy family): each long-distance
/// candidate is emitted wholesale at its split and the lazy loop parses
/// only the literal gap ahead of it, bounded there exactly like libzstd's
/// per-gap compressor call. Price competition between the tree and far
/// candidates stays in the opt family's parser, which visits every
/// position anyway; here a searched far-resume point pays full equal-run
/// extensions against still-agreeing near twins (measured 2.1x block time
/// on dll-class content). `ldm_seqs` is empty when LDM is not armed,
/// leaving one segment — the stock parse. Returns whether any candidate
/// was emitted (the quiet latch's "won" signal).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_block_lazy(
    knobs: &OptKnobs,
    win: &[u8],
    win_base: u64,
    block_start: u64,
    block_end: u64,
    max_window: u64,
    ldm_seqs: &[LdmSeq],
    table: &mut [u32],
    bt: &mut [u32],
    next_update: &mut u64,
    _scratch: &mut LazyScratch,
    lit_lens: &[u8; 256],
    rep: &mut [u32; 3],
    rep_pending: &mut u8,
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) -> bool {
    let block_end_idx = (block_end - win_base) as usize;
    let table_log = table.len().trailing_zeros();
    let bt_mask = bt.len() / 2 - 1;
    let mut finder = DubtFinder {
        win,
        win_base,
        block_end_idx,
        max_window,
        table,
        bt,
        next_update,
        table_log,
        bt_mask,
        mls: knobs.mls as usize,
        nb_compares: 1usize << knobs.search_log,
        min_match: knobs.min_match as usize,
        sufficient_len: knobs.sufficient_len as usize,
    };
    let mut lit_clamp = [0i32; 256];
    for (dst, &len) in lit_clamp.iter_mut().zip(lit_lens.iter()) {
        *dst = (len as i32).min(6);
    }

    // Long-distance candidates of this block: `ldm_i` is the first
    // unconsumed one. Generate's anchor skip keeps their spans disjoint
    // and splits ascending, so each segment ends at the next split.
    // `pend` is the pending literal run's start — it crosses segment
    // seams so every emission's literal length stays exact.
    let mut ldm_i = 0usize;
    let mut ldm_won = false;
    let mut seg = block_start;
    let mut pend = block_start;

    'segments: loop {
        let (end, seq) = match ldm_seqs.get(ldm_i) {
            Some(s) => (s.split, Some(*s)),
            None => (block_end, None),
        };
        let end_idx = (end - win_base) as usize;
        finder.block_end_idx = end_idx;

        // The very first frame position has nothing behind it.
        let mut pos = seg + (seg == 0) as u64;
        let mut anchor = pend;
        let mut miss = 0u32;

        while pos + (HASH_READ as u64) < end {
            let idx = (pos - win_base) as usize;
            let ll0 = (pos == anchor) as u32;
            // Depth 0: the tree search (floor MIN_MATCH — libzstd's lazy bar
            // accepts 4-byte matches at every searchLength), then the rep0
            // probe one byte ahead when the anchor is flush (the fast and
            // chain loops' trick: that byte becomes the pending literal,
            // keeping the rep0 offset rideable at ll>=1). `best_sv` is the
            // incumbent's value minus its offset price at its own position —
            // carried so no position's scale is ever gathered twice.
            let mut best = Match { off: 0, len: 0 };
            let mut best_sv = 0i32;
            if pos >= *finder.next_update {
                finder.fill_to(idx);
                let scale = lit_scale(win, idx, &lit_clamp);
                let f: Found = finder.find_best(idx, rep, ll0, MIN_MATCH, *rep_pending != 0, scale);
                if f.rep.len > 0 || f.cand.len > 0 {
                    // Reps precede tree candidates in the collection order and
                    // price 0, so a score tie keeps the rep — the strict-`>`
                    // argmax of the array loop this replaced.
                    let rep_v = if f.rep.len > 0 {
                        scale_value(scale, f.rep.len as usize)
                    } else {
                        i32::MIN
                    };
                    if rep_v >= f.score {
                        best = f.rep;
                        best_sv = rep_v;
                    } else {
                        best = f.cand;
                        best_sv = f.score;
                    }
                }
            }
            if *rep_pending == 0 {
                let probe = if pos == anchor {
                    pos + 1
                } else {
                    pos
                };
                if let Some(ml) = rep_probe(win, win_base, end_idx, probe, rep[0]) {
                    let v =
                        scale_value(lit_scale(win, (probe - win_base) as usize, &lit_clamp), ml);
                    // An empty incumbent (no tree candidate) prices 0 — the
                    // baseline the rep probe must beat.
                    let incumbent = if best.len > 0 {
                        best_sv
                    } else {
                        0
                    };
                    if v > incumbent {
                        best = Match {
                            off: 1,
                            len: ml as u32,
                        };
                        pos = probe;
                        best_sv = v;
                    }
                }
            }
            if best.len < MIN_MATCH as u32 {
                // Grow the probe step on long literal runs (the fast/chain
                // policy); the fill still indexes every stepped position.
                miss += 1;
                let step = 1 + (miss >> 2).min(255) as u64;
                pos += step;
                continue;
            }
            miss = 0;
            let mut best_pos = pos;
            let mut best_p = lazy_price(best.off);
            let mut best_v = best_sv + best_p;

            // Lazy walk (depth 2): each further position's repcode and tree
            // candidates compete under libzstd's alternating margins —
            // (rep_len x3, +1, +4) then (rep_len x4, +1, +7); a challenger
            // that clears either restarts the walk from its position. The
            // repcode is evaluated first; the search challenger then competes
            // against the updated incumbent (libzstd's order). `rep_mul`
            // keeps libzstd's rep discount (3/4 of the byte value at depth
            // 1); at the default lens the comparison is the flat `ml*mul`.
            // The x4 arm folds its `/4`s away exactly (no rounding there).
            if best.len < 64 {
                'lazy: loop {
                    macro_rules! lazy_step {
                        ($rep_mul:expr, $rep_m:expr, $search_m:expr) => {{
                            let p2 = pos + 1;
                            if end - p2 < HASH_READ as u64 {
                                break 'lazy;
                            }
                            pos = p2;
                            let idx2 = (p2 - win_base) as usize;
                            if p2 >= *finder.next_update {
                                finder.fill_to(idx2);
                                let scale2 = lit_scale(win, idx2, &lit_clamp);
                                let f = finder.find_best(
                                    idx2,
                                    rep,
                                    0,
                                    MIN_MATCH,
                                    *rep_pending != 0,
                                    scale2,
                                );
                                if f.rep.len > 0 {
                                    let v = scale_value(scale2, f.rep.len as usize);
                                    let wins = if $rep_mul == 4 {
                                        v > best_v - best_p + $rep_m
                                    } else {
                                        v * $rep_mul / 4 > best_v * $rep_mul / 4 - best_p + $rep_m
                                    };
                                    if wins {
                                        best = f.rep;
                                        best_pos = p2;
                                        best_v = v;
                                        best_p = 0;
                                    }
                                }
                                if f.cand.len > 0 && f.score > best_v - best_p + $search_m {
                                    best = f.cand;
                                    best_pos = p2;
                                    let p = lazy_price(f.cand.off);
                                    best_v = f.score + p;
                                    best_p = p;
                                    continue 'lazy;
                                }
                            }
                        }};
                    }
                    lazy_step!(3, 1, 4);
                    lazy_step!(4, 1, 7);
                    break;
                }
            }

            // Backward extension into the pending literals (literal offsets
            // only: moving a repcode's start would change its ll0 form). The
            // offset stays constant.
            let mut start = (best_pos - win_base) as usize;
            let mut ml = best.len as usize;
            if best.off > 3 {
                let anchor_idx = (anchor - win_base) as usize;
                let mut cand = start - (best.off as usize - 3);
                while start > anchor_idx && cand > 0 && win[cand - 1] == win[start - 1] {
                    cand -= 1;
                    start -= 1;
                    ml += 1;
                }
            }
            let (_, match_end) = push_seq_packed(
                win, win_base, anchor, start, ml, best.off, rep, literals, seqs,
            );
            if *rep_pending != 0 && best.off > 3 {
                *rep_pending -= 1;
            }
            pos = win_base + match_end as u64;
            anchor = pos;

            // Immediate offset-2 chain (libzstd's rep_offset2 loop): ll0
            // repcode sequences alternating rep0/rep1 by construction.
            while *rep_pending == 0 && end - pos >= MIN_MATCH as u64 {
                let ml2 = match rep_probe(win, win_base, end_idx, pos, rep[1]) {
                    Some(ml) => ml,
                    None => break,
                };
                let pidx = (pos - win_base) as usize;
                let (_, me) =
                    push_seq_packed(win, win_base, pos, pidx, ml2, 1, rep, literals, seqs);
                pos = win_base + me as u64;
                anchor = pos;
            }
        }

        // The gap's pending literals become the candidate's own literal run:
        // the emission starts at the split with no backward extension, so its
        // ll is exactly the gap length.
        let Some(seq) = seq else {
            // Last segment: the stock literals-only tail rule.
            if !seqs.is_empty() && anchor < end {
                let tail = (anchor - win_base) as usize..end_idx;
                literals.extend_from_slice(&win[tail]);
            }
            break 'segments;
        };
        ldm_i += 1;
        debug_assert_eq!(seq.split, end);
        // The 4-byte guard only defends a stale candidate (generate verified
        // the whole span at fill time); a failure drops it and re-opens the
        // segment from the split.
        let cand_abs = end.checked_sub(seq.offset as u64);
        let valid = match cand_abs {
            Some(c) if c >= win_base && seq.len as usize >= MIN_MATCH => {
                read4(win, (c - win_base) as usize) == read4(win, end_idx)
            },
            _ => false,
        };
        if !valid {
            // The dropped candidate's split re-opens as gap bytes; `pend`
            // keeps them attached to the next emission.
            seg = end;
            pend = anchor;
            continue 'segments;
        }
        let (_, match_end) = push_seq_packed(
            win,
            win_base,
            anchor,
            end_idx,
            seq.len as usize,
            seq.offset + 3,
            rep,
            literals,
            seqs,
        );
        ldm_won = true;
        if *rep_pending != 0 {
            *rep_pending -= 1;
        }
        let next = win_base + match_end as u64;
        // libzstd's ZSTD_ldm_limitTableUpdate: the lazy tables catch up at
        // most 512 bytes behind the emission — the candidate's interior
        // stays unindexed. Indexing it feeds the resume-point search
        // twin-period candidates whose equal runs scan to the block cap
        // (measured 3x the whole-block scan volume on dll-class content).
        if next > *finder.next_update + 1024 {
            *finder.next_update = next - 512;
        }
        if next >= block_end {
            break 'segments;
        }
        seg = next;
        pend = next;
    }
    ldm_won
}
