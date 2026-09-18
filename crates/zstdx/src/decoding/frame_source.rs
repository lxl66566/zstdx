//! Frame-boundary plumbing shared by the streaming decoders ([`FrameDecoder`]
//! re-initialization): peek the next frame's magic number, skip skippable
//! frames, and stage every byte consumed for the hunt so a retried attempt
//! (transient read error) replays them instead of re-reading them.

use core::mem;

use crate::{
    decoding::{
        FrameDecoder,
        errors::{FrameDecoderError, ReadFrameHeaderError},
        frame::is_skippable_magic,
    },
    io::{Error, ErrorKind, Read},
};

/// Serialized frame magic number length.
const MAGIC_LEN: usize = 4;
/// Serialized skippable-frame content size field length.
const SKIP_SIZE_LEN: usize = 4;
/// Longest serialized frame header: magic (4) + descriptor (1) + window
/// descriptor (1) + dictionary id (4) + frame content size (8).
const MAX_HEADER_LEN: usize = 18;

/// Bytes already consumed from the source while hunting the next frame
/// start, carried across [`start_frame`] calls: a transient read error
/// (e.g. `WouldBlock` on a non-blocking socket) must not lose them — the
/// source will not re-serve them, and a retry that re-reads would parse
/// payload as a magic number and corrupt the stream permanently.
#[derive(Default)]
pub(super) enum NextFrameStaging {
    /// No hunt in progress.
    #[default]
    Idle,
    /// Frame-header bytes read ahead of the parser. A retried attempt
    /// replays them before pulling more. The parser reads exact amounts
    /// bounded by [`MAX_HEADER_LEN`], so the stage never overflows.
    Header {
        buf: [u8; MAX_HEADER_LEN],
        filled: usize,
    },
    /// Inside a skippable frame's content: bytes still to discard.
    SkipContent { remaining: u64 },
}

/// Where the decoder stands when asking for the next frame.
#[derive(Clone, Copy)]
enum StreamPosition {
    /// No frame was decoded yet; an exhausted source is an error (an empty
    /// stream is not a stream of frames, as in the reference decoders).
    Start,
    /// A frame completed; an exhausted source is the clean end of the stream.
    AfterFrame,
}

enum FrameStart {
    Frame,
    CleanEnd,
}

/// Parse the stream's first frame header into `decoder`. Consumes leading
/// skippable frames transparently; fails if the source holds no data frame.
pub(crate) fn init_first_frame<R: Read>(
    source: &mut R,
    decoder: &mut FrameDecoder,
) -> Result<(), FrameDecoderError> {
    start_frame(source, decoder, StreamPosition::Start).map(|_| ())
}

/// Advance from a completed frame to the next one, consuming skippable
/// frames transparently. `Ok(false)` marks the clean end of the stream.
pub(crate) fn init_next_frame<R: Read>(
    source: &mut R,
    decoder: &mut FrameDecoder,
) -> Result<bool, FrameDecoderError> {
    Ok(matches!(
        start_frame(source, decoder, StreamPosition::AfterFrame)?,
        FrameStart::Frame
    ))
}

fn start_frame<R: Read>(
    source: &mut R,
    decoder: &mut FrameDecoder,
    position: StreamPosition,
) -> Result<FrameStart, FrameDecoderError> {
    // The staging rides outside the decoder for the attempt: the header
    // parser takes `&mut decoder` while reading through the staging.
    let mut staging = mem::take(&mut decoder.next_frame_staging);
    let result = hunt_frame_start(source, decoder, &mut staging, position);
    decoder.next_frame_staging = staging;
    result
}

fn hunt_frame_start<R: Read>(
    source: &mut R,
    decoder: &mut FrameDecoder,
    staging: &mut NextFrameStaging,
    position: StreamPosition,
) -> Result<FrameStart, FrameDecoderError> {
    loop {
        // Resume a failed attempt: finish a pending skippable-frame discard,
        // or replay the staged header bytes below.
        if matches!(staging, NextFrameStaging::SkipContent { .. }) {
            discard_skippable(source, staging)?;
            *staging = NextFrameStaging::Idle;
            continue;
        }
        let mut peek = PrefixedReader::new(source, staging);
        if !peek.peek_magic()? {
            *staging = NextFrameStaging::Idle;
            return match position {
                StreamPosition::Start => Err(ReadFrameHeaderError::MagicNumberReadError(
                    Error::from(ErrorKind::UnexpectedEof),
                )
                .into()),
                StreamPosition::AfterFrame => Ok(FrameStart::CleanEnd),
            };
        }
        if is_skippable_magic(peek.magic()) {
            peek.skip_skippable()?;
            continue;
        }
        decoder.reset(&mut peek)?;
        *staging = NextFrameStaging::Idle;
        return Ok(FrameStart::Frame);
    }
}

fn eof_error() -> Error {
    Error::from(ErrorKind::UnexpectedEof)
}

fn magic_read_error(e: Error) -> FrameDecoderError {
    FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::MagicNumberReadError(e))
}

/// The active header stage of a hunt. Callers borrow only the staging, so
/// the source stays usable alongside it.
fn header_stage(staging: &mut NextFrameStaging) -> (&mut [u8; MAX_HEADER_LEN], &mut usize) {
    match staging {
        NextFrameStaging::Header { buf, filled } => (buf, filled),
        // `PrefixedReader::new` upgrades Idle, and the hunt drains
        // SkipContent before building a reader.
        NextFrameStaging::Idle | NextFrameStaging::SkipContent { .. } => {
            unreachable!("prefixed readers only run over header staging")
        },
    }
}

