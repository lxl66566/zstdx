//! Streaming encoders and decoders shaped after [`io::Read`]/[`io::Write`].
//!
//! - [`write::Encoder`] compresses what is written to it into an underlying
//!   writer ([`write::Decoder`] decodes into one)
//! - [`read::Encoder`] exposes compressed bytes through [`io::Read`]
//!   ([`read::Decoder`] decompresses while reading)

pub(crate) mod encoder_core;
pub mod read;
pub mod write;

#[cfg(test)]
mod tests;
