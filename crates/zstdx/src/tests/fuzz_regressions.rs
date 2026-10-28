#![cfg(test)]
//! Artifact walker + R73: every pass also replays the R72/R71 probe
//! shape classes (the todo-26 gate note) — the crafted beyond-window
//! frames and forced-window streams in the tests at the bottom. Craft
//! code is shared with `decode_window_semantics` where the classes
//! overlap. R74 adds the structure-mode program replay: the walker
//! mirrors the fuzz targets' program expansion byte for byte.

use alloc::{format, vec, vec::Vec};

#[test]
fn test_all_artifacts() {
    extern crate std;
    use std::{borrow::ToOwned, fs::File};

    use crate::decoding::{BlockDecodingStrategy, FrameDecoder};

    let mut frame_dec = FrameDecoder::new();

    let Some(entries) = super::fixture_entries("../zstdx-fuzz/artifacts/decode") else {
        return;
    };
    for file in entries {
        let file_name = file.unwrap().path();

        let fnstr = file_name.to_str().unwrap().to_owned();
        if !fnstr.contains("/crash-") {
            continue;
        }

        let mut f = File::open(file_name.clone()).unwrap();

        // ignore errors. It just should never panic on invalid input
        let _: Result<_, _> = frame_dec
            .reset(&mut f)
            .and_then(|()| frame_dec.decode_blocks(&mut f, BlockDecodingStrategy::All));
    }
}

/// Encoder-side crash artifacts: the input layout matches the `encode`
/// fuzz target — `[level, opts, k, rest]` with chunk `i` at zstd level
/// `(level + i) % 23` walking one thread's pooled slice state through
/// consecutive levels; the parser is mirrored byte for byte in
/// `crates/zstdx-fuzz/fuzz_targets/encode.rs` and `encode_stream.rs`
/// (separate crates, so it is duplicated). Every chunk must roundtrip
/// through our decoder and the reference implementation. The artifacts
/// are walked twice — forward, then in REVERSE — so one artifact's
/// ladder leaves pool residue that the next artifact's mixed levels must
/// survive. `k` bit 7 set replays the artifact in structure mode
/// (`walker_structured`): `rest` expands per the program grammar below.
#[test]
fn test_all_encode_artifacts() {
    extern crate std;
    use std::borrow::ToOwned;

    use crate::decoding::FrameDecoder;

    let Some(entries) = super::fixture_entries("../zstdx-fuzz/artifacts/encode") else {
        return;
    };
    let mut artifacts = Vec::new();
    for file in entries {
        let file_name = file.unwrap().path();

        let fnstr = file_name.to_str().unwrap().to_owned();
        if !fnstr.contains("/crash-") {
            continue;
        }

        artifacts.push(std::fs::read(file_name).unwrap());
    }

    for (pass, data) in artifacts.iter().chain(artifacts.iter().rev()).enumerate() {
        let (level_byte, opt_byte, k, rest) = match &data[..] {
            [l, o, k, rest @ ..] => (*l, *o, *k, rest),
            [l, o] => (*l, *o, 0, &[][..]),
            [l] => (*l, 0, 0, &[][..]),
            _ => (0, 0, 0, &[][..]),
        };

        if k & 0x80 != 0 {
            walker_structured(pass, level_byte, opt_byte, rest);
            continue;
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

            let level = crate::Level::from_zstd((i32::from(level_byte) + i32::from(i)) % 23);
            let mut opts = crate::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
            if opt_byte & 2 != 0 {
                opts = opts.pledged_size(Some(chunk.len() as u64));
            }
            let window = (opt_byte >> 2) & 0x0f;
            if window != 0 {
                opts = opts.with_input_shape(
                    crate::InputShape::default().with_window_log(u32::from(window) + 9),
                );
            }
            let ldm = match (opt_byte >> 6) & 3 {
                1 => crate::LdmMode::Enable,
                2 => crate::LdmMode::Disable,
                _ => crate::LdmMode::Auto,
            };
            opts = opts.long_distance_matching(ldm);

            let comp = crate::bulk::compress_with(chunk, &opts).unwrap();
            let mut back = Vec::with_capacity(chunk.len() + 16);
            FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            assert_eq!(
                &back[..],
                chunk,
                "pass {pass}: zstdx decode of own output differs"
            );

            let mut zback = Vec::new();
            zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
            assert_eq!(
                &zback[..],
                chunk,
                "pass {pass}: zstd decode of zstdx output differs"
            );
        }
    }
}

