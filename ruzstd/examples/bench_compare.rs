//! Head-to-head benchmark: ruzstd vs the zstd crate (libzstd bindings).
//!
//! Usage: cargo run --release --example bench_compare [--] [filter]
//! `filter` selects shapes by substring (e.g. `text` or `text.zst3`).
//!
//! Measures, per corpus entry:
//! - decode: ruzstd decode_all (slice API), ruzstd StreamingDecoder (64KiB reads),
//!   zstd::bulk::decode_all, zstd::stream (64KiB reads)
//! - encode: ruzstd CompressionLevel::Fastest, zstd level 1 and 3
//! All decode outputs are verified against the raw file on the first pass.

use ruzstd::decoding::{FrameDecoder, StreamingDecoder};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[inline(never)]
fn black_box<T>(v: T) -> T {
    std::hint::black_box(v)
}

const WARMUP: usize = 1;
const ITERS: usize = 5;

fn bench<F: FnMut()>(iters: usize, mut f: F) -> f64 {
    for _ in 0..WARMUP {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed().as_secs_f64() / iters as f64
}

fn mibs(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / secs
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");

    let mut entries: Vec<(String, Vec<u8>, Vec<u8>)> = Vec::new();
    for entry in fs::read_dir(&dir).expect("bench/corpus not found; run bench/gen_corpus.sh") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".zst1") && !name.ends_with(".zst3") && !name.ends_with(".zst9") {
            continue;
        }
        if !name.contains(&filter) {
            continue;
        }
        let raw_path = path.with_file_name(name.split('.').next().unwrap().to_owned() + ".raw");
        let compressed = fs::read(&path).unwrap();
        let raw = fs::read(&raw_path).unwrap();
        entries.push((name, compressed, raw));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!entries.is_empty(), "no corpus entries matched filter {filter:?}");

    let iters = ITERS;

    println!("== decode: ruzstd vs zstd crate ({} iters, warmup {WARMUP}) ==\n", iters);
    println!(
        "{:<14}{:>9}{:>9}{:>9}{:>9}{:>9}",
        "file", "ruz-slice", "ruz-strm", "zstd-slice", "zstd-strm", "MiB/s of"
    );
    for (name, compressed, raw) in &entries {
        // --- ruzstd slice decode, exact-size buffer
        let mut fr = FrameDecoder::new();
        let mut out = vec![0u8; raw.len()];
        fr.decode_all(compressed, &mut out).expect("ruzstd decode");
        assert_eq!(&out[..], &raw[..], "ruzstd decoded wrong bytes for {name}");

        let t = bench(iters, || {
            fr.decode_all(compressed, &mut out).unwrap();
        });
        let ruz_slice = mibs(raw.len() as u64, t);

        // --- ruzstd streaming decode (64KiB chunks into reused Vec)
        let mut sink = vec![0u8; 64 * 1024];
        let t = bench(iters, || {
            let mut dec = StreamingDecoder::new(&compressed[..]).unwrap();
            let mut total = 0usize;
            loop {
                let n = dec.read(&mut sink).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            assert_eq!(total, raw.len());
        });
        let ruz_strm = mibs(raw.len() as u64, t);

        // --- zstd crate bulk decode (ZSTD_decompress, exact size known)
        let t = bench(iters, || {
            black_box(zstd::bulk::decompress(&compressed[..], raw.len()).unwrap());
        });
        let z_slice = mibs(raw.len() as u64, t);

        // --- zstd crate streaming decode (64KiB chunks, single frame stream decoder reused)
        let t = bench(iters, || {
            let mut dec = zstd::stream::read::Decoder::new(&compressed[..]).unwrap();
            let mut total = 0usize;
            loop {
                let n = dec.read(&mut sink).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            assert_eq!(total, raw.len());
        });
        let z_strm = mibs(raw.len() as u64, t);

        println!(
            "{:<14}{:>9.0}{:>9.0}{:>9.0}{:>9.0}{:>9.0}",
            name, ruz_slice, ruz_strm, z_slice, z_strm, raw.len() as f64 / (1024.0 * 1024.0)
        );
    }

    if filter.is_empty() || Path::new(&filter).exists() {
        // encode bench only on raw files
        let mut shapes: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if name.ends_with(".raw") {
                shapes.push((name, fs::read(&path).unwrap()));
            }
        }
        shapes.sort_by(|a, b| a.0.cmp(&b.0));
        println!("\n== encode: ruzstd Fastest vs zstd crate ({} iters) ==\n", iters);
        println!(
            "{:<14}{:>9}{:>12}{:>9}{:>12}{:>9}{:>12}",
            "shape", "ruz MB/s", "ratio", "z1 MB/s", "ratio", "z3 MB/s", "ratio"
        );
        for (name, raw) in &shapes {
            // ruzstd Fastest
            let mut comp = ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
            let t = bench(iters, || {
                comp = ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
            });
            let ruz_enc = mibs(raw.len() as u64, t);
            let ruz_ratio = raw.len() as f64 / comp.len() as f64;
            // roundtrip sanity via libzstd
            let mut fr = FrameDecoder::new();
            let mut back = Vec::with_capacity(raw.len() + 16);
            fr.decode_all_to_vec(&comp, &mut back).unwrap();
            assert_eq!(&back[..], &raw[..], "ruzstd roundtrip mismatch for {name}");
            zstd::stream::copy_decode(&comp[..], &mut Vec::new()).unwrap();

            // zstd level 1
            let t = bench(iters, || {
                black_box(zstd::bulk::compress(&raw[..], 1).unwrap());
            });
            let z1 = mibs(raw.len() as u64, t);
            let z1_ratio =
                raw.len() as f64 / zstd::bulk::compress(&raw[..], 1).unwrap().len() as f64;

            // zstd level 3
            let t = bench(iters, || {
                black_box(zstd::bulk::compress(&raw[..], 3).unwrap());
            });
            let z3 = mibs(raw.len() as u64, t);
            let z3_ratio =
                raw.len() as f64 / zstd::bulk::compress(&raw[..], 3).unwrap().len() as f64;

            println!(
                "{:<14}{:>9.0}{:>12.2}{:>9.0}{:>12.2}{:>9.0}{:>12.2}",
                name, ruz_enc, ruz_ratio, z1, z1_ratio, z3, z3_ratio
            );
        }
    }
}
