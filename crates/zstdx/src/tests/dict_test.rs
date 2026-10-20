#[test]
fn test_dict_parsing() {
    use alloc::vec;

    use crate::decoding::dictionary::Dictionary;
    let mut raw = vec![0u8; 8];

    // correct magic num
    raw[0] = 0x37;
    raw[1] = 0xa4;
    raw[2] = 0x30;
    raw[3] = 0xec;

    // dict-id
    let dict_id = 0x47232101;
    raw[4] = 0x01;
    raw[5] = 0x21;
    raw[6] = 0x23;
    raw[7] = 0x47;

    // tables copied from ./dict_tests/dictionary
    let raw_tables = &[
        54, 16, 192, 155, 4, 0, 207, 59, 239, 121, 158, 116, 220, 93, 114, 229, 110, 41, 249, 95,
        165, 255, 83, 202, 254, 68, 74, 159, 63, 161, 100, 151, 137, 21, 184, 183, 189, 100, 235,
        209, 251, 174, 91, 75, 91, 185, 19, 39, 75, 146, 98, 177, 249, 14, 4, 35, 0, 0, 0, 40, 40,
        20, 10, 12, 204, 37, 196, 1, 173, 122, 0, 4, 0, 128, 1, 2, 2, 25, 32, 27, 27, 22, 24, 26,
        18, 12, 12, 15, 16, 11, 69, 37, 225, 48, 20, 12, 6, 2, 161, 80, 40, 20, 44, 137, 145, 204,
        46, 0, 0, 0, 0, 0, 116, 253, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    raw.extend(&raw_tables[..]);

    // offset history 3,10,25 (every rep must fall inside the 25 content bytes,
    // mirroring libzstd's ZSTD_loadDEntropy check)
    raw.extend(vec![3, 0, 0, 0]);
    raw.extend(vec![10, 0, 0, 0]);
    raw.extend(vec![25, 0, 0, 0]);

    // just some random bytes
    let raw_content = vec![
        1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 1, 1, 123, 3, 234, 23, 234, 34, 23, 234, 34, 34, 234, 234,
    ];
    raw.extend(&raw_content);

    let dict = Dictionary::decode_dict(&raw).unwrap();

    assert_eq!(dict.id, dict_id, "Dict-id did not get parsed correctly");
    assert_eq!(
        dict.dict_content, raw_content,
        "dict content did not get parsed correctly"
    );
    assert_eq!(
        dict.offset_hist,
        [3, 10, 25],
        "offset history did not get parsed correctly"
    );

    // test magic num checking
    raw[0] = 1;
    raw[1] = 1;
    raw[2] = 1;
    raw[3] = 1;
    assert!(
        Dictionary::decode_dict(&raw).is_err(),
        "The dict got decoded but the magic num was incorrect!"
    );

    // a rep of 0 or one past the content size is rejected at load time
    raw[0] = 0x37;
    raw[1] = 0xa4;
    raw[2] = 0x30;
    raw[3] = 0xec;
    let corrupt_reps: [&[u8]; 2] = [
        &[0, 0, 0, 0, 10, 0, 0, 0, 25, 0, 0, 0], // rep1 == 0
        &[3, 0, 0, 0, 10, 0, 0, 0, 26, 0, 0, 0], // rep3 > content size
    ];
    for reps in corrupt_reps {
        let mut corrupt = raw[..raw.len() - 12 - raw_content.len()].to_vec();
        corrupt.extend_from_slice(reps);
        corrupt.extend_from_slice(&raw_content);
        assert!(
            Dictionary::decode_dict(&corrupt).is_err(),
            "dict with reps {reps:?} must be rejected"
        );
    }
}

#[test]
fn test_dict_decoding() {
    extern crate std;
    use alloc::{borrow::ToOwned, string::String, vec::Vec};
    use std::{
        fs,
        io::{BufReader, Read},
        println,
    };

    use crate::decoding::{BlockDecodingStrategy, FrameDecoder};

    let mut success_counter = 0;
    let mut fail_counter_diff = 0;
    let mut fail_counter_size = 0;
    let mut fail_counter_bytes_read = 0;
    let mut total_counter = 0;
    let mut failed: Vec<String> = Vec::new();

    let mut speeds = Vec::new();
    let mut speeds_read = Vec::new();

    let Some(entries) = super::fixture_entries("./dict_tests/files") else {
        return;
    };
    let mut files: Vec<_> = entries.into_iter().collect();
    let dict = BufReader::new(fs::File::open("./dict_tests/dictionary").unwrap());
    let dict: Vec<u8> = dict.bytes().map(|x| x.unwrap()).collect();

    files.sort_by_key(|x| match x {
        Err(_) => String::new(),
        Ok(entry) => entry.path().to_str().unwrap().to_owned(),
    });

    let mut frame_dec = FrameDecoder::new();
    let dict = crate::decoding::dictionary::Dictionary::decode_dict(&dict).unwrap();
    frame_dec.add_dict(dict).unwrap();

    for file in files {
        let f = file.unwrap();
        let metadata = f.metadata().unwrap();
        let file_size = metadata.len();

        let p = String::from(f.path().to_str().unwrap());
        // corpus files are generated with a lowercase .zst suffix
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if !p.ends_with(".zst") {
            continue;
        }
        println!("Trying file: {p}");

        let mut content = fs::File::open(f.path()).unwrap();

        frame_dec.reset(&mut content).unwrap();

        let start_time = std::time::Instant::now();
        /////DECODING
        frame_dec
            .decode_blocks(&mut content, BlockDecodingStrategy::All)
            .unwrap();
        let result = frame_dec.collect().unwrap();
        let end_time = start_time.elapsed();

        match frame_dec.get_checksum_from_data() {
            Some(chksum) => {
                #[cfg(feature = "hash")]
                if frame_dec.get_calculated_checksum().unwrap() == chksum {
                    println!("Checksums are ok!\n");
                } else {
                    println!(
                        "Checksum did not match! From data: {}, calculated while decoding: {}\n",
                        chksum,
                        frame_dec.get_calculated_checksum().unwrap()
                    );
                }
                #[cfg(not(feature = "hash"))]
                println!(
                    "Checksum feature not enabled, skipping. From data: {}\n",
                    chksum
                );
            },
            None => println!("No checksums to test\n"),
        }

        let mut original_p = p.clone();
        original_p.truncate(original_p.len() - 4);
        let original_f = BufReader::new(fs::File::open(original_p).unwrap());
        let original: Vec<u8> = original_f.bytes().map(|x| x.unwrap()).collect();

        println!("Results for file: {}", p.clone());
        let mut success = true;

        if original.len() != result.len() {
            println!(
                "Result has wrong length: {}, should be: {}",
                result.len(),
                original.len()
            );
            success = false;
            fail_counter_size += 1;
        }

        if frame_dec.bytes_read_from_source() != file_size {
            println!(
                "Framedecoder counted wrong amount of bytes: {}, should be: {}",
                frame_dec.bytes_read_from_source(),
                file_size
            );
            success = false;
            fail_counter_bytes_read += 1;
        }

        let mut counter = 0;
        let min = if original.len() < result.len() {
            original.len()
        } else {
            result.len()
        };
        for idx in 0..min {
            if original[idx] != result[idx] {
                counter += 1;
                // println!(
                //    "Original {} not equal to result {} at byte: {}",
                //    original[idx], result[idx], idx,
                //);
            }
        }

        if counter > 0 {
            println!("Result differs in at least {counter} bytes from original");
            success = false;
            fail_counter_diff += 1;
        }

        if success {
            success_counter += 1;
        } else {
            failed.push(p.clone());
        }
        total_counter += 1;

        let dur = end_time.as_micros() as usize;
        let speed = result.len()
            / if dur == 0 {
                1
            } else {
                dur
            };
        let speed_read = file_size as usize
            / if dur == 0 {
                1
            } else {
                dur
            };
        println!("SPEED: {speed}");
        println!("SPEED_read: {speed_read}");
        speeds.push(speed);
        speeds_read.push(speed_read);
    }

    println!("###################");
    println!("Summary:");
    println!("###################");
    println!(
        "Total: {total_counter}, Success: {success_counter}, WrongSize: {fail_counter_size}, \
         WrongBytecount: {fail_counter_bytes_read}, Diffs: {fail_counter_diff}"
    );
    println!("Failed files: ");
    for f in &failed {
        println!("{f}");
    }

    let speed_len = speeds.len();
    let sum_speed: usize = speeds.into_iter().sum();
    let avg_speed = sum_speed / speed_len;
    let avg_speed_bps = avg_speed * 1_000_000;
    if avg_speed_bps < 1000 {
        println!("Average speed: {avg_speed_bps} B/s");
    } else if avg_speed_bps < 1_000_000 {
        println!("Average speed: {} KB/s", avg_speed_bps / 1000);
    } else {
        println!("Average speed: {} MB/s", avg_speed_bps / 1_000_000);
    }

    let speed_read_len = speeds_read.len();
    let sum_speed_read: usize = speeds_read.into_iter().sum();
    let avg_speed_read = sum_speed_read / speed_read_len;
    let avg_speed_read_bps = avg_speed_read * 1_000_000;
    if avg_speed_read_bps < 1000 {
        println!("Average speed reading: {avg_speed_read_bps} B/s");
    } else if avg_speed_bps < 1_000_000 {
        println!("Average speed reading: {} KB/s", avg_speed_read_bps / 1000);
    } else {
        println!(
            "Average speed reading: {} MB/s",
            avg_speed_read_bps / 1_000_000
        );
    }

    assert!(failed.is_empty(), "failed files: {failed:?}");
}

/// Encoder-side dictionary roundtrips and libzstd interop over the real
/// `zstd --train` fixture: compress the raw .service files against the
/// dictionary, decode with our decoder and with libzstd, and cross-check
/// sizes against libzstd's own dictionary encoding of the same files.
#[test]
fn test_dict_encoding() {
    extern crate std;
    use alloc::vec::Vec;
    use std::fs;

    let Some(entries) = super::fixture_entries("./dict_tests/files") else {
        return;
    };
    let dict: Vec<u8> = fs::read("./dict_tests/dictionary").unwrap();

    let mut files: Vec<_> = entries
        .into_iter()
        .filter_map(|f| {
            let p = f.ok()?.path();
            (p.extension()?.to_str()? == "service").then_some(p)
        })
        .collect();
    files.sort();
    let files = &files[..files.len().min(40)];

    let mut ours_smaller = 0;
    let mut theirs_smaller = 0;
    for path in files {
        let data = fs::read(path).unwrap();
        for level in [
            crate::Level::Fastest,
            crate::Level::Fast,
            crate::Level::Balanced,
        ] {
            let opts = crate::EncoderOptions::new(level).dictionary(&dict);
            let compressed = crate::bulk::compress_with(&data, &opts).unwrap();
            // our decoder, dictionary attached
            let dec_opts = crate::DecoderOptions::new().dictionary(&dict);
            let roundtrip =
                crate::bulk::decompress_with(&compressed, data.len(), &dec_opts).unwrap();
            assert_eq!(roundtrip, data, "{path:?} at {level:?}");
            // libzstd decodes our dictionary frames
            let mut zdec = zstd::bulk::Decompressor::with_dictionary(&dict).unwrap();
            let mut zout = alloc::vec![0u8; data.len()];
            let n = zdec.decompress_to_buffer(&compressed, &mut zout).unwrap();
            assert_eq!(&zout[..n], &data[..], "libzstd decode of {path:?}");
            // and encodes the same content for a size cross-check
            let mut zenc = zstd::bulk::Compressor::with_dictionary(level.as_i32(), &dict).unwrap();
            let zcomp = zenc.compress(&data).unwrap();
            if compressed.len() <= zcomp.len() {
                ours_smaller += 1;
            } else {
                theirs_smaller += 1;
            }
        }
    }
    assert!(
        ours_smaller + theirs_smaller > 0,
        "no dictionary comparisons ran"
    );
    std::println!("dict sizes: ours <= libzstd in {ours_smaller}, behind in {theirs_smaller}");
}

/// `FrameCompressor::set_parsed_dictionary` shares one parse across
/// frames: multi-frame reuse on a single compressor produces the exact
/// bytes the raw `set_dictionary` form (one fresh compressor per frame,
/// parse per frame) produces, and the two attach forms displace each
/// other.
#[test]
fn frame_compressor_parsed_dictionary_multi_frame() {
    extern crate std;
    use alloc::vec::Vec;
    use std::fs;

    let Some(entries) = super::fixture_entries("./dict_tests/files") else {
        return;
    };
    let dict: Vec<u8> = fs::read("./dict_tests/dictionary").unwrap();
    let mut files: Vec<_> = entries
        .into_iter()
        .filter_map(|f| {
            let p = f.ok()?.path();
            (p.extension()?.to_str()? == "service").then_some(p)
        })
        .collect();
    files.sort();
    let sources: Vec<Vec<u8>> = files
        .into_iter()
        .take(3)
        .map(|p| fs::read(p).unwrap())
        .collect();
    if sources.is_empty() {
        return;
    }

    // Parsed form: one compressor, many frames, one shared parse.
    let parsed = crate::EncoderDictionary::parse(&dict).unwrap();
    let mut enc = crate::encoding::FrameCompressor::new(crate::Level::Fast);
    enc.set_parsed_dictionary(&parsed);
    let mut parsed_frames = Vec::new();
    for src in &sources {
        enc.set_source(src.as_slice());
        enc.set_drain(Vec::new());
        enc.compress();
        let frame = enc.set_drain(Vec::new()).expect("drain was just set");
        parsed_frames.push(frame);
    }
    assert!(parsed_frames.iter().all(|f| !f.is_empty()));

    // Raw form: a fresh compressor per frame parses its own dictionary and
    // must land on the same bytes.
    for (src, parsed_frame) in sources.iter().zip(&parsed_frames) {
        let mut raw = crate::encoding::FrameCompressor::new(crate::Level::Fast);
        raw.set_dictionary(&dict);
        raw.set_source(src.as_slice());
        let mut out = Vec::new();
        raw.set_drain(&mut out);
        raw.compress();
        assert_eq!(&out, parsed_frame, "raw and parsed forms disagree");
    }

    // The attach forms displace each other: parsed after raw, raw after
    // parsed.
    let src = sources[0].as_slice();
    enc.set_dictionary(&dict);
    enc.set_source(src);
    enc.set_drain(Vec::new());
    enc.compress();
    let raw_again = enc.set_drain(Vec::new()).unwrap();
    assert_eq!(raw_again, parsed_frames[0]);
    enc.set_parsed_dictionary(&parsed);
    enc.set_source(src);
    enc.set_drain(Vec::new());
    enc.compress();
    let parsed_again = enc.set_drain(Vec::new()).unwrap();
    assert_eq!(parsed_again, parsed_frames[0]);

    // and every frame roundtrips through the dictionary decoder
    let mut dec = crate::decoding::FrameDecoder::new();
    dec.add_dict(crate::decoding::Dictionary::decode_dict(&dict).unwrap())
        .unwrap();
    for (src, frame) in sources.iter().zip(&parsed_frames) {
        let mut input = frame.as_slice();
        dec.reset(&mut input).unwrap();
        dec.decode_blocks(&mut input, crate::decoding::BlockDecodingStrategy::All)
            .unwrap();
        assert_eq!(dec.collect().unwrap(), *src);
    }
}

/// The ring decode path (dictionary frames) enforces the declared
/// Frame_Content_Size against its own output count: a pledged dict stream
/// decodes clean, and corrupting only the header's FCS field rejects the
/// frame at the finish point.
#[test]
fn dict_frame_content_size_is_enforced() {
    extern crate std;
    use alloc::{string::String, vec::Vec};
    use std::{fs, println};

    use crate::decoding::{BlockDecodingStrategy, FrameDecoder, errors::FrameDecoderError};

    let Some(dict) = super::fixture_bytes("./dict_tests/dictionary") else {
        return;
    };
    let Some(mut files) = super::fixture_entries("./dict_tests/files") else {
        return;
    };
    files.sort_by_key(|f| match f {
        Ok(entry) => entry.path().to_string_lossy().into_owned(),
        Err(_) => String::new(),
    });
    let Some(path) = files
        .into_iter()
        .filter_map(|f| f.ok().map(|f| f.path()))
        .find(|p| p.extension().is_some_and(|e| e == "service"))
    else {
        return;
    };
    let mut data = fs::read(path).unwrap();
    data.truncate(64 * 1024);

    // A pledged dictionary stream: its header declares the content size.
    let mut sink = Vec::new();
    let mut enc = crate::stream::write::Encoder::with_options(
        &mut sink,
        crate::EncoderOptions::new(crate::Level::Fastest)
            .dictionary(&dict)
            .pledged_size(Some(data.len() as u64)),
    )
    .unwrap();
    crate::io::Write::write_all(&mut enc, &data).unwrap();
    enc.finish().unwrap();

    let decode = |frame: &[u8]| -> Result<Vec<u8>, FrameDecoderError> {
        let mut dec = FrameDecoder::new();
        dec.add_dict(crate::decoding::Dictionary::decode_dict(&dict).unwrap())?;
        let mut source = frame;
        dec.reset(&mut source)?;
        dec.decode_blocks(&mut source, BlockDecodingStrategy::All)?;
        dec.collect().ok_or(FrameDecoderError::NotYetInitialized)
    };

    assert_eq!(decode(&sink).unwrap(), data);

    // Corrupt only the declared FCS field: the blocks still decode, the
    // frame must still be rejected, naming the mismatch.
    let (declared, fcs_at) = super::multi_frame::locate_fcs(&sink);
    assert_eq!(declared, data.len() as u64);
    let mut corrupt = sink;
    corrupt[fcs_at] = corrupt[fcs_at].wrapping_add(1);
    match decode(&corrupt) {
        Err(FrameDecoderError::ContentSizeMismatch { declared, actual }) => {
            assert_eq!(declared, data.len() as u64 + 1);
            assert_eq!(actual, data.len() as u64);
        },
        other => panic!("expected ContentSizeMismatch, got {other:?}"),
    }
    println!("dict frame FCS check at {} bytes", data.len());
}

/// Fixtures for the seed-image cache gates: the formatted dictionary plus
/// three payload shapes (4 KiB json-ish, 64 KiB text, 4 KiB of dict-twin
/// repeats) and a raw-content dictionary.
#[cfg(all(test, feature = "std"))]
struct SeedCacheFixtures {
    formatted: alloc::vec::Vec<u8>,
    raw: alloc::vec::Vec<u8>,
    payloads: alloc::vec::Vec<alloc::vec::Vec<u8>>,
}

#[cfg(all(test, feature = "std"))]
fn seed_cache_fixtures() -> Option<SeedCacheFixtures> {
    extern crate std;
    use alloc::{vec, vec::Vec};
    use std::fs;
    let formatted = fs::read("./dict_tests/dictionary").ok()?;
    let mut text = Vec::new();
    for entry in super::fixture_entries("./dict_tests/files")?
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("service") {
            text.extend_from_slice(&fs::read(path).unwrap());
            if text.len() >= 64 * 1024 {
                break;
            }
        }
    }
    if text.is_empty() {
        return None;
    }
    let mut json = alloc::string::String::new();
    for i in 0..128 {
        use core::fmt::Write as _;
        core::write!(
            json,
            "{{\"unit\":{},\"state\":\"active\",\"path\":\"/system slice {}\",\"score\":{}}}",
            i,
            i % 7,
            i * 31
        )
        .unwrap();
    }
    // Repeats of dictionary content: the shape the dictionary exists for.
    let twins = formatted[formatted.len() - 4096..].to_vec();
    let raw = formatted[64..64 + 48 * 1024].to_vec();
    Some(SeedCacheFixtures {
        formatted,
        raw,
        payloads: vec![json.into_bytes(), text, twins],
    })
}

