//! Parse-shaping gates and heuristics: the MT job-start deep-offset ramp,
//! the incompressibility probe gate, the fast strategy's interior fill
//! density policy, and the strip-seed scan.

use super::hash::read8;

/// Backward bytes that must agree (beyond the 8-byte anchor) before a strip
/// position becomes the job-start seed offset: long enough that word-level
/// repeats (~10-15 agreeing bytes on natural text) cannot qualify, short
/// enough that one cache line of checking settles it.
pub(super) const SEED_AGREE: usize = 48;
/// Spread anchors behind the 56-byte agreement window where a seed candidate
/// must still agree (one u64 compare each). The window alone cannot tell a
/// strip-long period from a long local repeat: a duplicated block of ≥56
/// bytes near the strip tail agrees with itself at every offset inside the
/// block, and the nearest-first scan prefers it (measured: a 194-byte
/// duplicated source statement at offset 2244 beat a 441226-byte tile period
/// and burned the seed budget, collapsing that job's ratio). A genuine
/// period or duplicated block agrees at every distance it spans, so spread
/// confirms separate the classes; anchors that underflow the candidate's own
/// history are skipped (short strips).
pub(super) const SEED_CONFIRMS: [usize; 3] = [64, 640, 6144];
/// Seed matches to emit before retiring the seed: three literal offsets
/// both clear the repcode gate and rotate `rep` until `rep[0]` holds the
/// seed offset, so the regular repcode probes take over from there.
pub(super) const SEED_MATCHES: u8 = 3;

/// Probe attempts an unused seed survives: a seed whose offset stops
/// matching (broken period, or a repeated block that ended) must not pay a
/// dead compare for the rest of the job.
pub(super) const SEED_BUDGET: u32 = 8192;
/// Strip length below which the seed scan stays scalar: the AVX-512 block
/// walk's lowest block reads up to byte 63 + 71, and `last + 8 >= 136`
/// keeps every such load inside data. Shorter strips are cheap anyway.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
pub(super) const SEED_SCAN_MIN: usize = 128;

/// Deep-offset ramp at multithreaded job starts (decode-parallelism
/// experiment; see `encoding::mt`): while armed, a match whose source lies
/// below `start` (a cross-boundary read into the previous job's output)
/// must reach at least `depth` bytes below `start`. Sources at or after
/// `start` stay legal at any offset — they are in-job reads the executing
/// piece produces itself. This DEPTH semantics (not an offset floor) is
/// what a parallel stage B needs: a piece's completion interval is bounded
/// by its shallowest crossing read, wherever that read sits in the piece.
/// All-zero disables the gate; the hot-path check folds to two
/// comparisons.
#[derive(Clone, Copy)]
pub(crate) struct RampGate {
    /// Absolute job start; sources below it are cross-boundary reads.
    pub(super) start: u64,
    /// Armed marker (the ramp region end `start + depth`; 0 = off).
    pub(super) end: u64,
    /// Minimum cross-boundary depth.
    pub(super) depth: u64,
}

impl RampGate {
    pub(crate) const OFF: Self = Self {
        start: 0,
        end: 0,
        depth: 0,
    };

    /// Whether the gate is armed; scan-loop instantiation choice keys off
    /// this (disarmed loops compile the checks out entirely).
    #[inline(always)]
    pub(super) fn is_armed(&self) -> bool {
        self.end != 0
    }

    /// Whether the match `(pos_abs, cand_abs)` violates the ramp: a source
    /// inside the `depth` bytes just below the job start. `pos_abs` is
    /// unused — the constraint is position-independent (the depth of a
    /// crossing read is `start - cand`, not its offset).
    #[inline(always)]
    pub(super) fn blocks(&self, pos_abs: u64, cand_abs: u64) -> bool {
        let _ = pos_abs;
        self.end != 0 && cand_abs < self.start && cand_abs + self.depth > self.start
    }

