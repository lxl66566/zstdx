//! DUBT match finder: a faithful port of libzstd's doubly-unsorted binary
//! tree (`ZSTD_updateDUBT` / `ZSTD_insertDUBT1` / `ZSTD_DUBT_findBestMatch`
//! in `zstd_lazy.c`), the finder behind libzstd's btlazy2 strategy.
//!
//! The split from the optimal parser's tree ([`super::opt::Finder`]) is the
//! fill architecture: filling a position costs three stores — hash-head
//! update, chain link to the previous head, unsorted mark — with no
//! comparisons. Sorting is amortized into the searches: a search first walks
//! its bucket's unsorted chain (reversing it into a backtrack link),
//! nullifies the chain's far end, then batch-sorts the walked nodes
//! oldest-to-newest via one budgeted tree descent each. Nodes the walk
//! never reaches stay unsorted forever — match interiors and stepped-over
//! positions pay nothing beyond the fill, which is the whole speed point.
//!
//! Like libzstd (and unlike the opt tree), entries are **u32**: the tree
//! ring and hash heads are random-access working sets sized by the window
//! (48 MiB at W22), where entry width is the dominant lever — the u64 form
//! measured x2.87 vs libzstd on skewed.best, memory-latency-bound (200M L1
//! + 100M dTLB misses per 32 MiB, one serialized load per tree step).
//!
//! Where C's search returns the single longest match, ours prices every
//! strictly improving candidate against the position's literal scale and
//! keeps the two argmaxes the lazy selection consumes ([`Found`]) — fused
//! into the descent, so no candidate list is ever written. The wider
//! selection is the +2.5% density over zstd-13; pricing it per candidate
//! in the driver's array loop was the tier's speed deficit.
//!
//! Slot layout per ring position: `[0]` is the chain link while unsorted
//! and the smaller child once sorted; `[1]` is the unsorted mark (or the
//! backtrack link during a search's chain walk) and the larger child once
//! sorted. Because children are always older than their parent and every
//! sorted node's slots were written by a completed descent, a descent never
//! crosses an unsorted node; a stale ring read resolves as a candidate
//! outside the window and dies on the floor check, and every candidate's
//! bytes are verified before use, so a corrupted link can only waste
//! compares, never falsify a match.

use super::opt::{Match, OPT_NUM, count_from, hash3_at, hash4_at, hash5_at, read4};

/// Offset price class (`highbit32(offBase)`); ~free for repcodes and for
/// "no candidate".
#[inline(always)]
pub(crate) fn lazy_price(off: u32) -> i32 {
    if off > 3 {
        off.ilog2() as i32
    } else {
        0
    }
}

/// Displaced-literal value of a match of `len` bytes at a position whose
/// four-byte literal scale is `s` (the driver's `lit_scale`).
#[inline(always)]
pub(crate) fn scale_value(s: i32, len: usize) -> i32 {
    debug_assert!(len >= super::match_generator::MIN_MATCH || len == 0);
    s * len as i32 / 4
}

/// One search's selection, priced against the position's literal scale:
/// the longest repcode candidate (price 0 by construction) and the best
/// literal-offset candidate by value-minus-offset-price, either side
/// absent at `len == 0`. This is the improving candidate set of the plain
/// port reduced to its two argmaxes — the only facts the lazy selection
/// consumes — computed inside the descent so the per-candidate array
/// round trip (store per improving candidate, reload and re-price in the
/// driver) never happens.
pub(crate) struct Found {
    /// Longest repcode candidate; `off` is the wire off-base (1-3).
    pub(crate) rep: Match,
    /// Best literal-offset candidate by `score`.
    pub(crate) cand: Match,
    /// `scale_value(scale, cand.len) - lazy_price(cand.off)`; `i32::MIN`
    /// while `cand.len == 0`.
    pub(crate) score: i32,
}

/// Marks a node as inserted but not yet sorted. The value 1 is also
/// position 1's stored form: a child pointer to it misreads as unsorted,
/// which at worst re-sorts an already-sorted node — bounded by the descent
/// budget and byte verification, the same ambiguity libzstd documents in
/// `ZSTD_insertDUBT1`.
const UNSORTED: u32 = 1;

/// Storage bias (kept at zero so outputs stay byte-identical to the u64
/// form: position 0's entry reads as the null link and position 1's as the
/// unsorted mark, exactly the sentinel collisions the u64 port carried).
const POS_BIAS: u64 = 0;

