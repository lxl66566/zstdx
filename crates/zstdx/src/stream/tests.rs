//! Tests for the streaming encoders: byte-equality with the one-shot paths,
//! option plumbing, drop behavior and interop with the reference decoder.

use alloc::{collections::VecDeque, vec, vec::Vec};

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

/// A pledged stream below 256 bytes used to serialize a 1-byte
/// Frame_Content_Size field the windowed descriptor cannot express (flag 0
/// means the field is absent), shifting every following byte — both this
/// crate's and the reference decoder rejected the frame. The declaration is
/// dropped now and the frame must roundtrip.
#[test]
fn small_pledged_streams_roundtrip() {
    for n in [0usize, 1, 200, 255, 256, 300] {
        let data = vec![b'x'; n];
        let mut sink = Vec::new();
        let mut enc = write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Fastest).pledged_size(Some(n as u64)),
        )
        .unwrap();
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
        assert_eq!(crate::stream::decode_all(&sink[..]).unwrap(), data, "n={n}");
        #[cfg(feature = "std")]
        assert_eq!(
            zstd::stream::decode_all(&sink[..]).unwrap(),
            data,
            "reference n={n}"
        );
    }
}

/// A stream pledged a content size it did not write fails at finish instead
/// of emitting a frame every decoder rejects (libzstd treats the pledge as a
/// hard contract). Both directions (over- and undershoot) on the
/// single-threaded core and, on multi-core builds, the mt core: workers(4)
/// keeps the output empty because neither core encodes a 20-byte tail ahead
/// of finish.
#[test]
fn pledged_size_mismatch_fails_finish() {
    let data = vec![b'q'; 20];
    #[cfg(feature = "std")]
    let workers = [1u32, 4];
    #[cfg(not(feature = "std"))]
    let workers = [1u32];
    for w in workers {
        for pledged in [10u64, 100] {
            let mut sink = Vec::new();
            let mut enc = write::Encoder::with_options(
                &mut sink,
                EncoderOptions::new(Level::Fastest)
                    .workers(w)
                    .pledged_size(Some(pledged)),
            )
            .unwrap();
            enc.write_all(&data).unwrap();
            match enc.finish() {
                Err(crate::Error::PledgedSizeMismatch {
                    pledged: p,
                    actual: a,
                }) => assert_eq!((p, a), (pledged, data.len() as u64)),
                other => panic!("workers {w} pledge {pledged}: got {other:?}"),
            }
            assert!(sink.is_empty(), "no frame may be emitted");
        }
    }
}

/// The read-side encoder surfaces the same contract: a source that ends
/// short of the pledge errors on the read hitting EOF (the core finishes
/// there), and finish() keeps reporting the failure afterwards.
#[test]
fn pledged_size_mismatch_fails_read_encoder() {
    let data = vec![b'r'; 40];
    let mut enc = read::Encoder::with_options(
        data.as_slice(),
        EncoderOptions::new(Level::Fastest).pledged_size(Some(100)),
    )
    .unwrap();
    let mut out = Vec::new();
    // The read path crosses the io::Error boundary, so the check matches the
    // Display text the umbrella error carries through both io backends.
    let err = crate::io::Read::read_to_end(&mut enc, &mut out).unwrap_err();
    let msg = alloc::format!("{err}");
    assert!(
        msg.contains("pledged content size 100 does not match the 40 bytes written"),
        "{err}"
    );
    match enc.finish() {
        Err(crate::Error::PledgedSizeMismatch {
            pledged: 100,
            actual: 40,
        }) => {},
        other => panic!("expected PledgedSizeMismatch, got {other:?}"),
    }
}