    /// Backward-extension floor for a match's source, in window indices.
    /// Extension moves source and match back in lockstep, so the source
    /// walks contiguously: one at or after the job start must stop AT the
    /// boundary — letting it pass would drag it through the whole shallow
    /// band `start - depth .. start`, and an extension that dies inside
    /// the band would emit an illegal shallow crossing read. A source
    /// already below the band (it passed `blocks`) only gets deeper under
    /// extension, so it extends down to the window floor.
    #[inline(always)]
    pub(super) fn ext_floor(&self, cand_idx: usize, win_base: u64) -> usize {
        if self.end != 0 && win_base + cand_idx as u64 >= self.start {
            self.start.saturating_sub(win_base) as usize
        } else {
            0
        }
    }
}

/// Const-generic [`RampGate`] checks: the fast scan loops instantiate with
/// `RAMPED = false` on disarmed frames (all bulk-ST paths; MT jobs without
/// the env ramp), so the gate's three live u64s and its per-site branches
/// fold away entirely instead of riding the loop as never-taken state.
#[inline(always)]
pub(super) fn ramp_blocks<const RAMPED: bool>(ramp: RampGate, pos_abs: u64, cand_abs: u64) -> bool {
    RAMPED && ramp.blocks(pos_abs, cand_abs)
}

#[inline(always)]
pub(super) fn ramp_ext_floor<const RAMPED: bool>(
    ramp: RampGate,
    cand_idx: usize,
    win_base: u64,
) -> usize {
    if RAMPED {
        ramp.ext_floor(cand_idx, win_base)
    } else {
        0
    }
}

/// Log of the incompressibility gate's probe table (see
/// [`Matcher::skip_if_incompressible`]). 32K tag slots keep the overwrite
/// rate of a block's ~2000 samples negligible.
pub(super) const GATE_PROBE_LOG: u32 = 15;
/// Blocks below this pay little enough in the matcher that the gate's
/// sample pass is not worth its own cost.
pub(super) const GATE_MIN_BLOCK: usize = 16384;

/// Short-match interior fill policy of the fast strategy, selected per
/// block from the previous block's parse density (the field
/// `MatchGeneratorDriver::covered_fill`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CoveredFill {
    /// Every covered position. Structured shapes need it: their
    /// phase-shifted repeats have no second earlier copy for a shifted
    /// probe to hit instead (dual anchors alone: json +1.6%, text +3.0%).
    Dense,
    /// Every second covered position. On dll-class sparse parses the
    /// duplicated mass has many copies, and the pair-probed scan covers
    /// both strides' phases (any two adjacent probes span the parities),
    /// so the halved insert volume keeps the coverage at ~0.04% size.
    Strided,
    /// Every covered position at every length — the dictionary-row chain
    /// emits. libzstd's lazy-family fill is dense through a stored
    /// sequence's interior (its row update skips only past 384-byte jumps,
    /// 96 head + 32 tail); the tuned stride grids exist for no-dict speed,
    /// and a small dictionary frame's parse rides near-duplicate twins
    /// sitting exactly on the skipped phases.
    DictDense,
}

/// Sequence floor of the covered-fill density gate: below it a block's
/// parse carries no usable density signal (literal-run tails).
pub(super) const COVERED_GATE_MIN_SEQS: usize = 64;
/// Literal bytes per sequence above which a block counts as dll-class
/// sparse for the covered-fill density gate (structured shapes sit at
/// 2.6-3.0).
pub(super) const COVERED_GATE_AVG_LL: usize = 4;

/// Scan instantiation of the fast strategy for the upcoming block,
/// decided from the previous block's parse (the [`CoveredFill`]
/// precedent). The dense body hosts levers whose unconditional hosting is
/// falsified: even a never-taken acceptance-bar branch costs text.fastest
/// 12% wall (see the negative notes), while match-dense narrow-alphabet
/// parses (json: 97% of blocks) win on them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ScanDensity {
    /// The stock scan body, byte-for-byte.
    Plain,
    /// Hosts the fed-back-literal-price acceptance bar and miss-run
    /// stepping (see [`DENSE_BAR_MIN_DIST`]/[`DENSE_STEP_AFTER`]).
    Dense,
}

