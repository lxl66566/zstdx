//! Hash-table maintenance and the emit helpers of the fast/dfast/chain
//! scans: position packing, inserts and backfills, the chain walk, and the
//! job-start table clear.

use alloc::vec::Vec;

use super::{
    MIN_MATCH, PREFILL_STRIDE,
    gates::{CoveredFill, RampGate},
    hash::{ChainHashWidth, extend_match, hash_at_log, hash_at_width, hash8_at_log, read4},
};
use crate::{
    decoding::sequence_execution::do_offset_history,
    encoding::{SeqWord, seq_codes::pack_seq},
};

// The opt strategies' never-valid table entry (see [`super::opt`] for the
// biased monotone-position form).
pub(super) const EMPTY: u32 = 0;

/// Encode an absolute position as a fast/dfast/chain table entry. The +1
/// bias keeps a zeroed slot dead (no position maps to zero), so fresh and
/// unwritten slots never resolve; position `2^32 - 1` collides with the
/// sentinel — one dead insert per 4 GiB cycle, pure noise.
#[inline(always)]
pub(in crate::encoding) fn pack_pos(abs: u64) -> u32 {
    (abs as u32).wrapping_add(1)
}

/// Decode a fast/dfast/chain entry against the scanning position `pos`:
/// the entry's high bits were truncated, so rebuild them from `pos` and
/// unwrap one 4 GiB cycle when the value landed above it. Live entries are
/// strictly older than `pos`, so a reconstruction at or above it wraps —
/// and a wrap that underflows unmasks a stale entry from an earlier frame
/// (whose positions share no domain with `pos`); those die like empty
/// slots. The caller's window-range check and the byte compare behind it
/// decide the rest.
///
/// The scan loops now resolve entries as distances (see [`chain_search`]
/// and the fast/dfast `resolve` closures), so only the entry-roundtrip
/// tests exercise this exact form.
#[cfg(test)]
#[inline(always)]
pub(super) fn unpack_pos(entry: u32, pos: u64) -> Option<u64> {
    if entry == 0 {
        return None;
    }
    let cand = (pos & !(u32::MAX as u64)) + entry as u64 - 1;
    let cand = if cand >= pos {
        cand.checked_sub(1 << 32)?
    } else {
        cand
    };
    Some(cand)
}

