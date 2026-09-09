//! The compression levels this crate can actually produce.
//!
//! Every variant is backed by a real implementation; levels that the original
//! zstd expresses as numbers between 1 and 22 (default, better, best, ...)
//! map onto the nearest variant here (see [`Level::approximate_zstd`] for the
//! exact ranges).

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
    /// A single-probe hash matcher (libzstd's `fast` strategy).
    Fastest,
    /// This level is roughly equivalent to Zstd compression levels 3-5.
    /// A short hash-chain matcher with one lazy step.
    Fast,
    /// This level is roughly equivalent to Zstd compression levels 6-9.
    /// A deeper hash-chain matcher with two lazy steps.
    Balanced,
    /// This level is roughly equivalent to Zstd compression levels 10-15.
    /// The optimal parser in its cheapest setting: noticeably denser than
    /// `Balanced` at a fraction of `Opt`'s cost.
    Best,
    /// This level is roughly equivalent to Zstd compression levels 16-17.
    /// An optimal-price parser over binary-tree matches (libzstd's
    /// `btopt`): whole-bit price estimates with skip heuristics.
    Opt,
    /// This level is roughly equivalent to Zstd compression levels 18-22.
    /// The optimal parser at its densest (libzstd's `btultra`/`btultra2`):
    /// fractional-bit prices, a match+1-literal recheck and a statistics
    /// seeding pass over the first block.
    Ultra,
}

impl Level {
    /// The level used when none is specified.
    pub const DEFAULT: Level = Level::Fastest;

    /// Map a numeric libzstd level onto the nearest implemented strategy:
    /// negatives and 1-2 stay `Fastest`, 3-5 round to `Fast`, 6-9 to
    /// `Balanced`, 10-15 to `Best`, 16-17 to `Opt` and everything above to
    /// `Ultra`.
    pub const fn approximate_zstd(level: i32) -> Level {
        if level > 17 {
            Level::Ultra
        } else if level > 15 {
            Level::Opt
        } else if level > 9 {
            Level::Best
        } else if level >= 6 {
            Level::Balanced
        } else if level >= 3 {
            Level::Fast
        } else {
            Level::Fastest
        }
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn zstd_mapping_ranges() {
        assert_eq!(Level::approximate_zstd(-5), Level::Fastest);
        assert_eq!(Level::approximate_zstd(0), Level::Fastest);
        assert_eq!(Level::approximate_zstd(1), Level::Fastest);
        assert_eq!(Level::approximate_zstd(2), Level::Fastest);
        assert_eq!(Level::approximate_zstd(3), Level::Fast);
        assert_eq!(Level::approximate_zstd(5), Level::Fast);
        assert_eq!(Level::approximate_zstd(6), Level::Balanced);
        assert_eq!(Level::approximate_zstd(9), Level::Balanced);
        assert_eq!(Level::approximate_zstd(10), Level::Best);
        assert_eq!(Level::approximate_zstd(15), Level::Best);
        assert_eq!(Level::approximate_zstd(16), Level::Opt);
        assert_eq!(Level::approximate_zstd(17), Level::Opt);
        assert_eq!(Level::approximate_zstd(18), Level::Ultra);
        assert_eq!(Level::approximate_zstd(22), Level::Ultra);
    }
}
