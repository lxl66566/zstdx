//! One-shot compression and decompression of in-memory buffers.
//!
//! These are the fastest entry points: [`compress`] rides the pooled slice
//! fast path (borrowed matcher window, blocks append straight into the
//! output) and [`decompress`] uses the flat decode path.

use crate::{
    Level, Result,
    decoding::{FrameDecoder, errors::FrameDecoderError},
};

/// Compress `source` into a fresh zstd frame.
///
/// This is infallible: compressing into memory cannot fail short of
/// allocation. It reuses a per-thread pooled encoder state, so repeated calls
/// don't pay for hash table and entropy table setup.
///
/// ```rust
/// let data = b"the quick brown fox jumps over the lazy dog";
/// let compressed = zstdx::bulk::compress(data, zstdx::Level::Fastest);
/// let decompressed = zstdx::bulk::decompress(&compressed, data.len()).unwrap();
/// assert_eq!(&decompressed[..], data);
/// ```
pub fn compress(source: &[u8], level: Level) -> alloc::vec::Vec<u8> {
    crate::encoding::compress_slice_to_vec(source, level)
}

/// [`compress`] with a full option set: more than one worker engages the
/// multithreaded job path on std builds (single-threaded when a dictionary
/// is attached), the checksum flag decides whether the frame carries a
/// content checksum, and an attached dictionary is parsed here — an invalid
/// dictionary is the one fallible case.
///
/// ```rust
/// let data = b"the quick brown fox jumps over the lazy dog";
/// let opts = zstdx::EncoderOptions::new(zstdx::Level::Fastest).checksum(true);
/// let compressed = zstdx::bulk::compress_with(data, &opts).unwrap();
/// let decompressed = zstdx::bulk::decompress(&compressed, data.len()).unwrap();
/// assert_eq!(&decompressed[..], data);
/// ```
pub fn compress_with(
    source: &[u8],
    options: &crate::EncoderOptions,
) -> Result<alloc::vec::Vec<u8>> {
    if let Some(raw) = &options.dictionary {
        let dict = crate::encoding::dictionary::EncDictionary::parse(raw)?;
        let shape = crate::InputShape {
            len: Some(source.len() as u64),
            window_log: options.input_shape.window_log,
        };
        return Ok(crate::encoding::compress_slice_with_dictionary(
            source,
            options.level,
            options.checksum,
            shape,
            &dict,
        ));
    }
    #[cfg(feature = "std")]
    if options.workers > 1 {
        return Ok(crate::encoding::mt::compress_slice_mt(
            source,
            options.level,
            options.checksum,
            options.workers,
            options.input_shape.window_log,
        ));
    }
    Ok(crate::encoding::compress_slice_shaped(
        source,
        options.level,
        options.checksum,
        crate::InputShape {
            len: None,
            window_log: options.input_shape.window_log,
        },
    ))
}

/// Decompress a zstd stream (possibly several concatenated frames) into a
/// fresh Vec.
///
/// `capacity` is a starting hint. Frames that carry their content size in the
/// header still start from the hint, so prefer `0` over a wrong guess; the
/// buffer doubles automatically whenever the decoded data does not fit. When
/// you know the exact size, [`decompress_to_buffer`] avoids the retries.
///
/// ```rust
/// let compressed = zstdx::bulk::compress(b"abcabcabc", zstdx::Level::Fastest);
/// let out = zstdx::bulk::decompress(&compressed, 0).unwrap();
/// assert_eq!(out, b"abcabcabc");
/// ```
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
///
/// ```rust
/// let data = b"abcabcabc";
/// let compressed = zstdx::bulk::compress(data, zstdx::Level::Fastest);
/// let mut buf = [0u8; 16];
/// let written = zstdx::bulk::decompress_to_buffer(&compressed, &mut buf).unwrap();
/// assert_eq!(&buf[..written], data);
/// ```
pub fn decompress_to_buffer(source: &[u8], destination: &mut [u8]) -> Result<usize> {
    FrameDecoder::new()
        .decode_all(source, destination)
        .map_err(Into::into)
}

