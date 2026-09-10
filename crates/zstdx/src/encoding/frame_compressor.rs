//! Utilities and interfaces for encoding an entire frame. Allows reusing resources

use alloc::vec::Vec;
use core::convert::TryInto;

use super::{
    block_header::BlockHeader, frame_header::FrameHeader, levels::*,
    match_generator::MatchGeneratorDriver, Matcher,
};
use crate::fse::fse_encoder::{default_ll_table, default_ml_table, default_of_table, FSETable};
use crate::Level;

use crate::io::{Read, Write};

/// Frame checksum accumulator. With the `hash` feature it is an XXH64 over
/// the frame content; without it a no-op so the block paths share one shape.
/// The raw-block writers feed it while they copy (`write_appending`), which
/// removes the separate hash read pass over incompressible data.
pub(crate) struct FrameHasher {
    #[cfg(feature = "hash")]
    inner: crate::xxh64::Xxh64,
}

#[cfg(feature = "hash")]
impl FrameHasher {
    pub(crate) fn new() -> Self {
        Self {
            inner: crate::xxh64::Xxh64::new(0),
        }
    }
    #[inline(always)]
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.inner.write(bytes);
    }
    #[inline(always)]
    pub(crate) fn write_appending(&mut self, out: &mut Vec<u8>, bytes: &[u8]) {
        self.inner.write_appending(out, bytes);
    }
    #[inline(always)]
    pub(crate) fn write_appending_from(
        &mut self,
        out: &mut Vec<u8>,
        bytes: &[u8],
        hash_from: usize,
    ) {
        self.inner.write_appending_from(out, bytes, hash_from);
    }
    /// Uniform scan fused with the checksum absorb: RLE blocks come out
    /// fully hashed in the scan's single pass; anything else returns the
    /// resume offset for the outcome paths. Misaligned streams (streaming
    /// reader) fall back to a plain scan that hashes from zero.
    #[inline(always)]
    pub(crate) fn scan_uniform(&mut self, data: &[u8]) -> (bool, usize) {
        if self.inner.mem_is_empty() {
            self.inner.scan_uniform_absorbing(data)
        } else {
            let uniform = super::util::is_uniform(data);
            if uniform {
                self.inner.write(data);
                (true, data.len())
            } else {
                (false, 0)
            }
        }
    }
    #[inline(always)]
    pub(crate) fn finish(&self) -> u32 {
        self.inner.finish() as u32
    }
}

#[cfg(not(feature = "hash"))]
impl FrameHasher {
    pub(crate) fn new() -> Self {
        Self {}
    }
    #[inline(always)]
    pub(crate) fn write(&mut self, _bytes: &[u8]) {}
    #[inline(always)]
    pub(crate) fn write_appending(&mut self, out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(bytes);
    }
    #[inline(always)]
    pub(crate) fn write_appending_from(
        &mut self,
        out: &mut Vec<u8>,
        bytes: &[u8],
        _hash_from: usize,
    ) {
        out.extend_from_slice(bytes);
    }
    #[inline(always)]
    pub(crate) fn scan_uniform(&mut self, data: &[u8]) -> (bool, usize) {
        (super::util::is_uniform(data), 0)
    }
    #[inline(always)]
    pub(crate) fn finish(&self) -> u32 {
        0
    }
}

/// Frame-checksum backend shared by the block paths: check a block for a
/// uniform run while absorbing it, absorb whatever the scan did not cover,
/// copy a raw block out while absorbing from the scan's offset, and produce
/// the frame checksum. The inline [`FrameHasher`] implements all four by
/// itself; the offloaded implementation (std + hash) runs the absorbs on a
/// sidecar thread and its `scan_block` returns the block's full length as
/// the covered offset, which turns the resume paths into no-ops.
pub(crate) trait BlockChecksum {
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize);
    fn hash_tail(&mut self, bytes: &[u8]);
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize);
    fn finish32(&mut self) -> u32;
}

impl BlockChecksum for FrameHasher {
    #[inline(always)]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        self.scan_uniform(data)
    }
    #[inline(always)]
    fn hash_tail(&mut self, bytes: &[u8]) {
        self.write(bytes);
    }
    #[inline(always)]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize) {
        self.write_appending_from(out, bytes, from);
    }
    #[inline(always)]
    fn finish32(&mut self) -> u32 {
        self.finish()
    }
}

