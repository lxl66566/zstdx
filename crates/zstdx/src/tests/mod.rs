#[cfg(test)]
use alloc::vec;
#[cfg(test)]
use alloc::vec::Vec;

#[cfg(test)]
extern crate std;

#[cfg(all(test, not(feature = "std")))]
impl crate::io_nostd::Read for std::fs::File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, crate::io_nostd::Error> {
        std::io::Read::read(self, buf).map_err(|e| {
            if e.get_ref().is_none() {
                crate::io_nostd::Error::from(crate::io_nostd::ErrorKind::Other)
            } else {
                crate::io_nostd::Error::new(
                    crate::io_nostd::ErrorKind::Other,
                    alloc::boxed::Box::new(e.into_inner().unwrap()),
                )
            }
        })
    }
}

#[cfg(all(test, feature = "std"))]
#[allow(dead_code)]
fn assure_error_impl() {
    // not a real test just there to throw an compiler error if Error is not derived correctly

    use crate::decoding::errors::FrameDecoderError;
    let _: &dyn std::error::Error = &FrameDecoderError::NotYetInitialized;
}

#[cfg(all(test, feature = "std"))]
#[allow(dead_code)]
fn assure_decoder_send_sync() {
    // not a real test just there to throw an compiler error if FrameDecoder is Send + Sync

    use crate::decoding::FrameDecoder;
    let decoder = FrameDecoder::new();
    std::thread::spawn(move || {
        drop(decoder);
    });
}

#[test]
fn skippable_frame() {
    use crate::decoding::{errors, frame};

    let mut content = vec![];
    content.extend_from_slice(&0x184d2a50u32.to_le_bytes());
    content.extend_from_slice(&300u32.to_le_bytes());
    assert_eq!(8, content.len());
    let err = frame::read_frame_header(content.as_slice());
    assert!(matches!(
        err,
        Err(errors::ReadFrameHeaderError::SkipFrame {
            magic_number: 0x184d2a50u32,
            length: 300
        })
    ));

    content.clear();
    content.extend_from_slice(&0x184d2a5fu32.to_le_bytes());
    content.extend_from_slice(&0xffffffffu32.to_le_bytes());
    assert_eq!(8, content.len());
    let err = frame::read_frame_header(content.as_slice());
    assert!(matches!(
        err,
        Err(errors::ReadFrameHeaderError::SkipFrame {
            magic_number: 0x184d2a5fu32,
            length: 0xffffffff
        })
    ));
}

#[cfg(test)]
#[test]
fn test_frame_header_reading() {
    use crate::decoding::frame;

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let mut content = compressed.as_slice();
    let (_frame, _) = frame::read_frame_header(&mut content).unwrap();
}

#[test]
fn test_block_header_reading() {
    use crate::{decoding, decoding::frame};

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let mut content = compressed.as_slice();
    let (_frame, _) = frame::read_frame_header(&mut content).unwrap();

    let mut block_dec = decoding::block_decoder::new();
    let block_header = block_dec.read_block_header(&mut content).unwrap();
    let _ = block_header; //TODO validate blockheader in a smart way
}

#[test]
fn test_frame_decoder() {
    use crate::decoding::{BlockDecodingStrategy, FrameDecoder};

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let mut content = compressed.as_slice();

    let mut frame_dec = FrameDecoder::new();
    frame_dec.reset(&mut content).unwrap();
    frame_dec
        .decode_blocks(&mut content, BlockDecodingStrategy::All)
        .unwrap();
}