/// R72/R71 probe classes on the DECODE side, replayed on every walker
/// pass (the todo-26 gate note): the crafted beyond-window frames whose
/// acceptance flips between the streaming generation model and the
/// one-shot whole-history rule, plus the dict-prefill expiry classes.
/// The pinned per-path outcomes live in `decode_window_semantics`; this
/// pass instead checks LIVE agreement with the reference — any frame a
/// zstdx path accepts, the reference path must accept with equal bytes
/// (the reverse direction is not asserted: zstdx rejecting where the
/// reference accepts is the deliberately stricter generation model / the
/// pinned single-pass-shortcut deviation).
#[test]
fn test_window_probe_frames() {
    extern crate std;

    use std::io::Read as StdRead;

    use super::decode_window_semantics::{dict4096, frame};
    use crate::{
        decoding::{FrameDecoder, StreamingDecoder, dictionary::Dictionary},
        io::Read,
    };

    fn stream(frame: &[u8]) -> Option<Vec<u8>> {
        let mut dec = StreamingDecoder::new(frame).ok()?;
        let mut out = Vec::new();
        let mut sink = [0u8; 4096];
        loop {
            match Read::read(&mut dec, &mut sink) {
                Ok(0) => return Some(out),
                Ok(n) => out.extend_from_slice(&sink[..n]),
                Err(_) => return None,
            }
        }
    }

    fn stream_dict(frame: &[u8], dict: &[u8]) -> Option<Vec<u8>> {
        let mut decoder = FrameDecoder::new();
        decoder.add_dict(Dictionary::load(dict).unwrap()).unwrap();
        let mut dec = StreamingDecoder::new_with_decoder(frame, decoder).ok()?;
        let mut out = Vec::new();
        let mut sink = [0u8; 4096];
        loop {
            match Read::read(&mut dec, &mut sink) {
                Ok(0) => return Some(out),
                Ok(n) => out.extend_from_slice(&sink[..n]),
                Err(_) => return None,
            }
        }
    }

    fn oneshot(frame: &[u8]) -> Option<Vec<u8>> {
        let mut dec = FrameDecoder::new();
        let mut out = vec![0u8; 1 << 20];
        dec.decode_all(frame, &mut out)
            .ok()
            .map(|n| out[..n].to_vec())
    }

    fn oneshot_dict(frame: &[u8], dict: &[u8]) -> Option<Vec<u8>> {
        let mut dec = FrameDecoder::new();
        dec.add_dict(Dictionary::load(dict).unwrap()).unwrap();
        let mut out = vec![0u8; 1 << 20];
        dec.decode_all(frame, &mut out)
            .ok()
            .map(|n| out[..n].to_vec())
    }

    let plain: Vec<(&str, Vec<u8>)> = vec![
        ("ctrl_w10_o900", frame(10, 180, 1000, 900, None)),
        ("fa_w10_o100000", frame(10, 180, 1000, 100000, None)),
        ("sw_w10_o3100", frame(10, 180, 1000, 3100, None)),
        ("ctrl_w16_o50000", frame(16, 3, 60000, 50000, None)),
        ("fa_w16_o100000", frame(16, 3, 60000, 100000, None)),
        ("sw6_w16_o200000", frame(16, 6, 60000, 200000, None)),
        ("sw6_w16_o360000", frame(16, 6, 60000, 360000, None)),
        (
            "pledged_w10_f400015_o7000",
            frame(10, 200, 2000, 7000, Some(400015)),
        ),
        (
            "pledged_w10_f3015_o2500",
            frame(10, 3, 1000, 2500, Some(3015)),
        ),
        (
            "pledged_w10_f60015_o50000",
            frame(10, 60, 1000, 50000, Some(600015)),
        ),
    ];
    for (name, f) in &plain {
        if let Some(out) = stream(f) {
            let mut reference = Vec::new();
            zstd::stream::copy_decode(&f[..], &mut reference)
                .unwrap_or_else(|e| panic!("{name}: reference stream rejects: {e}"));
            assert_eq!(reference, out, "{name}: reference stream bytes differ");
        }
        if let Some(out) = oneshot(f) {
            let mut reference = vec![0u8; 1 << 20];
            let n = zstd::bulk::Decompressor::new()
                .unwrap()
                .decompress_to_buffer(f, &mut reference)
                .unwrap_or_else(|e| panic!("{name}: reference oneshot rejects: {e}"));
            assert_eq!(
                &reference[..n],
                &out[..],
                "{name}: reference oneshot bytes differ"
            );
        }
    }

    let dict = dict4096();
    let pairs: Vec<(&str, Vec<u8>)> = vec![
        ("dict_pre2000_o4000", frame(10, 2, 1000, 4000, None)),
        ("dict_pre2000_o7000", frame(10, 2, 1000, 7000, None)),
        ("dict_pre4000_o4500", frame(10, 4, 1000, 4500, None)),
    ];
    for (name, f) in &pairs {
        if let Some(out) = stream_dict(f, &dict) {
            let mut reference = Vec::new();
            let mut zdec = zstd::stream::Decoder::with_dictionary(&f[..], &dict).unwrap();
            StdRead::read_to_end(&mut zdec, &mut reference)
                .unwrap_or_else(|e| panic!("{name}: reference stream-dict rejects: {e}"));
            assert_eq!(reference, out, "{name}: reference stream-dict bytes differ");
        }
        if let Some(out) = oneshot_dict(f, &dict) {
            let mut reference = vec![0u8; 1 << 20];
            let n = zstd::bulk::Decompressor::with_dictionary(&dict)
                .unwrap()
                .decompress_to_buffer(f, &mut reference)
                .unwrap_or_else(|e| panic!("{name}: reference oneshot-dict rejects: {e}"));
            assert_eq!(
                &reference[..n],
                &out[..],
                "{name}: reference oneshot-dict bytes differ"
            );
        }
    }
}

