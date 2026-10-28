#![no_main]
//! Decoder robustness and reference agreement: junk input must fail
//! cleanly, valid input must decode identically to the reference ON EACH
//! PATH — the streaming path (small reads, so block boundaries and
//! internal wrap-arounds are crossed many times) against the reference
//! streaming decoder, the flat `decode_all` path (whole-history one-shot
//! semantics, concatenated frames included) against the reference
//! one-shot `ZSTD_decompress`.
//!
//! The two zstdx paths are deliberately NOT compared with each other:
//! one-shot and streaming decode differ BY DESIGN (the streaming
//! generation model, R72) — a crafted wrapped-ring frame legitimately
//! produces different accept/bytes through the two paths, which is
//! exactly what the R73 campaign's first artifact was (the old
//! `streamed == flat` assert fired on it; the reference agreed with each
//! path separately). The checks are one-directional: zstdx rejecting
//! where the reference accepts is not an error (the deliberately
//! stricter generation model covers that direction); anything a zstdx
//! path accepts, its reference counterpart must accept with equal bytes
//! — except the streamed byte-compare, which only runs when the
//! reference's own streaming and one-shot outputs agree: on a
//! window-invalid aliased frame the accepted bytes are caller-layout
//! defined (libzstd's ring resolves the aliased offset against whatever
//! its own buffer geometry retains; the zstd crate's layout yielded
//! stale-ring bytes the CLI's does not — R73 artifact
//! crash-e9590bef, adjudicated not a zstdx defect), so no
//! cross-implementation byte oracle exists there. Accept/reject
//! agreement is still checked unconditionally.
//!
//! Decoded size is bounded: a 300 KB crafted input of RLE/match blocks
//! can declare gigabytes of output, and neither path's checks matter past
//! the point the box runs out of RAM — oversized frames are dropped from
//! the check instead.

use std::io::Read;

use libfuzzer_sys::fuzz_target;
use zstdx::decoding::{FrameDecoder, StreamingDecoder, errors::FrameDecoderError};

/// Output past this bound skips every check (see the module doc).
const MAX_CHECK: usize = 32 << 20;

