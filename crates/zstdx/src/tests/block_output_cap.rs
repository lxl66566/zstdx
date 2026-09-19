//! Regression tests for the per-block output cap
//! ([`crate::common::max_block_output`]): a hostile frame claiming gigabytes
//! of block output must be rejected with an error before any block-sized
//! allocation, and no decode path may silently produce blocks larger than
//! min(window, 128 KiB).

#![cfg(test)]

use alloc::{vec, vec::Vec};

use crate::decoding::{
    BlockDecodingStrategy, Dictionary, FrameDecoder,
    errors::{
        DecodeBlockContentError, DecompressBlockError, ExecuteSequencesError, FrameDecoderError,
    },
};

fn push_block(out: &mut Vec<u8>, last: bool, btype: u32, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

/// A frame whose final Compressed block claims `n_seq * 65539` (~3 GB) of
/// match output from ~129 KB of input: empty Raw literals, all three FSE
/// streams in RLE mode (LL code 0, OF code 7, ML code 52) and a zero-filled
/// bitstream carrying the 23 extra bits per sequence. The declared window is
/// 1 MiB, so the per-block output cap is 128 KiB.
///
/// With `history_blocks` the frame leads with that many full 128 KiB Raw
/// blocks: the compressed block's offset-7 matches get history (a decoder
/// without the cap executes sequences until the caller's buffer runs out)
/// and the input crosses the MT decoder's size floor for 4 blocks.
fn greedy_match_frame(history_blocks: usize) -> Vec<u8> {
    // The block content is header-capped at MAX_BLOCK_SIZE; the zero-filled
    // bitstream fills everything the sequence section header leaves.
    const SECTION_OVERHEAD: usize = 1 // literals header
        + 3 // nbSeq in the 255 form
        + 1 // modes byte
        + 3; // one RLE symbol per stream
    let n_seq = (crate::common::MAX_BLOCK_SIZE as usize - SECTION_OVERHEAD) * 8 / 23;
    assert!(n_seq > 0x7f00);
    let used = n_seq * 23;
    let pad = 8 - (used % 8); // end marker must sit within the last byte
    let stream_len = (used + pad) / 8;

    let mut block = Vec::new();
    // Literals section: Raw, regenerated size 0.
    block.push(0x00);
    // Sequence section: nbSeq (255 form), all-RLE modes, RLE symbols.
    let rest = (n_seq - 0x7f00) as u16;
    block.extend_from_slice(&[0xff, rest as u8, (rest >> 8) as u8]);
    block.push(0b01_01_01_00); // LL=RLE, OF=RLE, ML=RLE
    block.push(0x00); // LL code 0: ll = 0
    block.push(0x07); // OF code 7: offset value 128 + 7 extra bits
    block.push(0x34); // ML code 52: ml = 65539 + 16 extra bits
    // Backwards bitstream: zero filler, end marker in the last byte.
    block.resize(SECTION_OVERHEAD + stream_len, 0);
    *block.last_mut().unwrap() = 1 << (8 - pad);

    let mut frame = Vec::new();
    frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
    frame.push(0x00); // FHD: no FCS, no checksum, no dict id
    frame.push(0x50); // window descriptor: 1 MiB
    for _ in 0..history_blocks {
        push_block(&mut frame, false, 0, &vec![0xaau8; 128 * 1024]);
    }
    push_block(&mut frame, true, 2, &block);
    frame
}

fn block_output_too_large(err: &FrameDecoderError) -> bool {
    match err {
        FrameDecoderError::FailedToReadBlockBody(inner) => match inner {
            DecodeBlockContentError::BlockOutputTooLarge { .. } => true,
            DecodeBlockContentError::DecompressBlockError(e) => matches!(
                e,
                DecompressBlockError::BlockOutputTooLarge { .. }
                    | DecompressBlockError::ExecuteSequencesError(
                        ExecuteSequencesError::BlockOutputTooLarge { .. }
                    )
            ),
            _ => false,
        },
        _ => false,
    }
}

/// The review's DoS shape on the ring path: with a dictionary registered the
/// frame decodes through the ring buffer, where the block output used to be
/// reserved up front — a ~290 KB frame forced a multi-gigabyte allocation
/// (panic on allocation failure). The cap must reject the block before the
/// reserve instead.
#[test]
fn ring_path_rejects_huge_block_output_claim() {
    let frame = greedy_match_frame(0);

    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.add_dict(Dictionary::load(b"history").unwrap()).unwrap();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("huge block output claim must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });
}

#[test]
fn execute_sequences_rejects_output_over_cap() {
    use crate::{
        blocks::sequence_section::Sequence,
        decoding::{scratch::DecoderScratch, sequence_execution::execute_sequences},
    };

    // Two max-length matches exceed the 128 KiB absolute cap (window 1 MiB).
    let mut scratch = DecoderScratch::new(1024 * 1024);
    scratch.sequences.push(Sequence {
        ll: 0,
        ml: 65539,
        of: 4,
    });
    scratch.sequences.push(Sequence {
        ll: 0,
        ml: 65539,
        of: 4,
    });
    let err = execute_sequences(&mut scratch).unwrap_err();
    let ExecuteSequencesError::BlockOutputTooLarge { max } = err else {
        panic!("unexpected error: {err:?}")
    };
    assert_eq!(max, 128 * 1024);

    // A small declared window caps a block below the absolute maximum too.
    let mut scratch = DecoderScratch::new(1024);
    scratch.literals_buffer.resize(2048, 0);
    let err = execute_sequences(&mut scratch).unwrap_err();
    let ExecuteSequencesError::BlockOutputTooLarge { max } = err else {
        panic!("unexpected error: {err:?}")
    };
    assert_eq!(max, 1024);

    // A block within the cap reserves and executes as before.
    let mut scratch = DecoderScratch::new(1024 * 1024);
    scratch.sequences.push(Sequence {
        ll: 4,
        ml: 8,
        of: 1,
    });
    scratch.literals_buffer.extend_from_slice(b"abcd");
    execute_sequences(&mut scratch).unwrap();
    assert_eq!(scratch.buffer.len(), 12);
}

/// A 1 KiB-window frame holding one Raw block of `size` bytes: out of spec
/// for any `size` above the window (libzstd: "Decompressed Block Size
/// Exceeds Maximum"), legal at exactly `size == window`.
fn small_window_raw_frame(size: usize) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
    frame.push(0x00); // FHD: no FCS, no checksum, no dict id
    frame.push(0x00); // window descriptor: 1 KiB
    push_block(&mut frame, true, 0, &vec![0xaau8; size]);
    frame
}

/// Every decode path must enforce the per-block cap of min(window, 128 KiB)
/// on Raw/RLE blocks too, and must still accept a block at the cap.
#[test]
fn small_window_rejects_oversized_raw_block() {
    let frame = small_window_raw_frame(128 * 1024);

    // Ring path (dictionary attached) via decode_block_content.
    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.add_dict(Dictionary::load(b"history").unwrap()).unwrap();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("oversized Raw block must be rejected");
    assert!(matches!(
        err,
        FrameDecoderError::FailedToReadBlockBody(DecodeBlockContentError::BlockOutputTooLarge {
            max: 1024
        })
    ));

    // Flat streaming path.
    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("oversized Raw block must be rejected");
    assert!(matches!(
        err,
        FrameDecoderError::FailedToReadBlockBody(DecodeBlockContentError::BlockOutputTooLarge {
            max: 1024
        })
    ));

    // decode_all's direct-into-slice path, with far more target room than
    // any legal block could fill (it is the cap, not the target, that trips).
    let mut dec = FrameDecoder::new();
    let mut out = vec![0u8; 32 * 1024 * 1024];
    let err = dec
        .decode_all(frame.as_slice(), &mut out)
        .expect_err("oversized Raw block must be rejected");
    assert!(matches!(
        err,
        FrameDecoderError::FailedToReadBlockBody(DecodeBlockContentError::BlockOutputTooLarge {
            max: 1024
        })
    ));

    // Control: a block at exactly the window decodes on the same shape.
    let frame = small_window_raw_frame(1024);
    let mut dec = FrameDecoder::new();
    let mut out = vec![0u8; 4096];
    assert_eq!(dec.decode_all(frame.as_slice(), &mut out).unwrap(), 1024);
}

/// Same claim on the flat path, into a far larger target than any block may
/// fill: the executor must stop at the block cap instead of silently writing
/// the sequences' full output (libzstd reports corruption there).
#[test]
fn flat_path_rejects_huge_block_output_claim() {
    let frame = greedy_match_frame(1);

    let mut dec = FrameDecoder::new();
    let mut out = vec![0u8; 32 * 1024 * 1024];
    let err = dec
        .decode_all(frame.as_slice(), &mut out)
        .expect_err("huge block output claim must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });
}

/// The parallel decoder must apply the cap in stage A, before a segment's
/// claimed output sizes the output buffer.
#[cfg(feature = "std")]
#[test]
fn mt_stage_a_rejects_huge_block_output_claim() {
    let frame = greedy_match_frame(4); // ~655 KB: crosses the MT input floor

    let mut out = Vec::new();
    let err = crate::decoding::mt_decode_to_vec_for_tests(
        &frame,
        &mut out,
        4,
        crate::decoding::DEFAULT_MAX_WINDOW_SIZE,
    )
    .expect_err("huge block output claim must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });
}

/// A 1 KiB-window frame whose single Compressed block declares a 2 KiB
/// stored body. libzstd bounds every block's *stored* size by
/// min(window, 128K) at the header stage ("Block Size Exceeds Maximum"),
/// so the frame is corrupt before its body is even read — the output-side
/// cap alone never sees it (a compressed body this small claims little
/// output).
fn small_window_oversized_stored_block() -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
    frame.push(0x00); // FHD: no FCS, no checksum, no dict id
    frame.push(0x00); // window descriptor: 1 KiB
    push_block(&mut frame, true, 2, &[0x00; 2048]);
    frame
}

/// The stored-size cap must fire on every decode path, and the reference
/// decoder must reject the same frame.
#[cfg(feature = "std")]
#[test]
fn small_window_rejects_oversized_compressed_stored_size() {
    let frame = small_window_oversized_stored_block();

    // Flat one-shot path.
    let mut dec = FrameDecoder::new();
    let mut out = vec![0u8; 1024 * 1024];
    let err = dec
        .decode_all(frame.as_slice(), &mut out)
        .expect_err("oversized stored body must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });

    // Flat streaming path.
    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("oversized stored body must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });

    // Ring path (dictionary attached).
    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.add_dict(Dictionary::load(b"history").unwrap()).unwrap();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("oversized stored body must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });

    // Parallel decoder: the scan declines the frame and the sequential
    // fallback reports it.
    let mut out = Vec::new();
    let err = crate::decoding::mt_decode_to_vec_for_tests(
        &frame,
        &mut out,
        4,
        crate::decoding::DEFAULT_MAX_WINDOW_SIZE,
    )
    .expect_err("oversized stored body must be rejected");
    assert!(block_output_too_large(&err), "unexpected error: {}", {
        err
    });

    // Reference agreement.
    assert!(
        zstd::decode_all(frame.as_slice()).is_err(),
        "libzstd must reject the oversized stored body"
    );
}

/// The cap must not reject our own encoder's frames: a forced small window
/// shrinks the block size (and with it every stored body) below the window.
/// Roundtrips through the sequential and parallel decoders and libzstd.
#[cfg(feature = "std")]
#[test]
fn tiny_window_encoder_frames_roundtrip() {
    let unit = b"the quick brown fox jumps over the lazy dog; ";
    let mut data = Vec::with_capacity(2 * 1024 * 1024);
    while data.len() < 2 * 1024 * 1024 {
        data.extend_from_slice(unit);
    }
    let opts = |level: crate::Level, workers: u32| {
        crate::EncoderOptions::new(level)
            .workers(workers)
            .with_input_shape(crate::InputShape::default().with_window_log(10))
    };
    for level in [
        crate::Level::Fastest,
        crate::Level::Fast,
        crate::Level::Balanced,
        crate::Level::Best,
    ] {
        for workers in [1u32, 2] {
            let compressed = crate::bulk::compress_with(&data, &opts(level, workers)).unwrap();
            // Premise: the frame really declares the tiny window.
            let (header, _) =
                crate::decoding::frame::read_frame_header(&mut compressed.as_slice()).unwrap();
            assert!(header.window_size().unwrap() <= 4096);

            let mut out = vec![0u8; data.len()];
            let n = FrameDecoder::new()
                .decode_all(compressed.as_slice(), &mut out)
                .unwrap();
            assert_eq!(&out[..n], &data[..], "{level:?}/{workers}");

            let mt = crate::bulk::decompress_with(
                compressed.as_slice(),
                0,
                &crate::DecoderOptions::new().threads(2),
            )
            .unwrap();
            assert_eq!(mt, data, "{level:?}/{workers}");

            let mut lib = Vec::new();
            zstd::stream::copy_decode(compressed.as_slice(), &mut lib).unwrap();
            assert_eq!(lib, data, "{level:?}/{workers}");
        }
    }
}
