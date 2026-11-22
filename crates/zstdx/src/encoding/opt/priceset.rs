//! The ultra tier's variant-scan price set: for one DP position, write the
//! price of every enumerated mlen of every candidate match into the node
//! arrays, keeping the improving entries.
//!
//! The ultra scan is order-independent, which is what makes the vector form
//! bit-exact. Each mlen lane reads only its own cell (`prices[cur + mlen]`)
//! and writes only its own entry, and the scalar form's descending lane
//! order collapses to one per-lane rule: a lane at `p` compares against the
//! cell's pre-scan value when `p` stays at or below `last_pos`, and against
//! the MAX_PRICE the gap fill wrote when `p` lands beyond it (the first,
//! longest lane's `pos2 > last_pos` arm fills `(last_pos, p_max]` before
//! any other lane runs). The kernel reproduces that rule by gap-filling
//! first, then comparing every lane against the post-fill cells and writing
//! the improving ones — ascending instead of descending, same final state.
//! The longest lane of an extending candidate is the one exception: the
//! scalar form writes it through the `pos2 > last_pos` arm unconditionally,
//! and a wrapped price (`c_base + mirror` sums mod 2^32) can lose the
//! MAX_PRICE comparison, so its write stands outside the comparison.
//!
//! Lane prices are affine in the length price — `c_base + ml_price_by_len`,
//! every add wrapping u32 with no rounding, the per-length mirror being the
//! exact scalar length term — so every lane price equals the scalar form's
//! `base_price + off_term + len_term` bit for bit. The comparison is the
//! same unsigned `<` (the AVX-2 form biases both operands with the sign bit
//! and takes the signed greater-than), so every write decision matches too.
//!
//! Non-ultra rows never reach this module: their early-abort scan (`break`
//! on the first non-improving lane) is order-dependent.

use super::{MAX_PRICE, Match, OptState, Optimal};

/// One candidate's span and its base-constant price. Match lengths are
/// strictly increasing, so `m - 1` stays in range.
#[inline(always)]
fn candidate_span(
    matches: &[Match],
    m: usize,
    min_match: usize,
    state: &OptState,
    base_price: u32,
) -> (u32, usize, usize, u32) {
    let cand = &matches[m];
    let last_ml = cand.len as usize;
    let start_ml = if m > 0 {
        matches[m - 1].len as usize + 1
    } else {
        min_match
    };
    let c_base = base_price.wrapping_add(state.match_price_off_term::<true>(cand.off));
    (cand.off, start_ml, last_ml, c_base)
}

/// The gap fill the scalar form's first lane runs: cells `(from, to)` keep
/// MAX_PRICE and litlen 1 (the `to` cell's own entry write covers it).
///
/// SAFETY: `to` stays inside the scratch.
#[inline(always)]
unsafe fn fill_gap(prices: &mut [u32], entries: &mut [Optimal], from: usize, to: usize) {
    let mut p = from + 1;
    while p < to {
        // SAFETY: p < to.
        unsafe {
            *prices.get_unchecked_mut(p) = MAX_PRICE;
            entries.get_unchecked_mut(p).litlen = 1;
        }
        p += 1;
    }
}

/// One improving lane's entry write.
///
/// SAFETY: `pos2` stays inside the scratch.
#[inline(always)]
unsafe fn write_lane(
    prices: &mut [u32],
    entries: &mut [Optimal],
    pos2: usize,
    off: u32,
    mlen: u32,
    price: u32,
) {
    // SAFETY: pos2 below OPT_SIZE (the caller's scan bound).
    unsafe {
        let e = entries.get_unchecked_mut(pos2);
        e.off = off;
        e.mlen = mlen;
        e.litlen = 0;
        *prices.get_unchecked_mut(pos2) = price;
    }
}

/// Lanes `[mlen_from, mlen_to)`: compare against the post-fill cells,
/// write the improving ones. Also the AVX-2 form's remainder step.
///
/// SAFETY: `cur + mlen_to` stays inside the scratch and `mlen_to` below
/// the per-length mirror.
#[inline(always)]
unsafe fn lanes_scalar(
    prices: &mut [u32],
    entries: &mut [Optimal],
    state: &OptState,
    cur: usize,
    c_base: u32,
    off: u32,
    mlen_from: usize,
    mlen_to: usize,
) {
    let mirror = &state.ml_len_price;
    let mut mlen = mlen_from;
    while mlen < mlen_to {
        let pos2 = cur + mlen;
        // SAFETY: mlen < mlen_to below the mirror, pos2 below OPT_SIZE.
        unsafe {
            let price = c_base.wrapping_add(*mirror.get_unchecked(mlen));
            if price < *prices.get_unchecked(pos2) {
                write_lane(prices, entries, pos2, off, mlen as u32, price);
            }
        }
        mlen += 1;
    }
}

