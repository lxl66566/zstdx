//! `ZSTD_DCtx`: the decode-side parameters and the streaming-decode state
//! behind `ZSTD_decompressStream`.
//!
//! The streaming machine stages unconsumed input in the context (libzstd
//! does the same through its internal `inBuff`): input is accepted when a
//! full block (plus the checksum trailer behind a last block) is staged,
//! decoded one block per step through the public `FrameDecoder` API, and
//! drained into the caller's buffer as far as it fits. Frame boundaries
//! consume skippable frames and roll into the next frame, so concatenated
//! streams decode transparently; the return value is 0 exactly when a frame
//! completed and everything decoded from it has been flushed.

use zstdx::decoding::{BlockDecodingStrategy, FrameDecoder};

use crate::{
    ZSTD_inBuffer, ZSTD_outBuffer,
    error::{self, DecodeSite, ErrorCode},
};

/// `ZSTD_DStreamOutSize`: one block of output; also the "more to do" hint.
pub const DSTREAM_OUT_SIZE: usize = 128 * 1024;
/// `ZSTD_DStreamInSize`: one block plus its header.
pub const DSTREAM_IN_SIZE: usize = DSTREAM_OUT_SIZE + 3;
/// Input staged before the staging buffer is compacted (one memmove per this
/// many consumed bytes; the live tail stays bounded by a block extent).
const COMPACT_LIMIT: usize = 512 * 1024;
/// Input handed to the decoder per pump step.
const FEED_CHUNK: usize = 128 * 1024;

/// The sticky decode parameters of a context (libzstd defaults).
#[derive(Clone, Default)]
pub struct Settings {
    /// `ZSTD_d_windowLogMax` (bytes are `1 << log`); the engine clamps to
    /// the format maximum.
    pub window_log_max: Option<u32>,
    pub dict: Option<Vec<u8>>,
}

impl Settings {
    /// Validate and apply one `ZSTD_DCtx_setParameter` pair.
    pub fn set_parameter(&mut self, param: i32, value: i32) -> Result<(), ErrorCode> {
        match crate::ZSTD_dParameter::try_from(param)? {
            crate::ZSTD_dParameter::WindowLogMax => {
                // 0 means the default limit (2^27); the format maximum is 2^41.
                if value != 0 && !(10..=41).contains(&value) {
                    return Err(ErrorCode::ParameterOutOfBound);
                }
                self.window_log_max =
                    (value != 0).then_some(u32::try_from(value).expect("bounds checked"));
            },
        }
        Ok(())
    }
}

/// One-shot decompression of `src` into `dst`; returns the written length.
pub fn decompress_oneshot(
    settings: &Settings,
    src: &[u8],
    dst: &mut [u8],
) -> Result<usize, ErrorCode> {
    let mut options = zstdx::DecoderOptions::new();
    if let Some(log) = settings.window_log_max {
        options = options.max_window_size(1 << log);
    }
    if let Some(dict) = &settings.dict {
        options = options.dictionary(dict);
    }
    zstdx::bulk::decompress_to_buffer_with(src, dst, &options)
        .map_err(|e| error::decode(&e, DecodeSite::OneShot))
}

/// Live streaming-decoder state inside a DCtx.
struct DStream {
    inner: FrameDecoder,
    /// Compressed bytes that arrived but were not consumed yet;
    /// `input[..start]` is already decoded.
    input: Vec<u8>,
    start: usize,
    /// False until a frame header was parsed; reset at every frame boundary.
    inited: bool,
    /// Whether the frame parsed last announced a content checksum (the
    /// trailer must be staged with the last block).
    checksummed: bool,
    /// Decoded output not yet handed to the caller; `delivered` counts what
    /// left the sink.
    sink: Vec<u8>,
    delivered: usize,
    /// Frames completed on this stream; the final "fully flushed" answer
    /// (0) requires at least one, so a stream of only skippable frames
    /// never reports completion.
    frames_done: u32,
}

