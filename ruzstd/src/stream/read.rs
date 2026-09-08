//! io::Read-shaped streaming types.

use super::encoder_core::FrameEncoderCore;
use crate::io::{Error, Read};
use crate::{EncoderOptions, Level, Result};

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
