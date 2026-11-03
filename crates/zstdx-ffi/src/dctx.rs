//! `ZSTD_DCtx`: the decode-side parameters and the streaming-decode state
//! behind `ZSTD_decompressStream`.
//!
//! The streaming machine is a port of the reference's
//! `ZSTD_decompressStream` stage engine: input is claimed piece by piece
//! (frame header chunks, 3-byte block headers, whole block bodies, 4-byte
//! checksum trailers, skippable content), every completed piece is
//! processed at once, and decoded output is released one flush round per
//! block. A flush round that cannot empty the pending buffer ends the
//! call, a frame ends every call that finishes it, and a frame whose
//! output is still pending at its end holds back one input byte until the
//! flush completes. The return value is the reference's exact next-input
//! hint: 0 when a frame is completely decoded and fully flushed.
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

/// `ZSTD_DStreamOutSize`: one block of output.
pub const DSTREAM_OUT_SIZE: usize = 128 * 1024;
/// `ZSTD_DStreamInSize`: one block plus its header.
pub const DSTREAM_IN_SIZE: usize = DSTREAM_OUT_SIZE + 3;

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

/// The wire type of a block body. The reserved type never reaches a body:
/// the header-piece parse rejects it first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockWireType {
    Raw,
    Rle,
    Compressed,
}

/// What the streaming machine accumulates or awaits next: the reference's
/// per-piece stages (`ZSTDds_*`) with its streaming stages (`zdss_*`)
/// folded in. Input is claimed piece by piece, never in bulk.
#[derive(Clone, Copy)]
enum Stage {
    /// No frame is open: the next frame header accumulates at the staging
    /// cursor (the reference's `zdss_loadHeader` over its `headerBuffer`).
    FrameHeader,
    /// A 3-byte block header awaits at the cursor.
    BlockHeader,
    /// A block body: the wire type, the last-block flag, the whole body's
    /// wire size, and the wire bytes still to claim. Raw bodies release
    /// piecewise as their bytes arrive; the others wait for the whole
    /// body and decode in one step.
    BlockBody {
        wire: BlockWireType,
        last: bool,
        size: usize,
        remaining: usize,
    },
    /// The frame's 4-byte checksum trailer awaits.
    Checksum,
    /// Skippable-frame content: this many bytes to claim and discard.
    SkipContent(usize),
    /// The frame's last piece went out: the next result is the completion
    /// answer (0), or 1 while decoded output is still pending.
    FrameEnded,
}

/// One `ZSTD_decompressStream` call's answer.
struct CallOutcome {
    /// The libzstd hint: 0 when a frame is completely decoded and fully
    /// flushed, otherwise the next input piece's wanted size.
    hint: usize,
    /// The reference's `zdss_loadHeader` short-header return, which sits
    /// before its no-forward-progress guard and never counts there.
    guard_exempt: bool,
}

/// The staged prefix plus the call's remaining input, as one address space
/// for the shortcut's frame walk. Position 0 is the frame start.
fn chained_byte(staged: &[u8], rest: &[u8], pos: usize) -> Option<u8> {
    staged
        .get(pos)
        .copied()
        .or_else(|| rest.get(pos - staged.len()).copied())
}

/// `ZSTD_findFrameCompressedSize` over the staged prefix plus the call's
/// remaining input: the reference measures the single-pass shortcut from
/// this call's input start, so a frame that began in an earlier call has
/// its start bytes invisible here, fails the walk and loses the shortcut.
fn chained_frame_size(staged: &[u8], rest: &[u8]) -> Option<usize> {
    let Ok(crate::frame::HeaderParse::Complete(full)) = crate::frame::parse_full_header(staged)
    else {
        return None;
    };
    let total = staged.len() + rest.len();
    let mut pos = full.header_size;
    loop {
        if pos + 3 > total {
            return None;
        }
        let b0 = chained_byte(staged, rest, pos)?;
        let b1 = chained_byte(staged, rest, pos + 1)?;
        let b2 = chained_byte(staged, rest, pos + 2)?;
        let last = b0 & 1 == 1;
        let block_type = (b0 >> 1) & 3;
        let size = (usize::from(b0) >> 3) | (usize::from(b1) << 5) | (usize::from(b2) << 13);
        // The walk itself takes header sizes at face value (the 128 KiB
        // ceiling is a decode-time check), exactly as frame_compressed_size.
        let body = match block_type {
            0 | 2 => size,
            1 => 1,
            _ => return None,
        };
        pos += 3 + body;
        if pos > total {
            return None;
        }
        if last {
            if full.checksum {
                pos += 4;
                if pos > total {
                    return None;
                }
            }
            return Some(pos);
        }
    }
}

