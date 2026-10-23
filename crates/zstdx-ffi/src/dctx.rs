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
//!
//! The context dictionary is one parsed instance shared with every decoder
//! built from the context (and with `DDict` handles): a `usingDict` call
//! parses into the slot, `loadDictionary` does the same, and each stream
//! re-arm hands the `FrameDecoder` a clone of the share.

use std::sync::Arc;

use zstdx::decoding::{BlockDecodingStrategy, FrameDecoder};

use crate::{
    ZSTD_ResetDirective, ZSTD_inBuffer, ZSTD_outBuffer,
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

/// A parsed decoding dictionary shared between the context, its streams and
/// any `ZSTD_DDict` handle.
pub type SharedDict = Arc<zstdx::decoding::Dictionary>;

/// The sticky decode parameters of a context (libzstd defaults).
#[derive(Clone, Default)]
pub struct Settings {
    /// `ZSTD_d_windowLogMax` (bytes are `1 << log`); the engine clamps to
    /// the format maximum.
    pub window_log_max: Option<u32>,
}

impl Settings {
    /// Validate and apply one `ZSTD_DCtx_setParameter` pair.
    pub fn set_parameter(&mut self, param: i32, value: i32) -> Result<(), ErrorCode> {
        match crate::ZSTD_dParameter::try_from(param)? {
            crate::ZSTD_dParameter::WindowLogMax => {
                // 0 means the default limit (2^27); the accepted range is
                // the reference dParam bound, ZSTD_WINDOWLOG_ABSOLUTEMIN..=
                // ZSTD_WINDOWLOG_MAX_64 (the format maximum 2^41 stays out).
                if value != 0 && !(10..=31).contains(&value) {
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
/// `dict` is the dictionary for this call (None decodes plain).
pub fn decompress_oneshot(
    settings: &Settings,
    dict: Option<&SharedDict>,
    src: &[u8],
    dst: &mut [u8],
) -> Result<usize, ErrorCode> {
    let mut options = zstdx::DecoderOptions::new();
    if let Some(log) = settings.window_log_max {
        options = options.max_window_size(1 << log);
    }
    if let Some(dict) = dict {
        options = options.parsed_dictionary(Arc::clone(dict));
    }
    zstdx::bulk::decompress_to_buffer_with(src, dst, &options).map_err(|e| {
        // Below the prefix size the reference loop never starts (its
        // startingInputLength gate): any failure with that little input
        // is the leftover-input srcSize_wrong.
        if src.len() < crate::frame::HEADER_PREFIX {
            return ErrorCode::SrcSizeWrong;
        }
        if let zstdx::Error::Frame(
            zstdx::decoding::errors::FrameDecoderError::ReadFrameHeaderError(header_error),
        ) = &e
        {
            use zstdx::decoding::errors::ReadFrameHeaderError as Rhe;
            match header_error {
                // The reference one-shot loop judges magic only after its
                // decompressFrame precondition (a visible header + block
                // header in the remaining input); shorter garbage tails
                // are srcSize_wrong, not prefix_unknown.
                Rhe::BadMagicNumber(_) => return bad_magic_tail_code(src),
                // A magic read that fails on the tail the frame walk
                // leaves below the prefix size is the series' leftover
                // input - the reference loop never starts on it.
                Rhe::MagicNumberReadError(_)
                    if frame_series_tail(src).len() < crate::frame::HEADER_PREFIX =>
                {
                    return ErrorCode::SrcSizeWrong
                },
                _ => {},
            }
        }
        error::decode(&e, DecodeSite::OneShot)
    })
}

/// Walk every complete frame of a series (skippable ones included) and
/// return the tail where the engine stopped, plus whether any real
/// (non-skippable) frame completed in front of it. A truncated final
/// frame stays in the tail - it is leftover input, not a boundary.
fn frame_series_walk(src: &[u8]) -> (&[u8], bool) {
    let mut pos = 0;
    let mut real_frame_done = false;
    while src.len() - pos >= crate::frame::HEADER_PREFIX {
        let step = match crate::frame::frame_compressed_size(&src[pos..]) {
            Ok(size) if pos + size <= src.len() => size,
            _ => break,
        };
        real_frame_done |= !matches!(
            crate::frame::parse_header(&src[pos..]),
            Ok(crate::frame::Header::Skippable { .. })
        );
        pos += step;
    }
    (&src[pos..], real_frame_done)
}

/// The tail the frame-series walk leaves.
fn frame_series_tail(src: &[u8]) -> &[u8] {
    frame_series_walk(src).0
}

/// The reference's answer for a bad-magic failure in the one-shot loop:
/// the tail is where the engine saw the magic; the reference requires
/// header size + block header there before believing the garbage is a
/// frame (srcSize_wrong otherwise). A decoded frame in front of the tail
/// promotes even a passing tail to srcSize_wrong (the reference's
/// moreThan1Frame rule); skippable frames skip past the flag and never
/// promote.
fn bad_magic_tail_code(src: &[u8]) -> ErrorCode {
    let (tail, promoted) = frame_series_walk(src);
    if tail.len() < crate::frame::HEADER_PREFIX {
        // Below the multi-frame loop threshold: leftover input.
        return ErrorCode::SrcSizeWrong;
    }
    let want = crate::frame::header_size_formula(tail[4]) + 3;
    if tail.len() < want || promoted {
        ErrorCode::SrcSizeWrong
    } else {
        ErrorCode::PrefixUnknown
    }
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
    /// Whether the completion answer (0) already went out for the frame
    /// just finished; the next call then asks for the next frame's
    /// header, like the reference's between-frames loadHeader shortcut.
    done_reported: bool,
}

impl DStream {
    fn new(settings: &Settings, dict: Option<&SharedDict>) -> Result<Self, ErrorCode> {
        let mut inner = FrameDecoder::new();
        if let Some(log) = settings.window_log_max {
            inner.set_max_window_size(1_u64 << log);
        }
        if let Some(dict) = dict {
            // The options-based registration rule: under the dictionary's
            // own id and the null id — libzstd applies a ref'd dictionary
            // to every frame regardless of the header's dictID field, so a
            // dictIDFlag-suppressed frame decodes with it attached.
            inner
                .add_options_dict(Arc::clone(dict))
                .map_err(|_| ErrorCode::Generic)?;
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
            done_reported: false,
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

    /// Whether the stream sits on a frame boundary with nothing staged or
    /// pending: the state a parameters reset requires.
    fn at_boundary(&self) -> bool {
        self.start == self.input.len()
            && self.sink.len() == self.delivered
            && (!self.inited || self.inner.is_finished())
    }

    /// The between-frames state: a real frame finished, everything
    /// decoded from it was flushed, nothing is staged. The completion
    /// answer (0) is given once; afterwards this is the state the
    /// reference answers from its loadHeader shortcut, which also sits
    /// before its no-forward-progress guard.
    fn idle_done(&self) -> bool {
        let frame_done = self.inited
            && self.inner.is_finished()
            && self.inner.can_collect() == 0
            && self.start == self.input.len();
        let boundary_idle = !self.inited && self.frames_done > 0 && self.start == self.input.len();
        self.sink.len() == self.delivered && (frame_done || boundary_idle)
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
        // The format's block maximum, the write-shaped decoder's twin
        // check: without it a corrupt 21-bit size claim makes the stream
        // stage up to 2 MiB of body bytes that never arrive instead of
        // erroring (the core would reject the block only once staged).
        if size > crate::frame::MAX_BLOCK_CONTENT {
            return Err(ErrorCode::CorruptionDetected);
        }
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
                Err(crate::frame::ParseError::BadMagic | crate::frame::ParseError::Malformed) => {
                    return Err(ErrorCode::PrefixUnknown);
                },
                // Frame-head constraints the walker can judge on its own;
                // libzstd's streaming decode reports the same codes.
                Err(crate::frame::ParseError::ReservedBit) => {
                    return Err(ErrorCode::FrameParameterUnsupported);
                },
                Err(crate::frame::ParseError::WindowTooLarge) => {
                    return Err(ErrorCode::FrameParameterWindowTooLarge);
                },
            }
        }
    }
}

/// `ZSTD_NO_FORWARD_PROGRESS_MAX`: consecutive no-progress streaming
/// calls before the reference refuses.
const NO_FORWARD_PROGRESS_MAX: u32 = 16;

/// The between-frames suggestion: the reference's loadHeader shortcut
/// returns max(ZSTD_FRAMEHEADERSIZE_MIN, hSize) - staged + blockHeaderSize
/// before its no-forward-progress guard; with nothing staged that is 9.
const NEXT_FRAME_HINT: usize = 6 + 3;

/// The `ZSTD_DCtx` object.
pub struct DCtx {
    pub settings: Settings,
    /// The context dictionary, parsed once and shared with every stream
    /// (set by `usingDict` — sticky until `initDStream` per the legacy
    /// contract — or `loadDictionary`; cleared by a parameters reset).
    pub dict: Option<SharedDict>,
    stream: Option<DStream>,
    /// Sticky first error (libzstd calls a failed stream undefined; this
    /// layer replays the code until a reset).
    sticky: Option<ErrorCode>,
    /// Consecutive streaming calls that moved neither cursor (the
    /// reference no-forward-progress guard).
    no_forward: u32,
}

impl DCtx {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            dict: None,
            stream: None,
            sticky: None,
            no_forward: 0,
        }
    }

    /// `ZSTD_DCtx_loadDictionary`: rejected mid-frame; a null or empty
    /// dictionary clears the slot. The parse is eager (the dictionary is
    /// digested once), so an unloadable dictionary fails here — libzstd
    /// digests eagerly on the decode side too.
    pub fn load_dictionary(&mut self, dict: Option<&[u8]>) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        self.dict = match dict {
            Some(bytes) => Some(Arc::new(
                zstdx::decoding::Dictionary::load(bytes)
                    .map_err(|_| ErrorCode::DictionaryCorrupted)?,
            )),
            None => None,
        };
        Ok(())
    }

    /// `ZSTD_DCtx_setParameter`: rejected while a frame is open (the
    /// reference checks its streamStage before dispatching the value).
    pub fn set_parameter(&mut self, param: i32, value: i32) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        self.settings.set_parameter(param, value)
    }

    /// `ZSTD_DCtx_reset`: session resets never fail; parameter resets fail
    /// unless the stream sits on a frame boundary. Unknown directive values
    /// are no-ops returning success, as libzstd's fall-through.
    pub fn reset(&mut self, directive: ZSTD_ResetDirective) -> Result<(), ErrorCode> {
        match directive {
            ZSTD_ResetDirective::SessionOnly => self.reset_session(),
            ZSTD_ResetDirective::Parameters => self.reset_parameters()?,
            ZSTD_ResetDirective::SessionAndParameters => {
                self.reset_session();
                self.reset_parameters()?;
            },
        }
        Ok(())
    }

    fn reset_session(&mut self) {
        self.stream = None;
        self.sticky = None;
        self.no_forward = 0;
    }

    fn reset_parameters(&mut self) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        self.settings = Settings::default();
        self.dict = None;
        Ok(())
    }

    /// `ZSTD_initDStream`: session reset (per the legacy doc contract the
    /// dictionary reference is cleared; parameters survive). The
    /// no-forward-progress guard restarts too - the reference runs
    /// ZSTD_decompressBegin under every session reset, and a DStream is
    /// its DCtx.
    pub fn init_stream(&mut self) {
        self.reset_session();
        self.dict = None;
    }

    /// `ZSTD_initDStream_usingDict`: session reset then a dictionary
    /// load; an unloadable dictionary is the return (the reference
    /// propagates ZSTD_DCtx_loadDictionary's failure).
    pub fn init_stream_with_dict(&mut self, dict: Option<&[u8]>) -> Result<(), ErrorCode> {
        self.reset_session();
        self.load_dictionary(dict)
    }

    /// `ZSTD_initDStream_usingDDict`: session reset with the handle's
    /// parsed dictionary taking the slot directly (no re-parse; a null
    /// handle clears it).
    pub fn init_stream_with_ddict(&mut self, dict: Option<&SharedDict>) {
        self.reset_session();
        self.dict = dict.cloned();
    }

    /// `ZSTD_resetDStream`: a bare session reset (the dictionary
    /// reference survives, unlike ZSTD_initDStream).
    pub fn init_stream_keep_dict(&mut self) {
        self.reset_session();
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
        let (out_start, in_start) = (output.pos, input.pos);
        let hint = self.decompress_stream_inner(output, input)?;
        if hint == 0 {
            // A completed frame restarts the guard, as the reference's
            // decompressBegin does at every frame boundary.
            self.no_forward = 0;
            return Ok(0);
        }
        if output.pos == out_start && input.pos == in_start {
            // The between-frames state returns from before the reference
            // guard (its loadHeader shortcut); those calls never count.
            let idle = self.stream.as_ref().is_some_and(|s| s.idle_done());
            if !idle {
                self.no_forward += 1;
                if self.no_forward >= NO_FORWARD_PROGRESS_MAX {
                    return Err(if output.pos == output.size {
                        ErrorCode::NoForwardProgressDestFull
                    } else {
                        ErrorCode::NoForwardProgressInputEmpty
                    });
                }
            }
        } else {
            self.no_forward = 0;
        }
        Ok(hint)
    }

    fn decompress_stream_inner(
        &mut self,
        output: &mut ZSTD_outBuffer,
        input: &mut ZSTD_inBuffer,
    ) -> Result<usize, ErrorCode> {
        if self.stream.is_none() {
            let dict = self.dict.as_ref();
            match DStream::new(&self.settings, dict) {
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
            stream.done_reported = false;
            stream.pump()?;
        }
        stream.deliver(out, &mut output.pos);
        input.pos = in_pos;
        if stream.sink.len() > stream.delivered {
            // Output the caller's buffer could not fit yet.
            return Ok(stream.sink.len() - stream.delivered);
        }
        if stream.idle_done() {
            // The completion answer goes out once per finished frame;
            // further calls want the next frame's header, like the
            // reference's between-frames loadHeader shortcut.
            if core::mem::replace(&mut stream.done_reported, true) {
                return Ok(NEXT_FRAME_HINT);
            }
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
