//! Multi-frame stream handling: the streaming and flat decode paths must
//! agree, and both must match the reference `zstd` crate on every stream
//! shape (concatenated frames, skippable frames, trailing garbage).

#![cfg(test)]
use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};

use crate::{
    Level,
    decoding::{FrameDecoder, StreamingDecoder, errors::FrameDecoderError},
    io::{Error, ErrorKind, Read},
};

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
            },
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
    assert!(streamed.is_err(), "streamed decode must fail: {streamed:?}");
    assert!(flat.is_err(), "flat decode must fail: {flat:?}");
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
    let mut out = vec![0x50, 0x2a, 0x4d, 0x18];
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
    // after the first. The second frame's declared Frame_Content_Size is 5,
    // matching its RLE block (the originally fuzzed 0x39 declared 57 and is
    // now the content_size_mismatch case below).
    let input = [
        0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00, 0x23, 0x00, 0x00, 0x38, //
        0x28, 0xb5, 0x2f, 0xfd, 0x38, 0x05, 0x2b, 0x00, 0x00, 0x04,
    ];
    expect_consistent(&input);
    let out = flat_decode(&input).unwrap();
    assert_eq!(out, [0x38, 0x38, 0x38, 0x38, 0x04, 0x04, 0x04, 0x04, 0x04]);
    // No reference cross-check: this frame sets reserved frame-descriptor
    // bits (FHD 0x38), which libzstd rejects ("unsupported frame parameter")
    // while zstdx currently tolerates; only the internal paths must agree.
}

/// A frame whose header declares a Frame_Content_Size its blocks do not
/// produce must be rejected by every decode path (libzstd reports the same
/// shape as a size error at the frame tail).
#[test]
fn content_size_mismatch_is_an_error() {
    // The original b5593b58 second frame: single-segment, 1-byte FCS field
    // declaring 57 bytes, one RLE block producing 5.
    let input = [
        0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00, 0x23, 0x00, 0x00, 0x38, //
        0x28, 0xb5, 0x2f, 0xfd, 0x38, 0x39, 0x2b, 0x00, 0x00, 0x04,
    ];
    expect_content_size_error(&input);
    #[cfg(feature = "std")]
    assert!(zstd::decode_all(&input[..]).is_err(), "reference rejects");

    // A real frame with its declared FCS patched one byte beyond the actual
    // content must fail the same way (the encoder itself cannot emit this;
    // the header field is the only thing corrupted). The one-shot paths
    // declare no FCS, so the well-formed frame comes from a pledged stream.
    let mut corrupt = pledged_frame(&vec![b'x'; 300]);
    let (declared, fcs_at) = locate_fcs(&corrupt);
    assert_eq!(declared, 300);
    corrupt[fcs_at] += 1;
    expect_content_size_error(&corrupt);
    #[cfg(feature = "std")]
    assert!(zstd::decode_all(&corrupt[..]).is_err(), "reference rejects");

    // The uncorrupted frame still decodes on every path.
    expect_consistent(&pledged_frame(&vec![b'x'; 300]));

    // An empty frame declaring FCS=0 stays valid (0 == 0), skippable frames
    // around it included.
    expect_consistent(&concat(&[&skippable(b"x"), &pledged_frame(&[])]));
}

/// A well-formed single frame declaring its content size.
fn pledged_frame(data: &[u8]) -> Vec<u8> {
    let mut sink = Vec::new();
    let mut enc = crate::stream::write::Encoder::with_options(
        &mut sink,
        crate::EncoderOptions::new(Level::Fastest).pledged_size(Some(data.len() as u64)),
    )
    .unwrap();
    crate::io::Write::write_all(&mut enc, data).unwrap();
    enc.finish().unwrap();
    sink
}

/// Walk a frame header to its Frame_Content_Size field, returning the
/// declared value and the field's byte offset.
pub(super) fn locate_fcs(frame: &[u8]) -> (u64, usize) {
    let fhd = frame[4];
    let single_segment = (fhd >> 5) & 1 == 1;
    let dict_len = match fhd & 3 {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => unreachable!("two-bit flag"),
    };
    let pos = 5 + usize::from(!single_segment) + dict_len;
    let declared = match fhd >> 6 {
        0 => {
            assert!(single_segment, "no FCS field to locate");
            u64::from(frame[pos])
        },
        1 => u64::from(u16::from_le_bytes([frame[pos], frame[pos + 1]])) + 256,
        2 => u64::from(u32::from_le_bytes([
            frame[pos],
            frame[pos + 1],
            frame[pos + 2],
            frame[pos + 3],
        ])),
        3 => u64::from_le_bytes([
            frame[pos],
            frame[pos + 1],
            frame[pos + 2],
            frame[pos + 3],
            frame[pos + 4],
            frame[pos + 5],
            frame[pos + 6],
            frame[pos + 7],
        ]),
        _ => unreachable!("two-bit flag"),
    };
    (declared, pos)
}

