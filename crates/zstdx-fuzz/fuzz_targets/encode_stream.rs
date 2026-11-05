#![no_main]
//! Streaming-encoder coverage on the `encode` target's input layout
//! (mirrored byte for byte in `crates/zstdx-fuzz/fuzz_targets/encode.rs`
//! and `crates/zstdx/src/tests/fuzz_regressions.rs::
//! test_all_encode_artifacts` — separate crates, so the tiny parser is
//! duplicated): `[level, opts, k, rest]`, chunk `i` at zstd level
//! `(level + i) % 23` walking one thread through consecutive levels, the
//! option bits identical for every chunk (`&1` checksum, `&2` pledged
//! size = the chunk's own length, bits 2..5 window nibble, bits 6..7 LDM
//! tri-state), `k` with bit 7 clear = length-prefixed chunks ahead of a
//! final remainder chunk covering all of `rest`; bit 7 set switches
//! `rest` to structure mode, a small program expanded into one large
//! payload (see `encode.rs::expand_program`, duplicated here — `ph`
//! bits 5..6 pick the streaming core and, on the read core, its feed
//! entry).
//!
//! The feed surface rides two derived fields: the level byte's spare
//! high bits set the worker count (`level % 23` consumes the low bits;
//! `1 + level / 23` gives the single-threaded core and every mt count
//! up to 12), and the read core's feed entry alternates between the
//! plain slice, the NT-store `NtSource` and the zero-copy
//! `OwnedEncoder` — so the campaign reaches the mt stream core, its
//! stepped finish drain and both zero-copy feed entries, not only the
//! single-threaded core the fixed layout used to drive.
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
//! `structured_seed` (R74) does the same through the program grammar at
//! block-crossing scale.

use std::io::Read;

use libfuzzer_sys::fuzz_target;
use zstdx::Level;

fuzz_target!(|data: &[u8]| {
    cross_window_seed(data);
    structured_seed(data);

    let (level_byte, opt_byte, k, rest) = match data {
        [l, o, k, rest @ ..] => (*l, *o, *k, rest),
        [l, o] => (*l, *o, 0, &[][..]),
        [l] => (*l, 0, 0, &[][..]),
        _ => (0, 0, 0, &[][..]),
    };

    if k & 0x80 != 0 {
        structured_roundtrip(level_byte, opt_byte, rest);
        return;
    }

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
        opts = opts
            .long_distance_matching(ldm)
            .workers(workers_of(level_byte));

        let mut comp = Vec::new();
        match entry_of(rest) {
            Entry::Plain => {
                let mut enc = zstdx::stream::read::Encoder::with_options(chunk, opts).unwrap();
                enc.read_to_end(&mut comp).unwrap();
                enc.finish().unwrap();
            },
            Entry::Nt => {
                let mut enc = zstdx::stream::read::Encoder::with_options(
                    zstdx::stream::read::NtSource::new(chunk),
                    opts,
                )
                .unwrap();
                enc.read_to_end(&mut comp).unwrap();
                enc.finish().unwrap();
            },
            Entry::Owned => {
                let mut enc =
                    zstdx::stream::read::OwnedEncoder::with_options(chunk.to_vec().into(), opts)
                        .unwrap();
                enc.read_to_end(&mut comp).unwrap();
                enc.finish().unwrap();
            },
        }

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

/// Worker count from the level byte's spare high bits: `level % 23`
/// consumes the low ones, so the drive sweeps the single-threaded core
/// and the mt counts 2..=12 alongside the levels.
fn workers_of(level_byte: u8) -> u32 {
    1 + u32::from(level_byte) / 23
}

/// The chunked walk's read-core feed entry, off `rest`'s first byte.
enum Entry {
    Plain,
    Nt,
    Owned,
}

fn entry_of(rest: &[u8]) -> Entry {
    match rest.first().copied().unwrap_or(0) % 3 {
        1 => Entry::Nt,
        2 => Entry::Owned,
        _ => Entry::Plain,
    }
}

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

/// Structure-mode expansion cap — see `encode.rs` (duplicated).
const EXPAND_CAP: usize = 256 * 1024;

/// FNV-1a over the program bytes; `| 1` keeps the xorshift state nonzero.
fn program_seed(prog: &[u8]) -> u64 {
    prog.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    }) | 1
}

/// Operand readers; bytes past the program end read as zero, so truncated
/// events execute with defaulted operands instead of failing.
fn u16_field(prog: &[u8], i: usize) -> usize {
    usize::from(u16::from_le_bytes([
        prog.get(i).copied().unwrap_or(0),
        prog.get(i + 1).copied().unwrap_or(0),
    ]))
}

