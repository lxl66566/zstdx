//! Optimal-price parser: a port of libzstd's btopt / btultra strategies
//! (`zstd_opt.c`). Matches come from a lazily-filled binary search tree over
//! suffixes (one `smaller`/`larger` child pair per position); every probed
//! position runs a forward DP over stretches ("a match followed by N
//! literals") priced with adaptive fractional-bit statistics, and the cheapest
//! path is emitted as sequences.
//!
//! The price state (symbol frequencies) persists across blocks: the first
//! block seeds from its own literal histogram plus libzstd's baseline
//! sequence-stat tables, later blocks downscale the accumulated statistics,
//! and every emitted sequence increments them (`ZSTD_rescaleFreqs` /
//! `ZSTD_updateStats`). `Ultra` additionally re-parses the first block once
//! purely to seed the statistics (`ZSTD_initStats_ultra`).

use alloc::vec::Vec;

use super::match_generator::{push_seq_packed, HASH_READ};
use super::seq_codes::{encode_literal_length, encode_match_len};
use super::SeqWord;
use crate::decoding::sequence_execution::do_offset_history;

/// Prices are fixed-point with 1/256-bit resolution (BITCOST_ACCURACY = 8).
const BITCOST_MULTIPLIER: u32 = 256;
const MAX_PRICE: u32 = 1 << 30;
/// DP window; matches reaching beyond this trigger immediate encoding.
const OPT_NUM: usize = 1 << 12;
const OPT_SIZE: usize = OPT_NUM + 3;
/// Literal frequency scaling factor so stats adapt within a block.
const LITFREQ_ADD: u32 = 2;
/// Blocks at or below this size price symbols from the predefined tables.
const PREDEF_THRESHOLD: usize = 8;

/// Positions in table entries live in the low 48 bits; the high 16 carry the
/// epoch (shared convention with the other matcher strategies).
const POS_MASK: u64 = (1u64 << 48) - 1;
const EMPTY: u64 = 0;

/// Whole-bit weight: `highbit32(stat+1)` scaled (libzstd's `ZSTD_bitWeight`).
#[inline(always)]
fn bit_weight(stat: u32) -> u32 {
    (stat + 1).ilog2() * BITCOST_MULTIPLIER
}

/// Fractional-bit weight: log2 approximated by linear interpolation
/// (`ZSTD_fracWeight`).
#[inline(always)]
fn frac_weight(raw: u32) -> u32 {
    let stat = raw + 1;
    let hb = stat.ilog2();
    debug_assert!(hb < 23, "weight shift overflow");
    (hb * BITCOST_MULTIPLIER) + ((stat << 8) >> hb)
}

/// `ZSTD_newRep`: repcode history after one sequence with wire offset
/// `off_base` and `ll0` literal-length-zero flag.
#[inline]
fn new_rep(rep: &[u32; 3], off_base: u32, ll0: bool) -> [u32; 3] {
    let mut out = *rep;
    // The history helper takes the literal length; a zero length encodes the
    // ll0 case, so flip the flag into a length of 0 or 1.
    do_offset_history(off_base, (!ll0) as u32, &mut out);
    out
}

/// One candidate match: wire offset (repcodes 1..=3, literal offset + 3) and
/// length (`ZSTD_match_t`).
#[derive(Clone, Copy)]
struct Match {
    off: u32,
    len: u32,
}

/// One DP entry: the stretch ending at this relative position — `mlen` bytes
/// of match (off) followed by `litlen` literals, with the price of both and
/// the repcode history at this position (`ZSTD_optimal_t`).
#[derive(Clone, Copy)]
struct Optimal {
    price: u32,
    off: u32,
    mlen: u32,
    litlen: u32,
    rep: [u32; 3],
}

impl Default for Optimal {
    fn default() -> Self {
        Self {
            price: MAX_PRICE,
            off: 0,
            mlen: 0,
            litlen: 0,
            rep: [1, 4, 8],
        }
    }
}

/// Per-strategy knobs (libzstd clevels 16-19 as the reference points).
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct OptKnobs {
    /// Tree compares per search (`searchLog`).
    pub search_log: u32,
    /// Matches longer than this are encoded immediately (`targetLength`).
    pub sufficient_len: u32,
    /// Shortest match the parser records; also the repcode check width
    /// (libzstd's parser `minMatch`, 3 or 4).
    pub min_match: u32,
    /// Main hash width in bytes (libzstd's `mls` template, 3-5; independent
    /// of `min_match` — libzstd hashes 5 bytes at level 16 while the parser
    /// still accepts 4-byte matches).
    pub mls: u32,
    /// Tree ring size as a power of two in positions (2 slots each).
    pub bt_log: u32,
    /// Single-probe 3-byte table; 0 disables (only Ultra uses one).
    pub hash3_log: u32,
    /// btultra family: fractional prices, match+1-literal recheck, 2-pass
    /// first-block statistics seeding.
    pub ultra: bool,
}