/// The uncached fresh-seed reference for a (dict, level, payload): the
/// mt-fallback slice entry builds a fresh owned state per call.
#[cfg(all(test, feature = "std"))]
fn seed_cache_fresh_frame(
    dict: &crate::EncoderDictionary,
    level: crate::Level,
    payload: &[u8],
) -> alloc::vec::Vec<u8> {
    let shared = dict.shared();
    crate::encoding::compress_slice_with_dictionary(
        payload,
        level,
        false,
        crate::InputShape {
            len: Some(payload.len() as u64),
            window_log: None,
        },
        &shared,
        None,
    )
}

/// The seed-image cache's output contract: a frame seeded from a restored
/// image is byte-identical to one seeded fresh, across the level ladder,
/// both dictionary kinds, and cache misses that must not hit (level and
/// dictionary changes interleaved between hits).
#[cfg(feature = "std")]
#[test]
fn dict_seed_image_cache_byte_identity() {
    let Some(f) = seed_cache_fixtures() else {
        return;
    };
    let dicts = [&f.formatted[..], &f.raw[..]];
    for level in [1i32, 3, 9, 13, 15, 19] {
        for (di, &dict) in dicts.iter().enumerate() {
            let parsed = crate::EncoderDictionary::parse(dict).unwrap();
            let level = crate::Level::from_zstd(level);
            let cached = |payload: &[u8]| -> alloc::vec::Vec<u8> {
                let options = crate::EncoderOptions::new(level)
                    .checksum(false)
                    .parsed_dictionary(&parsed);
                crate::bulk::compress_with(payload, &options).unwrap()
            };
            let other_level = crate::Level::from_zstd(if level.as_i32() == 1 {
                19
            } else {
                1
            });
            let other_options = crate::EncoderOptions::new(other_level)
                .checksum(false)
                .parsed_dictionary(&parsed);
            let other_dict_options = crate::EncoderOptions::new(level)
                .checksum(false)
                .parsed_dictionary(&crate::EncoderDictionary::parse(&f.raw).unwrap());

            for payload in &f.payloads {
                // Capture (pool cold on this thread for the key, or left
                // by an earlier iteration with another key).
                let first = cached(payload);
                assert_eq!(
                    first,
                    seed_cache_fresh_frame(&parsed, level, payload),
                    "capture frame (dict {di}, L{})",
                    level.as_i32()
                );
                // Hit: same key again, pool warm from the frame above.
                assert_eq!(
                    cached(payload),
                    seed_cache_fresh_frame(&parsed, level, payload),
                    "restore frame (dict {di}, L{})",
                    level.as_i32()
                );
                // A different payload under the same key: different
                // shape (its own key), must equal its own fresh frame
                // whether it hit or missed.
                let short = &payload[..payload.len() / 2];
                assert_eq!(
                    cached(short),
                    seed_cache_fresh_frame(&parsed, level, short),
                    "half-size frame (dict {di}, L{})",
                    level.as_i32()
                );
                // Interleaved misses: another level, another dictionary,
                // then the original key again (the slot was displaced, so
                // this re-seeds and re-captures).
                crate::bulk::compress_with(payload, &other_options).unwrap();
                crate::bulk::compress_with(payload, &other_dict_options).unwrap();
                assert_eq!(
                    cached(payload),
                    seed_cache_fresh_frame(&parsed, level, payload),
                    "post-miss frame (dict {di}, L{})",
                    level.as_i32()
                );
            }
        }
    }
}

