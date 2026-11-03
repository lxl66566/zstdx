//! Handcrafted decode-edge frames pinned as regressions: the classes the
//! window-semantics and multi-frame pages leave open — a block at exactly
//! Block_Maximum_Size, repcode reach across ring wraps (the offset-history
//! path at near-window distances, including its expiry two generations
//! later), checksum trailer corruption on the streaming path, tiny raw
//! frames, skippable-only streams, and a truncation walk over every header
//! and body boundary of a checksummed frame. Every crafted frame either
//! roundtrips or fails by design; accepted frames are cross-checked
//! against the reference decoder where the paths share semantics.

#![cfg(test)]

use alloc::{format, string::String, vec, vec::Vec};

use crate::decoding::{FrameDecoder, StreamingDecoder, errors::FrameDecoderError};

/// `n` extra bits carrying `v`, preceded by a `1` marker bit, LSB-first —
/// the tail of a one-sequence offset bitstream (same layout as
/// `decode_window_semantics::bitstream_of`).
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
/// sequence (LL 9, ML code 2 => 5 match bytes, OF code `offset.ilog2()`
/// with `offset - (1<<of_code)` extra bits): output 9 + 5 + 1 trailing
/// literal = 15 bytes. `offset <= 3` selects the repcode domain, so
/// `offset == 1` emits OF code 0 (reuse `rep0`).
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

