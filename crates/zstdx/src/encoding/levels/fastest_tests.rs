use alloc::vec::Vec;
#[cfg(test)]
#[cfg(feature = "std")]
use std::io::Write as _;

use crate::{Level, encoding::compress_to_vec};

#[test]
fn fastest_does_not_expand_incompressible_blocks_past_raw_size() {
    assert_fastest_does_not_exceed_raw(8 * 1024);
}

#[test]
fn fastest_does_not_expand_incompressible_max_size_blocks() {
    assert_fastest_does_not_exceed_raw(128 * 1024);
}

fn assert_fastest_does_not_exceed_raw(len: usize) {
    let data = xorshift(len);
    let raw = compress_to_vec(data.as_slice(), Level::Uncompressed);
    let fastest = compress_to_vec(data.as_slice(), Level::Fastest);

    assert!(
        fastest.len() <= raw.len(),
        "fastest output should not exceed raw frame size for {len} bytes: {} > {}",
        fastest.len(),
        raw.len()
    );
}

/// The fast rows' LDM far class (R19): a full-window (64 MiB) frame arms
/// the gap-parse model — the mutated unit copies repeat at 16 MiB periods,
/// far beyond both rows' stock scan domains, so the armed bulk parse must
/// ride them wholesale while the unpledged stream (which never arms on
/// these rows) re-encodes each copy from scratch. Decodes through both
/// implementations either way.
#[cfg(feature = "std")]
#[test]
fn fast_rows_far_repeats_ride_ldm_at_full_window() {
    let unit_len = 16 * 1024 * 1024;
    let unit = xorshift(unit_len);
    let mut data = Vec::with_capacity(4 * unit_len);
    for copy in 0..4u32 {
        for (i, &b) in unit.iter().enumerate() {
            // One flipped byte per 4 KiB keeps the copies' 64-byte windows
            // intact while no copy is exact (the mt test corpus's recipe).
            data.push(if i % 4096 == (copy as usize * 1024) % 4096 {
                b ^ 0x5a
            } else {
                b
            });
        }
    }
    for level in [Level::Fastest, Level::Fast] {
        let armed = crate::encoding::compress_slice_to_vec(data.as_slice(), level);
        let mut sink = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut sink,
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            for piece in data.chunks(256 * 1024) {
                enc.write_all(piece).unwrap();
            }
            enc.finish().unwrap();
        }
        assert_eq!(
            crate::bulk::decompress(armed.as_slice(), data.len()).unwrap(),
            data
        );
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(armed.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        // The unpledged stream stays on the stock scan: three unit copies
        // re-encoded from scratch dominate its size, while the armed parse
        // sells each copy as one far match.
        assert!(
            armed.len() * 2 < sink.len(),
            "{level:?}: armed {} vs stock stream {}",
            armed.len(),
            sink.len()
        );
    }
}

fn xorshift(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut data = Vec::with_capacity(len);
    while data.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.extend_from_slice(&state.to_le_bytes());
    }
    data.truncate(len);
    data
}
