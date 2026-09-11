//! Matching algorithm used to find repeated parts of the original data
//!
//! The Zstd format relies on finding repeated sequences of data and compressing these
//! sequences as instructions for the decoder. A sequence basically tells the decoder
//! "Go back X bytes and copy Y bytes to the end of your decode buffer".
//!
//! This is a port of the official zstd matcher family: one contiguous
//! window buffer, single-probe hash tables (newest-wins overwrite
//! semantics, dfast's second short-hash table for the Fast level), hash
//! chains for the levels above, forward match extension in u64 chunks plus
//! backward extension into pending literals. Positions are tracked as
//! absolute u64 offsets. The fast/dfast/chain table slots are u32: the
//! position biased by one and truncated (zero stays the never-written
//! sentinel), with the high bits reconstructed from the scanning position
//! and a window-range check at read time — half the working set of the
//! former u64 slots. The opt strategies keep their own u64 tables with an
//! `(epoch << 48) | position` tag; a reset bumps that epoch instead of
//! clearing them, while the u32 tables need no clearing at all.

use alloc::vec::Vec;

use super::{
    Matcher, SeqWord, Sequence,
    opt::{OptKnobs, OptScratch, OptState},
    seq_codes::{decode_packed, pack_seq},
};
use crate::Level;
// Shared with the decoder so both sides agree on offset-history semantics.
use crate::decoding::sequence_execution::do_offset_history;

/// Shortest match worth encoding; matches the format's MINMATCH range.
pub(super) const MIN_MATCH: usize = 4;
/// The hash reads a full u64, so insertable/scannable positions need this
/// many window bytes ahead to stay in bounds.
pub(super) const HASH_READ: usize = 8;
/// Hash table size as a power of two.
const HASH_LOG: u32 = 15;
/// 5-byte window multiply prime (libzstd `prime5bytes`); only the low 64
/// bits of the product feed the slot index.
const HASH_PRIME: u64 = 0xc2b2_ae3d_27d4_eb4f;
/// Prefill grid spacing (libzstd's `fastHashFillStep`, also what its
/// dictionary-content load uses for fast/dfast): the strip's mid-distance
/// match coverage survives a 3x coarser grid, at a third of the fill cost.
/// Periodic-repeat locking is NOT the grid's job — see `seed_offset`; a
/// dense grid cannot provide it anyway, because on clumped data the twin
/// slot's newest entry is always a recent same-hash recurrence, burying any
/// period-old twin. The chain strategy deviates from libzstd's dense
/// dictionary fill here: its strip is the full window (libzstd's job prefix
/// is window>>3), so a dense fill would cost half the job's scan time; the
/// grid keeps the head table's first hop and the chain links (grid
/// positions link to the previous same-hash grid position) while unwritten
/// chain slots simply read as dead or stale entries, which the walk's
/// domain check already discards.
const PREFILL_STRIDE: usize = 3;
/// Backward bytes that must agree (beyond the 8-byte anchor) before a strip
/// position becomes the job-start seed offset: long enough that word-level
/// repeats (~10-15 agreeing bytes on natural text) cannot qualify, short
/// enough that one cache line of checking settles it.
const SEED_AGREE: usize = 48;
/// Seed matches to emit before retiring the seed: three literal offsets
/// both clear the repcode gate and rotate `rep` until `rep[0]` holds the
/// seed offset, so the regular repcode probes take over from there.
const SEED_MATCHES: u8 = 3;
/// Probe attempts an unused seed survives: a seed whose offset stops
/// matching (broken period, or a repeated block that ended) must not pay a
/// dead compare for the rest of the job.
const SEED_BUDGET: u32 = 8192;
/// Strip length below which the seed scan stays scalar: the AVX-512 block
/// walk's lowest block reads up to byte 63 + 71, and `last + 8 >= 136`
/// keeps every such load inside data. Shorter strips are cheap anyway.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
const SEED_SCAN_MIN: usize = 128;
/// History kept for matching; also the window size declared in the frame header.
const MAX_WINDOW: usize = 0xc0000;

/// Log of the incompressibility gate's probe table (see
/// [`Matcher::skip_if_incompressible`]). 32K tag slots keep the overwrite
/// rate of a block's ~2000 samples negligible.
const GATE_PROBE_LOG: u32 = 15;
/// Blocks below this pay little enough in the matcher that the gate's
/// sample pass is not worth its own cost.
const GATE_MIN_BLOCK: usize = 16384;

// The opt strategies' never-valid table entry; their positions live in
// the low 48 bits with the epoch in the high 16 (see
// [`MatchGeneratorDriver::reset`]).
const EMPTY: u64 = 0;

/// Encode an absolute position as a fast/dfast/chain table entry. The +1
/// bias keeps a zeroed slot dead (no position maps to zero), so fresh and
/// unwritten slots never resolve; position `2^32 - 1` collides with the
/// sentinel — one dead insert per 4 GiB cycle, pure noise.
#[inline(always)]
fn pack_pos(abs: u64) -> u32 {
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
#[inline(always)]
fn unpack_pos(entry: u32, pos: u64) -> Option<u64> {
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

/// Which search loop the matcher runs. The `chain` buffer is the second
/// table for the two-table strategies and empty for [`Strategy::Fast`].
#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    /// One single-probe table keyed by a 5-byte hash (level Fastest,
    /// libzstd's `fast`).
    Fast,
    /// Two single-probe tables: an 8-byte long hash in `table` and a
    /// 5-byte short hash in `chain` (level Fast, libzstd's `dfast`).
    /// The payload is the short-table log.
    Dfast(u32),
    /// Head table plus chain links walked up to the level's search depth
    /// with lazy deferral (levels above Fast, libzstd's `lazy` family).
    /// The payload is the chain-table log.
    Chain(u32),
    /// Optimal-price parser over a binary match tree (levels Opt/Ultra,
    /// libzstd's btopt/btultra). The `chain` buffer holds the tree ring.
    Opt(OptKnobs),
}

/// Per-level search parameters, following libzstd's `clevels.h` ladder.
#[derive(Clone, Copy, PartialEq)]
struct LevelParams {
    hash_log: u32,
    /// Match window; also the window declared in the frame header.
    window: usize,
    strategy: Strategy,
    search_depth: u32,
    lazy_depth: u32,
}

const FASTEST_PARAMS: LevelParams = LevelParams {
    hash_log: HASH_LOG,
    window: MAX_WINDOW,
    strategy: Strategy::Fast,
    search_depth: 1,
    lazy_depth: 0,
};

/// libzstd level 3: H17 (long) C16 (short) with a 1 MiB window (the level's
/// W21 is A/B-pending; the smaller window also keeps the frame header as-is).
const FAST_PARAMS: LevelParams = LevelParams {
    hash_log: 17,
    window: 1 << 20,
    strategy: Strategy::Dfast(16),
    search_depth: 0,
    lazy_depth: 0,
};

/// The chain table is position-indexed, so its log doubles as the match
/// reach; pinning it to the window makes every in-window position linked
/// (no reach truncation). The window stays 1 MiB across the chain levels
/// (zstd's ladder below 16 does the same): on the 32 MiB corpus a 4 MiB
/// window only ever finds farther — not longer — matches, whose offset
/// codes cost more than the length saves (json and skewed both lose ratio
/// AND walk speed to the extra cache misses). Depth 8/H20 sits at the
/// speed knee of the real-chain walk (16 attempts, zstd-9's searchLog).
const BALANCED_PARAMS: LevelParams = LevelParams {
    hash_log: 20,
    window: 1 << 20,
    strategy: Strategy::Chain(20),
    search_depth: 8,
    lazy_depth: 2,
};

/// Level Best, roughly zstd 10-15: the optimal parser in its cheapest
/// setting (libzstd uses btopt for these levels on smaller inputs; the
/// hash-chain variant this replaces could not reach the tier's ratio at
/// any search depth).
const BEST_KNOBS: OptKnobs = OptKnobs {
    search_log: 4,
    sufficient_len: 32,
    min_match: 4,
    mls: 4,
    bt_log: 20,
    hash3_log: 0,
    ultra: false,
};
const BEST_PARAMS: LevelParams = LevelParams {
    hash_log: 20,
    window: 1 << 20,
    strategy: Strategy::Opt(BEST_KNOBS),
    search_depth: 0,
    lazy_depth: 0,
};

/// Level Opt, roughly zstd 16-17 (btopt): whole-bit prices with the
/// skip/early-abort heuristics and a 4-byte main hash. The tree ring
/// matches the 1 MiB window so every in-window position stays linked.
const OPT_KNOBS: OptKnobs = OptKnobs {
    search_log: 5,
    sufficient_len: 64,
    min_match: 4,
    mls: 4,
    bt_log: 20,
    hash3_log: 0,
    ultra: false,
};
const OPT_PARAMS: LevelParams = LevelParams {
    hash_log: 20,
    window: 1 << 20,
    strategy: Strategy::Opt(OPT_KNOBS),
    search_depth: 0,
    lazy_depth: 0,
};

/// Level Ultra, roughly zstd 18-22 (btultra/btultra2): fractional prices,
/// 3-byte matches via the hash3 table, and the 2-pass first-block
/// statistics seeding.
const ULTRA_KNOBS: OptKnobs = OptKnobs {
    search_log: 7,
    sufficient_len: 256,
    min_match: 3,
    mls: 3,
    bt_log: 20,
    hash3_log: 17,
    ultra: true,
};
const ULTRA_PARAMS: LevelParams = LevelParams {
    hash_log: 21,
    window: 1 << 20,
    strategy: Strategy::Opt(ULTRA_KNOBS),
    search_depth: 0,
    lazy_depth: 0,
};

fn params_for_level(level: Level) -> LevelParams {
    match level {
        Level::Uncompressed | Level::Fastest => FASTEST_PARAMS,
        Level::Fast => FAST_PARAMS,
        Level::Balanced => BALANCED_PARAMS,
        Level::Best => BEST_PARAMS,
        Level::Opt => OPT_PARAMS,
        Level::Ultra => ULTRA_PARAMS,
    }
}

/// Hash a window u64 whose low 5 bytes are the hashed prefix (the full u64
/// load feeds the multiplier directly: bits above the fifth byte only add
/// input entropy). Five bytes skip the frequent 4-byte boilerplate fragments
/// so probes land on structural repeats instead of recent junk.
#[inline(always)]
fn hash5(v: u64) -> usize {
    ((v & 0x00ff_ffff_ffff).wrapping_mul(HASH_PRIME) as usize >> (64 - HASH_LOG))
        & ((1 << HASH_LOG) - 1)
}

/// Hash the 5 bytes at `idx`. Caller guarantees `idx + 5 <= win.len()` (the
/// scanning and emit loops bound-check once per loop, not per position).
#[inline(always)]
fn hash_at(win: &[u8], idx: usize) -> usize {
    // SAFETY: see contract above; the hash itself is [`hash5`].
    unsafe { hash5(win.as_ptr().add(idx).cast::<u64>().read_unaligned()) }
}

/// [`hash_at`] for a runtime hash-log (the chain strategies size their
/// tables per level).
#[inline(always)]
fn hash_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & 0x00ff_ffff_ffff;
        (v.wrapping_mul(HASH_PRIME) as usize >> (64 - log)) & ((1usize << log) - 1)
    }
}

/// Hash the 8 bytes at `idx` into the dfast long table (libzstd's
/// `prime8bytes` multiply). Same read contract as [`hash_at_log`].
#[inline(always)]
fn hash8_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at_log.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned();
        (v.wrapping_mul(0xcf1b_bcdc_b7a5_6463) as usize >> (64 - log)) & ((1usize << log) - 1)
    }
}

/// Read 4 window bytes at `idx`. Caller guarantees `idx + 4 <= win.len()`.
#[inline(always)]
fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
}