/// The R73 campaign's first artifact class, pinned: a w10 frame whose
/// ring wraps after 2490 RLE bytes, ending in a sequence whose offset
/// (2497) stays inside the whole history but resolves through the
/// wrapped ring into the post-wrap prefix — the two zstdx paths decode
/// it to DIFFERENT bytes, each matching its own libzstd counterpart
/// exactly (streaming vs `ZSTD_decompressStream`, one-shot vs
/// `ZSTD_decompress`). The pre-R73 decode fuzz target asserted
/// `streamed == flat` and crashed here; the asymmetry is by design
/// (R72's generation model). Reference agreement on both paths plus the
/// divergence pin itself.
#[test]
fn test_wrapped_ring_alias_frame() {
    use crate::{
        decoding::{FrameDecoder, StreamingDecoder},
        io::Read,
    };

    // seq block: 10 raw literals, one sequence LL 9 / ML 2 (=5) / OF
    // code 11 with 452 extra bits -> offset (1<<11) - 3 + 452 = 2497.
    let of_code = 11u32;
    let extra = 452u32;
    let region: u64 = (1u64 << of_code) | u64::from(extra);
    let mut blk = vec![10u8 << 3];
    blk.extend_from_slice(b"ababababab");
    blk.push(0x01);
    blk.push(0b01_01_01_00);
    blk.extend_from_slice(&[9u8, of_code as u8, 2u8]);
    blk.push((region & 0xff) as u8);
    blk.push(((region >> 8) & 0xff) as u8);
    let mut f = vec![0x28u8, 0xb5, 0x2f, 0xfd, 0x01, 0x00, 0x00];
    for (len, byte) in [(2usize, 0x00u8), (488, 0x5a), (1000, 0x5a), (1000, 0x5a)] {
        let hdr = ((len as u32) << 3) | (1 << 1);
        f.extend_from_slice(&hdr.to_le_bytes()[..3]);
        f.push(byte);
    }
    let hdr = ((blk.len() as u32) << 3) | (2 << 1) | 1;
    f.extend_from_slice(&hdr.to_le_bytes()[..3]);
    f.extend_from_slice(&blk);

    let mut dec = StreamingDecoder::new(&f[..]).unwrap();
    let mut streamed = Vec::new();
    let mut sink = [0u8; 4096];
    loop {
        match dec.read(&mut sink) {
            Ok(0) => break,
            Ok(n) => streamed.extend_from_slice(&sink[..n]),
            Err(e) => panic!("zstdx streaming rejected the alias frame: {e}"),
        }
    }
    let mut fr = FrameDecoder::new();
    let mut flat = vec![0u8; 1 << 20];
    let n = fr.decode_all(&f, &mut flat).unwrap();
    flat.truncate(n);

    assert_eq!(streamed.len(), flat.len(), "both paths decode 2505 bytes");
    assert_ne!(
        streamed, flat,
        "the wrapped-ring alias class must stay asymmetric (generation model)"
    );

    let mut zstream = Vec::new();
    zstd::stream::copy_decode(&f[..], &mut zstream)
        .expect("reference streaming rejects the alias frame");
    assert_eq!(zstream, streamed, "reference streaming bytes differ");

    let mut rbuf = vec![0u8; 1 << 20];
    let rn = zstd::bulk::Decompressor::new()
        .unwrap()
        .decompress_to_buffer(&f, &mut rbuf)
        .expect("reference oneshot rejects the alias frame");
    assert_eq!(&rbuf[..rn], &flat[..], "reference oneshot bytes differ");
}

