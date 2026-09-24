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

/// The fast rows' mid-size band (R20): a 32 MiB far-class source arms on
/// the pre-header far-class screen — the mutated 1 MiB unit copies repeat
/// inside the 4 MiB head sample (the dll class's own 2-4 MiB distance
/// range), while a same-size random source shows collision-level twins
/// and stays stock. Both entries that hold their head at header time
/// (bulk, pledged stream) must reach the same verdict: their frames
/// share every block byte (the compat stream layer's header form — FCS,
/// single segment — is the only sanctioned difference). The unpledged
/// stream never arms on these rows (recorded residue).
#[cfg(feature = "std")]
#[test]
fn fast_rows_midband_screen_arms_and_random_stays_stock() {
    // The head block repeats from byte zero (the dll class's near-field
    // twins clear the screen's cheap-reject bar inside the first MiB),
    // and the 3 MiB units put the copy distance past both rows' stock
    // windows (768 KiB-2 MiB) while the copies stay visible inside the
    // 4 MiB head sample — the dll class's own dominant 2-4 MiB range.
    let head_unit = 256 * 1024;
    let head = xorshift(head_unit);
    let unit_len = 3 * 1024 * 1024;
    let unit = xorshift(unit_len);
    let mut data = Vec::with_capacity(4 * head_unit + 11 * unit_len);
    for (src, copies) in [(&head, 4u32), (&unit, 11u32)] {
        for copy in 0..copies {
            for (i, &b) in src.iter().enumerate() {
                data.push(if i % 4096 == (copy as usize * 1024) % 4096 {
                    b ^ 0x5a
                } else {
                    b
                });
            }
        }
    }
    let random = xorshift(data.len());
    for level in [Level::Fastest, Level::Fast] {
        let armed = crate::encoding::compress_slice_to_vec(data.as_slice(), level);
        assert_eq!(
            crate::bulk::decompress(armed.as_slice(), data.len()).unwrap(),
            data
        );
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(armed.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        // The windowed armed frame carries the far window (W26) in its
        // descriptor byte.
        assert_eq!(armed[5], 0x80, "{level:?}: armed window descriptor");
        // The pledged stream screens the same head sample: same verdict,
        // same parse — its single-segment form (pledged 32 MiB under the
        // 64 MiB window) spends 9 header bytes to the bulk form's 6.
        let pledged = pledged_stream(data.as_slice(), level);
        assert_eq!(&pledged[9..], &armed[6..], "{level:?}: pledged vs bulk");
        // The unpledged stream stays stock: the unit copies re-encode.
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
        assert!(
            armed.len() * 2 < sink.len(),
            "{level:?}: armed {} vs stock stream {}",
            armed.len(),
            sink.len()
        );
        // A same-size random head fails the screen (wide alphabet, zero
        // twins): the bulk frame keeps the stock windowed header — byte
        // for byte the header a below-band random source carries — and
        // the pledged stream's descriptor stays stock too.
        let stock = crate::encoding::compress_slice_to_vec(random.as_slice(), level);
        let below_band = crate::encoding::compress_slice_to_vec(&random[..5 * unit_len], level);
        assert_eq!(
            &stock[..6],
            &below_band[..6],
            "{level:?}: stock header moved"
        );
        let pledged = pledged_stream(random.as_slice(), level);
        assert_eq!(pledged[5], stock[5], "{level:?}: pledged stock descriptor");
        assert_eq!(
            crate::bulk::decompress(pledged.as_slice(), random.len()).unwrap(),
            random
        );
    }
}

#[cfg(feature = "std")]
fn pledged_stream(data: &[u8], level: Level) -> Vec<u8> {
    let mut sink = Vec::new();
    let mut enc = crate::stream::write::Encoder::with_options(
        &mut sink,
        crate::EncoderOptions::new(level).pledged_size(Some(data.len() as u64)),
    )
    .unwrap();
    for piece in data.chunks(256 * 1024) {
        std::io::Write::write_all(&mut enc, piece).unwrap();
    }
    enc.finish().unwrap();
    sink
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
