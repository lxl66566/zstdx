//! Tests for the streaming encoders: byte-equality with the one-shot paths,
//! option plumbing, drop behavior and interop with the reference decoder.

use alloc::{vec, vec::Vec};

use crate::{
    EncoderOptions, Level, bulk, encoding,
    io::Write as _,
    stream::{read, write},
};

#[cfg(test)]
fn shapes() -> Vec<Vec<u8>> {
    let mut pseudo_random = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = move || {
        pseudo_random ^= pseudo_random << 13;
        pseudo_random ^= pseudo_random >> 7;
        pseudo_random ^= pseudo_random << 17;
        pseudo_random
    };
    alloc::vec![
        alloc::vec![],
        alloc::vec![1],
        alloc::vec![7u8; 5],
        alloc::vec![b'x'; 300 * 1024],
        (0..128 * 1024).map(|_| (rand() & 0xff) as u8).collect(),
        (0..900 * 1024).map(|i| (i % 61) as u8).collect(),
        // exact block multiple: the trailing empty last block case
        alloc::vec![b'y'; 128 * 1024],
    ]
}

#[cfg(test)]
fn write_in_chunks<W: crate::io::Write>(data: &[u8], chunk: usize, enc: &mut write::Encoder<W>) {
    for piece in data.chunks(chunk) {
        enc.write_all(piece).unwrap();
    }
}

#[test]
fn write_encoder_matches_oneshot_bytes() {
    for input in shapes() {
        for level in [Level::Uncompressed, Level::Fastest] {
            let reference = encoding::compress_to_vec(input.as_slice(), level);
            // one big write, byte-sized writes and block-sized writes must
            // all produce the identical frame
            for chunk in [usize::MAX, 1, 7 * 1024, 128 * 1024] {
                let mut sink = Vec::new();
                let mut enc = write::Encoder::new(&mut sink, level).unwrap();
                write_in_chunks(&input, chunk, &mut enc);
                enc.finish().unwrap();
                assert_eq!(
                    sink,
                    reference,
                    "len {} level {level:?} chunk {chunk}",
                    input.len()
                );
            }
        }
    }
}

#[test]
fn read_encoder_matches_oneshot_bytes() {
    for input in shapes() {
        let reference = encoding::compress_to_vec(input.as_slice(), Level::Fastest);
        let mut enc = read::Encoder::new(input.as_slice(), Level::Fastest).unwrap();
        let mut out = Vec::new();
        crate::io::Read::read_to_end(&mut enc, &mut out).unwrap();
        assert_eq!(out, reference, "len {}", input.len());
    }
}

#[test]
fn pledged_size_lands_in_header() {
    let data = vec![b'z'; 100 * 1024];
    let mut sink = Vec::new();
    let mut enc = write::Encoder::with_options(
        &mut sink,
        EncoderOptions::new(Level::Fastest).pledged_size(Some(data.len() as u64)),
    )
    .unwrap();
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    assert_eq!(bulk::decompress(&sink, 0).unwrap(), data);
    #[cfg(feature = "std")]
    {
        let content_size = zstd::zstd_safe::get_frame_content_size(&sink).unwrap();
        assert_eq!(content_size, Some(data.len() as u64));
    }
}

#[test]
fn checksum_option_toggles_trailer() {
    let data = vec![b'c'; 64 * 1024];
    let with = {
        let mut sink = Vec::new();
        let mut enc = write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Fastest).checksum(true),
        )
        .unwrap();
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
        sink
    };
    let without = {
        let mut sink = Vec::new();
        let mut enc = write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Fastest).checksum(false),
        )
        .unwrap();
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
        sink
    };
    if cfg!(feature = "hash") {
        assert_eq!(with.len(), without.len() + 4);
    } else {
        assert_eq!(with.len(), without.len());
    }
    assert_eq!(bulk::decompress(&with, 0).unwrap(), data);
    assert_eq!(bulk::decompress(&without, 0).unwrap(), data);
}

#[test]
fn auto_finish_closes_frame() {
    let data = b"auto finished frame payload";
    let mut sink = Vec::new();
    {
        let mut enc = write::Encoder::new(&mut sink, Level::Fastest)
            .unwrap()
            .auto_finish();
        enc.write_all(data).unwrap();
    }
    assert_eq!(bulk::decompress(&sink, 0).unwrap(), data);
}