/// Truncate a position to its stored u32 form.
#[inline(always)]
fn pack_pos(pos: u64) -> u32 {
    (pos + POS_BIAS) as u32
}

/// Rebuild the absolute position of stored entry `v` read while scanning
/// `pos`. Candidates are window-bounded (far below 4 GiB away), so the
/// entry's high bits are implied by `pos`; a stale entry from an earlier
/// 4 GiB period rebuilds as a phantom inside that band, which the range
/// checks reject or the byte compare verifies — it can only waste work.
#[inline(always)]
fn unpack_pos(v: u32, pos: u64) -> u64 {
    pos.wrapping_sub((pos as u32).wrapping_sub(v) as u64)
        .wrapping_sub(POS_BIAS)
}

/// The DUBT finder: hash heads (`table`) plus the two-slot ring (`bt`),
/// both u32 like libzstd's btlazy2 tables.
pub(crate) struct DubtFinder<'a, 'b> {
    pub(crate) win: &'a [u8],
    pub(crate) win_base: u64,
    pub(crate) block_end_idx: usize,
    pub(crate) max_window: u64,
    pub(crate) table: &'b mut [u32],
    pub(crate) table_log: u32,
    pub(crate) bt: &'b mut [u32],
    pub(crate) bt_mask: usize,
    pub(crate) mls: usize,
    pub(crate) nb_compares: usize,
    pub(crate) min_match: usize,
    pub(crate) sufficient_len: usize,
    pub(crate) next_update: &'b mut u64,
}

impl DubtFinder<'_, '_> {
    /// Lowest absolute position usable as a candidate when scanning `pos`.
    #[inline]
    fn cand_floor(&self, pos: u64) -> u64 {
        pos.saturating_sub(self.max_window).max(self.win_base)
    }

    #[inline(always)]
    fn hash_main(&self, idx: usize) -> usize {
        match self.mls {
            3 => hash3_at(self.win, idx, self.table_log),
            4 => hash4_at(self.win, idx, self.table_log),
            _ => hash5_at(self.win, idx, self.table_log),
        }
    }

    /// `ZSTD_updateDUBT`: fill `[next_update, target)` at three stores per
    /// position. Positions below the window floor can never resolve as
    /// candidates, so the fill starts at the floor.
    pub(crate) fn fill_to(&mut self, target_idx: usize) {
        debug_assert!(target_idx + super::match_generator::HASH_READ <= self.block_end_idx);
        let target_abs = self.win_base + target_idx as u64;
        let fill_floor = target_abs.saturating_sub(self.max_window);
        let mut idx =
            ((*self.next_update).max(self.win_base).max(fill_floor) - self.win_base) as usize;
        // SAFETY: the hash masks to the table size and the ring slot to the
        // ring by construction; positions are live window offsets.
        unsafe {
            let table = self.table.as_mut_ptr();
            let bt = self.bt.as_mut_ptr();
            while idx < target_idx {
                let h = self.hash_main(idx);
                let v = pack_pos(self.win_base + idx as u64);
                let slot = bt.add(2 * (v as usize & self.bt_mask));
                *slot = *table.add(h);
                *slot.add(1) = UNSORTED;
                *table.add(h) = v;
                idx += 1;
            }
        }
        *self.next_update = target_abs;
    }

