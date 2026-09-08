//! Porting layer for code written against the [`zstd` crate] (the libzstd
//! bindings): the same module layout, type names, numeric levels and
//! `io::Result` signatures, on top of the pure-Rust implementation.
//!
//! Swap `zstd::` for `ruzstd::compat::` and most code compiles unchanged.
//! Known differences, all forced by what this crate implements today:
//!
//! - Numeric levels map onto the implemented strategies through
//!   [`Level::approximate_zstd`][crate::Level::approximate_zstd]: 1-2 stay
//!   near zstd 1, 3-5 near 3-5, 6-9 near 6-9 and 10-22 near 12-15; ratios
//!   approach the requested band rather than match it exactly.
//! - `multithread(n)` with `n > 1` and dictionary-taking constructors fail
//!   with an error (the native [`EncoderOptions`][crate::EncoderOptions]
//!   surface exists; the streaming backends do not yet).
//! - `zstd_safe`, `zstd::dict` (trained dictionaries) and `set_parameter`
//!   have no equivalent; the native low-level API is
//!   [`crate::encoding`]/[`crate::decoding`].
//!
//! [`zstd` crate]: https://docs.rs/zstd/0.13

pub mod bulk;
pub mod stream;

#[cfg(test)]
mod tests;

use crate::Level;
use std::io;

/// Default compression level, mirroring the zstd crate's constant.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

/// The accepted range of compression levels.
pub fn compression_level_range() -> core::ops::RangeInclusive<i32> {
    1..=22
}

/// Map a numeric libzstd level onto the nearest implemented strategy.
pub(crate) fn map_level(level: i32) -> Level {
    Level::approximate_zstd(level)
}

pub(crate) fn unsupported_io(feature: crate::Feature) -> io::Error {
    io::Error::other(crate::Error::Unsupported { feature })
}