#[cfg(feature = "std")]
#[test]
fn on_finish_reports_result_and_writer() {
    let data = b"callback sees the finished writer";
    let mut sink = Vec::new();
    let seen = std::cell::RefCell::new(0usize);
    {
        let mut enc = write::Encoder::new(&mut sink, Level::Fastest)
            .unwrap()
            .on_finish(|res: crate::Result<&mut Vec<u8>>| {
                *seen.borrow_mut() = res.unwrap().len();
            });
        enc.write_all(data).unwrap();
    }
    // the callback received the same buffer the frame went into
    assert_eq!(*seen.borrow(), sink.len());
    assert_eq!(bulk::decompress(&sink, 0).unwrap(), data);
}

#[test]
fn flush_forces_partial_block() {
    // Incompressible payload: a flushed partial block is visible in the
    // sink as roughly its own size of bytes.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let data: Vec<u8> = (0..10 * 1024).map(|_| (rand() & 0xff) as u8).collect();
    let mut sink = Vec::new();
    let mut enc = write::Encoder::new(&mut sink, Level::Fastest).unwrap();
    enc.write_all(&data).unwrap();
    enc.flush().unwrap();
    // observe the sink through the encoder to keep the borrow valid
    let flushed_len = enc.get_ref().len();
    assert!(flushed_len > 5 * 1024);
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    let mut expect = data.clone();
    expect.extend_from_slice(&data);
    assert_eq!(bulk::decompress(&sink, 0).unwrap(), expect);
}

#[test]
fn workers_option_routes() {
    // std builds accept workers > 1 (multithreaded core, or the
    // single-threaded fallback for raw levels and single-core processes);
    // no_std builds keep rejecting it.
    #[cfg(feature = "std")]
    {
        let data = b"stream mt smoke payload";
        let mut sink = Vec::new();
        let mut enc =
            write::Encoder::with_options(&mut sink, EncoderOptions::new(Level::Fastest).workers(2))
                .unwrap();
        enc.write_all(data).unwrap();
        enc.finish().unwrap();
        assert_eq!(bulk::decompress(&sink, 0).unwrap(), data);
    }
    #[cfg(not(feature = "std"))]
    assert!(matches!(
        write::Encoder::with_options(Vec::new(), EncoderOptions::new(Level::Fastest).workers(2)),
        Err(crate::Error::Unsupported {
            feature: crate::Feature::Multithread
        })
    ));
}

#[cfg(feature = "std")]
#[test]
fn interop_with_zstd_crate() {
    let data: Vec<u8> = (0..300 * 1024).map(|i| (i % 199) as u8).collect();
    let mut sink = Vec::new();
    let mut enc = write::Encoder::new(&mut sink, Level::Fastest).unwrap();
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
    let mut decoded = Vec::new();
    zstd::stream::copy_decode(sink.as_slice(), &mut decoded).unwrap();
    assert_eq!(decoded, data);

    // read-side encoder output decodes with libzstd too
    let mut enc = read::Encoder::new(data.as_slice(), Level::Fastest).unwrap();
    let mut compressed = Vec::new();
    crate::io::Read::read_to_end(&mut enc, &mut compressed).unwrap();
    let mut decoded = Vec::new();
    zstd::stream::copy_decode(compressed.as_slice(), &mut decoded).unwrap();
    assert_eq!(decoded, data);
}

/// A concatenated stream: three frames of different shapes with skippable
/// frames wedged between them.
#[cfg(test)]
fn multi_frame_stream() -> (Vec<u8>, Vec<u8>) {
    let a = bulk::compress(b"first frame", Level::Fastest);
    let b = bulk::compress(vec![7u8; 200 * 1024].as_slice(), Level::Uncompressed);
    let c = bulk::compress(&[], Level::Fastest);
    let mut stream = Vec::new();
    stream.extend_from_slice(&a);
    stream.extend_from_slice(&0x184d2a50u32.to_le_bytes());
    stream.extend_from_slice(&300u32.to_le_bytes());
    stream.extend(core::iter::repeat_n(0xab, 300));
    stream.extend_from_slice(&b);
    stream.extend_from_slice(&0x184d2a5fu32.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes());
    stream.extend_from_slice(&c);
    let mut plain = Vec::new();
    plain.extend_from_slice(b"first frame");
    plain.extend(core::iter::repeat_n(7u8, 200 * 1024));
    (stream, plain)
}

#[test]
fn read_decoder_is_transparent_over_frames() {
    let (stream, plain) = multi_frame_stream();
    let mut dec = read::Decoder::new(stream.as_slice()).unwrap();
    let mut out = Vec::new();
    crate::io::Read::read_to_end(&mut dec, &mut out).unwrap();
    assert_eq!(out, plain);

    // single_frame stops after the first frame
    let mut dec = read::Decoder::new(stream.as_slice())
        .unwrap()
        .single_frame();
    let mut out = Vec::new();
    crate::io::Read::read_to_end(&mut dec, &mut out).unwrap();
    assert_eq!(out, b"first frame");
}

