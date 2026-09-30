//! `ZSTD_CCtx`: the sticky parameter set, the one-shot entries built on it,
//! and the streaming-encode state behind `ZSTD_compressStream2`.
//!
//! The context owns zstdx-side state objects and never touches codec
//! internals: one-shot compression rides a streaming encoder core over a
//! staging `Vec` (the only public path that pledges the content size into
//! the frame header, as libzstd does by default), and the streaming entries
//! reuse the same encoder across calls with a delivered-cursor sink.
//!
//! libzstd's parameter model splits in two: the sticky parameters (level,
//! window, frame flags, workers) apply to `compress2` and the streaming
//! entries only — the one-shot variants (`compressCCtx`, `usingDict`,
//! `usingCDict`) run on fresh level-only settings, as `zstd.h` documents.
//! The dictionary loaded through `loadDictionary` and the pledged size set
//! through `setPledgedSrcSize` are session state: they apply to the frames
//! that follow and drop on a parameters reset.

use std::io::Write as _;

use crate::{
    ZSTD_EndDirective, ZSTD_ResetDirective, ZSTD_cParameter, ZSTD_inBuffer, ZSTD_outBuffer,
    error::{self, ErrorCode},
};

/// Input fed to the encoder per pump step: bounds the staged output to
/// roughly one block plus the caller's output capacity, mirroring how
/// libzstd's internal buffer keeps memory flat regardless of buffer sizes.
const FEED_CHUNK: usize = 128 * 1024;

/// `ZSTD_CLEVEL_DEFAULT`: FFI level 0 selects it.
const DEFAULT_CLEVEL: i32 = 3;

/// The sticky compression parameters of a context (libzstd defaults).
#[derive(Clone)]
pub struct Settings {
    /// Raw libzstd level; 0 means default, negatives clamp to 1 at use.
    pub level: i32,
    pub window_log: Option<u32>,
    pub checksum: bool,
    pub content_size: bool,
    pub workers: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            level: 0,
            window_log: None,
            // libzstd writes no frame checksum by default.
            checksum: false,
            // ...but does write the content size whenever known.
            content_size: true,
            workers: 0,
        }
    }
}

impl Settings {
    /// Fresh settings for the one-shot variants: the level argument and
    /// libzstd's frame defaults, nothing sticky.
    #[must_use]
    pub fn one_shot(level: i32) -> Self {
        Self {
            level,
            ..Self::default()
        }
    }

    /// The zstdx level for the raw FFI value: 0 selects the libzstd default
    /// (3); out-of-range values clamp (negatives to 1 — zstdx models no
    /// accelerated levels — and >22 to 22).
    #[must_use]
    pub fn level(&self) -> zstdx::Level {
        zstdx::Level::from_zstd(if self.level == 0 {
            DEFAULT_CLEVEL
        } else {
            self.level
        })
    }

    /// Snapshot into encoder options; `pledged` is the content size to
    /// declare in the frame header when `content_size` is on.
    fn encoder_options(&self, pledged: Option<u64>) -> zstdx::EncoderOptions {
        let mut options = zstdx::EncoderOptions::new(self.level())
            .checksum(self.checksum)
            .pledged_size(self.content_size.then_some(pledged).flatten())
            .workers(self.workers);
        if let Some(window_log) = self.window_log {
            options =
                options.with_input_shape(zstdx::InputShape::default().with_window_log(window_log));
        }
        options
    }