/// Read 8 window bytes at `idx`. Caller guarantees `idx + 8 <= win.len()`.
#[inline(always)]
fn read8(win: &[u8], idx: usize) -> u64 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u64>().read_unaligned() }
}

/// Longest common prefix of `win[i..]` and `win[j..]` in u64 chunks. `i` is
/// the current scan position and `j` a candidate strictly before it, so
/// bounding by `i` also bounds `j`.
#[inline(always)]
fn extend_match(win: &[u8], i: usize, j: usize) -> usize {
    let limit = win.len() - i;
    let base = win.as_ptr();
    let mut len = 0;
    // SAFETY: i and j are valid indices and i + len + 8 <= win.len() bounds
    // the reads on both sides (j <= i).
    unsafe {
        while len + 8 <= limit {
            let a = base.add(i + len).cast::<u64>().read_unaligned();
            let b = base.add(j + len).cast::<u64>().read_unaligned();
            if a == b {
                len += 8;
            } else {
                return len + ((a ^ b).trailing_zeros() >> 3) as usize;
            }
        }
    }
    while len < limit && win[i + len] == win[j + len] {
        len += 1;
    }
    len
}

/// Offset price for the chain strategy's lazy gain comparisons (libzstd's
/// `ZSTD_highbit32(offBase)` with offBase = offset+1): the offset-code
/// exponent in ~bits. Repcode incumbents price 0.
#[inline(always)]
fn price_of(idx: usize, cand: usize) -> i32 {
    ((idx - cand + 1) as u32).ilog2() as i32
}

/// Assumed literal code lengths before the block encoder's first
/// measurement arrives: the historical flat constant of the static gate
/// (~4 bits per literal).
const DEFAULT_LIT_LENS: [u8; 256] = [4; 256];

/// Whether a non-rep match clears its offset's price: literals cost ~4 bits
/// per byte and the offset its highbit, plus a constant for the sequence
/// code overhead (libzstd's raw approximation from its lazy gain checks).
/// Without this gate the densely pre-indexed multithread job strips flood
/// the stream with five-byte matches a megabyte back — high-entropy shapes
/// lost ratio and speed alike to the emission storm.
/// The +7 margin is libzstd's depth-2 replacement constant; used here at
/// the fast/dfast seed probes and as the constant term of the chain
/// strategy's literal-cost-aware gate ([`pays_for_offset_lit`]).
#[inline(always)]
fn pays_for_offset(ml: usize, idx: usize, cand: usize, rep_hit: bool) -> bool {
    rep_hit || ml * 4 >= (idx - cand).ilog2() as usize + 7
}

/// The chain strategy's store gate: store the match only if the literals it
/// displaces cost more than its offset's price (highbit plus the +7 margin,
/// libzstd's depth-2 replacement constant). Literal bytes price at the code
/// lengths of the previous block's Huffman table — the *marginal* cost,
/// which separates the shapes the flat +7 margin traded against each other
/// (json 6.21/6.55, text 368.1/366.3 at +4/+7; the whole swing is 5-byte
/// matches in the 8-32 KiB offset band): text's displaced bytes are nearly
/// absent from its hyper-skewed residual literal stream (~0.2 bits/B on
/// average — the average itself is no discriminator) and price at the cap,
/// while json's are common in its residual stream and price cheap. For
/// matches longer than eight bytes the first eight price the run (the
/// decision band is ml ≤ 7 anyway: at the 1 MiB window an 8-byte match
/// clears even the flat-4 constant). With `lit_lens == DEFAULT_LIT_LENS`
/// this reduces to the static +7 gate bit-for-bit.
#[inline(always)]
fn pays_for_offset_lit(
    win: &[u8],
    idx: usize,
    ml: usize,
    cand: usize,
    rep_hit: bool,
    lit_lens: &[u8; 256],
) -> bool {
    if rep_hit {
        return true;
    }
    let price = (idx - cand).ilog2() as usize + 7;
    let k = ml.min(6);
    let mut cost = 0usize;
    for &b in &win[idx..idx + k] {
        cost += lit_lens[b as usize] as usize;
    }
    cost * ml >= price * k
}

/// The lazy walk's displaced-literal value of a match: the bits the match
/// saves by covering `len` bytes instead of emitting them as literals,
/// estimated from the first four bytes (every walk candidate is at least
/// MIN_MATCH long) at the code lengths fed back by the block encoder (see
/// [`MatchGeneratorDriver::lit_lens`]), clamped to 6 bits per byte — the
/// walk is a *local* selection between adjacent positions, and uncapped
/// 11-bit prices let cap-priced bytes dominate a whole walk's decisions
/// (measured: no clamp / 8 / 6 / 5 on balanced bulk-st sizes — json
/// 4726875/4701990/4697420/4725425, text 91243/91232/91200/91140; the
/// json swing dwarfs the text one at every step). With
/// `lit_lens == DEFAULT_LIT_LENS` this is exactly `len * 4`, so the walk's
/// gain comparisons reduce bit-for-bit to the flat-scale versions.
#[inline(always)]
fn lit_value(win: &[u8], idx: usize, len: usize, lit_lens: &[u8; 256]) -> i32 {
    debug_assert!(len >= MIN_MATCH);
    let s = (lit_lens[win[idx] as usize] as i32).min(6)
        + (lit_lens[win[idx + 1] as usize] as i32).min(6)
        + (lit_lens[win[idx + 2] as usize] as i32).min(6)
        + (lit_lens[win[idx + 3] as usize] as i32).min(6);
    s * len as i32 / 4
}

/// Store `abs` as the newest position for its hash. Caller guarantees
/// `idx` has at least `MIN_HASH` bytes of window behind it.
#[inline(always)]
fn insert_at(win: &[u8], table: &mut [u32], idx: usize, abs: u64) {
    let h = hash_at(win, idx);
    // SAFETY: the hash masks down to HASH_LOG bits and the table always
    // holds 1 << HASH_LOG slots, so the index cannot leave it.
    unsafe {
        *table.get_unchecked_mut(h) = pack_pos(abs);
    }
}

/// Push one sequence's literals and packed code/add-bits streams, and
/// update the repeated-offset history. Shared by the fast, chain and opt
/// emit paths; returns the literal length and the window-relative match end.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn push_seq_packed(
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
fn insert_covered(
    win: &[u8],
    table: &mut [u32],
    win_base: u64,
    start: usize,
    match_len: usize,
    insert_max: u64,
) {
    if match_len <= 16 {
        let end = (win_base + (start + match_len) as u64).min(insert_max);
        let mut p = win_base + start as u64;
        while p < end {
            insert_at(win, table, (p - win_base) as usize, p);
            p += 1;
        }
    } else {
        let base = win_base + start as u64;
        let hi = base + match_len as u64 - 2;
        if hi <= insert_max {
            let lo = base + 2;
            insert_at(win, table, (lo - win_base) as usize, lo);
            if hi > lo {
                insert_at(win, table, (hi - win_base) as usize, hi);
            }
        }
    }
}

/// Shared mutable state of the single-table strategies (fast and chain):
/// the head hash table, the output streams and the per-block constants,
/// bundled so the emit helpers stay inside the register argument budget
/// (their former twelve-to-fourteen-argument free signatures moved several
/// arguments through the stack on every call).
struct TableEmit<'a> {
    table: &'a mut [u32],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    win_base: u64,
    /// Last absolute position whose hash reads stay inside the window.
    insert_max: u64,
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
    fn emit(
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
            self.win_base,
            start,
            match_len,
            self.insert_max,
        );
        self.win_base + match_end as u64
    }

    /// [`TableEmit::emit`] for the chain strategies: the covered range is
    /// indexed with complete hash head plus chain links (a coarse grid for
    /// long matches, so huge runs cannot dominate the hash work), keeping
    /// later chain walks connected to same-hash predecessors.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn emit_chain(
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
        let step = (if match_len <= 64 {
            1
        } else {
            4
        }) as u64;
        let end_abs = (self.win_base + match_end as u64).min(self.insert_max);
        let mut p = self.win_base + start as u64;
        while p < end_abs {
            let i = (p - self.win_base) as usize;
            let h = hash_at_log(win, i, hash_log);
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
    fn rep1_chain(&mut self, win: &[u8], pos: u64, block_end: u64, rep: &mut [u32; 3]) -> u64 {
        let mut pos = pos;
        while block_end - pos >= MIN_MATCH as u64 {
            let Some(cand_abs) = pos.checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            let pidx = (pos - self.win_base) as usize;
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pidx) {
                break;
            }
            let ml = extend_match(win, pidx, cand);
            debug_assert!(ml >= MIN_MATCH);
            pos = self.emit(win, pos, pidx, ml, 1, rep);
        }
        pos
    }
}

/// Shared mutable state of the dfast strategy: the two tables, the output
/// streams and the per-block constants, bundled so the emit helpers stay
/// inside the register argument budget (their former fourteen-argument
/// signatures moved several arguments through the stack on every call).
struct DfastEmit<'a> {
    long: &'a mut [u32],
    small: &'a mut [u32],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    win_base: u64,
    /// Last window index whose hash reads stay inside the window.
    insert_max_idx: usize,
    long_log: u32,
    small_log: u32,
}