/// Sequence floor of the dense-scan gate: below it a block's parse carries
/// no usable density signal (skewed's literal-run tails sit at ≤ 10
/// sequences per block).
pub(super) const DENSE_GATE_MIN_SEQS: usize = 512;
/// Literal bytes per sequence ceiling of the dense-scan gate: dll-class
/// sparse parses (median 8.6) stay plain — their far matches are payload
/// the bar would decline.
pub(super) const DENSE_GATE_MAX_AVG_LL: usize = 4;
/// Covered-symbol ceiling of the dense-scan gate: the width of the literal
/// alphabet under the previous block's fed-back Huffman lengths. This is
/// the signal that separates the classes — text's four match-dense blocks
/// carry 99.5% of its sequences and are locally json-shaped in every parse
/// statistic (nseq, avg_ll), but price their literals on a ~80-symbol
/// alphabet at 5.4+ bits/B where json's residual stream covers 11-25
/// symbols at 3.5, dll's sparsest passing block covers 43. The bar sits
/// mid-gap (json max 25, dll min 43, text 78+).
pub(super) const DENSE_GATE_SYMS_MAX: usize = 32;
/// Offset floor of the dense scan's acceptance bar: nearer matches clear
/// the flat-4 price already, so only far candidates pay the price gather.
pub(super) const DENSE_BAR_MIN_DIST: usize = 1024;
/// Consecutive missed bytes after which the dense scan doubles the pair
/// advance: json's mid-run weak matches are net-negative on both axes
/// (skipping them shrinks size AND raises speed), text's are payload —
/// hence dense-mode only.
pub(super) const DENSE_STEP_AFTER: usize = 3;

/// Count the symbols the fed-back literal table covers (code lengths below
/// the 11-bit cap [`MatchGeneratorDriver::note_literal_costs`] maps
/// uncovered symbols to). 256 ops per block, never per byte.
pub(super) fn covered_lit_symbols(lit_lens: &[u8; 256]) -> usize {
    lit_lens.iter().filter(|&&l| l < 11).count()
}

/// Wide-alphabet screen of the small-input dense policy (the first-block
/// analog of [`DENSE_GATE_SYMS_MAX`]'s covered-symbol signal, which needs
/// a previous block's fed-back prices and so can never fire on a
/// single-block frame). Cost contract: O(<= 640 strided samples), never
/// O(n) — the screen runs once per window on paths where one extra
/// window-sized pass costs a fifth to half of the whole call (a
/// full-window distinct scan measured skewed-16 KiB fastest at -47%;
/// an unconditional dense tier below 2 KiB measured skewed-1K at -15%
/// with zero ratio change).
///
/// Two strided passes, calibrated on the corpus classes (skewed 16
/// symbols, json 38-39, text 66-99) at every size 1-128 KiB: a
/// 128-sample narrow pass exits tiny-alphabet inputs (skewed reads 16
/// of 16, json never below 27; exit bar [`SMALL_DENSE_NARROW_MAX`]),
/// then a 512-sample pass decides — the wide classes read 46-62, the
/// structured ones 32-39, and the bar [`SMALL_DENSE_WIDE_MIN`] splits
/// that gap (margins >= 3 on both sides down to 1 KiB windows, where
/// the 512-sample stride is 2).
pub(super) const SMALL_DENSE_NARROW_MAX: u32 = 20;
pub(super) const SMALL_DENSE_WIDE_MIN: u32 = 42;

