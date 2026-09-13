//! Literal-length / match-length / offset code computation shared by the
//! matcher's emit path and the block encoder. The matcher pushes one packed
//! [`SeqWord`](super::SeqWord) per sequence (`code << 16 | ml << 8 | ll`,
//! merged add-bits payload, total add-bit count), so the block encoder never
//! re-reads the raw (ll, ml, of) triples.

use super::SeqWord;

/// Exclusive upper bound of every wire code value (LL ≤ 35, ML ≤ 52,
/// OF ≤ 31): each code fits 6 bits. Sizes the encoder-side sequence-code
/// histograms and normalization scratch, which never see a code ≥ 64.
pub(crate) const SEQ_CODE_SPACE: usize = 64;

/// Pack one (ll, ml, of-wire) triple into the word the block encoder
/// consumes: the three code bytes plus the code-specific add bits merged
/// into a single payload and width.
#[inline(always)]
pub(crate) fn pack_seq(ll: u32, ml: u32, of: u32) -> SeqWord {
    // Low codes carry no add bits (LL codes 0-15 are ll itself, ML codes
    // 0-31 are ml-3) and cover the dominant mass on every structured shape
    // (json.fast: 99.997% of ll, 96.5% of ml), so the merged payload
    // degenerates to the offset's own bits — no LUT/META loads, no
    // variable-shift merge.
    debug_assert!(ml >= 3, "match lengths below 3 cannot be encoded");
    if ll < 16 && ml < 35 {
        let log = of.ilog2();
        return SeqWord {
            codes: ll | ((ml - 3) << 8) | (log << 16),
            add: (of & ((1 << log) - 1)) as u64,
            add_nb: log as u8,
        };
    }
    let (lc, la, ln) = encode_literal_length(ll);
    let (mc, ma, mn) = encode_match_len(ml);
    let (oc, oa, on) = encode_offset(of);
    debug_assert!((lc as usize) < SEQ_CODE_SPACE);
    debug_assert!((mc as usize) < SEQ_CODE_SPACE);
    debug_assert!((oc as usize) < SEQ_CODE_SPACE);
    SeqWord {
        codes: lc as u32 | (mc as u32) << 8 | (oc as u32) << 16,
        add: la as u64 | ((ma as u64) << ln) | ((oa as u64) << (ln + mn)),
        add_nb: (ln + mn + on) as u8,
    }
}

/// Per-code metadata for literal lengths: (base value, extra bit count).
const fn ll_meta() -> [(u32, u8); 36] {
    let mut t = [(0u32, 0u8); 36];
    let mut code = 0usize;
    while code <= 15 {
        t[code] = (code as u32, 0);
        code += 1;
    }
    let rest = [
        (16u32, 1u8),
        (18, 1),
        (20, 1),
        (22, 1),
        (24, 2),
        (28, 2),
        (32, 3),
        (40, 3),
        (48, 4),
        (64, 6),
        (128, 7),
        (256, 8),
        (512, 9),
        (1024, 10),
        (2048, 11),
        (4096, 12),
        (8192, 13),
        (16384, 14),
        (32768, 15),
        (65536, 16),
    ];
    let mut i = 0;
    while i < rest.len() {
        t[16 + i] = rest[i];
        i += 1;
    }
    t
}
pub(crate) const LL_META: [(u32, u8); 36] = ll_meta();

/// Literal-length code for the dense low range; mirrors the ladder below.
const fn ll_code_lut() -> [u8; 64] {
    let mut t = [0u8; 64];
    let mut len = 0usize;
    while len < 64 {
        let mut code = 35;
        while code > 0 {
            if LL_META[code].0 as usize <= len {
                break;
            }
            code -= 1;
        }
        t[len] = code as u8;
        len += 1;
    }
    t
}
const LL_CODE_LUT: [u8; 64] = ll_code_lut();

#[inline]
pub(crate) fn encode_literal_length(len: u32) -> (u8, u32, usize) {
    if len < 64 {
        let code = LL_CODE_LUT[len as usize] as usize;
        let (base, bits) = LL_META[code];
        return (code as u8, len - base, bits as usize);
    }
    match len {
        64..=127 => (25, len - 64, 6),
        128..=255 => (26, len - 128, 7),
        256..=511 => (27, len - 256, 8),
        512..=1023 => (28, len - 512, 9),
        1024..=2047 => (29, len - 1024, 10),
        2048..=4095 => (30, len - 2048, 11),
        4096..=8191 => (31, len - 4096, 12),
        8192..=16383 => (32, len - 8192, 13),
        16384..=32767 => (33, len - 16384, 14),
        32768..=65535 => (34, len - 32768, 15),
        65536..=131071 => (35, len - 65536, 16),
        _ => unreachable!(),
    }
}