impl DfastEmit<'_> {
    /// Insert `idx` into both tables. Caller guarantees `idx + HASH_READ`
    /// bytes of window.
    #[inline(always)]
    fn insert_both(&mut self, win: &[u8], idx: usize) {
        // SAFETY: both hashes are masked to their tables' sizes.
        unsafe {
            let entry = pack_pos(self.win_base + idx as u64);
            *self
                .long
                .get_unchecked_mut(hash8_at_log(win, idx, self.long_log)) = entry;
            *self
                .small
                .get_unchecked_mut(hash_at_log(win, idx, self.small_log)) = entry;
        }
    }

    /// Emit one sequence (literals from `anchor_idx`, match at `start`) and
    /// seed libzstd's complementary anchors instead of indexing every
    /// covered position: both tables take the position two past the scan
    /// position the match was found at (`curr_idx + 2`), the long table the
    /// match end minus two, the small table the match end minus one.
    /// Anchors past `insert_max_idx` are dropped; the scan tail never
    /// probes them. Returns the new anchor as a window index.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn emit(
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
                *self
                    .small
                    .get_unchecked_mut(hash_at_log(win, match_end - 1, self.small_log)) =
                    pack_pos(self.win_base + (match_end - 1) as u64);
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
    fn rep_chain(
        &mut self,
        win: &[u8],
        pos_idx: usize,
        ilimit_idx: usize,
        rep: &mut [u32; 3],
    ) -> usize {
        let mut pos = pos_idx;
        while pos <= ilimit_idx {
            let Some(cand_abs) = (self.win_base + pos as u64).checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
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

pub struct MatchGeneratorDriver {
    /// Contiguous history: `win[0]` is absolute position `win_base`.
    /// Capacity holds two windows plus one block, so compaction (which keeps
    /// MAX_WINDOW) runs at ~1x data volume amortized; blocks are read
    /// directly into the spare tail.
    win: Vec<u8>,
    /// Direct-window mode: the window points into caller-owned memory
    /// instead of `win` (see [`MatchGeneratorDriver::adopt_window`]).
    ext: Option<ExtWindow>,
    win_base: u64,
    /// Absolute end of committed data; matching runs up to `block_end`.
    pos: u64,
    block_end: u64,
    /// Start of the literals not yet covered by a sequence.
    anchor: u64,
    /// Absolute start of the last committed block (for `get_last_space`).
    block_start: u64,
    /// Head hash table for the fast/dfast/chain strategies; u32 entries
    /// (see [`pack_pos`]).
    table: Vec<u32>,
    /// Second search table for the two-table strategies (the dfast short
    /// hash or the chain links; see [`Strategy`]), u32 entries like
    /// `table`; empty for the fast strategy.
    chain: Vec<u32>,
    /// Head hash table for the opt strategies, epoch-tagged u64 entries.
    opt_table: Vec<u64>,
    /// The opt strategies' binary-tree ring: two u64 link slots per ring
    /// position; empty outside opt.
    bt: Vec<u64>,
    /// Single-probe 3-byte table for the opt strategies with `min_match == 3`
    /// (libzstd's hashTable3); empty otherwise.
    hash3: Vec<u64>,
    /// Content-tag table of the incompressibility gate (see
    /// [`Matcher::skip_if_incompressible`]): slot = high hash bits of a
    /// sampled 8-byte window, entry = its low 32 bits. Entries are
    /// position-independent content tags, so the table is never reset: a
    /// stale tag can only produce a spurious hit, which selects the
    /// conservative path.
    probe: Vec<u32>,
    /// Price statistics for the opt strategies, persisting across blocks.
    opt_state: OptState,
    /// DP scratch for the opt strategies (~130 KiB; allocated on demand).
    opt_scratch: Option<OptScratch>,
    /// Tree fill point for the opt strategies: positions below it are
    /// already inserted into the binary tree.
    next_update: u64,
    epoch: u64,
    miss_count: usize,
    params: LevelParams,
    /// Repeated-offset history, kept in lockstep with the decoder's
    /// `offset_hist` so repcode probes see the same candidates it will.
    rep: [u32; 3],
    /// Literal-offset sequences still required before repcode references
    /// are decodable: 0 means the history above is known to match the
    /// decoder's. A job that starts mid-frame (see the mt module) cannot
    /// know the decoder's history; three literal offsets shift it fully
    /// into known territory because the update is a plain 3-slot shift.
    rep_pending: u8,
    /// Huffman code lengths of the table that encoded the previous block's
    /// literals (fed back by the block encoder via `note_literal_costs`;
    /// uncovered symbols priced at the 11-bit cap — they force a rebuild
    /// and land at the longest codes). The chain strategy's store gate
    /// prices the literals a match displaces against them. Reset per
    /// frame/job so output bytes stay a function of the frame/job content
    /// alone.
    lit_lens: [u8; 256],
    /// Long-repeat offset probed directly at a multithreaded job's start
    /// (see [`Self::prefill_window`]): 0 = inactive. The fast strategy's
    /// single-probe table cannot hold a periodic repeat's twin — on clumped
    /// data every slot's newest entry is a recent same-hash recurrence, so
    /// the period is unreachable through the hash path until the scan has
    /// self-indexed a full period. The seed sidesteps the table: bytes are
    /// compared at the exact offset, so three seed matches both encode
    /// legally (literal offsets, no repcode gate) and rotate `rep` until it
    /// holds the period, which the repcode probes then ride.
    seed_offset: u32,
    /// Seed matches emitted so far; the seed retires after
    /// [`SEED_MATCHES`].
    seed_hits: u8,
    /// Probe attempts left before an unused seed retires; bounds the cost
    /// of a seed that never matches.
    seed_budget: u32,
    slice_size: usize,
}

/// Borrowed window for the slice-compression path. The caller guarantees the
/// buffer stays alive and unmodified until the matcher is reset or dropped;
/// only the single thread that called `adopt_window` touches it.
struct ExtWindow {
    data: *const u8,
    len: usize,
}

// The pointer never escapes the matcher, and the compress_slice entry points
// keep the borrowed buffer on the same thread's stack for the whole call.
unsafe impl Send for MatchGeneratorDriver {}

/// Resolve the active window (owned or borrowed). A free function so callers
/// can split-borrow `win`/`ext` against `&mut table`.
fn window_slice<'a>(win: &'a [u8], ext: Option<&'a ExtWindow>) -> &'a [u8] {
    match ext {
        None => win,
        // SAFETY: the ExtWindow invariant (caller buffer alive and unchanged
        // until reset) makes this slice valid for as long as the matcher is.
        Some(e) => unsafe { core::slice::from_raw_parts(e.data, e.len) },
    }
}

impl MatchGeneratorDriver {
    /// The window size frames compressed at `level` declare in their
    /// header; usable without constructing an instance.
    pub fn window_for_level(level: Level) -> u64 {
        params_for_level(level).window as u64
    }

    /// Create a matcher whose blocks hold `slice_size` bytes of input (the
    /// zstd block maximum is 128 KiB).
    pub fn new(slice_size: usize) -> Self {
        // Two windows of capacity: compacting down to MAX_WINDOW then only
        // after another MAX_WINDOW of input means the copy_within runs at
        // ~1x data volume instead of once per block.
        Self {
            win: Vec::with_capacity(2 * MAX_WINDOW + slice_size),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            table: alloc::vec![0u32; 1usize << HASH_LOG],
            chain: Vec::new(),
            opt_table: Vec::new(),
            bt: Vec::new(),
            hash3: Vec::new(),
            probe: Vec::new(),
            opt_state: OptState::new(),
            opt_scratch: None,
            next_update: 0,
            // Epoch 0 is the never-valid state of a zeroed table.
            epoch: 1,
            miss_count: 0,
            params: FASTEST_PARAMS,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size,
        }
    }

    /// Create a matcher for direct-window mode: no owned window is allocated
    /// and blocks are handed over through [`Self::adopt_window`] instead of
    /// [`Matcher::block_tail`].
    pub fn new_direct() -> Self {
        Self {
            win: Vec::new(),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            table: alloc::vec![0u32; 1usize << HASH_LOG],
            chain: Vec::new(),
            opt_table: Vec::new(),
            bt: Vec::new(),
            hash3: Vec::new(),
            probe: Vec::new(),
            opt_state: OptState::new(),
            opt_scratch: None,
            next_update: 0,
            epoch: 1,
            miss_count: 0,
            params: FASTEST_PARAMS,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size: 0,
        }
    }

    /// Size the search tables for `level` (no-op when unchanged), so pooled
    /// states re-size at most once per level change.
    fn apply_level(&mut self, level: Level) {
        let params = params_for_level(level);
        if params != self.params {
            // Exactly one table family is live per strategy; switching
            // families drops the other's buffers.
            match params.strategy {
                Strategy::Fast => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = Vec::new();
                },
                Strategy::Dfast(small_log) => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = alloc::vec![0u32; 1usize << small_log];
                },
                Strategy::Chain(chain_log) => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = alloc::vec![0u32; 1usize << chain_log];
                },
                Strategy::Opt(knobs) => {
                    self.opt_table = alloc::vec![EMPTY; 1usize << params.hash_log];
                    // The tree ring: two link slots per ring position.
                    self.bt = alloc::vec![EMPTY; 2usize << knobs.bt_log];
                    self.hash3 = if knobs.hash3_log > 0 {
                        alloc::vec![EMPTY; 1usize << knobs.hash3_log]
                    } else {
                        Vec::new()
                    };
                    self.table = Vec::new();
                    self.chain = Vec::new();
                },
            }
            if !matches!(params.strategy, Strategy::Opt(_)) {
                self.opt_table = Vec::new();
                self.bt = Vec::new();
                self.hash3 = Vec::new();
            }
            if matches!(params.strategy, Strategy::Opt(_)) && self.opt_scratch.is_none() {
                self.opt_scratch = Some(OptScratch::new());
            }
            // The owned window (streaming path) compacts down to the level's
            // window; grow the buffer so block_tail's set_len stays inside
            // the capacity. Direct-window matchers (slice size zero) never
            // use block_tail.
            if self.slice_size > 0 {
                let need = 2 * params.window + self.slice_size;
                if self.win.capacity() < need {
                    self.win.reserve(need - self.win.len());
                }
            }
            self.params = params;
        }
    }

    /// Point the window at caller-owned memory: `data` holds the bytes at
    /// absolute offset `base`. The caller must then declare the block to
    /// match with [`Self::set_block`]. `data` must stay alive and unmodified
    /// until the next `adopt_window` or `reset`.
    pub fn adopt_window(&mut self, data: &[u8], base: u64) {
        debug_assert!(self.ext.is_some() || self.win.is_empty());
        debug_assert_ne!(data, &[][..]);
        self.ext = Some(ExtWindow {
            data: data.as_ptr(),
            len: data.len(),
        });
        self.win_base = base;
    }

    /// Declare `[start, end)` (absolute offsets inside the adopted window)
    /// as the block to match next. Matching restarts at `start`; the history
    /// below it is only a match source.
    pub fn set_block(&mut self, start: u64, end: u64) {
        debug_assert!(start >= self.win_base);
        debug_assert!(end <= self.win_base + self.ext.as_ref().map_or(0, |e| e.len) as u64);
        self.pos = start;
        self.block_start = start;
        self.block_end = end;
        self.anchor = start;
    }

    /// Forbid repcode references until three literal-offset sequences have
    /// rewritten the repeated-offset history. Used by multithreaded jobs
    /// that start mid-frame: the decoder's history there is unknown, and a
    /// repcode reference to a stale value would copy from the wrong place.
    /// Mirrors libzstd's `ZSTD_invalidateRepCodes`.
    pub fn gate_repcodes(&mut self) {
        self.rep_pending = 3;
    }

    /// Prepare the matcher for a multithreaded job and index a strip of
    /// borrowed history — `data` holds the bytes at absolute offset `base` —
    /// into the search tables, so a scan that starts after it can match into
    /// it (the job path adopts the previous job's tail as match window;
    /// without this pass the tables hold no position inside that window and
    /// sequences never reference it). Grid positions are linked
    /// oldest-to-newest, keeping the newest-wins order the walks rely on.
    /// The opt strategies need no explicit fill: their tree fills lazily
    /// from `next_update`, so rewinding it to the strip start makes the
    /// first search index the strip through the regular tree-fill path.
    ///
    /// The u32 head tables are zeroed first, even for an empty strip: the
    /// pooled state carries whatever earlier jobs — this frame or a
    /// previous one — left, and a leftover entry that decodes into the
    /// window with matching bytes is a legal-looking candidate whose
    /// presence depends on which worker ran which job. Clearing makes the
    /// candidates a function of this job's strip and scan alone, so the
    /// frame bytes stay reproducible; libzstd's job path clears its tables
    /// per job for the same reason. The chain strategy's link table needs
    /// no clear: a chain slot is only ever read at a candidate position,
    /// and candidates arise only from the cleared head table or from link
    /// values written this job — stale link slots are unreachable. The
    /// dfast `chain` buffer is a second probed head table and is cleared.
    /// The opt tables need no clear (their entries carry the epoch, bumped
    /// per job).
    pub fn prefill_window(&mut self, data: &[u8], base: u64) {
        clear_table(&mut self.table);
        if !matches!(self.params.strategy, Strategy::Chain(_)) {
            self.chain.fill(0);
        }
        if data.len() < HASH_READ {
            return;
        }
        let last = data.len() - HASH_READ;
        match self.params.strategy {
            Strategy::Opt(_) => {
                self.next_update = self.next_update.min(base);
            },
            Strategy::Fast => {
                // Sparse grid, oldest-to-newest, newest-wins per slot — the
                // single-strategy table has no chain to walk, so a buried
                // twin is unreachable (see PREFILL_STRIDE).
                let table = &mut self.table[..];
                let mut idx = 0;
                while idx < last {
                    insert_at(data, table, idx, base + idx as u64);
                    idx += PREFILL_STRIDE;
                }
                self.acquire_seed(data, last);
            },
            Strategy::Dfast(small_log) => {
                let long_log = self.params.hash_log;
                let long = &mut self.table[..];
                let small = &mut self.chain[..];
                let mut idx = 0;
                while idx < last {
                    // SAFETY: both hashes are masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(base + idx as u64);
                        *long.get_unchecked_mut(hash8_at_log(data, idx, long_log)) = entry;
                        *small.get_unchecked_mut(hash_at_log(data, idx, small_log)) = entry;
                    }
                    idx += PREFILL_STRIDE;
                }
                // The double table is as burial-prone as the fast one for
                // period-long twins: both probes are single-candidate.
                self.acquire_seed(data, last);
            },
            Strategy::Chain(_) => {
                let hash_log = self.params.hash_log;
                let chain_mask = self.chain.len() - 1;
                let table = &mut self.table[..];
                let chain = &mut self.chain[..];
                let mut idx = 0;
                while idx < last {
                    let abs = base + idx as u64;
                    // SAFETY: the hash masks to hash_log bits, the absolute
                    // position to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let h = hash_at_log(data, idx, hash_log);
                        let head = *table.get_unchecked(h);
                        *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                        *table.get_unchecked_mut(h) = pack_pos(abs);
                    }
                    idx += PREFILL_STRIDE;
                }
                // The head table's first hop is as burial-prone as the fast
                // strategy's single probe; seed the walk-independent path.
                self.acquire_seed(data, last);
            },
        }
    }

    #[inline(always)]
    fn idx_of(&self, abs: u64) -> usize {
        (abs - self.win_base) as usize
    }

    /// Scan back from the strip's tail for the nearest position whose 8
    /// bytes equal the tail's and whose preceding [`SEED_AGREE`] bytes agree,
    /// and install its distance as the job-start seed offset (see
    /// `seed_offset`). Word-level repeats die at ~15 agreeing bytes, so only
    /// a genuine long repeat — a period riding the strip, or a duplicated
    /// block — qualifies. The offset is by construction inside the strip,
    /// hence inside the window, so seed matches are always encodable.
    /// `last` is the strip's final insertable index; the anchor bytes
    /// `[last, last + 8)` abut the job start.
    fn acquire_seed(&mut self, data: &[u8], last: usize) {
        let a8 = read8(data, last);
        if let Some(u) = seed_scan(data, last, a8) {
            self.seed_offset = (last - u) as u32;
            self.seed_hits = 0;
            self.seed_budget = SEED_BUDGET;
        }
    }
}

