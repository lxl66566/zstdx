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
//! - `k` with bit 7 clear: number of length-prefixed chunks ahead of a final remainder chunk. `pos
//!   = 0`; for `i in 0..=k`: when `i < k` and at least two bytes of `rest` remain, a `u16` LE
//!   length prefix at `rest[pos..]` picks `chunk = rest[pos+2 .. pos+2+len]` (clamped to the
//!   available bytes) and advances `pos` past it; otherwise the chunk is the whole remainder — so
//!   ALL of `rest` is covered. Inputs shorter than the header bytes degrade to a single empty
//!   payload. Bit 7 set switches `rest` to structure mode: a small program expanded
//!   deterministically into one large payload (see [`expand_program`]), the R74 lever for the
//!   multi-KiB-period structure mutation never builds.
//!
//! Every chunk's frame must decode back to that chunk through both our
//! decoder and the reference implementation.
//!
//! `cross_window_seed` additionally constructs a beyond-window redundancy
//! payload per input (R73): the R71 campaign proved evolved inputs never
//! build multi-KiB-period structure, so that class is pinned here as
//! committed seed-construction code instead. `structured_seed` (R74) does
//! the same through the program grammar at block-crossing scale. `ramp_seed` compresses
//! one ramp-armed level-3-class MT frame per input plus plain workers-0/1
//! controls, the in-process ramp class the env-gated production arming
//! keeps out of every campaign.

use libfuzzer_sys::fuzz_target;
use zstdx::{Level, decoding::FrameDecoder, set_mt_ramp_depth_for_tests};

fuzz_target!(|data: &[u8]| {
    cross_window_seed(data);
    structured_seed(data);
    ramp_seed(data);

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

/// Structure-mode expansion cap: two full 128 KiB blocks plus tail, so
/// repeat events can cross block boundaries.
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

/// Structure mode (R74): `rest` is a small program expanded
/// deterministically into one payload of up to [`EXPAND_CAP`] bytes plus
/// an optional synthetic raw dictionary. A 7-byte repeat event demands a
/// copy at any distance up to 16 MiB, so `max_len` stays small while the
/// encoder sees multi-KiB-period structure: repeats past 128 KiB block
/// boundaries, past a declared window, and into dictionary content.
///
/// Grammar, after the program byte `ph` (bit 0 attaches the dictionary,
/// bits 1..4 set its log size `1 << (4 + v)`, bits 5..7 are ignored
/// here). Each event tag is `op << 6`:
/// - 0 LIT: `n = (tag & 0x3f) + 1` verbatim bytes follow;
/// - 1 REP: `dist = u24 + 1`, `len = u16 + 1`, `flips = u8 & 0x0f`; copies `len` bytes from `dist`
///   back in the virtual history (dictionary content, then output), so a distance past the output
///   echoes the dictionary; `flips` pseudo-random bytes inside the copied span are replaced — the
///   mutated-copy class;
/// - 2 NOISE: `n = u24 + 1` pseudo-random bytes;
/// - 3 PAD: `n = u24 + 1` copies of the next byte — long RLE spans.
///
/// Every length clamps to the program end and the cap, and an
/// unreachable distance clamps to the virtual history, so garbage
/// degrades to fewer events and never panics. Expansion is pure: one
/// xorshift stream seeded over the program bytes feeds the dictionary
/// content and every pseudo-random byte, so the walker mirrors it byte
/// for byte. Duplicated in `encode_stream.rs` and in the walker.
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

/// One structure-mode roundtrip through the bulk core: expand, encode on
/// the shared option surface, decode back through both decoders — with
/// the dictionary attached when the program built one, because the frame
/// declares it. Mirrored in the walker's artifact pass.
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
    opts = opts.long_distance_matching(ldm);
    if let Some(d) = dict.as_deref() {
        opts = opts.dictionary(d);
    }

    let comp = zstdx::bulk::compress_with(&payload, &opts).unwrap();

    let back = match dict.as_deref() {
        Some(d) => {
            zstdx::bulk::decompress_with(&comp, 0, &zstdx::DecoderOptions::new().dictionary(d))
                .unwrap()
        },
        None => {
            let mut back = Vec::with_capacity(payload.len() + 16);
            FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            back
        },
    };
    assert_eq!(
        &back[..],
        &payload[..],
        "zstdx decode of structured payload differs"
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
                "zstd decode of structured payload length differs"
            );
            assert_eq!(
                &rbuf[..n],
                &payload[..],
                "zstd decode of structured payload differs"
            );
        },
        None => {
            let mut zback = Vec::new();
            zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
            assert_eq!(
                &zback[..],
                &payload[..],
                "zstd decode of structured payload differs"
            );
        },
    }
}

/// Event writers for the seed constructor. Each writes exactly the
/// operand layout `expand_program` reads.
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

/// Committed structured-seed constructor (the R66/R73 pattern), the R74
/// scale class `cross_window_seed` cannot reach: it derives a compact
/// program from the fuzz bytes and roundtrips its expansion, so payload
/// shapes with twins past the 128 KiB block boundary — and dictionary
/// echoes — run on every input regardless of corpus state. The two large
/// period classes put every twin more than one block out; the window
/// nibble then decides whether they sit past the declared window too.
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

/// The ramp D lattice for the committed ramp frames: 512 KiB and 1 MiB
/// leave crossing reads legal at and past their band, and 2 MiB equals
/// the forced window log 21, so a crossing source always sits inside D
/// and every job start reads only in-job sources.
const RAMP_DEPTHS: [u64; 3] = [512 * 1024, 1024 * 1024, 2 * 1024 * 1024];

