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
        // The unpledged stream stays on the stock scan: its screen sees
        // only the first copy's novel 4 MiB run (the repeats start at the
        // 16 MiB period) and rejects — the documented miss-class — so
        // three unit copies re-encode from scratch, while the armed parse
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
/// stream screens its staged head too (R23): its windowed no-FCS form
/// matches the bulk frame byte for byte.
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
    let mut data = Vec::with_capacity(4 * head_unit + 11 * unit_len + 17);
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
    // A 17-byte tail keeps the total off the block grid, so the unpledged
    // stream's finish encodes its final partial block as the last block
    // (an eagerly drained stream ending exactly on the grid would emit the
    // 3-byte empty closing block instead — the pledged path's own
    // documented closing-form difference).
    data.extend_from_slice(&[0xa5; 17]);
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
        // The unpledged stream screens its staged head (R23): same sample,
        // same verdict, same parse — its windowed no-FCS form matches the
        // bulk frame byte for byte, header included.
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
        assert_eq!(sink, armed, "{level:?}: unpledged stream vs bulk");
        // A flush below the screen's staging span cancels it: the frame
        // stays stock (stock window descriptor), and the unit copies
        // re-encode from scratch.
        let mut flushed = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut flushed,
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            enc.write_all(&data[..64 * 1024]).unwrap();
            enc.flush().unwrap();
            for piece in data[64 * 1024..].chunks(256 * 1024) {
                enc.write_all(piece).unwrap();
            }
            enc.finish().unwrap();
        }
        assert_ne!(flushed[5], 0x80, "{level:?}: flush-cancelled descriptor");
        assert!(
            armed.len() * 2 < flushed.len(),
            "{level:?}: armed {} vs stock flush-cancelled {}",
            armed.len(),
            flushed.len()
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
        // The unpledged random stream screens and rejects: its frame is
        // byte-for-byte the stock bulk frame.
        let mut rsink = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut rsink,
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            for piece in random.chunks(256 * 1024) {
                enc.write_all(piece).unwrap();
            }
            enc.finish().unwrap();
        }
        assert_eq!(rsink, stock, "{level:?}: unpledged random stream vs stock");
    }
}