#[test]
fn test_decode_from_to() {
    use crate::decoding::FrameDecoder;

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let Some(original) = fixture_bytes("./decodecorpus_files/z000088") else {
        return;
    };
    let mut frame_dec = FrameDecoder::new();

    let content: Vec<u8> = compressed;

    let mut target = vec![0u8; 1024 * 1024];

    // first part
    let source1 = &content[..50 * 1024];
    let (read1, written1) = frame_dec
        .decode_from_to(source1, target.as_mut_slice())
        .unwrap();

    // second part explicitely without checksum
    let source2 = &content[read1..content.len() - 4];
    let (read2, written2) = frame_dec
        .decode_from_to(source2, &mut target[written1..])
        .unwrap();

    // must have decoded until checksum
    assert_eq!(read1 + read2, content.len() - 4);

    // insert checksum separatly to test that this is handled correctly
    let chksum_source = &content[read1 + read2..];
    let (read3, written3) = frame_dec
        .decode_from_to(chksum_source, &mut target[written1 + written2..])
        .unwrap();

    // this must result in these values because just the checksum was processed
    assert_eq!(read3, 4);
    assert_eq!(written3, 0);

    let read = read1 + read2 + read3;
    let written = written1 + written2;

    let result = &target.as_slice()[..written];

    assert_eq!(
        read,
        content.len(),
        "Byte counter was wrong (decoded bytes != input length)"
    );

    match frame_dec.get_checksum_from_data() {
        Some(chksum) => {
            #[cfg(feature = "hash")]
            if frame_dec.get_calculated_checksum().unwrap() == chksum {
                std::println!("Checksums are ok!\n");
            } else {
                std::println!(
                    "Checksum did not match! From data: {}, calculated while decoding: {}\n",
                    chksum,
                    frame_dec.get_calculated_checksum().unwrap()
                );
            }
            #[cfg(not(feature = "hash"))]
            std::println!(
                "Checksum feature not enabled, skipping. From data: {}\n",
                chksum
            );
        },
        None => std::println!("No checksums to test\n"),
    }

    assert_eq!(original.len(), result.len(), "Result has wrong length");

    let mut counter = 0;
    let min = if original.len() < result.len() {
        original.len()
    } else {
        result.len()
    };
    for idx in 0..min {
        if original[idx] != result[idx] {
            counter += 1;
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    assert_eq!(counter, 0, "Result differs from original");
}

/// A small window pins the flat decode buffer at its steady-state size
/// (window + 2 max blocks), far below this frame's output: the flat loop
/// must pause with source bytes left over and resume on the next call. It
/// used to hand those bytes to the ring-buffer loop, which decoded them
/// into an empty ring and rejected the legal frame with
/// NotEnoughBytesInDictionary.
#[test]
fn decode_from_to_resumes_after_flat_pause() {
    use crate::{EncoderOptions, InputShape, Level, bulk, decoding::FrameDecoder};

    let unit = b"the quick brown fox jumps over the lazy dog; ";
    let len = 4 * 1024 * 1024;
    let mut data = Vec::with_capacity(len);
    while data.len() < len {
        let take = unit.len().min(len - data.len());
        data.extend_from_slice(&unit[..take]);
    }
    let compressed = bulk::compress_with(
        &data,
        &EncoderOptions::new(Level::Fastest)
            .with_input_shape(InputShape::default().with_window_log(10)),
    )
    .unwrap();

    // The premise: the frame really declares a small window (the encoder
    // may round the forced log up).
    let (header, _) =
        crate::decoding::frame::read_frame_header(&mut compressed.as_slice()).unwrap();
    let window = header.window_size().unwrap();
    assert!(window <= 4096, "window {window} is not small");

    let mut dec = FrameDecoder::new();
    let mut src: &[u8] = &compressed;
    dec.reset(&mut src).unwrap();
    let mut target = vec![0u8; data.len()];
    let mut written = 0usize;
    let mut first_call_wrote = None;
    while !dec.is_finished() {
        let (read, out) = dec.decode_from_to(src, &mut target[written..]).unwrap();
        if first_call_wrote.is_none() {
            first_call_wrote = Some(out);
        }
        assert!(read + out > 0, "decode_from_to made no progress");
        src = &src[read..];
        written += out;
    }
    // The first call must have hit the flat buffer's high-water mark
    // instead of decoding the whole frame.
    assert!(
        first_call_wrote.unwrap() < data.len(),
        "flat loop never paused"
    );
    assert!(src.is_empty());
    assert_eq!(written, data.len());
    assert_eq!(&target[..], &data[..]);
}

/// Trailing bytes after a frame's last block belong to the next frame (or
/// the caller): the flat path must leave them in the source untouched. The
/// unguarded ring loop used to parse them as a block header, so a
/// multi-frame input died on a bogus header in the middle of frame two.
#[test]
fn decode_from_to_leaves_trailing_bytes() {
    use crate::{EncoderOptions, InputShape, Level, bulk, decoding::FrameDecoder};

    let opts = || {
        EncoderOptions::new(Level::Fastest)
            .with_input_shape(InputShape::default().with_window_log(10))
    };
    let a = b"first frame payload, repeated repeated repeated";
    let b: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let frame_a = bulk::compress_with(a, &opts()).unwrap();
    let frame_b = bulk::compress_with(&b, &opts()).unwrap();
    let mut both = frame_a.clone();
    both.extend_from_slice(&frame_b);

    let mut dec = FrameDecoder::new();
    let mut src: &[u8] = &both;
    dec.reset(&mut src).unwrap();
    let mut target = vec![0u8; a.len() + b.len()];
    let mut written = 0usize;
    while !dec.is_finished() {
        let (read, out) = dec.decode_from_to(src, &mut target[written..]).unwrap();
        assert!(read + out > 0, "decode_from_to made no progress");
        src = &src[read..];
        written += out;
    }
    assert_eq!(written, a.len());
    assert_eq!(&target[..written], &a[..]);
    // Exactly frame two is left for the caller.
    assert_eq!(src, frame_b.as_slice());

    let mut dec = FrameDecoder::new();
    let mut target2 = vec![0u8; b.len()];
    let mut src2: &[u8] = src;
    dec.reset(&mut src2).unwrap();
    let mut written2 = 0usize;
    while !dec.is_finished() {
        let (read, out) = dec.decode_from_to(src2, &mut target2[written2..]).unwrap();
        assert!(read + out > 0, "decode_from_to made no progress");
        src2 = &src2[read..];
        written2 += out;
    }
    assert!(src2.is_empty());
    assert_eq!(&target2[..], &b[..]);
}

#[test]
fn test_specific_file() {
    use crate::decoding::{BlockDecodingStrategy, FrameDecoder};

    let path = "./decodecorpus_files/z000068.zst";
    let Some(compressed) = fixture_bytes(path) else {
        return;
    };
    let mut content = compressed.as_slice();

    let mut frame_dec = FrameDecoder::new();
    frame_dec.reset(&mut content).unwrap();
    frame_dec
        .decode_blocks(&mut content, BlockDecodingStrategy::All)
        .unwrap();
    let result = frame_dec.collect().unwrap();

    let Some(original) = fixture_bytes("./decodecorpus_files/z000088") else {
        return;
    };

    std::println!("Results for file: {path}");

    if original.len() != result.len() {
        std::println!(
            "Result has wrong length: {}, should be: {}",
            result.len(),
            original.len()
        );
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
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    if counter > 0 {
        std::println!("Result differs in at least {counter} bytes from original");
    }
}

#[test]
#[cfg(feature = "std")]
fn test_streaming() {
    use std::io::Read;

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let Some(original) = fixture_bytes("./decodecorpus_files/z000088") else {
        return;
    };
    let mut content = compressed.as_slice();
    let mut stream = crate::decoding::StreamingDecoder::new(&mut content).unwrap();

    let mut result = Vec::new();
    Read::read_to_end(&mut stream, &mut result).unwrap();

    assert_eq!(original.len(), result.len(), "Result has wrong length");

    let mut counter = 0;
    let min = if original.len() < result.len() {
        original.len()
    } else {
        result.len()
    };
    for idx in 0..min {
        if original[idx] != result[idx] {
            counter += 1;
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    assert_eq!(counter, 0, "Result differs from original");

    // Test resetting to a new file while keeping the old decoder

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000068.zst") else {
        return;
    };
    let Some(original) = fixture_bytes("./decodecorpus_files/z000068") else {
        return;
    };
    let mut content = compressed.as_slice();
    let mut stream = crate::decoding::StreamingDecoder::new_with_decoder(
        &mut content,
        stream.into_frame_decoder(),
    )
    .unwrap();

    let mut result = Vec::new();
    Read::read_to_end(&mut stream, &mut result).unwrap();

    std::println!("Results for file:");

    assert_eq!(original.len(), result.len(), "Result has wrong length");

    let mut counter = 0;
    let min = if original.len() < result.len() {
        original.len()
    } else {
        result.len()
    };
    for idx in 0..min {
        if original[idx] != result[idx] {
            counter += 1;
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    assert_eq!(counter, 0, "Result differs from original");
}

#[test]
fn test_incremental_read() {
    use crate::decoding::FrameDecoder;

    let Some(compressed) = fixture_bytes("./decodecorpus_files/abc.txt.zst") else {
        return;
    };
    let mut unread_compressed_content = compressed.as_slice();

    let mut frame_dec = FrameDecoder::new();
    frame_dec.reset(&mut unread_compressed_content).unwrap();

    let mut output = [0u8; 3];
    let (_, written) = frame_dec
        .decode_from_to(unread_compressed_content, &mut output)
        .unwrap();

    assert_eq!(written, 3);
    assert_eq!(output.map(char::from), ['a', 'b', 'c']);

    assert!(frame_dec.is_finished());
    let written = frame_dec.collect_to_writer(&mut output[..]).unwrap();
    assert_eq!(written, 3);
    assert_eq!(output.map(char::from), ['d', 'e', 'f']);
}

#[test]
#[cfg(not(feature = "std"))]
fn test_streaming_no_std() {
    use crate::io::Read;

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000088.zst") else {
        return;
    };
    let Some(original) = fixture_bytes("./decodecorpus_files/z000088") else {
        return;
    };
    let mut content = compressed.as_slice();
    let mut stream = crate::decoding::StreamingDecoder::new(&mut content).unwrap();

    let mut result = vec![0; original.len()];
    Read::read_exact(&mut stream, &mut result).unwrap();

    assert_eq!(original.len(), result.len(), "Result has wrong length");

    let mut counter = 0;
    let min = if original.len() < result.len() {
        original.len()
    } else {
        result.len()
    };
    for idx in 0..min {
        if original[idx] != result[idx] {
            counter += 1;
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    assert_eq!(counter, 0, "Result differs from original");

    // Test resetting to a new file while keeping the old decoder

    let Some(compressed) = fixture_bytes("./decodecorpus_files/z000068.zst") else {
        return;
    };
    let Some(original) = fixture_bytes("./decodecorpus_files/z000068") else {
        return;
    };
    let mut content = compressed.as_slice();
    let mut stream = crate::decoding::StreamingDecoder::new_with_decoder(
        &mut content,
        stream.into_frame_decoder(),
    )
    .unwrap();

    let mut result = vec![0; original.len()];
    Read::read_exact(&mut stream, &mut result).unwrap();

    std::println!("Results for file:");

    assert_eq!(original.len(), result.len(), "Result has wrong length");

    let mut counter = 0;
    let min = if original.len() < result.len() {
        original.len()
    } else {
        result.len()
    };
    for idx in 0..min {
        if original[idx] != result[idx] {
            counter += 1;
            // std::println!(
            //    "Original {:3} not equal to result {:3} at byte: {}",
            //    original[idx], result[idx], idx,
            //);
        }
    }
    assert_eq!(counter, 0, "Result differs from original");
}

#[test]
fn test_decode_all() {
    use crate::decoding::{FrameDecoder, errors::FrameDecoderError};

    let skip_frame = |input: &mut Vec<u8>, length: usize| {
        input.extend_from_slice(&0x184d2a50u32.to_le_bytes());
        input.extend_from_slice(&(length as u32).to_le_bytes());
        input.resize(input.len() + length, 0);
    };

    let mut original = Vec::new();
    let mut input = Vec::new();

    let Some(z89z) = fixture_bytes("./decodecorpus_files/z000089.zst") else {
        return;
    };
    let Some(z89) = fixture_bytes("./decodecorpus_files/z000089") else {
        return;
    };
    let Some(z90z) = fixture_bytes("./decodecorpus_files/z000090.zst") else {
        return;
    };
    let Some(z90) = fixture_bytes("./decodecorpus_files/z000090") else {
        return;
    };

    skip_frame(&mut input, 300);
    input.extend_from_slice(&z89z);
    original.extend_from_slice(&z89);
    skip_frame(&mut input, 400);
    input.extend_from_slice(&z90z);
    original.extend_from_slice(&z90);
    skip_frame(&mut input, 500);

    let mut decoder = FrameDecoder::new();

    // decode_all with correct buffers.
    let mut output = vec![0; original.len()];
    let result = decoder.decode_all(&input, &mut output).unwrap();
    assert_eq!(result, original.len());
    assert_eq!(output, original);

    // decode_all with smaller output length.
    let mut output = vec![0; original.len() - 1];
    let result = decoder.decode_all(&input, &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::TargetTooSmall)),
        "{result:?}"
    );

    // decode_all with larger output length.
    let mut output = vec![0; original.len() + 1];
    let result = decoder.decode_all(&input, &mut output).unwrap();
    assert_eq!(result, original.len());
    assert_eq!(&output[..result], original);

    // decode_all with truncated regular frame.
    let mut output = vec![0; original.len()];
    let result = decoder.decode_all(&input[..input.len() - 600], &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::FailedToReadBlockBody(_))),
        "{result:?}"
    );

    // decode_all with truncated skip frame.
    let mut output = vec![0; original.len()];
    let result = decoder.decode_all(&input[..input.len() - 1], &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::FailedToSkipFrame)),
        "{result:?}"
    );

    // decode_all_to_vec with correct output capacity.
    let mut output = Vec::new();
    output.reserve_exact(original.len());
    decoder.decode_all_to_vec(&input, &mut output).unwrap();
    assert_eq!(output, original);

    // decode_all_to_vec with smaller output capacity.
    let mut output = Vec::new();
    output.reserve_exact(original.len() - 1);
    let result = decoder.decode_all_to_vec(&input, &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::TargetTooSmall)),
        "{result:?}"
    );

    // decode_all_to_vec with larger output capacity.
    let mut output = Vec::new();
    output.reserve_exact(original.len() + 1);
    decoder.decode_all_to_vec(&input, &mut output).unwrap();
    assert_eq!(output, original);
}

