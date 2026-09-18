//! Regression tests for the per-block output cap
//! ([`crate::common::max_block_output`]): a hostile frame claiming gigabytes
//! of block output must be rejected with an error before any block-sized
//! allocation, and no decode path may silently produce blocks larger than
//! min(window, 128 KiB).

use alloc::{string::ToString, vec, vec::Vec};

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
/// With `history_block` the frame leads with a full 128 KiB Raw block, so the
/// compressed block's offset-7 matches have history and a decoder without the
/// cap would execute sequences until the caller's buffer runs out.
fn greedy_match_frame(history_block: bool) -> Vec<u8> {
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
    if history_block {
        push_block(&mut frame, false, 0, &vec![0xaau8; 128 * 1024]);
    }
    push_block(&mut frame, true, 2, &block);
    frame
}

fn block_output_too_large(err: &FrameDecoderError) -> bool {
    matches!(
        err,
        FrameDecoderError::FailedToReadBlockBody(DecodeBlockContentError::DecompressBlockError(
            DecompressBlockError::ExecuteSequencesError(
                ExecuteSequencesError::BlockOutputTooLarge { .. }
            )
        ))
    )
}

/// The review's DoS shape on the ring path: with a dictionary registered the
/// frame decodes through the ring buffer, where the block output used to be
/// reserved up front — a ~290 KB frame forced a multi-gigabyte allocation
/// (panic on allocation failure). The cap must reject the block before the
/// reserve instead.
#[test]
fn ring_path_rejects_huge_block_output_claim() {
    let frame = greedy_match_frame(false);

    let mut src = frame.as_slice();
    let mut dec = FrameDecoder::new();
    dec.add_dict(Dictionary::load(b"history").unwrap()).unwrap();
    dec.reset(&mut src).unwrap();
    let err = dec
        .decode_blocks(&mut src, BlockDecodingStrategy::All)
        .expect_err("huge block output claim must be rejected");
    assert!(
        block_output_too_large(&err),
        "unexpected error: {}",
        err.to_string()
    );
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
