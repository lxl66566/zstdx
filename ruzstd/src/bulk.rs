//! One-shot compression and decompression of in-memory buffers.
//!
//! These are the fastest entry points: [`compress`] rides the pooled slice
//! fast path (borrowed matcher window, blocks append straight into the
//! output) and [`decompress`] uses the flat decode path.

use crate::decoding::errors::FrameDecoderError;
use crate::decoding::FrameDecoder;
use crate::{Level, Result};

/// Compress `source` into a fresh zstd frame.
///
/// This is infallible: compressing into memory cannot fail short of
/// allocation. It reuses a per-thread pooled encoder state, so repeated calls
/// don't pay for hash table and entropy table setup.
///
/// ```rust
/// let data = b"the quick brown fox jumps over the lazy dog";
/// let compressed = ruzstd::bulk::compress(data, ruzstd::Level::Fastest);
/// let decompressed = ruzstd::bulk::decompress(&compressed, data.len()).unwrap();
/// assert_eq!(&decompressed[..], data);
/// ```
pub fn compress(source: &[u8], level: Level) -> alloc::vec::Vec<u8> {
    crate::encoding::compress_slice_to_vec(source, level)
}

/// Decompress a zstd stream (possibly several concatenated frames) into a
/// fresh Vec.
///
/// `capacity` is a starting hint. Frames that carry their content size in the
/// header still start from the hint, so prefer `0` over a wrong guess; the
/// buffer doubles automatically whenever the decoded data does not fit. When
/// you know the exact size, [`decompress_to_buffer`] avoids the retries.
pub fn decompress(source: &[u8], capacity: usize) -> Result<alloc::vec::Vec<u8>> {
    let mut decoder = FrameDecoder::new();
    let mut capacity = capacity.max(64 * 1024);
    loop {
        let mut out = alloc::vec::Vec::with_capacity(capacity);
        match decoder.decode_all_to_vec(source, &mut out) {
            Ok(()) => return Ok(out),
            Err(FrameDecoderError::TargetTooSmall) => capacity *= 2,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Decompress a zstd stream into the caller's buffer.
///
/// Returns the number of bytes written. `destination` must be large enough
/// for all decoded data; the whole input is consumed (multiple frames and
/// skippable frames included).
pub fn decompress_to_buffer(source: &[u8], destination: &mut [u8]) -> Result<usize> {
    FrameDecoder::new()
        .decode_all(source, destination)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn shapes() -> Vec<Vec<u8>> {
        let mut pseudo_random = 0x9E37_79B9_7F4A_7C15u64;
        let mut rand = move || {
            pseudo_random ^= pseudo_random << 13;
            pseudo_random ^= pseudo_random >> 7;
            pseudo_random ^= pseudo_random << 17;
            pseudo_random
        };
        vec![
            vec![],
            vec![1],
            vec![7u8; 5],
            vec![b'x'; 300 * 1024],
            (0..130 * 1024).map(|_| (rand() & 0xFF) as u8).collect(),
            (0..900 * 1024).map(|i| (i % 61) as u8).collect(),
        ]
    }

    #[test]
    fn roundtrip_both_levels() {
        for input in shapes() {
            for level in [Level::Uncompressed, Level::Fastest] {
                let compressed = compress(&input, level);
                // exact capacity, hint too small (must grow), and no hint
                let out = decompress(&compressed, input.len()).unwrap();
                assert_eq!(out, input, "len {} level {level:?}", input.len());
                let out = decompress(&compressed, input.len() / 2).unwrap();
                assert_eq!(out, input);
                let out = decompress(&compressed, 0).unwrap();
                assert_eq!(out, input);
            }
        }
    }

    #[test]
    fn to_buffer_reports_written() {
        let input = b"small payload for the exact-size path";
        let compressed = compress(input, Level::Fastest);
        let mut out = vec![0u8; input.len() + 8];
        let written = decompress_to_buffer(&compressed, &mut out).unwrap();
        assert_eq!(written, input.len());
        assert_eq!(&out[..written], input);
        let mut exact = vec![0u8; input.len()];
        decompress_to_buffer(&compressed, &mut exact).unwrap();
        assert_eq!(&exact[..], input);
    }

    #[cfg(feature = "std")]
    #[test]
    fn interop_with_zstd_crate() {
        let input: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let compressed = compress(&input, Level::Fastest);
        // libzstd decodes our frames (checksum included)
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(compressed.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, input);
        // and we decode libzstd's frames
        let zstd_made = zstd::bulk::compress(&input, 3).unwrap();
        assert_eq!(decompress(&zstd_made, 0).unwrap(), input);
    }
}