/// The longest lane of one candidate: an extending candidate writes it
/// unconditionally (the module doc's exception); otherwise it compares
/// like the rest.
///
/// SAFETY: `p_max` stays inside the scratch, `last_ml` below the mirror.
#[inline(always)]
unsafe fn lane_longest(
    prices: &mut [u32],
    entries: &mut [Optimal],
    state: &OptState,
    p_max: usize,
    off: u32,
    last_ml: usize,
    c_base: u32,
    extended: bool,
) {
    // SAFETY: last_ml below the mirror, p_max below OPT_SIZE.
    unsafe {
        let price = c_base.wrapping_add(*state.ml_len_price.get_unchecked(last_ml));
        if extended || price < *prices.get_unchecked(p_max) {
            write_lane(prices, entries, p_max, off, last_ml as u32, price);
        }
    }
}

/// The scalar form of the ultra variant scan: the no-AVX-2 fallback, and
/// the reference the AVX-2 form is tested against.
///
/// SAFETY: the caller's DP scan bounds hold — `cur + last_ml < OPT_NUM`
/// for every candidate (the jump branch took anything at or beyond
/// OPT_NUM), `last_ml` stays below the per-length mirror (the jump branch
/// took anything above `sufficient_len`), and `nb` is the caller's match
/// count so every `matches` index is in range.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn price_set_scalar(
    prices: &mut [u32],
    entries: &mut [Optimal],
    state: &OptState,
    matches: &[Match],
    nb: usize,
    min_match: usize,
    cur: usize,
    base_price: u32,
    last_pos: &mut usize,
) {
    for m in 0..nb {
        let (off, start_ml, last_ml, c_base) =
            candidate_span(matches, m, min_match, state, base_price);
        let p_max = cur + last_ml;
        let extended = p_max > *last_pos;
        if extended {
            // SAFETY: p_max < OPT_NUM.
            unsafe { fill_gap(prices, entries, *last_pos, p_max) };
            *last_pos = p_max;
        }
        // SAFETY: the lane bound is last_ml below the mirror.
        unsafe { lanes_scalar(prices, entries, state, cur, c_base, off, start_ml, last_ml) };
        // SAFETY: p_max below OPT_SIZE, last_ml below the mirror.
        unsafe {
            lane_longest(
                prices, entries, state, p_max, off, last_ml, c_base, extended,
            );
        }
    }
}