/// Live streaming-decoder state inside a DCtx: a port of the reference's
/// `ZSTD_decompressStream` stage engine. Input is claimed piece by piece
/// (frame header chunks, 3-byte block headers, whole block bodies, 4-byte
/// checksums, skippable content), each piece is processed as it completes,
/// and decoded output is released to the caller one flush round per block.
/// A flush round that cannot empty the pending buffer ends the call, and a
/// frame whose output is still pending at its end holds back one input
/// byte (the reference's hostage) until the flush completes.
// The bool set is the stage engine's own state flags, one field per
// documented transition; folding them would hide the port's shape.
#[allow(clippy::struct_excessive_bools)]
struct DStream {
    inner: FrameDecoder,
    /// Input claimed from the caller but not yet processed; `cursor` is
    /// the machine's position, everything below it is spent.
    staging: Vec<u8>,
    cursor: usize,
    stage: Stage,
    /// The frame being decoded announces a content checksum.
    checksummed: bool,
    /// `min(window, 128 KiB)` of the open frame: the header-piece bound on
    /// a block's declared size.
    block_size_max: usize,
    /// Decoded output not yet handed to the caller.
    pending: Vec<u8>,
    delivered: usize,
    /// Raw-body bytes already handed out during piecewise arrival; skipped
    /// when the engine releases the completed block, so they are not
    /// delivered twice.
    pre_released: usize,
    /// A frame ended with output pending: one claimed input byte is held
    /// back until the flush completes.
    hostage: bool,
    /// A real (non-skippable) frame is open.
    frame_open: bool,
    /// The context's `use_once` prefix: consumed when the first real frame
    /// completes (subsequent frames decode plain, as the reference's
    /// getDDict does).
    dict_use_once: bool,
    /// The dictionary generation this stream's decoder was armed with.
    dict_generation: u32,
    /// A frame consumed the use-once prefix this call; the context slot
    /// clears when the call returns.
    use_once_consumed: bool,
}

impl DStream {
    fn new(
        settings: &Settings,
        dict: Option<&SharedDict>,
        dict_use_once: bool,
        dict_generation: u32,
    ) -> Result<Self, ErrorCode> {
        let mut stream = Self {
            inner: FrameDecoder::new(),
            staging: Vec::new(),
            cursor: 0,
            stage: Stage::FrameHeader,
            checksummed: false,
            block_size_max: 0,
            pending: Vec::new(),
            delivered: 0,
            pre_released: 0,
            hostage: false,
            frame_open: false,
            dict_use_once,
            dict_generation,
            use_once_consumed: false,
        };
        stream.arm_decoder(settings, dict)?;
        Ok(stream)
    }

    /// Point the stream's decoder at `dict`: a fresh `FrameDecoder` with
    /// the window limit and the dictionary registered under the options
    /// rule (the dictionary's own id and the null id — libzstd applies a
    /// ref'd dictionary to every frame regardless of the header's dictID
    /// field, so a dictIDFlag-suppressed frame decodes with it attached).
    fn arm_decoder(
        &mut self,
        settings: &Settings,
        dict: Option<&SharedDict>,
    ) -> Result<(), ErrorCode> {
        let mut inner = FrameDecoder::new();
        if let Some(log) = settings.window_log_max {
            inner.set_max_window_size(1_u64 << log);
        }
        if let Some(dict) = dict {
            inner
                .add_options_dict(Arc::clone(dict))
                .map_err(|_| ErrorCode::Generic)?;
        }
        self.inner = inner;
        Ok(())
    }

    fn staged(&self) -> &[u8] {
        &self.staging[self.cursor..]
    }

