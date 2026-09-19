//! Porting layer for code written against the [`zstd` crate] (the libzstd
//! bindings): the same module layout, type names, numeric levels and
//! `io::Result` signatures, on top of the pure-Rust implementation.
//!
//! Swap `zstd::` for `zstdx::compat::` and most code compiles unchanged.
//! Known differences, all forced by what this crate implements today:
//!
//! - Numeric levels map exactly through [`Level::from_zstd`][crate::Level::from_zstd].
//! - `multithread(n)` with `n > 1` runs the native multithreaded streaming encoder; it must be set
//!   before the stream starts. Dictionary-taking constructors are supported (single-threaded).
//! - `zstd_safe` and `zstd::dict` (trained dictionaries) have no equivalent; the native low-level
//!   API is [`crate::encoding`]/[`crate::decoding`].
//!
//! [`zstd` crate]: https://docs.rs/zstd/0.13

pub mod bulk;
pub mod stream;

#[cfg(test)]
mod tests;

use crate::Level;

/// Default compression level, mirroring the zstd crate's constant.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

/// The accepted range of compression levels.
pub fn compression_level_range() -> core::ops::RangeInclusive<i32> {
    1..=22
}

/// Map a numeric libzstd level onto this crate's level. Level 0 is
/// libzstd's "default" (3), unlike the crate-native convention where
/// [`Level::Uncompressed`] wraps data in raw blocks.
pub(crate) fn map_level(level: i32) -> Level {
    if level == 0 {
        Level::from_zstd(DEFAULT_COMPRESSION_LEVEL)
    } else {
        Level::from_zstd(level)
    }
}
