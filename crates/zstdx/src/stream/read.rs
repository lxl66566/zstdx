//! io::Read-shaped streaming types.

use super::encoder_core::{FrameEncoderCore, ST_PULL};
use crate::{
    DecoderOptions, EncoderOptions, Level, Result,
    decoding::{BlockDecodingStrategy, FrameDecoder, frame_source},
    io::{Error, Read},
};

/// Compress data pulled from an underlying [`Read`] and expose the encoded
/// frame through [`Read`].
///
/// The frame is closed once the source reports end of file; reads after that
/// return `Ok(0)`. Read to that end before [`Encoder::finish`], which
/// reclaims the underlying reader and refuses to drop undelivered frame
/// bytes.
///
/// ```rust
/// use zstdx::{Level, io::Read, stream::read::Encoder};
///
/// let mut enc = Encoder::new(b"the quick brown fox".as_slice(), Level::Fastest).unwrap();
/// let mut compressed = Vec::new();
/// enc.read_to_end(&mut compressed).unwrap();
/// assert!(!compressed.is_empty());
/// ```
pub struct Encoder<R: Read> {
    source: Option<R>,
    core: FrameEncoderCore,
    /// Pump staging buffer: reused across pulls so it is zeroed once per
    /// encoder instead of once per 16 KiB pull. The single-threaded core's
    /// path; the mt core reads straight into its accumulate buffer. The
    /// 16 KiB granularity is measured (interleaved A/B): 64/128 KiB pulls
    /// overshoot the job grid's posting boundaries and pay for it at the
    /// buffer's recycle points.
    chunk: alloc::vec::Vec<u8>,
}

impl<R: Read> Encoder<R> {
    /// Create an encoder with default options.
    pub fn new(source: R, level: Level) -> Result<Self> {
        Self::with_options(source, EncoderOptions::new(level))
    }

    /// Create an encoder from a builder option set.
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_options(source: R, options: EncoderOptions) -> Result<Self> {
        Self::try_with_options(source, &options).map_err(|(_, err)| err)
    }

    /// Like [`Encoder::with_options`], but hands the source back when the
    /// options fail to materialize, so callers keep ownership of it (the
    /// same shape as the write encoder's `try_with_options`).
    pub(crate) fn try_with_options(
        source: R,
        options: &EncoderOptions,
    ) -> Result<Self, (R, crate::Error)> {
        match FrameEncoderCore::new(options) {
            Ok(core) => Ok(Self {
                source: Some(source),
                core,
                chunk: alloc::vec![0u8; ST_PULL],
            }),
            Err(err) => Err((source, err)),
        }
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
    ///
    /// The encoder must be read to end of stream first (the final [`Read`]
    /// call returns `Ok(0)`): the pending encoded bytes carry the frame's
    /// closing block, so finishing before that fails with
    /// [`Error::UnreadOutput`][crate::Error::UnreadOutput] instead of
    /// silently truncating everything already read. Fails with
    /// [`Error::PledgedSizeMismatch`][crate::Error::PledgedSizeMismatch]
    /// when a pledged content size was not met by those bytes.
    pub fn finish(self) -> Result<R> {
        self.try_finish().map_err(|(err, _reader)| err)
    }

    /// Finish the stream, reclaiming the underlying reader even on failure.
    ///
    /// [`Encoder::finish`] drops the reader together with the error; this
    /// hands both back, so a caller abandoning the output (the source
    /// errored mid-stream, the frame is no longer wanted) keeps the reader
    /// for whatever comes after it. The undelivered encoded bytes are
    /// dropped, truncating everything the encoder already read.
    // the Err variant hands the reader back; boxing the error would change
    // the public signature
    #[allow(clippy::result_large_err)]
    pub fn try_finish(mut self) -> Result<R, (crate::Error, R)> {
        let source = self.source.take().unwrap();
        match self.core.finish() {
            Ok(()) => match self.core.pending_output() {
                0 => Ok(source),
                pending => Err((crate::Error::UnreadOutput { bytes: pending }, source)),
            },
            Err(err) => Err((err, source)),
        }
    }
}

impl<R: Read> Read for Encoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        while !self.core.has_output() && !self.core.is_finished() {
            self.core
                .pump(self.source.as_mut().unwrap(), &mut self.chunk)?;
        }
        Ok(self.core.split_output(buf))
    }
}

