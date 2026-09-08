//! io::Read-shaped streaming types.

use super::encoder_core::FrameEncoderCore;
use crate::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use crate::decoding::{BlockDecodingStrategy, FrameDecoder};
use crate::io::{Error, Read};
use crate::{DecoderOptions, EncoderOptions, Level, Result};

/// Compress data pulled from an underlying [`Read`] and expose the encoded
/// frame through [`Read`].
///
/// The frame is closed once the source reports end of file; reads after that
/// return `Ok(0)`.
///
/// ```rust
/// use ruzstd::stream::read::Encoder;
/// use ruzstd::Level;
/// use std::io::Read;
///
/// let mut enc = Encoder::new(b"the quick brown fox".as_slice(), Level::Fastest).unwrap();
/// let mut compressed = Vec::new();
/// enc.read_to_end(&mut compressed).unwrap();
/// assert!(!compressed.is_empty());
/// ```
pub struct Encoder<R: Read> {
    source: Option<R>,
    core: FrameEncoderCore,
}

impl<R: Read> Encoder<R> {
    /// Create an encoder with default options.
    pub fn new(source: R, level: Level) -> Result<Self> {
        Self::with_options(source, EncoderOptions::new(level))
    }

    /// Create an encoder from a builder option set.
    pub fn with_options(source: R, options: EncoderOptions) -> Result<Self> {
        Ok(Self {
            source: Some(source),
            core: FrameEncoderCore::new(&options)?,
        })
    }

    /// Recommended size for read buffers: one full block.
    pub fn recommended_output_size() -> usize {
        crate::common::MAX_BLOCK_SIZE as usize
    }

    /// Acquires a reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        self.source.as_ref().unwrap()
    }

    /// Acquires a mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        self.source.as_mut().unwrap()
    }

    /// Reclaim the underlying reader. The frame is closed with whatever has
    /// been consumed so far; the rest of the source stays unread.
    pub fn finish(mut self) -> R {
        self.core.finish();
        self.source.take().unwrap()
    }
}

impl<R: Read> Read for Encoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        while !self.core.has_output() && !self.core.is_finished() {
            self.core.pump_from(self.source.as_mut().unwrap())?;
        }
        Ok(self.core.split_output(buf))
    }
}

/// Decompress a zstd stream while reading from it.
///
/// Unlike [`crate::decoding::StreamingDecoder`] (which is documented to
/// decode a single frame), this decoder is transparent over concatenated
/// frames and skippable frames; call [`Decoder::single_frame`] to stop after
/// the first instead.
///
/// ```rust
/// use ruzstd::stream::read::Decoder;
/// use std::io::Read;
///
/// let compressed = ruzstd::bulk::compress(b"a b c b a", ruzstd::Level::Fastest);
/// let mut dec = Decoder::new(&compressed[..]).unwrap();
/// let mut output = Vec::new();
/// dec.read_to_end(&mut output).unwrap();
/// assert_eq!(output, b"a b c b a");
/// ```
pub struct Decoder<R: Read> {
    source: R,
    decoder: FrameDecoder,
    single_frame: bool,
    /// True once the last allowed frame is decoded and drained.
    finished: bool,
}

impl<R: Read> Decoder<R> {
    /// Create a decoder with default options; reads the first frame header
    /// immediately (an empty stream is an error, as in the libzstd bindings).
    pub fn new(source: R) -> Result<Self> {
        Self::with_options(source, DecoderOptions::new())
    }

    /// Create a decoder from a builder option set.
    pub fn with_options(mut source: R, options: DecoderOptions) -> Result<Self> {
        let mut decoder = FrameDecoder::new();
        if let Some(max) = options.max_window_size {
            decoder.set_max_window_size(max);
        }
        if let Some(dict) = options.dictionary {
            decoder.add_dict(dict)?;
        }
        Self::init_first_frame(&mut source, &mut decoder)?;
        Ok(Self {
            source,
            decoder,
            single_frame: false,
            finished: false,
        })
    }

    /// Restrict decoding to the first frame; reads return `Ok(0)` once it is
    /// drained even if more frames follow.
    pub fn single_frame(mut self) -> Self {
        self.single_frame = true;
        self
    }

    /// Acquires a reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.source
    }

    /// Acquires a mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.source
    }

    /// Reclaim the underlying reader.
    pub fn finish(self) -> R {
        self.source
    }

    fn init_first_frame(source: &mut R, decoder: &mut FrameDecoder) -> Result<()> {
        let mut peek = PrefixedReader::new(source);
        if !peek.peek_magic()? {
            return Err(FrameDecoderError::ReadFrameHeaderError(
                ReadFrameHeaderError::MagicNumberReadError(eof_error()),
            )
            .into());
        }
        decoder.reset(&mut peek)?;
        Ok(())
    }

    /// Serve collectible bytes or decode more blocks; `Ok(0)` marks the end
    /// of the allowed frames.
    fn pump(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        loop {
            if self.finished {
                return Ok(0);
            }
            if self.decoder.can_collect() > 0 {
                return self.decoder.read(buf);
            }
            if self.decoder.is_finished() {
                if self.single_frame || !self.init_next_frame()? {
                    self.finished = true;
                    return Ok(0);
                }
                continue;
            }
            self.decoder
                .decode_blocks(&mut self.source, BlockDecodingStrategy::UptoBlocks(1))
                .map_err(|e| crate::error::into_io(crate::Error::Frame(e)))?;
        }
    }

    /// Initialize the next frame from the source. Returns `false` on a clean
    /// end of stream; consumes skippable frames transparently.
    fn init_next_frame(&mut self) -> Result<bool> {
        loop {
            let mut peek = PrefixedReader::new(&mut self.source);
            if !peek.peek_magic()? {
                return Ok(false);
            }
            if (0x184D2A50..=0x184D2A5F).contains(&peek.magic()) {
                peek.skip_skippable()?;
                continue;
            }
            self.decoder.reset(&mut peek)?;
            return Ok(true);
        }
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.pump(buf)
    }
}

fn eof_error() -> Error {
    Error::from(crate::io::ErrorKind::UnexpectedEof)
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
    fn peek_magic(&mut self) -> Result<bool> {
        while self.filled < 4 {
            match self.inner.read(&mut self.magic[self.filled..]) {
                Ok(0) if self.filled == 0 => return Ok(false),
                Ok(0) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(eof_error()),
                    )
                    .into())
                }
                Ok(n) => self.filled += n,
                Err(e) if e.kind() == crate::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::MagicNumberReadError(e),
                    )
                    .into())
                }
            }
        }
        Ok(true)
    }

    fn magic(&self) -> u32 {
        u32::from_le_bytes(self.magic)
    }

    /// Skip a skippable frame behind the (already peeked) magic number.
    fn skip_skippable(&mut self) -> Result<()> {
        // The header parser never runs for this frame, so the peeked magic
        // must not be served as frame bytes: discard the prefix first.
        self.served = self.filled;
        let mut len_bytes = [0u8; 4];
        self.read_exact(&mut len_bytes)?;
        let mut left = u32::from_le_bytes(len_bytes) as usize;
        let mut trash = [0u8; 8 * 1024];
        while left > 0 {
            let take = left.min(trash.len());
            let n = self.read(&mut trash[..take])?;
            if n == 0 {
                return Err(FrameDecoderError::FailedToSkipFrame.into());
            }
            left -= n;
        }
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..])? {
                0 => return Err(eof_error()),
                n => filled += n,
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