/// The bulk one-shot honors a pledged size exactly as the streaming core
/// does: the declaration lands in the frame header (a source the row's
/// window covers takes the single-segment form), an absent pledge leaves
/// the header undeclaring, and a disagreeing pledge fails with the same
/// error the stream raises at finish.
#[test]
fn bulk_pledged_size_declares_and_validates() {
    let mut data = alloc::vec![0u8; 300 * 1024];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for b in &mut data {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = (state >> 33) as u8 & 0xf;
    }
    let opts =
        crate::EncoderOptions::new(crate::Level::Fastest).pledged_size(Some(data.len() as u64));
    let frame = crate::bulk::compress_with(&data, &opts).unwrap();
    let (declared, _) = multi_frame::locate_fcs(&frame);
    assert_eq!(declared, data.len() as u64);
    assert_eq!(crate::bulk::decompress(&frame, data.len()).unwrap(), data);

    // No pledge: the header declares nothing (no FCS field, no
    // single-segment flag).
    let plain =
        crate::bulk::compress_with(&data, &crate::EncoderOptions::new(crate::Level::Fastest))
            .unwrap();
    assert_eq!(plain[4] >> 6, 0);
    assert_eq!((plain[4] >> 5) & 1, 0);

    // A disagreeing pledge is rejected outright, not silently dropped.
    let wrong =
        crate::EncoderOptions::new(crate::Level::Fastest).pledged_size(Some(data.len() as u64 + 1));
    match crate::bulk::compress_with(&data, &wrong) {
        Err(crate::Error::PledgedSizeMismatch { pledged, actual }) => {
            assert_eq!(
                (pledged, actual),
                (data.len() as u64 + 1, data.len() as u64)
            );
        },
        other => panic!("expected PledgedSizeMismatch, got {other:?}"),
    }
}

