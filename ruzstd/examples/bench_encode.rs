//! Encode-only benchmark with phase timing: matcher vs entropy coding.
//! Usage: cargo run --release --example bench_encode

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".raw") {
            continue;
        }
        let raw = fs::read(&path).unwrap();
        let iters = 3;
        let comp = ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
        // roundtrip check
        let mut fr = ruzstd::decoding::FrameDecoder::new();
        let mut back = Vec::with_capacity(raw.len() + 16);
        fr.decode_all_to_vec(&comp, &mut back).unwrap();
        assert_eq!(&back[..], &raw[..]);
        let t = Instant::now();
        for _ in 0..iters {
            std::hint::black_box(ruzstd::encoding::compress_to_vec(
                &raw[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            ));
        }
        let el = t.elapsed().as_secs_f64() / iters as f64;
        println!(
            "{name:<12} {:.0} MiB/s  ratio {:.2}",
            raw.len() as f64 / (1024.0 * 1024.0) / el,
            raw.len() as f64 / comp.len() as f64
        );
    }
}
