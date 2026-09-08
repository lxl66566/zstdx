//! The compression levels this crate can actually produce.
//!
//! Every variant is backed by a real implementation; levels that the original
//! zstd expresses as numbers between 1 and 22 (default, better, best, ...)
//! will appear here as further variants once their matchers land.

/// The compression mode used impacts the speed of compression,
/// and resulting compression ratios. Faster compression will result
/// in worse compression ratios, and vice versa.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Level {
    /// This level does not compress the data at all, and simply wraps
    /// it in a Zstandard frame.
    Uncompressed,
    /// This level is roughly equivalent to Zstd compression level 1.
    Fastest,
}

impl Level {
    /// The level used when none is specified.
    pub const DEFAULT: Level = Level::Fastest;
}

impl Default for Level {
    fn default() -> Self {
        Self::DEFAULT
    }
}