/// Plaintext that `test_fixtures/window_128mib.zst` and
/// `test_fixtures/window_256mib.zst` decode to. The 128mib window fixture was
/// produced with `zstd --long=27` (a 128 MiB window descriptor) from this exact
/// content, so the bytes must match what is generated here. The 256mib window
/// fixture was created by editing the zstd header to claim the larger window
#[cfg(test)]
fn window_128_and_256mib_plaintext() -> Vec<u8> {
    "The quick brown fox jumps over the lazy dog.\n"
        .repeat(4096)
        .into_bytes()
}

/// Plaintext that `test_fixtures/window_8mib.zst` decodes to. Produced with
/// `zstd --long=23` (an 8 MiB window descriptor), below the default limit.
#[cfg(test)]
fn window_8mib_plaintext() -> Vec<u8> {
    "Sphinx of black quartz, judge my vow.\n"
        .repeat(4096)
        .into_bytes()
}

/// Entries of a fixture directory, or `None` when it does not exist. Fixture
/// directories (`decodecorpus_files`, `dict_tests`, the fuzz artifacts) are
/// excluded from the published crate; corpus tests skip when they are absent.
#[cfg(test)]
fn fixture_entries(path: &str) -> Option<Vec<std::io::Result<std::fs::DirEntry>>> {
    match std::fs::read_dir(path) {
        Ok(entries) => Some(entries.collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::println!("skipping: {path} not present (excluded from the published crate)");
            None
        },
        Err(e) => panic!("failed to read fixture dir {path}: {e}"),
    }
}

