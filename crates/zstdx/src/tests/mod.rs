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

pub mod bit_reader;
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