/// Chain-walk search from the hash head `entry` at window index `idx`,
/// returning the longest match's (length, candidate window index) —
/// `(0, usize::MAX)` when nothing matched. Every candidate is beat-checked,
/// so stale chain links (see the sparse fill in rep1_chain) only cost probes,
/// never correctness.
///
/// The head entry is passed in (not loaded) because the caller's insert
/// block links its chain slot to the same value: the position is hashed and
/// the head read exactly once per scan step (nothing writes the table
/// between the probe and the insert). `#[inline(always)]` with the same
/// budget rationale as [`TableEmit::rep1_chain`]: left to its own judgment
/// the inliner outlines the body (two call sites in one scan loop), and the
/// calling convention — a closure environment re-loaded per call plus a
/// dozen stack round-trips — measured at ~66% of json.balanced cycles.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn chain_search(
    win: &[u8],
    chain: *const u32,
    idx: usize,
    entry: u32,
    win_base: u64,
    block_end: u64,
    search_depth: usize,
    chain_mask: usize,
    max_window: u64,
    ramp: RampGate,
) -> (usize, usize) {
    let pos_abs = win_base + idx as u64;
    // Oldest usable candidate age: within the level window and inside the
    // live window buffer, folded into one distance compare (below) — the
    // same resolve form as the fast loop's.
    let reach = (pos_abs - win_base).min(max_window);
    let mut best_len = 0usize;
    let mut best_cand = usize::MAX;
    let mut tried = 0usize;
    let mut dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
    // `dist - 1 < reach` admits exactly dist ∈ [1, reach]: dist 0 is a stale
    // slot holding this very position (pack_pos is injective per 4-GiB
    // cycle, but old-cycle slots alias anything), whose candidate would
    // byte-compare against itself; larger dist wraps huge for the empty
    // sentinel, stale 4-GiB-cycle entries and at-or-newer-than-pos
    // reconstructions. The walk's monotone (links target strictly older
    // positions) keeps an out-of-window dist the exact break the old
    // candidate-floor check was. The subtraction is wrapping for the same
    // reason: dist 0 must wrap to u64::MAX to be rejected, not panic in
    // debug builds.
    while tried < search_depth && dist.wrapping_sub(1) < reach {
        let cand_abs = pos_abs - dist;
        let cand = (cand_abs - win_base) as usize;
        // Beat-check (libzstd's "potentially better" read): the 4 bytes
        // ending at best_len+1 decide whether the candidate can strictly
        // improve, so most hash collisions reject on one load instead of a
        // full extend. With no best yet the probe sits at 0, the plain
        // first-4 compare. The probe stays inside the block: still looping
        // means best_len is short of the block end (the break below fires
        // otherwise), so [probe, probe+4) ends at most at the block end.
        let probe = best_len.saturating_sub(3);
        if !ramp.blocks(pos_abs, cand_abs) && read4(win, cand + probe) == read4(win, idx + probe) {
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
        tried += 1;
        // SAFETY: masked to the chain table size.
        let entry = unsafe { *chain.add(cand_abs as usize & chain_mask) };
        dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
    }
    (best_len, best_cand)
}

/// Store `abs` as the newest position for its hash into a table of `log`
/// bits. Caller guarantees `idx` has at least 5 bytes of window behind it.
#[inline(always)]
pub(super) fn insert_at(win: &[u8], table: &mut [u32], idx: usize, abs: u64, log: u32) {
    // SAFETY: hash_at_log masks to log bits and the table holds
    // 1 << log slots, so the index cannot leave it.
    unsafe {
        *table.get_unchecked_mut(hash_at_log(win, idx, log)) = pack_pos(abs);
    }
}

/// Dictionary-load backfill of one stride group's skipped positions (the
/// `[1, PREFILL_STRIDE)` offsets after `idx`): a position claims its slot
/// only while the slot is still empty, so the stride grid's newest-wins
/// inserts are untouched and the oldest twin of an otherwise-idle slot
/// survives (libzstd's `ZSTD_dtlm_full` fill). 5-byte hash variant.
pub(super) fn backfill_empty(
    table: &mut [u32],
    win: &[u8],
    base: u64,
    idx: usize,
    log: u32,
    last: usize,
) {
    // SAFETY: same indexing contract as insert_at; the slot check reads
    // before the write, both inside the table.
    unsafe {
        for p in 1..PREFILL_STRIDE {
            let q = idx + p;
            if q < last {
                let h = hash_at_log(win, q, log);
                if *table.get_unchecked(h) == 0 {
                    *table.get_unchecked_mut(h) = pack_pos(base + q as u64);
                }
            }
        }
    }
}

/// [`backfill_empty`]'s 8-byte hash variant (the dfast long table).
pub(super) fn backfill8_empty(
    table: &mut [u32],
    win: &[u8],
    base: u64,
    idx: usize,
    log: u32,
    last: usize,
) {
    // SAFETY: same indexing contract as insert_at; the slot check reads
    // before the write, both inside the table.
    unsafe {
        for p in 1..PREFILL_STRIDE {
            let q = idx + p;
            if q < last {
                let h = hash8_at_log(win, q, log);
                if *table.get_unchecked(h) == 0 {
                    *table.get_unchecked_mut(h) = pack_pos(base + q as u64);
                }
            }
        }
    }
}

/// [`insert_at`] with the chain link written too: the head insert parks a
/// candidate position whose chain slot the walk side reads, so every head
/// entry needs its link (see `prefill_window`'s stale-slot notes — a
/// link-less head entry parks the walk on a slot whose content depends on
/// whatever earlier job last wrote it).
#[inline(always)]
pub(super) fn insert_linked_at(
    win: &[u8],
    table: &mut [u32],
    chain: &mut [u32],
    idx: usize,
    abs: u64,
    log: u32,
    width: ChainHashWidth,
) {
    // SAFETY: hash_at_width masks to log bits and the tables hold 1 << log
    // and chain.len() slots; the absolute position masks to the chain size
    // (absolute key; see emit_chain's note on the walk side's indexing).
    unsafe {
        let h = hash_at_width(win, idx, log, width);
        let head = *table.get_unchecked(h);
        *chain.get_unchecked_mut(abs as usize & (chain.len() - 1)) = head;
        *table.get_unchecked_mut(h) = pack_pos(abs);
    }
}

/// Push one sequence's literals and packed code/add-bits streams, and
/// update the repeated-offset history. Shared by the fast, chain and opt
/// emit paths; returns the literal length and the window-relative match end.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(in crate::encoding) fn push_seq_packed(
    win: &[u8],
    win_base: u64,
    anchor: u64,
    start: usize,
    match_len: usize,
    of_value: u32,
    rep: &mut [u32; 3],
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) -> (u32, usize) {
    let anchor_idx = (anchor - win_base) as usize;
    let ll = (start - anchor_idx) as u32;
    // Most sequences carry only a handful of literals; an out-of-line memcpy
    // per match costs more than the copy itself. Half the sequences on
    // structured data are repcode chains with no literals at all — their
    // copy is skipped outright.
    let lits = &win[anchor_idx..start];
    if ll == 0 {
        do_offset_history(of_value, 0, rep);
        seqs.push(pack_seq(0, match_len as u32, of_value));
        return (0, start + match_len);
    }
    // SAFETY: both sides are within the window / buffer; unaligned because
    // byte-granular. The 8-byte read may overlap the match that follows the
    // literals, so it stays in bounds whenever 8 bytes from the anchor fit
    // the window; the write may spill up to 7 bytes past the new length, but
    // reserve(8) covers them and later pushes overwrite them.
    unsafe {
        if lits.len() <= 8 && anchor_idx + 8 <= win.len() {
            literals.reserve(8);
            let dst = literals.as_mut_ptr().add(literals.len());
            let v = lits.as_ptr().cast::<u64>().read_unaligned();
            dst.cast::<u64>().write_unaligned(v);
            literals.set_len(literals.len() + lits.len());
        } else {
            literals.extend_from_slice(lits);
        }
    }
    do_offset_history(of_value, ll, rep);
    seqs.push(pack_seq(ll, match_len as u32, of_value));
    (ll, start + match_len)
}

