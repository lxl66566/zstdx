//! Structures and utilities used for compressing/encoding data into the Zstd format.

#[cfg(all(feature = "std", feature = "hash"))]
pub(crate) mod async_checksum;
pub(crate) mod block_enc;
pub(crate) mod block_header;
pub(crate) mod btlazy;
pub(crate) mod checksum;
pub(crate) mod dictionary;
pub(crate) mod dubt;
pub(crate) mod frame_header;
pub(crate) mod ldm;
pub(crate) mod match_generator;
#[cfg(feature = "std")]
pub(crate) mod mt;
pub(crate) mod opt;
pub(crate) mod reach_probe;
pub(crate) mod seq_codes;
pub(crate) mod util;

pub(crate) mod frame_compressor;
#[cfg(feature = "job_trace")]
pub mod job_trace;
mod levels;
use alloc::vec::Vec;

pub use frame_compressor::{
    FrameCompressor, compress_slice_opts, compress_slice_shaped, compress_slice_to_vec,
    compress_slice_with_dictionary,
};
pub(crate) use levels::compress_fastest;
pub use match_generator::MatchGeneratorDriver;
#[cfg(feature = "std")]
#[doc(hidden)]
pub use mt::mt_job_size_for;
#[cfg(feature = "std")]
#[doc(hidden)]
pub use mt::set_mt_ramp_depth_for_tests;
use seq_codes::pack_seq;

use crate::{
    Level,
    io::{Read, Write},
};

/// Convenience function to compress some source into a target without reusing any resources of the
/// compressor
/// ```rust
/// use zstdx::{Level, encoding::compress};
/// let data: &[u8] = &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// let mut target = Vec::new();
/// compress(data, &mut target, Level::Fastest);
/// ```
pub fn compress<R: Read, W: Write>(source: R, target: W, level: Level) {
    let mut frame_enc = FrameCompressor::new(level);
    frame_enc.set_source(source);
    frame_enc.set_drain(target);
    frame_enc.compress();
}

/// Convenience function to compress some source into a Vec without reusing any resources of the
/// compressor
/// ```rust
/// use zstdx::{Level, encoding::compress_to_vec};
/// let data: &[u8] = &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// let compressed = compress_to_vec(data, Level::Fastest);
/// ```
pub fn compress_to_vec<R: Read>(source: R, level: Level) -> Vec<u8> {
    let mut vec = Vec::new();
    compress(source, &mut vec, level);
    vec
}