fn u24_field(prog: &[u8], i: usize) -> usize {
    u16_field(prog, i) | (usize::from(prog.get(i + 2).copied().unwrap_or(0)) << 16)
}

/// Structure-mode program expansion, byte-identical to
/// `encode.rs::expand_program` (duplicated like the input parser). `ph`
/// bit 0 attaches a synthetic raw dictionary sized `1 << (4 + bits 1..4)`;
/// event tags are `op << 6`: 0 LIT (verbatim run), 1 REP (copy from a
/// chosen back-distance in the virtual history with a few flipped bytes),
/// 2 NOISE, 3 PAD (constant run). Every length clamps to the program end
/// and the cap; garbage degrades to fewer events and never panics.
fn expand_program(prog: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut rng = Rng(program_seed(prog));
    let ph = prog.first().copied().unwrap_or(0);
    let dict = if ph & 1 != 0 {
        let log = 4 + usize::from((ph >> 1) & 0x0f);
        Some((0..1usize << log.min(19)).map(|_| rng.byte()).collect())
    } else {
        None
    };
    let dict_len = dict.as_ref().map_or(0, Vec::len);
    let mut out: Vec<u8> = Vec::new();
    let mut i = 1;
    while i < prog.len() && out.len() < EXPAND_CAP {
        let tag = prog[i];
        i += 1;
        match tag >> 6 {
            0 => {
                let n = (usize::from(tag & 0x3f) + 1)
                    .min(prog.len() - i)
                    .min(EXPAND_CAP - out.len());
                out.extend_from_slice(&prog[i..i + n]);
                i += n;
            },
            1 => {
                let dist = u24_field(prog, i) + 1;
                let len = (u16_field(prog, i + 3) + 1).min(EXPAND_CAP - out.len());
                let flips = prog.get(i + 5).copied().unwrap_or(0) & 0x0f;
                i = (i + 6).min(prog.len());
                // Virtual history = dict ++ out. The source index stays
                // behind the write cursor, so an overlapping copy reads
                // the bytes it just wrote.
                let hist = dict_len + out.len();
                if hist == 0 {
                    continue;
                }
                let base = hist - dist.min(hist);
                let start = out.len();
                for j in 0..len {
                    let b = if base + j < dict_len {
                        dict.as_ref().unwrap()[base + j]
                    } else {
                        out[base + j - dict_len]
                    };
                    out.push(b);
                }
                for _ in 0..flips {
                    let p = start + rng.next() as usize % (out.len() - start);
                    out[p] = rng.byte();
                }
            },
            2 => {
                let n = (u24_field(prog, i) + 1).min(EXPAND_CAP - out.len());
                i = (i + 3).min(prog.len());
                for _ in 0..n {
                    out.push(rng.byte());
                }
            },
            _ => {
                let n = (u24_field(prog, i) + 1).min(EXPAND_CAP - out.len());
                let v = prog.get(i + 3).copied().unwrap_or(0);
                i = (i + 4).min(prog.len());
                out.extend(vec![v; n]);
            },
        }
    }
    (out, dict)
}

/// One structure-mode roundtrip through the streaming cores: `ph` bits
/// 5..6 pick the write core in 7 KiB pieces (the chunking machinery plus
/// the owned-window adoption a large payload drives) or the read core
/// through one of its three feed entries. The dictionary rides the
/// options into the stream encoder; both decoders get it attached on
/// the way back.
fn structured_roundtrip(level_byte: u8, opt_byte: u8, prog: &[u8]) {
    let (payload, dict) = expand_program(prog);
    let level = Level::from_zstd(i32::from(level_byte) % 23);
    let mut opts = zstdx::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
    if opt_byte & 2 != 0 {
        opts = opts.pledged_size(Some(payload.len() as u64));
    }
    let window = (opt_byte >> 2) & 0x0f;
    if window != 0 {
        opts = opts
            .with_input_shape(zstdx::InputShape::default().with_window_log(u32::from(window) + 9));
    }
    let ldm = match (opt_byte >> 6) & 3 {
        1 => zstdx::LdmMode::Enable,
        2 => zstdx::LdmMode::Disable,
        _ => zstdx::LdmMode::Auto,
    };
    opts = opts
        .long_distance_matching(ldm)
        .workers(workers_of(level_byte));
    if let Some(d) = dict.as_deref() {
        opts = opts.dictionary(d);
    }

    // `ph` bits 5..6 pick the core and feed: 0 the write core, 1..3 the
    // read core through the plain slice, the NT reader and the owned
    // source (bit 6 used to be the read/write pick alone; bit 5 unused).
    let comp = match (prog.first().copied().unwrap_or(0) >> 5) & 3 {
        1 => {
            let mut comp = Vec::new();
            let mut enc =
                zstdx::stream::read::Encoder::with_options(payload.as_slice(), opts).unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish().unwrap();
            comp
        },
        2 => {
            let mut comp = Vec::new();
            let mut enc = zstdx::stream::read::Encoder::with_options(
                zstdx::stream::read::NtSource::new(&payload),
                opts,
            )
            .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish().unwrap();
            comp
        },
        3 => {
            let mut comp = Vec::new();
            let mut enc =
                zstdx::stream::read::OwnedEncoder::with_options(payload.to_vec().into(), opts)
                    .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish().unwrap();
            comp
        },
        _ => {
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
        },
    };

    let back = match dict.as_deref() {
        Some(d) => {
            zstdx::bulk::decompress_with(&comp, 0, &zstdx::DecoderOptions::new().dictionary(d))
                .unwrap()
        },
        None => {
            let mut back = Vec::with_capacity(payload.len() + 16);
            zstdx::decoding::FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            back
        },
    };
    assert_eq!(
        &back[..],
        &payload[..],
        "zstdx decode of structured stream differs"
    );

    match dict.as_deref() {
        Some(d) => {
            let mut rbuf = vec![0u8; payload.len() + 64];
            let n = zstd::bulk::Decompressor::with_dictionary(d)
                .unwrap()
                .decompress_to_buffer(&comp, &mut rbuf)
                .unwrap();
            assert_eq!(
                n,
                payload.len(),
                "zstd decode of structured stream length differs"
            );
            assert_eq!(
                &rbuf[..n],
                &payload[..],
                "zstd decode of structured stream differs"
            );
        },
        None => {
            let mut zback = Vec::new();
            zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
            assert_eq!(
                &zback[..],
                &payload[..],
                "zstd decode of structured stream differs"
            );
        },
    }
}

