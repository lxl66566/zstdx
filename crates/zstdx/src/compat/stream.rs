//! Streaming types mirroring `zstd::stream` (read/write submodules and the
//! one-shot functions).

use std::{
    io::{self, Read as _},
    vec::Vec,
};

use super::map_level;
use crate::{DecoderOptions, EncoderOptions};

/// Common state of the compat encoders: options are collected until the
/// stream starts, then materialized into the native encoder.
struct EncoderState<W: io::Write> {
    writer: Option<W>,
    options: EncoderOptions,
    encoder: Option<crate::stream::write::Encoder<W>>,
}

impl<W: io::Write> EncoderState<W> {
    fn new(writer: W, level: i32) -> Self {
        Self {
            writer: Some(writer),
            options: EncoderOptions::new(map_level(level)),
            encoder: None,
        }
    }

    fn set_started(&self) -> io::Result<()> {
        if self.encoder.is_some() {
            Err(io::Error::other(crate::Error::Parameter(
                crate::ParameterError::AlreadyStreaming,
            )))
        } else {
            Ok(())
        }
    }

    fn materialize(&mut self) -> io::Result<&mut crate::stream::write::Encoder<W>> {
        if self.encoder.is_none() {
            let writer = self.writer.take().expect("writer kept until start");
            let encoder = crate::stream::write::Encoder::with_options(writer, self.options.clone())
                .map_err(io::Error::from)?;
            self.encoder = Some(encoder);
        }
        Ok(self.encoder.as_mut().unwrap())
    }

    fn do_finish(&mut self) -> io::Result<()> {
        if let Some(encoder) = &mut self.encoder {
            encoder.do_finish().map_err(io::Error::from)
        } else {
            // empty stream: finish a never-started encoder to emit the
            // empty frame
            let encoder = self.materialize()?;
            encoder.do_finish().map_err(io::Error::from)
        }
    }

    fn take_writer(&mut self) -> W {
        match self.encoder.take() {
            Some(encoder) => encoder
                .finish()
                .expect("do_finish ran before the writer is reclaimed"),
            None => self.writer.take().expect("writer kept until start"),
        }
    }
}

pub mod write {
    //! io::Write-shaped types mirroring `zstd::stream::write`.

    use super::*;

    /// A compression encoder mirroring `zstd::stream::write::Encoder`.
    ///
    /// Infallible in practice (kept `io::Result` for source compatibility);
    /// parameters must be set before the first write.
    pub struct Encoder<W: io::Write> {
        state: EncoderState<W>,
    }

    impl<W: io::Write> Encoder<W> {
        /// Creates a new encoder. A level of 0 or any negative/positive
        /// value compresses with the fast strategy.
        pub fn new(writer: W, level: i32) -> io::Result<Self> {
            Ok(Self {
                state: EncoderState::new(writer, level),
            })
        }

        /// Creates an encoder bound to a dictionary.
        pub fn with_dictionary(writer: W, level: i32, dictionary: &[u8]) -> io::Result<Self> {
            let mut state = EncoderState::new(writer, level);
            state.options = state.options.dictionary(dictionary);
            Ok(Self { state })
        }

        /// Sets the pledged source size (written into the frame header; a
        /// stream writing a different size fails at finish); fails once
        /// streaming started.
        pub fn set_pledged_src_size(&mut self, size: Option<u64>) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.pledged_size = size;
            Ok(())
        }

