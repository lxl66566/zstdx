//! Offset/literal price model backing the lazy gain comparisons of the
//! chain family.

use super::MIN_MATCH;

/// Offset price for the chain strategy's lazy gain comparisons (libzstd's
/// `ZSTD_highbit32(offBase)` with offBase = offset+1): the offset-code
/// exponent in ~bits. Repcode incumbents price 0.
#[inline(always)]
pub(super) fn price_of(idx: usize, cand: usize) -> i32 {
    ((idx - cand + 1) as u32).ilog2() as i32
}

/// Assumed literal code lengths before the block encoder's first
/// measurement arrives: the historical flat constant of the static gate
/// (~4 bits per literal).
pub(super) const DEFAULT_LIT_LENS: [u8; 256] = [4; 256];

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
pub(super) fn pays_for_offset(ml: usize, idx: usize, cand: usize, rep_hit: bool) -> bool {
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
pub(super) fn pays_for_offset_lit(
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
pub(super) fn lit_value(win: &[u8], idx: usize, len: usize, lit_lens: &[u8; 256]) -> i32 {
    debug_assert!(len >= MIN_MATCH);
    let s = (lit_lens[win[idx] as usize] as i32).min(6)
        + (lit_lens[win[idx + 1] as usize] as i32).min(6)
        + (lit_lens[win[idx + 2] as usize] as i32).min(6)
        + (lit_lens[win[idx + 3] as usize] as i32).min(6);
    s * len as i32 / 4
}
