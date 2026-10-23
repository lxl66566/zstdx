//! R65 accepts-vs-libzstd parity regressions: each test pins a malformed
//! input class where zstdx and libzstd disagreed on accept/reject (see
//! docs/src/dev/review/parity.md). The reference behavior is libzstd's
//! `ZSTD_decompress` / `zstd -d` (v1.5.7).

#![cfg(test)]

use alloc::{vec, vec::Vec};

use crate::decoding::{
    FrameDecoder,
    errors::{
        DecodeBlockContentError, DecompressBlockError, FrameDecoderError, ReadFrameHeaderError,
        SequencesHeaderParseError,
    },
};
// The handcrafted shapes live in the shared parity module so the bench
// differential replays the exact bytes these tests pin.
use crate::parity::{BlockWireType, one_sequence_frame, push_block};

fn decode(data: &[u8]) -> Result<Vec<u8>, FrameDecoderError> {
    let mut decoder = FrameDecoder::new();
    let mut out = alloc::vec![0u8; 1 << 20];
    decoder
        .decode_all(data, &mut out)
        .map(|n| out[..n].to_vec())
}

/// RFC 8878 3.1.1.3.1.2: the Symbol_Compression_Modes byte's low two bits
/// are reserved and must be zero. libzstd rejects a set field as corruption
/// (`ZSTD_decodeSeqHeaders`' `*ip & 3` gate); zstdx silently decoded such
/// frames before (R65 differential scan).
#[test]
fn frame_with_reserved_bits_in_mode_byte_is_rejected() {
    let out = decode(&one_sequence_frame(0b01_01_01_00)).expect("reserved field zero decodes");
    assert_eq!(out.len(), 15);

    for modes in [0b01_01_01_01u8, 0b01_01_01_10, 0b00_00_00_11, 0xa8 | 0x03] {
        let frame = one_sequence_frame(modes);
        let err = decode(&frame).expect_err("reserved bits set must be rejected");
        let chain = match &err {
            FrameDecoderError::FailedToReadBlockBody(
                DecodeBlockContentError::DecompressBlockError(
                    DecompressBlockError::SequencesHeaderParseError(e),
                ),
            ) => Some(e),
            _ => None,
        };
        assert!(
            matches!(chain, Some(SequencesHeaderParseError::ReservedBitsSet { got }) if *got == modes),
            "modes {modes:#04x}, got {err:?}"
        );
    }
}

/// Frame_Content_Size = 0 with the single-segment flag and an empty last
/// Raw block: libzstd takes the field at face value (windowSize 0,
/// blockSizeMax 0) and accepts the empty frame; zstdx agrees. Pinned so a
/// future "window too small" check does not reject it (R65: both accept).
#[test]
fn single_segment_zero_fcs_empty_frame_is_accepted() {
    let mut frame = Vec::new();
    frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
    frame.push(0x20); // single segment, FCS field size 1
    frame.push(0x00); // FCS = 0
    push_block(&mut frame, true, BlockWireType::Raw, b""); // empty last Raw block
    let out = decode(&frame).expect("empty single-segment frame decodes");
    assert!(out.is_empty());
}

/// Trailing garbage after a finished frame is a next-frame header parse
/// error in zstdx (same as the CLI's "unknown header"), not a silent accept.
#[test]
fn trailing_garbage_is_rejected() {
    let frame = one_sequence_frame(0b01_01_01_00);
    let mut data = frame.clone();
    data.extend_from_slice(&[0x00, 0x01, 0x02]);
    let err = decode(&data).expect_err("trailing garbage must be rejected");
    assert!(matches!(
        err,
        FrameDecoderError::ReadFrameHeaderError(
            ReadFrameHeaderError::MagicNumberReadError(_) | ReadFrameHeaderError::BadMagicNumber(_)
        )
    ));
}

/// A single-segment frame declaring Frame_Content_Size = 2^27+1: its window
/// (= FCS) is exactly libzstd's `ZSTD_MAXWINDOWSIZE_DEFAULT`, so the default
/// decode limit accepts it (libzstd: `windowSize > maxWindowSize` is false).
/// zstdx rejected this one window value before (limit 2^27, strict `>`).
/// One byte more is rejected by both. The frames are header-only; the pin is
/// that the failure is NOT the window check.
#[test]
fn default_window_limit_is_exactly_libzstds() {
    fn frame(fcs: u64) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
        f.push(0xe0); // single segment, FCS field size 8
        f.extend_from_slice(&fcs.to_le_bytes());
        f
    }
    let err = decode(&frame((1 << 27) + 1)).expect_err("no blocks present");
    assert!(
        !matches!(err, FrameDecoderError::WindowSizeTooBig { .. }),
        "2^27+1 must pass the default window check: {err:?}"
    );
    let err = decode(&frame((1 << 27) + 2)).expect_err("over the limit");
    assert!(matches!(
        err,
        FrameDecoderError::WindowSizeTooBig { requested, max }
            if requested == (1 << 27) + 2 && max == crate::decoding::DEFAULT_MAX_WINDOW_SIZE
    ));
}