/// [`decompress_to_buffer`] with a full option set: more than one decode
/// thread engages the parallel decoder on std builds (see
/// [`DecoderOptions::threads`][crate::DecoderOptions::threads]); an
/// attached dictionary is parsed here and used by the sequential decoder.
pub fn decompress_to_buffer_with(
    source: &[u8],
    destination: &mut [u8],
    options: &crate::DecoderOptions,
) -> Result<usize> {
    #[cfg(feature = "std")]
    if options.threads > 1 && options.dictionary.is_none() {
        let max = options
            .max_window_size
            .unwrap_or(crate::decoding::DEFAULT_MAX_WINDOW_SIZE);
        return crate::decoding::mt::decode_all_mt(source, destination, options.threads, max)
            .map_err(Into::into);
    }
    if let Some(raw) = &options.dictionary {
        let dict = crate::decoding::Dictionary::load(raw).map_err(crate::Error::Dictionary)?;
        let mut decoder = FrameDecoder::new();
        decoder.add_dict(dict).map_err(crate::Error::Frame)?;
        return decoder.decode_all(source, destination).map_err(Into::into);
    }
    #[cfg(not(feature = "std"))]
    let _ = options;
    decompress_to_buffer(source, destination)
}

/// [`decompress`] with a full option set (see
/// [`decompress_to_buffer_with`] for the decode-thread switch and
/// dictionary handling).
pub fn decompress_with(
    source: &[u8],
    capacity: usize,
    options: &crate::DecoderOptions,
) -> Result<alloc::vec::Vec<u8>> {
    #[cfg(feature = "std")]
    if options.threads > 1 && options.dictionary.is_none() {
        let max = options
            .max_window_size
            .unwrap_or(crate::decoding::DEFAULT_MAX_WINDOW_SIZE);
        let mut out = alloc::vec::Vec::with_capacity(capacity.max(64 * 1024));
        match crate::decoding::mt::decode_to_vec_mt(source, &mut out, options.threads, max) {
            Ok(()) => return Ok(out),
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(raw) = &options.dictionary {
        let dict = crate::decoding::Dictionary::load(raw).map_err(crate::Error::Dictionary)?;
        let mut decoder = FrameDecoder::new();
        decoder.add_dict(dict).map_err(crate::Error::Frame)?;
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
    #[cfg(not(feature = "std"))]
    let _ = options;
    decompress(source, capacity)
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    fn shapes() -> Vec<Vec<u8>> {
        let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
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
            (0..130 * 1024).map(|_| (rand() & 0xff) as u8).collect(),
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

    /// Repeated calls through the pooled slice state must be byte-identical:
    /// the recycled entropy-build buffers carry stale contents that only
    /// live rows overwrite, so a dead-row read would flip the output from
    /// the second call on (multi-block inputs also exercise the
    /// previous-table adoption between blocks and calls).
    #[test]
    fn pooled_calls_byte_identical() {
        let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            pseudo_random ^= pseudo_random << 13;
            pseudo_random ^= pseudo_random >> 7;
            pseudo_random ^= pseudo_random << 17;
            pseudo_random
        };
        let mut mixed: Vec<u8> = Vec::new();
        for i in 0..300 * 1024u32 {
            // Compressible stretches interleaved with random bytes keep
            // both the entropy builds and the raw fallbacks live.
            mixed.push(if i % 97 < 64 {
                b'a' + (i % 23) as u8
            } else {
                (rand() & 0xff) as u8
            });
        }
        mixed.extend((0..130 * 1024).map(|i| (i % 61) as u8));
        mixed.extend((0..64 * 1024).map(|_| (rand() & 0xff) as u8));
        for level in [Level::Fastest, Level::Fast, Level::Balanced, Level::Best] {
            let opts = crate::EncoderOptions::new(level).checksum(false);
            let first = compress_with(&mixed, &opts).unwrap();
            for _ in 0..3 {
                assert_eq!(
                    compress_with(&mixed, &opts).unwrap(),
                    first,
                    "pooled call diverged at {level:?}"
                );
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

    /// A forced window log must shrink the declared window (and with it the
    /// reachable history), roundtrip through libzstd, and leave the forced
    /// frames decodable by our own decoder.
    #[cfg(feature = "std")]
    #[test]
    fn forced_window_log_bounds_reach() {
        // Period above the forced window but below the level's own: only
        // the wide window can bridge the repeat. The unit itself must be
        // aperiodic or any window bridges it.
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let unit: Vec<u8> = (0..64 * 1024)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect();
        let input: Vec<u8> = (0..16).flat_map(|_| unit.iter().copied()).collect();
        let wide = compress(&input, Level::Ultra);
        let opts = crate::EncoderOptions::new(Level::Ultra)
            .with_input_shape(crate::InputShape::default().with_window_log(15));
        let narrow = compress_with(&input, &opts).unwrap();
        assert!(
            narrow.len() > wide.len(),
            "forced W15 must lose the 64K period: {} vs {}",
            narrow.len(),
            wide.len()
        );
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(narrow.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, input);
        assert_eq!(decompress(&narrow, 0).unwrap(), input);
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
