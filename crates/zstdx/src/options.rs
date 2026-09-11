//! Builder-style option sets shared by the high-level entry points.
//!
//! Options are plain data validated where possible at build time; nothing
//! here performs io. Consuming builder methods keep construction chainable:
//!
//! ```rust
//! use zstdx::{EncoderOptions, Level};
//! let opts = EncoderOptions::new(Level::Fastest)
//!     .pledged_size(Some(12))
//!     .checksum(false);
//! ```

use crate::Level;

/// Externally declared input shape: what the caller knows about the whole
/// frame beyond the level. Applied to the level's parameter row when the
/// encoder resets.
///
/// ```rust
/// use zstdx::{EncoderOptions, InputShape, Level};
/// let shape = InputShape::default().with_window_log(18);
/// let opts = EncoderOptions::new(Level::Fastest).with_input_shape(shape);
/// ```
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct InputShape {
    /// Whole-frame byte length, when known: the window and tables downsize
    /// to the source (libzstd's `ZSTD_adjustCParams`).
    pub len: Option<u64>,
    /// Forced window log (clamped to 10..=27), overriding the level's row
    /// before the length adjustment (a known smaller length still clamps
    /// it, as in libzstd).
    pub window_log: Option<u32>,
}

impl InputShape {
    /// Set a forced window log (see the field docs).
    pub const fn with_window_log(mut self, log: u32) -> Self {
        self.window_log = Some(log);
        self
    }

    /// Set the known whole-frame length (see the field docs).
    pub const fn with_len(mut self, len: u64) -> Self {
        self.len = Some(len);
        self
    }
}

/// Parameters of an encoder, applied when the encoder is constructed.
///
/// The default matches the crate's one-shot paths: checksum on (when the
/// `hash` feature is enabled) and single-threaded.
#[derive(Debug, Clone)]
pub struct EncoderOptions {
    pub(crate) level: Level,
    pub(crate) checksum: bool,
    pub(crate) pledged_size: Option<u64>,
    pub(crate) workers: u32,
    pub(crate) input_shape: InputShape,
    /// Raw zstd dictionary for compression (parsed when an encoder takes
    /// the options). Multithreaded paths fall back to single-threaded with
    /// a dictionary attached.
    pub(crate) dictionary: Option<alloc::vec::Vec<u8>>,
}

impl EncoderOptions {
    pub const fn new(level: Level) -> Self {
        Self {
            level,
            checksum: cfg!(feature = "hash"),
            pledged_size: None,
            workers: 0,
            input_shape: InputShape {
                len: None,
                window_log: None,
            },
            dictionary: None,
        }
    }

    /// Attach a raw zstd dictionary (as produced by `zstd --train`) for
    /// compression: its content becomes the frame's match history, its
    /// entropy tables seed the first blocks, and its id is declared in the
    /// frame header. Invalid dictionaries fail when the encoder is built.
    pub fn dictionary(mut self, dict: &[u8]) -> Self {
        self.dictionary = Some(dict.to_vec());
        self
    }

    /// Override the input-shape knobs (forced window log). The pledged
    /// size ([`Self::pledged_size`]) wins for the length when both are
    /// set.
    pub const fn with_input_shape(mut self, shape: InputShape) -> Self {
        self.input_shape = shape;
        self
    }

    /// Write a frame checksum (default: on with the `hash` feature, off
    /// without it — the flag is inert when the feature is disabled).
    pub const fn checksum(mut self, enabled: bool) -> Self {
        self.checksum = enabled;
        self
    }

    /// Pledge the total input size so it can be written into the frame
    /// header, letting decoders preallocate exactly.
    pub const fn pledged_size(mut self, size: Option<u64>) -> Self {
        self.pledged_size = size;
        self
    }

    /// Number of worker threads for compression. `0` (the default) compresses
    /// on the calling thread. More than one worker engages the multithreaded
    /// job paths on std builds — both [`bulk::compress_with`][crate::bulk::compress_with]
    /// and the streaming encoders — falling back to the single-threaded core
    /// for raw-block levels and single-core processes. Without std the
    /// streaming encoders reject it with
    /// [`Error::Unsupported`][crate::Error::Unsupported].
    pub const fn workers(mut self, workers: u32) -> Self {
        self.workers = workers;
        self
    }
}

/// Parameters of a decoder, applied when the decoder is constructed.
///
/// The dictionary is stored raw and parsed when a decoder takes the options.
#[derive(Debug, Clone)]
pub struct DecoderOptions {
    pub(crate) max_window_size: Option<u64>,
    pub(crate) dictionary: Option<alloc::vec::Vec<u8>>,
    pub(crate) threads: u32,
}

impl DecoderOptions {
    pub const fn new() -> Self {
        Self {
            max_window_size: None,
            dictionary: None,
            threads: 0,
        }
    }

    /// Upper bound on the window size a frame may request (values above the
    /// format maximum are clamped). Frames exceeding the bound fail to decode;
    /// the default is [`DEFAULT_MAX_WINDOW_SIZE`][crate::decoding::DEFAULT_MAX_WINDOW_SIZE].
    pub const fn max_window_size(mut self, max: u64) -> Self {
        self.max_window_size = Some(max);
        self
    }

    /// Attach a zstd dictionary for decoding.
    pub fn dictionary(mut self, dict: &[u8]) -> Self {
        self.dictionary = Some(dict.to_vec());
        self
    }

    /// Number of worker threads for decoding. `0` (the default) decodes on
    /// the calling thread; more than one engages the parallel decoder on std
    /// builds for one-shot inputs with restart points (job-based encoders
    /// such as libzstd's `-T` mode and this crate's multithreaded compressor
    /// emit them; other inputs fall back to the sequential decoder).
    pub const fn threads(mut self, threads: u32) -> Self {
        self.threads = threads;
        self
    }
}

impl Default for DecoderOptions {
    fn default() -> Self {
        Self::new()
    }
}