    /// Validate and apply one `ZSTD_CCtx_setParameter` pair. The return is
    /// the applied value, as in the reference tree: levels and flags clamp
    /// into range, windowLog validates 0 (default) or 10..=31, and only
    /// genuinely out-of-range requests error.
    pub fn set_parameter(
        &mut self,
        param: ZSTD_cParameter,
        value: i32,
    ) -> Result<usize, ErrorCode> {
        match param {
            ZSTD_cParameter::CompressionLevel => {
                // Reference range: 0 selects the default (and returns it),
                // other values clamp to [minCLevel, maxCLevel].
                let applied = if value == 0 {
                    DEFAULT_CLEVEL
                } else {
                    value.clamp(-131_072, 22)
                };
                self.level = applied;
                // Negative levels exist in the settings but cannot ride a
                // size_t return; the reference answers 0 for them.
                Ok(applied.max(0) as usize)
            },
            ZSTD_cParameter::WindowLog => {
                // 0 means "level default"; the engine clamps to its own
                // 10..=27 window range.
                if value != 0 && !(10..=31).contains(&value) {
                    return Err(ErrorCode::ParameterOutOfBound);
                }
                self.window_log =
                    (value != 0).then_some(u32::try_from(value).expect("bounds checked"));
                Ok(value.max(0) as usize)
            },
            ZSTD_cParameter::ChecksumFlag | ZSTD_cParameter::ContentSizeFlag => {
                let flag = value != 0;
                match param {
                    ZSTD_cParameter::ChecksumFlag => self.checksum = flag,
                    ZSTD_cParameter::ContentSizeFlag => self.content_size = flag,
                    _ => unreachable!("narrowed above"),
                }
                Ok(usize::from(flag))
            },
            ZSTD_cParameter::NbWorkers => {
                if !(0..=256).contains(&value) {
                    return Err(ErrorCode::ParameterOutOfBound);
                }
                // zstdx engages its job paths from 2 workers; a dictionary
                // rides the single-threaded core (documented engine limit).
                self.workers = u32::try_from(value).expect("bounds checked");
                Ok(value.max(0) as usize)
            },
        }
    }
}

/// The context's loaded dictionary, parsed once so every following frame
/// skips the digest. A dictionary that fails to parse is retained as a
/// pending error: libzstd digests lazily too, so the failure surfaces when
/// the next frame starts, not at the load call.
pub enum LoadedDict {
    Parsed(zstdx::EncoderDictionary),
    Invalid,
}

/// One-shot compression of `src` into `dst`; returns the written length.
/// `dict` overrides the settings' dictionary for this call only.
pub fn compress_oneshot(
    settings: &Settings,
    dict: Option<&zstdx::EncoderDictionary>,
    src: &[u8],
    dst: &mut [u8],
) -> Result<usize, ErrorCode> {
    let pledged = Some(src.len() as u64);
    let mut options = settings.encoder_options(pledged);
    if let Some(dict) = dict {
        options = options.parsed_dictionary(dict);
    }
    let sink = Vec::with_capacity(crate::compress_bound(src.len()));
    let mut encoder =
        zstdx::stream::write::Encoder::with_options(sink, options).map_err(|e| map_encode(&e))?;
    encoder.write_all(src).map_err(|e| map_io(&e))?;
    encoder.do_finish().map_err(|e| map_encode(&e))?;
    copy_out(encoder.get_ref(), dst)
}

/// Move a finished frame into the caller's buffer.
fn copy_out(compressed: &[u8], dst: &mut [u8]) -> Result<usize, ErrorCode> {
    if compressed.len() > dst.len() {
        return Err(ErrorCode::DstSizeTooSmall);
    }
    dst[..compressed.len()].copy_from_slice(compressed);
    Ok(compressed.len())
}

fn map_encode(error: &zstdx::Error) -> ErrorCode {
    error::encode(error)
}

fn map_io(error: &std::io::Error) -> ErrorCode {
    // The sink is in-memory; an I/O error is always a wrapped codec error.
    match error
        .get_ref()
        .and_then(|e| e.downcast_ref::<zstdx::Error>())
    {
        Some(inner) => error::encode(inner),
        None => ErrorCode::Generic,
    }
}

/// Live streaming-encoder state inside a CCtx.
struct StreamState {
    encoder: zstdx::stream::write::Encoder<Vec<u8>>,
    /// Bytes of `encoder`'s sink already handed to the caller.
    delivered: usize,
    /// `do_finish` ran: only draining may follow for this frame.
    frame_closed: bool,
    /// The frame is closed and fully drained; the next call opens a new one.
    finished: bool,
}

