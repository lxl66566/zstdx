//! The pull-shaped streaming encoder: pushed input, pulled output.

use super::encoder_core::FrameEncoderCore;
use crate::{EncoderOptions, Level, Result, io::Write};

/// Compress pushed input and serve the encoded frame on demand.
///
/// The io-shaped encoders own their delivery cadence: [`write::Encoder`](super::write::Encoder)
/// drains into its writer after every fed chunk, [`read::Encoder`](super::read::Encoder)
/// pulls its source between deliveries. This type holds no io object and
/// hands the cadence to the caller: input enters through [`Encoder::write`],
/// which encodes without delivering anything, and the encoded bytes leave
/// only through [`Encoder::split_output`] (straight into a caller buffer)
/// or [`Encoder::write_output_to`] (into a caller writer), in stream
/// order. A caller that delivers everything after every write and before
/// [`Encoder::flush`] and [`Encoder::finish`] produces exactly the
/// [`write::Encoder`](super::write::Encoder) bytes: the frame-closing
/// shapes read what the core has already handed out, and that cadence
/// keeps the core's output empty at every entry, like the write drain.
///
/// ```rust
/// use zstdx::{Level, stream::raw::Encoder};
///
/// let mut enc = Encoder::new(Level::Fastest).unwrap();
/// enc.write(b"the quick brown fox").unwrap();
/// enc.finish().unwrap();
/// let mut compressed = vec![0u8; enc.pending_output()];
/// let n = enc.split_output(&mut compressed);
/// assert_eq!(n, compressed.len());
/// assert_eq!(
///     zstdx::bulk::decompress(&compressed, 0).unwrap(),
///     b"the quick brown fox"
/// );
/// ```
pub struct Encoder {
    core: FrameEncoderCore,
}

impl Encoder {
    /// Create an encoder with default options.
    pub fn new(level: Level) -> Result<Self> {
        Self::with_options(EncoderOptions::new(level))
    }

    /// Create an encoder from a builder option set.
    // options are consumed builder data; by value keeps the chaining API
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_options(options: EncoderOptions) -> Result<Self> {
        Ok(Self {
            core: FrameEncoderCore::new(&options)?,
        })
    }

    /// Stage input for the frame, encoding blocks as they fill. Nothing
    /// is delivered: the encoded bytes wait for [`Encoder::split_output`]
    /// and [`Encoder::write_output_to`]. Fails with
    /// [`Error::PledgedSizeMismatch`](crate::Error::PledgedSizeMismatch)
    /// at the first write that pushes the fed total past a pledged
    /// content size, before anything is staged.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        assert!(!self.core.is_finished(), "write after finish");
        self.core.write(data)
    }

    /// Emit the staged bytes early as a non-last block. A no-op when
    /// nothing is staged; the emitted bytes wait for delivery like any
    /// other. Panics after [`Encoder::finish`].
    pub fn flush(&mut self) {
        assert!(!self.core.is_finished(), "flush after finish");
        self.core.flush_block();
    }

    /// Close the frame: the staged bytes (or an empty block) become the
    /// last block and the checksum, if enabled, is appended. Fails when a
    /// pledged content size was not met (the closing block is not
    /// emitted). The undelivered frame bytes stay pending for delivery;
    /// every following write or flush panics.
    pub fn finish(&mut self) -> Result<()> {
        self.core.finish()
    }

    /// Whether [`Encoder::finish`] closed the frame.
    pub fn is_finished(&self) -> bool {
        self.core.is_finished()
    }

    /// Encoded bytes not yet delivered.
    pub fn pending_output(&self) -> usize {
        self.core.pending_output()
    }

    /// Whether partial deliveries may span calls with bytes left pending
    /// in the encoder without moving its frame bytes: the multithreaded
    /// core's delivery queue holds them natively, while the
    /// single-threaded core's closing shape reads the delivery cadence —
    /// a caller of that core keeps it empty between calls (the drain
    /// cadence the FFI layer pins its single-threaded bytes to).
    pub fn keeps_pending_output(&self) -> bool {
        self.core.keeps_pending_output()
    }

    /// Deliver encoded bytes into `buf`, without an intermediate copy,
    /// returning the delivered length. Partial deliveries resume where
    /// the previous one stopped; the buffer's allocation is kept.
    pub fn split_output(&mut self, buf: &mut [u8]) -> usize {
        self.core.split_output(buf)
    }

    /// Deliver every pending encoded byte into `w`. A failed write keeps
    /// the undelivered tail pending, so the caller's retry resumes where
    /// it stopped.
    pub fn write_output_to(&mut self, w: &mut impl Write) -> Result<(), crate::io::Error> {
        self.core.write_output_to(w)
    }
}
