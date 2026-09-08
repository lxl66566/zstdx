//! io::Write-shaped streaming encoder.

use super::encoder_core::FrameEncoderCore;
use crate::io::{Error, Write};
use crate::{EncoderOptions, Level, Result};

/// Compress data and write it to an underlying [`Write`].
///
/// Data is encoded block by block as it arrives; the frame header is written
/// with the first block and the frame is closed by [`Encoder::finish`].
/// Dropping the encoder without finishing loses the unflushed tail, exactly
/// like the libzstd bindings.
///
/// ```rust
/// use ruzstd::stream::write::Encoder;
/// use ruzstd::Level;
/// use std::io::Write;
///
/// let mut enc = Encoder::new(Vec::new(), Level::Fastest).unwrap();
/// enc.write_all(b"the quick brown fox").unwrap();
/// let compressed = enc.finish().unwrap();
/// ```
pub struct Encoder<W: Write> {
    writer: Option<W>,
    core: FrameEncoderCore,
}

impl<W: Write> Encoder<W> {
    /// Create an encoder with default options (checksum on, single frame,
    /// single thread). Infallible: no bytes are written until the first
    /// block is complete or the stream is finished.
    pub fn new(writer: W, level: Level) -> Result<Self> {
        Self::with_options(writer, EncoderOptions::new(level))
    }

    /// Create an encoder from a builder option set.
    pub fn with_options(writer: W, options: EncoderOptions) -> Result<Self> {
        Ok(Self {
            writer: Some(writer),
            core: FrameEncoderCore::new(&options)?,
        })
    }

    /// Wrap this encoder so the frame is finished when it is dropped.
    pub fn auto_finish(self) -> AutoFinishEncoder<W> {
        AutoFinishEncoder {
            encoder: Some(self),
            on_finish: None,
        }
    }

    /// Like [`Encoder::auto_finish`], but the callback receives the result
    /// of the implicit `finish()` (and the inner writer on success). It runs
    /// during drop, so it must not panic.
    pub fn on_finish<F: FnMut(Result<W>)>(self, f: F) -> AutoFinishEncoder<W, F> {
        AutoFinishEncoder {
            encoder: Some(self),
            on_finish: Some(f),
        }
    }

    /// Finish the stream and return the inner writer.
    pub fn finish(mut self) -> Result<W> {
        self.do_finish()?;
        Ok(self.writer.take().unwrap())
    }

    /// Finish the stream, handing back the encoder on failure.
    pub fn try_finish(mut self) -> Result<W, (Self, crate::Error)> {
        match self.do_finish() {
            Ok(()) => Ok(self.writer.take().unwrap()),
            Err(e) => Err((self, e)),
        }
    }

    /// Finish the stream without consuming the encoder; every following
    /// [`Write::write`] panics, mirroring the libzstd bindings.
    pub fn do_finish(&mut self) -> Result<()> {
        self.core.finish();
        self.drain()
    }

    /// Recommended size for write batches: one full block.
    pub fn recommended_input_size() -> usize {
        crate::common::MAX_BLOCK_SIZE as usize
    }

    /// Acquires a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        self.writer.as_ref().unwrap()
    }

    /// Acquires a mutable reference to the underlying writer.
    ///
    /// Note that mutation of the writer may result in surprising results
    /// (as in: broken output) if the encoder is used afterwards.
    pub fn get_mut(&mut self) -> &mut W {
        self.writer.as_mut().unwrap()
    }

    fn drain(&mut self) -> Result<()> {
        let writer = self.writer.as_mut().unwrap();
        self.core
            .write_output_to(writer)
            .map_err(crate::Error::from)
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        if self.core.is_finished() {
            panic!("write after finish");
        }
        self.core.write(buf);
        self.drain()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Error> {
        if self.core.is_finished() {
            panic!("flush after finish");
        }
        // Emit the staged partial block early so a reader can make progress;
        // the inner writer is flushed afterwards.
        self.core.flush_block();
        self.drain()?;
        self.writer.as_mut().unwrap().flush()
    }
}

/// An [`Encoder`] that finishes its frame on drop. Errors from that implicit
/// finish are reported through the [`Encoder::on_finish`] callback if one was
/// installed, and dropped otherwise.
pub struct AutoFinishEncoder<W: Write, F: FnMut(Result<W>) = fn(Result<W>)> {
    encoder: Option<Encoder<W>>,
    on_finish: Option<F>,
}

impl<W: Write, F: FnMut(Result<W>)> AutoFinishEncoder<W, F> {
    /// Acquires a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        self.encoder.as_ref().unwrap().get_ref()
    }

    /// Acquires a mutable reference to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        self.encoder.as_mut().unwrap().get_mut()
    }
}

impl<W: Write, F: FnMut(Result<W>)> Write for AutoFinishEncoder<W, F> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.encoder.as_mut().unwrap().write(buf)
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.encoder.as_mut().unwrap().flush()
    }
}

impl<W: Write, F: FnMut(Result<W>)> Drop for AutoFinishEncoder<W, F> {
    fn drop(&mut self) {
        if let Some(encoder) = self.encoder.take() {
            let result = encoder.finish();
            if let Some(f) = self.on_finish.as_mut() {
                f(result);
            }
        }
    }
}