/// Index a match's covered range (the fast strategy's fill policy).
/// Short matches keep every position (they carry the alignment coverage on
/// structured data). Long matches only index two anchors (zstd fast's fill
/// policy): one just inside the start, one just before the end — the scan
/// loop already indexes the positions it probes, so interior coverage only
/// needs seed points for the phases the scan skips over, and hashing a
/// 4-byte grid across long matches dominated encoder time. Both anchors
/// need HASH_READ bytes of window ahead; a match reaching the insert bound
/// simply leaves them out. Outlined from [`TableEmit::emit`] (see there).
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(super) fn insert_covered(
    win: &[u8],
    table: &mut [u32],
    mut chain: Option<&mut [u32]>,
    win_base: u64,
    start: usize,
    match_len: usize,
    insert_max: u64,
    log: u32,
    fill: CoveredFill,
    width: ChainHashWidth,
) {
    // One insert form per fill site: the chain strategies link every
    // covered position they head-insert (see `insert_linked_at`), the fast
    // strategy has no chain to link.
    macro_rules! put {
        ($idx:expr, $abs:expr) => {
            match chain.as_deref_mut() {
                Some(chain) => insert_linked_at(win, table, chain, $idx, $abs, log, width),
                None => insert_at(win, table, $idx, $abs, log),
            }
        };
    }
    if match_len <= 16 {
        let end = (win_base + (start + match_len) as u64).min(insert_max);
        let mut p = win_base + start as u64;
        // The clamped end can fall below p — tiny windows saturate
        // insert_max to win_base, and rep-chain tails emit to MIN_MATCH of
        // the block end, not HASH_READ. Bail on the empty range: the
        // wrapped `end - p` below would peel-insert one position whose
        // hash reads past the window (fuzz-found heap overread).
        if p >= end {
            return;
        }
        // Density-gated stride-2 form first, so the dense path below stays
        // instruction-identical on the shapes that never gate (the branch
        // lives in this outlined body, never in the scan loop).
        if fill == CoveredFill::Strided {
            // Kept rolled: the modal gated match holds 3-4 stride-2
            // inserts, where an unrolled pair form's peel and tail checks
            // cost more than they save (measured on dll100).
            while p < end {
                put!((p - win_base) as usize, p);
                p += 2;
            }
            return;
        }
        // Peel the odd tail before the loop: `while p < end` alone unrolls
        // mod 2 behind a per-entry parity guard that mispredicts on every
        // other insert (measured on json.fastest).
        if (end - p) & 1 == 1 {
            put!((p - win_base) as usize, p);
            p += 1;
        }
        while p < end {
            put!((p - win_base) as usize, p);
            put!((p + 1 - win_base) as usize, p + 1);
            p += 2;
        }
    } else if fill == CoveredFill::DictDense {
        // Dictionary-row chain emits fill the whole interior (see the
        // variant): near-duplicate dict twins sit on every phase.
        let end = (win_base + (start + match_len) as u64).min(insert_max);
        let mut p = win_base + start as u64;
        if p < end && (end - p) & 1 == 1 {
            put!((p - win_base) as usize, p);
            p += 1;
        }
        while p < end {
            put!((p - win_base) as usize, p);
            p += 1;
        }
    } else {
        let base = win_base + start as u64;
        let hi = base + match_len as u64 - 2;
        if hi <= insert_max {
            let lo = base + 2;
            put!((lo - win_base) as usize, lo);
            if hi > lo {
                put!((hi - win_base) as usize, hi);
            }
        }
    }
}

