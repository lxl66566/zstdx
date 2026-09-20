//! The tagged row-matcher strategy loop (rows 5-12, libzstd's
//! `ZSTD_RowFindBestMatch` storage under this crate's chain selection
//! semantics) — a method of [`super::MatchGeneratorDriver`], split out of
//! the driver file for size. Field and helper access resolves through the
//! parent module.
//!
//! Layout: the head table is an array of rows of `1 << ROW_LOG` u32
//! entries, each entry packing the tag into its top byte and the biased
//! position's low 24 bits below (`(tag << 24) | ((pos + 1) & MASK24)`; 0
//! is the never-written sentinel), the slot chosen by the position's low
//! bits — a single store per insert and a single cache line per search,
//! with every candidate position known without a dependent load: the
//! latency mechanism the chain's link chase (~66 cyc/step through a
//! 4-6 MiB two-table working set) cannot reach. This deviates from
//! libzstd's layout twice: the tag is packed into the entry (their SSE
//! compare needs contiguous tag bytes; extracting packed tags through
//! `srli+cmpeq+movemask` costs two vector ops and saves the second line
//! on every search, insert and covered-fill store). The cycling
//! insertion head is libzstd's own (`ZSTD_row_nextIndex`), kept in a
//! parallel one-byte-per-row array instead of their tag-row byte 0: the
//! array is 32-64 KiB (L2-resident against the multi-MiB entry table)
//! and off the entry line, so the fill loop's head load never serializes
//! behind its own cold-line stores, and no slot is burned on head
//! storage (libzstd's rows hold 15 of 16; ours hold all 16). 24-bit
//! positions wrap every 16 MiB; the rebuild against the scanning
//! position admits the unique candidate in `[pos - 2^24, pos)`, which the
//! window check (≤ 4 MiB) arbitrates.

use super::*;

/// Tag width carved out of the row hash (libzstd's `ZSTD_ROW_HASH_TAG_BITS`).
pub(super) const ROW_TAG_BITS: u32 = 8;
pub(super) const ROW_TAG_MASK: u32 = (1 << ROW_TAG_BITS) - 1;
/// The packed entry's position width: 24 bits, wrapping every 16 MiB
/// (see [`row_unpack`]).
pub(super) const POS24_MASK: u32 = 0x00ff_ffff;

/// Full row hash of the `width`-byte window at `idx`: the top
/// `hash_log - ROW_LOG + ROW_TAG_BITS` bits of the width hash, as a u32 —
/// the bits above the tag select the row (`>> ROW_TAG_BITS << ROW_LOG`
/// stays below `1 << hash_log` for any `ROW_LOG <= hash_log`, which is
/// what keeps insert and search inside the tables).
#[inline(always)]
pub(super) fn row_hash_at(
    win: &[u8],
    idx: usize,
    hash_log: u32,
    row_log: u32,
    width: ChainHashWidth,
) -> u32 {
    let hbits = hash_log - row_log + ROW_TAG_BITS;
    debug_assert!(hbits <= 32);
    // SAFETY: same read contract as hash_at_width (HASH_READ bytes of
    // window ahead of idx; the callers bound-check once per loop).
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & width.mask();
        (v.wrapping_mul(HASH_PRIME) >> (64 - hbits)) as u32
    }
}

/// L1 prefetch hint (no-op off x86_64).
#[inline(always)]
unsafe fn prefetch_l1(p: *const u8) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: prefetch is a hint, legal on any addressable pointer.
        unsafe { core::arch::x86_64::_mm_prefetch(p.cast(), core::arch::x86_64::_MM_HINT_T0) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = p;
    }
}