/// Per-code metadata for match lengths: (base value, extra bit count).
const fn ml_meta() -> [(u32, u8); 53] {
    let mut t = [(0u32, 0u8); 53];
    let mut code = 0usize;
    // codes 0..=31 encode len = code + 3 directly
    while code < 32 {
        t[code] = (code as u32 + 3, 0);
        code += 1;
    }
    let rest = [
        (35u32, 1u8),
        (37, 1),
        (39, 1),
        (41, 1),
        (43, 2),
        (47, 2),
        (51, 3),
        (59, 3),
        (67, 4),
        (83, 4),
        (99, 5),
        (131, 7),
        (259, 8),
        (515, 9),
        (1027, 10),
        (2051, 11),
        (4099, 12),
        (8195, 13),
        (16387, 14),
        (32771, 15),
        (65539, 16),
    ];
    let mut i = 0;
    while i < rest.len() {
        t[32 + i] = rest[i];
        i += 1;
    }
    t
}
pub(crate) const ML_META: [(u32, u8); 53] = ml_meta();

/// Match-length code for the dense low range (len < 131).
const fn ml_code_lut() -> [u8; 131] {
    let mut t = [0u8; 131];
    let mut len = 0usize;
    while len < 131 {
        let mut code = 52;
        while code > 0 {
            if ML_META[code].0 as usize <= len {
                break;
            }
            code -= 1;
        }
        t[len] = code as u8;
        len += 1;
    }
    t
}
const ML_CODE_LUT: [u8; 131] = ml_code_lut();

#[inline]
pub(crate) fn encode_match_len(len: u32) -> (u8, u32, usize) {
    debug_assert!(len >= 3, "match lengths below 3 cannot be encoded");
    if len < 131 {
        let code = ML_CODE_LUT[len as usize] as usize;
        let (base, bits) = ML_META[code];
        return (code as u8, len - base, bits as usize);
    }
    match len {
        131..=258 => (43, len - 131, 7),
        259..=514 => (44, len - 259, 8),
        515..=1026 => (45, len - 515, 9),
        1027..=2050 => (46, len - 1027, 10),
        2051..=4098 => (47, len - 2051, 11),
        4099..=8194 => (48, len - 4099, 12),
        8195..=16386 => (49, len - 8195, 13),
        16387..=32770 => (50, len - 16387, 14),
        32771..=65538 => (51, len - 32771, 15),
        65539..=131074 => (52, len - 65539, 16),
        _ => unreachable!(),
    }
}

/// Offset code: the wire value's log2 plus its low bits as the add payload.
#[inline]
pub(crate) fn encode_offset(len: u32) -> (u8, u32, usize) {
    let log = len.ilog2();
    let lower = len & ((1 << log) - 1);
    (log as u8, lower, log as usize)
}

/// Inverse of the packed emit: reconstruct (ll, ml, of-wire) from one packed
/// code triple and its merged add-bits payload. Codes carry their add-bit
/// counts in LL_META/ML_META and the offset's count is the code itself, so no
/// per-field widths are stored.
#[inline]
pub(crate) fn decode_packed(packed: u32, add: u64) -> (u32, u32, u32) {
    let ll_code = (packed & 0xff) as usize;
    let ml_code = ((packed >> 8) & 0xff) as usize;
    let of_code = (packed >> 16) as usize;
    let ll_nb = LL_META[ll_code].1 as u64;
    let ml_nb = ML_META[ml_code].1 as u64;
    let ll = LL_META[ll_code].0 as u64 + (add & ((1u64 << ll_nb) - 1));
    let ml = ML_META[ml_code].0 as u64 + ((add >> ll_nb) & ((1u64 << ml_nb) - 1));
    let of = (1u64 << of_code) + (add >> (ll_nb + ml_nb));
    (ll as u32, ml as u32, of as u32)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    /// decode_packed must invert the three encoders for every encodable
    /// value; the add payload layout is what the reconstruction relies on.
    #[test]
    fn packed_roundtrip() {
        let mut lls = Vec::new();
        for len in 0..131u32 {
            lls.push(len);
        }
        lls.extend([255, 256, 511, 4096, 65536, 131071]);
        let mut mls = Vec::new();
        for len in 3..131u32 {
            mls.push(len);
        }
        mls.extend([258, 514, 1026, 65538, 131074]);
        let ofs: Vec<u32> = (1..70).chain([255, 256, 1000, 65539, 786435]).collect();
        for &ll in &lls {
            let (lc, la, ln) = encode_literal_length(ll);
            for &ml in &mls {
                let (mc, ma, mn) = encode_match_len(ml);
                for &of in &ofs {
                    let (oc, oa, on) = encode_offset(of);
                    let packed = lc as u32 | (mc as u32) << 8 | (oc as u32) << 16;
                    let add = la as u64 | ((ma as u64) << ln) | ((oa as u64) << (ln + mn));
                    assert_eq!(
                        decode_packed(packed, add),
                        (ll, ml, of),
                        "ll {ll} ml {ml} of {of}"
                    );
                    let _ = on;
                }
            }
        }
    }
}