/// Zero a job-start table. Large clears stream megabytes per job through
/// the bus; non-temporal stores skip the ownership read and keep the zero
/// lines from evicting the scan's working set, while small tables stay
/// cache-warm for the probes that follow the clear.
fn clear_table(t: &mut [u32]) {
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
unsafe fn clear_table_avx512(t: &mut [u32]) {
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
const NT_CLEAR_MIN: usize = 1 << 20;

/// Nearest `u < last` with the anchor's 8 bytes and [`SEED_AGREE`]
/// agreeing bytes before it (see [`MatchGeneratorDriver::acquire_seed`]).
/// The 8-byte compare subsumes the 4-byte prefilter the scalar walk used
/// to run first: a matching u64's low half is the u32 at the same index.
fn seed_scan(data: &[u8], last: usize, a8: u64) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    if last >= SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected; every load stays inside
        // data (see the bound derivation in the callee).
        return unsafe { seed_scan_avx512(data, last, a8) };
    }
    let mut u = last;
    while u > 0 {
        u -= 1;
        if read8(data, u) == a8 && seed_agrees(data, u, last) {
            return Some(u);
        }
    }
    None
}

/// Whether the [`SEED_AGREE`] bytes before `u` equal those before `last`:
/// positions below [`SEED_AGREE`] never qualify (the `u >= k` bound).
fn seed_agrees(data: &[u8], u: usize, last: usize) -> bool {
    let mut k = 1;
    while u >= k && k <= SEED_AGREE && data[u - k] == data[last - k] {
        k += 1;
    }
    k > SEED_AGREE
}

/// AVX-512 seed scan: 64-candidate blocks from the anchor down, eight
/// overlapping unaligned 64-byte loads per block. Load `j` compares the
/// broadcast anchor against the positions `b + j + 8t`, so the eight 8-bit
/// masks assemble into one occupancy bit per candidate; blocks run top-down
/// and set bits are taken highest first — exactly the scalar walk's
/// nearest-first order, byte-for-byte. The top block starts at `last - 63`
/// (its highest lane is the anchor itself, skipped by the `u < last`
/// check); every load's final byte lands at `b + 7 + 64 <= last + 8 ==
/// data.len()`, which [`SEED_SCAN_MIN`] guarantees for the lowest block
/// too. The positions below the final block's start run scalar.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512f")]
unsafe fn seed_scan_avx512(data: &[u8], last: usize, a8: u64) -> Option<usize> {
    unsafe {
        use core::arch::x86_64::*;
        let pat = _mm512_set1_epi64(a8 as i64);
        let bottom = (last - 63) & 63;
        let mut b = last - 63;
        loop {
            let mut occ = 0u64;
            for j in 0..8 {
                // SAFETY: b + j + 64 <= data.len() for every block (top block:
                // last - 63 + 7 + 64 == last + 8; lower blocks read lower).
                let v = _mm512_loadu_si512(data.as_ptr().add(b + j).cast());
                // Load j's lane t compares the position b + j + 8t; spread its
                // mask bit t to occupancy bit j + 8t (== u - b). The per-bit
                // loop only runs on nonzero masks — pure overhead on
                // non-repeating data.
                let mut m = _mm512_cmpeq_epi64_mask(v, pat) as u64;
                while m != 0 {
                    let t = m.trailing_zeros();
                    m &= m - 1;
                    occ |= 1 << (j + 8 * t as usize);
                }
            }
            while occ != 0 {
                let bit = occ.ilog2();
                occ ^= 1 << bit;
                let u = b + bit as usize;
                if u < last && seed_agrees(data, u, last) {
                    return Some(u);
                }
            }
            if b == bottom {
                break;
            }
            b -= 64;
        }
        (0..bottom)
            .rev()
            .find(|&u| read8(data, u) == a8 && seed_agrees(data, u, last))
    }
}

impl Matcher for MatchGeneratorDriver {
    fn reset(&mut self, level: Level) {
        self.apply_level(level);
        self.ext = None;
        self.win.clear();
        self.win_base = 0;
        self.pos = 0;
        self.block_end = 0;
        self.anchor = 0;
        self.block_start = 0;
        // Stale opt-table entries from previous frames fail the epoch check;
        // the u32 tables need no reset (entries decode against the scanning
        // position and die on the window-range check).
        self.epoch += 1;
        if self.epoch > 0xffff {
            self.opt_table.fill(EMPTY);
            self.bt.fill(EMPTY);
            self.hash3.fill(EMPTY);
            self.epoch = 1;
        }
        self.miss_count = 0;
        // Matches the decoder's per-frame offset_hist reset.
        self.rep = [1, 4, 8];
        self.rep_pending = 0;
        self.lit_lens = DEFAULT_LIT_LENS;
        self.seed_offset = 0;
        self.seed_hits = 0;
        self.seed_budget = 0;
        // The opt parser re-seeds its statistics and re-fills its tree.
        self.next_update = 0;
        self.opt_state.reset();
    }

    fn window_size(&self) -> u64 {
        self.params.window as u64
    }

    fn repcode_snapshot(&self) -> [u32; 3] {
        self.rep
    }

    fn restore_repcode(&mut self, rep: [u32; 3]) {
        self.rep = rep;
    }

    fn note_literal_costs(&mut self, lengths: &[u8; 256]) {
        // Uncovered symbols price at the format's 11-bit cap: emitting one
        // forces a table rebuild and lands it at the longest codes.
        for (i, &nb) in lengths.iter().enumerate() {
            self.lit_lens[i] = if nb == 0 {
                11
            } else {
                nb
            };
        }
    }

    fn block_tail(&mut self) -> &mut [u8] {
        debug_assert!(self.ext.is_none(), "block_tail is owned-window only");
        if self.win.len() + self.slice_size > self.win.capacity() {
            let keep = self.win.len().min(self.params.window);
            let drop = self.win.len() - keep;
            self.win.copy_within(drop.., 0);
            self.win.truncate(keep);
            self.win_base += drop as u64;
        }
        let old = self.win.len();
        // SAFETY: the capacity check above guarantees old + slice_size fits.
        // The caller writes the block into the returned slice and passes the
        // written count to commit_block, which shrinks the length back to
        // exactly that, so the never-written tail bytes are never observed.
        unsafe { self.win.set_len(old + self.slice_size) };
        &mut self.win[old..]
    }

    fn get_last_space(&mut self) -> &[u8] {
        &window_slice(&self.win, self.ext.as_ref())[self.idx_of(self.block_start)..]
    }

    fn commit_block(&mut self, read_bytes: usize) {
        debug_assert!(self.ext.is_none(), "commit_block is owned-window only");
        let old_len = self.win.len() - self.slice_size;
        // SAFETY: the caller wrote read_bytes bytes into the tail slice
        // handed out by block_tail.
        unsafe { self.win.set_len(old_len + read_bytes) };
        // Matching restarts at the head of the freshly committed block; the
        // window behind it is only a match source.
        self.pos = self.win_base + old_len as u64;
        self.block_start = self.pos;
        self.block_end = self.win_base + self.win.len() as u64;
        self.anchor = self.pos;
    }

