//! Encode-only benchmark of the zstdx fast path over the corpus, with the
//! zstd crate's level 1 as the interleaved reference.
//!
//! Usage: cargo run --release --example bench_encode [-- filter...]
//! `BENCH_BUDGET_MS` sets the per-side budget (default 500).

#[path = "common/mod.rs"]
mod common;

use common::{black_box, Ab};
use std::fs;
use std::path::PathBuf;

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");

    let ab = Ab::default();
    println!(
        "encode (interleaved A/B vs zstd -1, budget {:.0} ms/side)",
        ab.min_secs * 1000.0
    );
    println!(
        "{:<16}{:>9}{:>9}  {}",
        "shape", "ruz", "zstd1", "xslow  (MiB/s; ruz ratio)"
    );
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".raw") {
            continue;
        }
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let raw = fs::read(&path).unwrap();

        // correctness gate
        let comp = zstdx::bulk::compress(&raw, zstdx::Level::Fastest);
        let mut back = Vec::with_capacity(raw.len() + 16);
        zstdx::decoding::FrameDecoder::new()
            .decode_all_to_vec(&comp, &mut back)
            .unwrap();
        assert_eq!(&back[..], &raw[..]);
        zstd::stream::copy_decode(&comp[..], &mut Vec::new()).unwrap();
        let ratio = raw.len() as f64 / comp.len() as f64;

        let report = ab.measure(
            || {
                black_box(zstdx::bulk::compress(&raw, zstdx::Level::Fastest));
            },
            || {
                black_box(zstd::bulk::compress(&raw, 1).unwrap());
            },
        );
        report.print(&name, raw.len() as u64, "", "");
        println!("{:<16}zstdx ratio {ratio:.2}", "");
    }
}