/// The cache's `FrameCompressor` reuse path: multi-frame reuse through the
/// per-instance slot produces the fresh-seed bytes at every frame, with
/// key-displacing frames interleaved.
#[cfg(feature = "std")]
#[test]
fn dict_seed_image_cache_frame_compressor_reuse() {
    let Some(f) = seed_cache_fixtures() else {
        return;
    };
    let sources: alloc::vec::Vec<&[u8]> =
        f.payloads.iter().map(alloc::vec::Vec::as_slice).collect();
    for level in [crate::Level::from_zstd(3), crate::Level::from_zstd(19)] {
        let parsed = crate::EncoderDictionary::parse(&f.formatted).unwrap();
        // A fresh compressor per frame is the fresh-seed reference (the
        // same frame shape the reuse path emits, checksum and all).
        let fresh_frame = |src: &[u8]| {
            let mut fresh = crate::encoding::FrameCompressor::new(level);
            fresh.set_parsed_dictionary(&parsed);
            fresh.set_source(src);
            let mut out = alloc::vec::Vec::new();
            fresh.set_drain(&mut out);
            fresh.compress();
            out
        };
        let mut enc = crate::encoding::FrameCompressor::new(level);
        enc.set_parsed_dictionary(&parsed);
        for src in &sources {
            enc.set_source(*src);
            enc.set_drain(alloc::vec::Vec::new());
            enc.compress();
            let reused = enc.set_drain(alloc::vec::Vec::new()).unwrap();
            assert_eq!(reused, fresh_frame(src), "reuse L{}", level.as_i32());
        }
        // Displace the key (a raw dictionary), then come back: the next
        // frame re-seeds and still matches.
        enc.set_dictionary(&f.raw[..1024]);
        enc.set_source(sources[0]);
        enc.set_drain(alloc::vec::Vec::new());
        enc.compress();
        let _ = enc.set_drain(alloc::vec::Vec::new()).unwrap();
        enc.set_parsed_dictionary(&parsed);
        enc.set_source(sources[0]);
        enc.set_drain(alloc::vec::Vec::new());
        enc.compress();
        let back = enc.set_drain(alloc::vec::Vec::new()).unwrap();
        assert_eq!(
            back,
            fresh_frame(sources[0]),
            "post-displacement L{}",
            level.as_i32()
        );
    }
}

