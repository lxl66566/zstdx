#![no_main]
//! Streaming-encoder coverage on the `encode` target's input layout
//! (mirrored byte for byte in `crates/zstdx-fuzz/fuzz_targets/encode.rs`
//! and `crates/zstdx/src/tests/fuzz_regressions.rs::
//! test_all_encode_artifacts` — separate crates, so the tiny parser is
//! duplicated): `[level, opts, k, rest]`, chunk `i` at zstd level
//! `(level + i) % 23` walking one thread through consecutive levels, the
//! option bits identical for every chunk (`&1` checksum, `&2` pledged
//! size = the chunk's own length, bits 2..5 window nibble, bits 6..7 LDM
//! tri-state), `k` length-prefixed chunks ahead of a final remainder
//! chunk covering all of `rest`.
//!
//! Each chunk drives the read-side streaming core (one `read_to_end`
//! pull, the `matrix --mode enc-stream` / `dump --stream` drive). The
//! streaming core takes the same `EncoderOptions`, so every option bit
//! of the layout — the window nibble included — is expressible here;
//! none is skipped. The frame must decode back through both decoders.
//!
//! Byte identity with the bulk path is deliberately NOT asserted here:
//! the matcher's pooled state survives between frames, so the same input
//! can produce different (equally valid) frames depending on what ran
//! before in the process. Cross-build byte comparisons must use fresh
//! processes (the zstdx-bench `dump` tool).
//!
//! `cross_window_seed` additionally constructs a beyond-window redundancy
//! payload per input (R73) and drives it through BOTH stream cores with
//! no pledge and no length hint — the unknown-length param-resolution
//! path the todo-26 encoder bug lived on. The payload builder is
//! duplicated in `encode.rs` (separate functions, same families).

use std::io::Read;

use libfuzzer_sys::fuzz_target;
use zstdx::Level;

fuzz_target!(|data: &[u8]| {
    cross_window_seed(data);

    let (level_byte, opt_byte, k, rest) = match data {
        [l, o, k, rest @ ..] => (*l, *o, *k, rest),
        [l, o] => (*l, *o, 0, &[][..]),
        [l] => (*l, 0, 0, &[][..]),
        _ => (0, 0, 0, &[][..]),
    };

    let mut pos = 0;
    for i in 0..=k {
        let chunk = if i < k && rest.len() - pos >= 2 {
            let len = u16::from_le_bytes([rest[pos], rest[pos + 1]]) as usize;
            let start = pos + 2;
            let end = (start + len).min(rest.len());
            pos = end;
            &rest[start..end]
        } else {
            let chunk = &rest[pos..];
            pos = rest.len();
            chunk
        };

        let level = Level::from_zstd((i32::from(level_byte) + i32::from(i)) % 23);
        let mut opts = zstdx::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
        if opt_byte & 2 != 0 {
            opts = opts.pledged_size(Some(chunk.len() as u64));
        }
        // 0 keeps the row's window; 1..=15 force window log 10..=24 (the
        // format floor is 10, the engine cap 27).
        let window = (opt_byte >> 2) & 0x0f;
        if window != 0 {
            opts = opts.with_input_shape(
                zstdx::InputShape::default().with_window_log(u32::from(window) + 9),
            );
        }
        let ldm = match (opt_byte >> 6) & 3 {
            1 => zstdx::LdmMode::Enable,
            2 => zstdx::LdmMode::Disable,
            _ => zstdx::LdmMode::Auto,
        };
        opts = opts.long_distance_matching(ldm);

        let mut enc = zstdx::stream::read::Encoder::with_options(chunk, opts).unwrap();
        let mut comp = Vec::new();
        enc.read_to_end(&mut comp).unwrap();
        enc.finish().unwrap();

        let mut back = Vec::with_capacity(chunk.len() + 16);
        zstdx::decoding::FrameDecoder::new()
            .decode_all_to_vec(&comp, &mut back)
            .unwrap();
        assert_eq!(&back[..], chunk, "zstdx decode of streamed output differs");

        let mut zback = Vec::new();
        zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
        assert_eq!(&zback[..], chunk, "zstd decode of streamed output differs");
    }
});

/// Deterministic xorshift64 source, seeded from the fuzz bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }
}

