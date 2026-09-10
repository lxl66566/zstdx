//! Multi-frame stream handling: the streaming and flat decode paths must
//! agree, and both must match the reference `zstd` crate on every stream
//! shape (concatenated frames, skippable frames, trailing garbage).

#![cfg(test)]
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::decoding::errors::FrameDecoderError;
use crate::decoding::{FrameDecoder, StreamingDecoder};
use crate::io::Read;
use crate::Level;

/// Decode through the streaming path with deliberately awkward read sizes so
/// block boundaries and frame boundaries are crossed mid-read.
fn stream_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = StreamingDecoder::new(input).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut sink = [0u8; 7];
    loop {
        match dec.read(&mut sink) {
            Ok(0) => return Ok(out),
            Ok(n) => out.extend_from_slice(&sink[..n]),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Decode through the flat path, mirroring the fuzz target's geometric retry.
fn flat_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut step = 1usize << 16;
    loop {
        let mut fr = FrameDecoder::new();
        match fr.decode_all_to_vec(input, &mut out) {
            Ok(()) => return Ok(out),
            Err(FrameDecoderError::TargetTooSmall) => {
                out.reserve(step);
                step *= 2;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn expect_consistent(input: &[u8]) {
    let streamed = stream_decode(input).expect("streamed decode must succeed");
    let flat = flat_decode(input).expect("flat decode must succeed");
    assert_eq!(streamed, flat, "streamed and flat decodes disagree");
}

fn expect_consistent_error(input: &[u8]) {
    let streamed = stream_decode(input);
    let flat = flat_decode(input);
    assert!(
        streamed.is_err(),
        "streamed decode must fail: {:?}",
        streamed
    );
    assert!(flat.is_err(), "flat decode must fail: {:?}", flat);
}

#[cfg(feature = "std")]
fn expect_reference_ok(input: &[u8]) {
    let reference = zstd::decode_all(input).expect("zstd crate must decode");
    assert_eq!(flat_decode(input).unwrap(), reference);
    assert_eq!(stream_decode(input).unwrap(), reference);
}

#[cfg(feature = "std")]
fn expect_reference_error(input: &[u8]) {
    assert!(
        zstd::decode_all(input).is_err(),
        "zstd crate must reject {input:02x?}"
    );
    expect_consistent_error(input);
}

fn frame(data: &[u8]) -> Vec<u8> {
    crate::bulk::compress(data, Level::Fastest)
}

fn skippable(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x50, 0x2A, 0x4D, 0x18];
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

#[test]
fn two_frames_decode_completely() {
    let input = concat(&[&frame(b"aaaa"), &frame(b"bbbbb")]);
    expect_consistent(&input);
    assert_eq!(flat_decode(&input).unwrap(), b"aaaabbbbb");
    #[cfg(feature = "std")]
    expect_reference_ok(&input);
}

#[test]
fn fuzzer_found_two_frame_input() {
    // crash-b5593b58b45a98a3e4d44fb3c94ab6b545a3b7c6: two tiny frames (RLE
    // blocks emitting 4x0x38 and 5x0x04); the streaming path used to stop
    // after the first.
    let input = [
        0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00, 0x23, 0x00, 0x00, 0x38, //
        0x28, 0xb5, 0x2f, 0xfd, 0x38, 0x39, 0x2b, 0x00, 0x00, 0x04,
    ];
    expect_consistent(&input);
    let out = flat_decode(&input).unwrap();
    assert_eq!(out, [0x38, 0x38, 0x38, 0x38, 0x04, 0x04, 0x04, 0x04, 0x04]);
    // No reference cross-check: this frame sets reserved frame-descriptor
    // bits (FHD 0x38), which libzstd rejects ("unsupported frame parameter")
    // while zstdx currently tolerates; only the internal paths must agree.
}

#[test]
fn skippable_frames_are_transparent() {
    // between data frames
    let input = concat(&[&frame(b"aaaa"), &skippable(b"junkjunk"), &frame(b"bbbbb")]);
    expect_consistent(&input);
    assert_eq!(flat_decode(&input).unwrap(), b"aaaabbbbb");

    // before the first data frame
    let input = concat(&[&skippable(b"junkjunk"), &frame(b"aaaa")]);
    expect_consistent(&input);
    assert_eq!(flat_decode(&input).unwrap(), b"aaaa");

    // empty skippable frames
    let input = concat(&[&frame(b"aaaa"), &skippable(b""), &frame(b"bbbbb")]);
    expect_consistent(&input);
    assert_eq!(flat_decode(&input).unwrap(), b"aaaabbbbb");
    #[cfg(feature = "std")]
    {
        expect_reference_ok(&concat(&[
            &frame(b"aaaa"),
            &skippable(b"junkjunk"),
            &frame(b"bbbbb"),
        ]));
        expect_reference_ok(&concat(&[&skippable(b"junkjunk"), &frame(b"aaaa")]));
    }
}

#[test]
fn empty_frame_is_not_end_of_stream() {
    // A frame with no content must not read as EOF while more frames follow.
    let input = concat(&[&frame(b""), &frame(b"tail")]);
    expect_consistent(&input);
    assert_eq!(flat_decode(&input).unwrap(), b"tail");
    #[cfg(feature = "std")]
    expect_reference_ok(&input);
}

#[test]
fn trailing_bytes_are_an_error() {
    for extra in 1..8 {
        let input = concat(&[&frame(b"aaaa"), &[0xABu8; 8][..extra]]);
        expect_consistent_error(&input);
        #[cfg(feature = "std")]
        expect_reference_error(&input);
    }
}

#[test]
fn truncated_next_frame_is_an_error() {
    // second frame cut off mid-header
    let input = concat(&[&frame(b"aaaa"), &frame(b"bbbbb")[..6]]);
    expect_consistent_error(&input);

    // skippable frame with truncated payload
    let input = concat(&[&frame(b"aaaa"), &skippable(b"0123456789")[..12]]);
    expect_consistent_error(&input);
    #[cfg(feature = "std")]
    {
        expect_reference_error(&concat(&[&frame(b"aaaa"), &frame(b"bbbbb")[..6]]));
        expect_reference_error(&concat(&[&frame(b"aaaa"), &skippable(b"0123456789")[..12]]));
    }
}