        /// Includes or excludes the frame checksum (default on); fails once
        /// streaming started.
        pub fn include_checksum(&mut self, include: bool) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.checksum = include && cfg!(feature = "hash");
            Ok(())
        }

        /// Enables multithreaded compression; more than one worker is not
        /// implemented yet.
        pub fn multithread(&mut self, workers: u32) -> io::Result<()> {
            if workers > 1 {
                return Err(super::super::unsupported_io(crate::Feature::Multithread));
            }
            Ok(())
        }

        /// Returns a wrapper that finishes the stream on drop.
        pub fn auto_finish(self) -> AutoFinishEncoder<W> {
            AutoFinishEncoder {
                encoder: Some(self),
                on_finish: None,
            }
        }

        /// Returns a drop-finishing wrapper whose callback receives the
        /// result of the implicit finish. It runs during drop, so it must
        /// not panic.
        pub fn on_finish<F: FnMut(io::Result<W>)>(self, f: F) -> AutoFinishEncoder<W, F> {
            AutoFinishEncoder {
                encoder: Some(self),
                on_finish: Some(f),
            }
        }

        /// Finishes the stream and returns the inner writer.
        pub fn finish(mut self) -> io::Result<W> {
            self.do_finish()?;
            Ok(self.state.take_writer())
        }

        /// Finishes the stream, handing back the encoder on failure.
        // the Err variant hands the encoder back to the caller; boxing it would
        // deviate from the mirrored zstd-crate signature
        #[allow(clippy::result_large_err)]
        pub fn try_finish(mut self) -> Result<W, (Self, io::Error)> {
            match self.do_finish() {
                Ok(()) => Ok(self.state.take_writer()),
                Err(e) => Err((self, e)),
            }
        }

        /// Finishes the stream without consuming the encoder; writes panic
        /// afterwards.
        pub fn do_finish(&mut self) -> io::Result<()> {
            self.state.do_finish()
        }

        /// Recommended size of write batches: one full block.
        pub fn recommended_input_size() -> usize {
            crate::stream::write::Encoder::<W>::recommended_input_size()
        }

        /// Acquires a reference to the underlying writer.
        pub fn get_ref(&self) -> &W {
            match &self.state.encoder {
                Some(encoder) => encoder.get_ref(),
                None => self.state.writer.as_ref().expect("writer kept until start"),
            }
        }

        /// Acquires a mutable reference to the underlying writer.
        pub fn get_mut(&mut self) -> &mut W {
            match &mut self.state.encoder {
                Some(encoder) => encoder.get_mut(),
                None => self.state.writer.as_mut().expect("writer kept until start"),
            }
        }
    }

    impl<W: io::Write> io::Write for Encoder<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.state.materialize()?.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            match &mut self.state.encoder {
                Some(encoder) => encoder.flush(),
                None => Ok(()),
            }
        }
    }

    /// An encoder that finishes the stream on drop.
    pub struct AutoFinishEncoder<W: io::Write, F: FnMut(io::Result<W>) = fn(io::Result<W>)> {
        encoder: Option<Encoder<W>>,
        on_finish: Option<F>,
    }

    impl<W: io::Write, F: FnMut(io::Result<W>)> AutoFinishEncoder<W, F> {
        /// Acquires a reference to the underlying writer.
        pub fn get_ref(&self) -> &W {
            self.encoder.as_ref().unwrap().get_ref()
        }

        /// Acquires a mutable reference to the underlying writer.
        pub fn get_mut(&mut self) -> &mut W {
            self.encoder.as_mut().unwrap().get_mut()
        }
    }

    impl<W: io::Write, F: FnMut(io::Result<W>)> io::Write for AutoFinishEncoder<W, F> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.encoder.as_mut().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.encoder.as_mut().unwrap().flush()
        }
    }

    impl<W: io::Write, F: FnMut(io::Result<W>)> Drop for AutoFinishEncoder<W, F> {
        fn drop(&mut self) {
            if let Some(encoder) = self.encoder.take() {
                let result = encoder.finish();
                if let Some(f) = self.on_finish.as_mut() {
                    f(result);
                }
            }
        }
    }

    /// A decoder that decompresses written data into an underlying writer,
    /// mirroring `zstd::stream::write::Decoder`.
    pub struct Decoder<W: io::Write> {
        decoder: crate::stream::write::Decoder<W>,
    }

    impl<W: io::Write> Decoder<W> {
        /// Creates a new decoder.
        pub fn new(writer: W) -> io::Result<Self> {
            Ok(Self {
                decoder: crate::stream::write::Decoder::new(writer).map_err(io::Error::from)?,
            })
        }

        /// Creates a decoder bound to a dictionary.
        pub fn with_dictionary(writer: W, dictionary: &[u8]) -> io::Result<Self> {
            Ok(Self {
                decoder: crate::stream::write::Decoder::with_options(
                    writer,
                    DecoderOptions::new().dictionary(dictionary),
                )
                .map_err(io::Error::from)?,
            })
        }

        /// Sets the maximum window log distance (log2 of bytes); applies to
        /// frames not yet decoded.
        pub fn window_log_max(&mut self, log: u32) -> io::Result<()> {
            self.decoder
                .set_max_window_size(1u64 << log)
                .map_err(io::Error::from)
        }

        /// Returns a wrapper that flushes the writer after each write.
        pub fn auto_flush(self) -> AutoFlushDecoder<W> {
            AutoFlushDecoder {
                decoder: Some(self),
            }
        }

        /// Acquires a reference to the underlying writer.
        pub fn get_ref(&self) -> &W {
            self.decoder.get_ref()
        }

        /// Acquires a mutable reference to the underlying writer.
        pub fn get_mut(&mut self) -> &mut W {
            self.decoder.get_mut()
        }

        /// Destructures this object into the underlying writer.
        pub fn into_inner(self) -> W {
            self.decoder.into_inner()
        }

        /// Recommended size of write batches: one full block.
        pub fn recommended_input_size() -> usize {
            crate::stream::write::Decoder::<W>::recommended_input_size()
        }
    }

    impl<W: io::Write> io::Write for Decoder<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.decoder.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.decoder.flush()
        }
    }

    /// A decoder that flushes its writer after each write.
    pub struct AutoFlushDecoder<W: io::Write> {
        decoder: Option<Decoder<W>>,
    }

    impl<W: io::Write> AutoFlushDecoder<W> {
        /// Acquires a reference to the underlying writer.
        pub fn get_ref(&self) -> &W {
            self.decoder.as_ref().unwrap().get_ref()
        }

        /// Acquires a mutable reference to the underlying writer.
        pub fn get_mut(&mut self) -> &mut W {
            self.decoder.as_mut().unwrap().get_mut()
        }

        /// Returns the wrapped decoder.
        pub fn into_inner(mut self) -> Decoder<W> {
            self.decoder.take().expect("decoder kept until drop")
        }
    }

    impl<W: io::Write> io::Write for AutoFlushDecoder<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let res = self.decoder.as_mut().unwrap().write(buf);
            if res.is_ok() {
                self.decoder.as_mut().unwrap().flush()?;
            }
            res
        }

        fn flush(&mut self) -> io::Result<()> {
            self.decoder.as_mut().unwrap().flush()
        }
    }
}

