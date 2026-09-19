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
    /// The stream was pledged a content size that does not match the bytes
    /// actually written. The frame header declares the pledge, so the frame
    /// would be rejected by every decoder; it is not emitted.
    #[error(
        "pledged content size {pledged} does not match the {actual} bytes written; the frame \
         would be rejected by decoders"
    )]
    PledgedSizeMismatch { pledged: u64, actual: u64 },
    /// A read-side [`stream::Encoder`][crate::stream::read::Encoder] was
    /// finished while encoded bytes were still pending in its output buffer.
    /// The pending tail carries the frame's closing block, so dropping it
    /// would leave every byte already read a truncated stream; read the
    /// encoder to end of stream (the final read returns `Ok(0)`) before
    /// finishing.
    #[error(
        "encoder finished with {bytes} unread encoded bytes; read it to end of stream before \
         finishing"
    )]
    UnreadOutput { bytes: usize },
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