/// Prefetch the row `hash` selects (the scan loop's pipelining: issued
/// one search ahead, under the incumbent's shadow).
#[inline(always)]
pub(super) fn row_prefetch<const ROW_LOG: u32>(table: *const u32, hash: u32) {
    let rel_row = ((hash >> ROW_TAG_BITS) << ROW_LOG) as usize;
    // SAFETY: rel_row + entries stays inside the table (row_hash_at's
    // width derivation); prefetch tolerates any address.
    unsafe {
        prefetch_l1(table.add(rel_row).cast());
        if ROW_LOG >= 5 {
            prefetch_l1(table.add(rel_row + 16).cast());
        }
        if ROW_LOG >= 6 {
            prefetch_l1(table.add(rel_row + 32).cast());
            prefetch_l1(table.add(rel_row + 48).cast());
        }
    }
}

/// Pack a row entry: the tag in the top byte, the position's biased low
/// 24 bits below. A position ≡ `2^24 - 1` packs its low bits to zero and
/// reads as the sentinel — one dead insert per 16 MiB wrap.
#[inline(always)]
pub(super) fn row_entry(hash: u32, abs: u64) -> u32 {
    ((hash & ROW_TAG_MASK) << 24) | (((abs as u32).wrapping_add(1)) & POS24_MASK)
}

/// Match mask of `tag` against the row's packed entries (libzstd's
/// `ZSTD_row_getMatchMask` without the head rotation — slots are
/// position-indexed here): bit i is set iff entry i's tag equals `tag`.
/// x86_64: one SSE2 load+shift+compare+movemask per FOUR entries — a
/// chunk of four packed u32s yields one bit per entry by comparing whole
/// lanes (the shift puts the tag in the lane's low byte and zeroes the
/// rest) and taking the lane sign bit, so `entries / 4` chunks cover the
/// row (baseline ISA, no runtime detection); otherwise a scalar
/// fallback with identical semantics. A byte-granular movemask over the
/// shifted lanes was this port's founding trap: it returns one bit per
/// BYTE with the entry's bit at stride 4, so reading the bit index as a
/// slot index searched only entries 0/4/8/12 and tag-verified only
/// entry 0 — three quarters of every row invisible (text.l5 +42% size).
#[inline(always)]
pub(super) fn row_match_mask<const ROW_LOG: u32>(row: *const u32, tag: u8) -> u64 {
    let entries = 1usize << ROW_LOG;
    let mut m: u64 = 0;
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        // SAFETY: the row spans `entries` u32 slots (the caller's bounds).
        let want = unsafe { _mm_set1_epi32(tag as i32) };
        for c in (0..entries / 4).rev() {
            let chunk = unsafe { _mm_loadu_si128(row.add(c * 4).cast()) };
            let tags = unsafe { _mm_srli_epi32(chunk, 24) };
            let eq = unsafe { _mm_cmpeq_epi32(tags, want) };
            m = (m << 4)
                | unsafe { _mm_movemask_ps(core::mem::transmute::<__m128i, __m128>(eq)) } as u64;
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // SAFETY: same span as above.
        for i in 0..entries {
            m |= u64::from((unsafe { *row.add(i) } >> 24) as u8 == tag) << i;
        }
    }
    m
}