/// Event writers for the seed constructor — see `encode.rs` (duplicated).
fn push_u24(prog: &mut Vec<u8>, v: usize) {
    prog.push((v & 0xff) as u8);
    prog.push(((v >> 8) & 0xff) as u8);
    prog.push(((v >> 16) & 0xff) as u8);
}

fn push_noise(prog: &mut Vec<u8>, n: usize) {
    prog.push(0x80);
    push_u24(prog, n - 1);
}

fn push_repeat(prog: &mut Vec<u8>, dist: usize, len: usize, flips: u8) {
    prog.push(0x40);
    push_u24(prog, dist - 1);
    prog.extend_from_slice(&(len as u16 - 1).to_le_bytes());
    prog.push(flips);
}

/// Committed structured-seed constructor on the streaming cores (the
/// R66/R73 pattern; construction duplicated from `encode.rs`, the core
/// choice folded into `ph` bit 6): twins past the 128 KiB block boundary
/// and dictionary echoes through the owned-window machinery, every
/// input, regardless of corpus state.
fn structured_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    let period = [17 * 1024, 40 * 1024, 96 * 1024, 200 * 1024][byte(0) as usize & 3];
    let twin = 6 * 1024usize;
    let gap = 64 + byte(6) as usize % 512;
    let dict_log = 6 + byte(2) as usize % 6; // 64 B..2 KiB
    let mut prog = Vec::new();
    if byte(2) & 3 == 0 {
        prog.push(1 | ((dict_log - 4) as u8) << 1);
    } else {
        prog.push(0);
    }
    // Core and feed: 0 the write core, 1..3 the read core's plain,
    // NT-store and owned entries, so the constructor sweeps all four.
    prog[0] |= (byte(9) & 3) << 5;
    let litn = 8 + byte(4) as usize % 57;
    prog.push(litn as u8 - 1);
    prog.extend((0..litn).map(|j| byte(j + 8)));
    push_noise(&mut prog, period - litn);
    // With the dictionary attached, the echo distance reaches into it:
    // the twins sit at dict content plus payload span, the prefill
    // match class.
    let dist = if prog[0] & 1 != 0 {
        (1 << dict_log) + period
    } else {
        period
    };
    for _ in 0..3 {
        push_repeat(&mut prog, dist, twin, byte(5) & 0x0f);
        push_noise(&mut prog, gap);
    }
    // Modest constant tail: repcode adaptation inside the last block.
    prog.push(0xc0);
    push_u24(&mut prog, 2048 + byte(7) as usize % 8192);
    prog.push(byte(7) ^ 0xa5);

    let rows = [3u8, 9, 13, 19, 22];
    let level_byte = rows[byte(3) as usize % rows.len()];
    let nibble = if byte(1) & 0x20 != 0 {
        1 + byte(1) % 7 // window log 10..=16; the 96/200 KiB periods sit past it
    } else {
        0
    };
    let opt_byte = (byte(1) & 3) | (nibble << 2);
    structured_roundtrip(level_byte, opt_byte, &prog);
}