    /// `ZSTD_insertDUBT1`: sort one unsorted node into the tree with a
    /// budgeted descent from its fill-time chain link. The descent threads
    /// the node's smaller/larger pointers down through the sorted nodes and
    /// nulls the trailing links (pruning the subtree below the budget).
    fn insert_dubt1(&mut self, curr: u64, mut nb: usize, unsort_limit: u64) {
        let floor = self.cand_floor(curr);
        let curr_idx = (curr - self.win_base) as usize;
        // The null link rebuilds as a huge position, so the range checks
        // below reject it without a separate test.
        let mut common_smaller = 0usize;
        let mut common_larger = 0usize;
        let mut dummy = 0u32;
        // SAFETY: ring slots are masked by construction; window reads stay
        // inside the block (count_from caps at block_end and the equal-tail
        // break precedes the ordering byte compare).
        unsafe {
            let bt = self.bt.as_mut_ptr();
            let own = bt.add(2 * (pack_pos(curr) as usize & self.bt_mask));
            let mut smaller: *mut u32 = own;
            let mut larger: *mut u32 = own.add(1);
            let mut v = *smaller; // the fill-time chain link
            while nb > 0 {
                let cand = unpack_pos(v, curr);
                if !(cand > floor && cand < curr) {
                    break;
                }
                nb -= 1;
                let cidx = (cand - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, curr_idx, cidx, ml, self.block_end_idx);
                // An equal tail cannot order the candidate; stop to keep
                // the tree consistent.
                if curr_idx + ml >= self.block_end_idx {
                    break;
                }
                let node = bt.add(2 * (v as usize & self.bt_mask));
                if *self.win.get_unchecked(cidx + ml) < *self.win.get_unchecked(curr_idx + ml) {
                    *smaller = v;
                    common_smaller = ml;
                    if cand <= unsort_limit {
                        smaller = &raw mut dummy;
                        break;
                    }
                    smaller = node.add(1);
                    v = *node.add(1);
                } else {
                    *larger = v;
                    common_larger = ml;
                    if cand <= unsort_limit {
                        larger = &raw mut dummy;
                        break;
                    }
                    larger = node;
                    v = *node;
                }
            }
            *smaller = 0;
            *larger = 0;
        }
    }