/// Row search from the position's full hash, returning the longest
/// match's (length, candidate window index) — `(0, usize::MAX)` when
/// nothing matched. The tag mask filters the row's entries first; the
/// survivors are collected with their window lines prefetched (libzstd's
/// `matchBuffer`: a candidate's beat-check read is a random line in the
/// multi-MiB window, and issuing the loads together lets them overlap
/// instead of each serializing behind the loop's own dependent chain —
/// an inline verify measured json.l7 x2.0 vs libzstd's row), then
/// verified exactly like the chain walk (beat-check + extend). Slots are
/// position-indexed (see row_insert), so the collection has no age
/// order; they are few (the tag is 8 bits of the same hash that selected
/// the row), and the buffer caps at the row width (rows 5-8 run
/// 8/8/15/15 attempts, rows 10-12 run 31/63/63 — the width-minus-one cap
/// matches libzstd's effective candidate count, whose tag-row byte 0
/// is the insertion head).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn row_search<const ROW_LOG: u32, const RAMPED: bool>(
    win: &[u8],
    table: *const u32,
    heads: *const u8,
    idx: usize,
    hash: u32,
    win_base: u64,
    block_end: u64,
    attempts: usize,
    max_window: u64,
    ramp: RampGate,
) -> (usize, usize) {
    // The candidate buffer spans the row (one u32 per slot; 64 entries
    // = 256 B at ROW_LOG 6). An array length cannot key off a const
    // generic through arithmetic (generic_const_exprs), so the shipped
    // widths materialize through this compile-time dispatch — each
    // ROW_LOG instantiation carries exactly its row.
    if ROW_LOG >= 6 {
        row_search_cap::<ROW_LOG, 64, RAMPED>(
            win, table, heads, idx, hash, win_base, block_end, attempts, max_window, ramp,
        )
    } else if ROW_LOG >= 5 {
        row_search_cap::<ROW_LOG, 32, RAMPED>(
            win, table, heads, idx, hash, win_base, block_end, attempts, max_window, ramp,
        )
    } else {
        row_search_cap::<ROW_LOG, 16, RAMPED>(
            win, table, heads, idx, hash, win_base, block_end, attempts, max_window, ramp,
        )
    }
}

/// [`row_search`]'s body at a fixed candidate-buffer width.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn row_search_cap<const ROW_LOG: u32, const CAND_CAP: usize, const RAMPED: bool>(
    win: &[u8],
    table: *const u32,
    heads: *const u8,
    idx: usize,
    hash: u32,
    win_base: u64,
    block_end: u64,
    attempts: usize,
    max_window: u64,
    ramp: RampGate,
) -> (usize, usize) {
    let rel_row = ((hash >> ROW_TAG_BITS) << ROW_LOG) as usize;
    let row = table.wrapping_add(rel_row);
    let tag = (hash & ROW_TAG_MASK) as u8;
    let mut matches = row_match_mask::<ROW_LOG>(row, tag);
    // The head orders the row newest-first: rotate the slot-indexed mask
    // so its LSB is the head slot and map bit i back to slot (head + i) —
    // the attempts budget then spends on the newest candidates exactly as
    // libzstd's head-rotated mask does. No early break on the age walk:
    // pooled-state residue and the search-time rep-advanced insert leave
    // mixed ages in a row, and a break would cut the walk short of newer
    // in-window candidates behind one stale entry.
    let row_mask = (1usize << ROW_LOG) - 1;
    // SAFETY: the head byte is the row's own (bounds as row_insert).
    let head = unsafe { *heads.add((hash >> ROW_TAG_BITS) as usize) } as usize & row_mask;
    if head != 0 {
        // The rotated field spans exactly the row's entries. At ROW_LOG 6
        // the field is the whole u64 and `1u64 << entries` would overflow
        // (debug panic; release x86 masks the count to 0, the mask
        // collapses to 0 and the row would find nothing) — the full-row
        // case rotates without a mask instead. `head` sits in [1,
        // row_mask] here, so the other shift is always in range.
        matches = if ROW_LOG >= 6 {
            matches.rotate_right(head as u32)
        } else {
            ((matches >> head) | (matches << (row_mask + 1 - head)))
                & ((1u64 << (row_mask + 1)) - 1)
        };
    }
    let pos_abs = win_base + idx as u64;
    // Oldest usable candidate distance: within the level window and
    // inside the live window buffer. The phase-1 loop works entirely in
    // the 24-bit wrap domain: `((pos + 1) - entry) mod 2^24` IS the true
    // distance (the packing bias cancels), so the reach check and the
    // candidate rebuild are one u32 subtraction each — no 64-bit rebuild
    // per candidate. The sentinel (entry 0) is filtered explicitly: it
    // would otherwise resolve to distance `pos + 1` — in-window at the
    // frame head — and rebuild to a wrapped index just below the window
    // start. A dist of 0 (a slot holding this very position) wraps the
    // `d - 1` compare to u32::MAX and dies on the same check.
    let reach = (pos_abs - win_base).min(max_window) as u32;
    let pos32 = (idx as u32).wrapping_add(win_base as u32).wrapping_add(1);
    // Phase 1: collect the row's in-window tag matches, prefetching each
    // candidate's window line as it is found.
    let mut cands = [0u32; CAND_CAP];
    let mut n = 0usize;
    let mut remaining = attempts.min(CAND_CAP);
    while matches != 0 && remaining > 0 {
        let age = matches.trailing_zeros() as usize;
        matches &= matches - 1;
        let slot = (head + age) & row_mask;
        // SAFETY: slot ranges over [0, entries).
        let entry = unsafe { *row.add(slot) };
        if entry & POS24_MASK == 0 {
            continue;
        }
        let d = pos32.wrapping_sub(entry) & POS24_MASK;
        if d.wrapping_sub(1) >= reach {
            continue;
        }
        let cand = (idx as u32).wrapping_sub(d);
        // SAFETY: the reach check bounds the candidate below and the
        // scan's HASH_READ margin above; prefetch tolerates any address
        // regardless.
        unsafe { prefetch_l1(win.as_ptr().add(cand as usize).cast()) };
        cands[n] = cand;
        n += 1;
        remaining -= 1;
    }
    // Phase 2: verify the collected candidates (their loads are in
    // flight from the phase-1 prefetches).
    let mut best_len = 0usize;
    let mut best_cand = usize::MAX;
    for &cand in &cands[..n] {
        let cand = cand as usize;
        // Beat-check (the chain walk's "potentially better" read): the 4
        // bytes ending at best_len+1 reject most tag collisions on one
        // load. The probe stays inside the block by the same argument as
        // chain_search (still looping means best_len is short of the
        // block end).
        let probe = best_len.saturating_sub(3);
        if !(RAMPED && ramp.blocks(pos_abs, win_base + cand as u64))
            && read4(win, cand + probe) == read4(win, idx + probe)
        {
            let ml = extend_match(win, idx, cand);
            if ml > best_len {
                best_len = ml;
                best_cand = cand;
                // Cannot be improved on within this block.
                if pos_abs + ml as u64 >= block_end {
                    break;
                }
            }
        }
    }
    (best_len, best_cand)
}

