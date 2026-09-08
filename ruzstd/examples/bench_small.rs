//! Small-payload encode benchmark: ruzstd `bulk::compress` vs the zstd
//! crate's bulk path, per-call throughput including allocator effects.
//!
//! Usage: cargo run --release --example bench_small [-- filter...]
//! Env: `IMPL=ruzstd|zstd` restricts to one implementation, `SIZE=<bytes>`
//! restricts to one payload size, `BENCH_BUDGET_MS` sets the budget.
//! Rounds batch ~4 MiB of calls so tiny payloads don't measure timer
//! overhead (see `examples/common`).

#[path = "common/mod.rs"]
mod common;

use common::{black_box, Ab};
use std::fs;
use std::path::PathBuf;

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).collect();
    let impl_sel = std::env::var("IMPL").unwrap_or_default();
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");

    let ab = Ab::default();
    println!(
        "small-payload encode (interleaved A/B, budget {:.0} ms/side)",
        ab.min_secs * 1000.0
    );
    println!(
        "{:<16}{:>9}{:>9}  {}",
        "shape", "ruz", "zstd1", "xslow  (MiB/s)"
    );
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
            let data = &raw[..size];
            // correctness gate
            let comp = ruzstd::bulk::compress(data, ruzstd::Level::Fastest);
            let mut back = Vec::with_capacity(data.len() + 16);
            ruzstd::decoding::FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            assert_eq!(&back[..], data);

            let batch = (4 * 1024 * 1024 / size).clamp(1, 100_000);
            let name = format!("{shape}-{}K", size / 1024);
            let bytes = (size * batch) as u64;
            if impl_sel == "zstd" {
                let stats = common::measure_solo(|| {
                    for _ in 0..batch {
                        black_box(zstd::bulk::compress(data, 1).unwrap());
                    }
                });
                println!("{name:<16}{:>9}{:>9.0}", "", stats.mibs(bytes));
            } else if impl_sel == "ruzstd" {
                let stats = common::measure_solo(|| {
                    for _ in 0..batch {
                        black_box(ruzstd::bulk::compress(data, ruzstd::Level::Fastest));
                    }
                });
                println!("{name:<16}{:>9.0}{:>9}", stats.mibs(bytes), "");
            } else {
                let report = ab.measure(
                    || {
                        for _ in 0..batch {
                            black_box(ruzstd::bulk::compress(data, ruzstd::Level::Fastest));
                        }
                    },
                    || {
                        for _ in 0..batch {
                            black_box(zstd::bulk::compress(data, 1).unwrap());
                        }
                    },
                );
                report.print(&name, bytes, "", "");
            }
        }
    }
}
