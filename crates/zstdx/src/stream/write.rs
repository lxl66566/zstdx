//! io::Write-shaped streaming encoders and decoders.

use alloc::vec::Vec;

use super::encoder_core::FrameEncoderCore;
use crate::{
    DecoderOptions, EncoderOptions, Level, Result,
    decoding::{
        BlockDecodingStrategy, FrameDecoder,
        errors::{FrameDecoderError, ReadFrameHeaderError},
    },
    io::{Error, Write},
};

/// Compress data and write it to an underlying [`Write`].
///
/// Data is encoded block by block as it arrives; the frame header is written
/// with the first block and the frame is closed by [`Encoder::finish`].
/// Dropping the encoder without finishing loses the unflushed tail, exactly
/// like the libzstd bindings.
///
/// ```rust
/// use std::io::Write;
///
/// use zstdx::{Level, stream::write::Encoder};
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
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
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
    // the Err variant hands the encoder back to the caller; boxing it would
    // change the public signature
    #[allow(clippy::result_large_err)]
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
        assert!(!self.core.is_finished(), "write after finish");
        self.core.write(buf);
        self.drain()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Error> {
        assert!(!self.core.is_finished(), "flush after finish");
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

/// Decompress a zstd stream fed through [`Write`] into an underlying writer.
///
/// Concatenated and skippable frames are decoded transparently; the input is
/// processed as far as it arrives, so decoding cost follows the writes.
///
/// ```rust
/// use std::io::Write;
///
/// use zstdx::stream::write::Decoder;
///
/// let compressed = zstdx::bulk::compress(b"payload", zstdx::Level::Fastest);
/// let mut sink = Vec::new();
/// let mut dec = Decoder::new(&mut sink).unwrap();
/// dec.write_all(&compressed).unwrap();
/// dec.flush().unwrap();
/// assert_eq!(sink, b"payload");
/// ```
pub struct Decoder<W: Write> {
    writer: W,
    inner: FrameDecoder,
    /// Compressed bytes that arrived but were not consumed yet.
    input: Vec<u8>,
    /// False until a frame header was parsed from the input; reset at every
    /// frame boundary.
    inited: bool,
    /// Whether the frame parsed last announced a content checksum, so the
    /// final block's 4-byte trailer can be accounted for before decoding.
    checksummed: bool,
}

impl<W: Write> Decoder<W> {
    /// Create a decoder with default options.
    pub fn new(writer: W) -> Result<Self> {
        Self::with_options(writer, DecoderOptions::new())
    }

    /// Create a decoder from a builder option set.
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_options(writer: W, options: DecoderOptions) -> Result<Self> {
        let mut inner = FrameDecoder::new();
        if let Some(max) = options.max_window_size {
            inner.set_max_window_size(max);
        }
        if let Some(dict) = &options.dictionary {
            let dict =
                crate::decoding::Dictionary::decode_dict(dict).map_err(crate::Error::Dictionary)?;
            inner.add_dict(dict)?;
        }
        Ok(Self {
            writer,
            inner,
            input: Vec::new(),
            inited: false,
            checksummed: false,
        })
    }

    /// Upper bound on the window size of frames not yet decoded; values
    /// above the format maximum are clamped.
    pub fn set_max_window_size(&mut self, max: u64) -> Result<()> {
        self.inner.set_max_window_size(max);
        Ok(())
    }

    /// Recommended size of write batches: one full block.
    pub fn recommended_input_size() -> usize {
        crate::common::MAX_BLOCK_SIZE as usize
    }

    /// Acquires a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Acquires a mutable reference to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Destructures this object into the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Wrap this decoder so the underlying writer is flushed after every
    /// write.
    pub fn auto_flush(self) -> AutoFlushDecoder<W> {
        AutoFlushDecoder { decoder: self }
    }

    /// Decode as much of the staged input as possible into the writer.
    ///
    /// The FrameDecoder errors out (and poisons itself) when a block's bytes
    /// are incomplete, so blocks are only handed over once their full extent
    /// is staged: header plus body plus, behind the last block of a
    /// checksummed frame, the 4-byte trailer.
    fn pump(&mut self) -> Result<()> {
        loop {
            if !self.inited {
                if !self.init_frame()? {
                    return Ok(());
                }
                continue;
            }
            if self.inner.is_finished() {
                self.inner.collect_to_writer(&mut self.writer)?;
                if self.input.is_empty() {
                    return Ok(());
                }
                self.inited = false;
                continue;
            }
            match self.next_block_need()? {
                Some(need) if self.input.len() >= need => {},
                _ => return Ok(()),
            }
            let read_before = self.inner.bytes_read_from_source();
            let mut source = &self.input[..];
            self.inner
                .decode_blocks(&mut source, BlockDecodingStrategy::UptoBlocks(1))
                .map_err(|e| crate::error::into_io(crate::Error::Frame(e)))?;
            let consumed = (self.inner.bytes_read_from_source() - read_before) as usize;
            self.input.drain(..consumed);
            self.inner.collect_to_writer(&mut self.writer)?;
        }
    }

    /// Bytes the next block needs before it can be decoded safely:
    /// `None` when too little is staged to even parse its header.
    fn next_block_need(&self) -> Result<Option<usize>> {
        if self.input.len() < 3 {
            return Ok(None);
        }
        let [b0, b1, b2, ..] = self.input[..] else {
            return Ok(None);
        };
        let last = b0 & 1 == 1;
        let block_type = (b0 >> 1) & 0b11;
        let size = ((b0 >> 3) as usize) | ((b1 as usize) << 5) | ((b2 as usize) << 13);
        let body = match block_type {
            0 | 2 => size, // Raw / Compressed carry `size` content bytes
            1 => 1,        // RLE encodes a single byte
            _ => {
                return Err(crate::Error::Frame(
                    FrameDecoderError::FailedToReadBlockHeader(
                        crate::decoding::errors::BlockHeaderReadError::FoundReservedBlock,
                    ),
                ))
            },
        };
        let trailer = if last && self.checksummed {
            4
        } else {
            0
        };
        Ok(Some(3 + body + trailer))
    }

    /// Parse the next frame header from the staged input, consuming
    /// skippable frames on the way. Returns `false` when more input is
    /// needed; the staging is then left untouched for the retry.
    fn init_frame(&mut self) -> Result<bool> {
        loop {
            if self.input.len() < 4 {
                return Ok(false);
            }
            // Pre-parse the header for the checksum flag (the decoder itself
            // keeps that flag private). A starvation error here just means
            // the header is not fully staged yet.
            let mut probe = &self.input[..];
            let checksummed = match crate::decoding::frame::read_frame_header(&mut probe) {
                Ok((header, _)) => header.descriptor.content_checksum_flag(),
                Err(ReadFrameHeaderError::SkipFrame { length, .. }) => {
                    let total = 8 + length as usize;
                    if self.input.len() < total {
                        return Ok(false);
                    }
                    self.input.drain(..total);
                    continue;
                },
                Err(e) => {
                    let starved = matches!(&e,
                        ReadFrameHeaderError::MagicNumberReadError(io)
                        | ReadFrameHeaderError::FrameDescriptorReadError(io)
                        | ReadFrameHeaderError::WindowDescriptorReadError(io)
                        | ReadFrameHeaderError::DictionaryIdReadError(io)
                        | ReadFrameHeaderError::FrameContentSizeReadError(io)
                        if io.kind() == crate::io::ErrorKind::UnexpectedEof);
                    if starved {
                        return Ok(false);
                    }
                    return Err(FrameDecoderError::ReadFrameHeaderError(e).into());
                },
            };
            let mut source = &self.input[..];
            self.inner.reset(&mut source).map_err(crate::Error::Frame)?;
            let consumed = self.input.len() - source.len();
            self.input.drain(..consumed);
            self.checksummed = checksummed;
            self.inited = true;
            return Ok(true);
        }
    }
}

impl<W: Write> Write for Decoder<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.input.extend_from_slice(buf);
        self.pump().map_err(crate::error::into_io)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.pump().map_err(crate::error::into_io)?;
        self.writer.flush()
    }
}

/// A [`Decoder`] that flushes the underlying writer after every write.
pub struct AutoFlushDecoder<W: Write> {
    decoder: Decoder<W>,
}

impl<W: Write> AutoFlushDecoder<W> {
    /// Acquires a reference to the underlying writer.
    pub fn get_ref(&self) -> &W {
        self.decoder.get_ref()
    }

    /// Acquires a mutable reference to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        self.decoder.get_mut()
    }
}

impl<W: Write> Write for AutoFlushDecoder<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let res = self.decoder.write(buf);
        if res.is_ok() {
            self.decoder.flush()?;
        }
        res
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.decoder.flush()
    }
}
