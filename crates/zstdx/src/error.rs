//! The error type of the high-level API.
//!
//! The low-level [`decoding`][crate::decoding] types keep their own error
//! enums (with thiserror-derived impls); this module adds the umbrella type
//! the [`bulk`][crate::bulk] and [`stream`][crate::stream] entry points use,
//! so `?` converts both frame corruption and io failures into one type.

use core::fmt;

use crate::{
    decoding::errors::{DictionaryDecodeError, FrameDecoderError},
    io,
};

pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Everything the high-level API can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The compressed data is not a valid zstd stream.
    #[error(transparent)]
    Frame(#[from] FrameDecoderError),
    /// A dictionary was rejected while being parsed.
    #[error(transparent)]
    Dictionary(#[from] DictionaryDecodeError),
    /// The underlying reader or writer failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The requested capability exists in the API surface but this build of
    /// zstdx does not implement it yet.
    #[error("{feature} is not implemented by this build of zstdx")]
    Unsupported { feature: Feature },
    /// A parameter was applied to a stream that already started.
    #[error("{0}")]
    Parameter(#[from] ParameterError),
}

/// Capabilities whose API surface exists ahead of the implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Feature {
    /// `EncoderOptions::workers` with more than one worker.
    Multithread,
    /// Compressing with a dictionary.
    DictionaryEncoding,
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Feature::Multithread => "multithreaded compression",
            Feature::DictionaryEncoding => "dictionary-based compression",
        };
        f.write_str(name)
    }
}

/// Invalid usage of a parameterized entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParameterError {
    #[error("streaming already started; set parameters before the first write")]
    AlreadyStreaming,
}

#[cfg(feature = "std")]
impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => io,
            other => std::io::Error::other(other),
        }
    }
}

// The no_std io::Error is constructed from a kind plus a boxed Display, so
// the umbrella error rides inside it.
#[cfg(not(feature = "std"))]
impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        io::Error::new(io::ErrorKind::Other, alloc::boxed::Box::new(e))
    }
}

/// Convert the umbrella error into the io error of the active build. A plain
/// `.into()` is ambiguous at call sites because the no_std io::Error also has
/// an inherent `from(ErrorKind)` constructor that shadows the trait method.
pub(crate) fn into_io(e: Error) -> io::Error {
    e.into()
}