impl DStream {
    fn new(settings: &Settings) -> Result<Self, ErrorCode> {
        let mut inner = FrameDecoder::new();
        if let Some(log) = settings.window_log_max {
            inner.set_max_window_size(1_u64 << log);
        }
        if let Some(dict) = &settings.dict {
            let dict = zstdx::decoding::Dictionary::load(dict)
                .map_err(|_| ErrorCode::DictionaryCorrupted)?;
            inner.add_dict(dict).map_err(|_| ErrorCode::Generic)?;
        }
        Ok(Self {
            inner,
            input: Vec::new(),
            start: 0,
            inited: false,
            checksummed: false,
            sink: Vec::new(),
            delivered: 0,
            frames_done: 0,
        })
    }

    fn staged(&self) -> &[u8] {
        &self.input[self.start..]
    }

    /// Drop the consumed prefix; the steady state only advances the cursor.
    fn compact(&mut self) {
        if self.start == self.input.len() || self.start >= COMPACT_LIMIT {
            self.input.drain(..self.start);
            self.start = 0;
        }
    }

    /// Copy staged output into the caller's buffer.
    fn deliver(&mut self, out: &mut [u8], pos: &mut usize) {
        let take = (self.sink.len() - self.delivered).min(out.len() - *pos);
        out[*pos..*pos + take].copy_from_slice(&self.sink[self.delivered..self.delivered + take]);
        *pos += take;
        self.delivered += take;
        if self.delivered == self.sink.len() {
            self.sink.clear();
            self.delivered = 0;
        }
    }

    /// Decode as much staged input as the block granularity allows.
    fn pump(&mut self) -> Result<(), ErrorCode> {
        loop {
            if !self.inited {
                if !self.init_frame()? {
                    return Ok(());
                }
                continue;
            }
            if self.inner.is_finished() {
                self.inner
                    .collect_to_writer(&mut self.sink)
                    .map_err(|_| ErrorCode::Generic)?;
                self.frames_done += 1;
                if self.start == self.input.len() {
                    self.compact();
                    return Ok(());
                }
                self.inited = false;
                continue;
            }
            match self.next_block_need()? {
                Some(need) if self.staged().len() >= need => {},
                _ => return Ok(()),
            }
            let read_before = self.inner.bytes_read_from_source();
            // Direct field slices: the source borrows `input` while
            // `decode_blocks` mutates `inner` (disjoint fields).
            let mut source: &[u8] = &self.input[self.start..];
            self.inner
                .decode_blocks(&mut source, BlockDecodingStrategy::UptoBlocks(1))
                .map_err(|e| error::frame(&e, DecodeSite::Stream))?;
            let consumed = (self.inner.bytes_read_from_source() - read_before) as usize;
            self.start += consumed;
            self.compact();
            self.inner
                .collect_to_writer(&mut self.sink)
                .map_err(|_| ErrorCode::Generic)?;
        }
    }

    /// Bytes the next block needs before it can be decoded safely (header
    /// plus body plus, behind the last block of a checksummed frame, the
    /// trailer): `None` when too little is staged to even parse the header.
    fn next_block_need(&self) -> Result<Option<usize>, ErrorCode> {
        let staged: &[u8] = &self.input[self.start..];
        let [b0, b1, b2, ..] = *staged else {
            return Ok(None);
        };
        let last = b0 & 1 == 1;
        let block_type = (b0 >> 1) & 3;
        let size = (usize::from(b0) >> 3) | (usize::from(b1) << 5) | (usize::from(b2) << 13);
        let body = match block_type {
            0 | 2 => size, // Raw / Compressed carry `size` content bytes
            1 => 1,        // RLE encodes a single byte
            _ => return Err(ErrorCode::CorruptionDetected),
        };
        let trailer = if last && self.checksummed {
            4
        } else {
            0
        };
        Ok(Some(3 + body + trailer))
    }

