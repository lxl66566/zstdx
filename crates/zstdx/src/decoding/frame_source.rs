//! Frame-boundary plumbing shared by the streaming decoders ([`FrameDecoder`]
//! re-initialization): peek the next frame's magic number, skip skippable
//! frames, and replay the peeked bytes in front of the source.

use crate::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use crate::decoding::frame::is_skippable_magic;
use crate::decoding::FrameDecoder;
use crate::io::{Error, ErrorKind, Read};

/// Where the decoder stands when asking for the next frame.
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
    loop {
        let mut peek = PrefixedReader::new(source);
        if !peek.peek_magic()? {
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
        return Ok(FrameStart::Frame);
    }
}

fn eof_error() -> Error {
    Error::from(ErrorKind::UnexpectedEof)
}

/// Serves the four magic-number bytes it read ahead, then the inner reader.
/// The frame-header parser reads exact amounts, so nothing beyond the prefix
/// is ever taken from the inner reader on behalf of the caller.
struct PrefixedReader<'a, R: Read> {
    inner: &'a mut R,
    magic: [u8; 4],
    /// Magic bytes already read from the inner reader.
    filled: usize,
    /// Magic bytes already handed out through [`Read`].
    served: usize,
}

impl<'a, R: Read> PrefixedReader<'a, R> {
    fn new(inner: &'a mut R) -> Self {
        Self {
            inner,
            magic: [0; 4],
            filled: 0,
            served: 0,
        }
    }

    /// Read the magic number strictly. `Ok(false)` means the inner reader
    /// ended cleanly before the first byte (a frame boundary); a partial
    /// magic number surfaces as the same error the header parser produces.
    fn peek_magic(&mut self) -> Result<bool, FrameDecoderError> {
        while self.filled < 4 {
            match self.inner.read(&mut self.magic[self.filled..]) {
                Ok(0) if self.filled == 0 => return Ok(false),
                Ok(0) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(eof_error()),
                    ))
                }
                Ok(n) => self.filled += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(e),
                    ))
                }
            }
        }
        Ok(true)
    }

    fn magic(&self) -> u32 {
        u32::from_le_bytes(self.magic)
    }

    /// Skip a skippable frame behind the (already peeked) magic number.
    fn skip_skippable(&mut self) -> Result<(), FrameDecoderError> {
        // The header parser never runs for this frame, so the peeked magic
        // must not be served as frame bytes: discard the prefix first.
        self.served = self.filled;
        let mut len_bytes = [0u8; 4];
        self.read_exact(&mut len_bytes)?;
        let mut left = u32::from_le_bytes(len_bytes) as usize;
        let mut trash = [0u8; 8 * 1024];
        while left > 0 {
            let take = left.min(trash.len());
            let n = self.read(&mut trash[..take]).map_err(|e| {
                FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::MagicNumberReadError(
                    e,
                ))
            })?;
            if n == 0 {
                return Err(FrameDecoderError::FailedToSkipFrame);
            }
            left -= n;
        }
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), FrameDecoderError> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]) {
                Ok(0) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(eof_error()),
                    ))
                }
                Ok(n) => filled += n,
                Err(e) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(e),
                    ))
                }
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for PrefixedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let outstanding = self.filled - self.served;
        if outstanding > 0 {
            let take = outstanding.min(buf.len());
            buf[..take].copy_from_slice(&self.magic[self.served..self.served + take]);
            self.served += take;
            return Ok(take);
        }
        self.inner.read(buf)
    }
}