/// Insert `abs` at the row `hash` selects, in the slot the cycling head
/// picks (libzstd's `ZSTD_row_nextIndex`: the byte names the last-insert
/// slot, the next insert takes its predecessor, so a full row evicts its
/// oldest entry — insertion order is position order, which is the
/// retention a fixed-capacity row needs: a repeat survives up to
/// `entries` same-bucket inserts instead of up to the first
/// same-row-same-phase collision a position-indexed slot suffers). The
/// head byte lives in a parallel one-byte-per-row array — off the entry
/// line (a same-line head costs the fill loop a load/compute/store chain
/// behind its own cold-line stores) and L2-resident at the row count.
#[inline(always)]
pub(super) fn row_insert<const ROW_LOG: u32>(table: *mut u32, heads: *mut u8, hash: u32, abs: u64) {
    let rel_row = ((hash >> ROW_TAG_BITS) << ROW_LOG) as usize;
    // SAFETY: the head index masks to the row count (row_hash_at's width
    // derivation); the entry slot masks to the row width.
    unsafe {
        let hp = heads.add((hash >> ROW_TAG_BITS) as usize);
        let slot = (*hp).wrapping_sub(1) as usize & ((1usize << ROW_LOG) - 1);
        *hp = slot as u8;
        *table.add(rel_row + slot) = row_entry(hash, abs);
    }
}

/// The covered fill's insert: [`row_insert`] — one head byte plus one
/// entry store per position, still cheaper than the chain's head+link
/// pair.
#[inline(always)]
pub(super) fn row_insert_fill<const ROW_LOG: u32>(
    table: *mut u32,
    heads: *mut u8,
    hash: u32,
    abs: u64,
) {
    row_insert::<ROW_LOG>(table, heads, hash, abs);
}