    fn start_matching(&mut self, mut handle_sequence: impl for<'a> FnMut(Sequence<'a>)) {
        let mut literals = Vec::new();
        let mut seqs: Vec<SeqWord> = Vec::new();
        self.start_matching_codes(&mut literals, &mut seqs);
        // Rebuild the interleaved callback order from the collected
        // buffers: each sequence's ll literals came right before it.
        // Zero-sequence blocks staged nothing; their literals are the block
        // itself, handed over straight from the window.
        let zero_seq = seqs.is_empty();
        let mut offset = 0usize;
        for &word in &seqs {
            let (ll, ml, of) = decode_packed(word.codes, word.add);
            let lits = &literals[offset..offset + ll as usize];
            offset += ll as usize;
            handle_sequence(Sequence::Triple {
                literals: lits,
                offset: of as usize,
                match_len: ml as usize,
            });
        }
        if zero_seq {
            handle_sequence(Sequence::Literals {
                literals: self.get_last_space(),
            });
        } else {
            handle_sequence(Sequence::Literals {
                literals: &literals[offset..],
            });
        }
    }

    fn start_matching_codes(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        match self.params.strategy {
            Strategy::Fast => self.start_matching_fast(literals, seqs),
            Strategy::Dfast(_) => self.start_matching_dfast(literals, seqs),
            Strategy::Chain(_) => self.start_matching_chain(literals, seqs),
            Strategy::Opt(knobs) => self.start_matching_opt(knobs, literals, seqs),
        }
    }

    fn skip_if_incompressible(&mut self) -> bool {
        // Only the opt strategies may gate: their skipped positions stay
        // matchable through the tree's lazy fill (next_update never
        // advances for a skipped block), so gating costs no ratio when a
        // later block duplicates a gated one. The table strategies would
        // lose the block as match history.
        if !matches!(self.params.strategy, Strategy::Opt(_)) {
            return false;
        }
        let win = window_slice(&self.win, self.ext.as_ref());
        let block = &win[(self.block_start - self.win_base) as usize..];
        let total = block.len();
        if total < GATE_MIN_BLOCK {
            return false;
        }
        if self.probe.is_empty() {
            self.probe.resize(1 << GATE_PROBE_LOG, 0);
        }
        let probe = &mut self.probe;
        // Strided repeat probe: a hit means a colliding 47-bit content hash
        // exists somewhere in the history sampled so far — near-certainly
        // the same 8 bytes — so the block may be matchable and must not be
        // gated. The odd stride avoids period locking against structured
        // inputs. Any match region of a stride's length or more holds a
        // sample, so only short sparse matches escape detection — those are
        // worth under a percent of the block, the price the threshold below
        // already accepted.
        let stride = (total >> 11) | 1;
        let mut hits = 0u32;
        let mut i = 0usize;
        while i + HASH_READ <= total {
            let h = read8(block, i).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
            let slot = (h >> (64 - GATE_PROBE_LOG)) as usize;
            let tag = h as u32;
            if probe[slot] == tag {
                hits += 1;
            } else {
                probe[slot] = tag;
            }
            i += stride;
        }
        if hits != 0 {
            return false;
        }
        // Exact byte histogram (four lanes break the store-forward chain):
        // the entropy decision must not ride on a sample's noise band —
        // a block wrongly kept below the bar forces a full opt parse plus
        // the window catch-up fill, hundreds of times the histogram cost.
        let mut lanes = [[0u32; 256]; 4];
        let (chunks, remainder) = block.as_chunks::<4>();
        for chunk in chunks {
            lanes[0][chunk[0] as usize] += 1;
            lanes[1][chunk[1] as usize] += 1;
            lanes[2][chunk[2] as usize] += 1;
            lanes[3][chunk[3] as usize] += 1;
        }
        for &b in remainder {
            lanes[0][b as usize] += 1;
        }
        let total_f = total as f64;
        let mut entropy_bits = 0.0f64;
        let mut distinct = 0u32;
        for (s, &c0) in lanes[0].iter().enumerate() {
            let c = c0 + lanes[1][s] + lanes[2][s] + lanes[3][s];
            if c > 0 {
                distinct += 1;
                entropy_bits -= c as f64 * crate::fse::fse_encoder::approx_log2(c as f64 / total_f);
            }
        }
        // Miller-Madow bias correction, as in the literals entropy gate.
        let bits_per_byte =
            entropy_bits / total_f + (distinct as f64 - 1.0) * 0.7213_4752_0559_1157 / total_f;
        // Well past the literals gate's reject floor: huffman on literals
        // this dense saves under 0.4% before the table description, and the
        // probe above already ruled out matches worth more.
        if bits_per_byte < 7.97 {
            return false;
        }
        self.skip_matching();
        true
    }

    fn skip_matching(&mut self) {
        // Only called for RLE blocks so far: every 5-byte window in a
        // uniform run hashes to the same slot, so indexing each byte just
        // rewrites one table entry. The first position covers that slot;
        // future probes into the run resolve through it or the repcode
        // chain. The opt strategies skip even that: their tree fills
        // lazily from `next_update`, which stays at the block start and
        // covers the run on the next block's fill.
        let idx = (self.block_start - self.win_base) as usize;
        let win = window_slice(&self.win, self.ext.as_ref());
        if idx + HASH_READ <= win.len() {
            match self.params.strategy {
                Strategy::Opt(_) => {},
                Strategy::Chain(log) => {
                    let h = hash_at_log(win, idx, log);
                    // SAFETY: h is masked to log bits, the absolute block
                    // start to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let head = *self.table.get_unchecked(h);
                        let chain_mask = self.chain.len() - 1;
                        *self
                            .chain
                            .get_unchecked_mut(self.block_start as usize & chain_mask) = head;
                        *self.table.get_unchecked_mut(h) = pack_pos(self.block_start);
                    }
                },
                Strategy::Dfast(small_log) => {
                    let hl = hash8_at_log(win, idx, self.params.hash_log);
                    let hs = hash_at_log(win, idx, small_log);
                    // SAFETY: both hashes masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(self.block_start);
                        *self.table.get_unchecked_mut(hl) = entry;
                        *self.chain.get_unchecked_mut(hs) = entry;
                    }
                },
                Strategy::Fast => {
                    insert_at(win, &mut self.table, idx, self.block_start);
                },
            }
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
    }
}

impl MatchGeneratorDriver {
    /// The single-probe `fast` strategy loop (level [`Level::Fastest`]).
    fn start_matching_fast(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        // Hot state lives in locals for the whole loop: the emit helpers
        // used to take `&mut self`, which forced a reload of every cursor
        // from memory after each match.
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_end = self.block_end;
        // saturating: tiny first blocks never reach an emit, so the bound is
        // never consulted when win.len() < HASH_READ.
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer: routing
        // them through the emit context's slice field kept the pointer
        // stack-resident (a re-load per access; see the dfast loop's note on
        // the same SROA failure).
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = self.table.as_mut_ptr();
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            win_base,
            insert_max,
        };
        let max_window = MAX_WINDOW as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut miss_count = self.miss_count;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let hash_read = HASH_READ as u64;

