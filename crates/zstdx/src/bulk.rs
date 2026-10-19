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
/// multithreaded job path on std builds (a dictionary seeds the frame's
/// first job from its content and entropy tables — see
/// [`crate::encoding`]'s mt module), the checksum flag decides whether the
/// frame carries a
/// content checksum, and an attached dictionary is parsed here — an invalid
/// dictionary is the one fallible case. A
/// [`pledged_size`][crate::EncoderOptions::pledged_size] lands in the frame
/// header exactly as it does on the streaming encoders (byte-identical
/// frames for the same options); a pledge that disagrees with `source`'s
/// length fails with
/// [`Error::PledgedSizeMismatch`][crate::Error::PledgedSizeMismatch], the
/// same contract the streaming core enforces at finish.
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
    if let Some(pledged) = options.pledged_size
        && pledged != source.len() as u64
    {
        return Err(crate::Error::PledgedSizeMismatch {
            pledged,
            actual: source.len() as u64,
        });
    }
    let declared = options.pledged_size;
    // The dictionary resolves once (the parsed form shares, the raw bytes
    // parse fallibly here) and routes below: more than one worker seeds
    // job zero from it on the multithreaded path, anything else runs the
    // single-threaded dictionary slice entry.
    let dict = match (&options.parsed_dictionary, &options.dictionary) {
        (parsed, _) if parsed.is_some() => parsed.clone(),
        (_, Some(raw)) => Some(alloc::sync::Arc::new(
            crate::encoding::dictionary::EncDictionary::parse(raw)?,
        )),
        (_, None) => None,
    };
    if let Some(dict) = &dict {
        // The header stamp: `include_dictid(false)` suppresses it, and the
        // mt slice entry cannot express that — such frames run the
        // single-threaded dictionary entry instead (an MT engagement
        // demotion like the small-input one, recorded in the compat
        // deviations).
        let dict_id = options.include_dictid.then(|| dict.header_id()).flatten();
        // The LDM switch's window interplay (libzstd's
        // ZSTD_LDM_DEFAULT_WINDOW_LOG): the multithreaded path consumes
        // the raised window through its own forced-window plumbing; its
        // per-job LDM arming follows the job model (see the compat
        // deviations), so only the window half of an explicit switch
        // reaches it.
        let window_log = options.ldm_window_log();
        #[cfg(feature = "std")]
        if options.workers > 1 && options.include_dictid {
            return Ok(crate::encoding::mt::compress_slice_mt(
                source,
                options.level,
                options.checksum,
                options.workers,
                window_log,
                declared,
                Some(dict),
            ));
        }
        return Ok(crate::encoding::compress_slice_with_dictionary_id(
            source,
            options.level,
            options.checksum,
            crate::InputShape {
                len: Some(source.len() as u64),
                window_log,
            },
            dict,
            declared,
            dict_id,
            options.long_distance_matching,
        ));
    }
    #[cfg(feature = "std")]
    if options.workers > 1 {
        return Ok(crate::encoding::mt::compress_slice_mt(
            source,
            options.level,
            options.checksum,
            options.workers,
            options.ldm_window_log(),
            declared,
            None,
        ));
    }
    Ok(crate::encoding::compress_slice_shaped_ldm(
        source,
        options.level,
        options.checksum,
        crate::InputShape {
            len: None,
            window_log: options.ldm_window_log(),
        },
        options.long_distance_matching,
        declared,
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
/// attached dictionary (raw or parsed) is used by the sequential decoder.
pub fn decompress_to_buffer_with(
    source: &[u8],
    destination: &mut [u8],
    options: &crate::DecoderOptions,
) -> Result<usize> {
    // Either dictionary form keeps the decode sequential: the parallel
    // decoder plans over the raw frame bytes alone and its fallback
    // builds a fresh frame decoder, so a dictionary it never saw would
    // be silently dropped.
    #[cfg(feature = "std")]
    if options.threads > 1 && options.dictionary.is_none() && options.parsed_dictionary.is_none() {
        let max = options
            .max_window_size
            .unwrap_or(crate::decoding::DEFAULT_MAX_WINDOW_SIZE);
        return crate::decoding::mt::decode_all_mt(source, destination, options.threads, max)
            .map_err(Into::into);
    }
    if let Some(dict) = &options.parsed_dictionary {
        let mut decoder = FrameDecoder::new();
        decoder
            .add_options_dict(alloc::sync::Arc::clone(dict))
            .map_err(crate::Error::Frame)?;
        return decoder.decode_all(source, destination).map_err(Into::into);
    }
    if let Some(raw) = &options.dictionary {
        let dict = alloc::sync::Arc::new(
            crate::decoding::Dictionary::load(raw).map_err(crate::Error::Dictionary)?,
        );
        let mut decoder = FrameDecoder::new();
        decoder
            .add_options_dict(dict)
            .map_err(crate::Error::Frame)?;
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
    if options.threads > 1 && options.dictionary.is_none() && options.parsed_dictionary.is_none() {
        let max = options
            .max_window_size
            .unwrap_or(crate::decoding::DEFAULT_MAX_WINDOW_SIZE);
        let mut out = alloc::vec::Vec::with_capacity(capacity.max(64 * 1024));
        match crate::decoding::mt::decode_to_vec_mt(source, &mut out, options.threads, max) {
            Ok(()) => return Ok(out),
            Err(e) => return Err(e.into()),
        }
    }
    let shared = options.parsed_dictionary.clone();
    if let Some(dict) = shared {
        let mut decoder = FrameDecoder::new();
        decoder
            .add_options_dict(dict)
            .map_err(crate::Error::Frame)?;
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
    if let Some(raw) = &options.dictionary {
        let dict = alloc::sync::Arc::new(
            crate::decoding::Dictionary::load(raw).map_err(crate::Error::Dictionary)?,
        );
        let mut decoder = FrameDecoder::new();
        decoder
            .add_options_dict(dict)
            .map_err(crate::Error::Frame)?;
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
    use crate::{EncoderOptions, io::Write as _};

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
            let opts = EncoderOptions::new(level).checksum(false);
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

    /// The superblock literals layout (`LitStreams`): a frame small enough
    /// to decode as one unit emits its Huffman literals sections as one
    /// stream (the four-stream form's jumptable plus extra tails never pay
    /// there), while an unpledged stream over the same bytes keeps the
    /// stock four-stream sections — its output must not depend on write
    /// chunking, so it cannot arm on a length it does not know. Both forms
    /// must roundtrip.
    #[test]
    fn small_frame_literals_take_one_stream() {
        // json-ish records: repetitive enough that a small frame's literal
        // run lands in [256, 1024) — the flip window between the stock
        // <256 single-stream rule and the 10-bit size-field ceiling.
        let mut payload = Vec::new();
        for i in 0..64u32 {
            // decimal digits without std formatting
            let digits = [(b'0' + (i / 10) as u8), b'0' + (i % 10) as u8];
            let n = if i < 10 {
                &digits[1..]
            } else {
                &digits[..]
            };
            payload.extend_from_slice(b"{\"id\":");
            payload.extend_from_slice(n);
            payload.extend_from_slice(b",\"name\":\"record-");
            payload.extend_from_slice(n);
            payload.extend_from_slice(b"\",\"value\":");
            payload.extend_from_slice(n);
            payload.extend_from_slice(b"83,\"tags\":[\"a\",\"b\"]}\n");
        }
        let opts = EncoderOptions::new(Level::Balanced).checksum(false);
        let small = compress_with(&payload, &opts).unwrap();

        // Unpledged stream over the same bytes: the stock layout.
        let mut sink = Vec::new();
        {
            let mut enc = crate::stream::write::Encoder::with_options(&mut sink, opts).unwrap();
            enc.write_all(&payload).unwrap();
            enc.finish().unwrap();
        }

        let sections = |frame: &[u8]| -> Vec<(u32, u8)> {
            // (regenerated_size, num_streams) of every Huffman literals
            // section: a tiny frame walker — header, then per-block.
            let mut out = Vec::new();
            let fhd = frame[4];
            let mut o = 5usize;
            if fhd & 0x20 == 0 {
                o += 1; // window descriptor
            }
            let fcsf = fhd >> 6;
            if fcsf > 0 || fhd & 0x20 != 0 {
                o += match fcsf {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    _ => 8,
                };
            }
            loop {
                let h = u32::from_le_bytes([frame[o], frame[o + 1], frame[o + 2], 0]);
                o += 3;
                let (last, bt, size) = (h & 1 != 0, (h >> 1) & 3, (h >> 3) as usize);
                if bt == 2 {
                    let mut ls = crate::blocks::literals_section::LiteralsSection::new();
                    ls.parse_from_header(&frame[o..o + 5]).unwrap();
                    if matches!(
                        ls.ls_type,
                        crate::blocks::literals_section::LiteralsSectionType::Compressed
                    ) {
                        out.push((ls.regenerated_size, ls.num_streams.unwrap()));
                    }
                }
                o += match bt {
                    1 => 1, // RLE content is a single byte
                    _ => size,
                };
                if last {
                    return out;
                }
            }
        };

        let small_secs = sections(&small);
        assert!(
            small_secs
                .iter()
                .any(|&(regen, _)| (256..1024).contains(&regen)),
            "fixture must reach the flip window: {small_secs:?}"
        );
        for &(regen, streams) in &small_secs {
            assert_eq!(
                streams, 1,
                "small frame section with {regen} literals must be one stream"
            );
        }
        // The stock side keeps four streams over the flip window (sections
        // below 256 stay single there).
        for &(regen, streams) in &sections(&sink) {
            if (256..1024).contains(&regen) {
                assert_eq!(streams, 4, "unpledged stream section with {regen} literals");
            }
        }

        assert_eq!(decompress(&small, payload.len()).unwrap(), payload);
        assert_eq!(decompress(&sink, payload.len()).unwrap(), payload);
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

    /// A gated (incompressible) block followed by a tail block shorter than
    /// HASH_READ: the next scan's catch-up insertions must stop HASH_READ
    /// bytes short of the window end, because the bulk window borrows the
    /// caller's slice and the old `block_start` bound read past the
    /// allocation (heap overread, ASAN-visible as "READ of size 8" just
    /// past the input region). A plain run cannot observe the overread —
    /// this locks the no-panic + valid-output behavior; run under
    /// `-Zsanitizer=address` to catch the actual read.
    #[test]
    fn gated_block_followed_by_sub_hash_read_tail() {
        let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            pseudo_random ^= pseudo_random << 13;
            pseudo_random ^= pseudo_random >> 7;
            pseudo_random ^= pseudo_random << 17;
            pseudo_random
        };
        // 128 KiB uniform random: the whole first block, over the gate's
        // 16 KiB floor and past the entropy bar, so the gate opens the gap
        // deterministically and the tail block triggers the catch-up fill.
        let body: Vec<u8> = (0..128 * 1024).map(|_| (rand() & 0xff) as u8).collect();
        // Tail lengths around the fill's HASH_READ margin.
        for tail in [1usize, 4, 7, 8, 9] {
            let mut input = body.clone();
            input.extend_from_slice(&body[..tail]);
            // One level per catch-up arm: Fast / Dfast / Chain.
            for level in [Level::Fastest, Level::Fast, Level::Balanced] {
                let compressed = compress(&input, level);
                let out = decompress(&compressed, input.len()).unwrap();
                assert_eq!(out, input, "tail {tail} level {level:?}");
            }
        }
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
        let opts = EncoderOptions::new(Level::Ultra)
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

    /// A prepared dictionary must steer the decode off the parallel fast
    /// path exactly like a raw one (R53): the parallel decoder's
    /// sequential fallback builds a fresh frame decoder, so a dictionary
    /// it never saw would be silently dropped and the frame's dictID
    /// would fail to resolve.
    #[cfg(feature = "std")]
    #[test]
    fn threads_with_prepared_dictionary_decode() {
        let Ok(raw) = std::fs::read("./dict_tests/dictionary") else {
            return;
        };
        let payload: Vec<u8> = b"[Unit]\nDescription=shared boilerplate block\n".repeat(2048);
        let parsed = crate::EncoderDictionary::parse(&raw).unwrap();
        let frame = compress_with(
            &payload,
            &EncoderOptions::new(Level::Balanced).parsed_dictionary(&parsed),
        )
        .unwrap();
        let dict = alloc::sync::Arc::new(crate::decoding::Dictionary::load(&raw).unwrap());
        for threads in [1, 4] {
            let options = crate::DecoderOptions::new()
                .parsed_dictionary(alloc::sync::Arc::clone(&dict))
                .threads(threads);
            let out = decompress_with(&frame, 0, &options).unwrap();
            assert_eq!(out, payload, "threads {threads}");
            let mut buffer = vec![0u8; payload.len()];
            let written = decompress_to_buffer_with(&frame, &mut buffer, &options).unwrap();
            assert_eq!(&buffer[..written], &payload[..], "threads {threads}");
        }
    }
}