/// Slice-path checksum backend: inline below the offload threshold (the
/// sidecar thread's spawn and hand-off cost more than hashing saves on small
/// inputs), offloaded above it, or disabled when the caller opted out of a
/// checksum (the uniform scan still runs for the RLE path).
pub(crate) enum SliceChecksum {
    #[cfg(feature = "hash")]
    Inline(FrameHasher),
    #[cfg(all(feature = "std", feature = "hash"))]
    Offload(super::async_checksum::AsyncChecksum),
    Off,
}

#[cfg(all(feature = "std", feature = "hash"))]
const ASYNC_MIN_INPUT: usize = 256 * 1024;

impl SliceChecksum {
    pub(crate) fn new(src_len: usize, checksum: bool) -> Self {
        #[cfg(not(all(feature = "std", feature = "hash")))]
        let _ = src_len;
        if !checksum {
            return Self::Off;
        }
        #[cfg(all(feature = "std", feature = "hash"))]
        if src_len >= ASYNC_MIN_INPUT {
            if let Some(offload) = super::async_checksum::AsyncChecksum::new() {
                return Self::Offload(offload);
            }
        }
        #[cfg(feature = "hash")]
        {
            Self::Inline(FrameHasher::new())
        }
        #[cfg(not(feature = "hash"))]
        {
            Self::Off
        }
    }
}

#[cfg(feature = "hash")]
impl BlockChecksum for SliceChecksum {
    #[inline]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        match self {
            Self::Inline(h) => h.scan_block(data),
            // The whole block is posted up front; the resume paths below see
            // a covered offset equal to the length and contribute nothing.
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => {
                h.write(data);
                (super::util::is_uniform(data), data.len())
            }
            Self::Off => (super::util::is_uniform(data), 0),
        }
    }
    #[inline]
    fn hash_tail(&mut self, bytes: &[u8]) {
        match self {
            Self::Inline(h) => h.hash_tail(bytes),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => h.write(bytes),
            Self::Off => {}
        }
    }
    #[inline]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize) {
        match self {
            Self::Inline(h) => h.raw_out(out, bytes, from),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => {
                out.extend_from_slice(bytes);
                if from < bytes.len() {
                    h.write(&bytes[from..]);
                }
            }
            Self::Off => out.extend_from_slice(bytes),
        }
    }
    #[inline]
    fn finish32(&mut self) -> u32 {
        match self {
            Self::Inline(h) => h.finish32(),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => h.finish(),
            Self::Off => 0,
        }
    }
}

/// Without the `hash` feature only the `Off` variant exists: uniform
/// detection still runs (the RLE path needs it).
#[cfg(not(feature = "hash"))]
impl BlockChecksum for SliceChecksum {
    #[inline]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        (super::util::is_uniform(data), 0)
    }
    #[inline]
    fn hash_tail(&mut self, _bytes: &[u8]) {}
    #[inline]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], _from: usize) {
        out.extend_from_slice(bytes);
    }
    #[inline]
    fn finish32(&mut self) -> u32 {
        0
    }
}

/// An interface for compressing arbitrary data with the ZStandard compression algorithm.
///
/// `FrameCompressor` will generally be used by:
/// 1. Initializing a compressor by providing a buffer of data using `FrameCompressor::new()`
/// 2. Starting compression and writing that compression into a vec using `FrameCompressor::begin`
///
/// # Examples
/// ```
/// use zstdx::{encoding::FrameCompressor, Level};
/// let mock_data: &[_] = &[0x1, 0x2, 0x3, 0x4];
/// let mut output = std::vec::Vec::new();
/// // Initialize a compressor.
/// let mut compressor = FrameCompressor::new(Level::Uncompressed);
/// compressor.set_source(mock_data);
/// compressor.set_drain(&mut output);
///
/// // `compress` writes the compressed output into the provided buffer.
/// compressor.compress();
/// ```
pub struct FrameCompressor<R: Read, W: Write, M: Matcher> {
    uncompressed_data: Option<R>,
    compressed_data: Option<W>,
    compression_level: Level,
    state: CompressState<M>,
    hasher: FrameHasher,
}

pub(crate) struct FseTables {
    pub(crate) ll_default: FSETable,
    pub(crate) ll_previous: Option<FSETable>,
    pub(crate) ml_default: FSETable,
    pub(crate) ml_previous: Option<FSETable>,
    pub(crate) of_default: FSETable,
    pub(crate) of_previous: Option<FSETable>,
}

