//! Structures and utilities used for compressing/encoding data into the Zstd format.

pub(crate) mod block_header;
pub(crate) mod blocks;
pub(crate) mod frame_header;
pub(crate) mod match_generator;
pub(crate) mod util;

mod frame_compressor;
mod levels;
pub use frame_compressor::FrameCompressor;
pub use match_generator::MatchGeneratorDriver;

use crate::io::{Read, Write};
use alloc::vec::Vec;

/// Convenience function to compress some source into a target without reusing any resources of the compressor
/// ```rust
/// use ruzstd::encoding::{compress, CompressionLevel};
/// let data: &[u8] = &[0,0,0,0,0,0,0,0,0,0,0,0];
/// let mut target = Vec::new();
/// compress(data, &mut target, CompressionLevel::Fastest);
/// ```
pub fn compress<R: Read, W: Write>(source: R, target: W, level: CompressionLevel) {
    let mut frame_enc = FrameCompressor::new(level);
    frame_enc.set_source(source);
    frame_enc.set_drain(target);
    frame_enc.compress();
}

/// Convenience function to compress some source into a Vec without reusing any resources of the compressor
/// ```rust
/// use ruzstd::encoding::{compress_to_vec, CompressionLevel};
/// let data: &[u8] = &[0,0,0,0,0,0,0,0,0,0,0,0];
/// let compressed = compress_to_vec(data, CompressionLevel::Fastest);
/// ```
pub fn compress_to_vec<R: Read>(source: R, level: CompressionLevel) -> Vec<u8> {
    let mut vec = Vec::new();
    compress(source, &mut vec, level);
    vec
}

/// The compression mode used impacts the speed of compression,
/// and resulting compression ratios. Faster compression will result
/// in worse compression ratios, and vice versa.
#[derive(Copy, Clone, Debug)]
pub enum CompressionLevel {
    /// This level does not compress the data at all, and simply wraps
    /// it in a Zstandard frame.
    Uncompressed,
    /// This level is roughly equivalent to Zstd compression level 1
    Fastest,
    /// This level is roughly equivalent to Zstd level 3,
    /// or the one used by the official compressor when no level
    /// is specified.
    ///
    /// UNIMPLEMENTED
    Default,
    /// This level is roughly equivalent to Zstd level 7.
    ///
    /// UNIMPLEMENTED
    Better,
    /// This level is roughly equivalent to Zstd level 11.
    ///
    /// UNIMPLEMENTED
    Best,
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

/// Trait used by the encoder that users can use to extend the matching facilities with their own algorithm
/// making their own tradeoffs between runtime, memory usage and compression ratio
///
/// This trait operates on buffers that represent the chunks of data the matching algorithm wants to work on.
/// Each one of these buffers is referred to as a *space*. One or more of these buffers represent the window
/// the decoder will need to decode the data again.
///
/// This library asks the Matcher for the writable tail of its window using `block_tail`, reads the next
/// block of input into it and commits the filled byte count back with `commit_block`.
///
/// Then it will either call `start_matching` or, if the space is deemed not worth compressing, `skip_matching` is called.
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
    /// Process the data in the last commited space for future matching AND generate matches for the data
    fn start_matching(&mut self, handle_sequence: impl for<'a> FnMut(Sequence<'a>));
    /// Buffer-based variant of [`Matcher::start_matching`]: the block's
    /// literals accumulate in `literals` and each match appends one
    /// [`EncodedSequence`] whose `ll` counts the literals emitted right
    /// before it (the interleaving is fully reconstructable). The default
    /// implementation wraps [`Matcher::start_matching`]; the built-in
    /// matcher overrides it so its hot emit path appends to the buffers
    /// directly instead of routing through a closure capture, which spills
    /// matcher state to the stack around every call.
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
            }
        });
    }
    /// Reset this matcher so it can be used for the next new frame
    fn reset(&mut self, level: CompressionLevel);
    /// The size of the window the decoder will need to execute all sequences produced by this matcher
    ///
    /// May change after a call to reset with a different compression level
    fn window_size(&self) -> u64;
    /// Snapshot the repeated-offset history maintained by the matcher
    ///
    /// A block whose generated sequences are ultimately not emitted (raw block
    /// fallback) must restore this snapshot: the decoder only updates its own
    /// history for sequences it actually decodes.
    fn repcode_snapshot(&self) -> [u32; 3];
    /// Restore a snapshot taken by [`Matcher::repcode_snapshot`]
    fn restore_repcode(&mut self, rep: [u32; 3]);
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