    /// Claim `n` input bytes into staging.
    ///
    /// # Safety
    /// `n` must not exceed the input's unread span (callers bound it by
    /// the piece arithmetic).
    unsafe fn claim(&mut self, input: &ZSTD_inBuffer, in_pos: &mut usize, n: usize) {
        if n > 0 {
            // SAFETY: n <= input.size - in_pos per the contract above.
            self.staging
                .extend_from_slice(unsafe { input.slice_at(*in_pos, n) });
            *in_pos += n;
        }
    }

    /// Drop spent staging below the cursor, keeping the last `keep` bytes
    /// under it: a block body in flight keeps its 3-byte header there,
    /// because the engine reads the whole block back out of staging once
    /// the body completes. Raw bodies stage their pieces above the cursor
    /// as they arrive, so their compaction waits for the completion.
    fn compact(&mut self, keep: usize) {
        if self.cursor < COMPACT_LIMIT && self.cursor < self.staging.len() {
            return;
        }
        let cut = self.cursor.saturating_sub(keep);
        if cut == 0 {
            return;
        }
        self.staging.drain(..cut);
        self.cursor -= cut;
    }

    /// Copy pending output into the caller's buffer.
    fn deliver(&mut self, out: &mut [u8], pos: &mut usize) {
        let take = (self.pending.len() - self.delivered).min(out.len() - *pos);
        out[*pos..*pos + take]
            .copy_from_slice(&self.pending[self.delivered..self.delivered + take]);
        *pos += take;
        self.delivered += take;
        if self.delivered == self.pending.len() {
            self.pending.clear();
            self.delivered = 0;
        }
    }

    /// Whether the stream sits between frames with nothing staged or
    /// pending: the state a parameters reset requires (the reference's
    /// `zdss_init`).
    fn at_boundary(&self) -> bool {
        matches!(self.stage, Stage::FrameHeader)
            && self.pending.is_empty()
            && self.cursor == self.staging.len()
    }

    /// The frame's last piece went out: enter the completion state and
    /// consume the use-once prefix when a real frame used it.
    fn finish_frame(&mut self) {
        self.stage = Stage::FrameEnded;
        if self.frame_open && core::mem::take(&mut self.dict_use_once) {
            self.use_once_consumed = true;
        }
        self.frame_open = false;
    }

    /// Decode one whole block (its 3-byte header plus body, both staged)
    /// through the engine and release the block's output into the pending
    /// buffer. The last block of a checksummed frame is fed without its
    /// trailer: the engine then reports the missing checksum, which the
    /// machine serves as its own piece next.
    fn decode_block(
        &mut self,
        header_pos: usize,
        body_end: usize,
        last: bool,
    ) -> Result<(), ErrorCode> {
        // Direct field slices: the source borrows `staging` while
        // `decode_blocks` mutates `inner` (disjoint fields).
        let mut source: &[u8] = &self.staging[header_pos..body_end];
        match self
            .inner
            .decode_blocks(&mut source, BlockDecodingStrategy::UptoBlocks(1))
        {
            Ok(_) => {},
            Err(zstdx::decoding::errors::FrameDecoderError::FailedToReadChecksum(_))
                if last && self.checksummed => {},
            Err(e) => return Err(error::frame(&e, DecodeSite::Stream)),
        }
        debug_assert!(source.is_empty());
        let mut released = Vec::new();
        self.inner
            .collect_to_writer(&mut released)
            .map_err(|_| ErrorCode::Generic)?;
        let skip = self.pre_released.min(released.len());
        self.pending.extend_from_slice(&released[skip..]);
        self.pre_released = 0;
        Ok(())
    }