/// Contents of a fixture file; `None` behaves like [`fixture_entries`].
#[cfg(test)]
fn fixture_bytes(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::println!("skipping: {path} not present (excluded from the published crate)");
            None
        },
        Err(e) => panic!("failed to read fixture {path}: {e}"),
    }
}

#[test]
fn test_large_window_decodes_when_limit_raised() {
    use crate::decoding::FrameDecoder;

    // 256 MiB window, above the default 128 MiB limit.
    let compressed = include_bytes!("../../test_fixtures/window_256mib.zst");
    let expected = window_128_and_256mib_plaintext();

    let mut decoder = FrameDecoder::new();
    decoder.set_max_window_size(300 * 1024 * 1024);
    assert_eq!(decoder.max_window_size(), 300 * 1024 * 1024);

    let mut output = vec![0u8; expected.len()];
    let written = decoder.decode_all(compressed, &mut output).unwrap();
    assert_eq!(written, expected.len());
    assert_eq!(output, expected);
}

#[test]
fn test_large_window_rejected_at_default_limit() {
    use crate::decoding::{DEFAULT_MAX_WINDOW_SIZE, FrameDecoder, errors::FrameDecoderError};

    let compressed = include_bytes!("../../test_fixtures/window_256mib.zst");

    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.max_window_size(), DEFAULT_MAX_WINDOW_SIZE);

    let mut output = vec![0u8; window_128_and_256mib_plaintext().len()];
    let result = decoder.decode_all(compressed, &mut output);
    // The reported max must be the effective configured limit, not the spec maximum.
    assert!(
        matches!(
            result,
            Err(FrameDecoderError::WindowSizeTooBig { requested, max })
                if requested == 256 * 1024 * 1024 && max == DEFAULT_MAX_WINDOW_SIZE
        ),
        "{result:?}"
    );
}

#[test]
fn test_multi_frame_large_window_decodes_when_raised() {
    use crate::decoding::FrameDecoder;

    // Two large-window frames back to back exercise both the first-frame
    // (FrameDecoderState::new) and later-frame (FrameDecoderState::reset) paths,
    // which historically applied the window check inconsistently.
    let frame = include_bytes!("../../test_fixtures/window_256mib.zst");
    let single = window_128_and_256mib_plaintext();

    let mut input = Vec::new();
    input.extend_from_slice(frame);
    input.extend_from_slice(frame);

    let mut expected = Vec::new();
    expected.extend_from_slice(&single);
    expected.extend_from_slice(&single);

    let mut decoder = FrameDecoder::new();
    decoder.set_max_window_size(300 * 1024 * 1024);

    let mut output = vec![0u8; expected.len()];
    let written = decoder.decode_all(&input, &mut output).unwrap();
    assert_eq!(written, expected.len());
    assert_eq!(output, expected);
}

#[test]
fn test_large_window_rejected_on_first_and_later_frames() {
    use crate::decoding::{FrameDecoder, errors::FrameDecoderError};

    let big = include_bytes!("../../test_fixtures/window_256mib.zst"); // 256 MiB window
    let small = include_bytes!("../../test_fixtures/window_8mib.zst"); // 8 MiB window

    // First frame: a single 256 MiB-window frame is rejected under the default limit.
    let mut decoder = FrameDecoder::new();
    let mut output = vec![0u8; window_128_and_256mib_plaintext().len()];
    let result = decoder.decode_all(big, &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::WindowSizeTooBig { .. })),
        "first frame should be rejected: {result:?}"
    );

    // Later frame: a small-window frame decodes, then a 256 MiB-window frame is
    // rejected under the default limit, covering the FrameDecoderState::reset path.
    let mut input = Vec::new();
    input.extend_from_slice(small);
    input.extend_from_slice(big);

    let mut decoder = FrameDecoder::new();
    let mut output =
        vec![0u8; window_8mib_plaintext().len() + window_128_and_256mib_plaintext().len()];
    let result = decoder.decode_all(&input, &mut output);
    assert!(
        matches!(result, Err(FrameDecoderError::WindowSizeTooBig { .. })),
        "later frame should be rejected: {result:?}"
    );
}

