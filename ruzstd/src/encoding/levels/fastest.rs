use crate::{
    common::MAX_BLOCK_SIZE,
    encoding::{
        block_header::BlockHeader, blocks::compress_block, frame_compressor::CompressState, Matcher,
    },
};
use alloc::vec::Vec;
use core::convert::TryInto;

/// Compresses a single block at [`crate::encoding::CompressionLevel::Fastest`].
///
/// # Parameters
/// - `state`: [`CompressState`] so the compressor can refer to data before
///   the start of this block
/// - `last_block`: Whether or not this block is going to be the last block in the frame
///   (needed because this info is written into the block header)
/// - `uncompressed_data`: A block's worth of uncompressed data, taken from the
///   larger input
/// - `output`: As `uncompressed_data` is compressed, it's appended to `output`.
/// Exact uniform-run detection, word-at-a-time like libzstd's `ZSTD_isRLE`
/// (the chunked u64 compare widens to SIMD compares on x86_64 and aarch64).
#[inline]
fn is_uniform(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    let first = data[0];
    let broadcast = u64::from(first) * 0x0101_0101_0101_0101;
    let mut chunks = data.chunks_exact(8);
    let all_eq = chunks.all(|c| u64::from_le_bytes(c.try_into().unwrap()) == broadcast);
    if !all_eq {
        return false;
    }
    chunks.remainder().iter().all(|&b| b == first)
}

#[inline]
pub fn compress_fastest<M: Matcher>(
    state: &mut CompressState<M>,
    last_block: bool,
    uncompressed_data: Vec<u8>,
    output: &mut Vec<u8>,
) {
    let block_size = uncompressed_data.len() as u32;
    // First check to see if run length encoding can be used for the entire block
    if is_uniform(&uncompressed_data) {
        let rle_byte = uncompressed_data[0];
        state.matcher.commit_space(uncompressed_data);
        state.matcher.skip_matching();
        let header = BlockHeader {
            last_block,
            block_type: crate::blocks::block::BlockType::RLE,
            block_size,
        };
        // Write the header, then the block
        header.serialize(output);
        output.push(rle_byte);
    } else {
        // Compress as a standard compressed block
        let mut compressed = Vec::new();
        state.matcher.commit_space(uncompressed_data);
        let rep = state.matcher.repcode_snapshot();
        // Take the reusable entropy tables out of the state by value: the
        // block encoder then can't touch them, so a raw fallback simply puts
        // them back instead of restoring deep clones (each FSE table clone
        // is hundreds of small allocations).
        let old_huff = state.last_huff_table.take();
        let mut old_tables = [
            state.fse_tables.ll_previous.take(),
            state.fse_tables.ml_previous.take(),
            state.fse_tables.of_previous.take(),
        ];
        let tables = {
            // Borrows of the taken tables and the defaults for one call.
            let previous = [
                old_tables[0].as_ref(),
                old_tables[1].as_ref(),
                old_tables[2].as_ref(),
            ];
            compress_block(
                &mut state.matcher,
                old_huff.as_ref(),
                &previous,
                (
                    &state.fse_tables.ll_default,
                    &state.fse_tables.ml_default,
                    &state.fse_tables.of_default,
                ),
                &mut compressed,
            )
        };
        let compressed_size = compressed.len();
        // If compression does not shrink the block, store it raw instead.
        // Also preserve the format guard that compressed blocks must not
        // exceed the maximum block size.
        if compressed_size >= block_size as usize || compressed_size > MAX_BLOCK_SIZE as usize {
            state.matcher.restore_repcode(rep);
            state.last_huff_table = old_huff;
            state.fse_tables.ll_previous = old_tables[0].take();
            state.fse_tables.ml_previous = old_tables[1].take();
            state.fse_tables.of_previous = old_tables[2].take();
            let header = BlockHeader {
                last_block,
                block_type: crate::blocks::block::BlockType::Raw,
                block_size,
            };
            // Write the header, then the block
            header.serialize(output);
            output.extend_from_slice(state.matcher.get_last_space());
        } else {
            // Adopt the tables this block was encoded with; anything the
            // block did not replace falls back to the previous table.
            state.last_huff_table = match tables.huff {
                Some(new) => Some(new),
                None => old_huff,
            };
            state.fse_tables.ll_previous = match tables.ll {
                Some(new) => Some(new),
                None => old_tables[0].take(),
            };
            state.fse_tables.ml_previous = match tables.ml {
                Some(new) => Some(new),
                None => old_tables[1].take(),
            };
            state.fse_tables.of_previous = match tables.of {
                Some(new) => Some(new),
                None => old_tables[2].take(),
            };
            let header = BlockHeader {
                last_block,
                block_type: crate::blocks::block::BlockType::Compressed,
                block_size: compressed_size as u32,
            };
            // Write the header, then the block
            header.serialize(output);
            output.extend(compressed);
        }
    }
}
