use crate::{
    common::MAX_BLOCK_SIZE,
    encoding::{
        block_header::BlockHeader, blocks::compress_block, frame_compressor::CompressState, util,
        Matcher,
    },
};
use alloc::vec::Vec;

/// Compresses a single block at [`crate::encoding::CompressionLevel::Fastest`].
///
/// # Parameters
/// - `state`: [`CompressState`] so the compressor can refer to data before
///   the start of this block
/// - `last_block`: Whether or not this block is going to be the last block in the frame
///   (needed because this info is written into the block header)
/// - `output`: As the block is compressed, it's appended to `output`.
///
/// The block data itself is the matcher's last committed space.
#[inline]
pub fn compress_fastest<M: Matcher>(
    state: &mut CompressState<M>,
    last_block: bool,
    output: &mut Vec<u8>,
) {
    let block_size = state.matcher.get_last_space().len() as u32;
    // First check to see if run length encoding can be used for the entire block
    if util::is_uniform(state.matcher.get_last_space()) {
        let rle_byte = state.matcher.get_last_space()[0];
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
        // Reserve the three-byte block header and compress straight into
        // `output`; the header is patched in place once the compressed size
        // is known, saving the per-block staging copy of the whole content.
        let start = output.len();
        output.extend_from_slice(&[0u8; 3]);
        let tables = compress_block(
            &mut state.matcher,
            old_huff.as_ref(),
            (
                &state.fse_tables.ll_default,
                &state.fse_tables.ml_default,
                &state.fse_tables.of_default,
            ),
            output,
            &mut state.scratch,
        );
        let compressed_size = output.len() - start - 3;
        // If compression does not shrink the block, store it raw instead.
        // Also preserve the format guard that compressed blocks must not
        // exceed the maximum block size.
        if compressed_size >= block_size as usize || compressed_size > MAX_BLOCK_SIZE as usize {
            output.truncate(start);
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
            let mut prefix = [0u8; 3];
            BlockHeader {
                last_block,
                block_type: crate::blocks::block::BlockType::Compressed,
                block_size: compressed_size as u32,
            }
            .serialize_into(&mut prefix);
            output[start..start + 3].copy_from_slice(&prefix);
        }
    }
}
