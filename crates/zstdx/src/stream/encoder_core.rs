//! The incremental frame-encoding core shared by the stream encoders.
//!
//! It assembles the same building blocks as [`FrameCompressor`]'s loop
//! (matcher window, block encoder, frame checksum) but is fed explicitly
//! instead of pulling from a [`Read`]: [`FrameEncoderCore::write`] stages
//! bytes, full blocks are committed and encoded as they fill, and
//! [`FrameEncoderCore::finish`] emits the last block plus checksum. With no
//! intermediate flushes the output is byte-identical to
//! [`encoding::compress`] over the same bytes.

use alloc::vec::Vec;

use crate::{
    EncoderOptions, Error, Level, Result,
    blocks::block::BlockType,
    common::MAX_BLOCK_SIZE,
    encoding::{
        Matcher,
        block_header::BlockHeader,
        blocks::compressed::BlockScratch,
        compress_fastest,
        frame_compressor::{BlockChecksum, CompressState, FrameHasher, FseTables},
        frame_header::FrameHeader,
        match_generator::MatchGeneratorDriver,
        reach_probe, util,
    },
};

/// Checksum backend that can be switched off at runtime: the streaming API
/// exposes the checksum as an option, so unlike the one-shot paths (which fix
/// it per feature set) the no-op arm must exist even with `hash` enabled.
pub(crate) enum StreamChecksum {
    On(FrameHasher),
    Off,
}

impl BlockChecksum for StreamChecksum {
    #[inline]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        match self {
            Self::On(h) => h.scan_block(data),
            Self::Off => (util::is_uniform(data), 0),
        }
    }

    #[inline]
    fn hash_tail(&mut self, bytes: &[u8]) {
        if let Self::On(h) = self {
            h.hash_tail(bytes);
        }
    }

    #[inline]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize) {
        match self {
            Self::On(h) => h.raw_out(out, bytes, from),
            Self::Off => out.extend_from_slice(bytes),
        }
    }

    #[inline]
    fn finish32(&mut self) -> u32 {
        match self {
            Self::On(h) => h.finish32(),
            Self::Off => 0,
        }
    }
}

pub(crate) struct FrameEncoderCoreSt {
    state: CompressState<MatchGeneratorDriver>,
    hasher: StreamChecksum,
    level: Level,
    checksum: bool,
    /// Block size in effect: the 128 KiB format maximum, capped by the
    /// declared window (a forced or downsized window below it shrinks
    /// blocks; RFC 8878 Block_Maximum_Size).
    block_size: usize,
    header: Vec<u8>,
    /// Input bytes of the block currently being assembled; always shorter
    /// than the block size outside [`FrameEncoderCoreSt::write`].
    staged: Vec<u8>,
    /// Whether the frame's reach probe (see [`reach_probe`]) is still
    /// pending: the first [`reach_probe::PROBE_SPAN`] bytes stage as one
    /// contiguous unit before the first block is matched.
    probe_pending: bool,
    /// Encoded bytes not yet consumed by the enclosing encoder, starting
    /// with the frame header before the first block. `out_read` is the
    /// consumed prefix: serving reads advances the cursor instead of
    /// draining, which would shift every remaining byte left per call.
    output: Vec<u8>,
    out_read: usize,
    blocks: u64,
    finished: bool,
}

impl FrameEncoderCoreSt {
    pub(crate) fn new(options: &EncoderOptions) -> Self {
        Self::new_with_dictionary(options, None).expect("no dictionary to fail")
    }