/// An in-memory source whose [`Read`] implementation copies through
/// non-temporal stores: the stream encoders' opt-in streaming feed.
///
/// [`Encoder`] over an opaque in-memory source pays one temporal copy per
/// fed byte inside the source's own `read` (the crate cannot specialize
/// the `Read` trait to reach it), and the multithreaded core's read pump
/// pulls in 1 MiB steps — under glibc's non-temporal threshold, so beside
/// the eight workers' traffic each 100 MB of feed moves about 300 MB of
/// memory. Wrapping the source in `NtSource` puts those pulls on the
/// crate's NT-store kernel, which skips the destination read-for-ownership
/// half (see `common::nt_copy`). The frame bytes are identical to the
/// plain slice source's at the same options: a copy cannot change the
/// stream, and the job schedule keys on the stream offset alone.
///
/// The gain is a loaded-machine effect on the multithreaded feed (the
/// dll-class fastest rows read about +5% at 8 workers); the
/// single-threaded core's 16 KiB staging pulls stay under the kernel's
/// span threshold and keep the plain copy. Non-x86_64 targets, no-std
/// builds and a forced scalar tier fall back to the temporal copy —
/// byte-identical, just not faster.
///
/// ```rust
/// use zstdx::{
///     io::Read as _,
///     stream::read::{Encoder, NtSource},
/// };
///
/// let data = b"the quick brown fox".to_vec();
/// let mut enc = Encoder::new(NtSource::new(&data), zstdx::Level::Fastest).unwrap();
/// let mut compressed = Vec::new();
/// enc.read_to_end(&mut compressed).unwrap();
/// enc.finish().unwrap();
/// assert_eq!(zstdx::stream::decode_all(&compressed[..]).unwrap(), data);
/// ```
pub struct NtSource<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> NtSource<'a> {
    /// Wrap a borrowed in-memory source (a slice behind a `Vec`, `Box`,
    /// `Arc` or plain array — whichever outlives the encoder).
    pub fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }
}

impl Read for NtSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let remaining = &self.src[self.pos..];
        let n = remaining.len().min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        crate::common::nt_copy::copy(&remaining[..n], &mut buf[..n]);
        self.pos += n;
        Ok(n)
    }
}

/// An in-memory source an [`OwnedEncoder`] owns for the stream's lifetime:
/// the accepted forms of the zero-copy feed. Whichever arm holds the bytes,
/// they stay at one address from construction until the encoder drops, so
/// the encoder's jobs can borrow absolute ranges of them directly.
///
/// ```rust
/// use zstdx::stream::read::{OwnedEncoder, OwnedSource};
///
/// let source = OwnedSource::from(b"the quick brown fox".to_vec());
/// let mut enc = OwnedEncoder::new(source, zstdx::Level::Fastest).unwrap();
/// let mut compressed = Vec::new();
/// use zstdx::io::Read as _;
/// enc.read_to_end(&mut compressed).unwrap();
/// let source = enc.finish().unwrap();
/// assert_eq!(source.as_slice(), b"the quick brown fox");
/// ```
#[cfg(feature = "std")]
#[derive(Debug)]
pub enum OwnedSource {
    /// An owned buffer, handed back by [`OwnedEncoder::finish`].
    Vec(alloc::vec::Vec<u8>),
    /// A shared slice: the encoder holds one handle, the caller may keep
    /// others.
    Shared(alloc::sync::Arc<[u8]>),
    /// Bytes with a static lifetime.
    Static(&'static [u8]),
}

#[cfg(feature = "std")]
impl OwnedSource {
    /// The source bytes.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Vec(v) => v,
            Self::Shared(s) => s,
            Self::Static(s) => s,
        }
    }
}

#[cfg(feature = "std")]
impl From<alloc::vec::Vec<u8>> for OwnedSource {
    fn from(v: alloc::vec::Vec<u8>) -> Self {
        Self::Vec(v)
    }
}

