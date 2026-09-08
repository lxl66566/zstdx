//! Builder-style option sets shared by the high-level entry points.
//!
//! Options are plain data validated where possible at build time; nothing
//! here performs io. Consuming builder methods keep construction chainable:
//!
//! ```rust
//! use ruzstd::{EncoderOptions, Level};
//! let opts = EncoderOptions::new(Level::Fastest)
//!     .pledged_size(Some(12))
//!     .checksum(false);
//! ```

use crate::Level;

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
}

impl EncoderOptions {
    pub const fn new(level: Level) -> Self {
        Self {
            level,
            checksum: cfg!(feature = "hash"),
            pledged_size: None,
            workers: 0,
        }
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
    /// on the calling thread; more than one worker currently fails with
    /// [`Error::Unsupported`][crate::Error::Unsupported] until the
    /// multithreaded backend lands.
    pub const fn workers(mut self, workers: u32) -> Self {
        self.workers = workers;
        self
    }
}

/// Parameters of a decoder, applied when the decoder is constructed.
///
/// The dictionary is stored raw and parsed when a decoder takes the options.
#[derive(Debug, Clone, Default)]
pub struct DecoderOptions {
    pub(crate) max_window_size: Option<u64>,
    pub(crate) dictionary: Option<alloc::vec::Vec<u8>>,
}

impl DecoderOptions {
    pub const fn new() -> Self {
        Self {
            max_window_size: None,
            dictionary: None,
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
}