    /// One `ZSTD_decompressStream` call: the reference's stage loop over
    /// flush rounds and input pieces. Positions are the caller's cursors,
    /// committed by the caller only on success.
    #[allow(clippy::too_many_arguments)]
    // One function per stage transition would lose the reference's
    // single-loop stage machine, which this port keeps shape-parallel to
    // `ZSTD_decompressStream`'s internal loop.
    #[allow(clippy::too_many_lines)]
    fn advance(
        &mut self,
        settings: &Settings,
        dict: Option<&SharedDict>,
        dict_use_once: bool,
        generation: u32,
        out: &mut [u8],
        out_pos: &mut usize,
        input: &mut ZSTD_inBuffer,
        in_pos: &mut usize,
    ) -> Result<CallOutcome, ErrorCode> {
        // This call's input starts here in staging: the single-pass
        // shortcut measures from it, so a header that began in an earlier
        // call cannot shortcut.
        let call_start = self.staging.len();
        // Every stage arm ends its iteration with an explicit `continue`,
        // mirroring the reference's per-stage flow.
        #[allow(clippy::needless_continue)]
        loop {
            // Flush round: pending output to the caller; a round that
            // cannot empty the pending buffer ends the call.
            if self.delivered < self.pending.len() {
                self.deliver(out, out_pos);
                if self.delivered < self.pending.len() {
                    break;
                }
                self.pending.clear();
                self.delivered = 0;
            }
            let remaining = input.size - *in_pos;
            match self.stage {
                Stage::FrameHeader => {
                    match crate::frame::parse_full_header(self.staged()) {
                        Ok(crate::frame::HeaderParse::Complete(full)) => {
                            match full.frame_type {
                                crate::frame::FrameType::Skippable => {
                                    self.cursor += full.header_size;
                                    self.compact(0);
                                    self.checksummed = false;
                                    self.frame_open = false;
                                    self.stage = Stage::SkipContent(
                                        full.content_size.expect("skippable sets it") as usize,
                                    );
                                    continue;
                                },
                                crate::frame::FrameType::Zstd => {
                                    // Single-pass shortcut: a frame with a
                                    // pledged size that fits the caller's
                                    // buffer and arrives whole in this call
                                    // decodes in one step, straight into
                                    // the caller's output.
                                    if let Some(fcs) = full.content_size
                                        && (out.len() - *out_pos) as u64 >= fcs
                                    {
                                        let have = self.staging.len() - call_start;
                                        // SAFETY: remaining is the
                                        // input's unread span.
                                        let rest = unsafe { input.slice_at(*in_pos, remaining) };
                                        if let Some(csize) =
                                            chained_frame_size(&self.staging[call_start..], rest)
                                            && csize >= have
                                            && csize - have <= remaining
                                        {
                                            // SAFETY: csize - have <= remaining.
                                            unsafe {
                                                self.claim(input, in_pos, csize - have);
                                            };
                                            let frame_end = call_start + csize;
                                            let written = decompress_oneshot(
                                                settings,
                                                dict,
                                                &self.staging[call_start..frame_end],
                                                &mut out[*out_pos..],
                                            )?;
                                            *out_pos += written;
                                            self.staging.truncate(call_start);
                                            self.cursor = call_start;
                                            self.finish_frame();
                                            continue;
                                        }
                                    }
                                    // The per-frame dictionary consult: a
                                    // slot change re-arms the decoder.
                                    if self.dict_generation != generation {
                                        self.arm_decoder(settings, dict)?;
                                        self.dict_use_once = dict_use_once;
                                        self.dict_generation = generation;
                                    }
                                    // Direct field slices: the source
                                    // borrows `staging` while `reset`
                                    // mutates `inner` (disjoint fields).
                                    let tail_len = self.staging.len() - self.cursor;
                                    let mut source: &[u8] = &self.staging[self.cursor..];
                                    self.inner
                                        .reset(&mut source)
                                        .map_err(|e| error::frame(&e, DecodeSite::Stream))?;
                                    self.cursor += tail_len - source.len();
                                    self.compact(0);
                                    self.checksummed = full.checksum;
                                    self.block_size_max = full.block_size_max as usize;
                                    self.frame_open = true;
                                    self.stage = Stage::BlockHeader;
                                    continue;
                                },
                            }
                        },
                        Ok(crate::frame::HeaderParse::Wanted(want)) => {
                            let staged = self.staging.len() - self.cursor;
                            let to_load = want - staged;
                            if to_load > remaining {
                                // Claim what is there and ask for the rest;
                                // the enlarged visible prefix is judged
                                // before the hint goes out.
                                // SAFETY: remaining is the input's unread span.
                                unsafe { self.claim(input, in_pos, remaining) };
                                let want = match crate::frame::parse_full_header(self.staged()) {
                                    Ok(crate::frame::HeaderParse::Wanted(w)) => w,
                                    // The claim was strictly short of the
                                    // header, so this cannot complete.
                                    Ok(crate::frame::HeaderParse::Complete(_)) => want,
                                    Err(e) => {
                                        // This error path is behind the
                                        // reference's `input->pos = input->size`
                                        // commit; the claim stands.
                                        input.pos = *in_pos;
                                        return Err(crate::frame::parse_error_code(e));
                                    },
                                };
                                let staged = self.staging.len() - self.cursor;
                                return Ok(CallOutcome {
                                    hint: want.max(FRAME_HEADER_MIN) - staged + 3,
                                    guard_exempt: true,
                                });
                            }
                            // SAFETY: to_load <= remaining.
                            unsafe { self.claim(input, in_pos, to_load) };
                            continue;
                        },
                        Err(e) => return Err(crate::frame::parse_error_code(e)),
                    }
                },
                Stage::BlockHeader => {
                    let staged = self.staging.len() - self.cursor;
                    if staged < 3 {
                        let need = 3 - staged;
                        if need > remaining {
                            // SAFETY: remaining is the input's unread span.
                            unsafe { self.claim(input, in_pos, remaining) };
                            break;
                        }
                        // SAFETY: need <= remaining.
                        unsafe { self.claim(input, in_pos, need) };
                    }
                    let [b0, b1, b2, ..] = *self.staged() else {
                        break;
                    };
                    let last = b0 & 1 == 1;
                    let wire = match (b0 >> 1) & 3 {
                        0 => BlockWireType::Raw,
                        1 => BlockWireType::Rle,
                        2 => BlockWireType::Compressed,
                        // The reserved block type is corruption at the
                        // header piece, exactly as getcBlockSize.
                        _ => return Err(ErrorCode::CorruptionDetected),
                    };
                    let size =
                        (usize::from(b0) >> 3) | (usize::from(b1) << 5) | (usize::from(b2) << 13);
                    // getcBlockSize: the RLE body is its single content
                    // byte; `size` is then the regenerated length.
                    let c_size = if wire == BlockWireType::Rle {
                        1
                    } else {
                        size
                    };
                    if c_size > self.block_size_max {
                        return Err(ErrorCode::CorruptionDetected);
                    }
                    self.cursor += 3;
                    self.compact(3);
                    if c_size == 0 {
                        // An empty block carries no body piece and produces
                        // no output: straight to the next header or the
                        // trailer.
                        if last {
                            if self.checksummed {
                                self.stage = Stage::Checksum;
                            } else {
                                self.finish_frame();
                            }
                        }
                        continue;
                    }
                    self.stage = Stage::BlockBody {
                        wire,
                        last,
                        size: c_size,
                        remaining: c_size,
                    };
                    continue;
                },
                Stage::BlockBody {
                    wire: BlockWireType::Raw,
                    last,
                    size,
                    remaining,
                } => {
                    // Raw bodies stream piecewise: every byte that
                    // arrives is output already, at least one byte per
                    // piece (an empty input stops the call instead).
                    if input.size == *in_pos {
                        break;
                    }
                    let piece = remaining.min(input.size - *in_pos);
                    // SAFETY: piece <= remaining input.
                    unsafe { self.claim(input, in_pos, piece) };
                    let start = self.staging.len() - piece;
                    self.pending.extend_from_slice(&self.staging[start..]);
                    self.pre_released += piece;
                    self.cursor += piece;
                    let rest = remaining - piece;
                    if rest == 0 {
                        let header_pos = self.cursor - size - 3;
                        let body_end = self.cursor;
                        self.decode_block(header_pos, body_end, last)?;
                        self.compact(0);
                        if last {
                            if self.checksummed {
                                self.stage = Stage::Checksum;
                            } else {
                                self.finish_frame();
                            }
                        } else {
                            self.stage = Stage::BlockHeader;
                        }
                    } else {
                        self.stage = Stage::BlockBody {
                            wire: BlockWireType::Raw,
                            last,
                            size,
                            remaining: rest,
                        };
                    }
                    continue;
                },
                Stage::BlockBody {
                    wire: _,
                    last,
                    size,
                    remaining,
                } => {
                    // Compressed and RLE bodies decode whole: stage the
                    // rest of the piece first.
                    let staged = self.staging.len() - self.cursor;
                    let need = remaining - staged;
                    if need > input.size - *in_pos {
                        // SAFETY: the input's unread span.
                        unsafe { self.claim(input, in_pos, input.size - *in_pos) };
                        break;
                    }
                    // SAFETY: need <= remaining input.
                    unsafe { self.claim(input, in_pos, need) };
                    let header_pos = self.cursor - 3;
                    let body_end = self.cursor + size;
                    self.decode_block(header_pos, body_end, last)?;
                    self.cursor = body_end;
                    self.compact(0);
                    if last {
                        if self.checksummed {
                            self.stage = Stage::Checksum;
                        } else {
                            self.finish_frame();
                        }
                    } else {
                        self.stage = Stage::BlockHeader;
                    }
                    continue;
                },
                Stage::Checksum => {
                    let staged = self.staging.len() - self.cursor;
                    let need = 4 - staged;
                    if need > remaining {
                        // SAFETY: remaining is the input's unread span.
                        unsafe { self.claim(input, in_pos, remaining) };
                        break;
                    }
                    // SAFETY: need <= remaining.
                    unsafe { self.claim(input, in_pos, need) };
                    // Direct field slices: the source borrows `staging`
                    // while the decoder call mutates `inner`.
                    let source: &[u8] = &self.staging[self.cursor..self.cursor + 4];
                    let mut sink = [0u8; 0];
                    match self.inner.decode_from_to(source, &mut sink) {
                        Ok(_) => {},
                        Err(e) => return Err(error::frame(&e, DecodeSite::Stream)),
                    }
                    self.cursor += 4;
                    self.compact(0);
                    self.finish_frame();
                    continue;
                },
                Stage::SkipContent(rest) => {
                    if rest == 0 {
                        self.finish_frame();
                        continue;
                    }
                    let take = rest.min(remaining);
                    if take == 0 {
                        break;
                    }
                    // Skippable content is claimed and discarded in place,
                    // never staged.
                    *in_pos += take;
                    if take == rest {
                        self.finish_frame();
                    } else {
                        self.stage = Stage::SkipContent(rest - take);
                    }
                    continue;
                },
                Stage::FrameEnded => break,
            }
        }
        // Result: the completion answer or the piece hint.
        if matches!(self.stage, Stage::FrameEnded) {
            if self.pending.is_empty() {
                if self.hostage {
                    if *in_pos >= input.size {
                        // The hostage byte was not re-offered; stay.
                        return Ok(CallOutcome {
                            hint: 1,
                            guard_exempt: false,
                        });
                    }
                    // Release the hostage: the byte belongs to the
                    // finished frame and is consumed unread.
                    *in_pos += 1;
                    self.hostage = false;
                }
                self.stage = Stage::FrameHeader;
                return Ok(CallOutcome {
                    hint: 0,
                    guard_exempt: false,
                });
            }
            if !self.hostage {
                // Output pending: hold one claimed input byte back so the
                // caller must call again. It is a byte of the finished
                // frame, so the hold-back only rewinds the claim.
                debug_assert_eq!(self.staging.len(), self.cursor);
                debug_assert!(*in_pos > 0);
                *in_pos -= 1;
                if self.cursor > 0 {
                    self.staging.pop();
                    self.cursor -= 1;
                }
                self.hostage = true;
            }
            return Ok(CallOutcome {
                hint: 1,
                guard_exempt: false,
            });
        }
        let staged = self.staging.len() - self.cursor;
        let hint = match self.stage {
            Stage::BlockHeader => 3 - staged,
            Stage::BlockBody {
                last: false,
                remaining,
                ..
            } => remaining + 3 - staged,
            Stage::BlockBody {
                last: true,
                remaining,
                ..
            } => remaining - staged,
            Stage::Checksum => 4 - staged,
            // Skippable content is claimed directly, nothing is staged.
            Stage::SkipContent(rest) => rest,
            Stage::FrameHeader | Stage::FrameEnded => {
                unreachable!("both stages return their own hint above")
            },
        };
        Ok(CallOutcome {
            hint,
            guard_exempt: false,
        })
    }
}