/// Shared mutable state of the single-table strategies (fast and chain):
/// the head hash table, the output streams and the per-block constants,
/// bundled so the emit helpers stay inside the register argument budget
/// (their former twelve-to-fourteen-argument free signatures moved several
/// arguments through the stack on every call).
pub(super) struct TableEmit<'a> {
    pub(super) table: &'a mut [u32],
    pub(super) literals: &'a mut Vec<u8>,
    pub(super) seqs: &'a mut Vec<SeqWord>,
    pub(super) win_base: u64,
    /// Last absolute position whose hash reads stay inside the window.
    pub(super) insert_max: u64,
    /// Log of `table` (the fast strategy's hash log).
    pub(super) hash_log: u32,
    /// Short-match interior fill policy of this block (density-coupled;
    /// see [`CoveredFill`]). Only `emit` reads it.
    pub(super) covered_fill: CoveredFill,
    /// The chain-table hash width (fast rows always pass `Six`; only the
    /// linked arm of `insert_covered` reads it).
    pub(super) width: ChainHashWidth,
}

impl TableEmit<'_> {
    /// Emit the sequence for a match covering `match_len` bytes at window
    /// index `start`, update the repeated-offset history, index the covered
    /// range and return the new cursor (which is also the new anchor). The
    /// sequence is pushed straight into the packed code/add-bits streams the
    /// block encoder consumes — the raw (ll, ml, of) triple never gets its
    /// own buffer. The coverage indexing is outlined ([`insert_covered`]):
    /// with it inline, the whole helper grew past the inliner's budget, and
    /// every scan-loop call site paid a full spill/reload of the emit
    /// context (literals/seqs/rep) around the call.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit(
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
        insert_covered(
            win,
            self.table,
            None,
            self.win_base,
            start,
            match_len,
            self.insert_max,
            self.hash_log,
            self.covered_fill,
            ChainHashWidth::Five,
        );
        self.win_base + match_end as u64
    }

    /// [`TableEmit::emit`] with the covered fill chain-linked: the chain
    /// strategies' repcode continuation emits through here, and a covered
    /// fill without its link parks a head entry the walk side cannot trust
    /// (see `insert_linked_at`).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_linked(
        &mut self,
        win: &[u8],
        chain: &mut [u32],
        anchor: u64,
        start: usize,
        match_len: usize,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            1,
            rep,
            self.literals,
            self.seqs,
        );
        insert_covered(
            win,
            self.table,
            Some(chain),
            self.win_base,
            start,
            match_len,
            self.insert_max,
            self.hash_log,
            self.covered_fill,
            self.width,
        );
        self.win_base + match_end as u64
    }

    /// [`TableEmit::emit`] for the chain strategies: the covered range is
    /// indexed with complete hash head plus chain links (a coarse grid for
    /// long matches, so huge runs cannot dominate the hash work), keeping
    /// later chain walks connected to same-hash predecessors.
    /// `inline(always)`: at a plain `#[inline]` LLVM leaves the chain
    /// scan's one emit call outlined, and every sequence pays the fat
    /// calling convention (ten-plus args, Vec fields through `self`) —
    /// the same disease the outlined search closure had.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_chain(
        &mut self,
        win: &[u8],
        chain: &mut [u32],
        hash_log: u32,
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
        let chain_mask = chain.len() - 1;
        // Covered-fill stride: dense up to 64 match bytes, every 4th
        // position beyond. libzstd's lazy rows index every interior
        // position; the grid bounds the fill work of huge same-hash runs
        // at 1/4 interior candidate density — a deliberate speed-for-ratio
        // trade, the first knob to re-check on a chain-band ratio
        // regression. Dictionary rows fill dense (their parse rides
        // near-duplicate twins on the skipped phases; the fill cost is
        // bounded by the small payload).
        let step = (if match_len <= 64 || self.covered_fill == CoveredFill::DictDense {
            1
        } else {
            4
        }) as u64;
        let end_abs = (self.win_base + match_end as u64).min(self.insert_max);
        let mut p = self.win_base + start as u64;
        while p < end_abs {
            let i = (p - self.win_base) as usize;
            let h = hash_at_width(win, i, hash_log, self.width);
            // SAFETY: h is masked to hash_log bits, p to the chain table
            // size. The chain slot key is the ABSOLUTE position — the walk
            // side resolves candidates absolutely: a window-relative index
            // diverges from it once win_base stops being a multiple of the
            // chain size (window compaction in streaming, per-block adopted
            // windows in bulk, job-relative windows in MT), scrambling every
            // walk past its first hop.
            unsafe {
                let head = *self.table.get_unchecked(h);
                *chain.get_unchecked_mut(p as usize & chain_mask) = head;
                *self.table.get_unchecked_mut(h) = pack_pos(p);
            }
            p += step;
        }
        self.win_base + match_end as u64
    }

    /// Probe continuations at the second repeated offset immediately after a
    /// match (zstd fast's rep_offset2 loop). Alternating-period data chains
    /// rep0/rep1 matches back to back with zero literals; emitting with
    /// of_value 1 at ll == 0 swaps rep0/rep1, so the loop alternates distances
    /// on its own. Returns the cursor after the last chained match.
    /// `inline(always)` for the same budget reason as [`DfastEmit::rep_chain`].
    #[inline(always)]
    pub(super) fn rep1_chain<const RAMPED: bool>(
        &mut self,
        win: &[u8],
        mut chain: Option<&mut [u32]>,
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
            if RAMPED && ramp.blocks(pos, cand_abs) {
                break;
            }
            let pidx = (pos - self.win_base) as usize;
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pidx) {
                break;
            }
            let ml = extend_match(win, pidx, cand);
            debug_assert!(ml >= MIN_MATCH);
            pos = match chain.as_deref_mut() {
                Some(chain) => self.emit_linked(win, chain, pos, pidx, ml, rep),
                None => self.emit(win, pos, pidx, ml, 1, rep),
            };
        }
        pos
    }
}

