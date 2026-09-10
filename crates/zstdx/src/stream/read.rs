//! io::Read-shaped streaming types.

use super::encoder_core::FrameEncoderCore;
use crate::decoding::frame_source;
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
/// use zstdx::stream::read::Encoder;
/// use zstdx::Level;
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
/// Like [`crate::decoding::StreamingDecoder`], this decoder is transparent
/// over concatenated frames and skippable frames; it additionally offers
/// [`Decoder::single_frame`] to stop after the first one.
///
/// ```rust
/// use zstdx::stream::read::Decoder;
/// use std::io::Read;
///
/// let compressed = zstdx::bulk::compress(b"a b c b a", zstdx::Level::Fastest);
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
        if let Some(dict) = &options.dictionary {
            let dict =
                crate::decoding::Dictionary::decode_dict(dict).map_err(crate::Error::Dictionary)?;
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

    /// Recommended size for read batches: one full block.
    pub fn recommended_output_size() -> usize {
        crate::common::MAX_BLOCK_SIZE as usize
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
        frame_source::init_first_frame(source, decoder)?;
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
                let next = frame_source::init_next_frame(&mut self.source, &mut self.decoder)
                    .map_err(|e| crate::error::into_io(crate::Error::Frame(e)));
                if self.single_frame || !next? {
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
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.pump(buf)
    }
}
