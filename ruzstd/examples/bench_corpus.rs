//! End-to-end benchmark: decode every decodecorpus_files/*.zst in memory,
//! verify against the uncompressed counterpart, then time full-corpus passes.
//!
//! Usage: cargo run --release --example bench_corpus [iterations]

use ruzstd::decoding::FrameDecoder;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("decodecorpus_files");

    let mut entries: Vec<(PathBuf, Vec<u8>, Option<Vec<u8>>)> = Vec::new();
    for entry in fs::read_dir(&dir).expect("read_dir decodecorpus_files") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".zst") {
            continue;
        }
        let compressed = fs::read(&path).unwrap();
        let raw = fs::read(path.with_file_name(&name[..name.len() - 4])).ok();
        entries.push((path, compressed, raw));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!entries.is_empty(), "no .zst files found");

    let mut fr = FrameDecoder::new();
    let mut out = Vec::new();

    // Warm-up + correctness pass
    let mut total_raw = 0u64;
    let mut total_comp = 0u64;
    for (path, compressed, raw) in &entries {
        out.clear();
        out.shrink_to_fit();
        out.reserve(raw.as_ref().map_or(1, |r| r.len() + 1));
        fr.decode_all_to_vec(compressed, &mut out)
            .unwrap_or_else(|e| panic!("decode {:?} failed: {e}", path));
        if let Some(raw) = raw {
            assert_eq!(&out[..], &raw[..], "decoded output mismatch for {:?}", path);
        }
        total_raw += out.len() as u64;
        total_comp += compressed.len() as u64;
    }
    println!(
        "corpus: {} files, {:.2} MiB raw, {:.2} MiB compressed (verified OK)",
        entries.len(),
        total_raw as f64 / (1024.0 * 1024.0),
        total_comp as f64 / (1024.0 * 1024.0),
    );

    for i in 0..iterations {
        let start = Instant::now();
        let mut bytes = 0u64;
        for (path, compressed, raw) in &entries {
            out.clear();
            out.shrink_to_fit();
            out.reserve(raw.as_ref().map_or(1, |r| r.len() + 1));
            fr.decode_all_to_vec(compressed, &mut out)
                .unwrap_or_else(|e| panic!("decode {:?} failed: {e}", path));
            bytes += out.len() as u64;
        }
        let elapsed = start.elapsed();
        println!(
            "iter {}: {:.4} s, {:.1} MiB/s",
            i,
            elapsed.as_secs_f64(),
            bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
        );
    }
}
