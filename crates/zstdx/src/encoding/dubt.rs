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

/// Marks a node as inserted but not yet sorted. Positions stay far below
/// bit 63 (the opt tree's `POS_MASK` premise), so the sentinel cannot
/// collide with a real entry.
const UNSORTED: u64 = 1 << 63;

/// The DUBT finder: hash heads (`table`, raw positions) plus the two-slot
/// ring (`bt`).
pub(crate) struct DubtFinder<'a, 'b> {
    pub(crate) win: &'a [u8],
    pub(crate) win_base: u64,
    pub(crate) block_end_idx: usize,
    pub(crate) max_window: u64,
    pub(crate) table: &'b mut [u64],
    pub(crate) table_log: u32,
    pub(crate) bt: &'b mut [u64],
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
                let pos = self.win_base + idx as u64;
                let slot = bt.add(2 * (pos as usize & self.bt_mask));
                *slot = *table.add(h);
                *slot.add(1) = UNSORTED;
                *table.add(h) = pos;
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
        let mut common_smaller = 0usize;
        let mut common_larger = 0usize;
        let mut dummy = 0u64;
        // SAFETY: ring slots are masked by construction; window reads stay
        // inside the block (count_from caps at block_end and the equal-tail
        // break precedes the ordering byte compare).
        unsafe {
            let bt = self.bt.as_mut_ptr();
            let own = bt.add(2 * (curr as usize & self.bt_mask));
            let mut smaller: *mut u64 = own;
            let mut larger: *mut u64 = own.add(1);
            let mut cand = *smaller; // the fill-time chain link
            while nb > 0 && cand != 0 && cand > floor && cand < curr {
                nb -= 1;
                let cidx = (cand - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, curr_idx, cidx, ml, self.block_end_idx);
                // An equal tail cannot order the candidate; stop to keep
                // the tree consistent.
                if curr_idx + ml >= self.block_end_idx {
                    break;
                }
                let node = bt.add(2 * (cand as usize & self.bt_mask));
                if *self.win.get_unchecked(cidx + ml) < *self.win.get_unchecked(curr_idx + ml) {
                    *smaller = cand;
                    common_smaller = ml;
                    if cand <= unsort_limit {
                        smaller = &raw mut dummy;
                        break;
                    }
                    smaller = node.add(1);
                    cand = *node.add(1);
                } else {
                    *larger = cand;
                    common_larger = ml;
                    if cand <= unsort_limit {
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
    }

    /// `ZSTD_DUBT_findBestMatch` generalized to candidate collection (the
    /// btlazy2 selection prices every improving candidate, not just the
    /// longest): consume the bucket's unsorted backlog, then descend from
    /// the head collecting strictly longer candidates, repcodes first.
    /// Returns the match count; `length_to_beat` starts at `min_match`.
    pub(crate) fn find_all_matches(
        &mut self,
        matches: &mut [Match],
        idx: usize,
        rep: &[u32; 3],
        ll0: u32,
        length_to_beat: usize,
        rep_gated: bool,
    ) -> usize {
        debug_assert!(idx + super::match_generator::HASH_READ <= self.block_end_idx);
        let pos = self.win_base + idx as u64;
        let floor = self.cand_floor(pos);
        let mut best_len = length_to_beat - 1;
        let mut mnum = 0usize;

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
                        matches[mnum] = Match {
                            off: rep_code - ll0 + 1,
                            len: rep_len as u32,
                        };
                        mnum += 1;
                        best_len = rep_len;
                        if rep_len > self.sufficient_len || idx + rep_len >= self.block_end_idx {
                            return mnum;
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
            let mut prev = 0u64;
            while cand != 0 && cand > unsort_limit && cand < pos {
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
            if cand != 0 && cand > unsort_limit && cand < pos {
                let node = bt.add(2 * (cand as usize & self.bt_mask));
                if *node.add(1) == UNSORTED {
                    *node = 0;
                    *node.add(1) = 0;
                }
            }
            // Batch-sort the walked nodes oldest-to-newest; the backtrack
            // link must be read before the sort overwrites the slot.
            let mut m = prev;
            while m != 0 {
                let node = bt.add(2 * (m as usize & self.bt_mask));
                let newer = *node.add(1);
                self.insert_dubt1(m, nb_candidates, unsort_limit);
                m = newer;
                nb_candidates += 1;
            }

            // Sorted descent from the head, threading `pos` into the tree
            // while collecting strictly improving candidates.
            let head = *self.table.as_ptr().add(h);
            *self.table.as_mut_ptr().add(h) = pos;
            let mut cand = head;
            let mut common_smaller = 0usize;
            let mut common_larger = 0usize;
            let mut nb = self.nb_compares;
            let mut dummy = 0u64;
            let own = bt.add(2 * (pos as usize & self.bt_mask));
            let mut smaller: *mut u64 = own;
            let mut larger: *mut u64 = own.add(1);
            while nb > 0 && cand != 0 && cand > floor && cand < pos {
                nb -= 1;
                let cidx = (cand - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, idx, cidx, ml, self.block_end_idx);
                if ml > best_len {
                    if cand + ml as u64 > match_end {
                        match_end = cand + ml as u64;
                    }
                    best_len = ml;
                    matches[mnum] = Match {
                        off: (pos - cand + 3) as u32,
                        len: ml as u32,
                    };
                    mnum += 1;
                    if ml > OPT_NUM || idx + ml >= self.block_end_idx {
                        break;
                    }
                }
                if idx + ml >= self.block_end_idx {
                    break;
                }
                let node = bt.add(2 * (cand as usize & self.bt_mask));
                if *self.win.get_unchecked(cidx + ml) < *self.win.get_unchecked(idx + ml) {
                    *smaller = cand;
                    common_smaller = ml;
                    if cand <= bt_low {
                        smaller = &raw mut dummy;
                        break;
                    }
                    smaller = node.add(1);
                    cand = *node.add(1);
                } else {
                    *larger = cand;
                    common_larger = ml;
                    if cand <= bt_low {
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
        mnum
    }
}