/// Beyond-window redundancy under a forced window on an UNPLEDGED stream
/// (the todo-26 class): the payload's twins sit past the declared window,
/// the level rows are the chain_reach families the stock ladder once let
/// scan past it, and neither core receives a length — the unknown-length
/// path where `clamp_geometry_to_window` is the only guard. The reference
/// decoder rejects every offset the window does not cover. Families and
/// parameter derivation mirror `encode.rs::cross_window_seed`.
fn cross_window_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    let seed = u64::from_le_bytes([
        byte(0),
        byte(1),
        byte(2),
        byte(3),
        byte(4),
        byte(5),
        byte(6),
        byte(7),
    ]) | 0x9e37_79b9_7f4a_7c15;
    let window_log = 10 + byte(0) as usize % 4;
    let window = 1usize << window_log;
    let len = (window * 8).min(32 * 1024);
    let payload = cross_window_payload(&mut Rng(seed), data, window, len, byte(1) as usize % 3);
    let rows = [9i32, 13, 16, 19, 22];
    let level = Level::from_zstd(rows[byte(3) as usize % rows.len()]);
    let opts = zstdx::EncoderOptions::new(level)
        .with_input_shape(zstdx::InputShape::default().with_window_log(window_log as u32));

    // The write core crosses the chunking machinery; window 1 KiB with
    // 7 KiB pieces keeps every piece far past the window.
    let comp = if byte(4) & 1 == 0 {
        let mut sink = Vec::new();
        {
            use std::io::Write;
            let mut enc = zstdx::stream::write::Encoder::with_options(&mut sink, opts).unwrap();
            for piece in payload.chunks(7_000) {
                enc.write_all(piece).unwrap();
            }
            enc.finish().unwrap();
        }
        sink
    } else {
        let mut comp = Vec::new();
        let mut enc = zstdx::stream::read::Encoder::with_options(payload.as_slice(), opts).unwrap();
        enc.read_to_end(&mut comp).unwrap();
        enc.finish().unwrap();
        comp
    };

    let mut back = Vec::with_capacity(payload.len() + 16);
    zstdx::decoding::FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(
        &back[..],
        &payload[..],
        "zstdx decode of streamed output differs"
    );

    let mut zback = Vec::new();
    zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
    assert_eq!(
        &zback[..],
        &payload[..],
        "zstd decode of streamed output differs"
    );
}

/// One beyond-window payload family — see `encode.rs::cross_window_payload`
/// (duplicated builder, same families).
fn cross_window_payload(
    rng: &mut Rng,
    data: &[u8],
    window: usize,
    len: usize,
    family: usize,
) -> Vec<u8> {
    let byte = |i: usize| data.get(i % data.len().max(1)).copied().unwrap_or(0x5a);
    let mut out = Vec::with_capacity(len);
    match family {
        0 => {
            let nfrag = 8 + rng.next() as usize % 40;
            let frag_len = 8 + rng.next() as usize % 24;
            let frags: Vec<Vec<u8>> = (0..nfrag)
                .map(|k| (0..frag_len).map(|j| byte(k * 37 + j)).collect())
                .collect();
            let gap = window / 2 + rng.next() as usize % (window / 2).max(1) + 1;
            while out.len() < len {
                out.extend_from_slice(&frags[rng.next() as usize % nfrag]);
                for _ in 0..gap {
                    out.push(rng.byte());
                }
            }
        },
        1 => {
            let frags: Vec<Vec<u8>> = (0..24)
                .map(|k| (0..8 + k % 32).map(|j| byte(k * 41 + j)).collect())
                .collect();
            while out.len() < len / 2 {
                out.extend_from_slice(&frags[rng.next() as usize % frags.len()]);
                for _ in 0..(8 + rng.next() as usize % 24) {
                    out.push(rng.byte());
                }
            }
            let first = out.clone();
            let mut i = 0usize;
            while out.len() < len {
                let end = (i + 24).min(first.len());
                out.extend_from_slice(&first[i..end]);
                i = if end == first.len() {
                    0
                } else {
                    end
                };
                for _ in 0..(8 + rng.next() as usize % 24) {
                    out.push(rng.byte());
                }
            }
        },
        _ => {
            let period = window + window / 2 + rng.next() as usize % (window / 2).max(1) + 1;
            let unit: Vec<u8> = (0..period).map(|j| byte(j)).collect();
            while out.len() < len {
                let start = out.len();
                out.extend_from_slice(&unit[..(len - out.len()).min(period)]);
                for _ in 0..(1 + rng.next() as usize % 4) {
                    let p = start + rng.next() as usize % period;
                    if p < out.len() {
                        out[p] = rng.byte();
                    }
                }
            }
        },
    }
    out.truncate(len);
    out
}