/// [`compress_to_vec`] with a caller-declared input shape: the matcher
/// sizes its window and tables accordingly, and a bare known length makes
/// the output match [`compress_slice_to_vec`] on the same data byte for
/// byte.
pub fn compress_to_vec_shaped<R: Read>(
    source: R,
    level: Level,
    shape: crate::InputShape,
) -> Vec<u8> {
    let mut vec = Vec::new();
    let mut frame_enc = FrameCompressor::new(level);
    frame_enc.set_input_shape(shape);
    frame_enc.set_source(source);
    frame_enc.set_drain(&mut vec);
    frame_enc.compress();
    vec
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::{compress_slice_to_vec, compress_to_vec};
    use crate::Level;

    /// Every numeric level 1-22 must roundtrip through its own parameter
    /// row, and the ladder's ends must order (level 22 well below level 1).
    /// Adjacent-level monotonicity is NOT asserted: the dfast→greedy
    /// transition is genuinely weaker on interleaved random fragments, and
    /// libzstd's own ladder inverts the same way on this input (its -1
    /// output is 10% smaller than its -5).
    #[test]
    fn full_ladder_roundtrip() {
        let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            pseudo_random ^= pseudo_random << 13;
            pseudo_random ^= pseudo_random >> 7;
            pseudo_random ^= pseudo_random << 17;
            pseudo_random
        };
        let mut input: Vec<u8> = Vec::new();
        let mut fragment = alloc::string::String::new();
        for i in 0..2000u32 {
            use core::fmt::Write;
            fragment.clear();
            let _ = write!(fragment, "level-ladder row {i} alpha beta gamma\n");
            input.extend_from_slice(fragment.as_bytes());
            input.extend_from_slice(&rand().to_le_bytes());
        }
        input.extend((0..4096u32).map(|i| (i % 61) as u8));
        let bottom = compress_slice_to_vec(&input, Level::Fastest).len();
        let mut top = usize::MAX;
        for lvl in 1..=22 {
            let level = Level::from_zstd(lvl);
            let compressed = compress_slice_to_vec(&input, level);
            let decompressed = crate::bulk::decompress(&compressed, input.len()).unwrap();
            assert_eq!(decompressed, input, "roundtrip failed at level {lvl}");
            top = compressed.len();
        }
        assert!(
            top * 6 < bottom * 5,
            "level 22 ({top}) must beat level 1 ({bottom})"
        );
    }

    /// The slice path (borrowed matcher window, no staging) must produce the
    /// same bytes as the streaming path for every input shape: empty, tiny,
    /// block-boundary straddling, window-crossing, RLE and incompressible.
    #[test]
    fn slice_path_matches_stream_path() {
        let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            pseudo_random ^= pseudo_random << 13;
            pseudo_random ^= pseudo_random >> 7;
            pseudo_random ^= pseudo_random << 17;
            pseudo_random
        };
        let mut inputs: Vec<Vec<u8>> = vec![
            vec![],
            vec![1],
            vec![7u8; 5],
            vec![b'x'; 300 * 1024],
            (0..130 * 1024).map(|_| (rand() & 0xff) as u8).collect(),
            (0..900 * 1024)
                .flat_map(|i| [(i % 251) as u8, 7u8, 7, 7, (i % 13) as u8, 9])
                .collect(),
        ];
        // Straddle block and window boundaries exactly.
        for len in [128 * 1024, 128 * 1024 + 1, 128 * 1024 - 1, 900 * 1024 + 7] {
            inputs.push((0..len).map(|i| (i % 61) as u8).collect());
        }
        for input in &inputs {
            assert_eq!(
                compress_slice_to_vec(input, Level::Fastest),
                super::compress_to_vec_shaped(
                    input.as_slice(),
                    Level::Fastest,
                    crate::InputShape::default().with_len(input.len() as u64)
                ),
                "mismatch at len {}",
                input.len()
            );
            assert_eq!(
                compress_slice_to_vec(input, Level::Uncompressed),
                compress_to_vec(input.as_slice(), Level::Uncompressed),
                "uncompressed mismatch at len {}",
                input.len()
            );
        }
    }
}

/// A sequence as emitted into the encoder's collection buffers by
/// [`Matcher::start_matching_into`]: literal length, match length and the
/// wire-format offset (1..=3 select a repeated offset, larger values encode
/// a literal offset as `actual_offset + 3`).
#[derive(Clone, Copy, Debug)]
pub struct EncodedSequence {
    pub ll: u32,
    pub ml: u32,
    pub of: u32,
}

/// One matcher-emitted sequence in the exact representation the
/// sequence-section encoder consumes: the packed code triple
/// `ll | ml << 8 | of << 16`, the merged add-bits payload (ll add in the
/// low bits, then ml, then of) and its total width. One buffer of these
/// replaces three parallel streams, so the matcher's emit path pays a
/// single push per sequence.
#[derive(Clone, Copy)]
pub struct SeqWord {
    pub(crate) codes: u32,
    pub(crate) add: u64,
    pub(crate) add_nb: u8,
}