#[test]
#[cfg(feature = "std")]
fn test_streaming_decoder_max_window_size() {
    use std::io::Read;

    use crate::decoding::{StreamingDecoder, errors::FrameDecoderError};

    let compressed = include_bytes!("../../test_fixtures/window_256mib.zst");
    let expected = window_128_and_256mib_plaintext();

    // The default StreamingDecoder rejects the 256 MiB window on init.
    match StreamingDecoder::new(compressed.as_slice()) {
        Err(FrameDecoderError::WindowSizeTooBig { .. }) => {},
        Err(e) => panic!("expected WindowSizeTooBig, got {e:?}"),
        Ok(_) => panic!("expected WindowSizeTooBig, got a decoder"),
    }

    // Raising the limit lets the same frame decode through the streaming wrapper.
    let mut stream =
        StreamingDecoder::new_with_max_window_size(compressed.as_slice(), 300 * 1024 * 1024)
            .unwrap();
    let mut result = Vec::new();
    Read::read_to_end(&mut stream, &mut result).unwrap();
    assert_eq!(result, expected);
}

#[test]
fn test_max_window_size_clamped_to_format_maximum() {
    use crate::{common::MAX_WINDOW_SIZE, decoding::FrameDecoder};

    let mut decoder = FrameDecoder::new();

    // A value below the format maximum is kept verbatim.
    decoder.set_max_window_size(2 * 1024 * 1024 * 1024);
    assert_eq!(decoder.max_window_size(), 2 * 1024 * 1024 * 1024);

    // A value above the format maximum is clamped down to it.
    decoder.set_max_window_size(u64::MAX);
    assert_eq!(decoder.max_window_size(), MAX_WINDOW_SIZE);
}

/// `EncoderOptions::include_dictid(false)` suppresses the frame header's
/// Dictionary_ID while the dictionary stays fully in effect (libzstd's
/// `ZSTD_c_dictIDFlag = 0`): header inspection on both engines' frames,
/// roundtrips with the dictionary attached, and interop in both
/// directions. (R55)
#[test]
fn include_dictid_suppresses_the_header_field() {
    extern crate std;
    use alloc::{format, vec::Vec};

    // The crate's own io trait: std::io::Write is the wrong trait under
    // --no-default-features (the encoder implements crate::io::Write,
    // which the std feature re-exports from std::io).
    use crate::io::Write as _;

    let samples: Vec<Vec<u8>> = (0..96u32)
        .map(|i| {
            format!(
                "{{\"user\":\"user_{i}\",\"event\":\"click\",\"score\":{},\"tags\":[\"a\",\"b\"\
                 ]}}\n",
                i % 7
            )
            .into_bytes()
        })
        .collect();
    // a real trained dictionary, so the id is nonzero; skipped when the
    // training backend is unavailable
    let Ok(dict) = zstd::dict::from_samples(&samples, 8 * 1024) else {
        return;
    };
    let content = b"{\"user\":\"user_5\",\"event\":\"click\",\"score\":3,\"tags\":[\"a\",\"b\"]}\n"
        .repeat(96);

    // bulk: the header field follows the flag, and only that field
    let stamped = crate::bulk::compress_with(
        &content,
        &crate::EncoderOptions::new(crate::Level::Fast).dictionary(&dict),
    )
    .unwrap();
    let bare = crate::bulk::compress_with(
        &content,
        &crate::EncoderOptions::new(crate::Level::Fast)
            .dictionary(&dict)
            .include_dictid(false),
    )
    .unwrap();
    assert!(zstd::zstd_safe::get_dict_id_from_frame(&stamped).is_some());
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&bare), None);
    // our own header parse agrees: no dictionary_id field at all
    let (header, _) = crate::decoding::frame::read_frame_header(&*bare).unwrap();
    assert_eq!(header.dictionary_id(), None);
    // the suppressed frame is smaller: only the id field left out
    assert!(bare.len() < stamped.len());

    // the dictionary still applies on both engines
    let dec_opts = crate::DecoderOptions::new().dictionary(&dict);
    let roundtrip = crate::bulk::decompress_with(&bare, content.len(), &dec_opts).unwrap();
    assert_eq!(roundtrip, content);
    let mut zdec = zstd::bulk::Decompressor::with_dictionary(&dict).unwrap();
    let mut zout = alloc::vec![0u8; content.len()];
    let n = zdec.decompress_to_buffer(&bare, &mut zout).unwrap();
    assert_eq!(&zout[..n], &content[..]);

    // reverse direction: libzstd's own dictID-less dict frame decodes here
    let mut zenc = zstd::bulk::Compressor::with_dictionary(3, &dict).unwrap();
    zenc.include_dictid(false).unwrap();
    let zbare = zenc.compress(&content).unwrap();
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&zbare), None);
    let zroundtrip = crate::bulk::decompress_with(&zbare, content.len(), &dec_opts).unwrap();
    assert_eq!(zroundtrip, content);

    // streaming core: same suppression, same roundtrip
    let stream_bare = {
        let mut encoder = crate::stream::write::Encoder::with_options(
            Vec::new(),
            crate::EncoderOptions::new(crate::Level::Fast)
                .dictionary(&dict)
                .include_dictid(false),
        )
        .unwrap();
        encoder.write_all(&content).unwrap();
        encoder.finish().unwrap()
    };
    assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&stream_bare), None);
    let stream_roundtrip =
        crate::bulk::decompress_with(&stream_bare, content.len(), &dec_opts).unwrap();
    assert_eq!(stream_roundtrip, content);

    // Multithreaded stream core: the suppression holds there too (on
    // single-core machines the ST core runs and passes identically); the
    // no-std build has no workers at all.
    #[cfg(feature = "std")]
    let mt_bare = {
        let mut encoder = crate::stream::write::Encoder::with_options(
            Vec::new(),
            crate::EncoderOptions::new(crate::Level::Fast)
                .dictionary(&dict)
                .include_dictid(false)
                .workers(2)
                .pledged_size(Some(content.len() as u64)),
        )
        .unwrap();
        encoder.write_all(&content).unwrap();
        encoder.finish().unwrap()
    };
    #[cfg(feature = "std")]
    {
        assert_eq!(zstd::zstd_safe::get_dict_id_from_frame(&mt_bare), None);
        let mt_roundtrip =
            crate::bulk::decompress_with(&mt_bare, content.len(), &dec_opts).unwrap();
        assert_eq!(mt_roundtrip, content);
    }
}