/// The cache's stream-core path: encoders materializing from the pooled
/// core (dict cores pool now, see `ST_CORE_POOL`) restore the seeded
/// image and produce the fresh-core bytes.
#[cfg(feature = "std")]
#[test]
fn dict_seed_image_cache_stream_core() {
    let Some(f) = seed_cache_fixtures() else {
        return;
    };
    let parsed = crate::EncoderDictionary::parse(&f.formatted).unwrap();
    let encode = |payload: &[u8], level: crate::Level| -> alloc::vec::Vec<u8> {
        let mut sink = alloc::vec::Vec::new();
        let mut enc = crate::stream::write::Encoder::with_options(
            &mut sink,
            crate::EncoderOptions::new(level)
                .checksum(false)
                .parsed_dictionary(&parsed),
        )
        .unwrap();
        crate::io::Write::write_all(&mut enc, payload).unwrap();
        enc.finish().unwrap();
        sink
    };
    for level in [
        crate::Level::from_zstd(3),
        crate::Level::from_zstd(13),
        crate::Level::from_zstd(19),
    ] {
        // First encoder on this thread: fresh core (capture). Later ones:
        // pooled cores (restores). A plain encoder and an other-level
        // dict encoder ride the same pool in between — the plain one
        // re-seeds through its own path, the other-level one displaces
        // the dict image.
        let first = encode(&f.payloads[0], level);
        let second = encode(&f.payloads[1], level);
        let third = encode(&f.payloads[0], level);
        let _plain = {
            let mut sink = alloc::vec::Vec::new();
            let mut enc = crate::stream::write::Encoder::with_options(
                &mut sink,
                crate::EncoderOptions::new(crate::Level::from_zstd(1)),
            )
            .unwrap();
            crate::io::Write::write_all(&mut enc, b"plain interlude").unwrap();
            enc.finish().unwrap();
            sink
        };
        let fourth = encode(&f.payloads[1], level);
        let _ = encode(&f.payloads[0], crate::Level::from_zstd(9));
        let fifth = encode(&f.payloads[0], level);
        assert_eq!(third, first, "restored stream core L{}", level.as_i32());
        assert_eq!(
            fifth,
            first,
            "post-displacement stream core L{}",
            level.as_i32()
        );
        assert_eq!(
            fourth,
            second,
            "second payload stream core L{}",
            level.as_i32()
        );
        // And the frames decode with the dictionary.
        let mut dec = crate::decoding::FrameDecoder::new();
        dec.add_dict(crate::decoding::Dictionary::decode_dict(&f.formatted).unwrap())
            .unwrap();
        for (frame, want) in [(&first, &f.payloads[0]), (&second, &f.payloads[1])] {
            let mut input = frame.as_slice();
            dec.reset(&mut input).unwrap();
            dec.decode_blocks(&mut input, crate::decoding::BlockDecodingStrategy::All)
                .unwrap();
            assert_eq!(dec.collect().unwrap(), *want);
        }
    }
}