/// Shared mutable state of the row scan's emit path: the two table views
/// as raw pointers (the scan loop's SROA discipline — the driver's slice
/// borrows must not be re-derived per call), the output streams and the
/// per-block constants.
pub(super) struct RowEmit<'a> {
    pub(super) table: *mut u32,
    pub(super) heads: *mut u8,
    pub(super) literals: &'a mut Vec<u8>,
    pub(super) seqs: &'a mut Vec<SeqWord>,
    pub(super) win_base: u64,
    /// Last absolute position whose hash reads stay inside the window.
    pub(super) insert_max: u64,
    pub(super) hash_log: u32,
    pub(super) width: ChainHashWidth,
}

impl RowEmit<'_> {
    /// Emit the sequence for a match covering `match_len` bytes at window
    /// index `start` and index the covered range into the rows — libzstd's
    /// update policy (`ZSTD_row_update_internal`): dense through a 384-byte
    /// span, then past that the head 96 plus the tail 32 positions dense
    /// with the middle skipped (a row's fixed capacity makes every skipped
    /// boundary anchor a permanently lost candidate, which a stride grid
    /// skips three of four — text.l5 +0.9% size on the stride form — while
    /// the skipped middle is pure fill volume: text is 99.6% match bytes,
    /// and the skip is the difference between x0.60 and x1.47 vs libzstd
    /// there; the stride middle bought dll32.l8 -0.9pp, declined — a
    /// --file-only class against a corpus-shape speed axis).
    /// `inline(always)`: the chain's `emit_chain` lesson — an outlined
    /// emit pays its calling convention per sequence.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit<const ROW_LOG: u32>(
        &mut self,
        win: &[u8],
        anchor: u64,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        let end_abs = (self.win_base + match_end as u64).min(self.insert_max);
        let from = self.win_base + start as u64;
        let mut seg_end = end_abs;
        if end_abs - from > 384 {
            seg_end = from + 96;
        }
        let mut p = from;
        while p < seg_end {
            let i = (p - self.win_base) as usize;
            let h = row_hash_at(win, i, self.hash_log, ROW_LOG, self.width);
            row_insert_fill::<ROW_LOG>(self.table, self.heads, h, p);
            p += 1;
        }
        if seg_end < end_abs {
            p = end_abs - 32;
            while p < end_abs {
                let i = (p - self.win_base) as usize;
                let h = row_hash_at(win, i, self.hash_log, ROW_LOG, self.width);
                row_insert_fill::<ROW_LOG>(self.table, self.heads, h, p);
                p += 1;
            }
        }
        self.win_base + match_end as u64
    }

    /// The row strategy's repcode continuation (`TableEmit::rep1_chain`'s
    /// twin): probe the second repeated offset after every match; each
    /// consumed position is row-inserted (a rep chain's covered fill).
    #[inline(always)]
    pub(super) fn rep1_row<const ROW_LOG: u32>(
        &mut self,
        win: &[u8],
        pos: u64,
        block_end: u64,
        rep: &mut [u32; 3],
        ramp: RampGate,
    ) -> u64 {
        let mut pos = pos;
        while block_end - pos >= MIN_MATCH as u64 {
            let Some(cand_abs) = pos.checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            if ramp.blocks(pos, cand_abs) {
                break;
            }
            let pidx = (pos - self.win_base) as usize;
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pidx) {
                break;
            }
            let ml = extend_match(win, pidx, cand);
            debug_assert!(ml >= MIN_MATCH);
            let h = row_hash_at(win, pidx, self.hash_log, ROW_LOG, self.width);
            row_insert_fill::<ROW_LOG>(self.table, self.heads, h, pos);
            pos = self.emit::<ROW_LOG>(win, pos, pidx, ml, 1, rep);
        }
        pos
    }
}