/// Deterministic xorshift64 source for the walker payloads.
struct WalkerRng(u64);

impl WalkerRng {
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

/// Structure-mode program expansion (R74), byte-identical to the fuzz
/// targets' `expand_program` (duplicated: separate crates). `rest` after
/// `k` bit 7 is `[ph, events...]`; `ph` bit 0 attaches a synthetic raw
/// dictionary sized `1 << (4 + bits 1..4)`, event tags are `op << 6`:
/// 0 LIT (verbatim run of `(tag & 0x3f) + 1` bytes), 1 REP (u24 dist+1,
/// u16 len+1, u8 flips — copy from the virtual history dict++out with up
/// to 15 flipped bytes), 2 NOISE (u24 count of stream bytes), 3 PAD (u24
/// count of one byte). Output caps at 256 KiB; truncated operands read
/// as zero.
fn walker_expand(prog: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    const EXPAND_CAP: usize = 256 * 1024;
    let mut rng = WalkerRng(
        prog.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        }) | 1,
    );
    let ph = prog.first().copied().unwrap_or(0);
    let dict = if ph & 1 != 0 {
        let log = 4 + usize::from((ph >> 1) & 0x0f);
        Some((0..1usize << log.min(19)).map(|_| rng.byte()).collect())
    } else {
        None
    };
    let dict_len = dict.as_ref().map_or(0, Vec::len);
    let field = |i: usize, n: usize| -> usize {
        let mut v = 0usize;
        for j in 0..n {
            v |= usize::from(prog.get(i + j).copied().unwrap_or(0)) << (8 * j);
        }
        v
    };
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
                let dist = field(i, 3) + 1;
                let len = (field(i + 3, 2) + 1).min(EXPAND_CAP - out.len());
                let flips = prog.get(i + 5).copied().unwrap_or(0) & 0x0f;
                i = (i + 6).min(prog.len());
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
                let n = (field(i, 3) + 1).min(EXPAND_CAP - out.len());
                i = (i + 3).min(prog.len());
                for _ in 0..n {
                    out.push(rng.byte());
                }
            },
            _ => {
                let n = (field(i, 3) + 1).min(EXPAND_CAP - out.len());
                let v = prog.get(i + 3).copied().unwrap_or(0);
                i = (i + 4).min(prog.len());
                out.extend(vec![v; n]);
            },
        }
    }
    (out, dict)
}

