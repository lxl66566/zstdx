//! Small-payload encode benchmark: ruzstd `compress_slice_to_vec` vs the
//! zstd crate's bulk path, per-call throughput including allocator effects.
//! Usage: cargo run --release --example bench_small [-- filter...]
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn bench(name: &str, data: &[u8], iters: usize) {
    // IMPL=ruzstd|zstd restricts the timed loop to one implementation.
    let impl_sel = std::env::var("IMPL").unwrap_or_default();
    // roundtrip check
    let comp = ruzstd::encoding::compress_slice_to_vec(
        data,
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    let mut fr = ruzstd::decoding::FrameDecoder::new();
    let mut back = Vec::with_capacity(data.len() + 16);
    fr.decode_all_to_vec(&comp, &mut back).unwrap();
    assert_eq!(&back[..], data);

    let mut el_ruzstd = f64::NAN;
    if impl_sel != "zstd" {
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(ruzstd::encoding::compress_slice_to_vec(
                data,
                ruzstd::encoding::CompressionLevel::Fastest,
            ));
        }
        el_ruzstd = t.elapsed().as_secs_f64() / iters as f64;
    }

    let zcomp = zstd::bulk::compress(data, 1).unwrap();
    let mut el_zstd = f64::NAN;
    if impl_sel != "ruzstd" {
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(zstd::bulk::compress(data, 1).unwrap());
        }
        el_zstd = t.elapsed().as_secs_f64() / iters as f64;
    }

    println!(
        "{name:<14} {:>7} B  ruzstd {:>8.0} MiB/s ratio {:>6.2} | zstd1 {:>8.0} MiB/s ratio {:>6.2}",
        data.len(),
        data.len() as f64 / (1024.0 * 1024.0) / el_ruzstd,
        data.len() as f64 / comp.len() as f64,
        data.len() as f64 / (1024.0 * 1024.0) / el_zstd,
        data.len() as f64 / zcomp.len() as f64,
    );
}

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");
    for shape in ["json", "text", "skewed", "random"] {
        if !filters.is_empty() && !filters.iter().any(|f| shape.contains(f.as_str())) {
            continue;
        }
        let raw = fs::read(dir.join(format!("{shape}.raw"))).unwrap();
        for size in [1024usize, 4096, 64 * 1024, 1024 * 1024] {
            // SIZE restricts the run to one payload size (in bytes) so
            // profilers attribute samples to a single code path.
            if let Ok(want) = std::env::var("SIZE") {
                if want.parse::<usize>().map(|w| w != size).unwrap_or(true) {
                    continue;
                }
            }
            // ITERS overrides the default cadence so profilers get a long run.
            let iters = std::env::var("ITERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| (64 * 1024 * 1024 / size).clamp(200, 20000));
            bench(&format!("{shape}-{}K", size / 1024), &raw[..size], iters);
        }
    }
}