impl FseTables {
    pub fn new() -> Self {
        Self {
            ll_default: default_ll_table(),
            ll_previous: None,
            ml_default: default_ml_table(),
            ml_previous: None,
            of_default: default_of_table(),
            of_previous: None,
        }
    }
}

pub(crate) struct CompressState<M: Matcher> {
    pub(crate) matcher: M,
    pub(crate) last_huff_table: Option<crate::huff0::huff0_encoder::HuffmanTable>,
    pub(crate) fse_tables: FseTables,
    /// Pooled per-block scratch (literals, sequences, code streams): reused
    /// across blocks so steady-state blocks run allocation-free.
    pub(crate) scratch: super::blocks::compressed::BlockScratch,
}

/// Per-thread pool for the slice entry point: the hash table, the three
/// default FSE tables and the block scratch are identical for every frame,
/// so rebuilding them per call dominates small inputs. The state is taken
/// out for the duration of the call, so reentrant compression (the drain of
/// a nested compressor) cannot observe the borrow.
#[cfg(feature = "std")]
std::thread_local! {
    static SLICE_STATE: core::cell::RefCell<Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>> =
        const { core::cell::RefCell::new(None) };
}

pub(crate) fn new_slice_state() -> CompressState<MatchGeneratorDriver> {
    CompressState {
        matcher: MatchGeneratorDriver::new_direct(),
        last_huff_table: None,
        fse_tables: FseTables::new(),
        scratch: Default::default(),
    }
}

/// Reset a pooled state for a new frame: the matcher's epoch bump retires
/// stale hash entries and the entropy tables return to their defaults.
pub(crate) fn reset_slice_state(state: &mut CompressState<MatchGeneratorDriver>, level: Level) {
    state.matcher.reset(level);
    state.last_huff_table = None;
    state.fse_tables.ll_previous = None;
    state.fse_tables.ml_previous = None;
    state.fse_tables.of_previous = None;
}

/// Take the per-thread pooled slice state, reset for a fresh frame at
/// `level`. Fresh states are built (and reset) when the pool is empty.
pub(crate) fn take_slice_state(
    level: Level,
) -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    #[cfg(feature = "std")]
    if let Some(mut s) = SLICE_STATE.with(|p| p.borrow_mut().take()) {
        reset_slice_state(&mut s, level);
        return s;
    }
    let mut fresh = alloc::boxed::Box::new(new_slice_state());
    reset_slice_state(&mut fresh, level);
    fresh
}

/// Return a state taken by [`take_slice_state`] to the pool.
pub(crate) fn return_slice_state(state: alloc::boxed::Box<CompressState<MatchGeneratorDriver>>) {
    #[cfg(feature = "std")]
    SLICE_STATE.with(|p| *p.borrow_mut() = Some(state));
    #[cfg(not(feature = "std"))]
    drop(state);
}

/// Compress an in-memory buffer into a fresh Vec with no intermediate
/// copies: the matcher window points directly into `src` (no read pass, no
/// window compaction, no window allocation) and blocks append straight into
/// the output (no staging buffer). Produces the same bytes as
/// [`super::compress`] over the same input.
pub fn compress_slice_to_vec(src: &[u8], level: Level) -> Vec<u8> {
    compress_slice_opts(src, level, cfg!(feature = "hash"))
}

/// [`compress_slice_to_vec`] with a runtime checksum switch (the frame
/// header flag and the trailing hash follow `checksum`, modulo the `hash`
/// feature).
pub fn compress_slice_opts(src: &[u8], level: Level, checksum: bool) -> Vec<u8> {
    let mut state = take_slice_state(level);
    let output = compress_with_state(&mut state, src, level, checksum);
    return_slice_state(state);
    output
}