/// Structure-mode artifact replay on the bulk core, mirroring the fuzz
/// targets' `structured_roundtrip`: expand, encode on the shared option
/// surface (dictionary attached when the program built one), decode back
/// through both decoders.
fn walker_structured(pass: usize, level_byte: u8, opt_byte: u8, prog: &[u8]) {
    use crate::decoding::FrameDecoder;

    let (payload, dict) = walker_expand(prog);
    let level = crate::Level::from_zstd(i32::from(level_byte) % 23);
    let mut opts = crate::EncoderOptions::new(level).checksum(opt_byte & 1 == 1);
    if opt_byte & 2 != 0 {
        opts = opts.pledged_size(Some(payload.len() as u64));
    }
    let window = (opt_byte >> 2) & 0x0f;
    if window != 0 {
        opts = opts
            .with_input_shape(crate::InputShape::default().with_window_log(u32::from(window) + 9));
    }
    let ldm = match (opt_byte >> 6) & 3 {
        1 => crate::LdmMode::Enable,
        2 => crate::LdmMode::Disable,
        _ => crate::LdmMode::Auto,
    };
    opts = opts.long_distance_matching(ldm);
    if let Some(d) = dict.as_deref() {
        opts = opts.dictionary(d);
    }

    let comp = crate::bulk::compress_with(&payload, &opts).unwrap();
    let back = match dict.as_deref() {
        Some(d) => {
            crate::bulk::decompress_with(&comp, 0, &crate::DecoderOptions::new().dictionary(d))
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
        "pass {pass}: zstdx decode of structured payload differs"
    );

    match dict.as_deref() {
        Some(d) => {
            let mut rbuf = vec![0u8; payload.len() + 64];
            let n = zstd::bulk::Decompressor::with_dictionary(d)
                .unwrap()
                .decompress_to_buffer(&comp, &mut rbuf)
                .unwrap();
            assert_eq!(n, payload.len(), "pass {pass}: reference length differs");
            assert_eq!(
                &rbuf[..n],
                &payload[..],
                "pass {pass}: reference bytes differ"
            );
        },
        None => {
            let mut zback = Vec::new();
            zstd::stream::copy_decode(comp.as_slice(), &mut zback).unwrap();
            assert_eq!(
                &zback[..],
                &payload[..],
                "pass {pass}: zstd decode of structured payload differs"
            );
        },
    }
}

/// R74 structure-mode pins, replayed on every walker pass: the grammar
/// itself (twin spans land exactly where the events say, flips perturb
/// at most their count, PAD tails are constant, the cap clamps, garbage
/// and truncated programs degrade without panic and expansion is pure),
/// the far-match proof (a payload whose only redundancy sits 200 KiB out
/// compresses far smaller under a window that covers it than under w10,
/// which forbids the offsets — the expansion really drives far matches
/// past a 128 KiB block boundary), and the dictionary echo through the
/// prefill match path.
#[test]
fn test_structured_programs() {
    fn push_u24(prog: &mut Vec<u8>, v: usize) {
        prog.push((v & 0xff) as u8);
        prog.push(((v >> 8) & 0xff) as u8);
        prog.push(((v >> 16) & 0xff) as u8);
    }
    fn noise(prog: &mut Vec<u8>, n: usize) {
        prog.push(0x80);
        push_u24(prog, n - 1);
    }
    fn rep(prog: &mut Vec<u8>, dist: usize, len: usize, flips: u8) {
        prog.push(0x40);
        push_u24(prog, dist - 1);
        prog.extend_from_slice(&(len as u16 - 1).to_le_bytes());
        prog.push(flips);
    }

    let period = 200 * 1024;
    let twin = 6 * 1024;
    let build = |flips: u8| -> Vec<u8> {
        let mut p = vec![0u8]; // ph: no dictionary
        p.push(63); // LIT 64
        p.extend((0..64u8).map(|b| b.wrapping_mul(7).wrapping_add(1)));
        noise(&mut p, period - 64);
        rep(&mut p, period, twin, flips);
        noise(&mut p, 128);
        rep(&mut p, period, twin, flips);
        p
    };

    // The twins land exactly where the events say: twin 1 copies
    // [0..twin) to `period`; twin 2, written at period+twin+128, copies
    // from twin+128 out.
    let plain = walker_expand(&build(0)).0;
    let twin2 = period + twin + 128;
    assert_eq!(plain.len(), twin2 + twin);
    assert_eq!(&plain[period..period + twin], &plain[..twin]);
    assert_eq!(
        &plain[twin2..twin2 + twin],
        &plain[twin + 128..2 * twin + 128]
    );

    // Flips bound the mutation: one expansion whose second REP copies
    // the first REP's span verbatim at distance `twin` with `flips`
    // set — at most `flips` bytes may differ (all compares stay inside
    // one expansion, because the program seed hashes every byte).
    let mut chain = vec![0u8];
    chain.push(63);
    chain.extend((0..64u8).map(|b| b.wrapping_mul(7).wrapping_add(1)));
    noise(&mut chain, period - 64);
    rep(&mut chain, period, twin, 0);
    rep(&mut chain, twin, twin, 3);
    let chained = walker_expand(&chain).0;
    let diffs = chained[period + twin..period + 2 * twin]
        .iter()
        .zip(&chained[period..period + twin])
        .filter(|(a, b)| a != b)
        .count();
    assert!(diffs <= 3, "{diffs} bytes differ, flips must bound them");

    // Far-match proof: under w10 the 200 KiB offsets are illegal, under
    // w19 they are matched — at least one twin's worth of savings.
    let size = |wlog: u32| -> usize {
        let opts = crate::EncoderOptions::new(crate::Level::from_zstd(19))
            .with_input_shape(crate::InputShape::default().with_window_log(wlog));
        crate::bulk::compress_with(&plain, &opts).unwrap().len()
    };
    let wide = size(19);
    let narrow = size(10);
    assert!(
        wide + 4096 < narrow,
        "w19 {wide} vs w10 {narrow}: far matches past the block boundary not taken"
    );

    // Dictionary echo: a REP whose distance reaches into the attached
    // dictionary copies its content into the payload; the frame declares
    // the dict, so both decoders need it attached.
    let dlog = 10; // 1 KiB
    let mut dprog = vec![1u8 | ((dlog - 4) as u8) << 1];
    dprog.push(15); // LIT 16
    dprog.extend((0..16u8).map(|b| b.wrapping_mul(11).wrapping_add(3)));
    rep(&mut dprog, (1 << dlog) + 16, 1 << dlog, 0);
    let (dpayload, ddict) = walker_expand(&dprog);
    let dd = ddict.as_ref().unwrap();
    assert_eq!(dd.len(), 1 << dlog);
    assert_eq!(dpayload.len(), 16 + (1 << dlog));
    assert_eq!(&dpayload[16..], &dd[..]);

    let opts = crate::EncoderOptions::new(crate::Level::from_zstd(9)).dictionary(dd);
    let comp = crate::bulk::compress_with(&dpayload, &opts).unwrap();
    let back = crate::bulk::decompress_with(&comp, 0, &crate::DecoderOptions::new().dictionary(dd))
        .unwrap();
    assert_eq!(
        back, dpayload,
        "dictionary echo frame differs through zstdx"
    );
    let mut rbuf = vec![0u8; dpayload.len() + 64];
    let n = zstd::bulk::Decompressor::with_dictionary(dd)
        .unwrap()
        .decompress_to_buffer(&comp, &mut rbuf)
        .unwrap();
    assert_eq!(
        &rbuf[..n],
        &dpayload[..],
        "dictionary echo differs through zstd"
    );

    // PAD tail: constant run to the end.
    let mut tailed = build(0);
    tailed.push(0xc0);
    push_u24(&mut tailed, 300 - 1);
    tailed.push(0xaa);
    let (tp, _) = walker_expand(&tailed);
    assert_eq!(tp.len(), twin2 + twin + 300);
    assert!(tp[twin2 + twin..].iter().all(|&b| b == 0xaa));

    // The cap clamps an over-long NOISE event; expansion is pure; empty
    // and garbage programs degrade without panic.
    let mut big = vec![0u8];
    noise(&mut big, 16 * 1024 * 1024);
    let (capped, _) = walker_expand(&big);
    assert_eq!(capped.len(), 256 * 1024);
    let (empty, edict) = walker_expand(&[]);
    assert!(empty.is_empty() && edict.is_none());
    let mut w = WalkerRng(0x1234_5678_9abc_def0);
    for len in [1usize, 2, 3, 7, 9, 79, 4096] {
        let prog: Vec<u8> = (0..len).map(|_| w.byte()).collect();
        let (a, da) = walker_expand(&prog);
        let (b, db) = walker_expand(&prog);
        assert_eq!(a, b, "expansion must be pure");
        assert_eq!(da, db);
        assert!(a.len() <= 256 * 1024);
    }
}

/// Beyond-window payload families for the walker's forced-window pass
/// (mirrored in the `encode`/`encode_stream` fuzz constructors): 0
/// fragment pool, 1 twin halves, 2 near-twin high period. Content is
/// self-generated from the seed.
fn walker_payload(seed: u64, window: usize, len: usize, family: usize) -> Vec<u8> {
    let mut rng = WalkerRng(seed);
    let mut out = Vec::with_capacity(len);
    match family {
        0 => {
            let nfrag = 24;
            let frags: Vec<Vec<u8>> = (0..nfrag)
                .map(|_| (0..24).map(|_| rng.byte()).collect())
                .collect();
            let gap = window / 2 + 1;
            while out.len() < len {
                out.extend_from_slice(&frags[rng.next() as usize % nfrag]);
                for _ in 0..gap {
                    out.push(rng.byte());
                }
            }
        },
        1 => {
            let frags: Vec<Vec<u8>> = (0..24)
                .map(|k| (0..8 + k % 32).map(|_| rng.byte()).collect())
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
            let period = window * 2;
            let unit: Vec<u8> = (0..period).map(|_| rng.byte()).collect();
            while out.len() < len {
                let start = out.len();
                out.extend_from_slice(&unit[..(len - out.len()).min(period)]);
                for _ in 0..=(rng.next() as usize % 4) {
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

/// R71 shape classes on the ENCODE side, replayed on every walker pass
/// (the todo-26 gate note): forced windows over payloads whose only twins
/// sit past the declared window, through the write core with no pledge
/// and no length hint — the unknown-length param-resolution path. One
/// serial in-process pass, so the pooled thread state carries table
/// geometry from cell to cell. Every frame must decode through the
/// reference (wire legality: no offset past the declared window) and our
/// decoder. Compact sibling of
/// `encode_edges::forced_window_unpledged_streams` (which pins the
/// 300 KiB shapes at w10/w16 through both cores); this pass adds the
/// near-twin family and the pooled walk.
#[test]
fn test_forced_window_streams() {
    use crate::io::Write;

    let cells = [
        (0usize, 10u32, 32 * 1024usize),
        (1, 10, 32 * 1024),
        (2, 10, 32 * 1024),
        (0, 16, 160 * 1024),
        (1, 16, 160 * 1024),
    ];
    let rows = [9i32, 13, 16, 19, 22];
    for (family, window_log, len) in cells {
        let payload = walker_payload(
            0x5eed_0000_0000_0000 + family as u64,
            1 << window_log,
            len,
            family,
        );
        for &row in &rows {
            let label = format!("family{family}/w{window_log}/l{row}");
            let opts = crate::EncoderOptions::new(crate::Level::from_zstd(row))
                .with_input_shape(crate::InputShape::default().with_window_log(window_log));
            let mut sink = alloc::vec![];
            {
                let mut enc = crate::stream::write::Encoder::with_options(&mut sink, opts).unwrap();
                for piece in payload.chunks(7_000) {
                    enc.write_all(piece).unwrap();
                }
                enc.finish().unwrap();
            }
            let reference = zstd::stream::decode_all(&sink[..])
                .unwrap_or_else(|e| panic!("{label}: reference decode failed: {e:?}"));
            assert_eq!(reference, payload, "{label}: reference bytes differ");
            assert_eq!(
                crate::stream::decode_all(&sink[..]).unwrap(),
                payload,
                "{label}: own bytes differ"
            );
        }
    }
}