/// Price statistics persisting across blocks; a zero `lit_length_sum` means
/// "first block" and re-seeds (`ZSTD_rescaleFreqs`).
pub(crate) struct OptState {
    lit_freq: [u32; 256],
    lit_length_freq: [u32; 36],
    match_length_freq: [u32; 53],
    /// Indexed by `highbit32(off_base)`.
    off_code_freq: [u32; 32],
    lit_sum: u32,
    lit_length_sum: u32,
    match_length_sum: u32,
    off_code_sum: u32,
    lit_base_price: u32,
    lit_length_base_price: u32,
    match_length_base_price: u32,
    off_code_base_price: u32,
    /// Price from the predefined distributions (tiny blocks).
    predef: bool,
}

/// Per-block DP scratch, reused across blocks; ~130 KiB, so allocated only
/// when an opt strategy is actually selected.
pub(crate) struct OptScratch {
    opt: Vec<Optimal>,
    matches: Vec<Match>,
}

impl OptScratch {
    pub(crate) fn new() -> Self {
        Self {
            opt: alloc::vec![Optimal::default(); OPT_SIZE],
            matches: alloc::vec![Match { off: 1, len: 0 }; OPT_SIZE],
        }
    }
}

/// libzstd's baseline first-block statistics (no dictionary in play here).
const BASE_LL_FREQS: [u32; 36] = [
    4, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1,
];
const BASE_OFF_FREQS: [u32; 32] = [
    6, 2, 1, 1, 2, 3, 4, 4, 4, 3, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

fn downscale_stats(table: &mut [u32], shift: u32, base1: bool) -> u32 {
    let mut sum = 0;
    for stat in table.iter_mut() {
        let base = if base1 { 1 } else { (*stat > 0) as u32 };
        *stat = base + (*stat >> shift);
        sum += *stat;
    }
    sum
}

fn scale_stats(table: &mut [u32], target_log: u32) -> u32 {
    let prev: u32 = table.iter().sum();
    let factor = prev >> target_log;
    if factor <= 1 {
        return prev;
    }
    downscale_stats(table, factor.ilog2(), true)
}

#[inline(always)]
fn weight<const ULTRA: bool>(stat: u32) -> u32 {
    if ULTRA {
        frac_weight(stat)
    } else {
        bit_weight(stat)
    }
}

#[inline]
fn ll_inc_price<const ULTRA: bool>(state: &OptState, lit_length: u32) -> i64 {
    state.lit_length_price::<ULTRA>(lit_length) as i64
        - state.lit_length_price::<ULTRA>(lit_length - 1) as i64
}

impl OptState {
    pub(crate) fn new() -> Self {
        Self {
            lit_freq: [0; 256],
            lit_length_freq: [0; 36],
            match_length_freq: [0; 53],
            off_code_freq: [0; 32],
            lit_sum: 0,
            lit_length_sum: 0,
            match_length_sum: 0,
            off_code_sum: 0,
            lit_base_price: 0,
            lit_length_base_price: 0,
            match_length_base_price: 0,
            off_code_base_price: 0,
            predef: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.lit_length_sum = 0;
    }

    fn set_base_prices<const ULTRA: bool>(&mut self) {
        self.lit_base_price = weight::<ULTRA>(self.lit_sum);
        self.lit_length_base_price = weight::<ULTRA>(self.lit_length_sum);
        self.match_length_base_price = weight::<ULTRA>(self.match_length_sum);
        self.off_code_base_price = weight::<ULTRA>(self.off_code_sum);
    }

    /// `ZSTD_rescaleFreqs`: seed the first block, downscale afterwards.
    fn rescale_freqs<const ULTRA: bool>(&mut self, src: &[u8]) {
        self.predef = src.len() <= PREDEF_THRESHOLD;
        if self.lit_length_sum == 0 {
            self.lit_freq = [0; 256];
            for &b in src {
                self.lit_freq[b as usize] += 1;
            }
            self.lit_sum = downscale_stats(&mut self.lit_freq, 8, false);
            self.lit_length_freq = BASE_LL_FREQS;
            self.lit_length_sum = self.lit_length_freq.iter().sum();
            self.match_length_freq = [1; 53];
            self.match_length_sum = 53;
            self.off_code_freq = BASE_OFF_FREQS;
            self.off_code_sum = self.off_code_freq.iter().sum();
        } else {
            self.lit_sum = scale_stats(&mut self.lit_freq, 12);
            self.lit_length_sum = scale_stats(&mut self.lit_length_freq, 11);
            self.match_length_sum = scale_stats(&mut self.match_length_freq, 11);
            self.off_code_sum = scale_stats(&mut self.off_code_freq, 11);
        }
        self.set_base_prices::<ULTRA>();
    }

    /// `ZSTD_updateStats`: fold one emitted sequence into the statistics.
    fn update_stats(&mut self, lit_length: u32, literals: &[u8], off_base: u32, match_length: u32) {
        for &b in literals {
            self.lit_freq[b as usize] += LITFREQ_ADD;
        }
        self.lit_sum += lit_length * LITFREQ_ADD;
        let (ll_code, ..) = encode_literal_length(lit_length);
        self.lit_length_freq[ll_code as usize] += 1;
        self.lit_length_sum += 1;
        let off_code = off_base.ilog2();
        self.off_code_freq[off_code as usize] += 1;
        self.off_code_sum += 1;
        let (ml_code, ..) = encode_match_len(match_length);
        self.match_length_freq[ml_code as usize] += 1;
        self.match_length_sum += 1;
    }

    /// Price of one literal byte (`ZSTD_rawLiteralsCost` for length 1).
    #[inline]
    fn lit_price<const ULTRA: bool>(&self, lit: u8) -> u32 {
        if self.predef {
            return 6 * BITCOST_MULTIPLIER;
        }
        let price = weight::<ULTRA>(self.lit_freq[lit as usize]);
        let max = self.lit_base_price - BITCOST_MULTIPLIER;
        self.lit_base_price - price.min(max)
    }

    /// Price of the literal-length code (`ZSTD_litLengthPrice`).
    #[inline]
    fn lit_length_price<const ULTRA: bool>(&self, lit_length: u32) -> u32 {
        if self.predef {
            return weight::<ULTRA>(lit_length);
        }
        let max_len = crate::common::MAX_BLOCK_SIZE;
        if lit_length == max_len {
            // Not representable in the format; price as one bit over the max.
            return BITCOST_MULTIPLIER + self.lit_length_price::<ULTRA>(max_len - 1);
        }
        let (code, _, nb) = encode_literal_length(lit_length);
        nb as u32 * BITCOST_MULTIPLIER + self.lit_length_base_price
            - weight::<ULTRA>(self.lit_length_freq[code as usize])
    }

    /// Price of the match half of a sequence (`ZSTD_getMatchPrice`).
    #[inline]
    fn match_price<const ULTRA: bool>(&self, off_base: u32, match_length: u32) -> u32 {
        let off_code = off_base.ilog2();
        if self.predef {
            return weight::<ULTRA>(match_length - 3) + (16 + off_code) * BITCOST_MULTIPLIER;
        }
        let mut price = off_code * BITCOST_MULTIPLIER + self.off_code_base_price
            - weight::<ULTRA>(self.off_code_freq[off_code as usize]);
        // Non-ultra handicaps long offsets to favor decode cache locality.
        if !ULTRA && off_code >= 20 {
            price += (off_code - 19) * 2 * BITCOST_MULTIPLIER;
        }
        let (ml_code, _, nb) = encode_match_len(match_length);
        price += nb as u32 * BITCOST_MULTIPLIER + self.match_length_base_price
            - weight::<ULTRA>(self.match_length_freq[ml_code as usize]);
        // Heuristic: make matches slightly costlier to favor fewer sequences.
        price + BITCOST_MULTIPLIER / 5
    }
}

/// Read 4 window bytes (same contract as the match_generator helpers).
#[inline(always)]
fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: callers only read positions with 4 bytes inside the window.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
}

/// Read 8 window bytes.
#[inline(always)]
fn read8(win: &[u8], idx: usize) -> u64 {
    // SAFETY: callers only read positions with 8 bytes inside the window.
    unsafe { win.as_ptr().add(idx).cast::<u64>().read_unaligned() }
}

/// libzstd's 3-byte hash (`ZSTD_hash3`: low 24 bits times prime3bytes).
#[inline(always)]
fn hash3_at(win: &[u8], idx: usize, log: u32) -> usize {
    debug_assert!((1..32).contains(&log));
    (((read4(win, idx) << 8).wrapping_mul(506832829)) >> (32 - log)) as usize
}

/// libzstd's 4-byte hash (`ZSTD_hash4`).
#[inline(always)]
fn hash4_at(win: &[u8], idx: usize, log: u32) -> usize {
    debug_assert!((1..32).contains(&log));
    ((read4(win, idx).wrapping_mul(2654435761)) >> (32 - log)) as usize
}

/// libzstd's 5-byte hash (`ZSTD_hash5`: low 40 bits times prime5bytes).
#[inline(always)]
fn hash5_at(win: &[u8], idx: usize, log: u32) -> usize {
    debug_assert!((1..64).contains(&log));
    let v = read8(win, idx) & 0xFF_FFFF_FFFF;
    (((v << 24).wrapping_mul(889523592379)) >> (64 - log)) as usize
}

/// Common-prefix length of `win[idx..]` and `win[cand..]` given `start`
/// pre-verified bytes, bounded by window index `limit`. Returns the full
/// length from offset zero, so callers take the result as-is (libzstd's
/// `ZSTD_count` returns only the delta from its start pointers).
fn count_from(win: &[u8], idx: usize, cand: usize, start: usize, limit: usize) -> usize {
    debug_assert!(cand <= idx && idx <= limit);
    let mut len = start;
    // SAFETY: the u64 reads stay inside [.., limit) which is at or before
    // the window end; cand <= idx bounds the candidate side.
    unsafe {
        let base = win.as_ptr();
        while len + 8 <= limit - idx {
            let a = base.add(idx + len).cast::<u64>().read_unaligned();
            let b = base.add(cand + len).cast::<u64>().read_unaligned();
            if a == b {
                len += 8;
            } else {
                return len + ((a ^ b).trailing_zeros() >> 3) as usize;
            }
        }
    }
    while len < limit - idx && win[idx + len] == win[cand + len] {
        len += 1;
    }
    len
}

/// Binary-tree match finder plus the per-block constants (`ZSTD_insertBt1` /
/// `ZSTD_insertBtAndGetAllMatches`).
struct Finder<'a, 'b> {
    win: &'a [u8],
    win_base: u64,
    block_end_idx: usize,
    epoch: u64,
    tag: u64,
    max_window: u64,
    table: &'b mut [u64],
    table_log: u32,
    bt: &'b mut [u64],
    /// Position mask for the ring (which holds 2 slots per position).
    bt_mask: usize,
    hash3: &'b mut [u64],
    hash3_log: u32,
    min_match: usize,
    mls: usize,
    nb_compares: usize,
    sufficient_len: usize,
    next_update: &'b mut u64,
}

impl Finder<'_, '_> {
    /// Lowest absolute position usable as a candidate when scanning `pos`.
    #[inline]
    fn cand_floor(&self, pos: u64) -> u64 {
        pos.saturating_sub(self.max_window).max(self.win_base)
    }

    /// Resolve a table entry to an absolute candidate position in
    /// `[floor, pos)`; anything else ends the walk.
    #[inline]
    fn resolve(&self, entry: u64, floor: u64, pos: u64) -> Option<u64> {
        let cand = entry & POS_MASK;
        if (entry >> 48 == self.epoch) && cand >= floor && cand < pos {
            Some(cand)
        } else {
            None
        }
    }

    #[inline(always)]
    fn hash_main(&self, idx: usize) -> usize {
        match self.mls {
            3 => hash3_at(self.win, idx, self.table_log),
            4 => hash4_at(self.win, idx, self.table_log),
            _ => hash5_at(self.win, idx, self.table_log),
        }
    }

    /// Insert position `idx` into the tree, threading its node among up to
    /// `nb_compares` candidates. Returns how many following positions the
    /// caller may skip (indexing the interior of long matches is redundant).
    fn insert_bt1(&mut self, idx: usize) -> usize {
        let pos = self.win_base + idx as u64;
        let floor = self.cand_floor(pos);
        let h = self.hash_main(idx);
        let mut cand_abs = self.resolve(self.table[h], floor, pos);
        let mut common_smaller = 0usize;
        let mut common_larger = 0usize;
        let mut best_len = 8usize;
        let mut match_end = pos + 9;
        let mut nb = self.nb_compares;
        let bt_low = pos.saturating_sub((self.bt.len() / 2) as u64);
        let mut dummy = EMPTY;
        // Insert-side counts cap at the DP window: the parser can only
        // exploit matches up to OPT_NUM positions, so a longer common prefix
        // cannot improve the tree's usefulness here. Without the cap,
        // re-filling a region the parser skipped (match interiors at far
        // offsets) re-counts hundreds of KB per candidate against stale
        // heads. Search-side counts stay exact; a cap hit breaks the walk
        // with the same "equal tail" semantics as the block-end hit.
        let count_limit = self.block_end_idx.min(idx + OPT_NUM);
        // SAFETY: h is masked to the table size and bt slots to the ring by
        // construction; window reads stay inside the block (count_from caps
        // at count_limit and the equal-tail case breaks before the byte
        // compare). Verified by debug_asserts at the call sites.
        unsafe {
            *self.table.get_unchecked_mut(h) = self.tag | pos;
            let mut smaller: *mut u64 = self.bt.as_mut_ptr().add(2 * (pos as usize & self.bt_mask));
            let mut larger: *mut u64 = smaller.add(1);
            while let Some(ca) = cand_abs {
                if nb == 0 {
                    break;
                }
                nb -= 1;
                let cand = (ca - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, idx, cand, ml, count_limit);
                if ml > best_len {
                    best_len = ml;
                    if ca + ml as u64 > match_end {
                        match_end = ca + ml as u64;
                    }
                }
                // An equal tail cannot order the candidate; stop to keep the
                // tree consistent. A cap hit is just as unordered.
                if idx + ml >= count_limit {
                    break;
                }
                if *self.win.get_unchecked(cand + ml) < *self.win.get_unchecked(idx + ml) {
                    *smaller = self.tag | ca;
                    common_smaller = ml;
                    if ca <= bt_low {
                        smaller = &mut dummy;
                        break;
                    }
                    let node = 2 * (ca as usize & self.bt_mask);
                    smaller = self.bt.as_mut_ptr().add(node + 1);
                    cand_abs = self.resolve(*self.bt.get_unchecked(node + 1), floor, pos);
                } else {
                    *larger = self.tag | ca;
                    common_larger = ml;
                    if ca <= bt_low {
                        larger = &mut dummy;
                        break;
                    }
                    let node = 2 * (ca as usize & self.bt_mask);
                    larger = self.bt.as_mut_ptr().add(node);
                    cand_abs = self.resolve(*self.bt.get_unchecked(node), floor, pos);
                }
            }
            *smaller = EMPTY;
            *larger = EMPTY;
        }
        let positions = if best_len > 384 {
            (best_len - 384).min(192)
        } else {
            0
        };
        positions.max((match_end - (pos + 8)) as usize)
    }

    /// Fill the tree with every position in `[next_update, target)`.
    fn update_tree(&mut self, target_idx: usize) {
        debug_assert!(target_idx + HASH_READ <= self.block_end_idx);
        let mut idx = ((*self.next_update).max(self.win_base) - self.win_base) as usize;
        while idx < target_idx {
            let forward = self.insert_bt1(idx).max(1);
            idx += forward;
        }
        *self.next_update = self.win_base + target_idx as u64;
    }

    /// Fill `hash3` up to `idx` and return its newest occupant
    /// (`ZSTD_insertAndFindFirstIndexHash3`).
    fn find_hash3(&mut self, idx: usize, cursor: &mut u64) -> Option<u64> {
        debug_assert!(idx + HASH_READ <= self.block_end_idx);
        let target = self.win_base + idx as u64;
        let mut p = (*cursor).max(self.win_base);
        let mask = self.hash3.len() - 1;
        // SAFETY: p is a live window position; the hash masks to the table.
        unsafe {
            while p < target {
                let h = hash3_at(self.win, (p - self.win_base) as usize, self.hash3_log) & mask;
                *self.hash3.get_unchecked_mut(h) = self.tag | p;
                p += 1;
            }
        }
        *cursor = target;
        let h = hash3_at(self.win, idx, self.hash3_log) & mask;
        let entry = self.hash3[h];
        (entry >> 48 == self.epoch).then_some(entry & POS_MASK)
    }

    /// Collect every candidate at `idx` whose length strictly improves on the
    /// previous one, repcodes first (`ZSTD_insertBtAndGetAllMatches`).
    /// Returns the match count; `length_to_beat` starts at `min_match`.
    #[allow(clippy::too_many_arguments)]
    fn get_all_matches(
        &mut self,
        matches: &mut [Match],
        idx: usize,
        rep: &[u32; 3],
        ll0: u32,
        length_to_beat: usize,
        next_update3: &mut u64,
        rep_gated: bool,
    ) -> usize {
        debug_assert!(idx + HASH_READ <= self.block_end_idx);
        let pos = self.win_base + idx as u64;
        let floor = self.cand_floor(pos);
        let mut best_len = length_to_beat - 1;
        let mut mnum = 0usize;

        // Repeated-offset candidates (the cheapest offsets to encode).
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
                // `ZSTD_readMINMATCH`: for width 3 both sides compare their
                // first three bytes (the low 24 bits of the u32 read).
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

        // Small-match table (Ultra's min_match == 3 only).
        if self.min_match == 3 && best_len < 3 && !self.hash3.is_empty() {
            if let Some(cand_abs) = self.find_hash3(idx, next_update3) {
                if cand_abs >= floor && pos - cand_abs < (1 << 18) {
                    let cand = (cand_abs - self.win_base) as usize;
                    let mlen = count_from(self.win, idx, cand, 0, self.block_end_idx);
                    if mlen >= 3 {
                        matches[mnum] = Match {
                            off: (pos - cand_abs + 3) as u32,
                            len: mlen as u32,
                        };
                        mnum += 1;
                        best_len = mlen;
                        if mlen > self.sufficient_len || idx + mlen >= self.block_end_idx {
                            *self.next_update = pos + 1;
                            return mnum;
                        }
                    }
                }
            }
        }

        // Tree search.
        let h = self.hash_main(idx);
        let mut cand_abs = self.resolve(self.table[h], floor, pos);
        let mut common_smaller = 0usize;
        let mut common_larger = 0usize;
        let mut match_end = pos + 9;
        let mut nb = self.nb_compares;
        let bt_low = pos.saturating_sub((self.bt.len() / 2) as u64);
        let mut dummy = EMPTY;
        // SAFETY: same invariants as insert_bt1.
        unsafe {
            *self.table.get_unchecked_mut(h) = self.tag | pos;
            let mut smaller: *mut u64 = self.bt.as_mut_ptr().add(2 * (pos as usize & self.bt_mask));
            let mut larger: *mut u64 = smaller.add(1);
            while let Some(ca) = cand_abs {
                if nb == 0 {
                    break;
                }
                nb -= 1;
                let cand = (ca - self.win_base) as usize;
                let mut ml = common_smaller.min(common_larger);
                ml = count_from(self.win, idx, cand, ml, self.block_end_idx);
                if ml > best_len {
                    if ca + ml as u64 > match_end {
                        match_end = ca + ml as u64;
                    }
                    best_len = ml;
                    matches[mnum] = Match {
                        off: (pos - ca + 3) as u32,
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
                if *self.win.get_unchecked(cand + ml) < *self.win.get_unchecked(idx + ml) {
                    *smaller = self.tag | ca;
                    common_smaller = ml;
                    if ca <= bt_low {
                        smaller = &mut dummy;
                        break;
                    }
                    let node = 2 * (ca as usize & self.bt_mask);
                    smaller = self.bt.as_mut_ptr().add(node + 1);
                    cand_abs = self.resolve(*self.bt.get_unchecked(node + 1), floor, pos);
                } else {
                    *larger = self.tag | ca;
                    common_larger = ml;
                    if ca <= bt_low {
                        larger = &mut dummy;
                        break;
                    }
                    let node = 2 * (ca as usize & self.bt_mask);
                    larger = self.bt.as_mut_ptr().add(node);
                    cand_abs = self.resolve(*self.bt.get_unchecked(node), floor, pos);
                }
            }
            *smaller = EMPTY;
            *larger = EMPTY;
        }
        // Skip re-indexing the interior of long repetitive stretches.
        *self.next_update = match_end - 8;
        mnum
    }
}

/// One full block parse. Sequences append to `literals`/`seqs` exactly like
/// the other strategies; `rep` and `rep_pending` follow the emitted stream.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_block<const ULTRA: bool>(
    knobs: &OptKnobs,
    win: &[u8],
    win_base: u64,
    block_start: u64,
    block_end: u64,
    max_window: u64,
    epoch: &mut u64,
    table: &mut [u64],
    bt: &mut [u64],
    hash3: &mut [u64],
    next_update: &mut u64,
    state: &mut OptState,
    scratch: &mut OptScratch,
    rep: &mut [u32; 3],
    rep_pending: &mut u8,
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) {
    let block_len = (block_end - block_start) as usize;
    // btultra2: seed statistics with a throwaway parse of the first block.
    if ULTRA && state.lit_length_sum == 0 && block_start == 0 && block_len > PREDEF_THRESHOLD {
        let saved_rep = *rep;
        let saved_pending = *rep_pending;
        run_once::<ULTRA>(
            knobs,
            win,
            win_base,
            block_start,
            block_end,
            max_window,
            *epoch,
            table,
            bt,
            hash3,
            next_update,
            state,
            scratch,
            rep,
            rep_pending,
            literals,
            seqs,
        );
        *rep = saved_rep;
        *rep_pending = saved_pending;
        literals.clear();
        seqs.clear();
        // Drop the pass-1 tree (libzstd rewinds its window limits instead; the
        // epoch tag achieves the same invalidation for free).
        *epoch += 1;
        if *epoch > 0xFFFF {
            table.fill(EMPTY);
            bt.fill(EMPTY);
            hash3.fill(EMPTY);
            *epoch = 1;
        }
        *next_update = block_start;
    }
    run_once::<ULTRA>(
        knobs,
        win,
        win_base,
        block_start,
        block_end,
        max_window,
        *epoch,
        table,
        bt,
        hash3,
        next_update,
        state,
        scratch,
        rep,
        rep_pending,
        literals,
        seqs,
    );
}

/// The caller guarantees the tables match the knobs' logs, the window covers
/// `[win_base, block_end]`, and `bt.len() >= 2`.
#[allow(clippy::too_many_arguments)]
fn run_once<const ULTRA: bool>(
    knobs: &OptKnobs,
    win: &[u8],
    win_base: u64,
    block_start: u64,
    block_end: u64,
    max_window: u64,
    epoch: u64,
    table: &mut [u64],
    bt: &mut [u64],
    hash3: &mut [u64],
    next_update: &mut u64,
    state: &mut OptState,
    scratch: &mut OptScratch,
    rep: &mut [u32; 3],
    rep_pending: &mut u8,
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) {
    let block_end_idx = (block_end - win_base) as usize;
    let ilimit_idx = block_end_idx.saturating_sub(8);
    state.rescale_freqs::<ULTRA>(&win[(block_start - win_base) as usize..block_end_idx]);

    let table_log = table.len().trailing_zeros();
    let bt_mask = bt.len() / 2 - 1;
    let mut finder = Finder {
        win,
        win_base,
        block_end_idx,
        epoch,
        tag: epoch << 48,
        max_window,
        table,
        table_log,
        bt,
        bt_mask,
        hash3,
        hash3_log: knobs.hash3_log,
        min_match: knobs.min_match as usize,
        mls: knobs.mls as usize,
        nb_compares: 1usize << knobs.search_log,
        sufficient_len: knobs.sufficient_len as usize,
        next_update,
    };
    let mut next_update3 = *finder.next_update;

    // The very first frame position has nothing behind it.
    let mut pos = block_start + (block_start == 0) as u64;
    let mut anchor = block_start;
    let opt = &mut scratch.opt[..];
    let matches = &mut scratch.matches[..];
    let sufficient_len = knobs.sufficient_len as usize;
    let min_match = knobs.min_match as usize;

    // The outer loop runs one DP series per match group; `pos` jumps to the
    // end of every emitted sequence.
    while pos + (HASH_READ as u64) < block_end {
        let idx = (pos - win_base) as usize;
        let litlen = (pos - anchor) as u32;
        let ll0 = (litlen == 0) as u32;
        // idx + HASH_READ stays inside the block by the loop guard.
        let nb = if pos < *finder.next_update {
            0 // Skipped area (interior of a long match).
        } else {
            finder.update_tree(idx);
            finder.get_all_matches(
                matches,
                idx,
                rep,
                ll0,
                min_match,
                &mut next_update3,
                *rep_pending != 0,
            )
        };
        if nb == 0 {
            pos += 1;
            continue;
        }

        opt[0] = Optimal {
            price: state.lit_length_price::<ULTRA>(litlen),
            off: 0,
            mlen: 0,
            litlen,
            rep: *rep,
        };

        let max_ml = matches[nb - 1].len as usize;
        let mut last_stretch = Optimal::default();
        let mut cur: usize;
        let mut last_pos: usize;
        let mut jumped = false;

        if max_ml > sufficient_len {
            // Large match: encode immediately, skip the DP.
            last_stretch.litlen = 0;
            last_stretch.mlen = max_ml as u32;
            last_stretch.off = matches[nb - 1].off;
            cur = 0;
            last_pos = max_ml;
        } else {
            // Price the first match's coverage span.
            let mut pos_i = 1usize;
            while pos_i < min_match {
                opt[pos_i].price = MAX_PRICE;
                opt[pos_i].mlen = 0;
                opt[pos_i].litlen = litlen + pos_i as u32;
                pos_i += 1;
            }
            for &cand in &matches[..nb] {
                let off = cand.off;
                let end = cand.len as usize;
                while pos_i <= end {
                    let price = opt[0].price + state.match_price::<ULTRA>(off, pos_i as u32);
                    opt[pos_i] = Optimal {
                        price: price + state.lit_length_price::<ULTRA>(0),
                        off,
                        mlen: pos_i as u32,
                        litlen: 0,
                        rep: [1, 4, 8],
                    };
                    pos_i += 1;
                }
            }
            last_pos = pos_i - 1;
            opt[pos_i].price = MAX_PRICE;

            // Extend the DP across further positions.
            cur = 1;
            while cur <= last_pos {
                let idx_cur = idx + cur;

                // Fix the current position with one literal if cheaper.
                {
                    let litlen_l = opt[cur - 1].litlen + 1;
                    let price = opt[cur - 1].price as i64
                        + state.lit_price::<ULTRA>(win[idx_cur - 1]) as i64
                        + ll_inc_price::<ULTRA>(state, litlen_l);
                    if price <= opt[cur].price as i64 {
                        let prev_match = opt[cur];
                        opt[cur] = opt[cur - 1];
                        opt[cur].litlen = litlen_l;
                        opt[cur].price = price as u32;
                        // btultra: a match ending right here followed by one
                        // literal can beat the all-literal path.
                        if ULTRA
                            && prev_match.litlen == 0
                            && ll_inc_price::<ULTRA>(state, 1) < 0
                            && pos + cur as u64 + 1 < block_end
                        {
                            let with1 = prev_match.price as i64
                                + state.lit_price::<ULTRA>(win[idx_cur]) as i64
                                + ll_inc_price::<ULTRA>(state, 1);
                            let with_more = price
                                + state.lit_price::<ULTRA>(win[idx_cur]) as i64
                                + ll_inc_price::<ULTRA>(state, litlen_l + 1);
                            if with1 < with_more && (with1 as u32) < opt[cur + 1].price {
                                let prev = cur - prev_match.mlen as usize;
                                let reps =
                                    new_rep(&opt[prev].rep, prev_match.off, opt[prev].litlen == 0);
                                opt[cur + 1] = prev_match;
                                opt[cur + 1].rep = reps;
                                opt[cur + 1].litlen = 1;
                                opt[cur + 1].price = with1 as u32;
                                if last_pos < cur + 1 {
                                    last_pos = cur + 1;
                                }
                            }
                        }
                    }
                }

                // A stretch ending on a match rewrites the offset history.
                if opt[cur].litlen == 0 {
                    let prev = cur - opt[cur].mlen as usize;
                    opt[cur].rep = new_rep(&opt[prev].rep, opt[cur].off, opt[prev].litlen == 0);
                }

                if idx_cur > ilimit_idx {
                    cur += 1;
                    continue;
                }
                if cur == last_pos {
                    break;
                }
                // btopt: skip positions the next entry already beats.
                if !ULTRA && opt[cur + 1].price <= opt[cur].price + BITCOST_MULTIPLIER / 2 {
                    cur += 1;
                    continue;
                }

                let ll0c = (opt[cur].litlen == 0) as u32;
                let base_price = opt[cur].price + state.lit_length_price::<ULTRA>(0);
                // idx_cur <= ilimit_idx keeps the search 8 bytes inside the
                // block end.
                let nb2 = if pos + (cur as u64) < *finder.next_update {
                    0
                } else {
                    finder.update_tree(idx_cur);
                    finder.get_all_matches(
                        matches,
                        idx_cur,
                        &opt[cur].rep,
                        ll0c,
                        min_match,
                        &mut next_update3,
                        *rep_pending != 0,
                    )
                };
                if nb2 == 0 {
                    cur += 1;
                    continue;
                }
                let longest = matches[nb2 - 1].len as usize;
                if longest > sufficient_len
                    || cur + longest >= OPT_NUM
                    || pos + (cur + longest) as u64 >= block_end
                {
                    last_stretch.litlen = 0;
                    last_stretch.mlen = longest as u32;
                    last_stretch.off = matches[nb2 - 1].off;
                    last_pos = cur + longest;
                    jumped = true;
                    break;
                }

                for m in 0..nb2 {
                    let off = matches[m].off;
                    let last_ml = matches[m].len as usize;
                    let start_ml = if m > 0 {
                        matches[m - 1].len as usize + 1
                    } else {
                        min_match
                    };
                    let mut mlen = last_ml;
                    while mlen >= start_ml {
                        let pos2 = cur + mlen;
                        let price = base_price + state.match_price::<ULTRA>(off, mlen as u32);
                        if pos2 > last_pos || price < opt[pos2].price {
                            while last_pos < pos2 {
                                last_pos += 1;
                                opt[last_pos].price = MAX_PRICE;
                                opt[last_pos].litlen = 1;
                            }
                            opt[pos2] = Optimal {
                                price,
                                off,
                                mlen: mlen as u32,
                                litlen: 0,
                                rep: [1, 4, 8],
                            };
                        } else if !ULTRA {
                            // btopt early abort: longer variants of this match
                            // cannot win either.
                            break;
                        }
                        mlen -= 1;
                    }
                }
                opt[last_pos + 1].price = MAX_PRICE;
                cur += 1;
            }
            if !jumped {
                last_stretch = opt[last_pos];
                cur = last_pos - last_stretch.mlen as usize;
            }
        }

        // ---- shortest path (libzstd's _shortestPath) ----
        debug_assert!(opt[0].mlen == 0);
        debug_assert!(last_pos >= last_stretch.mlen as usize);
        debug_assert_eq!(cur, last_pos - last_stretch.mlen as usize);
        if last_stretch.mlen == 0 {
            // All matches converted to literals; retry from the far end.
            pos += last_pos as u64;
            continue;
        }
        // The offset history updates flow through push_seq_packed during
        // emission (mirroring the decoder sequence by sequence); the path
        // already carries the rep state per position for the DP itself.
        if last_stretch.litlen != 0 {
            cur -= last_stretch.litlen as usize;
        }

        // Convert stretches (match + literals) into sequences (literals +
        // match) by walking the path backwards, overwriting opt in place.
        let store_end = cur + 2;
        debug_assert!(store_end < OPT_SIZE);
        opt[store_end] = last_stretch;
        let mut store_start = store_end;
        let mut stretch_pos = cur;
        loop {
            let next_stretch = opt[stretch_pos];
            opt[store_start].litlen = next_stretch.litlen;
            if next_stretch.mlen == 0 {
                break;
            }
            store_start -= 1;
            opt[store_start] = next_stretch;
            stretch_pos -= (next_stretch.litlen + next_stretch.mlen) as usize;
        }
        for entry in &opt[store_start..=store_end] {
            let litlen_s = entry.litlen;
            let mlen_s = entry.mlen;
            let off_s = entry.off;
            if mlen_s == 0 {
                // Leading literals of the series: not a sequence.
                pos = anchor + litlen_s as u64;
                continue;
            }
            let anchor_idx = (anchor - win_base) as usize;
            let start_idx = anchor_idx + litlen_s as usize;
            state.update_stats(litlen_s, &win[anchor_idx..start_idx], off_s, mlen_s);
            let (_, match_end_idx) = push_seq_packed(
                win,
                win_base,
                anchor,
                start_idx,
                mlen_s as usize,
                off_s,
                rep,
                literals,
                seqs,
            );
            if *rep_pending != 0 && off_s > 3 {
                *rep_pending -= 1;
            }
            anchor = win_base + match_end_idx as u64;
            pos = anchor;
        }
        state.set_base_prices::<ULTRA>();
    }

    if !seqs.is_empty() && anchor < block_end {
        let tail = (anchor - win_base) as usize..block_end_idx;
        literals.extend_from_slice(&win[tail]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_match_reference_points() {
        // ZSTD_bitWeight: highbit32(stat+1) << 8; stat 0 and 1 both map to
        // highbit 1 == 0 bits.
        assert_eq!(bit_weight(0), 0);
        assert_eq!(bit_weight(1), 256);
        assert_eq!(bit_weight(2), 256);
        assert_eq!(bit_weight(255), 8 * 256);
        // ZSTD_fracWeight: whole part + mantissa interpolation.
        assert_eq!(frac_weight(0), 256);
        assert_eq!(frac_weight(1), 512);
        assert_eq!(frac_weight(3), 512 + 256);
    }

    #[test]
    fn scaling_keeps_sums_bounded() {
        let mut table = [7000u32; 36];
        let sum = scale_stats(&mut table, 11);
        assert!(sum <= (1 << 12) + 36, "sum {sum}");
        assert!(table.iter().all(|&v| v >= 1));
    }

    #[test]
    fn new_rep_matches_update_semantics() {
        // A literal offset rotates the history.
        assert_eq!(new_rep(&[10, 20, 30], 13, false), [10, 10, 20]);
        // repcode 1 with literals pending re-uses rep0: no change.
        assert_eq!(new_rep(&[10, 20, 30], 1, false), [10, 20, 30]);
        // repcode 2 pulls rep1 to the front.
        assert_eq!(new_rep(&[10, 20, 30], 2, false), [20, 10, 30]);
        // repcode 1 at ll0 swaps the two most recent offsets.
        assert_eq!(new_rep(&[10, 20, 30], 1, true), [20, 10, 30]);
        // repcode 3 at ll0 uses rep0 - 1.
        assert_eq!(new_rep(&[10, 20, 30], 3, true), [9, 10, 20]);
    }
}