/// Finishing while encoded bytes are still pending would drop the frame's
/// closing block (the last block + checksum are the pending tail), leaving
/// every byte already read a truncated stream decoders reject. finish()
/// rejects that state instead; the escape is reading to end of stream.
#[test]
fn finish_with_unread_output_errors() {
    let data: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    let mut enc = read::Encoder::new(data.as_slice(), Level::Fastest).unwrap();
    let mut partial = [0u8; 128];
    let n = crate::io::Read::read(&mut enc, &mut partial).unwrap();
    assert!(n > 0);
    match enc.finish() {
        Err(crate::Error::UnreadOutput { bytes }) => assert!(bytes > 0, "{bytes}"),
        other => panic!("expected UnreadOutput, got {other:?}"),
    }

    // the documented contract: read to end of stream, then finish reclaims
    // the reader and the delivered frame decodes
    let mut enc = read::Encoder::new(data.as_slice(), Level::Fastest).unwrap();
    let mut comp = Vec::new();
    crate::io::Read::read_to_end(&mut enc, &mut comp).unwrap();
    enc.finish().unwrap();
    assert_eq!(bulk::decompress(&comp, 0).unwrap(), data);
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

/// A writer serving a scripted sequence of partial accepts (`Ok(n)`) and
/// errors, then accepting everything; `received` holds the bytes a real
/// writer would have taken.
#[cfg(test)]
struct ScriptedWriter {
    script: VecDeque<Result<usize, crate::io::Error>>,
    received: Vec<u8>,
}

#[cfg(test)]
impl crate::io::Write for ScriptedWriter {
    fn write(&mut self, buf: &[u8]) -> Result<usize, crate::io::Error> {
        match self.script.pop_front() {
            Some(Ok(n)) => {
                let n = n.min(buf.len());
                self.received.extend_from_slice(&buf[..n]);
                Ok(n)
            },
            Some(Err(e)) => Err(e),
            None => {
                self.received.extend_from_slice(buf);
                Ok(buf.len())
            },
        }
    }

    fn flush(&mut self) -> Result<(), crate::io::Error> {
        Ok(())
    }
}

fn would_block() -> crate::io::Error {
    crate::io::Error::from(crate::io::ErrorKind::WouldBlock)
}

/// Regression: a drain failure after a partial underlying write must keep
/// the undelivered encoded bytes addressable — clearing the pending output
/// on error dropped a frame stretch the writer never received, and the
/// retried stream shipped corrupt (both decoder-refused and silent).
#[test]
fn partial_write_then_error_keeps_pending_output() {
    // Two-plus full blocks: the first drain carries bytes the writer
    // half-accepts before failing.
    let data: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    let mut writer = ScriptedWriter {
        script: [Ok(1), Err(would_block())].into(),
        received: Vec::new(),
    };
    let mut enc = write::Encoder::new(&mut writer, Level::Fastest).unwrap();
    assert!(crate::io::Write::write(&mut enc, &data).is_err());
    // observe the sink through the encoder to keep the borrow valid
    assert_eq!(enc.get_ref().received.len(), 1);
    // The retry (here the flush of the staged tail) resumes the same bytes;
    // the finished frame must decode to exactly the input.
    enc.flush().unwrap();
    let writer = enc.finish().unwrap();
    assert_eq!(bulk::decompress(&writer.received, 0).unwrap(), data);
    #[cfg(feature = "std")]
    {
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(writer.received.as_slice(), &mut decoded).unwrap();
        assert_eq!(decoded, data);
    }
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

    /// A job whose strip tail sits in a period-1 run acquires the seed
    /// offset 1; once the job-start repcode gate opens, the rep probe's
    /// one-byte pos-advance makes the seed probe's candidate land on the
    /// iteration's own index — a zero-offset self-compare the store gate
    /// priced with ilog2(0). The panicked worker's jobs assembled empty,
    /// shipping frames missing whole jobs (the 2026-09-17 P0: unpledged
    /// row-9 stream-mt frames corrupt past 32 MiB). The second text head
    /// supplies the far-match anchors the failing alignment needs; the
    /// word generator is fixed so the alignment is reproduced exactly.
    #[test]
    fn strip_tail_period_run_roundtrips() {
        let mut rand = {
            let mut state = 0x9e37_79b9_7f4a_7c15u64;
            move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            }
        };
        let words: Vec<Vec<u8>> = (0..64)
            .map(|_| {
                let len = 3 + (rand() % 7) as usize;
                (0..len).map(|_| 97 + (rand() % 26) as u8).collect()
            })
            .collect();
        let textish_words = |len: usize| {
            let mut state = 7u64;
            let mut out = Vec::with_capacity(len);
            while out.len() < len {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let w = &words[((state >> 33) as usize) % words.len()];
                let take = w.len().min(len - out.len());
                out.extend_from_slice(&w[..take]);
            }
            out
        };
        let head = textish_words(384 * 1024);
        let mut data = Vec::with_capacity(head.len() * 2 + 384 * 1024);
        data.extend_from_slice(&head);
        data.resize(head.len() + 384 * 1024, 0);
        data.extend_from_slice(&head);
        for workers in [2u32, 8] {
            let comp = encode_write(&data, usize::MAX, Level::Balanced, workers, true, None);
            assert_both_decoders(&comp, &data, &format!("seed1/mt{workers}"));
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

    /// One write_all carrying the whole 32 MiB stream: far above the epoch
    /// scale, so the write loop refills through many bounded recycling
    /// steps; it must neither stall nor corrupt the frame.
    #[test]
    fn single_write_all_roundtrip() {
        let data = textish(32 * 1024 * 1024);
        let comp = encode_write(&data, usize::MAX, Level::Fastest, 4, true, None);
        assert_both_decoders(&comp, &data, "32MiB single write");
    }

    /// The reach probe on the multithreaded stream core (see
    /// `encoding::reach_probe`): an open-ended Balanced stream crossing the
    /// probe's staging gate runs the deferred decision, and the frame bytes
    /// must stay a pure function of the input — whichever way the decision
    /// lands and however the writes are chunked.
    #[test]
    fn reach_probe_unpledged_mt_write_independent() {
        let gate = encoding::reach_probe::PROBE_MIN_FRAME as usize;
        for (name, data) in [
            ("jsonish", jsonish(gate + 1024 * 1024)),
            ("near_local", near_local(gate + 1024 * 1024)),
        ] {
            let reference = encode_write(&data, usize::MAX, Level::Balanced, 4, false, None);
            for chunk in [64 * 1024, 333 * 1024, 3 * 1024 * 1024] {
                assert_eq!(
                    encode_write(&data, chunk, Level::Balanced, 4, false, None),
                    reference,
                    "{name}: chunk {chunk}"
                );
            }
            assert_eq!(
                bulk::decompress(&reference, data.len()).unwrap(),
                data,
                "{name}"
            );
        }
        // A flush ahead of the gate decides the stock reach; the stream
        // still must roundtrip.
        let data = jsonish(gate + 512 * 1024);
        let mut sink = Vec::new();
        let mut enc = write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Balanced).workers(4),
        )
        .unwrap();
        write_in_chunks(&data[..1024 * 1024], 128 * 1024, &mut enc);
        enc.flush().unwrap();
        write_in_chunks(&data[1024 * 1024..], 128 * 1024, &mut enc);
        enc.finish().unwrap();
        assert_eq!(bulk::decompress(&sink, data.len()).unwrap(), data);
    }

    /// A stream written exactly to its pledge shares the bulk job grid (and
    /// job flags), so the outputs must be byte-identical.
    #[test]
    fn pledged_stream_matches_bulk_mt() {
        let data = textish(7 * 1024 * 1024 + 999);
        for workers in [2u32, 4] {
            for checksum in [true, false] {
                let bulk_mt =
                    encoding::mt::compress_slice_mt(&data, Level::Fastest, checksum, workers, None);
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
        // The opt rows exercise the strip/job-floor coupling and ultra's
        // job-boundary seed parse — the deep tiers' job paths. One combo:
        // the deep parse is slow under debug builds (the data still splits
        // into two jobs at the opt strip's 4 MiB floor).
        let deep = textish(4 * 1024 * 1024 + 512 * 1024);
        for level in [Level::Opt, Level::Ultra] {
            let bulk_mt = encoding::mt::compress_slice_mt(&deep, level, false, 4, None);
            let streamed =
                encode_write(&deep, 1024 * 1024, level, 4, false, Some(deep.len() as u64));
            assert_eq!(streamed, bulk_mt, "{level:?}");
        }
        // The Balanced row on a shrink-class head: the deferred stream
        // probe shares the bulk strip, job floor and reach, so a pledged
        // stream stays byte-identical to bulk mt there too.
        let near = jsonish(9 * 1024 * 1024 + 123 * 1024);
        let bulk_mt = encoding::mt::compress_slice_mt(&near, Level::Balanced, false, 4, None);
        let streamed = encode_write(
            &near,
            1024 * 1024,
            Level::Balanced,
            4,
            false,
            Some(near.len() as u64),
        );
        assert_eq!(streamed, bulk_mt);
    }

    /// The pledged mid-size capture (see `MtEncoderCore::engage_midsize_capture`):
    /// a keep-class frame whose clamped window lands in the bulk capture's
    /// class must share the bulk grid, strips and JobPrefix arming, so the
    /// pledged stream is byte-identical to bulk mt — without it the clamped
    /// window sits below the `Job` bar and the stream pays a chain-only
    /// single-job parse (dll32: 5,460,884 vs bulk's 4,390,255). The corpus
    /// mirrors `mt_midsize_prefix_ldm_keeps_far_repeats`: a code-like
    /// wide-alphabet head, a 1 MiB in-unit repeat, and four mutated copies
    /// of one 5 MiB unit (redundancy beyond the chain reach, exactly LDM's
    /// class).
    #[test]
    fn pledged_midsize_capture_matches_bulk() {
        let unit_len = 5 * 1024 * 1024;
        let mut unit = lcg(unit_len);
        let head_len = 256 * 1024;
        let mut patterns = Vec::with_capacity(96);
        let mut state = 0x243f_6a88_85a3_08d3u64;
        for _ in 0..96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            patterns.push(state.to_le_bytes());
        }
        let mut head = Vec::with_capacity(head_len);
        let mut pick = 0xdead_beef_cafeu64;
        while head.len() < head_len {
            pick = pick.wrapping_mul(6364136223846793005).wrapping_add(1);
            head.extend_from_slice(&patterns[(pick >> 33) as usize % 96]);
        }
        unit[..head_len].copy_from_slice(&head);
        unit[1024 * 1024..1024 * 1024 + head_len].copy_from_slice(&head);
        let mut data = Vec::with_capacity(4 * unit_len);
        for copy in 0..4u32 {
            for (i, &b) in unit.iter().enumerate() {
                data.push(if i % 4096 == (copy as usize * 1024) % 4096 {
                    b ^ 0x5a
                } else {
                    b
                });
            }
        }
        let st = encoding::compress_slice_to_vec(&data, Level::Balanced);
        for (workers, checksum) in [(4u32, true), (4, false), (8, true)] {
            let bulk_mt =
                encoding::mt::compress_slice_mt(&data, Level::Balanced, checksum, workers, None);
            let streamed = encode_write(
                &data,
                1024 * 1024,
                Level::Balanced,
                workers,
                checksum,
                Some(data.len() as u64),
            );
            assert_eq!(streamed, bulk_mt, "workers {workers} checksum {checksum}");
            assert!(
                bulk_mt.len() <= st.len() + st.len() / 50,
                "the far class must ride the capture: mt {} vs st {}",
                bulk_mt.len(),
                st.len()
            );
        }
    }

    #[test]
    fn pooled_state_reuse_is_output_neutral() {
        use crate::encoding::{
            frame_compressor::{compress_job_blocks, new_slice_state, reset_slice_state},
            match_generator::LdmArming,
            reach_probe::ReachChoice,
        };
        let data = jsonish(11 * 1024 * 1024 + 100 * 1024);
        let shape = crate::InputShape::default();
        let reset =
            |st: &mut encoding::frame_compressor::CompressState<encoding::MatchGeneratorDriver>| {
                reset_slice_state(
                    st,
                    Level::Balanced,
                    shape,
                    ReachChoice::Shrink,
                    LdmArming::Job,
                );
            };
        let mk = || {
            let mut st = new_slice_state();
            reset(&mut st);
            st
        };
        // Job B on a fresh state vs on a pooled state that already parsed a
        // prior job (up to a full-window one): the reset + strip prefill
        // must retire every output-shaping carryover, so the pool's reuse
        // keeps a job's bytes a function of the frame content alone.
        let b = 5 * 1024 * 1024..10 * 1024 * 1024;
        let fresh = compress_job_blocks(&mut mk(), &data, b.clone(), 4096, false);
        for a in [
            0..5 * 1024 * 1024usize,
            0..4096usize,
            1024 * 1024..3 * 1024 * 1024usize,
        ] {
            let mut st = mk();
            compress_job_blocks(&mut st, &data, a.clone(), 4096, false);
            reset(&mut st);
            let reused = compress_job_blocks(&mut st, &data, b.clone(), 4096, false);
            assert_eq!(reused, fresh, "job A {a:?}");
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
            enc.finish().unwrap();
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

/// Near-local vocabulary text (the shape the reach probe's shrunk domain
/// serves), sized above the probe's engagement floor.
#[cfg(test)]
fn near_local(len: usize) -> Vec<u8> {
    let words: [&[u8]; 13] = [
        b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ", b"lorem ",
        b"ipsum ", b"dolor ", b"sit ", b"amet ",
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

/// Json-shaped records (a small user pool over near-local structure): the
/// shrink class of the reach probe (see `encoding::reach_probe`) — its far
/// chain candidates displace nearer repcode reuse, so the shrunk reach
/// parses the head cheaper.
#[cfg(all(test, feature = "std"))]
fn jsonish(len: usize) -> Vec<u8> {
    use std::format;
    let mut state = 42u64;
    let mut rnd = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (state >> 33) as u32
    };
    let events = ["click", "view", "purchase", "error", "login"];
    let mut out = Vec::with_capacity(len);
    let mut id = 0u64;
    while out.len() < len {
        let user = rnd() % 5000;
        let payload = rnd() % 25;
        let score = rnd() % 1_000_000;
        let rec = format!(
            "{{\"id\":{},\"user\":\"user_{}\",\"event\":\"{}\",\"ts\":{},\"payload\":\"{}\",\"\
             score\":{}.{:06}}}\n",
            id,
            user,
            events[(rnd() % 5) as usize],
            1700000000 + id,
            "x".repeat(payload as usize),
            score / 1_000_000,
            score % 1_000_000
        );
        out.extend_from_slice(rec.as_bytes());
        id += 1;
    }
    out
}

/// The reach probe (see `encoding::reach_probe`): a pledged Balanced stream
/// above the engagement floor runs the whole staging path — probe parse,
/// reach decision, head replay — and must stay a valid, deterministic
/// frame whichever way the decision lands.
#[test]
fn reach_probe_stream_roundtrips() {
    let len = encoding::reach_probe::PROBE_MIN_FRAME as usize + 123 * 1024;
    let data = near_local(len);
    // The mt leg needs the std-gated worker paths.
    #[cfg(feature = "std")]
    let legs = [(1, "st"), (4, "mt")];
    #[cfg(not(feature = "std"))]
    let legs = [(1, "st")];
    for (workers, label) in legs {
        let options = || {
            EncoderOptions::new(Level::Balanced)
                .pledged_size(Some(len as u64))
                .workers(workers)
        };
        let encode = || {
            let mut sink = Vec::new();
            let mut enc = write::Encoder::with_options(&mut sink, options()).unwrap();
            // Odd chunk sizes cross the probe's staging window and the
            // block boundaries in every alignment.
            write_in_chunks(&data, 333 * 1024 + 7, &mut enc);
            enc.finish().unwrap();
            sink
        };
        let a = encode();
        let b = encode();
        assert_eq!(a, b, "{label}: stream must be deterministic");
        assert_eq!(bulk::decompress(&a, len).unwrap(), data, "{label}");
        #[cfg(feature = "std")]
        {
            let mut decoded = Vec::new();
            zstd::stream::copy_decode(a.as_slice(), &mut decoded).unwrap();
            assert_eq!(decoded, data, "{label}: libzstd must decode");
        }
    }
    // Unpledged and unflushed: the probe never engages (no staged head),
    // the stream still must roundtrip; an explicit flush mid-head cancels
    // the pending probe the same way.
    let mut sink = Vec::new();
    let mut enc =
        write::Encoder::with_options(&mut sink, EncoderOptions::new(Level::Balanced)).unwrap();
    write_in_chunks(&data, 128 * 1024, &mut enc);
    enc.finish().unwrap();
    assert_eq!(bulk::decompress(&sink, len).unwrap(), data);
    let mut sink = Vec::new();
    let mut enc = write::Encoder::with_options(
        &mut sink,
        EncoderOptions::new(Level::Balanced).pledged_size(Some(len as u64)),
    )
    .unwrap();
    enc.write_all(&data[..1024 * 1024]).unwrap();
    enc.flush().unwrap();
    write_in_chunks(&data[1024 * 1024..], 512 * 1024, &mut enc);
    enc.finish().unwrap();
    assert_eq!(bulk::decompress(&sink, len).unwrap(), data);
}
