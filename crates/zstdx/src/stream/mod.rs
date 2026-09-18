//! Streaming encoders and decoders shaped after `std::io::Read`/`std::io::Write`.
//!
//! - [`write::Encoder`] compresses what is written to it into an underlying writer
//!   ([`write::Decoder`] decodes into one)
//! - [`read::Encoder`] exposes compressed bytes through `std::io::Read` ([`read::Decoder`]
//!   decompresses while reading, transparently over concatenated frames)
//!
//! One-shot conveniences over the same machinery: [`encode_all`],
//! [`decode_all`], [`copy_encode`] and [`copy_decode`].

pub(crate) mod encoder_core;
#[cfg(feature = "std")]
pub(crate) mod encoder_mt;
#[cfg(feature = "std")]
pub(crate) mod mt_pool;
pub mod read;
pub mod write;

#[cfg(test)]
mod tests;

use crate::{
    Level, Result,
    io::{Read, Write},
};

/// Compress everything `source` provides into a Vec.
///
/// ```rust
/// let data = b"abcabcabc";
/// let compressed = zstdx::stream::encode_all(&data[..], zstdx::Level::Fastest).unwrap();
/// assert_eq!(zstdx::stream::decode_all(&compressed[..]).unwrap(), data);
/// ```
pub fn encode_all<R: Read>(source: R, level: Level) -> Result<alloc::vec::Vec<u8>> {
    let mut output = alloc::vec::Vec::new();
    copy_encode(source, &mut output, level)?;
    Ok(output)
}

/// Decompress everything `source` provides into a Vec (all frames).
///
/// ```rust
/// let compressed = zstdx::stream::encode_all(&b"abcabcabc"[..], zstdx::Level::Fastest).unwrap();
/// assert_eq!(
///     zstdx::stream::decode_all(&compressed[..]).unwrap(),
///     b"abcabcabc"
/// );
/// ```
pub fn decode_all<R: Read>(source: R) -> Result<alloc::vec::Vec<u8>> {
    let mut output = alloc::vec::Vec::new();
    copy_decode(source, &mut output)?;
    Ok(output)
}

/// Compress everything `source` provides into `destination`.
pub fn copy_encode<R: Read, W: Write>(
    mut source: R,
    mut destination: W,
    level: Level,
) -> Result<()> {
    let mut encoder = write::Encoder::new(&mut destination, level)?;
    copy_between(&mut source, &mut encoder)?;
    encoder.do_finish()
}

/// Decompress everything `source` provides into `destination` (all frames).
///
/// ```rust
/// let compressed = zstdx::stream::encode_all(&b"abc"[..], zstdx::Level::Fastest).unwrap();
/// let mut out = Vec::new();
/// zstdx::stream::copy_decode(&compressed[..], &mut out).unwrap();
/// assert_eq!(out, b"abc");
/// ```
pub fn copy_decode<R: Read, W: Write>(source: R, mut destination: W) -> Result<()> {
    let mut decoder = read::Decoder::new(source)?;
    copy_between(&mut decoder, &mut destination)
}

/// std::io::copy is unavailable under no_std; pump through a stack buffer.
// the stack buffer keeps this copy loop allocation-free
#[allow(clippy::large_stack_arrays)]
fn copy_between<R: Read, W: Write>(source: &mut R, destination: &mut W) -> Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = source.read(&mut buffer).map_err(crate::Error::from)?;
        if n == 0 {
            return Ok(());
        }
        destination
            .write_all(&buffer[..n])
            .map_err(crate::Error::from)?;
    }
}
