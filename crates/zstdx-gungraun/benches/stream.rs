//! Deterministic streaming benchmarks: zstdx vs the zstd crate under
//! Callgrind.
//!
//! `decode` streams a libzstd-produced frame through the pull-based
//! streaming decoders with a 64 KiB sink; `encode_fastest`/`encode_balanced`
//! push the raw slice through the streaming encoders in 64 KiB writes into
//! an in-memory sink. The chunking crosses the internal block boundaries on
//! both sides equally.

// gungraun's macros emit `::`-rooted paths into the generated harness,
// which trips the workspace `unused_qualifications` lint at the expansion
// spans.
#![allow(unused_qualifications)]

use std::{
    hint::black_box,
    io::{Read as _, Write as _},
};

use gungraun::prelude::*;
use zstdx::Level;
use zstdx_gungraun::{DecodeCase, Shape, raw_slice, zstd_frame};

const CHUNK: usize = 64 * 1024;

fn stream_decode_zstdx(case: DecodeCase) -> usize {
    let compressed = case.compressed;
    let mut dec = zstdx::decoding::StreamingDecoder::new(&compressed[..]).unwrap();
    let mut sink = vec![0u8; CHUNK];
    let mut acc = 0usize;
    loop {
        let n = dec.read(&mut sink).unwrap();
        if n == 0 {
            break;
        }
        acc += n;
    }
    assert_eq!(acc, case.raw_len);
    black_box(acc)
}

fn stream_decode_zstd(compressed: &[u8], raw_len: usize) -> usize {
    let mut dec = zstd::stream::read::Decoder::new(compressed).unwrap();
    let mut sink = vec![0u8; CHUNK];
    let mut acc = 0usize;
    loop {
        let n = dec.read(&mut sink).unwrap();
        if n == 0 {
            break;
        }
        acc += n;
    }
    assert_eq!(acc, raw_len);
    black_box(acc)
}

#[library_benchmark]
#[bench::json(zstd_frame(Shape::Json, 3))]
#[bench::text(zstd_frame(Shape::Text, 3))]
#[bench::skewed(zstd_frame(Shape::Skewed, 3))]
#[bench::random(zstd_frame(Shape::Random, 3))]
#[bench::zeros(zstd_frame(Shape::Zeros, 3))]
fn zx_stream_decode(case: DecodeCase) -> usize {
    stream_decode_zstdx(case)
}

#[library_benchmark]
#[bench::json(zstd_frame(Shape::Json, 3))]
#[bench::text(zstd_frame(Shape::Text, 3))]
#[bench::skewed(zstd_frame(Shape::Skewed, 3))]
#[bench::random(zstd_frame(Shape::Random, 3))]
#[bench::zeros(zstd_frame(Shape::Zeros, 3))]
fn zc_stream_decode(case: DecodeCase) -> usize {
    let DecodeCase {
        compressed,
        raw_len,
        ..
    } = case;
    stream_decode_zstd(&compressed, raw_len)
}

library_benchmark_group!(
    name = stream_decode,
    compare_by_id = true,
    benchmarks = [zx_stream_decode, zc_stream_decode]
);

fn stream_encode_zstdx(raw: &[u8], level: Level) -> usize {
    let mut sink = Vec::with_capacity(raw.len() / 2 + CHUNK);
    {
        let mut enc = zstdx::stream::write::Encoder::new(&mut sink, level).unwrap();
        for chunk in raw.chunks(CHUNK) {
            enc.write_all(chunk).unwrap();
        }
        enc.finish().unwrap();
    }
    black_box(sink.len())
}

fn stream_encode_zstd(raw: &[u8], level: i32) -> usize {
    let mut sink = Vec::with_capacity(raw.len() / 2 + CHUNK);
    {
        let mut enc = zstd::stream::write::Encoder::new(&mut sink, level).unwrap();
        for chunk in raw.chunks(CHUNK) {
            enc.write_all(chunk).unwrap();
        }
        enc.finish().unwrap();
    }
    black_box(sink.len())
}

macro_rules! stream_encode_group {
    ($group:ident, $zx:ident, $zc:ident, $lvl_zx:expr, $lvl_z:expr) => {
        #[library_benchmark]
        #[bench::json(raw_slice(Shape::Json))]
        #[bench::text(raw_slice(Shape::Text))]
        #[bench::skewed(raw_slice(Shape::Skewed))]
        #[bench::random(raw_slice(Shape::Random))]
        #[bench::zeros(raw_slice(Shape::Zeros))]
        fn $zx(raw: Vec<u8>) -> usize {
            stream_encode_zstdx(&raw, $lvl_zx)
        }

        #[library_benchmark]
        #[bench::json(raw_slice(Shape::Json))]
        #[bench::text(raw_slice(Shape::Text))]
        #[bench::skewed(raw_slice(Shape::Skewed))]
        #[bench::random(raw_slice(Shape::Random))]
        #[bench::zeros(raw_slice(Shape::Zeros))]
        fn $zc(raw: Vec<u8>) -> usize {
            stream_encode_zstd(&raw, $lvl_z)
        }

        library_benchmark_group!(name = $group, compare_by_id = true, benchmarks = [$zx, $zc]);
    };
}

stream_encode_group!(
    stream_encode_fastest,
    zx_stream_encode_fastest,
    zc_stream_encode_fastest,
    Level::Fastest,
    1
);
stream_encode_group!(
    stream_encode_balanced,
    zx_stream_encode_balanced,
    zc_stream_encode_balanced,
    Level::Balanced,
    6
);

// Invoked at item position: the macro expands to the `fn main` itself. A
// `fn main() { main!(...) }` wrapper compiles but silently does nothing.
main!(library_benchmark_groups = [stream_decode, stream_encode_fastest, stream_encode_balanced]);