/// `ZSTD_NO_FORWARD_PROGRESS_MAX`: consecutive no-progress streaming
/// calls before the reference refuses.
const NO_FORWARD_PROGRESS_MAX: u32 = 16;

/// `ZSTD_FRAMEHEADERSIZE_MIN` for the zstd1 format: the floor of the
/// loadHeader hint (`MAX(FRAMEHEADERSIZE_MIN, hSize) - staged + 3`).
const FRAME_HEADER_MIN: usize = 6;

/// Spent staging below this many bytes stays until the whole tail is
/// consumed (one drain per frame in the steady state).
const COMPACT_LIMIT: usize = 4096;

/// The `ZSTD_DCtx` object.
pub struct DCtx {
    pub settings: Settings,
    /// The context dictionary, parsed once and shared with every stream
    /// (set by `usingDict` — sticky until `initDStream` per the legacy
    /// contract — or `loadDictionary`; cleared by a parameters reset).
    pub dict: Option<SharedDict>,
    /// `refPrefix` dictionaries decode one frame (libzstd's `use_once`).
    pub dict_use_once: bool,
    /// Bumped on every dictionary-slot change; a live stream compares it
    /// to re-arm its decoder at the next frame boundary (the reference
    /// consults its dictionary reference per frame).
    dict_generation: u32,
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
            dict_use_once: false,
            dict_generation: 0,
            stream: None,
            sticky: None,
            no_forward: 0,
        }
    }

    /// Replace the dictionary slot (any live stream re-arms at its next
    /// frame boundary).
    pub fn set_dict(&mut self, dict: Option<SharedDict>) {
        self.dict = dict;
        self.dict_use_once = false;
        self.dict_generation = self.dict_generation.wrapping_add(1);
    }

    /// `ZSTD_DCtx_loadDictionary`: rejected mid-frame; a null or empty
    /// dictionary clears the slot. The parse is eager (the dictionary is
    /// digested once), so an unloadable dictionary fails here — libzstd
    /// digests eagerly on the decode side too.
    pub fn load_dictionary(&mut self, dict: Option<&[u8]>) -> Result<(), ErrorCode> {
        self.load_dictionary_typed(dict, crate::params::DictContentType::Auto)
    }

    /// `loadDictionary` with the advanced content type. `rawContent`
    /// builds the pure match-history dictionary whatever the bytes start
    /// with (the reference's forced-raw load); `fullDict` refuses plain
    /// content with `dictionary_corrupted`.
    pub fn load_dictionary_typed(
        &mut self,
        dict: Option<&[u8]>,
        content_type: crate::params::DictContentType,
    ) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        let parsed = match dict {
            Some(bytes) => Some(Arc::new(
                Self::parse_dict(bytes, content_type)
                    .map_err(|()| ErrorCode::DictionaryCorrupted)?,
            )),
            None => None,
        };
        self.set_dict(parsed);
        Ok(())
    }

    fn parse_dict(
        bytes: &[u8],
        content_type: crate::params::DictContentType,
    ) -> Result<zstdx::decoding::Dictionary, ()> {
        use crate::params::DictContentType;
        match content_type {
            // The auto parse: formatted on the magic, raw content below it.
            DictContentType::Auto => zstdx::decoding::Dictionary::load(bytes).map_err(|_| ()),
            // Forced raw content, the plain match history.
            DictContentType::RawContent => Ok(zstdx::decoding::Dictionary::raw_content(bytes)),
            // A formatted dictionary only; anything else is corrupted.
            DictContentType::FullDict => {
                if bytes.len() < 8 || bytes.first_chunk::<4>() != Some(&crate::params::DICT_MAGIC) {
                    return Err(());
                }
                zstdx::decoding::Dictionary::decode_dict(bytes).map_err(|_| ())
            },
        }
    }

    /// `ZSTD_DCtx_refPrefix`: the prefix dictionary for exactly the next
    /// frame (`use_once`), clearing any existing reference.
    pub fn ref_prefix_typed(
        &mut self,
        prefix: Option<&[u8]>,
        content_type: crate::params::DictContentType,
    ) -> Result<(), ErrorCode> {
        self.load_dictionary_typed(prefix, content_type)?;
        self.dict_use_once = self.dict.is_some();
        Ok(())
    }

    /// `ZSTD_DCtx_refDDict`: attach a parsed handle dictionary to every
    /// following frame; a null handle clears the slot.
    pub fn ref_ddict(&mut self, ddict: Option<&SharedDict>) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        self.set_dict(ddict.cloned());
        Ok(())
    }

    /// Consume a `use_once` dictionary (after the frame that used it).
    pub fn consume_use_once(&mut self) {
        if self.dict_use_once {
            self.set_dict(None);
        }
    }

    /// `ZSTD_DCtx_setMaxWindowSize`: the byte form of the window limit
    /// (no 0-means-default special case; out-of-range bytes error).
    pub fn set_max_window_size(&mut self, max_window_size: usize) -> Result<(), ErrorCode> {
        if self.stream.as_ref().is_some_and(|s| !s.at_boundary()) {
            return Err(ErrorCode::StageWrong);
        }
        let min = 1_usize << 10;
        let max = 1_usize << 31;
        if max_window_size < min || max_window_size > max {
            return Err(ErrorCode::ParameterOutOfBound);
        }
        // Stored as the log (highbit32 of the byte limit, the value
        // getParameter reports); the 2^10..=2^31 range maps losslessly.
        self.settings.window_log_max = Some((max_window_size as u32).ilog2());
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
        self.dict_use_once = false;
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
        self.dict_use_once = false;
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
        self.set_dict(dict.cloned());
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
        let outcome = self.decompress_stream_inner(output, input)?;
        self.settle_use_once();
        if outcome.guard_exempt {
            // The reference's loadHeader return sits before its guard.
            return Ok(outcome.hint);
        }
        if output.pos == out_start && input.pos == in_start {
            self.no_forward += 1;
            if self.no_forward >= NO_FORWARD_PROGRESS_MAX {
                return Err(if output.pos == output.size {
                    ErrorCode::NoForwardProgressDestFull
                } else {
                    ErrorCode::NoForwardProgressInputEmpty
                });
            }
        } else {
            self.no_forward = 0;
        }
        Ok(outcome.hint)
    }

    fn decompress_stream_inner(
        &mut self,
        output: &mut ZSTD_outBuffer,
        input: &mut ZSTD_inBuffer,
    ) -> Result<CallOutcome, ErrorCode> {
        if self.stream.is_none() {
            let dict = self.dict.as_ref();
            match DStream::new(
                &self.settings,
                dict,
                self.dict_use_once,
                self.dict_generation,
            ) {
                Ok(stream) => self.stream = Some(stream),
                Err(e) => {
                    self.sticky = Some(e);
                    return Err(e);
                },
            }
        }
        // SAFETY: output.dst[..output.size] is the caller-validated write
        // pair from the C entry.
        let out = unsafe { crate::as_mut_slice(output.dst, output.size) };
        let mut out_pos = output.pos;
        let mut in_pos = input.pos;
        let dict = self.dict.clone();
        let generation = self.dict_generation;
        let stream = self.stream.as_mut().expect("just ensured");
        let result = stream.advance(
            &self.settings,
            dict.as_ref(),
            self.dict_use_once,
            generation,
            out,
            &mut out_pos,
            input,
            &mut in_pos,
        );
        match result {
            Ok(outcome) => {
                // Positions commit only on success; the reference leaves
                // the caller's cursors untouched when it errors mid-loop.
                output.pos = out_pos;
                input.pos = in_pos;
                Ok(outcome)
            },
            Err(e) => Err(e),
        }
    }

    /// Complete a call whose frame consumed the use-once prefix: the
    /// context slot clears before the answer goes out.
    pub fn settle_use_once(&mut self) {
        if let Some(stream) = self.stream.as_mut()
            && stream.use_once_consumed
        {
            stream.use_once_consumed = false;
            self.consume_use_once();
        }
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