#[cfg(feature = "std")]
impl From<alloc::boxed::Box<[u8]>> for OwnedSource {
    // `into_vec` keeps the boxed slice's allocation: a boxed slice's length
    // already equals its capacity.
    fn from(b: alloc::boxed::Box<[u8]>) -> Self {
        Self::Vec(b.into_vec())
    }
}

#[cfg(feature = "std")]
impl From<alloc::sync::Arc<[u8]>> for OwnedSource {
    fn from(s: alloc::sync::Arc<[u8]>) -> Self {
        Self::Shared(s)
    }
}

#[cfg(feature = "std")]
impl From<&'static [u8]> for OwnedSource {
    fn from(s: &'static [u8]) -> Self {
        Self::Static(s)
    }
}

/// Compress an owned in-memory source through [`Read`]: the zero-copy
/// multithreaded feed.
///
/// With more than one worker (and a compressing level on a multi-core
/// machine) the encoder borrows the source's bytes directly — every job
/// reads its absolute range of [`OwnedSource`] and its match window is the
/// source's own bytes below the job's start — so the serial input copy the
/// Read-trait pump pays is gone. The frame bytes are identical to
/// [`Encoder`] over the same source at the same options: the job schedule
/// is a pure function of the stream offset, and the feed's cadence is not
/// an input. The source's length is not declared for you: set
/// [`EncoderOptions::pledged_size`] or an input shape explicitly to get
/// the same header and schedule shapes the Read encoder would produce with
/// them.
///
/// The engagement demotions mirror [`Encoder`]'s: raw-block levels and
/// single-core machines run the single-threaded core, which stages the
/// source through one copy (the copy hides behind that core's encode
/// span). Reading works exactly like [`Encoder`]: pull until a read
/// returns `Ok(0)`, then [`OwnedEncoder::finish`] hands the source back.
///
/// ```rust
/// use zstdx::{Level, io::Read as _, stream::read::OwnedEncoder};
///
/// let data = b"abcabcabc".to_vec();
/// let mut enc = OwnedEncoder::with_options(
///     data.into(),
///     zstdx::EncoderOptions::new(Level::Fastest).workers(4),
/// )
/// .unwrap();
/// let mut compressed = Vec::new();
/// enc.read_to_end(&mut compressed).unwrap();
/// let source = enc.finish().unwrap();
/// assert_eq!(
///     zstdx::stream::decode_all(&compressed[..]).unwrap(),
///     source.as_slice()
/// );
/// ```
#[cfg(feature = "std")]
pub struct OwnedEncoder {
    /// Declared before `source`: the multithreaded core's job views borrow
    /// the source's bytes, and fields drop in declaration order, so the
    /// core's `Drop` — which joins every worker — runs before the source
    /// frees.
    core: FrameEncoderCore,
    source: OwnedSource,
    /// The single-threaded demotion's feed cursor: the chunked 16 KiB
    /// cadence keeps that core's consumer-dependent closing shape equal to
    /// the Read encoder's (see `FrameEncoderCore::pump_owned`). The
    /// multithreaded core feeds in one step and never reads it.
    st_fed: usize,
}

#[cfg(feature = "std")]
impl OwnedEncoder {
    /// Create an encoder with default options.
    pub fn new(source: OwnedSource, level: Level) -> Result<Self> {
        Self::with_options(source, EncoderOptions::new(level))
    }

    /// Create an encoder from a builder option set.
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_options(source: OwnedSource, options: EncoderOptions) -> Result<Self> {
        Self::try_with_options(source, &options).map_err(|(_, err)| err)
    }

    /// Like [`OwnedEncoder::with_options`], but hands the source back when
    /// the options fail to materialize, so callers keep ownership of it.
    pub(crate) fn try_with_options(
        source: OwnedSource,
        options: &EncoderOptions,
    ) -> Result<Self, (OwnedSource, crate::Error)> {
        match FrameEncoderCore::new_owned(options, source.as_slice()) {
            Ok(core) => Ok(Self {
                core,
                source,
                st_fed: 0,
            }),
            Err(err) => Err((source, err)),
        }
    }

