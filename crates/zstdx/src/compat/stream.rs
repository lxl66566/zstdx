//! Streaming types mirroring `zstd::stream` (read/write submodules and the
//! one-shot functions).

use std::{
    io::{self, Read as _},
    vec::Vec,
};

use super::{
    dict::{DecoderDictionary, EncoderDictionary},
    map_level,
};
use crate::{DecoderOptions, EncoderOptions};

/// Common state of the compat encoders: options are collected until the
/// stream starts, then materialized into the native encoder.
struct EncoderState<W: io::Write> {
    writer: Option<W>,
    options: EncoderOptions,
    /// libzstd's ContentSizeFlag: when false, a pledged size still sizes
    /// the frame but never reaches the header.
    contentsize_flag: bool,
    encoder: Option<crate::stream::write::Encoder<W>>,
}

impl<W: io::Write> EncoderState<W> {
    fn new(writer: W, level: i32) -> Self {
        Self {
            writer: Some(writer),
            options: EncoderOptions::new(map_level(level)),
            contentsize_flag: true,
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
            // ContentSizeFlag=false: the pledge degrades to a sizing hint
            // (`input_shape.len`) and leaves the header without the field.
            let mut options = self.options.clone();
            if !self.contentsize_flag
                && let Some(len) = options.pledged_size
            {
                options.pledged_size = None;
                let mut shape = options.input_shape;
                shape.len = Some(len);
                options = options.with_input_shape(shape);
            }
            match crate::stream::write::Encoder::try_with_options(writer, &options) {
                Ok(encoder) => self.encoder = Some(encoder),
                // Put the writer back so get_mut/finish still work after a
                // failed start.
                Err((writer, err)) => {
                    self.writer = Some(writer);
                    return Err(io::Error::from(err));
                },
            }
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

/// The accepted `window_log_max` range, libzstd's `ZSTD_d_windowLogMax`
/// bounds; the upper end also keeps `1 << log` from overflowing.
const WINDOW_LOG_MAX_RANGE: core::ops::RangeInclusive<u32> = 10..=31;

fn check_window_log_max(log: u32) -> io::Result<()> {
    if WINDOW_LOG_MAX_RANGE.contains(&log) {
        Ok(())
    } else {
        Err(io::Error::other(crate::Error::Parameter(
            crate::ParameterError::WindowLogMaxOutOfRange(log),
        )))
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
        /// Creates a new encoder. A level of 0 selects the default (3);
        /// other values map through `Level::from_zstd` (negatives clamp
        /// to 1 — zstdx models no accelerated levels).
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

        /// Creates an encoder using a reference prefix: the prefix bytes
        /// are match history for the first frame (libzstd's
        /// `ZSTD_c_refPrefix`, which loads the content raw — no entropy
        /// tables, no dictionary id, format-default repcodes). The
        /// engine's raw-dictionary path is exactly that load, so this is
        /// the dictionary constructor under the prefix name.
        pub fn with_ref_prefix(writer: W, level: i32, prefix: &[u8]) -> io::Result<Self> {
            Self::with_dictionary(writer, level, prefix)
        }

        /// Creates an encoder bound to an already-prepared dictionary
        /// (its parse is shared); the dictionary's level is used.
        pub fn with_prepared_dictionary(
            writer: W,
            dictionary: &EncoderDictionary,
        ) -> io::Result<Self> {
            let mut state = EncoderState::new(writer, dictionary.compression_level());
            state.options = dictionary.apply(state.options)?;
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

        /// Includes or excludes the content size in the frame header
        /// (default on). With it off, a pledged source size still sizes the
        /// frame but is not written; fails once streaming started.
        pub fn include_contentsize(&mut self, include: bool) -> io::Result<()> {
            self.state.set_started()?;
            self.state.contentsize_flag = include;
            Ok(())
        }

        /// Sets the maximum back-reference distance as `2^log_distance`
        /// (clamped to the format's 10..=27); fails once streaming started.
        pub fn window_log(&mut self, log_distance: u32) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.input_shape.window_log = Some(log_distance);
            Ok(())
        }

        /// Enables or disables storing of the dict id (default on). With it
        /// off the dictionary still applies — content, entropy tables,
        /// repcodes — but the frame header carries no Dictionary_ID, so
        /// decoders need the dictionary explicitly and decoding with a
        /// wrong one is undefined; fails once streaming started.
        pub fn include_dictid(&mut self, include: bool) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.include_dictid = include;
            Ok(())
        }

        /// Enables or disables long-distance matching. `true` forces the
        /// far-match source on and, with no [`Self::window_log`] set,
        /// raises the window to 27 as libzstd does; `false` restores the
        /// automatic per-level arming — the zstd crate's bool maps onto
        /// libzstd's auto value there, not an explicit disable. Fails
        /// once streaming started.
        pub fn long_distance_matching(&mut self, enable: bool) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.long_distance_matching = if enable {
                crate::LdmMode::Enable
            } else {
                crate::LdmMode::Auto
            };
            Ok(())
        }

        /// Enables multithreaded compression: `workers > 1` engages the
        /// native multithreaded streaming core (raw-block levels and
        /// single-core processes fall back to the single-threaded core, as
        /// in the native API; `workers <= 1` keeps it single-threaded).
        /// Fails once streaming started, like the other parameter setters.
        pub fn multithread(&mut self, workers: u32) -> io::Result<()> {
            self.state.set_started()?;
            self.state.options.workers = workers;
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
        /// Set on the first write: parameter setters refuse afterwards,
        /// the write-side counterpart of the read side's lazy start.
        started: bool,
    }

    impl<W: io::Write> Decoder<W> {
        /// Creates a new decoder.
        pub fn new(writer: W) -> io::Result<Self> {
            Ok(Self {
                decoder: crate::stream::write::Decoder::new(writer).map_err(io::Error::from)?,
                started: false,
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
                started: false,
            })
        }

        /// Creates a decoder bound to an already-prepared dictionary
        /// (its parse is shared). An invalid prepared dictionary is
        /// refused here, the decode side's attach point.
        pub fn with_prepared_dictionary(
            writer: W,
            dictionary: &DecoderDictionary,
        ) -> io::Result<Self> {
            let dict = dictionary.shared()?;
            Ok(Self {
                decoder: crate::stream::write::Decoder::with_options(
                    writer,
                    DecoderOptions::new().parsed_dictionary(dict),
                )
                .map_err(io::Error::from)?,
                started: false,
            })
        }

        /// Sets the maximum window log distance (log2 of bytes); applies to
        /// frames not yet decoded and fails once a write began.
        pub fn window_log_max(&mut self, log: u32) -> io::Result<()> {
            if self.started {
                return Err(io::Error::other(crate::Error::Parameter(
                    crate::ParameterError::AlreadyStreaming,
                )));
            }
            check_window_log_max(log)?;
            self.decoder
                .set_max_window_size(1u64 << log)
                .map_err(io::Error::from)
        }

        /// Returns a wrapper that flushes the stream on drop.
        pub fn auto_flush(self) -> AutoFlushDecoder<W> {
            AutoFlushDecoder {
                decoder: Some(self),
                on_flush: None,
            }
        }

        /// Returns a decoder that flushes the stream on drop.
        ///
        /// Calls the given callback with the result of the implicit flush.
        /// It runs during drop, so it must not panic.
        pub fn on_flush<F: FnMut(io::Result<()>)>(self, f: F) -> AutoFlushDecoder<W, F> {
            AutoFlushDecoder {
                decoder: Some(self),
                on_flush: Some(f),
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
            self.started = true;
            self.decoder.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.decoder.flush()
        }
    }

    /// A decoder that flushes the stream on drop (the zstd crate's
    /// semantics: writes buffer, the wrapper's drop performs the flush).
    pub struct AutoFlushDecoder<W: io::Write, F: FnMut(io::Result<()>) = fn(io::Result<()>)> {
        // Wrapped in an Option to take it during drop.
        decoder: Option<Decoder<W>>,
        on_flush: Option<F>,
    }

    impl<W: io::Write, F: FnMut(io::Result<()>)> AutoFlushDecoder<W, F> {
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

    impl<W: io::Write, F: FnMut(io::Result<()>)> io::Write for AutoFlushDecoder<W, F> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.decoder.as_mut().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.decoder.as_mut().unwrap().flush()
        }
    }

    impl<W: io::Write, F: FnMut(io::Result<()>)> Drop for AutoFlushDecoder<W, F> {
        fn drop(&mut self) {
            if let Some(mut decoder) = self.decoder.take() {
                let result = io::Write::flush(&mut decoder);
                if let Some(f) = self.on_flush.as_mut() {
                    f(result);
                }
            }
        }
    }
}

pub mod read {
    //! io::Read-shaped types mirroring `zstd::stream::read`.

    use super::*;

    /// A compression encoder exposing compressed bytes through `io::Read`,
    /// mirroring `zstd::stream::read::Encoder`.
    ///
    /// The native encoder materializes on the first read; parameters may be
    /// set until then.
    pub struct Encoder<R: io::Read> {
        source: Option<R>,
        options: EncoderOptions,
        /// libzstd's ContentSizeFlag: when false, a pledged size still
        /// sizes the frame but never reaches the header.
        contentsize_flag: bool,
        inner: Option<crate::stream::read::Encoder<R>>,
    }

    impl<R: io::Read> Encoder<R> {
        /// Creates a new encoder. A level of 0 selects the default (3);
        /// other values map through `Level::from_zstd` (negatives clamp
        /// to 1 — zstdx models no accelerated levels).
        pub fn new(source: R, level: i32) -> io::Result<Self> {
            Ok(Self {
                source: Some(source),
                options: EncoderOptions::new(map_level(level)),
                contentsize_flag: true,
                inner: None,
            })
        }

        /// Creates a new encoder around a `BufRead` (this porting layer
        /// reads through its own buffering, so the input is taken as-is).
        pub fn with_buffer(source: R, level: i32) -> io::Result<Self> {
            Self::new(source, level)
        }

        /// Creates an encoder bound to a dictionary.
        pub fn with_dictionary(source: R, level: i32, dictionary: &[u8]) -> io::Result<Self> {
            Ok(Self {
                source: Some(source),
                options: EncoderOptions::new(map_level(level)).dictionary(dictionary),
                contentsize_flag: true,
                inner: None,
            })
        }

        /// Creates an encoder bound to an already-prepared dictionary
        /// (its parse is shared); the dictionary's level is used.
        pub fn with_prepared_dictionary(
            source: R,
            dictionary: &EncoderDictionary,
        ) -> io::Result<Self> {
            let options = dictionary.apply(EncoderOptions::new(map_level(
                dictionary.compression_level(),
            )))?;
            Ok(Self {
                source: Some(source),
                options,
                contentsize_flag: true,
                inner: None,
            })
        }

        /// Sets the pledged source size (written into the frame header; a
        /// stream writing a different size fails at finish); fails once
        /// streaming started.
        pub fn set_pledged_src_size(&mut self, size: Option<u64>) -> io::Result<()> {
            self.not_started()?;
            self.options.pledged_size = size;
            Ok(())
        }

        /// Includes or excludes the frame checksum (default on); fails
        /// once streaming started.
        pub fn include_checksum(&mut self, include: bool) -> io::Result<()> {
            self.not_started()?;
            self.options.checksum = include && cfg!(feature = "hash");
            Ok(())
        }

        /// Includes or excludes the content size in the frame header
        /// (default on). With it off, a pledged source size still sizes
        /// the frame but is not written; fails once streaming started.
        pub fn include_contentsize(&mut self, include: bool) -> io::Result<()> {
            self.not_started()?;
            self.contentsize_flag = include;
            Ok(())
        }

        /// Sets the maximum back-reference distance as `2^log_distance`
        /// (clamped to the format's 10..=27); fails once streaming started.
        pub fn window_log(&mut self, log_distance: u32) -> io::Result<()> {
            self.not_started()?;
            self.options.input_shape.window_log = Some(log_distance);
            Ok(())
        }

        /// Enables or disables storing of the dict id (default on). With it
        /// off the dictionary still applies — content, entropy tables,
        /// repcodes — but the frame header carries no Dictionary_ID, so
        /// decoders need the dictionary explicitly and decoding with a
        /// wrong one is undefined; fails once streaming started.
        pub fn include_dictid(&mut self, include: bool) -> io::Result<()> {
            self.not_started()?;
            self.options.include_dictid = include;
            Ok(())
        }

        /// Enables or disables long-distance matching. `true` forces the
        /// far-match source on and, with no [`Self::window_log`] set,
        /// raises the window to 27 as libzstd does; `false` restores the
        /// automatic per-level arming — the zstd crate's bool maps onto
        /// libzstd's auto value there, not an explicit disable. Fails
        /// once streaming started.
        pub fn long_distance_matching(&mut self, enable: bool) -> io::Result<()> {
            self.not_started()?;
            self.options.long_distance_matching = if enable {
                crate::LdmMode::Enable
            } else {
                crate::LdmMode::Auto
            };
            Ok(())
        }

        /// Enables multithreaded compression: `workers > 1` engages the
        /// native multithreaded streaming core (raw-block levels and
        /// single-core processes fall back to the single-threaded core, as
        /// in the native API; `workers <= 1` keeps it single-threaded).
        /// Fails once streaming started, like the write-side parameter
        /// setters.
        pub fn multithread(&mut self, workers: u32) -> io::Result<()> {
            self.not_started()?;
            self.options.workers = workers;
            Ok(())
        }

        fn not_started(&self) -> io::Result<()> {
            if self.inner.is_some() {
                Err(io::Error::other(crate::Error::Parameter(
                    crate::ParameterError::AlreadyStreaming,
                )))
            } else {
                Ok(())
            }
        }

        /// Recommended size of read batches: one full block.
        pub fn recommended_output_size() -> usize {
            crate::stream::read::Encoder::<R>::recommended_output_size()
        }

        /// Acquires a reference to the underlying reader.
        pub fn get_ref(&self) -> &R {
            match &self.inner {
                Some(encoder) => encoder.get_ref(),
                None => self.source.as_ref().expect("source kept until start"),
            }
        }

        /// Acquires a mutable reference to the underlying reader.
        pub fn get_mut(&mut self) -> &mut R {
            match &mut self.inner {
                Some(encoder) => encoder.get_mut(),
                None => self.source.as_mut().expect("source kept until start"),
            }
        }

        /// Destructures this object into the underlying reader. Fails when a
        /// pledged content size was not met by the bytes consumed so far, or
        /// when encoded bytes are still unread (read the encoder to end of
        /// stream before finishing).
        pub fn finish(mut self) -> io::Result<R> {
            // A never-started encoder still owns its whole unread frame:
            // materialize so finish reports it instead of dropping it
            // silently.
            self.materialize()?;
            self.inner
                .take()
                .expect("inner kept until start")
                .finish()
                .map_err(io::Error::from)
        }

        /// Tries to fill `out` with encoded bytes.
        pub fn flush(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.materialize()?.read(out)
        }

        fn materialize(&mut self) -> io::Result<&mut crate::stream::read::Encoder<R>> {
            if self.inner.is_none() {
                let source = self.source.take().expect("source kept until start");
                // ContentSizeFlag=false: the pledge degrades to a sizing
                // hint (`input_shape.len`) and leaves the header without
                // the field.
                let mut options = self.options.clone();
                if !self.contentsize_flag
                    && let Some(len) = options.pledged_size
                {
                    options.pledged_size = None;
                    let mut shape = options.input_shape;
                    shape.len = Some(len);
                    options = options.with_input_shape(shape);
                }
                // On failure the source goes back before the error leaves,
                // mirroring the read decoder's materialize.
                let encoder = match crate::stream::read::Encoder::try_with_options(source, &options)
                {
                    Ok(encoder) => encoder,
                    Err((source, err)) => {
                        self.source = Some(source);
                        return Err(io::Error::from(err));
                    },
                };
                self.inner = Some(encoder);
            }
            Ok(self.inner.as_mut().unwrap())
        }
    }

    impl<R: io::Read> io::Read for Encoder<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.materialize()?.read(buf)
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

        /// Creates a new decoder using a reference prefix: the same raw
        /// content the encoder saw (libzstd's `ZSTD_d_refPrefix` — the
        /// bytes load as a raw-content dictionary under the null id and
        /// serve as history for the frame, whose header carries no
        /// dictionary id to check against).
        pub fn with_ref_prefix(source: R, prefix: &[u8]) -> io::Result<Self> {
            Self::with_dictionary(source, prefix)
        }

        /// Creates a new decoder around a `BufRead` (this porting layer
        /// reads through its own buffering, so the input is taken as-is).
        pub fn with_buffer(source: R) -> io::Result<Self> {
            Self::new(source)
        }

        /// Creates a new decoder bound to an already-prepared dictionary
        /// (its parse is shared).
        pub fn with_prepared_dictionary(
            source: R,
            dictionary: &DecoderDictionary,
        ) -> io::Result<Self> {
            let dict = dictionary.shared()?;
            Ok(Self {
                source: Some(source),
                options: DecoderOptions::new().parsed_dictionary(dict),
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
            check_window_log_max(log)?;
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
                // On failure the source goes back before the error leaves,
                // mirroring the write side's try_with_options: get_mut and
                // finish stay usable, and a retry re-runs the construction.
                // Bytes a failed first-frame parse already consumed from the
                // source are not replayed.
                let decoder =
                    match crate::stream::read::Decoder::try_with_options(source, &self.options) {
                        Ok(decoder) => decoder,
                        Err((source, err)) => {
                            self.source = Some(source);
                            return Err(io::Error::from(err));
                        },
                    };
                let decoder = if self.single_frame {
                    decoder.single_frame()
                } else {
                    decoder
                };
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

// The zstd crate re-exports these at `zstd::stream::` as well: the
// write-side encoder, the read-side decoder and its auto-finishing
// wrapper.
#[doc(no_inline)]
pub use self::{read::Decoder, write::AutoFinishEncoder, write::Encoder};

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