/// A far-class tile (R55): a seeded 2 MiB stream of records — a short
/// word plus the generator's raw 8 bytes. The words keep every block
/// parsing (raw blocks never reach the LDM indexer below the full
/// window), the raw bytes keep the alphabet past the mid-size gate's
/// [`LDM_SYMS_MIN`](crate::encoding) floor, and the sequence is globally
/// unique, so the only beyond-window matches are the tile copies.
#[cfg(test)]
fn ldm_text_tile() -> Vec<u8> {
    const WORDS: [&[u8]; 12] = [
        b"frame",
        b"match",
        b"distance",
        b"window",
        b"encode",
        b"stream",
        b"prefix",
        b"check",
        b"level",
        b"table",
        b"block",
        b"hash",
    ];
    let mut tile = Vec::with_capacity(2 << 20);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    while tile.len() < 2 << 20 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        tile.extend_from_slice(WORDS[(state >> 33) as usize % WORDS.len()]);
        tile.extend_from_slice(&state.to_le_bytes());
        tile.push(b' ');
    }
    tile
}

/// `EncoderOptions::long_distance_matching` (R55): `Enable` forces the
/// far-match source on below every auto bar and raises the window to 27
/// when none is forced (a forced window wins), `Disable` forces it off,
/// and `Auto` (the default) leaves every default frame byte-identical.
#[test]
fn long_distance_matching_override() {
    // 6 MiB of a 2 MiB text tile: repeats sit at 2-4 MiB distances, past
    // the Fastest row's 1 MiB stock window and inside the raised one.
    let tile = ldm_text_tile();
    let mut data = tile.clone();
    data.extend_from_slice(&tile);
    data.extend_from_slice(&tile);

    let auto =
        crate::bulk::compress_with(&data, &crate::EncoderOptions::new(crate::Level::Fastest))
            .unwrap();
    let enabled = crate::bulk::compress_with(
        &data,
        &crate::EncoderOptions::new(crate::Level::Fastest)
            .long_distance_matching(crate::LdmMode::Enable),
    )
    .unwrap();
    // The far repeats are unreachable at the stock window and land in the
    // raised one: strictly smaller, different bytes, same content back.
    assert!(
        enabled.len() < auto.len(),
        "enable must reach the far repeats ({} vs {})",
        enabled.len(),
        auto.len()
    );
    assert_ne!(enabled, auto);
    assert_eq!(crate::bulk::decompress(&enabled, data.len()).unwrap(), data);
    // libzstd decodes the frame (plain interop: the wire form is ordinary)
    assert_eq!(
        zstd::decode_all(enabled.as_slice()).unwrap(),
        data,
        "libzstd decodes the LDM-enabled frame"
    );
    // The raised window lands in the header: the source clamp leaves 2^23.
    let (header, _) = crate::decoding::frame::read_frame_header(&*enabled).unwrap();
    assert_eq!(header.window_size().unwrap(), 1 << 23);

    // A forced window wins over the enable default: the header carries
    // 2^18, and at that window the far repeats stay out of reach (the
    // frame still roundtrips — the override never breaks the form).
    let forced = crate::bulk::compress_with(
        &data,
        &crate::EncoderOptions::new(crate::Level::Fastest)
            .long_distance_matching(crate::LdmMode::Enable)
            .with_input_shape(crate::InputShape::default().with_window_log(18)),
    )
    .unwrap();
    let (forced_header, _) = crate::decoding::frame::read_frame_header(&*forced).unwrap();
    assert_eq!(forced_header.window_size().unwrap(), 1 << 18);
    assert_eq!(crate::bulk::decompress(&forced, data.len()).unwrap(), data);

    // Disable where auto is disarmed anyway (6 MiB is far under the 64 MiB
    // auto bar): byte-identical to the default — the override never arms
    // anything on its own.
    let disabled = crate::bulk::compress_with(
        &data,
        &crate::EncoderOptions::new(crate::Level::Fastest)
            .long_distance_matching(crate::LdmMode::Disable),
    )
    .unwrap();
    assert_eq!(disabled, auto);

    // Streaming core: the same override engages there too. The frame
    // shape differs from the pledged bulk one (no content size, the
    // unclamped 2^27 window), so the evidence is its own auto twin.
    let stream = |mode: crate::LdmMode| {
        use crate::io::Write as _;
        let mut encoder = crate::stream::write::Encoder::with_options(
            Vec::new(),
            crate::EncoderOptions::new(crate::Level::Fastest).long_distance_matching(mode),
        )
        .unwrap();
        encoder.write_all(&data).unwrap();
        encoder.finish().unwrap()
    };
    let stream_enabled = stream(crate::LdmMode::Enable);
    let stream_auto = stream(crate::LdmMode::Auto);
    assert!(stream_enabled.len() < stream_auto.len());
    assert_eq!(
        crate::stream::decode_all(stream_enabled.as_slice()).unwrap(),
        data
    );
}