/// The unpledged stream entry's far-class screen (R23): no declared
/// length exists at header time, so the staged head sample alone decides —
/// a dll-shaped stream arms whatever its total length (the window
/// descriptor is a maximum), the verdict is chunk-pattern-independent
/// (the staging gate is a byte-count gate), and every below-gate exit
/// (flush, early finish) plus the rejected classes stay byte-identical to
/// the stock output. The Read-path compressor routes through the same
/// core and arms identically.
#[cfg(feature = "std")]
#[test]
fn fast_rows_unpledged_stream_screen_arms_and_gate_edges_stay_stock() {
    // The dll-class head recipe (see the mid-band test): a 1 MiB head of
    // 256 KiB-period mutated copies clears the screen's cheap-reject bar,
    // then 3 MiB units put the copy distance past both rows' stock scan
    // windows — 13 MiB total, below the mid band everywhere: no pledged
    // or bulk entry arms on this data, the unpledged screen alone does.
    let head_unit = 256 * 1024;
    let head = xorshift(head_unit);
    let unit_len = 3 * 1024 * 1024;
    let unit = xorshift(unit_len);
    let mut data = Vec::with_capacity(4 * head_unit + 4 * unit_len + 17);
    for (src, copies) in [(&head, 4u32), (&unit, 4u32)] {
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
    // Off the block grid (see the mid-band test's tail note).
    data.extend_from_slice(&[0xa5; 17]);
    for level in [Level::Fastest, Level::Fast] {
        // Arms: the windowed frame carries the far window (W26) and the
        // unit copies ride it, so the armed size sits far under the stock
        // parse a below-gate exit keeps.
        let sink = unpledged_stream(&data, level);
        assert_eq!(sink[5], 0x80, "{level:?}: armed window descriptor");
        assert_eq!(
            crate::bulk::decompress(sink.as_slice(), data.len()).unwrap(),
            data
        );
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(sink.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
        // Determinism: the same bytes written in irregular 1 B..1 MiB
        // chunks encode identically (the staging gate counts bytes, not
        // writes).
        let ragged = unpledged_stream_ragged(&data, level);
        assert_eq!(ragged, sink, "{level:?}: ragged chunk pattern");
        // The Read-path compressor arms identically (16 KiB pulls).
        let mut read_sink = Vec::new();
        {
            let mut enc = crate::stream::read::Encoder::with_options(
                data.as_slice(),
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            std::io::Read::read_to_end(&mut enc, &mut read_sink).unwrap();
        }
        assert_eq!(read_sink, sink, "{level:?}: read-path stream");
        // A flush below the 4 MiB gate cancels the screen: stock window
        // descriptor and the stock parse (the unit copies re-encode).
        let mut flushed = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut flushed,
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            enc.write_all(&data[..128 * 1024]).unwrap();
            enc.flush().unwrap();
            for piece in data[128 * 1024..].chunks(256 * 1024) {
                enc.write_all(piece).unwrap();
            }
            enc.finish().unwrap();
        }
        assert_ne!(flushed[5], 0x80, "{level:?}: flush-cancelled descriptor");
        assert!(
            sink.len() * 2 < flushed.len(),
            "{level:?}: armed {} vs flush-cancelled {}",
            sink.len(),
            flushed.len()
        );
        // A stream that ends below the gate stays stock: the finish's
        // cancel keeps the stock window and the stock parse of the prefix.
        let short_len = 3 * 1024 * 1024;
        let mut short = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut short,
                crate::EncoderOptions::new(level),
            )
            .unwrap();
            enc.write_all(&data[..short_len]).unwrap();
            enc.finish().unwrap();
        }
        assert_ne!(short[5], 0x80, "{level:?}: short-stream descriptor");
        assert_eq!(
            crate::bulk::decompress(short.as_slice(), short_len).unwrap(),
            data[..short_len]
        );
        // The pledged entry's band did not move: a below-band pledge
        // keeps the stock descriptor.
        let pledged = pledged_stream(data.as_slice(), level);
        assert_ne!(pledged[5], 0x80, "{level:?}: below-band pledged descriptor");
        // A random head rejects (wide alphabet, collision-level twins):
        // the unpledged stream stays byte-identical to the stock bulk
        // frame, whose own mid-band screen rejected the same head.
        let random = xorshift(data.len());
        let stock = crate::encoding::compress_slice_to_vec(random.as_slice(), level);
        let rsink = unpledged_stream(&random, level);
        assert_eq!(rsink, stock, "{level:?}: unpledged random stream vs stock");
    }
}

#[cfg(feature = "std")]
fn unpledged_stream(data: &[u8], level: Level) -> Vec<u8> {
    let mut sink = Vec::new();
    let mut enc =
        crate::stream::write::Encoder::with_options(&mut sink, crate::EncoderOptions::new(level))
            .unwrap();
    for piece in data.chunks(256 * 1024) {
        std::io::Write::write_all(&mut enc, piece).unwrap();
    }
    enc.finish().unwrap();
    sink
}

/// [`unpledged_stream`] over an irregular write pattern: deterministic
/// xorshift-driven chunk sizes in 1 B..1 MiB, so the staging gate sees
/// every kind of write boundary.
#[cfg(feature = "std")]
fn unpledged_stream_ragged(data: &[u8], level: Level) -> Vec<u8> {
    let mut sink = Vec::new();
    let mut enc =
        crate::stream::write::Encoder::with_options(&mut sink, crate::EncoderOptions::new(level))
            .unwrap();
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut off = 0usize;
    while off < data.len() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = (((x >> 33) as usize) % (1 << 20) + 1).min(data.len() - off);
        std::io::Write::write_all(&mut enc, &data[off..off + n]).unwrap();
        off += n;
    }
    enc.finish().unwrap();
    sink
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
