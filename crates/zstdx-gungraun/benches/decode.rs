//! Deterministic bulk-decode benchmarks: zstdx vs the zstd crate under
//! Callgrind.
//!
//! Each group decodes the same frame bytes through both implementations
//! (`compare_by_id` prints the instruction-count delta per corpus shape).
//! Two frame families: `l1`/`l3`/`l9` decode libzstd-produced frames (the
//! interop case), `own_fastest`/`own_balanced`/`own_ultra` decode
//! zstdx-produced frames. Output goes into a caller-provided buffer on both
//! sides, so the measured region only differs in decoder work.

// gungraun's macros emit `::`-rooted paths into the generated harness,
// which trips the workspace `unused_qualifications` lint at the expansion
// spans.
#![allow(unused_qualifications)]

use std::hint::black_box;

use gungraun::prelude::*;
use zstdx::decoding::FrameDecoder;
use zstdx_gungraun::{DecodeCase, Shape, zstd_frame, zstdx_frame};

fn decode_zstdx(case: DecodeCase) -> usize {
    let DecodeCase {
        compressed,
        mut out,
        raw_len,
    } = case;
    let mut dec = FrameDecoder::new();
    let n = dec.decode_all(&compressed, &mut out).unwrap();
    assert_eq!(n, raw_len);
    black_box(n)
}

fn decode_zstd(case: DecodeCase) -> usize {
    let DecodeCase {
        compressed,
        mut out,
        ..
    } = case;
    let n = zstd::bulk::decompress_to_buffer(&compressed, &mut out).unwrap();
    black_box(n)
}

macro_rules! decode_group {
    ($group:ident, $zx:ident, $zc:ident, $frame:ident, $lvl:expr) => {
        #[library_benchmark]
        #[bench::json($frame(Shape::Json, $lvl))]
        #[bench::text($frame(Shape::Text, $lvl))]
        #[bench::skewed($frame(Shape::Skewed, $lvl))]
        #[bench::random($frame(Shape::Random, $lvl))]
        #[bench::zeros($frame(Shape::Zeros, $lvl))]
        fn $zx(case: DecodeCase) -> usize {
            decode_zstdx(case)
        }

        #[library_benchmark]
        #[bench::json($frame(Shape::Json, $lvl))]
        #[bench::text($frame(Shape::Text, $lvl))]
        #[bench::skewed($frame(Shape::Skewed, $lvl))]
        #[bench::random($frame(Shape::Random, $lvl))]
        #[bench::zeros($frame(Shape::Zeros, $lvl))]
        fn $zc(case: DecodeCase) -> usize {
            decode_zstd(case)
        }

        library_benchmark_group!(name = $group, compare_by_id = true, benchmarks = [$zx, $zc]);
    };
}

decode_group!(l1, zx_l1, zc_l1, zstd_frame, 1);
decode_group!(l3, zx_l3, zc_l3, zstd_frame, 3);
decode_group!(l9, zx_l9, zc_l9, zstd_frame, 9);
decode_group!(
    own_fastest,
    zx_own_fastest,
    zc_own_fastest,
    zstdx_frame,
    zstdx::Level::Fastest
);
decode_group!(
    own_balanced,
    zx_own_balanced,
    zc_own_balanced,
    zstdx_frame,
    zstdx::Level::Balanced
);
decode_group!(
    own_ultra,
    zx_own_ultra,
    zc_own_ultra,
    zstdx_frame,
    zstdx::Level::Ultra
);

// Invoked at item position: the macro expands to the `fn main` itself. A
// `fn main() { main!(...) }` wrapper compiles but silently does nothing.
main!(library_benchmark_groups = [l1, l3, l9, own_fastest, own_balanced, own_ultra]);