/// The Disable override where the auto policy arms: a 64 MiB far-class
/// source (the Frame arming bar), whose fast row auto-arms on geometry —
/// Disable must stand the split pass down and keep the stock window.
#[test]
fn long_distance_matching_disable_overrides_auto_arming() {
    // Exactly 2^26: the Frame bar is `declared >= LDM_FULL_WINDOW`, and
    // the stock window the clamp leaves (1 MiB) sits far below it.
    let tile = ldm_text_tile();
    let mut data = Vec::with_capacity(64 << 20);
    for _ in 0..32 {
        data.extend_from_slice(&tile);
    }

    let auto =
        crate::bulk::compress_with(&data, &crate::EncoderOptions::new(crate::Level::Fastest))
            .unwrap();
    let disabled = crate::bulk::compress_with(
        &data,
        &crate::EncoderOptions::new(crate::Level::Fastest)
            .long_distance_matching(crate::LdmMode::Disable),
    )
    .unwrap();
    assert_ne!(disabled, auto, "auto arms on this far-class source");
    assert!(
        disabled.len() > auto.len(),
        "the disarmed parse cannot reach the far repeats ({} vs {})",
        disabled.len(),
        auto.len()
    );
    // The disarmed frame keeps the stock window; the auto one widened.
    let (auto_header, _) = crate::decoding::frame::read_frame_header(&*auto).unwrap();
    let (off_header, _) = crate::decoding::frame::read_frame_header(&*disabled).unwrap();
    assert_eq!(auto_header.window_size().unwrap(), 1 << 26);
    assert_eq!(off_header.window_size().unwrap(), 1 << 20);
    assert_eq!(
        crate::bulk::decompress(&disabled, data.len()).unwrap(),
        data
    );
    assert_eq!(
        crate::bulk::decompress(&auto, data.len()).unwrap(),
        data,
        "the auto-armed far frame stays legal"
    );
}

/// The LDM override stays legal on every strategy family (R56): Enable
/// on the tagged rows is inert by design (no LDM candidate feed) and on
/// the btlazy2/opt rows arms the far source below every auto bar — each
/// family must still emit frames this engine and libzstd decode.
#[test]
fn ldm_override_roundtrips_every_family() {
    let data = ldm_text_tile();
    for level in [
        crate::Level::from_zstd(7), // tagged Row strategy: Enable is inert
        crate::Level::Best,         // btlazy2
        crate::Level::Opt,          // opt-family tree search
    ] {
        for mode in [crate::LdmMode::Enable, crate::LdmMode::Disable] {
            let frame = crate::bulk::compress_with(
                &data,
                &crate::EncoderOptions::new(level).long_distance_matching(mode),
            )
            .unwrap();
            assert_eq!(
                crate::bulk::decompress(&frame, data.len()).unwrap(),
                data,
                "{level:?}/{mode:?}"
            );
            assert_eq!(
                zstd::decode_all(frame.as_slice()).unwrap(),
                data,
                "{level:?}/{mode:?}: libzstd decodes"
            );
        }
    }
}

pub mod bit_reader;
pub mod block_output_cap;
pub mod decode_corpus;
pub mod dict_test;
#[cfg(feature = "std")]
pub mod encode_corpus;
pub mod fuzz_regressions;
pub mod multi_frame;

#[cfg(feature = "std")]
#[test]
fn verbose_disabled() {
    const { assert!(!crate::VERBOSE) }
}