impl StreamState {
    fn new(
        settings: &Settings,
        dict: Option<&zstdx::EncoderDictionary>,
        pledged: Option<u64>,
    ) -> Result<Self, ErrorCode> {
        let mut options = settings.encoder_options(pledged);
        if let Some(dict) = dict {
            options = options.parsed_dictionary(dict);
        }
        let encoder = zstdx::stream::write::Encoder::with_options(Vec::new(), options)
            .map_err(|e| map_encode(&e))?;
        Ok(Self {
            encoder,
            delivered: 0,
            frame_closed: false,
            finished: false,
        })
    }

    fn pending(&self) -> usize {
        self.encoder.get_ref().len() - self.delivered
    }

    /// Copy staged output into the caller's buffer, dropping the sink
    /// allocation once it fully drains.
    fn deliver(&mut self, out: &mut [u8], pos: &mut usize) {
        let sink = self.encoder.get_ref();
        let take = self.pending().min(out.len() - *pos);
        out[*pos..*pos + take].copy_from_slice(&sink[self.delivered..self.delivered + take]);
        *pos += take;
        self.delivered += take;
        if self.delivered == sink.len() {
            self.encoder.get_mut().clear();
            self.delivered = 0;
        }
    }
}

/// The `ZSTD_CCtx` object.
pub struct CCtx {
    pub settings: Settings,
    /// Session dictionary (`loadDictionary`; sticky until a parameters
    /// reset or `initCStream`'s legacy clear).
    dict: Option<LoadedDict>,
    /// `ZSTD_CCtx_setPledgedSrcSize`: applies to the next frame only and is
    /// consumed when that frame completes.
    pledged: Option<u64>,
    stream: Option<StreamState>,
    /// libzstd leaves a failed context in an undefined state; this layer
    /// keeps replaying the first error until a reset.
    sticky: Option<ErrorCode>,
}

