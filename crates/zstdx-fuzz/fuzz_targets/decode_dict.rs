#![no_main]
//! Dictionary decode paths (R73). Two surfaces:
//!
//! - robustness: arbitrary bytes parsed as a dictionary must fail cleanly, never panic (the
//!   original target surface, kept on the whole input and on the dictionary slice of the pair
//!   below);
//! - pair seeds: `[dlen_lo, dlen_hi, dict..., frame...]` with `dlen` a LE u16 clamped to the
//!   available bytes — the frame decodes with the dictionary attached the way the options-based
//!   entries attach it (own id and the null id, so id-suppressed frames decode too) through the
//!   streaming path (the generation model: dictionary content is a virtual generation-0 prefix that
//!   expires at the first ring wrap) and the flat whole-history path, each against its own
//!   reference counterpart with the dictionary — streaming vs the reference streaming decoder, flat
//!   vs the reference one-shot. The two zstdx paths are NOT compared with each other: they differ
//!   by design once the ring wraps (R72); see the `decode` target. Checks are one-directional
//!   (zstdx rejecting where the reference accepts is the stricter generation model), and the
//!   streamed byte-compare only runs when the reference's own streaming and one-shot outputs agree
//!   (layout-independent frames; on window-invalid aliased frames the accepted bytes are
//!   caller-layout defined — see the `decode` target's module doc). Oversized outputs skip the
//!   checks (decode bombs; see the `decode` target).
#[macro_use]
extern crate libfuzzer_sys;
extern crate zstdx;

use std::{io::Read, sync::Arc};

use zstdx::decoding::{Dictionary, FrameDecoder, StreamingDecoder, errors::FrameDecoderError};

/// Output past this bound skips every check.
const MAX_CHECK: usize = 32 << 20;

/// The pair oracle over an explicit (dict, frame) split.
fn oracle(dict_raw: &[u8], frame: &[u8]) {
    if frame.is_empty() {
        return;
    }
    let Ok(dict) = Dictionary::load(dict_raw) else {
        return;
    };
    let dict = Arc::new(dict);

    // Streaming pass; a mid-stream error leaves `streamed` unset (the
    // flat path still gets its own reference check — generation-model
    // reject/accept flips are by design).
    let mut streamed: Option<Vec<u8>> = None;
    if let Ok(mut dec) = StreamingDecoder::new_with_decoder(frame, {
        let mut d = FrameDecoder::new();
        d.add_options_dict(Arc::clone(&dict)).unwrap();
        d
    }) {
        let mut out = Vec::new();
        let mut sink = [0u8; 1024];
        loop {
            match dec.read(&mut sink) {
                Ok(0) => {
                    streamed = Some(out);
                    break;
                },
                Ok(n) => {
                    out.extend_from_slice(&sink[..n]);
                    if out.len() > MAX_CHECK {
                        return;
                    }
                },
                Err(_) => break,
            }
        }
    }

    let mut flat = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut fr = FrameDecoder::new();
        fr.add_options_dict(Arc::clone(&dict)).unwrap();
        match fr.decode_all_to_vec(frame, &mut flat) {
            Ok(()) => break,
            Err(FrameDecoderError::TargetTooSmall) => {
                assert!(step <= 1 << 30, "frame expands beyond 1 GiB");
                flat.reserve(step);
                step *= 2;
            },
            Err(_) => return,
        }
    }
    if flat.len() > MAX_CHECK {
        return;
    }

    // Flat vs the reference one-shot with the dictionary
    // (ZSTD_decompress_usingDDict semantics, whole history). A reference
    // output larger than the buffer is a length divergence and fails the
    // expect. Runs before the streamed compare: agreement with the
    // reference streaming output is the layout-independence adjudicator.
    let mut rbuf = vec![0u8; flat.capacity().max(1 << 16)];
    let n = zstd::bulk::Decompressor::with_dictionary(dict_raw)
        .expect("reference decoder init failed on a parsed dictionary")
        .decompress_to_buffer(frame, &mut rbuf)
        .expect("reference oneshot rejects a dict frame zstdx flat accepted");
    assert_eq!(
        &rbuf[..n],
        &flat[..],
        "reference and zstdx flat dict bytes differ"
    );

    if let Some(streamed) = &streamed {
        let mut zstream = Vec::new();
        let mut zdec =
            zstd::stream::Decoder::with_dictionary(std::io::Cursor::new(frame), dict_raw)
                .expect("reference decoder init failed on a parsed dictionary");
        zdec.read_to_end(&mut zstream)
            .expect("reference rejects a dict frame zstdx streaming accepted");
        // Byte-equality only on layout-independent frames (module doc).
        if zstream.as_slice() == &rbuf[..n] {
            assert_eq!(
                zstream, *streamed,
                "reference and zstdx streamed dict bytes differ"
            );
        }
    }
}