/// The restore leaves the driver exactly in the freshly seeded state
/// (modulo the documented unreachable/neutralized residue) and the
/// entropy side with the dictionary tables: the construction-level twin
/// of the byte-identity gates.
#[cfg(feature = "std")]
#[test]
fn dict_seed_image_state_equality() {
    use crate::encoding::frame_compressor::{CompressState, new_owned_state};
    let Some(f) = seed_cache_fixtures() else {
        return;
    };
    for level in [1i32, 3, 9, 13, 15, 19] {
        let level = crate::Level::from_zstd(level);
        let dict = alloc::sync::Arc::new(
            crate::encoding::dictionary::EncDictionary::parse(&f.formatted).unwrap(),
        );
        let shape = crate::InputShape {
            len: Some(f.payloads[0].len() as u64 + dict.content.len() as u64),
            window_log: None,
        };
        // Fresh seeding, canonicalized like a capture would.
        let mut fresh: CompressState<crate::encoding::match_generator::MatchGeneratorDriver> =
            new_owned_state();
        crate::encoding::dictionary::reset_with_dictionary_mode(
            &mut fresh,
            &dict,
            level,
            shape,
            crate::LdmMode::Auto,
        );
        fresh.matcher.canonicalize_for_seed_capture();
        // The eager tree fill mirrors the capture path's seeding order
        // (canonicalize, fill, capture).
        fresh.matcher.seed_fill_dictionary_tree();
        // Pooled seeding: capture, run a frame to dirty the state,
        // displace the key with another level, dirty again, then a key
        // hit restores.
        let mut pooled = new_owned_state();
        crate::encoding::dictionary::seed_with_cache(
            &mut pooled,
            &dict,
            level,
            shape,
            crate::LdmMode::Auto,
        );
        let _ = crate::encoding::frame_compressor::compress_with_state_dictionary(
            &mut pooled,
            &f.payloads[1],
            dict.header_id(),
            false,
            None,
        );
        crate::encoding::dictionary::seed_with_cache(
            &mut pooled,
            &dict,
            crate::Level::from_zstd(level.as_i32() % 19 + 1),
            shape,
            crate::LdmMode::Auto,
        );
        let _ = crate::encoding::frame_compressor::compress_with_state_dictionary(
            &mut pooled,
            &f.payloads[2],
            dict.header_id(),
            false,
            None,
        );
        crate::encoding::dictionary::seed_with_cache(
            &mut pooled,
            &dict,
            level,
            shape,
            crate::LdmMode::Auto,
        );
        assert!(
            crate::encoding::match_generator::same_seeded_state(&fresh.matcher, &pooled.matcher),
            "driver state diverges at L{}",
            level.as_i32()
        );
        assert_eq!(
            fresh.last_huff_table.is_some(),
            pooled.last_huff_table.is_some()
        );
        assert_eq!(
            fresh.fse_tables.ll_previous.is_some(),
            pooled.fse_tables.ll_previous.is_some()
        );
        assert_eq!(fresh.dict_entropy.huff, pooled.dict_entropy.huff);
        assert_eq!(fresh.dict_entropy.ll, pooled.dict_entropy.ll);
        assert_eq!(fresh.dict_entropy.cost_mode, pooled.dict_entropy.cost_mode);
    }
}