    /// Parse the next frame header from the staged input, consuming
    /// skippable frames on the way. `Ok(false)` means more input is needed.
    fn init_frame(&mut self) -> Result<bool, ErrorCode> {
        loop {
            if self.staged().len() < 4 {
                return Ok(false);
            }
            match crate::frame::parse_header(self.staged()) {
                Ok(crate::frame::Header::Skippable { total }) => {
                    if self.staged().len() < total {
                        return Ok(false);
                    }
                    self.start += total;
                    self.compact();
                },
                Ok(crate::frame::Header::Zstd(header)) => {
                    if self.staged().len() < header.header_len {
                        return Ok(false);
                    }
                    self.checksummed = header.checksum;
                    let mut source: &[u8] = &self.input[self.start..];
                    self.inner
                        .reset(&mut source)
                        .map_err(|e| error::frame(&e, DecodeSite::Stream))?;
                    let consumed = self.input.len() - self.start - source.len();
                    self.start += consumed;
                    self.compact();
                    self.inited = true;
                    return Ok(true);
                },
                Err(crate::frame::ParseError::NeedMore) => return Ok(false),
                Err(crate::frame::ParseError::BadMagic) => {
                    return Err(ErrorCode::PrefixUnknown);
                },
                Err(crate::frame::ParseError::Malformed) => {
                    return Err(ErrorCode::PrefixUnknown);
                },
            }
        }
    }
}

/// The `ZSTD_DCtx` object.
pub struct DCtx {
    pub settings: Settings,
    stream: Option<DStream>,
    /// Sticky first error (libzstd calls a failed stream undefined; this
    /// layer replays the code until a reset).
    sticky: Option<ErrorCode>,
}

impl DCtx {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            stream: None,
            sticky: None,
        }
    }

    /// `ZSTD_initDStream`: session reset (per the legacy doc contract the
    /// dictionary reference is cleared; parameters survive).
    pub fn init_stream(&mut self) {
        self.stream = None;
        self.settings.dict = None;
        self.sticky = None;
    }

    /// `ZSTD_decompressStream` core; the return is the libzstd hint
    /// (0 when a frame is completely decoded and fully flushed).
    pub fn decompress_stream(
        &mut self,
        output: &mut ZSTD_outBuffer,
        input: &mut ZSTD_inBuffer,
    ) -> Result<usize, ErrorCode> {
        if let Some(sticky) = self.sticky {
            return Err(sticky);
        }
        if self.stream.is_none() {
            match DStream::new(&self.settings) {
                Ok(stream) => self.stream = Some(stream),
                Err(e) => {
                    self.sticky = Some(e);
                    return Err(e);
                },
            }
        }
        let stream = self.stream.as_mut().expect("just ensured");
        let out = unsafe { crate::as_mut_slice(output.dst, output.size) };
        let mut in_pos = input.pos;
        loop {
            stream.deliver(out, &mut output.pos);
            if output.pos == output.size || in_pos == input.size {
                break;
            }
            let take = (input.size - in_pos).min(FEED_CHUNK);
            stream
                .input
                .extend_from_slice(unsafe { input.slice_at(in_pos, take) });
            in_pos += take;
            input.pos = in_pos;
            stream.pump()?;
        }
        stream.deliver(out, &mut output.pos);
        input.pos = in_pos;
        if stream.sink.len() > stream.delivered {
            // Output the caller's buffer could not fit yet.
            return Ok(stream.sink.len() - stream.delivered);
        }
        let frame_done = stream.inited
            && stream.inner.is_finished()
            && stream.inner.can_collect() == 0
            && stream.start == stream.input.len();
        // Past the last frame boundary (skippable frames trailing, or a
        // frame boundary that emptied the staging) the same completion
        // holds with no frame in progress.
        let idle_done =
            !stream.inited && stream.frames_done > 0 && stream.start == stream.input.len();
        if frame_done || idle_done {
            return Ok(0);
        }
        // More input or output will advance the frame; the hint is the
        // block-sized suggestion libzstd gives.
        Ok(DSTREAM_OUT_SIZE)
    }

    pub fn record_error(&mut self, code: ErrorCode) {
        self.sticky = Some(code);
    }
}

impl Default for DCtx {
    fn default() -> Self {
        Self::new()
    }
}