fn block(out: &mut Vec<u8>, last: bool, btype: u32, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

fn rle(out: &mut Vec<u8>, last: bool, len: u32, byte: u8) {
    let hdr = (len << 3) | (1 << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.push(byte);
}

/// Frame header for a `wlog` window (10..=23), optional 8-byte FCS field
/// and optional content checksum.
fn header(out: &mut Vec<u8>, wlog: u32, fcs: Option<u64>, checksum: bool) {
    out.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
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

/// Without `hash` the decoder still reads the 4-byte trailer but stands
/// verification down, so the appended value is arbitrary.
fn with_checksum(mut frame: Vec<u8>, payload: &[u8]) -> Vec<u8> {
    #[cfg(feature = "hash")]
    {
        let mut hash = crate::xxh64::Xxh64::new(0);
        hash.write(payload);
        frame.extend_from_slice(&(hash.finish() as u32).to_le_bytes());
    }
    #[cfg(not(feature = "hash"))]
    {
        let _ = payload;
        frame.extend_from_slice(&[0u8; 4]);
    }
    frame
}

fn oneshot(frame: &[u8]) -> Result<Vec<u8>, FrameDecoderError> {
    // decode_all_to_vec never grows its target; retry with reserves.
    let mut out = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut dec = FrameDecoder::new();
        match dec.decode_all_to_vec(frame, &mut out) {
            Ok(()) => return Ok(out),
            Err(FrameDecoderError::TargetTooSmall) => {
                out.reserve(step);
                step *= 2;
            },
            Err(e) => return Err(e),
        }
    }
}

fn stream(frame: &[u8]) -> Result<Vec<u8>, String> {
    use crate::io::Read;
    let mut dec = StreamingDecoder::new(frame).map_err(|e| format!("{e}"))?;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = dec.read(&mut buf).map_err(|e| format!("{e}"))?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// Reference one-shot decode (`ZSTD_decompress`, whole-history semantics,
/// the same API class as our `decode_all`; the stream API is the
/// generation model and rejects window-expired offsets by design).
#[cfg(feature = "std")]
fn reference(frame: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 1 << 20];
    let n = zstd::bulk::Decompressor::new()
        .expect("reference decoder init failed")
        .decompress_to_buffer(frame, &mut buf)
        .expect("reference one-shot rejects a pinned-legal frame");
    buf[..n].to_vec()
}

/// A block at exactly Block_Maximum_Size (`min(window, 128 KiB)` with
/// window 128 KiB) decodes through every path — the wrap threshold math
/// (`end + blockMax > cap`) must treat the maximal block as still fitting
/// its own generation — and the reference agrees. RLE form included: its
/// output claim passes the same cap check.
#[test]
fn block_at_exactly_block_maximum_size() {
    let payload: Vec<u8> = (0..131072u32).map(|i| (i % 251) as u8).collect();

    let mut frame = Vec::new();
    header(&mut frame, 17, None, true);
    block(&mut frame, true, 0, &payload);
    let frame = with_checksum(frame, &payload);
    assert_eq!(oneshot(&frame).unwrap(), payload);
    assert_eq!(stream(&frame).unwrap(), payload);
    #[cfg(feature = "std")]
    assert_eq!(&reference(&frame)[..], &payload[..]);

    // RLE at the same cap: one payload byte claiming the full 128 KiB.
    let ones = vec![0xc3u8; 131072];
    let mut frame = Vec::new();
    header(&mut frame, 17, None, true);
    rle(&mut frame, true, 131072, 0xc3);
    let frame = with_checksum(frame, &ones);
    assert_eq!(oneshot(&frame).unwrap(), ones);
    assert_eq!(stream(&frame).unwrap(), ones);
    #[cfg(feature = "std")]
    assert_eq!(&reference(&frame)[..], &ones[..]);
}

/// A real-offset sequence sets `rep0` near the streaming reach boundary,
/// then a repcode block (OF code 0) reuses it: both paths accept while the
/// distance stays inside the current generation pair, and the reference
/// one-shot agrees. Pinned second half: two ring wraps later the same
/// `rep0` outlives the previous generation, so streaming rejects through
/// the repcode path while one-shot whole-history still accepts (the
/// documented by-design divergence, now pinned for the rep domain).
#[test]
fn repcode_reach_across_wraps() {
    // 2000 bytes produced, then a real offset of exactly 2000 at dst 2009
    // (the reach boundary), then the repcode block reusing it at dst 2024.
    let mut frame = Vec::new();
    header(&mut frame, 10, None, false);
    rle(&mut frame, false, 1000, 0x5a);
    rle(&mut frame, false, 1000, 0x5a);
    let a = seq_block(2003);
    block(&mut frame, false, 2, &a);
    let b = seq_block(1);
    block(&mut frame, true, 2, &b);
    assert_eq!(oneshot(&frame).unwrap().len(), 2030);
    assert_eq!(stream(&frame).unwrap().len(), 2030);
    #[cfg(feature = "std")]
    assert_eq!(reference(&frame).len(), 2030);

    // Wrap expiry: gen1 = 3 RLE blocks (wrap fires at 3000+1024 > 3136),
    // block A holds rep0 = 3009 (exactly its reach: 3000 produced + 9
    // literals), gen2 fills to 3015 (wrap), gen3 = 1024+1024+65 = 2113
    // bytes (wrap), then the repcode block: reach = 2113 + 9 = 2122 < 3009.
    let mut frame = Vec::new();
    header(&mut frame, 10, None, false);
    for _ in 0..3 {
        rle(&mut frame, false, 1000, 0x5a);
    }
    let a = seq_block(3012); // of_code 11, actual offset 3009 = its reach
    block(&mut frame, false, 2, &a);
    for _ in 0..3 {
        rle(&mut frame, false, 1000, 0x5a);
    }
    rle(&mut frame, false, 1024, 0x33);
    rle(&mut frame, false, 1024, 0x33);
    rle(&mut frame, false, 65, 0x33);
    let c = seq_block(1);
    block(&mut frame, true, 2, &c);

    // One-shot holds the whole history: rep0 3009 stays reachable at dst 8137.
    assert_eq!(oneshot(&frame).unwrap().len(), 8143);
    // Streaming generation model: rep0 expired with gen2, corruption.
    let err = stream(&frame).unwrap_err();
    assert!(err.contains("offset"), "{err}");
    #[cfg(feature = "std")]
    assert_eq!(reference(&frame).len(), 8143);
}

/// Checksum trailer edges on the streaming path (the MT paths are pinned
/// in `frame_checksum`): a flipped trailer byte fails verification, and a
/// trailer cut short is a read error, never a silent accept.
#[test]
fn checksum_trailer_edges_streaming() {
    let mut frame = Vec::new();
    header(&mut frame, 10, None, true);
    rle(&mut frame, false, 1000, 0x5a);
    rle(&mut frame, false, 1000, 0x5a);
    rle(&mut frame, false, 1000, 0x5a);
    rle(&mut frame, false, 1000, 0x5a);
    let last = 96u32;
    rle(&mut frame, true, last, 0x5a);
    let payload = [0x5au8; 4096];
    let frame = with_checksum(frame, &payload);
    assert_eq!(stream(&frame).unwrap(), payload);

    // Verification only exists under `hash`.
    #[cfg(feature = "hash")]
    for flip in [0usize, 1, 2, 3] {
        let mut bad = frame.clone();
        let at = bad.len() - 4 + flip;
        bad[at] ^= 0xff;
        match stream(&bad) {
            Err(e) => assert!(e.contains("checksum"), "{e}"),
            // A corruption the RLE body absorbs cannot happen (only the
            // trailer is touched), so an accept is a verification miss.
            Ok(out) => panic!("flipped trailer byte {flip} accepted {} bytes", out.len()),
        }
    }
    // Truncated inside the trailer: a clean read error.
    for keep in [1usize, 2, 3] {
        let cut = &frame[..frame.len() - 4 + keep];
        assert!(stream(cut).is_err(), "{keep} trailer bytes must not decode");
    }
}

/// Frames whose entire payload is 1..8 bytes (a raw block at the minimum
/// sizes): both paths roundtrip and the reference agrees — the tiny end of
/// every size field.
#[test]
fn tiny_raw_frames_one_to_eight() {
    for n in 1..=8usize {
        let payload: Vec<u8> = (0..n).map(|i| 0xf0 + i as u8).collect();
        let mut frame = Vec::new();
        header(&mut frame, 10, Some(n as u64), false);
        block(&mut frame, true, 0, &payload);
        assert_eq!(oneshot(&frame).unwrap(), payload, "n {n}");
        assert_eq!(stream(&frame).unwrap(), payload, "n {n}");
        #[cfg(feature = "std")]
        assert_eq!(&reference(&frame)[..], &payload[..], "n {n}");
    }
}

/// A stream of only skippable frames carries no data: zero output bytes
/// on both paths and in the reference, and an empty skippable frame
/// between them changes nothing.
#[test]
fn skippable_only_stream_decodes_to_nothing() {
    let skippable = |payload: &[u8]| {
        let mut out = Vec::new();
        out.extend_from_slice(&0x184d2a50u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    };
    let mut input = skippable(b"junkjunk");
    input.extend_from_slice(&skippable(&[]));
    input.extend_from_slice(&skippable(b"tail"));

    assert_eq!(oneshot(&input).unwrap(), Vec::<u8>::new());
    assert_eq!(stream(&input).unwrap(), Vec::<u8>::new());
    #[cfg(feature = "std")]
    assert_eq!(reference(&input), b"");
}

/// Every proper prefix of a checksummed frame fails cleanly on both
/// paths: truncations at the magic, the descriptor, the window byte, each
/// block header and body, and the trailer. A prefix that decodes to EOF
/// would mean a truncated frame passed as complete.
#[test]
fn truncated_prefixes_fail_cleanly() {
    let mut frame = Vec::new();
    header(&mut frame, 10, None, true);
    rle(&mut frame, false, 1000, 0x5a);
    let seq = seq_block(1003);
    block(&mut frame, true, 2, &seq);
    let frame = with_checksum(frame, &[0x5au8; 1000]);

    // The empty input itself is an error for both sides (reference CLI:
    // "unexpected end of file"), so the walk starts after it.
    for cut in 1..frame.len() {
        let prefix = &frame[..cut];
        assert!(
            oneshot(prefix).is_err(),
            "one-shot accepted a {cut}-byte prefix of {}",
            frame.len()
        );
        // Streaming: init may fail, or a read must error before EOF.
        match StreamingDecoder::new(prefix) {
            Err(_) => {},
            Ok(mut dec) => {
                use crate::io::Read;
                let mut buf = [0u8; 4096];
                loop {
                    match dec.read(&mut buf) {
                        Err(_) => break,
                        Ok(0) => panic!("streaming hit clean EOF at cut {cut}"),
                        Ok(_) => {},
                    }
                }
            },
        }
    }
}