    /// The dictionary variant is fallible (parse errors); without one this
    /// reduces to the plain constructor.
    pub(crate) fn new_with_dictionary(
        options: &EncoderOptions,
        dict: Option<&crate::encoding::dictionary::EncDictionary>,
    ) -> Result<Self> {
        // Owned-window driver like FrameCompressor::new: the streaming core
        // feeds blocks through block_tail/commit_block, so unlike the slice
        // path (new_direct, borrowed window) the matcher must own its window.
        let mut state = CompressState {
            dict_entropy: Default::default(),
            matcher: MatchGeneratorDriver::new(MAX_BLOCK_SIZE as usize),
            last_huff_table: None,
            fse_tables: FseTables::new(),
            scratch: BlockScratch::default(),
        };
        let dict_id = match dict {
            Some(dict) => {
                let mut shape = crate::InputShape {
                    len: options.pledged_size,
                    window_log: options.input_shape.window_log,
                };
                // libzstd clamps the window by src + dict.
                shape.len = Some(shape.len.unwrap_or(0) + dict.content.len() as u64);
                crate::encoding::dictionary::reset_with_dictionary(
                    &mut state,
                    dict,
                    options.level,
                    shape,
                );
                dict.header_id()
            },
            None => {
                let shape = crate::InputShape {
                    len: options.pledged_size,
                    window_log: options.input_shape.window_log,
                };
                state.matcher.set_input_shape(shape);
                state.matcher.reset(options.level);
                None
            },
        };
        let checksum = options.checksum && cfg!(feature = "hash");
        // Dictionary frames keep the stock reach (see reach_probe), so only
        // a plain frame stages the probe's head.
        let probe_pending = dict_id.is_none()
            && reach_probe::eligible(options.level, crate::InputShape {
                len: options.pledged_size,
                window_log: options.input_shape.window_log,
            });
        let header = FrameHeader {
            frame_content_size: options.pledged_size,
            single_segment: false,
            content_checksum: checksum,
            dictionary_id: dict_id,
            window_size: Some(state.matcher.window_size()),
        };
        let mut serialized = Vec::with_capacity(18);
        header.serialize(&mut serialized);
        let block_size = state.matcher.block_size();
        Ok(Self {
            state,
            hasher: if checksum {
                StreamChecksum::On(FrameHasher::new())
            } else {
                StreamChecksum::Off
            },
            level: options.level,
            checksum,
            block_size,
            header: serialized,
            staged: Vec::with_capacity(MAX_BLOCK_SIZE as usize),
            probe_pending,
            output: Vec::with_capacity(MAX_BLOCK_SIZE as usize + 64),
            out_read: 0,
            blocks: 0,
            finished: false,
        })
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        debug_assert!(!self.finished);
        let mut data = data;
        while !data.is_empty() {
            // While the reach probe is pending (see reach_probe), the
            // frame's first PROBE_SPAN bytes stage as one contiguous unit —
            // the probe parses them as a whole before any block is matched.
            let limit = if self.probe_pending {
                reach_probe::PROBE_SPAN
            } else {
                self.block_size
            };
            let space = limit - self.staged.len();
            let n = space.min(data.len());
            self.staged.extend_from_slice(&data[..n]);
            data = &data[n..];
            if self.staged.len() == limit {
                if self.probe_pending {
                    self.state
                        .matcher
                        .consider_reach_probe(&self.staged, self.level);
                    self.probe_pending = false;
                }
                self.drain_full_blocks();
            }
        }
    }

    /// Encode every full staged block. Outside the probe's staging window
    /// this is the plain per-block encode; after it, the staged head drains
    /// in block-sized pieces.
    fn drain_full_blocks(&mut self) {
        while self.staged.len() >= self.block_size {
            self.encode_block(false);
        }
    }

