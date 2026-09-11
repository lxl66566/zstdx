//! A pure Rust implementation of the [Zstandard compression format](https://www.rfc-editor.org/rfc/rfc8878.pdf).
//!
//! ## High-level API
//! For most users these are the entry points:
//! - one-shot: [`compress`]/[`decompress`] (or [`bulk`] for buffer-to-buffer variants)
//! - streaming: [`stream`] with `io::Read`/`io::Write` shaped encoders and decoders
//! - configuration: [`Level`], [`EncoderOptions`], [`DecoderOptions`]
//!
//! Code written against the `zstd` crate (the libzstd bindings) ports with
//! minimal changes through [`compat`], which mirrors its shapes (numeric
//! levels, `io::Result`) on top of the same implementation.
//!
//! ## Decompression
//! The [decoding] module contains the code for decompression.
//! Decompression can be achieved by using the [`decoding::StreamingDecoder`]
//! or the more low-level [`decoding::FrameDecoder`]
//!
//! ## Compression
//! The [encoding] module contains the code for compression.
//! Compression can be achieved by using the [`encoding::compress`]/[`encoding::compress_to_vec`]
//! functions or [`encoding::FrameCompressor`]
#![doc = include_str!("../Readme.md")]
#![no_std]
#![deny(trivial_casts, trivial_numeric_casts, rust_2018_idioms)]

#[cfg(feature = "std")]
extern crate std;

#[cfg(not(feature = "rustc-dep-of-std"))]
extern crate alloc;

#[cfg(feature = "std")]
pub(crate) const VERBOSE: bool = false;

macro_rules! vprintln {
    ($($x:expr),*) => {
        #[cfg(feature = "std")]
        if crate::VERBOSE {
            std::println!($($x),*);
        }
    }
}

mod bit_io;
pub mod bulk;
mod common;
#[cfg(feature = "std")]
pub mod compat;
pub mod decoding;
#[cfg(feature = "dict_builder")]
#[cfg_attr(docsrs, doc(cfg(feature = "dict_builder")))]
pub mod dict;
pub mod encoding;

pub mod error;
pub mod level;
pub mod options;
pub mod stream;

pub use bulk::{compress, decompress};
pub use error::{Error, Feature, ParameterError, Result};
pub use level::Level;
pub use options::{DecoderOptions, EncoderOptions, InputShape};

pub(crate) mod blocks;

#[cfg(feature = "hash")]
pub(crate) mod xxh64;

#[cfg(feature = "fuzz_exports")]
pub mod fse;
#[cfg(feature = "fuzz_exports")]
pub mod huff0;

#[cfg(not(feature = "fuzz_exports"))]
pub(crate) mod fse;
#[cfg(not(feature = "fuzz_exports"))]
pub(crate) mod huff0;

#[cfg(feature = "std")]
pub mod io_std;

#[cfg(feature = "std")]
pub use io_std as io;

#[cfg(not(feature = "std"))]
pub mod io_nostd;

#[cfg(not(feature = "std"))]
pub use io_nostd as io;

mod tests;