fn compress_with_state(
    state: &mut CompressState<MatchGeneratorDriver>,
    src: &[u8],
    level: Level,
    checksum: bool,
) -> Vec<u8> {
    let mut hasher = SliceChecksum::new(src.len(), checksum);
    // Worst case (every block raw) is the input size plus block headers;
    // reserving it up front keeps the output free of realloc copies, and the
    // untouched tail of the reservation only costs address space.
    let block_size = crate::common::MAX_BLOCK_SIZE as usize;
    let block_overhead = 3 * (src.len() / block_size + 1);
    let mut output = Vec::with_capacity(src.len() + block_overhead + 32);
    let header = FrameHeader {
        frame_content_size: None,
        single_segment: false,
        content_checksum: checksum && cfg!(feature = "hash"),
        dictionary_id: None,
        window_size: Some(state.matcher.window_size()),
    };
    header.serialize(&mut output);
    let max_window = state.matcher.window_size();
    // The streaming reader cannot mark a just-filled block as last until the
    // next read returns EOF, so an input that is an exact multiple of the
    // block size ends with one empty raw block; emit the same shape to keep
    // the outputs byte-identical.
    let trailing_empty = !src.is_empty() && src.len() % block_size == 0;
    for (i, block) in src.chunks(block_size).enumerate() {
        let block_start = (i * block_size) as u64;
        let block_end = block_start + block.len() as u64;
        let last_block = block_end == src.len() as u64 && !trailing_empty;
        let hist = block_start.saturating_sub(max_window);
        state
            .matcher
            .adopt_window(&src[hist as usize..block_end as usize], hist);
        state.matcher.set_block(block_start, block_end);
        match level {
            Level::Uncompressed => {
                let header = BlockHeader {
                    last_block,
                    block_type: crate::blocks::block::BlockType::Raw,
                    block_size: block.len() as u32,
                };
                header.serialize(&mut output);
                BlockChecksum::raw_out(&mut hasher, &mut output, state.matcher.get_last_space(), 0);
            }
            Level::Fastest
            | Level::Fast
            | Level::Balanced
            | Level::Best
            | Level::Opt
            | Level::Ultra => {
                super::levels::compress_fastest(state, last_block, &mut output, &mut hasher)
            }
        }
    }
    // A frame needs at least one block: empty input, and the exact-multiple
    // tail above, encode one empty raw last block (mirroring the streaming
    // path).
    if src.is_empty() || trailing_empty {
        let header = BlockHeader {
            last_block: true,
            block_type: crate::blocks::block::BlockType::Raw,
            block_size: 0,
        };
        header.serialize(&mut output);
    }
    #[cfg(feature = "hash")]
    if checksum {
        output.extend_from_slice(&hasher.finish32().to_le_bytes());
    }
    output
}

/// Compress the blocks of one multithreaded job into `output`: no frame
/// header, no checksum (the mt driver writes both around the assembled job
/// stream). The caller resets the state per job (fresh entropy tables, so
/// every job's first block is self-describing) and gates repcodes on every
/// job except the first, which starts where the decoder's history matches
/// the format default [1, 4, 8].
///
/// `src` is the whole frame input and must stay alive and unchanged for the
/// call (the matcher window borrows into it); `job` is the byte range this
/// job encodes and `overlap` the preceding history the matcher may reference.
#[cfg(feature = "std")]
pub(crate) fn compress_job_blocks(
    state: &mut CompressState<MatchGeneratorDriver>,
    src: &[u8],
    job: core::ops::Range<usize>,
    overlap: usize,
    is_last_job: bool,
) -> Vec<u8> {
    let block_size = crate::common::MAX_BLOCK_SIZE as usize;
    let max_window = state.matcher.window_size() as usize;
    let mut output = Vec::with_capacity(job.len() + 3 * (job.len() / block_size + 1) + 8);
    // Uniform detection still runs (the RLE path), but the frame checksum is
    // the mt driver's job over the whole input.
    let mut hasher = SliceChecksum::new(0, false);
    // Index the strip before the first block adopts it, on every job: the
    // tables start empty (see prefill_window — including job zero, whose
    // pooled state may carry an earlier frame's entries), and without the
    // index pass the strip is only a legal boundary extension no sequence
    // can ever resolve into.
    let strip = job.start.saturating_sub(overlap);
    state
        .matcher
        .prefill_window(&src[strip..job.start], strip as u64);
    let mut cursor = job.start;
    while cursor < job.end {
        let block_end = (cursor + block_size).min(job.end);
        let last_block = is_last_job && block_end == job.end;
        // Candidates never precede this job's first indexed position, so the
        // window only needs the overlap strip plus the in-job history room.
        let hist = cursor
            .saturating_sub(max_window)
            .max(job.start.saturating_sub(overlap));
        state
            .matcher
            .adopt_window(&src[hist..block_end], hist as u64);
        state.matcher.set_block(cursor as u64, block_end as u64);
        super::levels::compress_fastest(state, last_block, &mut output, &mut hasher);
        cursor = block_end;
    }
    output
}