    /// Close the frame: the staged bytes (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        // A frame needs at least one block: empty input, and an input that
        // is an exact multiple of the block size, encode one empty raw last
        // block (mirroring the legacy streaming path).
        //
        // A still-pending probe means the frame never staged its head (a
        // short input or an early finish): the stock reach stays.
        self.probe_pending = false;
        self.drain_full_blocks();
        self.encode_block(true);
        if self.checksum {
            let checksum = self.hasher.finish32();
            self.output.extend_from_slice(&checksum.to_le_bytes());
        }
        self.finished = true;
    }

    /// Emit the staged bytes early as a non-last block. A no-op when nothing
    /// is staged.
    pub(crate) fn flush_block(&mut self) {
        debug_assert!(!self.finished);
        // A flush inside the probe's staging window cancels the probe (the
        // stock reach stays) and emits the staged head block by block.
        self.probe_pending = false;
        while !self.staged.is_empty() {
            self.encode_block(false);
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn has_output(&self) -> bool {
        self.out_read < self.output.len()
    }

    /// Hand the encoded bytes to `w`, keeping the output buffer's allocation.
    pub(crate) fn write_output_to(
        &mut self,
        w: &mut impl crate::io::Write,
    ) -> Result<(), crate::io::Error> {
        let res = w.write_all(&self.output[self.out_read..]);
        self.output.clear();
        self.out_read = 0;
        res
    }

    /// Serve encoded bytes without deallocating the output buffer.
    pub(crate) fn split_output(&mut self, buf: &mut [u8]) -> usize {
        let pending = &self.output[self.out_read..];
        let n = buf.len().min(pending.len());
        buf[..n].copy_from_slice(&pending[..n]);
        self.out_read += n;
        if self.out_read == self.output.len() {
            self.output.clear();
            self.out_read = 0;
        }
        n
    }

    fn encode_block(&mut self, last: bool) {
        // At most one block's worth: outside the probe's staging window the
        // staged bytes are always shorter than the block size.
        let n = self.staged.len().min(self.block_size);
        debug_assert!(n <= MAX_BLOCK_SIZE as usize);
        let tail = self.state.matcher.block_tail();
        tail[..n].copy_from_slice(&self.staged[..n]);
        self.state.matcher.commit_block(n);
        self.staged.drain(..n);
        if self.blocks == 0 {
            self.output.extend_from_slice(&self.header);
        }
        self.blocks += 1;
        // An empty block only occurs as the frame-closing block (empty
        // input or an exact block-size multiple) and is always raw; the
        // compressed path has never seen a zero-byte block.
        if self.level != Level::Uncompressed && n > 0 {
            compress_fastest(&mut self.state, last, &mut self.output, &mut self.hasher);
        } else {
            let header = BlockHeader {
                last_block: last,
                block_type: BlockType::Raw,
                block_size: n as u32,
            };
            header.serialize(&mut self.output);
            self.hasher
                .raw_out(&mut self.output, self.state.matcher.get_last_space(), 0);
        }
    }
}

/// The incremental frame-encoding core shared by the stream encoders: the
/// single-threaded block loop, or the burst-parallel job core when more than
/// one worker is available (std builds, a compressing level, more than one
/// usable core). Both expose the same interface, so the enclosing encoders
/// are oblivious to the choice. The single-threaded core is boxed: its
/// inline buffers dominate the enum, and the indirection keeps encoder
/// moves cheap; the mt core is a handful of scalars and Vecs and stays
/// inline.
#[allow(clippy::large_enum_variant)]
pub(crate) enum FrameEncoderCore {
    Single(alloc::boxed::Box<FrameEncoderCoreSt>),
    #[cfg(feature = "std")]
    Mt(super::encoder_mt::MtEncoderCore),
}

impl FrameEncoderCore {
    // Fallible only for an invalid dictionary (no_std + workers > 1 keeps
    // its Unsupported error; a dictionary forces the single-threaded core).
    pub(crate) fn new(options: &EncoderOptions) -> Result<Self> {
        if let Some(raw) = &options.dictionary {
            let dict = crate::encoding::dictionary::EncDictionary::parse(raw)?;
            return Ok(Self::Single(alloc::boxed::Box::new(
                FrameEncoderCoreSt::new_with_dictionary(options, Some(&dict))?,
            )));
        }
        if options.workers > 1 {
            // Same engagement conditions as the bulk mt path: raw-block
            // levels and single-core processes run the single-threaded core
            // instead.
            #[cfg(feature = "std")]
            if options.level != Level::Uncompressed
                && std::thread::available_parallelism().is_ok_and(|n| n.get() >= 2)
            {
                return Ok(Self::Mt(super::encoder_mt::MtEncoderCore::new(options)));
            }
            #[cfg(feature = "std")]
            return Ok(Self::Single(alloc::boxed::Box::new(
                FrameEncoderCoreSt::new(options),
            )));
            #[cfg(not(feature = "std"))]
            return Err(Error::Unsupported {
                feature: crate::Feature::Multithread,
            });
        }
        Ok(Self::Single(alloc::boxed::Box::new(
            FrameEncoderCoreSt::new(options),
        )))
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        match self {
            Self::Single(core) => core.write(data),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.write(data),
        }
    }

    /// Close the frame: the staged bytes (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        match self {
            Self::Single(core) => core.finish(),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.finish(),
        }
    }

    /// Emit the staged bytes early as a non-last block. A no-op when nothing
    /// is staged.
    pub(crate) fn flush_block(&mut self) {
        match self {
            Self::Single(core) => core.flush_block(),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.flush_block(),
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        match self {
            Self::Single(core) => core.is_finished(),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.is_finished(),
        }
    }

    pub(crate) fn has_output(&self) -> bool {
        match self {
            Self::Single(core) => core.has_output(),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.has_output(),
        }
    }

    /// Pull once from `source` and feed the encoder: the Read-side pump.
    /// Under std the mt core reads straight into its accumulate buffer's
    /// initialized spare capacity — one serial copy per byte instead of the
    /// staging chunk's two (the read path's dominant cost on the fastest
    /// tiers, where the encode span no longer hides it); the
    /// single-threaded core keeps the staging chunk (its pump hides behind
    /// the encode span, and its block-staged buffer is small).
    pub(crate) fn pump(
        &mut self,
        source: &mut impl crate::io::Read,
        chunk: &mut [u8],
    ) -> Result<()> {
        #[cfg(feature = "std")]
        {
            if let Self::Mt(core) = self {
                return core.pump_direct(source);
            }
        }
        self.pump_from(source, chunk)
    }

    /// Pull once from `source` into `chunk` and feed the encoder: returns
    /// after every source read, so the caller's loop decides how eagerly to
    /// drain. The staging chunk is caller-owned so it is zeroed once per
    /// encoder, not once per pull.
    pub(crate) fn pump_from(
        &mut self,
        source: &mut impl crate::io::Read,
        chunk: &mut [u8],
    ) -> Result<()> {
        if self.is_finished() {
            return Ok(());
        }
        match source.read(chunk).map_err(Error::from) {
            Ok(0) => self.finish(),
            Ok(n) => self.write(&chunk[..n]),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Hand the encoded bytes to `w`, keeping the output buffer's allocation.
    pub(crate) fn write_output_to(
        &mut self,
        w: &mut impl crate::io::Write,
    ) -> Result<(), crate::io::Error> {
        match self {
            Self::Single(core) => core.write_output_to(w),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.write_output_to(w),
        }
    }

    /// Serve encoded bytes without deallocating the output buffer.
    pub(crate) fn split_output(&mut self, buf: &mut [u8]) -> usize {
        match self {
            Self::Single(core) => core.split_output(buf),
            #[cfg(feature = "std")]
            Self::Mt(core) => core.split_output(buf),
        }
    }
}
