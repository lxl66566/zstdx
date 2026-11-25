//! Shared escape for the decode oracles' reference one-shot check
//! (included by `#[path]` from `decode.rs` and `decode_dict.rs`).
//! libzstd 1.6.0 (zstd-sys 2.2.0) added a Raw/RLE regenerated-size cap of
//! `min(window, 128 KiB)` to `ZSTD_decompressFrame`, where 1.5.7 bounded
//! Raw and RLE blocks only by the destination. zstdx's flat path keeps the
//! 1.5.7 lift (`tests/block_output_cap.rs`), so a frame with an over-cap
//! block decodes on the zstdx side only. This walker tells that
//! documented class from a genuine divergence: it replays the stream
//! through the streaming block stage, which enforces the cap at the
//! header, and reports whether the stream really holds an over-cap block.
//! Any other rejection stays an error at the caller.

use zstdx::decoding::{
    BlockDecodingStrategy, FrameDecoder,
    errors::{DecodeBlockContentError, FrameDecoderError, ReadFrameHeaderError},
};

/// Whether any Raw/RLE block in the (possibly multi-frame) stream claims
/// output over the per-block cap. `false` on any parse failure: the caller
/// treats the frame as a genuine divergence.
pub fn oversized_block_stream(mut data: &[u8]) -> bool {
    let mut dec = FrameDecoder::new();
    loop {
        match dec.reset(&mut data) {
            // Skippable frames carry no blocks; mirror `decode_all`'s skip
            // (the 8 header bytes are already consumed through the read).
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                let Some(rest) = data.get(u64::from(length) as usize..) else {
                    return false;
                };
                data = rest;
                continue;
            },
            Err(_) => return false,
            Ok(()) => {},
        }
        match dec.decode_blocks(&mut data, BlockDecodingStrategy::All) {
            Err(FrameDecoderError::FailedToReadBlockBody(
                DecodeBlockContentError::BlockOutputTooLarge { .. },
            )) => return true,
            Err(_) => return false,
            Ok(_) => {
                if data.is_empty() {
                    return false;
                }
            },
        }
    }
}