pub mod read {
    //! io::Read-shaped types mirroring `zstd::stream::read`.

    use super::*;

    /// A compression encoder exposing compressed bytes through `io::Read`,
    /// mirroring `zstd::stream::read::Encoder`.
    pub struct Encoder<R: io::Read> {
        inner: crate::stream::read::Encoder<R>,
    }

    impl<R: io::Read> Encoder<R> {
        /// Creates a new encoder over the reader.
        pub fn new(source: R, level: i32) -> io::Result<Self> {
            Ok(Self {
                inner: crate::stream::read::Encoder::new(source, map_level(level))
                    .map_err(io::Error::from)?,
            })
        }

        /// Recommended size of read batches: one full block.
        pub fn recommended_output_size() -> usize {
            crate::stream::read::Encoder::<R>::recommended_output_size()
        }

        /// Acquires a reference to the underlying reader.
        pub fn get_ref(&self) -> &R {
            self.inner.get_ref()
        }

        /// Acquires a mutable reference to the underlying reader.
        pub fn get_mut(&mut self) -> &mut R {
            self.inner.get_mut()
        }

        /// Destructures this object into the underlying reader. Fails when a
        /// pledged content size was not met by the bytes consumed so far, or
        /// when encoded bytes are still unread (read the encoder to end of
        /// stream before finishing).
        pub fn finish(self) -> io::Result<R> {
            self.inner.finish().map_err(io::Error::from)
        }

        /// Tries to fill `out` with encoded bytes.
        pub fn flush(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.inner.read(out)
        }
    }