/// Both decode paths must fail, naming the content-size mismatch.
fn expect_content_size_error(input: &[u8]) {
    let streamed = stream_decode(input).unwrap_err();
    assert!(
        streamed.contains("Frame_Content_Size mismatch"),
        "{streamed}"
    );
    let flat = flat_decode(input).unwrap_err();
    assert!(flat.contains("Frame_Content_Size mismatch"), "{flat}");
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
        let input = concat(&[&frame(b"aaaa"), &[0xabu8; 8][..extra]]);
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

/// A source that fails once with `WouldBlock` as soon as its position
/// reaches `fail_after`, then serves normally. Non-blocking sockets behave
/// like this: a failed read leaves the already-served bytes consumed.
struct OneShotWouldBlock<'a> {
    data: &'a [u8],
    pos: usize,
    fail_after: usize,
    failed: bool,
}

impl<'a> OneShotWouldBlock<'a> {
    fn new(data: &'a [u8], fail_after: usize) -> Self {
        Self {
            data,
            pos: 0,
            fail_after,
            failed: false,
        }
    }
}

impl Read for OneShotWouldBlock<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if !self.failed && self.pos >= self.fail_after {
            self.failed = true;
            return Err(Error::from(ErrorKind::WouldBlock));
        }
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Decode with deliberately small reads; the single injected `WouldBlock`
/// surfaces once and the retry must resume the stream.
fn decode_with_transient_error(input: &[u8], fail_after: usize) -> Vec<u8> {
    let mut dec = StreamingDecoder::new(OneShotWouldBlock::new(input, fail_after))
        .expect("the first frame header lies before the injection");
    let mut out = Vec::new();
    let mut sink = [0u8; 7];
    let mut failures = 0;
    loop {
        match dec.read(&mut sink) {
            Ok(0) => return out,
            Ok(n) => out.extend_from_slice(&sink[..n]),
            Err(e) => {
                failures += 1;
                assert_eq!(failures, 1, "transient failure was not recovered: {e}");
            },
        }
    }
}

#[test]
fn transient_error_in_next_frame_hunt_resumes() {
    let payload_a = b"aaaa";
    let payload_b = b"bbbbb";
    let expected = b"aaaabbbbb";

    // control: no injection decodes the concatenated frames
    let input = concat(&[&frame(payload_a), &frame(payload_b)]);
    let mut plain = Vec::new();
    let mut sink = [0u8; 7];
    let mut dec = StreamingDecoder::new(&input[..]).unwrap();
    while let Ok(n) = dec.read(&mut sink) {
        if n == 0 {
            break;
        }
        plain.extend_from_slice(&sink[..n]);
    }
    assert_eq!(plain, expected);

    // the injection lands right after frame two's magic number, before its
    // descriptor: the retry replays the peeked magic instead of re-reading
    // descriptor bytes as a magic number.
    let boundary = input.len() - frame(payload_b).len();
    assert_eq!(decode_with_transient_error(&input, boundary + 4), expected);
}

#[test]
fn transient_error_mid_skippable_frame_resumes() {
    // the injection lands right after the skip length field: no content was
    // served yet, the retry must resume the discard instead of re-reading
    // content as a magic number.
    let input = concat(&[&frame(b"aaaa"), &skippable(&[b'x'; 1000]), &frame(b"bbbbb")]);
    let content_start = input.len() - frame(b"bbbbb").len() - 1000;
    assert_eq!(
        decode_with_transient_error(&input, content_start),
        b"aaaabbbbb"
    );

    // the injection lands mid-content: 8 KiB (the trash pull size) were
    // already discarded, the retry must continue from there.
    let content = vec![b'x'; 20_000];
    let input = concat(&[&frame(b"aaaa"), &skippable(&content), &frame(b"bbbbb")]);
    let content_start = input.len() - frame(b"bbbbb").len() - 20_000;
    assert_eq!(
        decode_with_transient_error(&input, content_start + 8192 + 5),
        b"aaaabbbbb"
    );
}

/// Hunt staging belongs to the stream its bytes were read from: a decoder
/// abandoned mid next-frame hunt (staging holding the peeked magic) and
/// rebound to a new source must decode the new stream from byte zero — the
/// stale magic used to replay into the new stream's head and misparse it.
#[test]
fn staging_does_not_leak_across_streams() {
    let stream1 = concat(&[&frame(b"aaaa"), &frame(b"bbbbb")]);
    let stream2 = concat(&[&frame(b"ccccc"), &frame(b"ddddd")]);

    let mut dec = FrameDecoder::new();
    // Stream 1: frame 1 decodes, then frame 2's hunt stages the magic and
    // fails with WouldBlock right behind it; the caller abandons the stream.
    let boundary = stream1.len() - frame(b"bbbbb").len();
    {
        let mut s1 = StreamingDecoder::new_with_decoder(
            OneShotWouldBlock::new(&stream1, boundary + 4),
            &mut dec,
        )
        .unwrap();
        let mut out = Vec::new();
        let mut sink = [0u8; 7];
        loop {
            match s1.read(&mut sink) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&sink[..n]),
                Err(_) => break, // the injected WouldBlock: abandon stream 1
            }
        }
        assert_eq!(out, b"aaaa");
    }

    // The reclaimed decoder carries the staged magic; stream 2 must still
    // decode completely.
    let mut s2 = StreamingDecoder::new_with_decoder(stream2.as_slice(), &mut dec).unwrap();
    let mut out = Vec::new();
    let mut sink = [0u8; 7];
    loop {
        match s2.read(&mut sink) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&sink[..n]),
            Err(e) => panic!("stream 2 misparsed: {e}"),
        }
    }
    assert_eq!(out, b"cccccddddd");
}
