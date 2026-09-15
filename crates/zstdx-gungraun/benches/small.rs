//! Deterministic small-payload encode benchmarks: the 4-64 KiB band where
//! per-call fixed cost (allocator churn, per-block entropy builds) rivals
//! the per-byte scan cost, so wall-clock A/Bs drift with the machine.
//!
//! Each benchmark compresses the same small slice repeatedly through the
//! one-shot bulk API (256 calls: the per-thread state pool is warm after
//! the first, matching a steady-state small-payload caller), so the Ir
//! count divides into one shared setup plus 256 identical calls.

// gungraun's macros emit `::`-rooted paths into the generated harness,
// which trips the workspace `unused_qualifications` lint at the expansion
// spans.
#![allow(unused_qualifications)]

use std::hint::black_box;

use gungraun::prelude::*;
use zstdx::{EncoderOptions, Level};
use zstdx_gungraun::{Shape, raw_slice};

/// Calls per benchmark: enough that the first call's state-pool build stays
/// below ~1% of the total, few enough that a valgrind sweep stays fast.
const CALLS: usize = 256;

fn small_encode_zstdx(raw: &[u8], level: Level) -> usize {
    let opts = EncoderOptions::new(level).checksum(false);
    let mut len = 0;
    for _ in 0..CALLS {
        len = black_box(zstdx::bulk::compress_with(raw, &opts).unwrap().len());
    }
    len
}

fn small_encode_zstd(raw: &[u8], level: i32) -> usize {
    let mut len = 0;
    for _ in 0..CALLS {
        len = black_box(zstd::bulk::compress(raw, level).unwrap().len());
    }
    len
}

/// Slice a payload of `len` bytes out of the corpus shape.
fn sized(shape: Shape, len: usize) -> Vec<u8> {
    raw_slice(shape)[..len].to_vec()
}

macro_rules! small_pair {
    ($zx:ident, $zc:ident, $id:ident, $shape:expr, $len:expr, $lvl_zx:expr, $lvl_z:expr) => {
        #[library_benchmark]
        #[bench::$id(sized($shape, $len))]
        fn $zx(raw: Vec<u8>) -> usize {
            small_encode_zstdx(&raw, $lvl_zx)
        }

        #[library_benchmark]
        #[bench::$id(sized($shape, $len))]
        fn $zc(raw: Vec<u8>) -> usize {
            small_encode_zstd(&raw, $lvl_z)
        }
    };
}

small_pair!(
    zx_json4k,
    zc_json4k,
    json_4k,
    Shape::Json,
    4096,
    Level::Fastest,
    1
);
small_pair!(
    zx_json16k,
    zc_json16k,
    json_16k,
    Shape::Json,
    16384,
    Level::Fastest,
    1
);
small_pair!(
    zx_json64k,
    zc_json64k,
    json_64k,
    Shape::Json,
    65536,
    Level::Fastest,
    1
);
small_pair!(
    zx_text4k,
    zc_text4k,
    text_4k,
    Shape::Text,
    4096,
    Level::Fastest,
    1
);
small_pair!(
    zx_text16k,
    zc_text16k,
    text_16k,
    Shape::Text,
    16384,
    Level::Fastest,
    1
);
small_pair!(
    zx_text64k,
    zc_text64k,
    text_64k,
    Shape::Text,
    65536,
    Level::Fastest,
    1
);
small_pair!(
    zx_rand4k,
    zc_rand4k,
    random_4k,
    Shape::Random,
    4096,
    Level::Fastest,
    1
);
small_pair!(
    zx_rand8k,
    zc_rand8k,
    random_8k,
    Shape::Random,
    8192,
    Level::Fastest,
    1
);
small_pair!(
    zx_rand16k,
    zc_rand16k,
    random_16k,
    Shape::Random,
    16384,
    Level::Fastest,
    1
);

library_benchmark_group!(
    name = small_fastest,
    compare_by_id = true,
    benchmarks = [
        zx_json4k, zc_json4k, zx_json16k, zc_json16k, zx_json64k, zc_json64k, zx_text4k, zc_text4k,
        zx_text16k, zc_text16k, zx_text64k, zc_text64k, zx_rand4k, zc_rand4k, zx_rand8k, zc_rand8k,
        zx_rand16k, zc_rand16k,
    ]
);

// Invoked at item position: the macro expands to the `fn main` itself. A
// `fn main() { main!(...) }` wrapper compiles but silently does nothing.
main!(library_benchmark_groups = [small_fastest]);
