//! Benchmark decoding of explicit .zst files: bench_files <iterations> <file.zst>...
//!
//! Each file is decoded once for reference + optional verification against the
//! raw counterpart (same path minus .zst), then timed over `iterations` in-place
//! decodes into an exact-size buffer. Prints per-file throughput.

use ruzstd::decoding::FrameDecoder;
use std::fs;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        panic!("usage: bench_files <iterations> <file.zst>...");
    }
    let iterations: usize = args[1].parse().unwrap();

    let mut fr = FrameDecoder::new();
    for path in &args[2..] {
        let compressed = fs::read(path).unwrap();

        let mut reference = Vec::new();
        // decode_all_to_vec never grows the vector; retry with geometric growth
        let mut step = 1usize << 20;
        loop {
            match fr.decode_all_to_vec(&compressed, &mut reference) {
                Ok(()) => break,
                Err(ruzstd::decoding::errors::FrameDecoderError::TargetTooSmall) => {
                    step *= 2;
                    reference.reserve(step);
                }
                Err(e) => panic!("decode {} failed: {:?}", path, e),
            }
        }

        let raw_path = match path.rfind('.') {
            Some(i) => &path[..i],
            None => path.as_str(),
        };
        if let Ok(raw) = fs::read(raw_path) {
            assert_eq!(raw, reference, "decoded output mismatch for {}", path);
        }

        let mut out = vec![0u8; reference.len()];
        let mut raw_bytes = 0u64;
        let start = Instant::now();
        for _ in 0..iterations {
            fr.decode_all(&compressed, &mut out).unwrap();
            raw_bytes += out.len() as u64;
        }
        let elapsed = start.elapsed();

        assert_eq!(
            &out[..],
            &reference[..],
            "in-place decode mismatch for {}",
            path
        );
        println!(
            "{path}: {:.2} MiB raw, {:.1} MiB/s ({} iters, {:.4} s/iter)",
            reference.len() as f64 / (1024.0 * 1024.0),
            raw_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
            iterations,
            elapsed.as_secs_f64() / iterations as f64,
        );
    }
}