impl<R: Read, W: Write> FrameCompressor<R, W, MatchGeneratorDriver> {
    /// Create a new `FrameCompressor`
    pub fn new(compression_level: Level) -> Self {
        Self {
            uncompressed_data: None,
            compressed_data: None,
            compression_level,
            state: CompressState {
                matcher: MatchGeneratorDriver::new(1024 * 128),
                last_huff_table: None,
                fse_tables: FseTables::new(),
                scratch: Default::default(),
            },
            hasher: FrameHasher::new(),
        }
    }
}

impl<R: Read, W: Write, M: Matcher> FrameCompressor<R, W, M> {
    /// Create a new `FrameCompressor` with a custom matching algorithm implementation
    pub fn new_with_matcher(matcher: M, compression_level: Level) -> Self {
        Self {
            uncompressed_data: None,
            compressed_data: None,
            state: CompressState {
                matcher,
                last_huff_table: None,
                fse_tables: FseTables::new(),
                scratch: Default::default(),
            },
            compression_level,
            hasher: FrameHasher::new(),
        }
    }

    /// Before calling [FrameCompressor::compress] you need to set the source.
    ///
    /// This is the data that is compressed and written into the drain.
    pub fn set_source(&mut self, uncompressed_data: R) -> Option<R> {
        self.uncompressed_data.replace(uncompressed_data)
    }

    /// Before calling [FrameCompressor::compress] you need to set the drain.
    ///
    /// As the compressor compresses data, the drain serves as a place for the output to be writte.
    pub fn set_drain(&mut self, compressed_data: W) -> Option<W> {
        self.compressed_data.replace(compressed_data)
    }

    /// Compress the uncompressed data from the provided source as one Zstd frame and write it to the provided drain
    ///
    /// This will repeatedly call [Read::read] on the source to fill up blocks until the source returns 0 on the read call.
    /// Also [Write::write_all] will be called on the drain after each block has been encoded.
    ///
    /// To avoid endlessly encoding from a potentially endless source (like a network socket) you can use the
    /// [Read::take] function
    pub fn compress(&mut self) {
        // Clearing buffers to allow re-using of the compressor
        self.state.matcher.reset(self.compression_level);
        self.state.last_huff_table = None;
        self.hasher = FrameHasher::new();
        let source = self.uncompressed_data.as_mut().unwrap();
        let drain = self.compressed_data.as_mut().unwrap();
        // As the frame is compressed, it's stored here
        let output: &mut Vec<u8> = &mut Vec::with_capacity(1024 * 130);
        // First write the frame header
        let header = FrameHeader {
            frame_content_size: None,
            single_segment: false,
            content_checksum: cfg!(feature = "hash"),
            dictionary_id: None,
            window_size: Some(self.state.matcher.window_size()),
        };
        header.serialize(output);
        // Now compress block by block
        loop {
            // Read a single block's worth of uncompressed data straight into
            // the tail of the matcher's window (no intermediate buffer copy).
            let tail = self.state.matcher.block_tail();
            let mut read_bytes = 0;
            let last_block;
            'read_loop: loop {
                let new_bytes = source.read(&mut tail[read_bytes..]).unwrap();
                if new_bytes == 0 {
                    last_block = true;
                    break 'read_loop;
                }
                read_bytes += new_bytes;
                if read_bytes == tail.len() {
                    last_block = false;
                    break 'read_loop;
                }
            }
            self.state.matcher.commit_block(read_bytes);
            // Special handling is needed for compression of a totally empty file (why you'd want to do that, I don't know)
            if read_bytes == 0 {
                let header = BlockHeader {
                    last_block: true,
                    block_type: crate::blocks::block::BlockType::Raw,
                    block_size: 0,
                };
                // Write the header, then the block
                header.serialize(output);
                drain.write_all(output).unwrap();
                output.clear();
                break;
            }

            match self.compression_level {
                Level::Uncompressed => {
                    let header = BlockHeader {
                        last_block,
                        block_type: crate::blocks::block::BlockType::Raw,
                        block_size: read_bytes.try_into().unwrap(),
                    };
                    // Write the header, then the block (hashing as it goes)
                    header.serialize(output);
                    self.hasher
                        .write_appending(output, self.state.matcher.get_last_space());
                }
                Level::Fastest
                | Level::Fast
                | Level::Balanced
                | Level::Best
                | Level::Opt
                | Level::Ultra => {
                    compress_fastest(&mut self.state, last_block, output, &mut self.hasher)
                }
            }
            drain.write_all(output).unwrap();
            output.clear();
            if last_block {
                break;
            }
        }