/// Trait used by the encoder that users can use to extend the matching facilities with their own
/// algorithm making their own tradeoffs between runtime, memory usage and compression ratio
///
/// This trait operates on buffers that represent the chunks of data the matching algorithm wants to
/// work on. Each one of these buffers is referred to as a *space*. One or more of these buffers
/// represent the window the decoder will need to decode the data again.
///
/// This library asks the Matcher for the writable tail of its window using `block_tail`, reads the
/// next block of input into it and commits the filled byte count back with `commit_block`.
///
/// Then it will either call `start_matching` or, if the space is deemed not worth compressing,
/// `skip_matching` is called.
///
/// This is repeated until no more data is left to be compressed.
pub trait Matcher {
    /// Reserve the match-window tail for the next block and return it as a
    /// writable slice of the maximum block size. The caller fills in input
    /// data, then hands the filled byte count to [`Matcher::commit_block`].
    fn block_tail(&mut self) -> &mut [u8];
    /// Get a reference to the last commited space
    fn get_last_space(&mut self) -> &[u8];
    /// Commit `read` bytes written into the tail from [`Matcher::block_tail`]
    /// as the block to match against.
    fn commit_block(&mut self, read: usize);
    /// Just process the data in the last commited space for future matching
    fn skip_matching(&mut self);
    /// Process the data in the last commited space for future matching AND generate matches for the
    /// data
    fn start_matching(&mut self, handle_sequence: impl for<'a> FnMut(Sequence<'a>));
    /// Buffer-based variant of [`Matcher::start_matching`]: the block's
    /// literals accumulate in `literals` and each match appends one
    /// [`EncodedSequence`] whose `ll` counts the literals emitted right
    /// before it (the interleaving is fully reconstructable). A block that
    /// produces no sequences may leave `literals` empty; its literals are
    /// then the whole block, available through [`Matcher::get_last_space`].
    /// The default implementation wraps [`Matcher::start_matching`]; the
    /// built-in matcher overrides it so its hot emit path appends to the
    /// buffers directly instead of routing through a closure capture, which
    /// spills matcher state to the stack around every call.
    fn start_matching_into(
        &mut self,
        literals: &mut Vec<u8>,
        sequences: &mut Vec<EncodedSequence>,
    ) {
        self.start_matching(|seq| match seq {
            Sequence::Literals { literals: lits } => literals.extend_from_slice(lits),
            Sequence::Triple {
                literals: lits,
                offset,
                match_len,
            } => {
                literals.extend_from_slice(lits);
                sequences.push(EncodedSequence {
                    ll: lits.len() as u32,
                    ml: match_len as u32,
                    of: offset as u32,
                });
            },
        });
    }
    /// Packed variant of [`Matcher::start_matching_into`] and the block
    /// encoder's hot path: literals accumulate in `literals` while each
    /// match appends one [`SeqWord`] to `seqs` — the exact representation
    /// the sequence-section encoder consumes, so the raw (ll, ml, of)
    /// triples never round-trip through a separate buffer. `seqs.len()` is
    /// the sequence count; a block that produces no sequences may leave
    /// `literals` empty (see [`Matcher::start_matching_into`]).
    fn start_matching_codes(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        let mut sequences = Vec::new();
        self.start_matching_into(literals, &mut sequences);
        for seq in sequences {
            seqs.push(pack_seq(seq.ll, seq.ml, seq.of));
        }
    }
    /// Reset this matcher so it can be used for the next new frame
    fn reset(&mut self, level: Level);
    /// Declare what the caller knows about the whole frame before
    /// [`Matcher::reset`]: its exact length (the built-in matcher downsizes
    /// window and tables to the source, libzstd's `ZSTD_adjustCParams`)
    /// and/or a forced window log overriding the level's row. Per frame —
    /// implementations must not carry it across resets.
    fn set_input_shape(&mut self, _shape: crate::InputShape) {}
    /// Decide the frame's chain reach from its first bytes (the built-in
    /// matcher's shape-adaptive row-9 probe, see
    /// `match_generator::reach_probe`): called once per frame, before the
    /// first block is matched, with at least the probe's span of head bytes
    /// staged. The default keeps the level's stock reach.
    fn consider_reach_probe(&mut self, _head: &[u8], _level: Level) {}
    /// Donation-mode hooks for [`Matcher::consider_reach_probe`]'s frame
    /// (see `reach_probe`): `begin_probe_stats` arms a cost accumulation
    /// fed by the matcher's own parses, `take_probe_cost` returns the
    /// accumulated bits (a `None` return opts the matcher out of donation
    /// entirely — its staged blocks stand and the reach stays stock), and
    /// `restart_shrunk` rebuilds the matcher as a fresh shrunk-reach frame
    /// after a donated keep parse measured Shrink. The defaults are the
    /// undonated stock path.
    fn begin_probe_stats(&mut self) {}
    fn take_probe_cost(&mut self) -> Option<f64> {
        None
    }
    fn restart_shrunk(&mut self, _level: Level) {}
    /// Load dictionary content as the frame's match history after
    /// [`Matcher::reset`], before the first block (owned-window
    /// implementations only; the default drops it, and the encoder still
    /// declares the dictionary id and entropy tables — sequences simply
    /// never reference the content).
    fn load_dictionary(&mut self, _content: &[u8], _rep: [u32; 3]) {}
    /// The size of the window the decoder will need to execute all sequences produced by this
    /// matcher
    ///
    /// May change after a call to reset with a different compression level
    fn window_size(&self) -> u64;
    /// Largest block the matcher's frames may carry; the format caps blocks
    /// at the declared window (RFC 8878: Block_Maximum_Size =
    /// min(window, 128K)), so a window below 128 KiB must shrink blocks.
    /// The default keeps the format maximum.
    fn block_size(&self) -> usize {
        crate::common::MAX_BLOCK_SIZE as usize
    }
    /// Snapshot the repeated-offset history maintained by the matcher
    ///
    /// A block whose generated sequences are ultimately not emitted (raw block
    /// fallback) must restore this snapshot: the decoder only updates its own
    /// history for sequences it actually decodes.
    fn repcode_snapshot(&self) -> [u32; 3];
    /// Restore a snapshot taken by [`Matcher::repcode_snapshot`]
    fn restore_repcode(&mut self, rep: [u32; 3]);
    /// Pre-match incompressibility gate: sample the last committed block and,
    /// when it is near-certainly raw (max-entropy bytes and no sampled
    /// repeat anywhere in the frame's history), skip matching it and return
    /// true — the caller then emits the block raw without paying the match
    /// search. Implementations may only override this when skipped positions
    /// stay available as match history for later blocks (the opt strategies'
    /// tree fill is lazy, so a skipped block is indexed on the next block's
    /// fill; the table strategies' next scan dense-fills the gap before its
    /// first probe — a skipped block could otherwise turn a later duplicate
    /// of it raw). The default never gates.
    fn skip_if_incompressible(&mut self) -> bool {
        false
    }
    /// Hint the per-symbol Huffman code lengths of the table that encoded
    /// the previous block's literals (0 = symbol not covered). Matchers may
    /// price a candidate match against the marginal cost of the literals it
    /// displaces. Called once per Huffman-coded block; blocks whose literals
    /// go raw/RLE (and fresh frames or jobs) leave the matcher's current
    /// lengths untouched.
    fn note_literal_costs(&mut self, _lengths: &[u8; 256]) {}
    /// Whether the current frame's row splits 128 KiB blocks at entropy
    /// shifts (libzstd's post-parse block splitter; see
    /// `block_enc::split`). The default keeps the stock
    /// one-block-per-128-KiB shape.
    fn block_splitting_enabled(&self) -> bool {
        false
    }
}

#[derive(PartialEq, Eq, Debug)]
/// Sequences that a [`Matcher`] can produce
pub enum Sequence<'data> {
    /// Is encoded as a sequence for the decoder sequence execution.
    ///
    /// First the literals will be copied to the decoded data,
    /// then `match_len` bytes are copied from `offset` bytes back in the decoded data.
    ///
    /// `offset` is the wire representation: 1..=3 select a repeated offset
    /// (as updated per sequence by the decoder), any larger value encodes a
    /// literal offset as `actual_offset + 3`.
    Triple {
        literals: &'data [u8],
        offset: usize,
        match_len: usize,
    },
    /// This is returned as the last sequence in a block
    ///
    /// These literals will just be copied at the end of the sequence execution by the decoder
    Literals { literals: &'data [u8] },
}