/// `n` extra bits carrying `v`, preceded by a `1` marker bit, LSB-first.
/// Keep the builders in lockstep with `tests/decode_edges.rs`.
fn bitstream_of(n: u32, v: u32) -> Vec<u8> {
    let region: u64 = (1u64 << n) | u64::from(v);
    let nbytes = (n + 1).div_ceil(8);
    let mut out = Vec::new();
    for i in 0..nbytes {
        out.push(((region >> (8 * (nbytes - 1 - i))) & 0xff) as u8);
    }
    out.reverse();
    out
}

/// One compressed block: 10 raw literals, one RLE-coded sequence (LL 9,
/// ML code 2 => 5, OF code `offset.ilog2()`), output 15 bytes.
fn seq_block(offset: u32) -> Vec<u8> {
    let of_code = offset.ilog2();
    let extra = offset - (1 << of_code);
    let mut blk = Vec::new();
    blk.push(10u8 << 3);
    blk.extend_from_slice(b"ababababab");
    blk.push(0x01);
    blk.push(0b01_01_01_00);
    blk.extend_from_slice(&[9u8, of_code as u8, 2u8]);
    blk.extend_from_slice(&bitstream_of(of_code, extra));
    blk
}

fn edge_rle(out: &mut Vec<u8>, last: bool, len: u32, byte: u8) {
    let hdr = (len << 3) | (1 << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.push(byte);
}

fn edge_block(out: &mut Vec<u8>, last: bool, btype: u32, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

fn edge_header(out: &mut Vec<u8>, wlog: u32) {
    out.extend_from_slice(&0xfd2fb528u32.to_le_bytes());
    out.push(0x00);
    out.push(((wlog - 10) as u8) << 3);
}

/// Deterministic dictionary-edge seeds (R66 constructor pattern): a raw
/// dictionary whose content is reachable from the frame's matches — the
/// dict-tail reach class, the past-the-dict corruption class, and the
/// dict-expiry-at-first-wrap class pinned in
/// `tests/decode_window_semantics.rs::window_semantics_dict`, replayed
/// through the same pair oracle.
fn dict_edge_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x77);
    // Raw content dictionary (magic-free bytes parse as raw content); the
    // first bytes keep clear of the format magic so the parse stays raw.
    let mut dict: Vec<u8> = (0..4096u32).map(|i| byte((i % 64) as usize)).collect();
    dict[..4].fill(0x77);
    let mut frame = Vec::new();
    edge_header(&mut frame, 10);
    match byte(0) % 3 {
        // 2000 produced: a 4000-byte offset reaches 2000 bytes into the
        // dict tail — accepted everywhere with identical bytes.
        0 => {
            edge_rle(&mut frame, false, 1000, 0x5a);
            edge_rle(&mut frame, false, 1000, 0x5a);
            let a = seq_block(4003);
            edge_block(&mut frame, true, 2, &a);
        },
        // Past dict plus output: corruption everywhere.
        1 => {
            edge_rle(&mut frame, false, 1000, 0x5a);
            edge_rle(&mut frame, false, 1000, 0x5a);
            let a = seq_block(7003);
            edge_block(&mut frame, true, 2, &a);
        },
        // 4000 produced wraps the ring (cap 3136): the dict expired, so
        // streaming rejects while flat whole-history accepts.
        _ => {
            for _ in 0..4 {
                edge_rle(&mut frame, false, 1000, 0x5a);
            }
            let a = seq_block(4503);
            edge_block(&mut frame, true, 2, &a);
        },
    }
    oracle(&dict, &frame);
}

fuzz_target!(|data: &[u8]| {
    Dictionary::decode_dict(data).ok(); // may return error on invalid data, may not panic.

    dict_edge_seed(data);

    let (head, rest) = match data {
        [a, b, rest @ ..] => (u16::from_le_bytes([*a, *b]) as usize, rest),
        _ => return,
    };
    let dlen = head.min(rest.len()).min(64 * 1024);
    let (dict, frame) = rest.split_at(dlen);
    Dictionary::decode_dict(dict).ok();
    oracle(dict, frame);
});