        // Resolve a table entry to a window index. The truncated absolute
        // position is rebuilt as a *distance* from the scanning position:
        // `dist = pos - (entry - 1)` wraps to a huge u64 for both the empty
        // sentinel (dist = pos + 1 > reach) and candidates from the previous
        // 4 GiB cycle, so the window-range check `dist <= reach` (reach =
        // min(pos - win_base, max_window), i.e. `pos - lo`) rejects all
        // three failure modes in one compare. Invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip` then rejects them with one
        // predictable branch instead of a three-way check per probe
        // (libzstd's selectAddr trick).
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // dist = pos - entry + 1 without a debug panic on entry == 0
            // (the empty sentinel wraps to dist = pos + 1 > reach).
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Two-position pipeline (libzstd's ip0/ip1 interleave): the hash and
        // table entry of the next position are prepared before the current
        // one is probed, so the hash multiply and table-load latencies of
        // both positions overlap instead of chaining behind every probe. A
        // pair advances by the miss step once both positions miss; the
        // second position is dropped at the block tail.
        // The miss step can jump past the block end, so the guard must
        // saturate instead of relying on pos < block_end.
        //
        // The body exists in two phases. The seeded phase carries the
        // job-start state (`seed_offset`/`seed_hits`/`seed_budget`,
        // `rep_pending`), which only ever winds down; once it converges the
        // steady phase runs a body with all of it folded out. In one loop
        // those four values stayed live across every iteration (bulk-ST and
        // post-convergence MT scanning carried pure dead weight), and the
        // scan paid for them in spill traffic — a quarter of its cycles sat
        // on stack reloads. `$gated` is a literal, so the phase-only code
        // paths are constant-folded away in the steady instantiation.
        //
        // A steady/tail loop split that folds `pair_len` to the constant 2
        // (full pairs while a pair fits, then a single-position epilogue
        // instantiation) costs 3% fewer instructions but regressed
        // text.fastest wall by 7% — the third macro instantiation displaces
        // the hot blocks; do not retry without a code-layout story.
        macro_rules! scan_fast {
            ($restart:lifetime, $gated:literal) => {
                let idx0 = (pos - win_base) as usize;
                // One u64 load per position feeds the hash, the 4-byte
                // probe prefilter (its low half) and the repcode compares.
                let v0 = read8(win, idx0);
                let h0 = hash5(v0);
                // SAFETY: the hash masks to HASH_LOG bits and the table always
                // holds 1 << HASH_LOG slots (see insert_at).
                let prev0 = unsafe { *table_ptr.add(h0) };
                let cur0 = v0 as u32;
                let mut pair_len = 1u64;
                let mut idx1 = idx0;
                let mut h1 = h0;
                let mut prev1 = prev0;
                let mut cur1 = cur0;
                if block_end - pos > hash_read {
                    pair_len = 2;
                    idx1 = idx0 + 1;
                    let v1 = read8(win, idx1);
                    h1 = hash5(v1);
                    // SAFETY: as above.
                    prev1 = unsafe { *table_ptr.add(h1) };
                    cur1 = v1 as u32;
                }
                // Store after both lookups so each probe sees the pre-store
                // entry (newest-wins).
                // SAFETY: as above.
                unsafe {
                    *table_ptr.add(h0) = pack_pos(pos);
                }
                if pair_len == 2 {
                    // SAFETY: as above.
                    unsafe {
                        *table_ptr.add(h1) = pack_pos(pos + 1);
                    }
                }

                // Probe the first position. Repcode candidate first (mirrors
                // zstd's fast strategy: rep[0] only); a repcode match needs
                // at least one pending literal so of_value 1 stays encodable: with
                // literals pending the probe runs at the current position,
                // otherwise one byte ahead so that byte becomes the literal.
                // The probe bound is window-relative (`pidx >= rep[0]` is
                // `probe >= win_base + rep[0]` with win_base folded out).
                let mut rep1_armed = false;
                {
                    let anchor_idx = (anchor - win_base) as usize;
                    // A gated job start must not probe repcodes (unknown
                    // decoder history); probe 0 sits below the bound, which
                    // folds the underflow guard into the same single
                    // comparison.
                    let pidx = if $gated && rep_pending != 0 {
                        0
                    } else if idx0 == anchor_idx {
                        idx0 + 1
                    } else {
                        idx0
                    };
                    // OR-folded repcode prefilter for the pair: both
                    // positions' compares collapse into one unpredictable
                    // branch (adjacent-position rep hits are correlated, so
                    // the fold predicts no worse than either compare alone).
                    // A taken fold whose first position misses arms the
                    // second position's full probe — its compare is then
                    // known good. Probe and emission order are unchanged,
                    // so the output is byte-identical to the two-compare
                    // form. `cur1` reuses the already-loaded second position
                    // (read4(idx1) is its low half); 1 = never hit, covering
                    // both the tail pair (idx1 falls back to idx0) and the
                    // second position sitting below the window bound.
                    if pidx >= rep[0] as usize {
                        let r = rep[0] as usize;
                        let a = read4(win, pidx - r) ^ read4(win, pidx);
                        let b = if pair_len == 2 && idx1 >= r {
                            read4(win, idx1 - r) ^ cur1
                        } else {
                            1
                        };
                        if a == 0 || b == 0 {
                            if a == 0 {
                                let mut cand = pidx - r;
                                let mut ml = extend_match(win, pidx, cand);
                                if ml >= MIN_MATCH {
                                    let mut start = pidx;
                                    // Extend backwards into the pending literals;
                                    // the offset (pidx - cand) stays constant.
                                    while start > anchor_idx + 1
                                        && cand > 0
                                        && win[cand - 1] == win[start - 1]
                                    {
                                        cand -= 1;
                                        start -= 1;
                                        ml += 1;
                                    }
                                    anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                                    pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                                    // The chain's matches advance the cursor too.
                                    anchor = pos;
                                    miss_count = 0;
                                    continue $restart;
                                }
                            } else {
                                rep1_armed = true;
                            }
                        }
                    }

                    // Seed-offset probe (job starts only): direct byte compare
                    // at the prefill-detected long-repeat offset — the table
                    // cannot serve this candidate (see `seed_offset`). Emits as
                    // a plain literal-offset match, so it is legal under the
                    // repcode gate; each emit rotates `rep` toward holding the
                    // offset, and after SEED_MATCHES the repcode probes ride it
                    // without further seed help.
                    if $gated && seed_offset != 0 {
                    let ci = (pos - seed_offset as u64 - win_base) as usize;
                    if read4(win, ci) == cur0 {
                        let mut ml = extend_match(win, idx0, ci);
                        // Same bar as the hash path: below 6 the sequence
                        // overhead eats the match, and the offset-gain gate
                        // keeps a far seed honest about its worth.
                        if ml >= 6 && pays_for_offset(ml, idx0, ci, false) {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx0;
                            let mut ci = ci;
                            // Extend backwards into the pending literals;
                            // the offset stays constant.
                            while start > anchor_idx && ci > 0 && win[ci - 1] == win[start - 1] {
                                ci -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - ci + 3) as u32;
                            anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            pos = if rep_pending == 0 {
                                emit.rep1_chain(win, anchor, block_end, &mut rep)
                            } else {
                                anchor
                            };
                            anchor = pos;
                            miss_count = 0;
                            continue $restart;
                        }
                    }
                    // A seed that never pays retires on its budget instead
                    // of comparing dead bytes for the whole job.
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                let reach = (pos - win_base).min(max_window);
                let mut cand0 = resolve(prev0, pos, reach, idx0);
                if cand0 != idx0 && read4(win, cand0) == cur0 {
                    let mut ml = extend_match(win, idx0, cand0);
                    // A hash match already spans 5 bytes; below 6 the
                    // sequence overhead roughly equals the literals
                    // it covers, and rejecting it lets the scan try
                    // the next position where a longer match may
                    // start.
                    if ml >= 6 {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx0;
                        // Extend backwards into the pending literals;
                        // the offset (idx0 - cand) stays constant.
                        while start > anchor_idx && cand0 > 0 && win[cand0 - 1] == win[start - 1] {
                            cand0 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - cand0 + 3) as u32;
                        anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                        // A literal offset shifts the decoder's history
                        // one slot down; after the third one a job-start
                        // gate has fully converged and repcode use is
                        // safe again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            pos = if rep_pending == 0 {
                                emit.rep1_chain(win, anchor, block_end, &mut rep)
                            } else {
                                anchor
                            };
                        } else {
                            pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                        }
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }
            }

            // Probe the second position through the entry prepared above.
            // `pos1 == anchor` is impossible (anchor <= pos < pos + 1), so
            // the pending-literal select folds away here. The repcode probe
            // is armed by the folded prefilter above (which also applies the
            // window bound and the job-start gate), so it runs compare-free.
            if pair_len == 2 {
                if rep1_armed {
                    let mut cand = idx1 - rep[0] as usize;
                    let mut ml = extend_match(win, idx1, cand);
                    if ml >= MIN_MATCH {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        while start > anchor_idx + 1
                            && cand > 0
                            && win[cand - 1] == win[start - 1]
                        {
                            cand -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                        pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }

                let pos1 = win_base + idx1 as u64;
                let reach1 = (pos1 - win_base).min(max_window);
                let mut cand1 = resolve(prev1, pos1, reach1, idx1);
                if cand1 != idx1 && read4(win, cand1) == cur1 {
                    let mut ml = extend_match(win, idx1, cand1);
                    if ml >= 6 {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        while start > anchor_idx && cand1 > 0 && win[cand1 - 1] == win[start - 1] {
                            cand1 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - cand1 + 3) as u32;
                        anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                        // A literal offset shifts the decoder's history
                        // one slot down; after the third one a job-start
                        // gate has fully converged and repcode use is
                        // safe again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            pos = if rep_pending == 0 {
                                emit.rep1_chain(win, anchor, block_end, &mut rep)
                            } else {
                                anchor
                            };
                        } else {
                            pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                        }
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }
            }

            // Both positions missed: grow the probe step on long literal
            // runs so incompressible data does not pay a full hash per
            // byte. The step scales the whole pair so the probes-per-byte
            // density matches the single-position loop at every step size.
            // Faster-growing than libzstd's anchor-distance grid: our
            // single-probe table loses its far matches to overwrites when
            // incompressible gaps are probed densely (measured: json at
            // Fastest +9% size), and skipping is what keeps sparse-match
            // corpora fast.
            miss_count += pair_len as usize;
            let step = 1 + (miss_count >> 2).min(255) as u64;
            pos += pair_len * step;
            };
        }
        // Seeded phase: job starts only (bulk-ST skips it entirely); each
        // emit re-checks convergence at the head, and the values only ever
        // wind down.
        'seeded: while (rep_pending != 0 || seed_offset != 0)
            && block_end.saturating_sub(pos) >= hash_read
        {
            scan_fast!('seeded, true);
        }
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            scan_fast!('restart, false);
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        // A zero-sequence block stages nothing: its literals are exactly the
        // block, which the caller reads through get_last_space() instead of
        // paying a whole-block copy into and out of the scratch buffer.
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }

    /// The double-hash strategy loop (level [`Level::Fast`], libzstd's
    /// dfast): one 8-byte long-hash probe and one 5-byte short-hash probe
    /// per position — no chain walk, no lazy deferral. A two-position
    /// pipeline overlaps the hash multiplies and table loads, a short hit
    /// is upgraded by the long probe prepared for the next position, and
    /// matched ranges re-seed both tables through a few anchors (see
    /// [`DfastEmit::emit`]). The miss step grows with the literal run
    /// (one per 256 B, libzstd's `kSearchStrength` grid).
    #[allow(clippy::too_many_lines)]
    fn start_matching_dfast(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let long_log = self.table.len().trailing_zeros();
        let small_log = self.chain.len().trailing_zeros();
        let win_base = self.win_base;
        let block_len = (self.block_end - win_base) as usize;
        // Probing hashes 8 bytes, so the last probeable window index; the
        // insert bound coincides with it (the window ends at the block), so
        // one shared limit serves both.
        let limit_idx = block_len
            .saturating_sub(HASH_READ)
            .min(win.len().saturating_sub(HASH_READ));
        let insert_max_idx = limit_idx;
        let max_window = self.params.window as u64;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let mut anchor_idx = (self.anchor - win_base) as usize;
        let mut ip_idx = (self.pos - win_base) as usize;
        // The scan's own table writes go through raw pointers: routing them
        // through the emit context's slice fields kept the pointers,
        // log shifts and tag stack-resident (a re-load per access, the
        // classic SROA failure at this loop's live-value count).
        // SAFETY: derived here, before the context below takes its
        // borrows; both address the same memory, and the loop and the
        // emit helpers never access a slot concurrently.
        let long_ptr: *mut u32 = self.table.as_mut_ptr();
        let small_ptr: *mut u32 = self.chain.as_mut_ptr();
        let mut emit = DfastEmit {
            long: &mut self.table[..],
            small: &mut self.chain[..],
            literals,
            seqs,
            win_base,
            insert_max_idx,
            long_log,
            small_log,
        };

        // Resolve a table entry to a window index. Same distance trick as
        // the fast loop's `resolve`: `dist = pos - (entry - 1)` wraps huge
        // for the empty sentinel and previous-4-GiB-cycle entries, so
        // `dist <= reach` (reach = min(pos - win_base, max_window)) rejects
        // every failure mode in one compare, and invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip_idx` then rejects them with
        // predictable branches (libzstd's selectAddr trick).
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // Same double-wrapping form as the fast loop's `resolve`.
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Outer loop: one pass per emitted match; re-entering resets the
        // miss step. The inner loop walks single positions until a match
        // or the block tail. Like the fast loop, the body runs in two
        // phases (see `scan_fast`): the seeded phase carries the job-start
        // seed/gate state, the steady phase folds it out — this loop's live
        // set is even larger (two tables, two hash logs), so the dead
        // weight cost more here.
        macro_rules! scan_dfast {
            ($outer:lifetime, $gated:literal) => {
                // The short-match upgrade probes at ip + 1, so a pass needs a
                // full position pair inside the block.
                if ip_idx + 1 > limit_idx {
                    break $outer;
                }
                let mut ip1_idx = ip_idx + 1;
                let mut hl0 = hash8_at_log(win, ip_idx, long_log);
                // SAFETY: hl0 is masked to the long table size.
                let mut entry_l0 = unsafe { *long_ptr.add(hl0) };

                loop {
                let hs0 = hash_at_log(win, ip_idx, small_log);
                // SAFETY: hs0 is masked to the small table size.
                let entry_s0 = unsafe { *small_ptr.add(hs0) };
                let pos_abs = win_base + ip_idx as u64;
                // Oldest usable candidate age: within the level window and
                // inside the live window buffer.
                let reach = (pos_abs - win_base).min(max_window);
                // Insert after both lookups, before probing (newest-wins).
                // SAFETY: both hashes masked to their tables' sizes.
                unsafe {
                    let entry = pack_pos(pos_abs);
                    *long_ptr.add(hl0) = entry;
                    *small_ptr.add(hs0) = entry;
                }

                // Repcode pre-probe one byte ahead: the probed byte stays a
                // pending literal, so of_value 1 encodes rep0 instead of a
                // swap, and no backward extension may consume it. Gated job
                // starts skip the probe (unknown decoder history). The bound
                // is window-relative: `probe >= rep[0]` is
                // `win_base + probe >= win_base + rep[0]` with win_base
                // folded out.
                if !($gated && rep_pending != 0) {
                    let probe = ip_idx + 1;
                    if probe >= rep[0] as usize {
                        let cand = probe - rep[0] as usize;
                        if read4(win, cand) == read4(win, probe) {
                            let ml = extend_match(win, probe, cand);
                            debug_assert!(ml >= MIN_MATCH);
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, probe, ml, 1, &mut rep);
                            ip_idx = emit.rep_chain(win, anchor_idx, limit_idx, &mut rep);
                            // The chain's matches advance the anchor too.
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                }

                // Seed-offset probe (job starts only): direct byte compare
                // at the prefill-detected long-repeat offset — neither
                // single-candidate table can serve it (see `seed_offset`).
                // Mirrors the fast loop's twin block.
                if $gated && seed_offset != 0 {
                    let ci = (pos_abs - seed_offset as u64 - win_base) as usize;
                    if read4(win, ci) == read4(win, ip_idx) {
                        let mut ml = extend_match(win, ip_idx, ci);
                        if ml >= 6 && pays_for_offset(ml, ip_idx, ci, false) {
                            let mut start = ip_idx;
                            let mut c = ci;
                            while start > anchor_idx && c > 0 && win[c - 1] == win[start - 1] {
                                c -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - c + 3) as u32;
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain(win, anchor_idx, limit_idx, &mut rep)
                            } else {
                                anchor_idx
                            };
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                let hl1 = hash8_at_log(win, ip1_idx, long_log);

                // Long probe: a full 8-byte match at the long-hash candidate.
                {
                    let cand = resolve(entry_l0, pos_abs, reach, ip_idx);
                    if cand != ip_idx && read8(win, cand) == read8(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        // Backward catch-up into the pending literals; the
                        // offset (start - c) stays constant.
                        while start > anchor_idx && c > 0 && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if ip1_idx < anchor_idx {
                            // The emit moved the anchor past ip1, so the
                            // match covers it: indexing it cannot pollute
                            // later probes.
                            // SAFETY: hl1 is masked to the long table size.
                            unsafe {
                                *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                            }
                        }
                        // A literal offset shifts the decoder's history one
                        // slot down; after the third one a job-start gate
                        // has fully converged and repcode use is safe
                        // again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain(win, anchor_idx, limit_idx, &mut rep)
                            } else {
                                anchor_idx
                            };
                        } else {
                            ip_idx = emit.rep_chain(win, anchor_idx, limit_idx, &mut rep);
                        }
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue $outer;
                    }
                }

                // SAFETY: hl1 is masked to the long table size.
                let entry_l1 = unsafe { *long_ptr.add(hl1) };

                // Short probe: 4 bytes at the short-hash candidate, upgraded
                // by the long probe prepared for the next position.
                {
                    let cand = resolve(entry_s0, pos_abs, reach, ip_idx);
                    if cand != ip_idx && read4(win, cand) == read4(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let pos1_abs = win_base + ip1_idx as u64;
                        let reach1 = (pos1_abs - win_base).min(max_window);
                        let c1 = resolve(entry_l1, pos1_abs, reach1, ip1_idx);
                        if c1 != ip1_idx && read8(win, c1) == read8(win, ip1_idx) {
                            let l1len = extend_match(win, ip1_idx, c1);
                            if l1len > ml {
                                start = ip1_idx;
                                c = c1;
                                ml = l1len;
                            }
                        }
                        while start > anchor_idx && c > 0 && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if ip1_idx < anchor_idx {
                            // The emit moved the anchor past ip1, so the
                            // match covers it: indexing it cannot pollute
                            // later probes.
                            // SAFETY: hl1 is masked to the long table size;
                            // see the long probe above.
                            unsafe {
                                *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                            }
                        }
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain(win, anchor_idx, limit_idx, &mut rep)
                            } else {
                                anchor_idx
                            };
                        } else {
                            ip_idx = emit.rep_chain(win, anchor_idx, limit_idx, &mut rep);
                        }
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue $outer;
                    }
                }

                // Miss: advance the pair; the step grows with the literal
                // run (one per 256 B), so compressible data keeps probing
                // every byte while incompressible runs accelerate.
                ip_idx = ip1_idx;
                ip1_idx += 1 + ((ip1_idx - anchor_idx) >> 8);
                hl0 = hl1;
                entry_l0 = entry_l1;
                if ip1_idx > limit_idx {
                    break;
                }
            }
            // The pair left the block: nothing left to probe.
            break $outer;
            };
        }
        // Seeded phase (job starts only; bulk-ST skips it entirely), then
        // the steady phase — same convergence contract as `scan_fast`.
        'seeded: while (rep_pending != 0 || seed_offset != 0) && ip_idx < limit_idx {
            scan_dfast!('seeded, true);
        }
        'outer: loop {
            scan_dfast!('outer, false);
        }
        if !emit.seqs.is_empty() && anchor_idx < block_len {
            emit.literals.extend_from_slice(&win[anchor_idx..block_len]);
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }

    /// The hash-chain strategy loop (levels above [`Level::Fastest`]):
    /// walk same-hash candidates through the chain table up to the level's
    /// search depth, prefer repcode candidates (they encode nearly free),
    /// and defer emission across up to `lazy_depth` further positions when
    /// a longer match may start there — libzstd's lazy family.
    fn start_matching_chain(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let chain = &mut self.chain[..];
        let chain_mask = chain.len() - 1;
        let win_base = self.win_base;
        let block_end = self.block_end;
        let hash_log = self.params.hash_log;
        let search_depth = self.params.search_depth as usize;
        let lazy_depth = self.params.lazy_depth;
        let max_window = self.params.window as u64;
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer (see the
        // fast loop's note on the same SROA failure); emits go through the
        // context.
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = self.table.as_mut_ptr();
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            win_base,
            insert_max,
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

        // Chain-walk search from the hash head at window index `idx`,
        // returning the longest match's (length, candidate window index).
        // Every candidate is beat-checked, so stale chain links (see the
        // sparse fill in rep1_chain) only cost probes, never correctness.
        let search = |win: &[u8], chain: &[u32], idx: usize| -> (usize, usize) {
            // SAFETY: hash_at_log masks to hash_log bits and the table holds
            // 1 << hash_log slots.
            let h = hash_at_log(win, idx, hash_log);
            let mut entry = unsafe { *table_ptr.add(h) };
            let pos_abs = win_base + idx as u64;
            // Oldest usable candidate age: within the level window and
            // inside the live window buffer.
            let lo = pos_abs.saturating_sub(max_window).max(win_base);
            let mut best_len = 0usize;
            let mut best_cand = usize::MAX;
            let mut tried = 0usize;
            while tried < search_depth {
                // An empty or out-of-window link ends the walk (the link
                // targets only grow older).
                let Some(cand_abs) = unpack_pos(entry, pos_abs) else {
                    break;
                };
                if cand_abs < lo {
                    break;
                }
                let cand = (cand_abs - win_base) as usize;
                // Beat-check (libzstd's "potentially better" read): the 4
                // bytes ending at best_len+1 decide whether the candidate
                // can strictly improve, so most hash collisions reject on
                // one load instead of a full extend. With no best yet the
                // probe sits at 0, the plain first-4 compare. The probe
                // stays inside the block: still looping means best_len is
                // short of the block end (the break below fires otherwise),
                // so [probe, probe+4) ends at most at the block end.
                let probe = best_len.saturating_sub(3);
                if read4(win, cand + probe) == read4(win, idx + probe) {
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
                entry = unsafe { *chain.get_unchecked(cand_abs as usize & chain_mask) };
            }
            (best_len, best_cand)
        };

        while block_end.saturating_sub(pos) >= hash_read {
            let idx = (pos - win_base) as usize;
            let (mut best_len, mut best_cand) = search(win, chain, idx);

            // Repcode probe first when armed: with literals pending it runs
            // at the current position, otherwise one byte ahead so that
            // byte becomes the pending literal and of_value 1 stays
            // encodable (mirrors the fast loop). The offset encodes nearly
            // free, so bias it past the chain match.
            let mut rep_hit = false;
            if rep_pending == 0 {
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
                        if ml >= MIN_MATCH && ml + 3 > best_len {
                            best_len = ml;
                            best_cand = cand;
                            rep_hit = true;
                            pos = probe;
                        }
                    }
                }
            }

            // Seed-offset probe (job starts only): the prefill-detected
            // long-repeat offset as a direct candidate, before the chain's
            // candidates — clumped data buries the period twin too deep in
            // the chain for the depth-limited walk to reach (see
            // `seed_offset`). Mirrors the fast loop's twin block; the pays
            // gate below keeps a far seed honest.
            let mut seed_hit = false;
            if seed_offset != 0 {
                let ci = (pos - seed_offset as u64 - win_base) as usize;
                if read4(win, ci) == read4(win, idx) {
                    let ml = extend_match(win, idx, ci);
                    if ml >= 6 && ml + 3 > best_len {
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

            // Insert this position behind the probe (newest-wins), linking
            // the chain to the previous head. The chain slot key is the
            // absolute position — what the walk resolves candidates with.
            // SAFETY: both indices are masked to their tables' sizes.
            unsafe {
                let h = hash_at_log(win, idx, hash_log);
                let head = *table_ptr.add(h);
                *chain.get_unchecked_mut(pos as usize & chain_mask) = head;
                *table_ptr.add(h) = pack_pos(pos);
            }

            if best_len < MIN_MATCH
                || !pays_for_offset_lit(win, idx, best_len, best_cand, rep_hit, lit_lens)
            {
                // Grow the probe step on long literal runs (same policy as
                // the fast loop) so incompressible data does not pay a full
                // chain walk per byte. Faster-growing than libzstd's
                // anchor-distance grid: our per-probe chain walk is dearer,
                // and skipping over sparse-match gaps is what keeps the
                // Balanced levels fast on them.
                miss_count += 1;
                pos += 1 + (miss_count >> 2).min(255) as u64;
                continue;
            }
            miss_count = 0;

            // Lazy evaluation, ported to libzstd lazy's alternating gain
            // walk: each next position competes on offset-aware gain — a
            // match's distance is priced at its highbit (~4 bits per length
            // byte), so a much nearer match of near-equal length displaces a
            // long-range one (the selection error that cost text ~2% ratio:
            // pure-length comparison kept far chain candidates). A repcode
            // probe rides along (rep offsets price ~free). The walk advances
            // while some probe improves; a position that fails the depth-1
            // margins gets one second-chance probe at depth-2 margins before
            // giving up — exactly libzstd's ZSTD_lazy/ZSTD_lazy2 split.
            // Long-enough matches skip straight to emission.
            let mut best_pos = pos;
            let mut best_price = if rep_hit {
                0
            } else {
                price_of((best_pos - win_base) as usize, best_cand)
            };
            if best_len < 64 {
                // The incumbent's displaced-literal value (see lit_value);
                // recomputed whenever the incumbent changes so both sides
                // of the walk's gain comparisons price in fed-back literal
                // code lengths instead of the flat 4 bits/byte.
                let mut best_v = lit_value(win, (best_pos - win_base) as usize, best_len, lit_lens);
                'lazy: loop {
                    for (attempt, &(rep_mul, rep_m, search_m)) in
                        [(3i32, 1i32, 4i32), (4, 1, 7)].iter().enumerate()
                    {
                        if attempt > 0 && lazy_depth < 2 {
                            // Second-chance margins are the depth-2 strategy.
                            break;
                        }
                        let p2 = pos + 1;
                        if block_end.saturating_sub(p2) < hash_read {
                            break 'lazy;
                        }
                        pos = p2;
                        let idx2 = (p2 - win_base) as usize;
                        // Repcode probe at the stepped position; literals are
                        // pending by construction (the walk advanced past the
                        // anchor), so of_value 1 stays encodable. Skipped when
                        // the incumbent is itself a rep: rep-vs-rep only
                        // accepts a strictly longer ride, which the depth-0
                        // probe and rep1_chain already cover, and on
                        // rep-dense shapes the extra read4+extend per step
                        // visibly taxed scan speed.
                        if rep_pending == 0
                            && !rep_hit
                            && let Some(cand_abs) = p2.checked_sub(rep[0] as u64)
                            && cand_abs >= win_base
                        {
                            let ci = (cand_abs - win_base) as usize;
                            if read4(win, ci) == read4(win, idx2) {
                                let ml = extend_match(win, idx2, ci);
                                if ml >= MIN_MATCH {
                                    // Same gain-cap cheap reject as the
                                    // chain probe: lit_value tops out at 6
                                    // bits per byte (the clamp).
                                    if 6 * ml as i32 * rep_mul / 4
                                        > best_v * rep_mul / 4 - best_price + rep_m
                                    {
                                        let v = lit_value(win, idx2, ml, lit_lens);
                                        // rep_mul keeps libzstd's rep discount
                                        // (3/4 of the byte value at depth 1);
                                        // at the default lens v = ml*4 and this
                                        // is the flat ml*rep_mul comparison.
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
                        // Chain search at the stepped position.
                        let (len2, cand2) = search(win, chain, idx2);
                        let price2 = if len2 >= MIN_MATCH {
                            price_of((p2 - win_base) as usize, cand2)
                        } else {
                            0
                        };
                        if len2 >= MIN_MATCH {
                            // Cheap reject: lit_value tops out at 6 bits per
                            // byte (the clamp), so a candidate whose gain
                            // cap cannot beat the incumbent skips the four
                            // gathers entirely.
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

            // Backward extension into the pending literals; the offset
            // (start - cand) stays constant. A repcode emission must keep
            // one literal pending: of_value 1 with a zero literal length
            // resolves to a repcode *swap* on the decoder side, not rep0.
            let anchor_idx = (anchor - win_base) as usize;
            let floor = if rep_hit {
                anchor_idx + 1
            } else {
                anchor_idx
            };
            let mut cand = best_cand;
            let mut ml = best_len;
            while start > floor && cand > 0 && win[cand - 1] == win[start - 1] {
                cand -= 1;
                start -= 1;
                ml += 1;
            }
            let of_value = if rep_hit {
                1
            } else {
                (start - cand + 3) as u32
            };
            anchor = emit.emit_chain(win, chain, hash_log, anchor, start, ml, of_value, &mut rep);
            if rep_pending != 0 && of_value > 3 {
                rep_pending -= 1;
            }
            if seed_hit {
                seed_hits += 1;
                if seed_hits >= SEED_MATCHES {
                    seed_offset = 0;
                }
            }
            if rep_pending == 0 {
                pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
            } else {
                pos = anchor;
            }
            anchor = pos;
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

    /// Bridge into the optimal parser (levels Opt/Ultra): hands over the
    /// window, the epoch-tagged tables and the persistent price state, then
    /// stores back the cursors the parser advanced.
    fn start_matching_opt(
        &mut self,
        knobs: OptKnobs,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_start = self.block_start;
        let block_end = self.block_end;
        let max_window = self.params.window as u64;
        let mut epoch = self.epoch;
        let mut next_update = self.next_update;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let Some(scratch) = self.opt_scratch.as_mut() else {
            unreachable!("opt scratch allocated by apply_level")
        };
        // Disjoint field borrows: the window (win/ext) against the tables.
        if knobs.ultra {
            super::opt::run_block::<true>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                &mut epoch,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
            );
        } else {
            super::opt::run_block::<false>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                &mut epoch,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
            );
        }
        self.epoch = epoch;
        self.next_update = next_update;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.pos = block_end;
        self.anchor = block_end;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{MatchGeneratorDriver, pack_pos, unpack_pos};
    use crate::encoding::{Matcher, Sequence};

    fn block_label(i: usize) -> Vec<u8> {
        // "block N filler text; " without needing format! in no_std tests
        let mut v = Vec::new();
        v.extend_from_slice(b"block ");
        v.push(b'0' + i as u8);
        v.extend_from_slice(b" filler text; ");
        v
    }

    /// Feed `data` through the matcher one block at a time and reconstruct the
    /// original from the emitted sequences.
    fn match_and_reconstruct(data: &[u8], block_size: usize) -> Vec<u8> {
        let mut driver = MatchGeneratorDriver::new(block_size);
        driver.reset(crate::Level::Fastest);
        // Offset history mirrors the decoder's per-frame scratch.
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(block_size) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    // Matches may overlap their own output (offset < match_len).
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        reconstructed
    }

    #[test]
    fn reconstructs_short_runs() {
        let mut data = Vec::new();
        data.extend([0u8; 16]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([0u8; 8]);
        assert_eq!(match_and_reconstruct(&data, 8), data);
        assert_eq!(match_and_reconstruct(&data, 4), data);
    }

    #[test]
    fn reconstructs_across_blocks() {
        // Matches must reach into previous blocks through the shared window.
        let mut data = Vec::new();
        for i in 0..10 {
            data.extend_from_slice(&[0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0f]);
            data.extend_from_slice(&block_label(i));
        }
        assert_eq!(match_and_reconstruct(&data, 32), data);
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    fn reconstructs_random() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut data = Vec::with_capacity(300 * 1024);
        while data.len() < 300 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(&state.to_le_bytes());
        }
        assert_eq!(match_and_reconstruct(&data, 64 * 1024), data);
    }

    #[test]
    fn reconstructs_after_compaction() {
        // 3 MiB of data with repeats forces window compaction to run.
        let mut data = Vec::with_capacity(3 << 20);
        for i in 0..(3 << 20) / 64 {
            data.push((i % 251) as u8);
            data.extend(&[7u8; 63]);
        }
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    fn opt_levels_reconstruct() {
        let mut data = Vec::with_capacity(700 * 1024);
        let words = [
            &b"the quick brown fox "[..],
            &b"jumps over the lazy dog "[..],
            &b"lorem ipsum dolor sit amet "[..],
            b"\x00\x01\x02\x03 structured noise ",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        while data.len() < 700 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(words[(state as usize) % words.len()]);
        }
        for level in [crate::Level::Best, crate::Level::Opt, crate::Level::Ultra] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            driver.reset(level);
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = Vec::new();
            for block in data.chunks(128 * 1024) {
                driver.block_tail()[..block.len()].copy_from_slice(block);
                driver.commit_block(block.len());
                driver.start_matching(|seq| match seq {
                    Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                    Sequence::Triple {
                        literals,
                        offset,
                        match_len,
                    } => {
                        reconstructed.extend_from_slice(literals);
                        let actual = crate::decoding::sequence_execution::do_offset_history(
                            offset as u32,
                            literals.len() as u32,
                            &mut rep,
                        );
                        let start = reconstructed.len() - actual as usize;
                        for i in 0..match_len {
                            let b = reconstructed[start + i];
                            reconstructed.push(b);
                        }
                    },
                });
            }
            assert_eq!(reconstructed, data, "reconstruct {level:?}");
        }
    }

    #[test]
    fn pos_entry_roundtrip_across_cycles() {
        // A live entry always resolves back to its position, even across a
        // 4 GiB cycle boundary; a value numerically above the scan position
        // with no cycle to unwrap into is dead (stale cross-frame entry).
        for abs in [0u64, 1, 7, 0xffff_fffe, 5_000_000_000, 1 << 40] {
            for delta in [1u64, 2, 0x123, 0xffff_f000] {
                let pos = abs + delta;
                assert_eq!(unpack_pos(pack_pos(abs), pos), Some(abs), "{abs}+{delta}");
            }
        }
        // 2^32 - 1 collides with the empty sentinel: one dead position per
        // 4 GiB cycle, by design.
        assert_eq!(unpack_pos(pack_pos(0xffff_ffff), 1 << 40), None);
        assert_eq!(unpack_pos(pack_pos(1000), 10), None);
        assert_eq!(unpack_pos(0, 1 << 40), None);
    }

    /// The seed scan must return exactly the nearest qualifying position a
    /// naive byte-wise walk would find, on every path: the AVX-512 block
    /// walk (residue classes mod 8, block boundaries, the anchor's trivial
    /// self-match), the scalar tail below the last block, and strips too
    /// short for the block scheme.
    #[test]
    fn seed_scan_matches_naive_walk() {
        let qualify = |data: &[u8], u: usize, last: usize| -> bool {
            u >= 48 && data[u - 48..u] == data[last - 48..last]
        };
        let naive = |data: &[u8], last: usize| -> Option<usize> {
            let a8 = super::read8(data, last);
            (0..last)
                .rev()
                .find(|&u| super::read8(data, u) == a8 && qualify(data, u, last))
        };
        let check = |data: &[u8]| {
            let last = data.len() - 8;
            let expect = naive(data, last);
            assert_eq!(
                super::seed_scan(data, last, super::read8(data, last)),
                expect,
                "dispatched scan at len {}",
                data.len()
            );
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            if last >= super::SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
                assert_eq!(
                    // SAFETY: feature detected above; bounds argued at the
                    // definition.
                    unsafe { super::seed_scan_avx512(data, last, super::read8(data, last)) },
                    expect,
                    "avx512 scan at len {}",
                    data.len()
                );
            }
        };

        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // A 56-byte pattern planted as the anchor window `[last-48,
        // last+8)` and, disjoint from it, as the candidate window
        // `[hit-48, hit+8)`; everything else random. `hit` sweeps the
        // interesting offsets: residue classes mod 8, the block-grid
        // boundaries and the bottom tail (< 64).
        let len = 700;
        let last = len - 8;
        let mut data = alloc::vec![0u8; len];
        for hit in [
            last - 56,
            last - 57,
            last - 63,
            last - 64,
            last - 65,
            last - 72,
            last - 119,
            last - 120,
            last - 128,
            65,
            64,
            63,
            48,
        ] {
            for b in &mut data {
                *b = rng() as u8;
            }
            let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
            data[hit - 48..hit + 8].copy_from_slice(&pat);
            data[last - 48..].copy_from_slice(&pat);
            check(&data);
            assert_eq!(
                super::seed_scan(&data, last, super::read8(&data, last)),
                Some(hit),
                "planted hit {hit}"
            );
        }
        // A candidate below the agree bound never qualifies: the planted
        // anchor window alone must yield no seed.
        for b in &mut data {
            *b = rng() as u8;
        }
        let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
        data[last - 48..].copy_from_slice(&pat);
        check(&data);
        assert_eq!(
            super::seed_scan(&data, last, super::read8(&data, last)),
            None
        );
        // All-same bytes: every position trivially matches, nearest wins and
        // the anchor itself is excluded.
        let flat = alloc::vec![0x5Au8; len];
        check(&flat);
        assert_eq!(
            super::seed_scan(&flat, last, super::read8(&flat, last)),
            Some(last - 1)
        );
        // Exact periods across all residue classes mod 8: the scan must find
        // the period (or a multiple) exactly like the naive walk.
        for period in [200, 201, 202, 203, 204, 205, 206, 207, 256, 257] {
            let unit: Vec<u8> = (0..period).map(|_| rng() as u8).collect();
            let mut tiled = alloc::vec![0u8; 0];
            while tiled.len() < len {
                tiled.extend_from_slice(&unit);
            }
            tiled.truncate(len);
            check(&tiled);
        }
        // No repeat at all, at sizes just around the block-scheme minimum.
        for l in [48, 56, 63, 64, 120, 127, 128, 129, 136, len] {
            let mut data = alloc::vec![0u8; l];
            for b in &mut data {
                *b = rng() as u8;
            }
            // A u64-rng strip of this size has no 8-byte recurrence.
            check(&data);
        }
    }

    /// Stale u32 entries from an earlier frame must die on the domain
    /// check: their values share no position domain with the new frame's
    /// scan, and the wrap reconstruction must reject the ones numerically
    /// above it instead of underflowing into an out-of-bounds window index.
    /// Regression for a pooled-driver double-frame compression.
    #[test]
    fn stale_entries_survive_frame_reset() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // Frame 1: > 1 MiB so the tables end holding positions far above
        // frame 2's scan. Frame 2: small and structured, so its probes hit
        // stale slots from the first position on.
        let mut long = Vec::with_capacity(3 << 20);
        while long.len() < 3 << 20 {
            long.extend_from_slice(&rng().to_le_bytes());
            long.extend_from_slice(b"stable filler segment; ");
        }
        let mut small = Vec::new();
        for i in 0..64 {
            small.extend_from_slice(&block_label(i % 7));
            small.push(rng() as u8);
        }
        for level in [
            crate::Level::Fastest,
            crate::Level::Fast,
            crate::Level::Balanced,
        ] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            for data in [&long, &small] {
                driver.reset(level);
                let mut rep = [1u32, 4, 8];
                let mut reconstructed = Vec::new();
                for block in data.chunks(128 * 1024) {
                    driver.block_tail()[..block.len()].copy_from_slice(block);
                    driver.commit_block(block.len());
                    driver.start_matching(|seq| match seq {
                        Sequence::Literals { literals } => {
                            reconstructed.extend_from_slice(literals);
                        },
                        Sequence::Triple {
                            literals,
                            offset,
                            match_len,
                        } => {
                            reconstructed.extend_from_slice(literals);
                            let actual = crate::decoding::sequence_execution::do_offset_history(
                                offset as u32,
                                literals.len() as u32,
                                &mut rep,
                            );
                            let start = reconstructed.len() - actual as usize;
                            for i in 0..match_len {
                                let b = reconstructed[start + i];
                                reconstructed.push(b);
                            }
                        },
                    });
                }
                assert_eq!(reconstructed, *data, "reconstruct {level:?}");
            }
        }
    }

    #[test]
    fn skip_matching_indexes_for_later_blocks() {
        let mut driver = MatchGeneratorDriver::new(16);
        driver.reset(crate::Level::Fastest);
        let pattern = [3u8, 1, 4, 1, 5, 9, 2, 6];
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        driver.skip_matching();
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        let mut got_triple = false;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                // New-offset wire value: actual offset 8 encodes as 8 + 3.
                assert_eq!(offset, pattern.len() + 3);
                got_triple = true;
            }
        });
        assert!(
            got_triple,
            "second block must match the skipped first block"
        );
    }

    #[test]
    fn emits_repcode_sequences() {
        // Structured repetition at a fixed distance: the first repeat is found
        // by the hash probe (offset becomes rep[0]), later repeats must be
        // emitted as repcode 1 (wire offset value 1).
        let pattern: &[u8] = &[
            0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x91, 0x82, 0x73, 0x64,
        ];
        let mut data = Vec::new();
        for i in 0..40 {
            data.extend_from_slice(pattern);
            // Constant-length, varying separators keep one pending literal in
            // front of each repeat and hold the period stable.
            data.push(0xf0 ^ i as u8);
        }
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(crate::Level::Fastest);
        driver.block_tail()[..data.len()].copy_from_slice(&data);
        driver.commit_block(data.len());
        let mut repcodes = 0usize;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq
                && offset <= 3
            {
                repcodes += 1;
            }
        });
        assert!(
            repcodes > 0,
            "repeated structure must produce repcode matches"
        );
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }
}