    /// Reclaim the source. The frame is closed over the whole source; the
    /// encoder must be read to end of stream first (the final [`Read`]
    /// call returns `Ok(0)`): the pending encoded bytes carry the frame's
    /// closing block, so finishing before that fails with
    /// [`Error::UnreadOutput`][crate::Error::UnreadOutput] instead of
    /// silently truncating everything already read. Fails with
    /// [`Error::PledgedSizeMismatch`][crate::Error::PledgedSizeMismatch]
    /// when a pledged content size was not met by the source, at the first
    /// read (the whole source is fed in one step, so a mismatch surfaces
    /// where a Read-path stream discovers it: the offending read).
    pub fn finish(self) -> Result<OwnedSource> {
        self.try_finish().map_err(|(err, _)| err)
    }

    /// Finish the stream, reclaiming the source even on failure.
    ///
    /// [`OwnedEncoder::finish`] drops the source together with the error;
    /// this hands both back, so a caller abandoning the output keeps the
    /// source for whatever comes after it. The undelivered encoded bytes
    /// are dropped, truncating the frame the encoder already emitted.
    #[allow(clippy::result_large_err)]
    pub fn try_finish(mut self) -> Result<OwnedSource, (crate::Error, OwnedSource)> {
        match self.core.finish() {
            Ok(()) => match self.core.pending_output() {
                0 => Ok(self.source),
                pending => Err((crate::Error::UnreadOutput { bytes: pending }, self.source)),
            },
            Err(err) => Err((err, self.source)),
        }
    }
}

#[cfg(feature = "std")]
impl Read for OwnedEncoder {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        while !self.core.has_output() && !self.core.is_finished() {
            // The source's end is the stream's EOF: the multithreaded core
            // feeds in one step on the first pull, the single-threaded one
            // a chunk at a time (see `pump_owned`).
            self.core
                .pump_owned(self.source.as_slice(), &mut self.st_fed)?;
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
/// use zstdx::{io::Read, stream::read::Decoder};
///
/// let compressed = zstdx::bulk::compress(b"a b c b a", zstdx::Level::Fastest);
/// let mut dec = Decoder::new(&compressed[..]).unwrap();
/// let mut output = Vec::new();
/// dec.read_to_end(&mut output).unwrap();
/// assert_eq!(output, b"a b c b a");
/// ```
pub struct Decoder<R: Read> {
    source: R,
    inner: FrameDecoder,
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
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_options(source: R, options: DecoderOptions) -> Result<Self> {
        Self::try_with_options(source, &options).map_err(|(_, err)| err)
    }

    /// Like [`Decoder::with_options`], but hands the source back when
    /// construction fails, so callers keep ownership of it (the same shape
    /// as the write encoder's `try_with_options`).
    ///
    /// The eager first-frame parse may already have consumed bytes from the
    /// source when it fails; a retried construction keeps reading from
    /// wherever the source then stands.
    pub(crate) fn try_with_options(
        mut source: R,
        options: &DecoderOptions,
    ) -> Result<Self, (R, crate::Error)> {
        let mut decoder = FrameDecoder::new();
        if let Some(max) = options.max_window_size {
            decoder.set_max_window_size(max);
        }
        let mut build = || -> Result<()> {
            if let Some(dict) = &options.dictionary {
                let dict = alloc::sync::Arc::new(
                    crate::decoding::Dictionary::load(dict).map_err(crate::Error::Dictionary)?,
                );
                decoder.add_options_dict(dict)?;
            } else if let Some(dict) = &options.parsed_dictionary {
                decoder.add_options_dict(dict.clone())?;
            }
            Self::init_first_frame(&mut source, &mut decoder)
        };
        match build() {
            Ok(()) => Ok(Self {
                source,
                inner: decoder,
                single_frame: false,
                finished: false,
            }),
            Err(err) => Err((source, err)),
        }
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
            if self.inner.can_collect() > 0 {
                return self.inner.read(buf);
            }
            if self.inner.is_finished() {
                // The single-frame contract stops at the boundary: peeking
                // the next frame's header would consume bytes past the
                // frame and desynchronize a caller that keeps the source.
                if self.single_frame {
                    self.finished = true;
                    return Ok(0);
                }
                let next = frame_source::init_next_frame(&mut self.source, &mut self.inner)
                    .map_err(|e| crate::error::into_io(crate::Error::Frame(e)))?;
                if !next {
                    self.finished = true;
                    return Ok(0);
                }
                continue;
            }
            self.inner
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