/// Rows that read the ramp gate, one per scan family: Fast (dfast),
/// Fastest (fast), level 5 (the row matcher) and Balanced (the chain).
/// The tree rows refuse `arm_ramp`, so the lattice stays inside the scan
/// rows.
const RAMP_LEVELS: [Level; 4] = [
    Level::Fast,
    Level::Fastest,
    Level::from_zstd(5),
    Level::Balanced,
];

/// Alias span for the ramp payloads: an echoed head puts crossing reads
/// at every multiple of the alias below a job start, so each D in the
/// lattice has both a blocked and a legal crossing class.
const RAMP_ALIAS: usize = 256 * 1024;

const RAMP_WORDS: [&[u8]; 8] = [
    b"ramp ", b"piece ", b"depth ", b"window ", b"lane ", b"grid ", b"job ", b"raster ",
];

/// One ramp payload: a word-mix head for shallow crossing reads at every
/// job start, then that head echoed back at alias distance with a few
/// flipped bytes per copy, so near-twin crossing reads need their
/// extend/verify path.
fn ramp_payload(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < RAMP_ALIAS.min(len) {
        let w = RAMP_WORDS[(rng.next() >> 33) as usize % RAMP_WORDS.len()];
        out.extend_from_slice(w);
    }
    out.truncate(RAMP_ALIAS.min(len));
    while out.len() < len {
        let from = out.len() - RAMP_ALIAS;
        let take = RAMP_ALIAS.min(len - out.len());
        let echo: Vec<u8> = out[from..from + take].to_vec();
        let start = out.len();
        out.extend_from_slice(&echo);
        for _ in 0..4 {
            let p = start + rng.next() as usize % take;
            out[p] = rng.byte();
        }
    }
    out
}

/// Committed ramp seed constructor: level-3-class frames under the MT
/// deep-offset ramp, built in-process because production arming is the
/// `ZSTDX_MT_RAMP_BYTES` env gate and campaigns run without it. One input
/// picks one D of the lattice over one row of the scan families on a
/// forced window log 21. The armed frame runs first at workers 4, its
/// payload sized past the row's strip so the job grid cuts jobs past
/// job zero where the gate arms, then two plain controls at workers 0
/// and 1 run through the same thread, one pledged and one open. The armed
/// jobs return to the shared state pool with the gate live, so the next
/// take — the next job of the same frame, the next input's armed frame,
/// or the in-process ladder walk — must reset it; `arm_ramp` refuses the
/// tree rows, so the override is stored around the armed frame and
/// restored before any other encode runs.
fn ramp_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    // Half the inputs run the class: the armed frame is a multi-MiB MT
    // encode on top of the existing constructors, and every campaign
    // process still cycles thousands of armed frames through the pool.
    if byte(6) & 1 != 0 {
        return;
    }
    let depth = RAMP_DEPTHS[byte(0) as usize % RAMP_DEPTHS.len()];
    let level = RAMP_LEVELS[byte(1) as usize % RAMP_LEVELS.len()];
    let pledged = byte(2) & 1 == 0;
    // The payload clears the row's strip by the smallest margin that
    // still cuts a second job, where the gate arms: the strip is the
    // forced 2 MiB window on the fast/dfast/row rows and the 4 MiB chain
    // reach on Balanced.
    let len = if level == Level::Balanced {
        9
    } else {
        5
    } * 512
        * 1024;
    let seed = u64::from(byte(4)) | (u64::from(byte(5)) << 8) | 0x9e37_79b9_7f4a_7c15;
    let payload = ramp_payload(&mut Rng(seed), len);
    let shape = zstdx::InputShape::default().with_window_log(21);

    set_mt_ramp_depth_for_tests(depth);
    let armed = zstdx::bulk::compress_with(
        &payload,
        &zstdx::EncoderOptions::new(level)
            .checksum(byte(3) & 1 == 0)
            .with_input_shape(shape)
            .pledged_size(Some(len as u64))
            .workers(4),
    )
    .unwrap();
    set_mt_ramp_depth_for_tests(u64::MAX);

    let mut back = Vec::with_capacity(len + 16);
    FrameDecoder::new()
        .decode_all_to_vec(&armed, &mut back)
        .unwrap();
    assert_eq!(
        &back[..],
        &payload[..],
        "zstdx decode of the ramp frame differs"
    );

    let mut zback = Vec::new();
    zstd::stream::copy_decode(armed.as_slice(), &mut zback).unwrap();
    assert_eq!(
        &zback[..],
        &payload[..],
        "zstd decode of the ramp frame differs"
    );

    // Plain controls at workers 0 and 1: the bulk job path declares the
    // content size either way, so the pledge bit forks these frames
    // only (the header field present or absent).
    let ctrl = &payload[..32 * 1024];
    for (workers, pledge) in [(0u32, pledged), (1, !pledged)] {
        let opts = zstdx::EncoderOptions::new(level)
            .checksum(byte(3) & 1 == 0)
            .with_input_shape(shape)
            .pledged_size(pledge.then_some(ctrl.len() as u64))
            .workers(workers);
        let comp = zstdx::bulk::compress_with(ctrl, &opts).unwrap();
        let mut back = Vec::with_capacity(ctrl.len() + 16);
        FrameDecoder::new()
            .decode_all_to_vec(&comp, &mut back)
            .unwrap();
        assert_eq!(&back[..], ctrl, "zstdx decode of the ramp control differs");

        let mut zback = Vec::new();
        zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
        assert_eq!(&zback[..], ctrl, "zstd decode of the ramp control differs");
    }
}
