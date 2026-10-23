#![no_main]
//! Encoder roundtrip across the whole numeric ladder, the option surface
//! and the per-thread pooled state: input layout (mirrored byte for byte
//! in `fuzz_targets/encode_stream.rs` and
//! `crates/zstdx/src/tests/fuzz_regressions.rs::test_all_encode_artifacts`
//! — separate crates, so the tiny parser is duplicated):
//!
//! `[level, opts, k, rest...]`
//! - `level`: chunk `i` runs at zstd level `(level + i) % 23`, so every strategy row (raw block,
//!   fast, dfast, the tagged rows, the chain band, the wide rows 20-22) is reachable, and one input
//!   walks a LADDER of consecutive levels through one thread — the per-thread pooled slice state
//!   (tables/second carryover, the R68 bug 3 class) must survive every level transition.
//! - `opts` bits, identical for every chunk: `&1` checksum, `&2` pledged size (the chunk's own
//!   length), bits 2..5 a window nibble (0 keeps the row's window, 1..=15 force window log nibble+9
//!   — sub-block windows included, offsets must stay inside whatever window the frame declares),
//!   bits 6..7 the LDM tri-state (0/3 Auto, 1 Enable, 2 Disable).
//! - `k`: number of length-prefixed chunks ahead of a final remainder chunk. `pos = 0`; for `i in
//!   0..=k`: when `i < k` and at least two bytes of `rest` remain, a `u16` LE length prefix at
//!   `rest[pos..]` picks `chunk = rest[pos+2 .. pos+2+len]` (clamped to the available bytes) and
//!   advances `pos` past it; otherwise the chunk is the whole remainder — so ALL of `rest` is
//!   covered. Inputs shorter than the header bytes degrade to a single empty payload.
//!
//! Every chunk's frame must decode back to that chunk through both our
//! decoder and the reference implementation.

use libfuzzer_sys::fuzz_target;
use zstdx::{Level, decoding::FrameDecoder};

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

        let comp = zstdx::bulk::compress_with(chunk, &opts).unwrap();

        let mut back = Vec::with_capacity(chunk.len() + 16);
        FrameDecoder::new()
            .decode_all_to_vec(&comp, &mut back)
            .unwrap();
        assert_eq!(&back[..], chunk, "zstdx decode of own output differs");

        let mut zback = Vec::new();
        zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
        assert_eq!(&zback[..], chunk, "zstd decode of zstdx output differs");
    }
});
