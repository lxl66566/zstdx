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
//!
//! `cross_window_seed` additionally constructs a beyond-window redundancy
//! payload per input (R73): the R71 campaign proved evolved inputs never
//! build multi-KiB-period structure, so that class is pinned here as
//! committed seed-construction code instead.

use libfuzzer_sys::fuzz_target;
use zstdx::{Level, decoding::FrameDecoder};

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

/// Beyond-window redundancy under a forced window — the R71 campaign's
/// honest limit (coverage-guided mutation does not build multi-KiB-period
/// structure from <=4096 B seeds), constructed from the fuzz bytes on
/// every input (the R66 `weight_alphabet_cap_seed` pattern; the builder
/// is duplicated in `encode_stream.rs`, like the input parser). The input
/// picks the family, the window log, the level row and the fragment
/// pool; the payload then carries its twins only at spans the declared
/// window does not cover. The reference decoder rejects any emitted
/// offset the window does not cover (the todo-26 class), so the roundtrip
/// through both decoders pins the search-domain clamping.
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
    // The chain_reach families (chain 9, btlazy 13-15, opt 16-19) plus the
    // no-reach control row.
    let rows = [9i32, 13, 16, 19, 22];
    let level = Level::from_zstd(rows[byte(3) as usize % rows.len()]);
    let opts = zstdx::EncoderOptions::new(level)
        .with_input_shape(zstdx::InputShape::default().with_window_log(window_log as u32));

    let comp = zstdx::bulk::compress_with(&payload, &opts).unwrap();
    let mut back = Vec::with_capacity(payload.len() + 16);
    FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(
        &back[..],
        &payload[..],
        "zstdx decode of own output differs"
    );

    let mut zback = Vec::new();
    zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
    assert_eq!(
        &zback[..],
        &payload[..],
        "zstd decode of zstdx output differs"
    );
}

/// One beyond-window payload family (mirrored in `encode_stream.rs` and
/// `tests/fuzz_regressions.rs::test_forced_window_streams`):
/// 0: fragment pool re-drawn at pool-cycle spans — every twin sits many
///    windows out, none nearer (the `farfrags` class),
/// 1: twin halves — the whole first half replayed a half-payload later
///    (the `twinhalves` class),
/// 2: near-twin high period — one unit of >window bytes repeated with a
///    few byte flips per copy, so every match needs its extend/verify.
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
            // The second half replays the first half's spans with fresh
            // noise between: matches at ~len/2 distance, none nearer.
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
            // Period in (1.5*window, 2*window]: every period boundary is a
            // near twin one window past the previous copy.
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
