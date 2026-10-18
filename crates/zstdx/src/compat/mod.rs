//! Porting layer for code written against the [`zstd` crate] (the libzstd
//! bindings): the same module layout, type names, numeric levels and
//! `io::Result` signatures, on top of the pure-Rust implementation.
//!
//! Swap `zstd::` for `zstdx::compat::` and most code compiles unchanged.
//! Known differences, all forced by what this crate implements today:
//!
//! - Numeric levels map exactly through [`Level::from_zstd`][crate::Level::from_zstd];
//!   `compression_level_range` answers 1..=22 (no accelerated levels), and a level of 0 selects the
//!   default (3).
//! - `multithread(n)` with `n > 1` runs the native multithreaded streaming encoder; it must be set
//!   before the stream starts. Dictionary-taking constructors are supported (single-threaded),
//!   including the prepared [`dict::EncoderDictionary`]/[`dict::DecoderDictionary`] forms; the
//!   trainers live in [`dict`] behind the `dict_builder` feature.
//! - `zstd_safe` (and the `stream::{raw,zio}` layer around it) has no equivalent; the native
//!   low-level API is [`crate::encoding`]/[`crate::decoding`].
//! - `bulk::Decompressor::decompress` treats `capacity` as a growing hint, where the zstd crate
//!   errors when the content exceeds it; [`bulk::Decompressor::upper_bound`] always answers
//!   (upstream needs the `experimental` feature).
//! - `stream::read::{Encoder,Decoder}` take `R: Read` directly instead of wrapping it in a
//!   `BufReader`, so `get_ref`/`get_mut`/`finish` expose `R`, not `BufReader<R>`.
//!
//! [`zstd` crate]: https://docs.rs/zstd/0.13

pub mod bulk;
pub mod dict;
pub mod stream;

#[cfg(test)]
mod tests;

// The zstd crate re-exports these at its root: `zstd::Encoder` is the
// write-side encoder, `zstd::Decoder` the read-side decoder.
#[doc(no_inline)]
pub use stream::{decode_all, encode_all, read::Decoder, write::AutoFinishEncoder, write::Encoder};

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
