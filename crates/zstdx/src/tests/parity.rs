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

fn decode(data: &[u8]) -> Result<Vec<u8>, FrameDecoderError> {
    let mut decoder = FrameDecoder::new();
    let mut out = alloc::vec![0u8; 1 << 20];
    decoder
        .decode_all(data, &mut out)
        .map(|n| out[..n].to_vec())
}

fn push_block(out: &mut Vec<u8>, last: bool, btype: u32, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

/// A minimal valid frame with one Compressed block: Raw literals "ababababab"
/// (header 0x50 = 10 << 3), one sequence with all three FSE streams in RLE
/// mode (LL code 9 → ll 9, OF code 0 → rep0 = offset 1, ML code 2 → ml 5)
/// and a bare end-marker bitstream (every read is zero-width). Output:
/// "ababababa" + "aaaaa" (offset-1 match off the just-written 'a') + the
/// trailing literal 'b' = 15 bytes.
fn one_sequence_frame(modes_byte: u8) -> Vec<u8> {
    let mut block = Vec::new();
    block.push(0x50); // Raw literals, regenerated size 10
    block.extend_from_slice(b"ababababab");
    block.push(0x01); // nbSeq = 1
    block.push(modes_byte);
    block.push(9); // LL RLE code 9: ll = 9, 0 add bits
    block.push(0); // OF RLE code 0: offset value 1 (rep0), 0 add bits
    block.push(2); // ML RLE code 2: ml = 5, 0 add bits
    block.push(0x01); // bitstream: end marker only
    let mut frame = Vec::new();
    frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
    frame.push(0x00); // FHD: nothing declared
    frame.push(0x00); // window descriptor: 1 KiB
    push_block(&mut frame, true, 2, &block);
    frame
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
    push_block(&mut frame, true, 0, b""); // empty last Raw block
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
        FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::MagicNumberReadError(_))
            | FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::BadMagicNumber(_))
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