#[test]
fn write_decoder_is_transparent_over_frames() {
    let (stream, plain) = multi_frame_stream();
    let mut sink = Vec::new();
    {
        let mut dec = write::Decoder::new(&mut sink).unwrap();
        for chunk in stream.chunks(9 * 1024) {
            dec.write_all(chunk).unwrap();
        }
        dec.flush().unwrap();
    }
    assert_eq!(sink, plain);
}

#[test]
fn one_liner_functions_roundtrip() {
    let data: Vec<u8> = (0..150 * 1024).map(|i| (i * 31 % 251) as u8).collect();
    let compressed = crate::stream::encode_all(data.as_slice(), Level::Fastest).unwrap();
    assert_eq!(
        crate::stream::decode_all(compressed.as_slice()).unwrap(),
        data
    );
    let mut sink = Vec::new();
    crate::stream::copy_decode(compressed.as_slice(), &mut sink).unwrap();
    assert_eq!(sink, data);
    let mut re_encoded = Vec::new();
    crate::stream::copy_encode(data.as_slice(), &mut re_encoded, Level::Fastest).unwrap();
    assert_eq!(
        crate::stream::decode_all(re_encoded.as_slice()).unwrap(),
        data
    );
}

#[test]
fn truncated_stream_is_an_error() {
    let data: Vec<u8> = (0..100 * 1024).map(|i| (i % 97) as u8).collect();
    let compressed = bulk::compress(&data, Level::Fastest);
    let truncated = &compressed[..compressed.len() - 5];
    assert!(
        read::Decoder::new(truncated).is_err() || {
            let Ok(mut dec) = read::Decoder::new(truncated) else {
                return;
            };
            let mut out = Vec::new();
            crate::io::Read::read_to_end(&mut dec, &mut out).is_err()
        }
    );
    // garbage magic
    let bad = [0u8, 1, 2, 3, 4, 5, 6, 7];
    assert!(read::Decoder::new(bad.as_slice()).is_err());
    // clean empty stream is an error at construction (libzstd parity)
    assert!(read::Decoder::new(b"".as_slice()).is_err());
}

// Multithreaded streaming encoders: roundtrips through both decoders,
// output independent of the write pattern, byte-identity with the bulk mt
// path under an exact pledge, flush and read-side (pump_from) coverage.
#[cfg(feature = "std")]
mod mt {
    use alloc::format;

    use super::*;
    use crate::io::Read as _;