/// The absolute window ceiling: no libzstd configuration decodes a window
/// above 2^31 (`ZSTD_WINDOWLOG_MAX` = 31 on 64-bit; `ZSTD_d_windowLogMax`
/// cannot be raised further). A windowLog-31/mantissa-0 frame (= 2^31) is
/// the largest accepted; mantissa >= 1 is rejected even with the limit
/// raised to the clamp. Pinned at header level (header-only frames).
#[test]
fn absolute_window_ceiling_is_two_gibibytes() {
    fn frame(window_descriptor: u8) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
        f.push(0x00); // no single segment, no FCS
        f.push(window_descriptor);
        f
    }
    let mut decoder = FrameDecoder::new();
    decoder.set_max_window_size(u64::MAX); // clamps to MAX_WINDOW_SIZE
    let mut out = vec![0u8; 8];
    // windowLog 31, mantissa 0 -> exactly 2^31: accepted window
    let err = decoder
        .decode_all(&frame((31 - 10) << 3), &mut out)
        .expect_err("no blocks present");
    assert!(
        !matches!(
            err,
            FrameDecoderError::WindowSizeTooBig { .. }
                | FrameDecoderError::FrameHeaderError(
                    crate::decoding::errors::FrameHeaderError::WindowTooBig { .. }
                )
        ),
        "2^31 must be the accepted ceiling: {err:?}"
    );
    // windowLog 31, mantissa 1 -> 2^31 + 2^28: rejected at header level
    let err = decoder
        .decode_all(&frame(((31 - 10) << 3) | 1), &mut out)
        .expect_err("over the absolute ceiling");
    assert!(matches!(
        err,
        FrameDecoderError::FrameHeaderError(
            crate::decoding::errors::FrameHeaderError::WindowTooBig { .. }
        )
    ));
}

/// R66 literals tail parity 1/3: the reference's x86_64 fast decode
/// (4-stream literals, every stream >= 8 bytes) validates by output
/// length, not by exact bit consumption (`HUF_decompress4X1_.._fast` has
/// no `BIT_endOfDStream`). Stream 1 of this frame carries only 4 of its
/// 10 codes; the tail must keep decoding rotations of the frozen floor
/// window (the 8 bytes at the jump table) until the segment is full, and
/// the frame is ACCEPTED with that garbage visible in the output. zstdx
/// used to apply the fallback body's strict tail here and rejected.
#[test]
fn four_stream_literals_underflow_is_accepted_with_reference_garbage() {
    let frame = crate::parity::four_stream_underflow_frame();
    let out = decode(&frame).expect("under-running 4-stream literals decode");
    // streams 0/2/3: the full code list; stream 1: 4 codes then the
    // zero-rotation fill.
    let full: [u8; 10] = [1, 2, 3, 1, 1, 2, 3, 1, 2, 2];
    let mut expected = Vec::new();
    expected.extend_from_slice(&full);
    expected.extend_from_slice(&full[..4]);
    expected.extend_from_slice(&[0u8; 6]);
    expected.extend_from_slice(&full);
    expected.extend_from_slice(&full);
    assert_eq!(out, expected);
}

/// R66 literals tail parity 2/3: a tight single stream (every bit
/// consumed) decodes on the count-driven 1X path.
#[test]
fn single_stream_tight_literals_are_accepted() {
    let frame = crate::parity::single_stream_frame(false);
    let out = decode(&frame).expect("tight single-stream literals decode");
    assert_eq!(out, [1, 2, 3, 1, 1, 2, 3, 1, 2, 2]);
}

/// R66 literals tail parity 3/3: the same codes with 11 leftover zero
/// bits are rejected — the 1X path is count-driven then gated by the
/// exact-consumption check (`BIT_endOfDStream`), and zstdx used to skip
/// that gate entirely (bits-driven loop, count check only), accepting
/// frames libzstd rejects.
#[test]
fn single_stream_leftover_bits_are_rejected() {
    let frame = crate::parity::single_stream_frame(true);
    let err = decode(&frame).expect_err("leftover bits must be rejected");
    assert!(
        matches!(
            err,
            FrameDecoderError::FailedToReadBlockBody(
                DecodeBlockContentError::DecompressBlockError(
                    DecompressBlockError::DecompressLiteralsError(_),
                ),
            )
        ),
        "got {err:?}"
    );
}