/// The AVX-2 form: 8-lane `c_base + mirror` sweeps over the lane run, the
/// unsigned-improving mask scattered per set bit.
///
/// SAFETY: the bounds of [`price_set_scalar`] hold; AVX-2 was detected by
/// the once-per-block selector.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn price_set_avx2(
    prices: &mut [u32],
    entries: &mut [Optimal],
    state: &OptState,
    matches: &[Match],
    nb: usize,
    min_match: usize,
    cur: usize,
    base_price: u32,
    last_pos: &mut usize,
) {
    use core::arch::x86_64::{
        _mm256_add_epi32, _mm256_castsi256_ps, _mm256_cmpgt_epi32, _mm256_loadu_si256,
        _mm256_movemask_ps, _mm256_set1_epi32, _mm256_xor_si256,
    };
    let base = prices.as_ptr();
    let mirror = state.ml_len_price.as_ptr();
    let sign = _mm256_set1_epi32(i32::MIN);
    for m in 0..nb {
        let (off, start_ml, last_ml, c_base) =
            candidate_span(matches, m, min_match, state, base_price);
        let c_vec = _mm256_set1_epi32(c_base as i32);
        let p_max = cur + last_ml;
        let extended = p_max > *last_pos;
        if extended {
            // SAFETY: p_max < OPT_NUM.
            unsafe { fill_gap(prices, entries, *last_pos, p_max) };
            *last_pos = p_max;
        }
        // SAFETY: the 8-lane loads stay inside the mirror (the top lane
        // index is below last_ml) and the price loads inside the scratch
        // (the top position is below p_max).
        unsafe {
            let mut mlen = start_ml;
            while mlen + 8 <= last_ml {
                let p0 = cur + mlen;
                let new = _mm256_add_epi32(_mm256_loadu_si256(mirror.add(mlen).cast()), c_vec);
                let old = _mm256_loadu_si256(base.add(p0).cast());
                // Unsigned `<`: bias both operands, take signed `>`.
                let improving =
                    _mm256_cmpgt_epi32(_mm256_xor_si256(old, sign), _mm256_xor_si256(new, sign));
                let mut mask = _mm256_movemask_ps(_mm256_castsi256_ps(improving)) as u8;
                while mask != 0 {
                    let k = mask.trailing_zeros() as usize;
                    let len = mlen + k;
                    let price = c_base.wrapping_add(*mirror.add(len));
                    write_lane(prices, entries, p0 + k, off, len as u32, price);
                    mask &= mask - 1;
                }
                mlen += 8;
            }
            lanes_scalar(prices, entries, state, cur, c_base, off, mlen, last_ml);
        }
        // SAFETY: p_max below OPT_SIZE, last_ml below the mirror.
        unsafe {
            lane_longest(
                prices, entries, state, p_max, off, last_ml, c_base, extended,
            );
        }
    }
}

