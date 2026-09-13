//! The compression levels this crate can produce.
//!
//! A level is the numeric libzstd level (0-22); every value selects a real
//! parameter set (see the ladder in `match_generator`). The named constants
//! alias representative levels.

/// The compression mode used impacts the speed of compression,
/// and resulting compression ratios. Faster compression will result
/// in worse compression ratios, and vice versa.
///
/// Values follow libzstd: 0 stores the data uncompressed inside a Zstandard
/// frame, 1-22 trade speed for ratio. The crate's named tiers alias
/// representative numeric levels: [`Level::Fastest`] = 1, [`Level::Fast`] = 3,
/// [`Level::Balanced`] = 9, [`Level::Best`] = 13, [`Level::Opt`] = 17 and
/// [`Level::Ultra`] = 19.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

// The tier constants keep the enum-style PascalCase of the former enum.
#[allow(non_upper_case_globals)]
impl Level {
    /// Level 9: a deeper hash-chain matcher with two lazy steps
    /// (libzstd's `lazy2` strategy).
    pub const Balanced: Level = Level(9);
    /// Level 13: a binary-tree match finder with two lazy steps (libzstd's
    /// `btlazy2` strategy): the ladder's speed rung above [`Level::Balanced`].
    pub const Best: Level = Level(13);
    /// The level used when none is specified.
    pub const DEFAULT: Level = Level::Fastest;
    /// Level 3: a short hash-chain matcher with one lazy step
    /// (libzstd's `dfast` strategy).
    pub const Fast: Level = Level(3);
    /// Level 1: a single-probe hash matcher (libzstd's `fast` strategy).
    pub const Fastest: Level = Level(1);
    /// The highest libzstd level.
    pub const MAX: Level = Level(22);
    /// Level 17: an optimal-price parser over binary-tree matches
    /// (libzstd's `btopt`): whole-bit price estimates with skip heuristics.
    pub const Opt: Level = Level(17);
    /// Level 19: the optimal parser at its densest (libzstd's
    /// `btultra`/`btultra2`): fractional-bit prices, a match+1-literal
    /// recheck and a statistics seeding pass over the first block.
    pub const Ultra: Level = Level(19);
    /// Level 0: the data is not compressed at all, just wrapped
    /// in a Zstandard frame.
    pub const Uncompressed: Level = Level(0);

    /// Map a numeric libzstd level onto a level of this crate. Negatives
    /// clamp to 1 (libzstd retunes its fast strategy's acceleration for
    /// them, which this crate does not model separately), 0 selects
    /// [`Level::Uncompressed`], values above 22 clamp to 22.
    pub const fn from_zstd(level: i32) -> Level {
        if level < 0 {
            Level(1)
        } else if level > 22 {
            Level(22)
        } else {
            Level(level as u8)
        }
    }

    /// The numeric libzstd level (0-22).
    pub const fn as_i32(self) -> i32 {
        self.0 as i32
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
    fn zstd_mapping_clamps() {
        assert_eq!(Level::from_zstd(-5), Level::Fastest);
        assert_eq!(Level::from_zstd(0), Level::Uncompressed);
        assert_eq!(Level::from_zstd(1), Level::Fastest);
        assert_eq!(Level::from_zstd(22), Level(22));
        assert_eq!(Level::from_zstd(23), Level::MAX);
        assert_eq!(Level::from_zstd(99), Level::MAX);
    }

    #[test]
    fn tier_constants_are_ordered() {
        let tiers = [
            Level::Uncompressed,
            Level::Fastest,
            Level::Fast,
            Level::Balanced,
            Level::Best,
            Level::Opt,
            Level::Ultra,
        ];
        assert!(tiers.is_sorted());
        assert_eq!(Level::DEFAULT, Level::Fastest);
        assert_eq!(Level::Ultra.as_i32(), 19);
    }

    #[test]
    fn forced_window_log_reaches_matcher() {
        use crate::encoding::{MatchGeneratorDriver, Matcher as _};

        let mut d = MatchGeneratorDriver::new_direct();
        d.set_input_shape(crate::InputShape::default().with_window_log(15));
        d.reset(Level::Ultra);
        assert_eq!(d.window_size(), 1 << 15);
    }
}