/// Tiny frames join the dense bars whatever their alphabet shape (minus
/// the narrow exit): the cold start dominates the frame, so every-position
/// probing wins — json 1-3 KiB fast rows gain 7-17 B, dfast 1 KiB 7 B,
/// 4 KiB byte-neutral — while the same bars on structured alphabets lose
/// from 8 KiB up (json 16 KiB +170 B, 64 KiB +1051 B), which is where the
/// alphabet screen keeps excluding them. 4096 sits at the measured
/// crossover. Fast/dfast rows only (see
/// [`MatchGeneratorDriver::win_small_wide_fast`]): the btlazy head's hash
/// width dilutes on structured content under the join (json 2 KiB l9
/// +7 B) and keeps the strict verdict.
pub(super) const SMALL_DENSE_TINY_MAX: usize = 4096;

pub(super) fn small_dense_wide(win: &[u8]) -> bool {
    if sampled_distinct(win, 128) <= SMALL_DENSE_NARROW_MAX {
        return false;
    }
    sampled_distinct(win, 512) >= SMALL_DENSE_WIDE_MIN
}

/// Distinct byte values among `samples` strided reads of `win`. The
/// four bitmap words live in registers (a `[u64; 4]` indexed by
/// `b >> 6` compiles to a store-forwarding chain through memory, ~2.8
/// cycles per sample — half again the cost this screen may spend on a
/// 1 KiB fastest-tier call).
fn sampled_distinct(win: &[u8], samples: usize) -> u32 {
    let stride = (win.len() / samples).max(1);
    let (mut w0, mut w1, mut w2, mut w3) = (0u64, 0u64, 0u64, 0u64);
    let mut i = 0usize;
    while i < win.len() {
        let b = win[i];
        let bit = 1u64 << (b & 63);
        match b >> 6 {
            0 => w0 |= bit,
            1 => w1 |= bit,
            2 => w2 |= bit,
            _ => w3 |= bit,
        }
        i += stride;
    }
    w0.count_ones() + w1.count_ones() + w2.count_ones() + w3.count_ones()
}

/// Nearest `u < last` with the anchor's 8 bytes and [`SEED_AGREE`]
/// agreeing bytes before it (see [`MatchGeneratorDriver::acquire_seed`]).
/// The 8-byte compare subsumes the 4-byte prefilter the scalar walk used
/// to run first: a matching u64's low half is the u32 at the same index.
pub(super) fn seed_scan(data: &[u8], last: usize, a8: u64) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    if last >= SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected; every load stays inside
        // data (see the bound derivation in the callee).
        return unsafe { seed_scan_avx512(data, last, a8) };
    }
    let mut u = last;
    while u > 0 {
        u -= 1;
        if read8(data, u) == a8 && seed_agrees(data, u, last) && seed_confirms(data, u, last) {
            return Some(u);
        }
    }
    None
}

/// Whether the [`SEED_AGREE`] bytes before `u` equal those before `last`:
/// positions below [`SEED_AGREE`] never qualify (the `u >= k` bound).
pub(super) fn seed_agrees(data: &[u8], u: usize, last: usize) -> bool {
    let mut k = 1;
    while u >= k && k <= SEED_AGREE && data[u - k] == data[last - k] {
        k += 1;
    }
    k > SEED_AGREE
}

/// Whether the [`SEED_CONFIRMS`] anchors behind `u` still agree with those
/// behind `last` (see the const's doc): the local-repeat filter that keeps
/// the nearest-first seed honest. `u < last`, so `u >= t` bounds both reads.
#[inline(always)]
pub(super) fn seed_confirms(data: &[u8], u: usize, last: usize) -> bool {
    for t in SEED_CONFIRMS {
        if u >= t && read8(data, u - t) != read8(data, last - t) {
            return false;
        }
    }
    true
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
pub(super) unsafe fn seed_scan_avx512(data: &[u8], last: usize, a8: u64) -> Option<usize> {
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
                if u < last && seed_agrees(data, u, last) && seed_confirms(data, u, last) {
                    return Some(u);
                }
            }
            if b == bottom {
                break;
            }
            b -= 64;
        }
        (0..bottom).rev().find(|&u| {
            read8(data, u) == a8 && seed_agrees(data, u, last) && seed_confirms(data, u, last)
        })
    }
}
