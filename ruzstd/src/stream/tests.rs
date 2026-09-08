//! Tests for the streaming encoders: byte-equality with the one-shot paths,
//! option plumbing, drop behavior and interop with the reference decoder.

use crate::encoding;
use crate::io::Write as _;
use crate::stream::{read, write};
use crate::{bulk, EncoderOptions, Level};
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
fn shapes() -> Vec<Vec<u8>> {
    let mut pseudo_random = 0x9E37_79B9_7F4A_7C15u64;
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
        (0..128 * 1024).map(|_| (rand() & 0xFF) as u8).collect(),
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
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let data: Vec<u8> = (0..10 * 1024).map(|_| (rand() & 0xFF) as u8).collect();
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
fn workers_unsupported_until_implemented() {
    let err = match write::Encoder::with_options(
        Vec::new(),
        EncoderOptions::new(Level::Fastest).workers(2),
    ) {
        Ok(_) => panic!("workers > 1 must be rejected until implemented"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        crate::Error::Unsupported {
            feature: crate::Feature::Multithread
        }
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