    /// `ZSTD_DUBT_findBestMatch` fused with the btlazy2 selection's
    /// pricing (see [`Found`]): consume the bucket's unsorted backlog,
    /// then descend from the head pricing strictly improving candidates —
    /// repcodes first, kept by longest length (their scores are strictly
    /// increasing in it). `length_to_beat` starts at `min_match`.
    pub(crate) fn find_best(
        &mut self,
        idx: usize,
        rep: &[u32; 3],
        ll0: u32,
        length_to_beat: usize,
        rep_gated: bool,
        scale: i32,
    ) -> Found {
        debug_assert!(idx + super::match_generator::HASH_READ <= self.block_end_idx);
        let pos = self.win_base + idx as u64;
        let floor = self.cand_floor(pos);
        let mut best_len = length_to_beat - 1;
        let mut found = Found {
            rep: Match { off: 1, len: 0 },
            cand: Match { off: 0, len: 0 },
            score: i32::MIN,
        };

        // Repeated-offset candidates (the cheapest offsets to encode) —
        // same contract as the optimal parser's collection walk.
        if !rep_gated {
            for rep_code in ll0..3 + ll0 {
                let rep_offset = if rep_code == 3 {
                    rep[0].saturating_sub(1)
                } else {
                    rep[rep_code as usize]
                };
                let Some(cand_abs) = pos.checked_sub(rep_offset as u64) else {
                    continue;
                };
                if rep_offset == 0 || cand_abs < floor {
                    continue;
                }
                let cand = (cand_abs - self.win_base) as usize;
                let hit = if self.min_match == 3 {
                    (read4(self.win, cand) << 8) == (read4(self.win, idx) << 8)
                } else {
                    read4(self.win, cand) == read4(self.win, idx)
                };
                if hit {
                    let rep_len =
                        count_from(self.win, idx, cand, self.min_match, self.block_end_idx);
                    if rep_len > best_len {
                        found.rep = Match {
                            off: rep_code - ll0 + 1,
                            len: rep_len as u32,
                        };
                        best_len = rep_len;
                        if rep_len > self.sufficient_len || idx + rep_len >= self.block_end_idx {
                            // Same table contract as the normal exit: the searched
                            // position is inserted — fill form, head + chain link
                            // + unsorted mark, exactly what the next fill_to would
                            // write — and next_update moves past it, so no later
                            // fill re-inserts it. (Runs before the main hash/head
                            // handling below; cold path.)
                            let h = self.hash_main(idx);
                            let v = pack_pos(pos);
                            let slot = 2 * (v as usize & self.bt_mask);
                            self.bt[slot] = self.table[h];
                            self.bt[slot + 1] = UNSORTED;
                            self.table[h] = v;
                            *self.next_update = (*self.next_update).max(pos + 1);
                            return found;
                        }
                    }
                }
            }
        }

        let h = self.hash_main(idx);
        let bt_low = pos.saturating_sub((self.bt.len() / 2) as u64);
        let unsort_limit = bt_low.max(floor);
        let mut nb_candidates = self.nb_compares;

        let mut match_end = pos + 9;
        // SAFETY: slots are masked to the ring; candidate reads are floor-
        // and pos-checked; window reads as in insert_dubt1.
        unsafe {
            let bt = self.bt.as_mut_ptr();
            // Walk the unsorted chain from the head, reversing slot[1] into
            // a backtrack link toward newer nodes.
            let mut cand = *self.table.as_ptr().add(h);
            let mut prev = 0u32;
            loop {
                let cand_abs = unpack_pos(cand, pos);
                if !(cand_abs > unsort_limit && cand_abs < pos) {
                    break;
                }
                let node = bt.add(2 * (cand as usize & self.bt_mask));
                if *node.add(1) != UNSORTED || nb_candidates <= 1 {
                    break;
                }
                *node.add(1) = prev;
                prev = cand;
                cand = *node;
                nb_candidates -= 1;
            }
            // Nullify the chain's far end: the boundary node becomes a
            // leaf and everything older in this bucket is orphaned —
            // libzstd's deliberate ratio-for-speed trade.
            {
                // The null link (cand == 0) rebuilds huge and fails this
                // range check, so it skips the nullify block too.
                let cand_abs = unpack_pos(cand, pos);
                if cand_abs > unsort_limit && cand_abs < pos {
                    let node = bt.add(2 * (cand as usize & self.bt_mask));
                    if *node.add(1) == UNSORTED {
                        *node = 0;
                        *node.add(1) = 0;
                    }
                }
            }
            // Batch-sort the walked nodes oldest-to-newest; the backtrack
            // link must be read before the sort overwrites the slot.
            let mut m = prev;
            while m != 0 {
                let node = bt.add(2 * (m as usize & self.bt_mask));
                let newer = *node.add(1);
                self.insert_dubt1(unpack_pos(m, pos), nb_candidates, unsort_limit);
                m = newer;
                nb_candidates += 1;
            }

            // Sorted descent from the head, threading `pos` into the tree
            // while pricing strictly improving candidates. The strict `>`
            // keeps the first score maximum — the argmax (and its
            // tie-breaks) of the array loop the fusion replaced.
            let head = *self.table.as_ptr().add(h);
            *self.table.as_mut_ptr().add(h) = pack_pos(pos);
            let mut cand = head;
            let mut common_smaller = 0usize;
            let mut common_larger = 0usize;
            let mut nb = self.nb_compares;
            let mut dummy = 0u32;
            let own = bt.add(2 * (pack_pos(pos) as usize & self.bt_mask));
            let mut smaller: *mut u32 = own;
            let mut larger: *mut u32 = own.add(1);
            while nb > 0 {
                let cand_abs = unpack_pos(cand, pos);
                if !(cand_abs > floor && cand_abs < pos) {
                    break;
                }
                nb -= 1;
                let cidx = (cand_abs - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, idx, cidx, ml, self.block_end_idx);
                let at_end = idx + ml >= self.block_end_idx;
                if ml > best_len {
                    if cand_abs + ml as u64 > match_end {
                        match_end = cand_abs + ml as u64;
                    }
                    best_len = ml;
                    let off = (pos - cand_abs + 3) as u32;
                    let score = scale_value(scale, ml) - lazy_price(off);
                    if score > found.score {
                        found.score = score;
                        found.cand = Match {
                            off,
                            len: ml as u32,
                        };
                    }
                    if ml > OPT_NUM || at_end {
                        break;
                    }
                }
                if at_end {
                    break;
                }
                let node = bt.add(2 * (cand as usize & self.bt_mask));
                if *self.win.get_unchecked(cidx + ml) < *self.win.get_unchecked(idx + ml) {
                    *smaller = cand;
                    common_smaller = ml;
                    if cand_abs <= bt_low {
                        smaller = &raw mut dummy;
                        break;
                    }
                    smaller = node.add(1);
                    cand = *node.add(1);
                } else {
                    *larger = cand;
                    common_larger = ml;
                    if cand_abs <= bt_low {
                        larger = &raw mut dummy;
                        break;
                    }
                    larger = node;
                    cand = *node;
                }
            }
            *smaller = 0;
            *larger = 0;
        }
        // Skip re-indexing the interior of long repetitive stretches.
        *self.next_update = match_end - 8;
        found
    }
}