        // If the `hash` feature is enabled, then `content_checksum` is set to true in the header
        // and a 32 bit hash is written at the end of the data.
        #[cfg(feature = "hash")]
        {
            // Because we only have the data as a reader, we need to read all of it to calculate the checksum
            // Possible TODO: create a wrapper around self.uncompressed data that hashes the data as it's read?
            let content_checksum = self.hasher.finish();
            drain.write_all(&content_checksum.to_le_bytes()).unwrap();
        }
    }

    /// Get a mutable reference to the source
    pub fn source_mut(&mut self) -> Option<&mut R> {
        self.uncompressed_data.as_mut()
    }

    /// Get a mutable reference to the drain
    pub fn drain_mut(&mut self) -> Option<&mut W> {
        self.compressed_data.as_mut()
    }

    /// Get a reference to the source
    pub fn source(&self) -> Option<&R> {
        self.uncompressed_data.as_ref()
    }

    /// Get a reference to the drain
    pub fn drain(&self) -> Option<&W> {
        self.compressed_data.as_ref()
    }

    /// Retrieve the source
    pub fn take_source(&mut self) -> Option<R> {
        self.uncompressed_data.take()
    }

    /// Retrieve the drain
    pub fn take_drain(&mut self) -> Option<W> {
        self.compressed_data.take()
    }

    /// Before calling [FrameCompressor::compress] you can replace the matcher
    pub fn replace_matcher(&mut self, mut match_generator: M) -> M {
        core::mem::swap(&mut match_generator, &mut self.state.matcher);
        match_generator
    }

    /// Before calling [FrameCompressor::compress] you can replace the compression level
    pub fn set_compression_level(&mut self, compression_level: Level) -> Level {
        let old = self.compression_level;
        self.compression_level = compression_level;
        old
    }

    /// Get the current compression level
    pub fn compression_level(&self) -> Level {
        self.compression_level
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::FrameCompressor;
    use crate::common::MAGIC_NUM;
    use crate::decoding::FrameDecoder;
    use alloc::vec::Vec;

    /// Every real level must roundtrip through both this crate's decoder
    /// and libzstd, keep the slice and streaming paths byte-identical, and
    /// deepen the ratio monotonically on compressible data.
    #[test]
    fn level_ladder_roundtrips() {
        let mut data = Vec::with_capacity(700 * 1024);
        let words = [
            &b"the quick brown fox "[..],
            &b"jumps over the lazy dog "[..],
            &b"lorem ipsum dolor sit amet "[..],
            b"\x00\x01\x02\x03 structured noise ",
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        while data.len() < 700 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(words[(state as usize) % words.len()]);
        }
        let levels = [
            crate::Level::Fastest,
            crate::Level::Fast,
            crate::Level::Balanced,
            crate::Level::Best,
            crate::Level::Opt,
            crate::Level::Ultra,
        ];
        let mut sizes = Vec::new();
        for level in levels {
            let compressed = super::compress_slice_to_vec(&data, level);
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            assert_eq!(
                decoder.decode_all(&compressed, &mut out).unwrap(),
                data.len()
            );
            assert_eq!(&out[..], &data[..], "roundtrip {level:?}");
            let mut libzstd = Vec::new();
            zstd::stream::copy_decode(compressed.as_slice(), &mut libzstd).unwrap();
            assert_eq!(libzstd, data, "libzstd interop {level:?}");
            let streamed = crate::encoding::compress_to_vec(data.as_slice(), level);
            assert_eq!(streamed, compressed, "slice/stream identity {level:?}");
            sizes.push((level, compressed.len()));
        }
        for pair in sizes.windows(2) {
            assert!(
                pair[1].1 <= pair[0].1,
                "ratio must not worsen from {:?} to {:?}: {} > {}",
                pair[0].0,
                pair[1].0,
                pair[0].1,
                pair[1].1
            );
        }
    }

    #[test]
    fn frame_starts_with_magic_num() {
        let mock_data = [1_u8, 2, 3].as_slice();
        let mut output: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);
        compressor.set_source(mock_data);
        compressor.set_drain(&mut output);

        compressor.compress();
        assert!(output.starts_with(&MAGIC_NUM.to_le_bytes()));
    }

    #[test]
    fn very_simple_raw_compress() {
        let mock_data = [1_u8, 2, 3].as_slice();
        let mut output: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);
        compressor.set_source(mock_data);
        compressor.set_drain(&mut output);

        compressor.compress();
    }

    #[test]
    fn very_simple_compress() {
        let mut mock_data = vec![0; 1 << 17];
        mock_data.extend(vec![1; (1 << 17) - 1]);
        mock_data.extend(vec![2; (1 << 18) - 1]);
        mock_data.extend(vec![2; 1 << 17]);
        mock_data.extend(vec![3; (1 << 17) - 1]);
        let mut output: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);
        compressor.set_source(mock_data.as_slice());
        compressor.set_drain(&mut output);

        compressor.compress();

        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::with_capacity(mock_data.len());
        decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
        assert_eq!(mock_data, decoded);

        let mut decoded = Vec::new();
        zstd::stream::copy_decode(output.as_slice(), &mut decoded).unwrap();
        assert_eq!(mock_data, decoded);
    }

    #[test]
    fn rle_compress() {
        let mock_data = vec![0; 1 << 19];
        let mut output: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);
        compressor.set_source(mock_data.as_slice());
        compressor.set_drain(&mut output);

        compressor.compress();

        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::with_capacity(mock_data.len());
        decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
        assert_eq!(mock_data, decoded);
    }

    #[test]
    #[test]
    fn fse_repeat_tables_roundtrip() {
        // Multi-block payload (>= 2 x 128 KiB) with drifting-but-similar
        // sequence statistics, so later blocks reuse earlier blocks' FSE
        // tables (repeat mode). Both decoders must reproduce it exactly.
        let mut data = alloc::vec![];
        let template = b"{\"id\":123456,\"name\":\"user\",\"tags\":[\"a\",\"b\"],\"score\":42}\n";
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        while data.len() < 600 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(template);
            data.push(b'0' + (state % 10) as u8);
            data.push(b'0' + ((state >> 8) % 10) as u8);
        }
        let output = crate::encoding::compress_slice_to_vec(&data[..], crate::Level::Fastest);

        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::with_capacity(data.len());
        decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
        assert_eq!(data, decoded);

        let mut decoded = Vec::new();
        zstd::stream::copy_decode(output.as_slice(), &mut decoded).unwrap();
        assert_eq!(data, decoded);
    }

    #[test]
    fn aaa_compress() {
        let mock_data = vec![0, 1, 3, 4, 5];
        let mut output: Vec<u8> = Vec::new();
        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);
        compressor.set_source(mock_data.as_slice());
        compressor.set_drain(&mut output);

        compressor.compress();

        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::with_capacity(mock_data.len());
        decoder.decode_all_to_vec(&output, &mut decoded).unwrap();
        assert_eq!(mock_data, decoded);

        let mut decoded = Vec::new();
        zstd::stream::copy_decode(output.as_slice(), &mut decoded).unwrap();
        assert_eq!(mock_data, decoded);
    }

    #[cfg(feature = "hash")]
    #[test]
    fn checksum_two_frames_reused_compressor() {
        // Compress the same data twice using the same compressor and verify that:
        // 1. The checksum written in each frame matches what the decoder calculates.
        // 2. The hasher is correctly reset between frames (no cross-contamination).
        //    If the hasher were NOT reset, the second frame's calculated checksum
        //    would differ from the one stored in the frame data, causing assert_eq to fail.
        let data: Vec<u8> = (0u8..=255).cycle().take(1024).collect();

        let mut compressor = FrameCompressor::new(crate::Level::Uncompressed);

        // --- Frame 1 ---
        let mut compressed1 = Vec::new();
        compressor.set_source(data.as_slice());
        compressor.set_drain(&mut compressed1);
        compressor.compress();

        // --- Frame 2 (reuse the same compressor) ---
        let mut compressed2 = Vec::new();
        compressor.set_source(data.as_slice());
        compressor.set_drain(&mut compressed2);
        compressor.compress();

        fn decode_and_collect(compressed: &[u8]) -> (Vec<u8>, Option<u32>, Option<u32>) {
            let mut decoder = FrameDecoder::new();
            let mut source = compressed;
            decoder.reset(&mut source).unwrap();
            while !decoder.is_finished() {
                decoder
                    .decode_blocks(&mut source, crate::decoding::BlockDecodingStrategy::All)
                    .unwrap();
            }
            let mut decoded = Vec::new();
            decoder.collect_to_writer(&mut decoded).unwrap();
            (
                decoded,
                decoder.get_checksum_from_data(),
                decoder.get_calculated_checksum(),
            )
        }

        let (decoded1, chksum_from_data1, chksum_calculated1) = decode_and_collect(&compressed1);
        assert_eq!(decoded1, data, "frame 1: decoded data mismatch");
        assert_eq!(
            chksum_from_data1, chksum_calculated1,
            "frame 1: checksum mismatch"
        );

        let (decoded2, chksum_from_data2, chksum_calculated2) = decode_and_collect(&compressed2);
        assert_eq!(decoded2, data, "frame 2: decoded data mismatch");
        assert_eq!(
            chksum_from_data2, chksum_calculated2,
            "frame 2: checksum mismatch"
        );

        // Same data compressed twice must produce the same checksum.
        // If state leaked across frames, the second calculated checksum would differ.
        assert_eq!(
            chksum_from_data1, chksum_from_data2,
            "frame 1 and frame 2 should have the same checksum (same data, hash must reset per frame)"
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn fuzz_targets() {
        use std::io::Read;
        fn decode_zstdx(data: &mut dyn std::io::Read) -> Vec<u8> {
            let mut decoder = crate::decoding::StreamingDecoder::new(data).unwrap();
            let mut result: Vec<u8> = Vec::new();
            decoder.read_to_end(&mut result).expect("Decoding failed");
            result
        }

        fn decode_zstdx_writer(mut data: impl Read) -> Vec<u8> {
            let mut decoder = crate::decoding::FrameDecoder::new();
            decoder.reset(&mut data).unwrap();
            let mut result = vec![];
            while !decoder.is_finished() || decoder.can_collect() > 0 {
                decoder
                    .decode_blocks(
                        &mut data,
                        crate::decoding::BlockDecodingStrategy::UptoBytes(1024 * 1024),
                    )
                    .unwrap();
                decoder.collect_to_writer(&mut result).unwrap();
            }
            result
        }

        fn encode_zstd(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
            zstd::stream::encode_all(std::io::Cursor::new(data), 3)
        }

        fn encode_zstdx_uncompressed(data: &mut dyn std::io::Read) -> Vec<u8> {
            let mut input = Vec::new();
            data.read_to_end(&mut input).unwrap();

            crate::encoding::compress_to_vec(input.as_slice(), crate::Level::Uncompressed)
        }

        fn encode_zstdx_compressed(data: &mut dyn std::io::Read) -> Vec<u8> {
            let mut input = Vec::new();
            data.read_to_end(&mut input).unwrap();

            crate::encoding::compress_to_vec(input.as_slice(), crate::Level::Fastest)
        }

        fn decode_zstd(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
            let mut output = Vec::new();
            zstd::stream::copy_decode(data, &mut output)?;
            Ok(output)
        }
        if std::fs::exists("fuzz/artifacts/interop").unwrap_or(false) {
            for file in std::fs::read_dir("fuzz/artifacts/interop").unwrap() {
                if file.as_ref().unwrap().file_type().unwrap().is_file() {
                    let data = std::fs::read(file.unwrap().path()).unwrap();
                    let data = data.as_slice();
                    // Decoding
                    let compressed = encode_zstd(data).unwrap();
                    let decoded = decode_zstdx(&mut compressed.as_slice());
                    let decoded2 = decode_zstdx_writer(&mut compressed.as_slice());
                    assert!(
                        decoded == data,
                        "Decoded data did not match the original input during decompression"
                    );
                    assert_eq!(
                        decoded2, data,
                        "Decoded data did not match the original input during decompression"
                    );

                    // Encoding
                    // Uncompressed encoding
                    let mut input = data;
                    let compressed = encode_zstdx_uncompressed(&mut input);
                    let decoded = decode_zstd(&compressed).unwrap();
                    assert_eq!(
                        decoded, data,
                        "Decoded data did not match the original input during compression"
                    );
                    // Compressed encoding
                    let mut input = data;
                    let compressed = encode_zstdx_compressed(&mut input);
                    let decoded = decode_zstd(&compressed).unwrap();
                    assert_eq!(
                        decoded, data,
                        "Decoded data did not match the original input during compression"
                    );
                }
            }
        }
    }
}
