use alloc::vec::Vec;

use crate::{
    common::MAX_BLOCK_SIZE,
    encoding::{
        Matcher,
        block_header::BlockHeader,
        blocks::{
            compress_block,
            compressed::{BlockOutcome, PrevTable},
            split,
        },
        frame_compressor::{BlockChecksum, CompressState},
    },
    fse::fse_encoder::FSETable,
};

/// Compresses a single block at [`crate::Level::Fastest`].
///
/// # Parameters
/// - `state`: [`CompressState`] so the compressor can refer to data before the start of this block
/// - `last_block`: Whether or not this block is going to be the last block in the frame (needed
///   because this info is written into the block header)
/// - `output`: As the block is compressed, it's appended to `output`.
/// - `checksum`: Frame checksum backend; this function feeds it exactly the block's input bytes on
///   every path, fusing the absorb into the raw block copy where the backend hashes inline.
///
/// The block data itself is the matcher's last committed space.
#[inline]
pub fn compress_fastest<M: Matcher, C: BlockChecksum>(
    state: &mut CompressState<M>,
    last_block: bool,
    output: &mut Vec<u8>,
    hasher: &mut C,
) {
    let block_size = state.matcher.get_last_space().len() as u32;
    // The uniform scan doubles as the checksum pass: RLE blocks come out
    // fully hashed, anything else resumes at the first mismatch (fused into
    // the raw copy when the block ends up raw).
    let (uniform, hashed) = hasher.scan_block(state.matcher.get_last_space());
    if uniform {
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
    } else if state.matcher.skip_if_incompressible() {
        // Incompressible gate (opt strategies): the block goes raw without
        // paying the match search; the checksum absorb fuses into the copy
        // exactly like the raw fallback below.
        let header = BlockHeader {
            last_block,
            block_type: crate::blocks::block::BlockType::Raw,
            block_size,
        };
        header.serialize(output);
        hasher.raw_out(output, state.matcher.get_last_space(), hashed);
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
        let dict_entropy = state.dict_entropy;
        // The opt rows with a >=2^17 window split blocks at entropy shifts
        // (libzstd's post-parse block splitter); every other row takes the
        // stock single-block path. `headers_final` marks that the splitter
        // already wrote every partition's header and ran the per-partition
        // size guards, so the single-block patch and checks below are
        // skipped.
        let (outcome, headers_final) = if state.matcher.block_splitting_enabled() {
            match super::super::blocks::split::compress_split_block(
                &mut state.matcher,
                last_block,
                output,
                old_huff.as_ref(),
                (
                    &state.fse_tables.ll_default,
                    &state.fse_tables.ml_default,
                    &state.fse_tables.of_default,
                ),
                (&old_tables[0], &old_tables[1], &old_tables[2]),
                dict_entropy,
                &mut state.scratch,
            ) {
                split::SplitOutcome::Emitted(tables) => (BlockOutcome::Encoded(tables), true),
                split::SplitOutcome::Single(outcome) => (outcome, false),
            }
        } else {
            (
                compress_block(
                    &mut state.matcher,
                    old_huff.as_ref(),
                    (
                        &state.fse_tables.ll_default,
                        &state.fse_tables.ml_default,
                        &state.fse_tables.of_default,
                    ),
                    (&old_tables[0], &old_tables[1], &old_tables[2]),
                    dict_entropy,
                    output,
                    &mut state.scratch,
                ),
                false,
            )
        };
        let compressed_size = output.len() - start - 3;
        // If compression does not shrink the block, store it raw instead.
        // Also preserve the format guard that compressed blocks must not
        // exceed the maximum block size. The raw copy absorbs the frame
        // checksum on the way out.
        if matches!(outcome, BlockOutcome::Encoded(_))
            && (headers_final
                || (compressed_size < block_size as usize
                    && compressed_size <= MAX_BLOCK_SIZE as usize))
        {
            let tables = match outcome {
                BlockOutcome::Encoded(t) => t,
                BlockOutcome::Raw => unreachable!(),
            };
            // Adopt the tables this block was encoded with; anything the
            // block did not replace falls back to the previous table. A
            // retired table's transition buffer goes back to the pool. A
            // stream that wrote its own table (or lost it to a predefined
            // one) is no longer dictionary-seeded.
            state.last_huff_table = match tables.huff {
                Some(new) => {
                    state.dict_entropy.huff = false;
                    if let Some(old) = old_huff {
                        old.recycle_aligned(&mut state.scratch.huff);
                    }
                    Some(new)
                },
                None => old_huff,
            };
            // `Clear` drops the remembered table: the block overwrote the
            // decoder's table with a predefined or RLE one, so repeating the
            // old custom table in a later block would desync the streams.
            // Only a carried-over table keeps its dictionary seeding: a
            // fresh table replaces the statistics, a clear drops them.
            state.dict_entropy.ll = dict_entropy.ll && matches!(tables.ll, PrevTable::Keep);
            state.dict_entropy.ml = dict_entropy.ml && matches!(tables.ml, PrevTable::Keep);
            state.dict_entropy.of = dict_entropy.of && matches!(tables.of, PrevTable::Keep);
            state.fse_tables.ll_previous =
                replace_previous(old_tables[0].take(), tables.ll, &mut state.scratch.fse);
            state.fse_tables.ml_previous =
                replace_previous(old_tables[1].take(), tables.ml, &mut state.scratch.fse);
            state.fse_tables.of_previous =
                replace_previous(old_tables[2].take(), tables.of, &mut state.scratch.fse);
            let mut prefix = [0u8; 3];
            BlockHeader {
                last_block,
                block_type: crate::blocks::block::BlockType::Compressed,
                block_size: compressed_size as u32,
            }
            .serialize_into(&mut prefix);
            if !headers_final {
                output[start..start + 3].copy_from_slice(&prefix);
            }
            hasher.hash_tail(&state.matcher.get_last_space()[hashed..]);
        } else {
            output.truncate(start);
            state.matcher.restore_repcode(rep);
            state.last_huff_table = old_huff;
            state.fse_tables.ll_previous = old_tables[0].take();
            state.fse_tables.ml_previous = old_tables[1].take();
            state.fse_tables.of_previous = old_tables[2].take();
            // The block's freshly built tables are discarded unused.
            // (Recycled one by one: PrevTable is ~1.5 KB inline, so an
            // array of them would memcpy the empty slots too.)
            if let BlockOutcome::Encoded(tables) = outcome {
                discard_prev(tables.ll, &mut state.scratch.fse);
                discard_prev(tables.ml, &mut state.scratch.fse);
                discard_prev(tables.of, &mut state.scratch.fse);
                if let Some(table) = tables.huff {
                    table.recycle_aligned(&mut state.scratch.huff);
                }
            }
            // (raw fallback: the decoder never saw a sequence section, so
            // the remembered tables stay exactly as they were)
            let header = BlockHeader {
                last_block,
                block_type: crate::blocks::block::BlockType::Raw,
                block_size,
            };
            // Write the header, then the block (hashing as it goes)
            header.serialize(output);
            hasher.raw_out(output, state.matcher.get_last_space(), hashed);
        }
    }
}

/// Recycle a discarded outcome table's transition buffer (raw-fallback
/// blocks build tables they then throw away unused).
fn discard_prev(table: PrevTable, fse: &mut crate::fse::fse_encoder::FseBuildScratch) {
    if let PrevTable::New(table) = table {
        table.recycle(fse);
    }
}

/// Swap in the block's outcome for one remembered sequence table: the new
/// table when the block wrote one, the previous one on `Keep`; `Clear`
/// (predefined/RLE took over the decoder) and any replaced table return
/// their transition buffer to the pool.
fn replace_previous(
    old: Option<FSETable>,
    new: PrevTable,
    fse: &mut crate::fse::fse_encoder::FseBuildScratch,
) -> Option<FSETable> {
    match new {
        PrevTable::New(new) => {
            if let Some(old) = old {
                old.recycle(fse);
            }
            Some(new)
        },
        PrevTable::Keep => old,
        PrevTable::Clear => {
            if let Some(old) = old {
                old.recycle(fse);
            }
            None
        },
    }
}