fn oracle(data: &[u8]) {
    // Streaming pass; a mid-stream error leaves `streamed` unset (the
    // flat path still gets its own reference check — a frame streaming
    // rejects but flat accepts is the by-design generation-model class).
    let mut streamed: Option<Vec<u8>> = None;
    if let Ok(mut dec) = StreamingDecoder::new(data) {
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

    // decode_all_to_vec never grows its target; retry with geometric growth
    // (bounded: RLE blocks cap the expansion of small inputs well below
    // this).
    let mut flat = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut fr = FrameDecoder::new();
        match fr.decode_all_to_vec(data, &mut flat) {
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

    // Flat vs the reference one-shot (ZSTD_decompress decodes every
    // concatenated frame whole-history, like decode_all). A reference
    // output larger than the buffer is a length divergence and fails the
    // expect; anything at most flat's size must match byte for byte.
    // Runs before the streamed compare: agreement with the reference
    // streaming output is the layout-independence adjudicator below.
    let mut rbuf = vec![0u8; flat.capacity().max(1 << 16)];
    let n = zstd::bulk::Decompressor::new()
        .expect("reference decoder init failed")
        .decompress_to_buffer(data, &mut rbuf)
        .expect("reference oneshot rejects a frame zstdx flat accepted");
    assert_eq!(
        &rbuf[..n],
        &flat[..],
        "reference and zstdx flat bytes differ"
    );

    if let Some(streamed) = &streamed {
        // Streaming vs the reference streaming decoder (multi-frame
        // inputs included: both sides decode concatenated frames).
        let mut zstream = Vec::new();
        zstd::stream::copy_decode(std::io::Cursor::new(data), &mut zstream)
            .expect("reference stream rejects a frame zstdx streaming accepted");
        // Byte-equality only on layout-independent frames (module doc).
        if zstream.as_slice() == &rbuf[..n] {
            assert_eq!(
                zstream, *streamed,
                "reference and zstdx streamed bytes differ"
            );
        }
    }
}

/// `n` extra bits carrying `v`, preceded by a `1` marker bit, LSB-first —
/// the tail of a one-sequence offset bitstream. Keep the builders in
/// lockstep with `tests/decode_edges.rs` when editing either.
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

/// One compressed block with 10 raw literals and a single RLE-coded
/// sequence (LL 9, ML code 2 => 5, OF code `offset.ilog2()`): output 15
/// bytes, decoded offset value `offset` (`offset == 1` selects the repcode
/// domain, OF code 0 = reuse `rep0`).
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

fn edge_block(out: &mut Vec<u8>, last: bool, btype: u32, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

fn edge_rle(out: &mut Vec<u8>, last: bool, len: u32, byte: u8) {
    let hdr = (len << 3) | (1 << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.push(byte);
}

fn edge_header(out: &mut Vec<u8>, wlog: u32, fcs: Option<u64>, checksum: bool) {
    out.extend_from_slice(&0xfd2fb528u32.to_le_bytes());
    let mut fhd = 0u8;
    if fcs.is_some() {
        fhd |= 3 << 6;
    }
    if checksum {
        fhd |= 1 << 2;
    }
    out.push(fhd);
    out.push(((wlog - 10) as u8) << 3);
    if let Some(f) = fcs {
        out.extend_from_slice(&f.to_le_bytes());
    }
}

/// Deterministic edge-frame seeds (R66 constructor pattern): the crafted
/// decode-edge classes pinned in `tests/decode_edges.rs`, parameterized by
/// the fuzz bytes and replayed through the same oracle as the raw input.
/// One class per input keeps the per-input cost flat. Checksummed variants
/// carry a zero trailer: the verification-mismatch path with reference
/// agreement on the reject (the ok-checksum class is the corpus's stock
/// shape and the unit tests' pin).
fn edge_frames_seed(data: &[u8]) {
    let byte = |i: usize| data.get(i).copied().unwrap_or(0x5a);
    match byte(0) % 6 {
        // A raw and an RLE block at exactly Block_Maximum_Size.
        0 => {
            let mut frame = Vec::new();
            edge_header(&mut frame, 17, None, false);
            edge_block(&mut frame, true, 0, &vec![byte(3); 131072]);
            oracle(&frame);
            let mut frame = Vec::new();
            edge_header(&mut frame, 17, None, false);
            edge_rle(&mut frame, true, 131072, byte(4));
            oracle(&frame);
        },
        // Repcode chains: rep0 set near the streaming reach boundary, reused
        // in-generation (legal everywhere) and across two wraps (streaming
        // rejects, flat whole-history accepts — the by-design class).
        1 => {
            let mut frame = Vec::new();
            edge_header(&mut frame, 10, None, false);
            edge_rle(&mut frame, false, 1000, 0x5a);
            edge_rle(&mut frame, false, 1000, 0x5a);
            let a = seq_block(2003 + u32::from(byte(5)) % 7);
            edge_block(&mut frame, false, 2, &a);
            let b = seq_block(1);
            edge_block(&mut frame, true, 2, &b);
            oracle(&frame);

            let mut frame = Vec::new();
            edge_header(&mut frame, 10, None, false);
            for _ in 0..3 {
                edge_rle(&mut frame, false, 1000, 0x5a);
            }
            let a = seq_block(3012);
            edge_block(&mut frame, false, 2, &a);
            for _ in 0..3 {
                edge_rle(&mut frame, false, 1000, 0x5a);
            }
            edge_rle(&mut frame, false, 1024, 0x33);
            edge_rle(&mut frame, false, 1024, 0x33);
            edge_rle(&mut frame, false, 65, 0x33);
            let c = seq_block(1);
            edge_block(&mut frame, true, 2, &c);
            oracle(&frame);
        },
        // Tiny raw frames, 1..8 bytes, declared FCS included.
        2 => {
            for n in 1..=8usize {
                let payload: Vec<u8> = (0..n)
                    .map(|i| data.get(i % data.len().max(1)).copied().unwrap_or(0x31))
                    .collect();
                let mut frame = Vec::new();
                edge_header(&mut frame, 10, Some(n as u64), false);
                edge_block(&mut frame, true, 0, &payload);
                oracle(&frame);
            }
        },
        // Skippable-only streams (empty skippable included).
        3 => {
            let skippable = |payload: &[u8], out: &mut Vec<u8>| {
                out.extend_from_slice(&0x184d2a50u32.to_le_bytes());
                out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                out.extend_from_slice(payload);
            };
            let mut input = Vec::new();
            skippable(&data[..data.len().min(16)], &mut input);
            skippable(&[], &mut input);
            oracle(&input);
        },
        // Truncations at every boundary class of a checksum-flagged frame
        // (zero trailer: the mismatch reject path).
        4 => {
            let mut frame = Vec::new();
            edge_header(&mut frame, 10, None, true);
            edge_rle(&mut frame, false, 1000, 0x5a);
            let seq = seq_block(1003);
            edge_block(&mut frame, true, 2, &seq);
            frame.extend_from_slice(&[0; 4]);
            let len = frame.len();
            let cuts = [
                1 + byte(5) as usize % 4,
                4 + byte(6) as usize % 2,
                6,
                7 + byte(7) as usize % 8,
                len - 8 + byte(8) as usize % 8,
                len - 4 + byte(9) as usize % 4,
            ];
            for cut in cuts {
                oracle(&frame[..cut.min(len).max(1)]);
            }
        },
        // A zero trailer on an otherwise legal checksummed frame, plus a
        // single flipped trailer byte.
        _ => {
            let mut frame = Vec::new();
            edge_header(&mut frame, 10, None, true);
            edge_rle(&mut frame, false, 1000, 0x5a);
            edge_rle(&mut frame, true, 1000, 0x5a);
            frame.extend_from_slice(&[0; 4]);
            oracle(&frame);
            let last = frame.len() - 1 - byte(5) as usize % 4;
            frame[last] ^= byte(6).max(1);
            oracle(&frame);
        },
    }
}

fuzz_target!(|data: &[u8]| {
    edge_frames_seed(data);
    oracle(data);
});
