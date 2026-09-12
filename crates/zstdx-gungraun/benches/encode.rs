//! Deterministic bulk-encode benchmarks: zstdx vs the zstd crate under
//! Callgrind.
//!
//! One group per ladder level; both implementations compress the same raw
//! slice with their one-shot bulk API, so context setup and output
//! allocation are counted on both sides equally.

// gungraun's macros emit `::`-rooted paths into the generated harness,
// which trips the workspace `unused_qualifications` lint at the expansion
// spans.
#![allow(unused_qualifications)]

use std::hint::black_box;

use gungraun::prelude::*;
use zstdx::{EncoderOptions, Level};
use zstdx_gungraun::{Shape, raw_slice};

fn encode_zstdx(raw: &[u8], level: Level) -> usize {
    let comp =
        zstdx::bulk::compress_with(raw, &EncoderOptions::new(level).checksum(false)).unwrap();
    black_box(comp.len())
}

fn encode_zstd(raw: &[u8], level: i32) -> usize {
    let comp = zstd::bulk::compress(raw, level).unwrap();
    black_box(comp.len())
}

macro_rules! encode_group {
    ($group:ident, $zx:ident, $zc:ident, $lvl_zx:expr, $lvl_z:expr) => {
        #[library_benchmark]
        #[bench::json(raw_slice(Shape::Json))]
        #[bench::text(raw_slice(Shape::Text))]
        #[bench::skewed(raw_slice(Shape::Skewed))]
        #[bench::random(raw_slice(Shape::Random))]
        #[bench::zeros(raw_slice(Shape::Zeros))]
        fn $zx(raw: Vec<u8>) -> usize {
            encode_zstdx(&raw, $lvl_zx)
        }

        #[library_benchmark]
        #[bench::json(raw_slice(Shape::Json))]
        #[bench::text(raw_slice(Shape::Text))]
        #[bench::skewed(raw_slice(Shape::Skewed))]
        #[bench::random(raw_slice(Shape::Random))]
        #[bench::zeros(raw_slice(Shape::Zeros))]
        fn $zc(raw: Vec<u8>) -> usize {
            encode_zstd(&raw, $lvl_z)
        }

        library_benchmark_group!(name = $group, compare_by_id = true, benchmarks = [$zx, $zc]);
    };
}

encode_group!(fastest, zx_fastest, zc_fastest, Level::Fastest, 1);
encode_group!(fast, zx_fast, zc_fast, Level::Fast, 3);
encode_group!(balanced, zx_balanced, zc_balanced, Level::Balanced, 6);
encode_group!(best, zx_best, zc_best, Level::Best, 12);
encode_group!(opt, zx_opt, zc_opt, Level::Opt, 16);
encode_group!(ultra, zx_ultra, zc_ultra, Level::Ultra, 19);

// Invoked at item position: the macro expands to the `fn main` itself. A
// `fn main() { main!(...) }` wrapper compiles but silently does nothing.
main!(library_benchmark_groups = [fastest, fast, balanced, best, opt, ultra]);