/// Whether the AVX-2 kernel may run: resolved once per block by the DP
/// (never per node); `std` caches the feature probe.
#[inline]
pub(super) fn avx2_ready() -> bool {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        crate::common::simd::allows(crate::common::simd::SimdTier::Avx2)
            && std::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "std")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{
        super::{MAX_PRICE, Match, OPT_SIZE, OptState, Optimal},
        *,
    };

    /// The pre-kernel scan this module replaces: the original run_once
    /// loop's descending lane order, per candidate ascending. The kernels
    /// must land the identical final state on every scenario.
    ///
    /// SAFETY: the bounds of [`price_set_scalar`] hold.
    #[allow(clippy::too_many_arguments)]
    unsafe fn scan_descending(
        prices: &mut [u32],
        entries: &mut [Optimal],
        state: &OptState,
        matches: &[Match],
        nb: usize,
        min_match: usize,
        cur: usize,
        base_price: u32,
        last_pos: &mut usize,
    ) {
        for m in 0..nb {
            // SAFETY: the indices stay inside the scratch (the caller's
            // bound) and `nb` is the match count.
            unsafe {
                let off = matches[m].off;
                let last_ml = matches[m].len as usize;
                let start_ml = if m > 0 {
                    matches[m - 1].len as usize + 1
                } else {
                    min_match
                };
                let off_term = state.match_price_off_term::<true>(off);
                let mut mlen = last_ml;
                while mlen >= start_ml {
                    let pos2 = cur + mlen;
                    let price = base_price
                        .wrapping_add(off_term)
                        .wrapping_add(state.match_price_len_term::<true>(mlen as u32));
                    if pos2 > *last_pos || price < *prices.get_unchecked(pos2) {
                        while *last_pos < pos2 {
                            *last_pos += 1;
                            *prices.get_unchecked_mut(*last_pos) = MAX_PRICE;
                            entries.get_unchecked_mut(*last_pos).litlen = 1;
                        }
                        let e = entries.get_unchecked_mut(pos2);
                        e.off = off;
                        e.mlen = mlen as u32;
                        e.litlen = 0;
                        *prices.get_unchecked_mut(pos2) = price;
                    }
                    mlen -= 1;
                }
            }
        }
    }

    struct Twin {
        prices: Vec<u32>,
        entries: Vec<Optimal>,
        last_pos: usize,
    }

    impl Twin {
        fn fields(&self, p: usize) -> (u32, u32, u32) {
            let e = &self.entries[p];
            (e.off, e.mlen, e.litlen)
        }
    }

    /// Both kernels equal the descending scan over randomized scenarios:
    /// stale prices spanning the whole u32 range (wrapped lane prices that
    /// lose the MAX_PRICE comparison included), candidate lists of mixed
    /// band widths, and carried `last_pos` across positions so gap fills
    /// from earlier candidates and earlier positions interleave.
    #[test]
    fn kernels_match_the_descending_scan() {
        let src: &[u8] = b"the quick brown fox jumps over the lazy dog, and then some more bytes to grow the histogram";
        let mut rand = 0x243f_6a88_85a3_08d3u64;
        let next = |rand: &mut u64| {
            *rand = rand
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (*rand >> 33) as u32
        };
        let vector = avx2_ready();
        for scenario in 0..32u32 {
            let mut state = OptState::new();
            state.rescale_freqs::<true>(src);
            // Arbitrary mirror and offset prices, so lanes compare against
            // the whole u32 range instead of well-formed bitcosts.
            for v in &mut state.ml_len_price {
                *v = next(&mut rand);
            }
            for v in &mut state.off_price {
                *v = next(&mut rand);
            }
            state.off_code_base_price = next(&mut rand);
            state.match_length_base_price = next(&mut rand);
            let stale_prices: Vec<u32> = (0..OPT_SIZE)
                .map(|_| {
                    // A stale cell mix: markers, small prices, and values
                    // above MAX_PRICE that wrapped.
                    match next(&mut rand) % 8 {
                        0 => MAX_PRICE,
                        1 => MAX_PRICE + 1,
                        2 => 0,
                        _ => next(&mut rand),
                    }
                })
                .collect();
            let stale_entries: Vec<Optimal> = (0..OPT_SIZE)
                .map(|_| Optimal {
                    off: next(&mut rand),
                    mlen: next(&mut rand),
                    litlen: next(&mut rand),
                    rep: [next(&mut rand), next(&mut rand), next(&mut rand)],
                })
                .collect();
            let stale_last = (next(&mut rand) % 64) as usize;
            let make_twin = || Twin {
                prices: stale_prices.clone(),
                entries: stale_entries.clone(),
                last_pos: stale_last,
            };
            let mut twins = [make_twin(), make_twin(), make_twin()];
            for step in 0..8u32 {
                let cur = (next(&mut rand) % 1800 + (scenario + step) % 400) as usize;
                let mut matches = Vec::new();
                let mut len = 3usize + next(&mut rand) as usize % 3;
                while len < 300 && matches.len() < 8 {
                    matches.push(Match {
                        off: next(&mut rand).wrapping_add(1),
                        len: len as u32,
                    });
                    len += 1 + next(&mut rand) as usize % 40;
                }
                let nb = matches.len();
                let base_price = next(&mut rand);
                let min_match = 3;
                {
                    let [reference, scalar, avx2] = &mut twins;
                    // SAFETY: the scenario keeps cur + 300 below OPT_NUM
                    // and lengths below the mirror's 1024 cells.
                    unsafe {
                        scan_descending(
                            &mut reference.prices,
                            &mut reference.entries,
                            &state,
                            &matches,
                            nb,
                            min_match,
                            cur,
                            base_price,
                            &mut reference.last_pos,
                        );
                        price_set_scalar(
                            &mut scalar.prices,
                            &mut scalar.entries,
                            &state,
                            &matches,
                            nb,
                            min_match,
                            cur,
                            base_price,
                            &mut scalar.last_pos,
                        );
                        if vector {
                            price_set_avx2(
                                &mut avx2.prices,
                                &mut avx2.entries,
                                &state,
                                &matches,
                                nb,
                                min_match,
                                cur,
                                base_price,
                                &mut avx2.last_pos,
                            );
                        }
                    }
                }
                for p in 0..OPT_SIZE {
                    assert_eq!(
                        twins[1].prices[p], twins[0].prices[p],
                        "prices {p}, s{scenario} t{step}"
                    );
                    assert_eq!(
                        twins[1].fields(p),
                        twins[0].fields(p),
                        "entry {p}, s{scenario} t{step}"
                    );
                    if vector {
                        assert_eq!(
                            twins[2].prices[p], twins[0].prices[p],
                            "avx prices {p}, s{scenario} t{step}"
                        );
                        assert_eq!(
                            twins[2].fields(p),
                            twins[0].fields(p),
                            "avx entry {p}, s{scenario} t{step}"
                        );
                    }
                }
                for t in 1..twins.len() {
                    assert_eq!(
                        twins[t].last_pos, twins[0].last_pos,
                        "last_pos, s{scenario} t{step}"
                    );
                }
            }
        }
    }
}