/// Shared mutable state of the dfast strategy: the two tables, the output
/// streams and the per-block constants, bundled so the emit helpers stay
/// inside the register argument budget (their former fourteen-argument
/// signatures moved several arguments through the stack on every call).
pub(super) struct DfastEmit<'a> {
    pub(super) long: &'a mut [u32],
    pub(super) small: &'a mut [u32],
    pub(super) literals: &'a mut Vec<u8>,
    pub(super) seqs: &'a mut Vec<SeqWord>,
    pub(super) win_base: u64,
    /// Last window index whose hash reads stay inside the window.
    pub(super) insert_max_idx: usize,
    pub(super) long_log: u32,
    pub(super) small_log: u32,
    /// Short-table hash width: 4 on dictionary frames (libzstd's
    /// small-table dfast rows), 5 otherwise. A field, not a const generic:
    /// these insert sites run per sequence, not per position.
    pub(super) width: ChainHashWidth,
}

impl DfastEmit<'_> {
    /// Insert `idx` into both tables. Caller guarantees `idx + HASH_READ`
    /// bytes of window.
    #[inline(always)]
    pub(super) fn insert_both(&mut self, win: &[u8], idx: usize) {
        // SAFETY: both hashes are masked to their tables' sizes.
        unsafe {
            let entry = pack_pos(self.win_base + idx as u64);
            *self
                .long
                .get_unchecked_mut(hash8_at_log(win, idx, self.long_log)) = entry;
            *self
                .small
                .get_unchecked_mut(hash_at_width(win, idx, self.small_log, self.width)) = entry;
        }
    }

    /// Emit one sequence (literals from `anchor_idx`, match at `start`) and
    /// seed libzstd's complementary anchors instead of indexing every
    /// covered position: both tables take the position two past the scan
    /// position the match was found at (`curr_idx + 2`), the long table the
    /// match end minus two, the small table the match end minus one.
    /// Anchors past `insert_max_idx` are dropped; the scan tail never
    /// probes them. Returns the new anchor as a window index.
    ///
    /// `inline(always)` for the same budget reason as [`DfastEmit::rep_chain`]:
    /// at a plain `#[inline]` LLVM leaves the steady-phase sites outlined,
    /// and the eight-argument call (three stack-passed args, rep through
    /// memory, Vec fields via `self`) taxed every sequence with ~30% of
    /// the emit body (dll32 callgrind: 170 Ir/seq outlined, 26% of the
    /// whole fast encode; inlining: json.fast Ir −8.2%, dll100 wall +7%).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit(
        &mut self,
        win: &[u8],
        anchor_idx: usize,
        curr_idx: usize,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> usize {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            self.win_base + anchor_idx as u64,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        if curr_idx + 2 <= self.insert_max_idx {
            self.insert_both(win, curr_idx + 2);
        }
        if match_end >= 2 && match_end - 2 <= self.insert_max_idx {
            // SAFETY: masked to the long table size.
            unsafe {
                *self
                    .long
                    .get_unchecked_mut(hash8_at_log(win, match_end - 2, self.long_log)) =
                    pack_pos(self.win_base + (match_end - 2) as u64);
            }
        }
        if match_end >= 1 && match_end - 1 <= self.insert_max_idx {
            // SAFETY: masked to the small table size.
            unsafe {
                *self.small.get_unchecked_mut(hash_at_width(
                    win,
                    match_end - 1,
                    self.small_log,
                    self.width,
                )) = pack_pos(self.win_base + (match_end - 1) as u64);
            }
        }
        match_end
    }

    /// [`rep1_chain`] for the dfast strategy: each repcode position goes
    /// into both tables (no complementary anchors — libzstd's offset_2 loop
    /// only re-seeds the position it consumes). Returns the cursor as a
    /// window index, which is also the new anchor.
    /// `inline(always)`: the phase-split loop instantiates its call sites
    /// twice, which halves the inliner's budget per copy — left to its own
    /// `#[inline]` judgment the helper flips to an outlined call in the
    /// steady phase and every dense-match emit pays the argument setup.
    #[inline(always)]
    pub(super) fn rep_chain<const RAMPED: bool>(
        &mut self,
        win: &[u8],
        pos_idx: usize,
        ilimit_idx: usize,
        rep: &mut [u32; 3],
        ramp: RampGate,
    ) -> usize {
        let mut pos = pos_idx;
        while pos <= ilimit_idx {
            let Some(cand_abs) = (self.win_base + pos as u64).checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            if RAMPED && ramp.blocks(self.win_base + pos as u64, cand_abs) {
                break;
            }
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pos) {
                break;
            }
            let ml = extend_match(win, pos, cand);
            debug_assert!(ml >= MIN_MATCH);
            self.insert_both(win, pos);
            let (_ll, match_end) = push_seq_packed(
                win,
                self.win_base,
                self.win_base + pos as u64,
                pos,
                ml,
                1,
                rep,
                self.literals,
                self.seqs,
            );
            pos = match_end;
        }
        pos
    }
}