/// Trash a skippable frame's content from the source, tracking what is left
/// in `staging` so a failed attempt resumes the discard instead of losing
/// position.
fn discard_skippable<R: Read>(
    inner: &mut R,
    staging: &mut NextFrameStaging,
) -> Result<(), FrameDecoderError> {
    let NextFrameStaging::SkipContent { remaining } = staging else {
        unreachable!("skip discards only run over skip staging");
    };
    let mut trash = [0u8; 8 * 1024];
    while *remaining > 0 {
        let take = (*remaining).min(trash.len() as u64) as usize;
        match inner.read(&mut trash[..take]) {
            Ok(0) => return Err(FrameDecoderError::FailedToSkipFrame),
            Ok(n) => *remaining -= n as u64,
            Err(e) if e.kind() == ErrorKind::Interrupted => {},
            Err(e) => return Err(magic_read_error(e)),
        }
    }
    Ok(())
}

/// Serves the staged header bytes first, then the inner reader, appending
/// everything it pulls to the stage. The frame-header parser reads exact
/// amounts bounded by [`MAX_HEADER_LEN`], so nothing beyond the prefix is
/// ever taken from the inner reader on behalf of the caller and the stage
/// always covers every byte an interrupted attempt consumed.
struct PrefixedReader<'a, R: Read> {
    inner: &'a mut R,
    staging: &'a mut NextFrameStaging,
    /// Staged bytes already served as header bytes this attempt.
    served: usize,
}

impl<'a, R: Read> PrefixedReader<'a, R> {
    /// A fresh attempt replays from the first staged byte; `Idle` staging
    /// starts a new one.
    fn new(inner: &'a mut R, staging: &'a mut NextFrameStaging) -> Self {
        if matches!(staging, NextFrameStaging::Idle) {
            *staging = NextFrameStaging::Header {
                buf: [0; MAX_HEADER_LEN],
                filled: 0,
            };
        }
        Self {
            inner,
            staging,
            served: 0,
        }
    }

    /// Read the magic number strictly. `Ok(false)` means the inner reader
    /// ended cleanly before the first byte (a frame boundary); a partial
    /// magic number surfaces as the same error the header parser produces.
    fn peek_magic(&mut self) -> Result<bool, FrameDecoderError> {
        let (buf, filled) = header_stage(self.staging);
        while *filled < MAGIC_LEN {
            // Pull at most the magic: the bytes are staged for this hunt,
            // and anything read past them would be lost when the hunt ends.
            match self.inner.read(&mut buf[*filled..MAGIC_LEN]) {
                Ok(0) if *filled == 0 => return Ok(false),
                Ok(0) => return Err(magic_read_error(eof_error())),
                Ok(n) => *filled += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {},
                Err(e) => return Err(magic_read_error(e)),
            }
        }
        Ok(true)
    }

    fn magic(&self) -> u32 {
        let NextFrameStaging::Header { buf, .. } = &*self.staging else {
            unreachable!("prefixed readers only run over header staging");
        };
        u32::from_le_bytes(buf[..MAGIC_LEN].try_into().expect("MAGIC_LEN == 4"))
    }

    /// Skip a skippable frame behind the (already peeked) magic number.
    /// Leaves the staging holding the content bytes still to discard; the
    /// hunt drains it (to `Idle`) before the next frame, so a failed skip
    /// resumes instead of re-reading consumed bytes.
    fn skip_skippable(&mut self) -> Result<(), FrameDecoderError> {
        // The header parser never runs for this frame, so the peeked magic
        // must not be served as frame bytes: mark it consumed first.
        self.served = MAGIC_LEN;
        let mut len_bytes = [0u8; SKIP_SIZE_LEN];
        self.read_exact(&mut len_bytes)?;
        let remaining = u64::from(u32::from_le_bytes(len_bytes));
        *self.staging = NextFrameStaging::SkipContent { remaining };
        discard_skippable(self.inner, self.staging)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), FrameDecoderError> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]) {
                Ok(0) => return Err(magic_read_error(eof_error())),
                Ok(n) => filled += n,
                Err(e) => return Err(magic_read_error(e)),
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for PrefixedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let NextFrameStaging::Header {
            buf: staged,
            filled,
        } = &mut *self.staging
        else {
            unreachable!("prefixed readers only run over header staging");
        };
        // Replay staged bytes first: a retried attempt must not re-read what
        // the source already served. `served <= filled` always holds because
        // the parser reads exact amounts within MAX_HEADER_LEN.
        let outstanding = filled.saturating_sub(self.served);
        if outstanding > 0 {
            let take = outstanding.min(buf.len());
            buf[..take].copy_from_slice(&staged[self.served..self.served + take]);
            self.served += take;
            return Ok(take);
        }
        let n = self.inner.read(buf)?;
        // Stage every pulled byte so a failed attempt can replay it.
        let staged_now = n.min(MAX_HEADER_LEN - *filled);
        staged[*filled..*filled + staged_now].copy_from_slice(&buf[..staged_now]);
        *filled += staged_now;
        self.served += n;
        Ok(n)
    }
}
