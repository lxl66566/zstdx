//! `ZSTD_CCtx`: the sticky parameter set, the one-shot entries built on it,
//! and the streaming-encode state behind `ZSTD_compressStream2`.
//!
//! The context owns zstdx-side state objects and never touches codec
//! internals: one-shot compression rides the zero-copy bulk slice core with
//! the source size declared (the streaming entries' byte-identical frame
//! without the input staging), and the streaming entries reuse one encoder
//! across calls with a delivered-cursor sink.
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
    /// ZSTD_c_dictIDFlag (libzstd default: on).
    pub dict_id_flag: bool,
    /// ZSTD_c_enableLongDistanceMatching: 0 auto / 1 enable / 2 disable,
    /// libzstd's `ZSTD_ParamSwitch_e` (default: auto).
    pub ldm: zstdx::LdmMode,
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
            dict_id_flag: true,
            ldm: zstdx::LdmMode::Auto,
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
            .include_dictid(self.dict_id_flag)
            .long_distance_matching(self.ldm)
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
            ZSTD_cParameter::EnableLongDistanceMatching => {
                // ZSTD_ParamSwitch_e: 0 auto, 1 enable, 2 disable (the
                // reference's bounds are [ps_auto, ps_disable]); the
                // applied enum value comes back.
                if !(0..=2).contains(&value) {
                    return Err(ErrorCode::ParameterOutOfBound);
                }
                self.ldm = match value {
                    1 => zstdx::LdmMode::Enable,
                    2 => zstdx::LdmMode::Disable,
                    _ => zstdx::LdmMode::Auto,
                };
                Ok(value as usize)
            },
            ZSTD_cParameter::ChecksumFlag
            | ZSTD_cParameter::ContentSizeFlag
            | ZSTD_cParameter::DictIDFlag => {
                let flag = value != 0;
                match param {
                    ZSTD_cParameter::ChecksumFlag => self.checksum = flag,
                    ZSTD_cParameter::ContentSizeFlag => self.content_size = flag,
                    _ => self.dict_id_flag = flag,
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
/// skips the digest. The raw byte length rides along (the reference sizes
/// the frame window by the whole loaded buffer). A dictionary that fails
/// to parse is retained as a pending error: libzstd digests lazily too, so
/// the failure surfaces when the next frame starts, not at the load call.
pub enum LoadedDict {
    Parsed(zstdx::EncoderDictionary, usize),
    Invalid,
}

/// The `ZSTD_CCtx_refCDict` attachment: the handle's parsed dictionary,
/// its creation level and LDM mode, and the raw byte length the frame
/// window sizing consults. The dictionary is owned by an Arc clone, so
/// the handle's lifetime does not bind the context.
pub struct AttachedCDict {
    pub dict: Option<zstdx::EncoderDictionary>,
    pub level: i32,
    pub ldm: zstdx::LdmMode,
    pub raw_len: usize,
}

impl AttachedCDict {
    fn new(cdict: &crate::dict::CDict) -> Self {
        Self {
            dict: cdict.dict.clone(),
            level: cdict.level,
            ldm: cdict.ldm,
            raw_len: cdict.raw_len,
        }
    }
}

/// One-shot compression of `src` into `dst`; returns the written length.
/// `dict` overrides the settings' dictionary for this call only.
///
/// Rides the zero-copy bulk slice core (the borrowed matcher window and
/// straight-to-buffer blocks) with the source size declared — byte-identical
/// to the streaming core over the same options, without its input staging.
/// The one cell the slice core cannot express: the multithreaded slice path
/// always declares its content size (the decoder's piece-partition policy),
/// so `nbWorkers >= 2` with the content-size flag off keeps the streaming
/// core, which honors the flag.
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
    if settings.workers >= 2 && !settings.content_size {
        let sink = Vec::with_capacity(crate::compress_bound(src.len()));
        let mut encoder = zstdx::stream::write::Encoder::with_options(sink, options)
            .map_err(|e| map_encode(&e))?;
        encoder.write_all(src).map_err(|e| map_io(&e))?;
        encoder.do_finish().map_err(|e| map_encode(&e))?;
        return copy_out(encoder.get_ref(), dst);
    }
    let compressed = zstdx::bulk::compress_with(src, &options).map_err(|e| map_encode(&e))?;
    copy_out(&compressed, dst)
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
    /// `ZSTD_getFrameProgression` counters for the running frame: the
    /// input bytes accepted (this layer stages no input of its own, so
    /// ingested equals consumed) and the output bytes delivered.
    ingested: u64,
    flushed: u64,
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
            ingested: 0,
            flushed: 0,
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
    /// `refPrefix` dictionaries are single-use (libzstd's `prefixDict` is
    /// consumed when the next frame opens).
    dict_use_once: bool,
    /// The `ZSTD_CCtx_refCDict` attachment; cleared by any dictionary
    /// load, as the reference's `ZSTD_clearAllDicts`.
    cdict: Option<AttachedCDict>,
    /// The running frame's block size (`ZSTD_getBlockSize`), 0 before any
    /// frame opens. It survives the frame and a session reset, like the
    /// reference's `appliedParams`.
    pub applied_block_size: usize,
    /// `ZSTD_CCtx_setPledgedSrcSize`: applies to the next frame only and is
    /// consumed when that frame completes.
    pledged: Option<u64>,
    stream: Option<StreamState>,
    /// The finished frame's counters, until the next frame opens (the
    /// reference resets them at its next frame's init, not at completion).
    last_progression: (u64, u64),
    /// libzstd leaves a failed context in an undefined state; this layer
    /// keeps replaying the first error until a reset.
    sticky: Option<ErrorCode>,
}

impl CCtx {
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            dict: None,
            dict_use_once: false,
            cdict: None,
            applied_block_size: 0,
            pledged: None,
            stream: None,
            last_progression: (0, 0),
            sticky: None,
        }
    }

    /// The loaded dictionary, or the pending parse error to surface at the
    /// next frame.
    fn dict(&self) -> Result<Option<&zstdx::EncoderDictionary>, ErrorCode> {
        match &self.dict {
            Some(LoadedDict::Parsed(dict, _)) => Ok(Some(dict)),
            Some(LoadedDict::Invalid) => Err(ErrorCode::DictionaryCorrupted),
            None => Ok(None),
        }
    }

    /// `ZSTD_CCtx_setParameter`: parameters are rejected mid-frame, as in
    /// libzstd.
    pub fn set_parameter(&mut self, param: i32, value: i32) -> Result<usize, ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            // libzstd authorizes a mid-frame update for the block/level
            // knobs (ZSTD_isUpdateAuthorized); frame-shape parameters and
            // unknown ids stay init-stage only. Of the authorized set this
            // layer implements only the compression level: stored mid-frame,
            // applied to the next frame (the reference retunes the running
            // frame's blocks from the next one).
            if param != ZSTD_cParameter::CompressionLevel as i32 {
                return Err(ErrorCode::StageWrong);
            }
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
        self.load_dictionary_typed(dict, crate::params::DictContentType::Auto)
    }

    /// `ZSTD_CCtx_loadDictionary` with the advanced content type:
    /// `rawContent` loads the plain match history whatever the bytes start
    /// with, `auto` parses the magic, and `fullDict` accepts formatted
    /// dictionaries only (a plain one stays a pending error, surfacing at
    /// the next frame like every digest failure).
    pub fn load_dictionary_typed(
        &mut self,
        dict: Option<&[u8]>,
        content_type: crate::params::DictContentType,
    ) -> Result<(), ErrorCode> {
        use crate::params::DictContentType;
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.dict_use_once = false;
        self.cdict = None;
        self.dict = match dict {
            Some(bytes) => {
                let parse = match content_type {
                    DictContentType::RawContent => Ok(zstdx::EncoderDictionary::raw_content(bytes)),
                    DictContentType::Auto => zstdx::EncoderDictionary::parse(bytes),
                    DictContentType::FullDict => {
                        if bytes.len() < 8
                            || bytes.first_chunk::<4>() != Some(&crate::params::DICT_MAGIC)
                        {
                            Err(zstdx::Error::Dictionary(
                                zstdx::decoding::errors::DictionaryDecodeError::NotEnoughBytes,
                            ))
                        } else {
                            zstdx::EncoderDictionary::parse(bytes)
                        }
                    },
                };
                match parse {
                    Ok(parsed) => Some(LoadedDict::Parsed(parsed, bytes.len())),
                    // Retained: the error surfaces when a frame tries to use it.
                    Err(_) => Some(LoadedDict::Invalid),
                }
            },
            None => None,
        };
        Ok(())
    }

    /// `ZSTD_CCtx_refPrefix`: like `loadDictionary` but single-use — the
    /// slot clears when the next frame opens. A null or empty prefix only
    /// clears any existing dictionary, as in the reference.
    pub fn ref_prefix(
        &mut self,
        prefix: Option<&[u8]>,
        content_type: crate::params::DictContentType,
    ) -> Result<(), ErrorCode> {
        self.load_dictionary_typed(prefix, content_type)?;
        self.dict_use_once = self.dict.is_some();
        Ok(())
    }

    /// `ZSTD_CCtx_setParametersUsingCCtxParams`: adopt the bag. The
    /// reference copies it wholesale; this layer applies the knobs the
    /// engine can honor and refuses the bag outright when any knob it
    /// cannot honor is set to a non-default value (loudly, never
    /// silently), the same refusal list the advanced CDict constructors
    /// use. Parameters cannot be overridden with a dictionary attached.
    pub fn set_parameters_using(
        &mut self,
        bag: &crate::params::CCtxParams,
    ) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        if self.cdict.is_some() {
            return Err(ErrorCode::StageWrong);
        }
        if bag.unsupported_requests() {
            return Err(ErrorCode::ParameterUnsupported);
        }
        let level = bag.compression_level_for_context();
        self.settings
            .set_parameter(ZSTD_cParameter::CompressionLevel, level)?;
        self.settings
            .set_parameter(ZSTD_cParameter::WindowLog, bag.c_params_window_log())?;
        self.settings
            .set_parameter(ZSTD_cParameter::ContentSizeFlag, bag.content_size_flag())?;
        self.settings
            .set_parameter(ZSTD_cParameter::ChecksumFlag, bag.checksum_flag())?;
        self.settings
            .set_parameter(ZSTD_cParameter::DictIDFlag, bag.f_params_dict_id_flag())?;
        self.settings.set_parameter(
            ZSTD_cParameter::EnableLongDistanceMatching,
            bag.ldm_switch(),
        )?;
        Ok(())
    }

    /// `ZSTD_CCtx_setCParams`: the bounds check first, then the window
    /// knob alone; the rest of the knobs have no engine counterpart, so
    /// any non-zero request is refused with `parameter_unsupported`
    /// instead of silently ignored.
    pub fn set_c_params(
        &mut self,
        c_params: crate::params::ZSTD_compressionParameters,
    ) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        crate::params::check_c_params(c_params)?;
        if c_params.hashLog != 0
            || c_params.chainLog != 0
            || c_params.searchLog != 0
            || c_params.minMatch != 0
            || c_params.targetLength != 0
            || c_params.strategy.0 != 0
        {
            return Err(ErrorCode::ParameterUnsupported);
        }
        self.settings
            .set_parameter(ZSTD_cParameter::WindowLog, c_params.windowLog as i32)?;
        Ok(())
    }

    /// `ZSTD_CCtx_setFParams`: the three frame flags.
    pub fn set_f_params(
        &mut self,
        f_params: crate::params::ZSTD_frameParameters,
    ) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.settings
            .set_parameter(ZSTD_cParameter::ContentSizeFlag, f_params.contentSizeFlag)?;
        self.settings
            .set_parameter(ZSTD_cParameter::ChecksumFlag, f_params.checksumFlag)?;
        self.settings.set_parameter(
            ZSTD_cParameter::DictIDFlag,
            i32::from(f_params.noDictIDFlag == 0),
        )?;
        Ok(())
    }

    /// `ZSTD_CCtx_setParams`: check the cParams, apply the frame flags,
    /// then the cParams (the reference's all-or-none ordering).
    pub fn set_zstd_params(
        &mut self,
        params: crate::params::ZSTD_parameters,
    ) -> Result<(), ErrorCode> {
        crate::params::check_c_params(params.cParams)?;
        self.set_f_params(params.fParams)?;
        self.set_c_params(params.cParams)?;
        Ok(())
    }

    /// `ZSTD_CCtx_refCDict`: attach the handle's dictionary to every
    /// following frame. A null handle clears the slot and any loaded
    /// dictionary (the reference's clear-then-assign).
    pub fn ref_cdict(&mut self, cdict: Option<&crate::dict::CDict>) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.finished) {
            return Err(ErrorCode::StageWrong);
        }
        self.dict = None;
        self.dict_use_once = false;
        self.cdict = cdict.map(AttachedCDict::new);
        Ok(())
    }

    /// The frame's block size for `ZSTD_getBlockSize` (the reference's
    /// appliedParams snapshot).
    fn applied_block_size_snapshot(&self, pledged: Option<u64>) -> usize {
        let dict_len = match &self.cdict {
            Some(cdict) => cdict.raw_len,
            None => match &self.dict {
                Some(LoadedDict::Parsed(_, len)) => *len,
                _ => 0,
            },
        };
        let settings = match &self.cdict {
            // The attached handle's creation params win over the sticky
            // level and LDM mode, as the reference's cdict precedence.
            Some(cdict) => {
                let mut settings = self.settings.clone();
                settings.level = cdict.level;
                settings.ldm = cdict.ldm;
                settings
            },
            None => self.settings.clone(),
        };
        crate::params::applied_block_size(&settings, pledged, dict_len)
    }

    /// Consume a single-use dictionary at frame open and snapshot the
    /// frame's block size for `ZSTD_getBlockSize`.
    fn frame_opened(&mut self, pledged: Option<u64>) {
        // The snapshot sees the dictionary the frame opens with, the
        // use-once prefix included, then the prefix retires.
        self.applied_block_size = self.applied_block_size_snapshot(pledged);
        if self.dict_use_once {
            self.dict = None;
            self.dict_use_once = false;
        }
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
        self.cdict = None;
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
        let settings = match &self.cdict {
            Some(cdict) => {
                let mut settings = self.settings.clone();
                settings.level = cdict.level;
                settings.ldm = cdict.ldm;
                settings
            },
            None => self.settings.clone(),
        };
        let applied = self.applied_block_size_snapshot(Some(src.len() as u64));
        let dict = match &self.cdict {
            Some(cdict) => cdict.dict.clone(),
            None => self.dict()?.cloned(),
        };
        self.applied_block_size = applied;
        // A refPrefix dictionary retires with the frame it opened.
        if self.dict_use_once {
            self.dict = None;
            self.dict_use_once = false;
        }
        compress_oneshot(&settings, dict.as_ref(), src, dst)
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
            let (settings, dict) = match &self.cdict {
                Some(cdict) => {
                    let mut settings = self.settings.clone();
                    settings.level = cdict.level;
                    settings.ldm = cdict.ldm;
                    (settings, cdict.dict.as_ref())
                },
                None => (self.settings.clone(), self.dict()?),
            };
            match StreamState::new(&settings, dict, pledged) {
                Ok(state) => self.stream = Some(state),
                Err(e) => {
                    self.sticky = Some(e);
                    return Err(e);
                },
            }
            self.frame_opened(pledged);
        }
        let state = self.stream.as_mut().expect("just ensured");
        let (in_start, out_start) = (input.pos, output.pos);

        // SAFETY: output.dst[..output.size] is the caller-validated write
        // pair from the C entry.
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
                // SAFETY: take <= input.size - in_pos, so src[in_pos..in_pos+take]
                // stays inside the caller's read buffer.
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
        state.ingested += (input.pos - in_start) as u64;
        state.flushed += (output.pos - out_start) as u64;
        let pending = state.pending();
        if mode == ZSTD_EndDirective::End && pending == 0 {
            state.finished = true;
            self.last_progression = (state.ingested, state.flushed);
        }
        Ok(pending)
    }

    /// `ZSTD_sizeof_CCtx`: the state struct plus the loaded dictionary's
    /// bytes (the reference's `localDict` term).
    pub fn footprint(&self) -> usize {
        size_of_val(self)
            + match &self.cdict {
                Some(attached) => attached.raw_len,
                None => match &self.dict {
                    Some(LoadedDict::Parsed(_, len)) => *len,
                    _ => 0,
                },
            }
    }

    pub fn record_error(&mut self, code: ErrorCode) {
        self.sticky = Some(code);
    }

    /// `ZSTD_getFrameProgression`: the running frame's counters, or zeros
    /// when no frame is open (the reference resets them with the session).
    /// The single-threaded reference answers produced == flushed (its
    /// simplification), so this layer reports the delivered count for
    /// both.
    pub fn frame_progression(&self) -> (u64, u64, u64, u64) {
        let (ingested, flushed) = match &self.stream {
            Some(state) if !state.finished => (state.ingested, state.flushed),
            _ => self.last_progression,
        };
        (ingested, ingested, flushed, flushed)
    }
}

impl Default for CCtx {
    fn default() -> Self {
        Self::new()
    }
}