/// Zero a job-start table. Large clears stream megabytes per job through
/// the bus; non-temporal stores skip the ownership read and keep the zero
/// lines from evicting the scan's working set, while small tables stay
/// cache-warm for the probes that follow the clear.
pub(super) fn clear_table(t: &mut [u32]) {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    if t.len() >= NT_CLEAR_MIN && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected; the stores stay inside t
        // and the fence retires them before any read.
        unsafe { clear_table_avx512(t) };
        return;
    }
    t.fill(0);
}

/// Non-temporal table clear: aligned 64-byte streaming stores plus one
/// fence. Unaligned head and tail run as plain stores.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512f")]
pub(super) unsafe fn clear_table_avx512(t: &mut [u32]) {
    unsafe {
        use core::arch::x86_64::*;
        let zero = _mm512_setzero_si512();
        let mut p = t.as_mut_ptr();
        let end = p.add(t.len());
        // SAFETY: p advances only while strictly below end; the alignment head
        // and element tail each touch disjoint in-bounds ranges.
        while (p as usize) & 63 != 0 && p < end {
            *p = 0;
            p = p.add(1);
        }
        let aligned_end = (end as usize - ((end as usize) & 63)) as *mut u32;
        while p < aligned_end {
            _mm512_stream_si512(p.cast(), zero);
            p = p.add(16);
        }
        while p < end {
            *p = 0;
            p = p.add(1);
        }
        _mm_sfence();
    }
}

/// Slot count above which the job-start table clear goes non-temporal
/// (4 MiB of u32 slots): smaller tables profit more from staying cached.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
pub(super) const NT_CLEAR_MIN: usize = 1 << 20;
