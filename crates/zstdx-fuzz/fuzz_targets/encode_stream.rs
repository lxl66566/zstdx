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

use std::io::Read;

use libfuzzer_sys::fuzz_target;
use zstdx::Level;

fuzz_target!(|data: &[u8]| {
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