impl CCtx {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            dict: None,
            pledged: None,
            stream: None,
            sticky: None,
        }
    }

    /// The loaded dictionary, or the pending parse error to surface at the
    /// next frame.
    fn dict(&self) -> Result<Option<&zstdx::EncoderDictionary>, ErrorCode> {
        match &self.dict {
            Some(LoadedDict::Parsed(dict)) => Ok(Some(dict)),
            Some(LoadedDict::Invalid) => Err(ErrorCode::DictionaryCorrupted),
            None => Ok(None),
        }
    }

    /// `ZSTD_CCtx_setParameter`: parameters are rejected mid-frame, as in
    /// libzstd.
    pub fn set_parameter(&mut self, param: i32, value: i32) -> Result<usize, ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        let param = ZSTD_cParameter::try_from(param)?;
        self.settings.set_parameter(param, value)
    }

    /// `ZSTD_CCtx_setPledgedSrcSize`: rejected mid-frame; the pledge (any
    /// value but `ZSTD_CONTENTSIZE_UNKNOWN`) applies to the next frame.
    pub fn set_pledged(&mut self, size: u64) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.pledged = (size != u64::MAX).then_some(size);
        Ok(())
    }

    /// `ZSTD_CCtx_loadDictionary`: rejected mid-frame; a null or empty
    /// dictionary clears the slot.
    pub fn load_dictionary(&mut self, dict: Option<&[u8]>) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.dict = match dict {
            Some(bytes) => match zstdx::EncoderDictionary::parse(bytes) {
                Ok(parsed) => Some(LoadedDict::Parsed(parsed)),
                // Retained: the error surfaces when a frame tries to use it.
                Err(_) => Some(LoadedDict::Invalid),
            },
            None => None,
        };
        Ok(())
    }

    /// `ZSTD_CCtx_reset`: session resets never fail; parameter resets fail
    /// mid-frame (after a session reset in the combined form they cannot).
    /// Unknown directive values are no-ops returning success, as libzstd's
    /// fall-through.
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

    /// Drop the open frame and the error state; parameters and dictionary
    /// survive (libzstd's session reset contract).
    fn reset_session(&mut self) {
        self.stream = None;
        self.sticky = None;
        self.pledged = None;
    }

    fn reset_parameters(&mut self) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.settings = Settings::default();
        self.dict = None;
        Ok(())
    }

    /// `ZSTD_initCStream`: session reset (the legacy doc contract also
    /// clears the dictionary) plus the level.
    pub fn init_stream(&mut self, level: i32) {
        self.settings.level = level;
        self.dict = None;
        self.stream = None;
        self.sticky = None;
        self.pledged = None;
    }

    /// `ZSTD_compress2`: sticky settings and the loaded dictionary, always
    /// a fresh frame with the source size pledged (the single-round
    /// override in libzstd's init).
    pub fn compress2(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize, ErrorCode> {
        self.reset_session();
        let dict = self.dict()?;
        let settings = self.settings.clone();
        compress_oneshot(&settings, dict, src, dst)
    }

    /// `ZSTD_compressStream2` core; the return is the bytes-left-to-flush
    /// hint (0 when the requested mode completed).
    pub fn compress_stream2(
        &mut self,
        output: &mut ZSTD_outBuffer,
        input: &mut ZSTD_inBuffer,
        mode: ZSTD_EndDirective,
    ) -> Result<usize, ErrorCode> {
        if let Some(sticky) = self.sticky {
            return Err(sticky);
        }
        // A finished frame retires; the next call opens a fresh one, as
        // ZSTD_compressStream2 does not require an explicit reset.
        if self.stream.as_ref().is_some_and(|s| s.finished) {
            self.stream = None;
            // The pledge (if any) was consumed by the finished frame.
            self.pledged = None;
        }
        if self.stream.is_none() && mode == ZSTD_EndDirective::Continue && input.pos == input.size {
            // Nothing to stage and nothing to flush: a no-op call, avoiding
            // the pointless construction of an encoder for pure polling.
            output.pos = output.pos.min(output.size);
            input.pos = input.pos.min(input.size);
            return Ok(0);
        }
        if self.stream.is_none() {
            // libzstd overrides the pledge with the input size whenever the
            // frame opens on an e_end call (its "single round" rule).
            let pledged = if mode == ZSTD_EndDirective::End {
                Some((input.size - input.pos) as u64)
            } else {
                self.pledged
            };
            let dict = self.dict()?;
            match StreamState::new(&self.settings, dict, pledged) {
                Ok(state) => self.stream = Some(state),
                Err(e) => {
                    self.sticky = Some(e);
                    return Err(e);
                },
            }
        }
        let state = self.stream.as_mut().expect("just ensured");

        let out = unsafe { crate::as_mut_slice(output.dst, output.size) };
        let mut in_pos = input.pos;
        loop {
            state.deliver(out, &mut output.pos);
            if output.pos == output.size || in_pos == input.size || state.frame_closed {
                break;
            }
            let take = (input.size - in_pos).min(FEED_CHUNK);
            state
                .encoder
                .write_all(unsafe { input.slice_at(in_pos, take) })
                .map_err(|e| map_io(&e))?;
            in_pos += take;
            input.pos = in_pos;
        }
        match mode {
            ZSTD_EndDirective::Continue => {},
            ZSTD_EndDirective::Flush => {
                // Emit the staged bytes as a non-last block; idempotent
                // across the calls it takes to drain the output.
                state.encoder.flush().map_err(|e| map_io(&e))?;
            },
            ZSTD_EndDirective::End => {
                if !state.frame_closed {
                    state.encoder.do_finish().map_err(|e| map_encode(&e))?;
                    state.frame_closed = true;
                }
            },
        }
        state.deliver(out, &mut output.pos);
        input.pos = in_pos;
        let pending = state.pending();
        if mode == ZSTD_EndDirective::End && pending == 0 {
            state.finished = true;
        }
        Ok(pending)
    }

    pub fn record_error(&mut self, code: ErrorCode) {
        self.sticky = Some(code);
    }
}

impl Default for CCtx {
    fn default() -> Self {
        Self::new()
    }
}