impl MatchGeneratorDriver {
    /// The tagged row-matcher strategy loop: the chain scan's selection
    /// semantics (rep probes, the job-start seed, the literal-aware lazy
    /// walk, the store gate, backward extension, the miss ramp) over row
    /// storage — the candidate source swaps from a chain-link chase to a
    /// one-cache-line row scan.
    #[allow(clippy::too_many_lines)]
    pub(super) fn start_matching_row<const ROW_LOG: u32, const RAMPED: bool>(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let ramp = self.ramp;
        let block_end = self.block_end;
        let hash_log = self.params.hash_log;
        let attempts = (self.params.search_depth as usize).min((1usize << ROW_LOG) - 1);
        let lazy_depth = self.params.lazy_depth;
        let min_match = self.params.min_match as usize;
        let dict_row = self.dict_row;
        let width = ChainHashWidth::of(dict_row);
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // SAFETY: the scan's table/tag accesses go through raw pointers
        // (the fast loop's SROA note); derived here, before the emit
        // context below takes its borrow, and never resized during the
        // scan. The emit helpers write through the same pointers at call
        // sites where the loop holds no conflicting borrow.
        let table_ptr: *mut u32 = self.tables.as_mut_ptr();
        let heads_ptr: *mut u8 = self.row_heads.as_mut_ptr();
        let mut emit = RowEmit {
            table: table_ptr,
            heads: heads_ptr,
            literals,
            seqs,
            win_base,
            insert_max,
            hash_log,
            width,
        };
        let hash_read = HASH_READ as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut miss_count = self.miss_count;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let lit_lens = &self.lit_lens;

        // Row search from the full hash, inlined at both call sites (the
        // chain loop's chain_search discipline: an outlined search pays
        // its calling convention per position).
        let search = |win: &[u8], idx: usize, hash: u32| -> (usize, usize) {
            row_search::<ROW_LOG, RAMPED>(
                win, table_ptr, heads_ptr, idx, hash, win_base, block_end, attempts, max_window,
                ramp,
            )
        };

        // The body exists in two phases (the chain loop's pattern): the
        // seeded phase carries the job-start state, which only ever winds
        // down; the steady phase folds it out.
        macro_rules! scan_row {
            ($restart:lifetime, $gated:literal) => {
            let idx = (pos - win_base) as usize;
            let h = row_hash_at(win, idx, hash_log, ROW_LOG, width);
            // Pipelining of the next position's rows (the chain loop's
            // pre1): the tag and hash rows of the position the lazy walk
            // below usually searches first, prefetched under the incumbent
            // search's shadow (issue placement is load-bearing — the LDM
            // lesson).
            if block_end - pos > hash_read {
                let hn = row_hash_at(win, idx + 1, hash_log, ROW_LOG, width);
                row_prefetch::<ROW_LOG>(table_ptr, hn);
            }
            let (mut best_len, mut best_cand) = search(win, idx, h);

            // Repcode probe first when armed: with literals pending it
            // runs at the current position, otherwise one byte ahead so
            // that byte becomes the pending literal and of_value 1 stays
            // encodable (mirrors the chain loop).
            let mut rep_hit = false;
            if !$gated || rep_pending == 0 {
                let probe = if pos == anchor {
                    pos + 1
                } else {
                    pos
                };
                if let Some(cand_abs) = probe.checked_sub(rep[0] as u64)
                    && cand_abs >= win_base
                {
                    let pidx = (probe - win_base) as usize;
                    let cand = (cand_abs - win_base) as usize;
                    if read4(win, cand) == read4(win, pidx) {
                        let ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH
                            && ml + 3 > best_len
                            && !ramp.blocks(probe, cand_abs)
                        {
                            best_len = ml;
                            best_cand = cand;
                            rep_hit = true;
                            pos = probe;
                        }
                    }
                }
            }

            // Seed-offset probe (job starts only): the prefill-detected
            // long-repeat offset as a direct candidate, before the row's
            // candidates — a period twin can be evicted from its row by
            // newer same-bucket inserts (the eviction the chain's links
            // never pay). Mirrors the chain loop's twin block.
            let mut seed_hit = false;
            if $gated && seed_offset != 0 {
                let ci = (pos - seed_offset as u64 - win_base) as usize;
                // The rep probe may have advanced `pos` one byte past this
                // iteration's `idx`; a seed offset of exactly one then
                // resolves the candidate to `idx` itself — a self-compare
                // whose zero offset the store gate below prices. The
                // seed's twin must be strictly older than the probed
                // position.
                if ci < idx && read4(win, ci) == read4(win, idx) {
                    let ml = extend_match(win, idx, ci);
                    if ml >= 6 && ml + 3 > best_len && !ramp.blocks(pos, win_base + ci as u64) {
                        best_len = ml;
                        best_cand = ci;
                        rep_hit = false;
                        seed_hit = true;
                    }
                }
                if !seed_hit {
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }
            }

            // Insert this position behind the probe (the chain's form: the
            // possibly rep-advanced `pos` is stored at the searched
            // position's hash).
            row_insert::<ROW_LOG>(table_ptr, heads_ptr, h, pos);

            // Repcode matches stay legal from MIN_MATCH up regardless of
            // the row's min_match; the offset-pays gate matches the chain
            // rows (bypassed on dictionary frames, whose far matches are
            // the dict parse's payload).
            if (best_len < min_match && !(rep_hit && best_len >= MIN_MATCH))
                || (!dict_row
                    && !pays_for_offset_lit(win, idx, best_len, best_cand, rep_hit, lit_lens))
            {
                // Grow the probe step on long literal runs (the chain
                // loop's policy — deliberately faster-growing than
                // libzstd's anchor-distance grid: measured again on the
                // row, libzstd's strength-8 step costs json 1.4-2.9% size
                // and 12-14% speed for ~0.6% of text's, the ramp's
                // skipped positions being a ratio win on rep-dense
                // shapes).
                miss_count += 1;
                let step = 1 + (miss_count >> 2).min(255) as u64;
                pos += step;
                continue $restart;
            }
            miss_count = 0;

            // Lazy evaluation: the chain loop's literal-aware alternating
            // gain walk, verbatim (see parse_chain's block for the margin
            // and pricing commentary).
            let mut best_pos = pos;
            let mut best_price = if rep_hit {
                0
            } else {
                price_of((best_pos - win_base) as usize, best_cand)
            };
            if lazy_depth > 0 && best_len < 64 {
                let mut best_v =
                    lit_value(win, (best_pos - win_base) as usize, best_len, lit_lens);
                'lazy: loop {
                    for (attempt, &(rep_mul, rep_m, search_m)) in
                        [(3i32, 1i32, 4i32), (4, 1, 7)].iter().enumerate()
                    {
                        if attempt > 0 && lazy_depth < 2 {
                            break;
                        }
                        let p2 = pos + 1;
                        if block_end.saturating_sub(p2) < hash_read {
                            break 'lazy;
                        }
                        pos = p2;
                        let idx2 = (p2 - win_base) as usize;
                        let hash2 = row_hash_at(win, idx2, hash_log, ROW_LOG, width);
                        if block_end.saturating_sub(p2 + 1) >= hash_read {
                            let hn = row_hash_at(win, idx2 + 1, hash_log, ROW_LOG, width);
                            row_prefetch::<ROW_LOG>(table_ptr, hn);
                        }
                        // Repcode probe at the stepped position (the chain
                        // loop's skip-when-incumbent-is-rep form).
                        if (!$gated || rep_pending == 0)
                            && !rep_hit
                            && let Some(cand_abs) = p2.checked_sub(rep[0] as u64)
                            && cand_abs >= win_base
                            && !ramp.blocks(p2, cand_abs)
                        {
                            let ci = (cand_abs - win_base) as usize;
                            if read4(win, ci) == read4(win, idx2) {
                                let ml = extend_match(win, idx2, ci);
                                if ml >= MIN_MATCH {
                                    if 6 * ml as i32 * rep_mul / 4
                                        > best_v * rep_mul / 4 - best_price + rep_m
                                    {
                                        let v = lit_value(win, idx2, ml, lit_lens);
                                        if v * rep_mul / 4
                                            > best_v * rep_mul / 4 - best_price + rep_m
                                        {
                                            best_len = ml;
                                            best_cand = ci;
                                            best_pos = p2;
                                            best_price = 0;
                                            best_v = v;
                                            rep_hit = true;
                                            seed_hit = false;
                                            continue 'lazy;
                                        }
                                    }
                                }
                            }
                        }
                        // Row search at the stepped position, on the piped
                        // hash above.
                        let (len2, cand2) = search(win, idx2, hash2);
                        let price2 = if len2 >= min_match {
                            price_of((p2 - win_base) as usize, cand2)
                        } else {
                            0
                        };
                        if len2 >= min_match {
                            if 6 * len2 as i32 - price2 > best_v - best_price + search_m {
                                let v2 = lit_value(win, idx2, len2, lit_lens);
                                if v2 - price2 > best_v - best_price + search_m {
                                    best_len = len2;
                                    best_cand = cand2;
                                    best_pos = p2;
                                    best_price = price2;
                                    best_v = v2;
                                    rep_hit = false;
                                    seed_hit = false;
                                    continue 'lazy;
                                }
                            }
                        }
                    }
                    break;
                }
            }
            let mut start = (best_pos - win_base) as usize;

            // Backward extension into the pending literals; a repcode
            // emission must keep one literal pending (of_value 1 with a
            // zero literal length resolves to a repcode swap).
            let anchor_idx = (anchor - win_base) as usize;
            let floor = if rep_hit {
                anchor_idx + 1
            } else {
                anchor_idx
            };
            let cfl = ramp.ext_floor(best_cand, win_base);
            let mut cand = best_cand;
            let mut ml = best_len;
            while start > floor && cand > cfl && win[cand - 1] == win[start - 1] {
                cand -= 1;
                start -= 1;
                ml += 1;
            }
            let of_value = if rep_hit {
                1
            } else {
                (start - cand + 3) as u32
            };
            // Prefetch the post-match scan position's rows before the
            // emit: the top-of-loop pipelining only covers
            // scan-consecutive positions, and every emission breaks that
            // chain (the 1-ahead prefetch never ran for the jumped-to
            // position). The emit's literal copy and covered fill give
            // the load its shadow. The repcode continuation may extend
            // past `start + ml`; its own emissions land on fresh hashes
            // either way, and the scan's top-of-loop prefetch takes over
            // from the next step.
            // Prefetch the post-match scan position's rows before the
            // emit: the top-of-loop pipelining only covers
            // scan-consecutive positions, and every emission breaks that
            // chain. The emit's literal copy and covered fill give the
            // load its shadow (the repcode continuation may extend past
            // it; its own emissions hash fresh either way).
            let next = win_base + (start + ml) as u64;
            if block_end.saturating_sub(next) > hash_read {
                let hn = row_hash_at(win, start + ml, hash_log, ROW_LOG, width);
                row_prefetch::<ROW_LOG>(table_ptr, hn);
            }
            anchor = emit.emit::<ROW_LOG>(win, anchor, start, ml, of_value, &mut rep);
            if $gated && rep_pending != 0 && of_value > 3 {
                rep_pending -= 1;
            }
            if seed_hit {
                seed_hits += 1;
                if seed_hits >= SEED_MATCHES {
                    seed_offset = 0;
                }
            }
            if $gated && rep_pending != 0 {
                pos = anchor;
            } else {
                pos = emit.rep1_row::<ROW_LOG>(win, anchor, block_end, &mut rep, ramp);
            }
            anchor = pos;
            };
        }
        // Seeded phase: job starts only; each emit re-checks convergence
        // at the head, and the values only ever wind down.
        'seeded: while (rep_pending != 0 || seed_offset != 0)
            && block_end.saturating_sub(pos) >= hash_read
        {
            scan_row!('seeded, true);
        }
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            scan_row!('restart, false);
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }
}
