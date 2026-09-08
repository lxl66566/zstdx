//! Head-to-head benchmark: ruzstd vs the zstd crate (libzstd bindings).
//!
//! Usage: cargo run --release --example bench_compare [--] [filter]
//! `filter` selects shapes by substring (e.g. `text` or `text.zst3`);
//! `BENCH_BUDGET_MS` sets the per-side measurement budget (default 500).
//!
//! Every cell is an interleaved A/B measurement (see `examples/common`):
//! both sides alternate round by round so machine drift cancels, and the
//! reported ratio is the per-round median. Decode cells compare the slice
//! and streaming paths; encode cells compare our Fastest level against
//! zstd levels 1 and 3. All outputs are verified against the raw file
//! before anything is timed.

#[path = "common/mod.rs"]
mod common;

use common::Ab;
use ruzstd::decoding::{FrameDecoder, StreamingDecoder};
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;

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
    assert!(
        !entries.is_empty(),
        "no corpus entries matched filter {filter:?}"
    );

    let ab = Ab::default();
    println!(
        "== decode (interleaved A/B, budget {:.0} ms/side) ==",
        ab.min_secs * 1000.0
    );
    println!(
        "{:<16}{:>9}{:>9}   {}",
        "file", "ruz", "zstd", "xslow  (MiB/s of each; ratio = ruz_time/zstd_time)"
    );
    for (name, compressed, raw) in &entries {
        // correctness gate before anything is timed
        let mut fr = FrameDecoder::new();
        let mut out = vec![0u8; raw.len()];
        fr.decode_all(compressed, &mut out).expect("ruzstd decode");
        assert_eq!(&out[..], &raw[..], "ruzstd decoded wrong bytes for {name}");
        let mut decoded = Vec::new();
        zstd::stream::copy_decode(compressed.as_slice(), &mut decoded).unwrap();
        assert_eq!(
            &decoded[..],
            &raw[..],
            "zstd decoded wrong bytes for {name}"
        );

        let bytes = raw.len() as u64;
        // slice path
        let mut fr = FrameDecoder::new();
        let mut out = vec![0u8; raw.len()];
        let report = ab.measure(
            || {
                fr.decode_all(compressed, &mut out).unwrap();
                common::black_box(&out);
            },
            || {
                common::black_box(zstd::bulk::decompress(compressed, raw.len()).unwrap());
            },
        );
        report.print(&format!("{name}.sl"), bytes, "", "");

        // streaming path (64 KiB reads)
        let report = ab.measure(
            || {
                let mut dec = StreamingDecoder::new(&compressed[..]).unwrap();
                let mut sink = vec![0u8; 64 * 1024];
                let mut total = 0usize;
                loop {
                    let n = dec.read(&mut sink).unwrap();
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                assert_eq!(total, raw.len());
            },
            || {
                let mut dec = zstd::stream::read::Decoder::new(&compressed[..]).unwrap();
                let mut sink = vec![0u8; 64 * 1024];
                let mut total = 0usize;
                loop {
                    let n = dec.read(&mut sink).unwrap();
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                assert_eq!(total, raw.len());
            },
        );
        report.print(&format!("{name}.st"), bytes, "", "");
    }

    if filter.is_empty() {
        let mut shapes: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if name.ends_with(".raw") {
                shapes.push((name, fs::read(&path).unwrap()));
            }
        }
        shapes.sort_by(|a, b| a.0.cmp(&b.0));
        println!("\n== encode (interleaved A/B; ruz = Fastest) ==");
        println!(
            "{:<16}{:>9}{:>9}   {}",
            "shape", "ruz", "zstd", "xslow  (MiB/s of each; ratio = ruz_time/zstd_time)"
        );
        for (name, raw) in &shapes {
            // correctness gate: our frame decodes with both sides
            let comp = ruzstd::bulk::compress(raw, ruzstd::Level::Fastest);
            let mut back = Vec::with_capacity(raw.len() + 16);
            FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            assert_eq!(&back[..], &raw[..], "ruzstd roundtrip mismatch for {name}");
            zstd::stream::copy_decode(&comp[..], &mut Vec::new()).unwrap();
            let ratio = raw.len() as f64 / comp.len() as f64;

            let bytes = raw.len() as u64;
            let report = ab.measure(
                || {
                    common::black_box(ruzstd::bulk::compress(raw, ruzstd::Level::Fastest));
                },
                || {
                    common::black_box(zstd::bulk::compress(raw, 1).unwrap());
                },
            );
            report.print(&format!("{name}.z1"), bytes, "", "");
            let report = ab.measure(
                || {
                    common::black_box(ruzstd::bulk::compress(raw, ruzstd::Level::Fastest));
                },
                || {
                    common::black_box(zstd::bulk::compress(raw, 3).unwrap());
                },
            );
            report.print(&format!("{name}.z3"), bytes, "", "");
            println!("{:<16}ruzstd ratio {ratio:.2}", "");
        }
    }
}