    fn lcg(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..len).map(|_| (rand() & 0xff) as u8).collect()
    }

    /// Text-like data: repeating vocabulary with variation, so matches,
    /// repcodes and entropy coding all engage across job boundaries.
    fn textish(len: usize) -> Vec<u8> {
        let words: [&[u8]; 13] = [
            b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ",
            b"lorem ", b"ipsum ", b"dolor ", b"sit ", b"amet ",
        ];
        let mut state = 7u64;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = words[((state >> 33) as usize) % words.len()];
            let take = w.len().min(len - out.len());
            out.extend_from_slice(&w[..take]);
        }
        out
    }

    fn shapes() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("text", textish(5 * 1024 * 1024 + 123)),
            ("zeros", vec![0u8; 4 * 1024 * 1024]),
            ("random", lcg(2 * 1024 * 1024 + 7)),
            // exact job-size multiple: the trailing empty last block case
            ("exact-jobs", textish(4 * 1024 * 1024)),
            ("small", textish(100 * 1024)),
            ("empty", Vec::new()),
        ]
    }

    fn encode_write(
        data: &[u8],
        chunk: usize,
        level: Level,
        workers: u32,
        checksum: bool,
        pledged: Option<u64>,
    ) -> Vec<u8> {
        let mut sink = Vec::new();
        let mut enc = write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(level)
                .workers(workers)
                .checksum(checksum)
                .pledged_size(pledged),
        )
        .unwrap();
        for piece in data.chunks(chunk) {
            enc.write_all(piece).unwrap();
        }
        enc.finish().unwrap();
        sink
    }

    fn assert_both_decoders(comp: &[u8], data: &[u8], label: &str) {
        assert_eq!(bulk::decompress(comp, 0).unwrap(), data, "{label}");
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(comp, &mut decoded).unwrap();
        assert_eq!(decoded, data, "{label}");
    }

    #[test]
    fn roundtrip_both_decoders() {
        for (name, data) in shapes() {
            for workers in [2u32, 4, 8] {
                for checksum in [true, false] {
                    let label = format!("{name}/mt{workers}/ck{checksum}");
                    let comp =
                        encode_write(&data, 64 * 1024, Level::Fastest, workers, checksum, None);
                    assert_both_decoders(&comp, &data, &label);
                }
            }
        }
    }

    /// Deeper levels must ride the job path too: the repcode gate and the
    /// fresh-table job start interact with the deeper search.
    #[test]
    fn chain_levels_roundtrip() {
        let data = textish(4 * 1024 * 1024);
        for level in [Level::Fast, Level::Balanced] {
            let comp = encode_write(&data, 128 * 1024, level, 4, true, None);
            assert_both_decoders(&comp, &data, &format!("{level:?}"));
        }
    }

    /// Without flushes the job grid is absolute, so the frame bytes must not
    /// depend on how the input was written — regardless of where the bursts
    /// land (whole-input writes burst once at the end, small writes several
    /// times on the way).
    #[test]
    fn output_independent_of_write_pattern() {
        let data = textish(5 * 1024 * 1024 + 77);
        for workers in [2u32, 4] {
            let reference = encode_write(&data, usize::MAX, Level::Fastest, workers, true, None);
            for chunk in [64 * 1024, 128 * 1024, 333 * 1024, 1024 * 1024] {
                assert_eq!(
                    encode_write(&data, chunk, Level::Fastest, workers, true, None),
                    reference,
                    "workers {workers} chunk {chunk}"
                );
            }
        }
    }

    /// A stream written exactly to its pledge shares the bulk job grid (and
    /// job flags), so the outputs must be byte-identical.
    #[test]
    fn pledged_stream_matches_bulk_mt() {
        let data = textish(7 * 1024 * 1024 + 999);
        for workers in [2u32, 4] {
            for checksum in [true, false] {
                let bulk_mt =
                    encoding::mt::compress_slice_mt(&data, Level::Fastest, checksum, workers);
                let streamed = encode_write(
                    &data,
                    1024 * 1024,
                    Level::Fastest,
                    workers,
                    checksum,
                    Some(data.len() as u64),
                );
                assert_eq!(streamed, bulk_mt, "workers {workers} checksum {checksum}");
            }
        }
    }

    /// A flush makes the pending bytes visible early at the cost of a
    /// re-gridded job boundary; the reassembled stream must still decode.
    #[test]
    fn flush_interleaved_roundtrip() {
        let a = textish(2 * 1024 * 1024 + 5);
        let b = lcg(1024 * 1024);
        let mut sink = Vec::new();
        let mut enc =
            write::Encoder::with_options(&mut sink, EncoderOptions::new(Level::Fastest).workers(3))
                .unwrap();
        enc.write_all(&a).unwrap();
        enc.flush().unwrap();
        enc.write_all(&b).unwrap();
        enc.flush().unwrap();
        enc.write_all(&a).unwrap();
        enc.finish().unwrap();
        let mut expect = a.clone();
        expect.extend_from_slice(&b);
        expect.extend_from_slice(&a);
        assert_both_decoders(&sink, &expect, "flush-interleaved");
    }

    /// The read-side encoder drives the core through pump_from's small
    /// reads; the multithreaded bursts must stay transparent there.
    #[test]
    fn read_encoder_roundtrip() {
        let data = textish(3 * 1024 * 1024 + 9);
        for workers in [2u32, 4] {
            let mut enc = read::Encoder::with_options(
                data.as_slice(),
                EncoderOptions::new(Level::Fastest)
                    .checksum(true)
                    .workers(workers),
            )
            .unwrap();
            let mut comp = Vec::new();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish();
            assert_both_decoders(&comp, &data, &format!("read-side mt{workers}"));
        }
    }

    /// Raw-block levels fall back to the single-threaded core: the output
    /// must be byte-identical to a workers-less stream.
    #[test]
    fn uncompressed_level_falls_back_to_single() {
        let data = lcg(300 * 1024);
        let mt = encode_write(&data, 64 * 1024, Level::Uncompressed, 4, false, None);
        let st = encode_write(&data, 64 * 1024, Level::Uncompressed, 0, false, None);
        assert_eq!(mt, st);
    }
}

#[cfg(feature = "std")]
#[test]
fn decoders_decode_libzstd_frames() {
    let data: Vec<u8> = (0..128 * 1024).map(|i| (i % 211) as u8).collect();
    let compressed = zstd::stream::encode_all(data.as_slice(), 3).unwrap();

    let mut out = Vec::new();
    crate::io::Read::read_to_end(
        &mut read::Decoder::new(compressed.as_slice()).unwrap(),
        &mut out,
    )
    .unwrap();
    assert_eq!(out, data);

    let mut sink = Vec::new();
    {
        let mut dec = write::Decoder::new(&mut sink).unwrap();
        dec.write_all(&compressed).unwrap();
        dec.flush().unwrap();
    }
    assert_eq!(sink, data);
}
