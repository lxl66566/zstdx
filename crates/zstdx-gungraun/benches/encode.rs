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
    ($group:ident, $zx:ident, $zc:ident, $lvl_zx:expr, $lvl_z:expr, [$($low:ident => $cap:ident),+]) => {
        #[library_benchmark]
        $(#[bench::$low(raw_slice(Shape::$cap))])+
        fn $zx(raw: Vec<u8>) -> usize {
            encode_zstdx(&raw, $lvl_zx)
        }

        #[library_benchmark]
        $(#[bench::$low(raw_slice(Shape::$cap))])+
        fn $zc(raw: Vec<u8>) -> usize {
            encode_zstd(&raw, $lvl_z)
        }

        library_benchmark_group!(name = $group, compare_by_id = true, benchmarks = [$zx, $zc]);
    };
}

encode_group!(fastest, zx_fastest, zc_fastest, Level::Fastest, 1, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros
]);
encode_group!(fast, zx_fast, zc_fast, Level::Fast, 3, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros
]);
encode_group!(balanced, zx_balanced, zc_balanced, Level::Balanced, 6, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros
]);
encode_group!(best, zx_best, zc_best, Level::Best, 12, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros
]);
// The dll shape joins the two parse-heavy tiers it is tracked against
// (the wall matrix's dll100 opt/ultra gap); the cheaper tiers keep the
// five-shape sweep.
encode_group!(opt, zx_opt, zc_opt, Level::Opt, 16, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros, dll => Dll
]);
encode_group!(ultra, zx_ultra, zc_ultra, Level::Ultra, 19, [
    json => Json, text => Text, skewed => Skewed, random => Random, zeros => Zeros, dll => Dll
]);

// Invoked at item position: the macro expands to the `fn main` itself. A
// `fn main() { main!(...) }` wrapper compiles but silently does nothing.
main!(library_benchmark_groups = [fastest, fast, balanced, best, opt, ultra]);