    impl<R: io::Read> io::Read for Encoder<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    /// A decompression decoder mirroring `zstd::stream::read::Decoder`.
    pub struct Decoder<R: io::Read> {
        source: Option<R>,
        options: DecoderOptions,
        inner: Option<crate::stream::read::Decoder<R>>,
        single_frame: bool,
    }

    impl<R: io::Read> Decoder<R> {
        /// Creates a new decoder; the first frame header is parsed lazily on
        /// the first read so `window_log_max` can still be applied.
        pub fn new(source: R) -> io::Result<Self> {
            Ok(Self {
                source: Some(source),
                options: DecoderOptions::new(),
                inner: None,
                single_frame: false,
            })
        }

        /// Creates a new decoder bound to a dictionary.
        pub fn with_dictionary(source: R, dictionary: &[u8]) -> io::Result<Self> {
            Ok(Self {
                source: Some(source),
                options: DecoderOptions::new().dictionary(dictionary),
                inner: None,
                single_frame: false,
            })
        }

        /// Restricts decoding to the first frame.
        pub fn single_frame(mut self) -> Self {
            self.single_frame = true;
            self
        }

        /// Sets the maximum window log distance (log2 of bytes); must be
        /// called before the first read.
        pub fn window_log_max(&mut self, log: u32) -> io::Result<()> {
            if self.inner.is_some() {
                return Err(io::Error::other(crate::Error::Parameter(
                    crate::ParameterError::AlreadyStreaming,
                )));
            }
            self.options.max_window_size = Some(1u64 << log);
            Ok(())
        }

        /// Recommended size of read batches: one full block.
        pub fn recommended_output_size() -> usize {
            crate::stream::read::Decoder::<R>::recommended_output_size()
        }

        /// Acquires a reference to the underlying reader.
        pub fn get_ref(&self) -> &R {
            match &self.inner {
                Some(decoder) => decoder.get_ref(),
                None => self.source.as_ref().expect("source kept until start"),
            }
        }

        /// Acquires a mutable reference to the underlying reader.
        ///
        /// It is inadvisable to directly read from the underlying reader.
        pub fn get_mut(&mut self) -> &mut R {
            match &mut self.inner {
                Some(decoder) => decoder.get_mut(),
                None => self.source.as_mut().expect("source kept until start"),
            }
        }

        /// Destructures this object into the underlying reader.
        pub fn finish(self) -> R {
            match self.inner {
                Some(decoder) => decoder.finish(),
                None => self.source.expect("source kept until start"),
            }
        }

        fn materialize(&mut self) -> io::Result<&mut crate::stream::read::Decoder<R>> {
            if self.inner.is_none() {
                let source = self.source.take().expect("source kept until start");
                let mut decoder =
                    crate::stream::read::Decoder::with_options(source, self.options.clone())
                        .map_err(io::Error::from)?;
                if self.single_frame {
                    decoder = decoder.single_frame();
                }
                self.inner = Some(decoder);
            }
            Ok(self.inner.as_mut().unwrap())
        }
    }

    impl<R: io::Read> io::Read for Decoder<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.materialize()?.read(buf)
        }
    }
}

/// Compresses everything `source` provides into a Vec.
pub fn encode_all<R: io::Read>(source: R, level: i32) -> io::Result<Vec<u8>> {
    crate::stream::encode_all(source, map_level(level)).map_err(io::Error::from)
}

/// Decompresses everything `source` provides into a Vec.
pub fn decode_all<R: io::Read>(source: R) -> io::Result<Vec<u8>> {
    crate::stream::decode_all(source).map_err(io::Error::from)
}

/// Compresses everything `source` provides into `destination`.
pub fn copy_encode<R: io::Read, W: io::Write>(
    source: R,
    destination: W,
    level: i32,
) -> io::Result<()> {
    crate::stream::copy_encode(source, destination, map_level(level)).map_err(io::Error::from)
}

/// Decompresses everything `source` provides into `destination`.
pub fn copy_decode<R: io::Read, W: io::Write>(source: R, destination: W) -> io::Result<()> {
    crate::stream::copy_decode(source, destination).map_err(io::Error::from)
}
