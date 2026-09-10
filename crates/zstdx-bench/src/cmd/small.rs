//! Small-payload encode benchmark: zstdx `bulk::compress` vs the zstd
//! crate's bulk path, per-call throughput including allocator effects.
//!
//! Rounds batch ~4 MiB of calls so tiny payloads don't measure timer
//! overhead (see `common`). `--impl` restricts to one implementation and
//! `--size` to one payload size so profilers attribute samples to a single
//! code path.

use crate::{
    common::{Ab, apply_budget, black_box, measure_solo, want},
    corpus::{Shape, load_raw},
};

/// Zeros is skipped: RLE payloads say nothing about the small-call paths.
const SMALL_SHAPES: [Shape; 4] = [Shape::Json, Shape::Text, Shape::Skewed, Shape::Random];

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Impl {
    Zstdx,
    Zstd,
}

#[derive(clap::Args)]
pub struct Args {
    /// Corpus shapes to include (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    pub shape: Vec<Shape>,
    /// Payload sizes in bytes (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub size: Vec<usize>,
    /// Restrict to one implementation
    #[arg(long, value_enum)]
    pub r#impl: Option<Impl>,
    /// Per-side measurement budget in milliseconds
    #[arg(long)]
    pub budget_ms: Option<f64>,
}

pub fn run(args: &Args) {
    apply_budget(args.budget_ms);
    let sizes: Vec<usize> = if args.size.is_empty() {
        vec![1024, 4096, 64 * 1024, 1024 * 1024]
    } else {
        args.size.clone()
    };
    let ab = Ab::default();
    println!(
        "small-payload encode (interleaved A/B, budget {:.0} ms/side)",
        ab.min_secs * 1000.0
    );
    println!("{:<16}{:>9}{:>9}  xslow  (MiB/s)", "shape", "ruz", "zstd1");
    for shape in SMALL_SHAPES
        .iter()
        .copied()
        .filter(|s| want(&args.shape, s))
    {
        let raw = load_raw(shape);
        for size in &sizes {
            let data = &raw[..(*size).min(raw.len())];
            // correctness gate
            let comp = zstdx::bulk::compress(data, zstdx::Level::Fastest);
            let mut back = Vec::with_capacity(data.len() + 16);
            zstdx::decoding::FrameDecoder::new()
                .decode_all_to_vec(&comp, &mut back)
                .unwrap();
            assert_eq!(&back[..], data);

            let batch = (4 * 1024 * 1024 / data.len()).clamp(1, 100_000);
            let name = format!("{}-{}K", shape.raw_name(), data.len() / 1024);
            let bytes = (data.len() * batch) as u64;
            match args.r#impl {
                Some(Impl::Zstd) => {
                    let stats = measure_solo(|| {
                        for _ in 0..batch {
                            black_box(zstd::bulk::compress(data, 1).unwrap());
                        }
                    });
                    println!("{name:<16}{:>9}{:>9.0}", "", stats.mibs(bytes));
                },
                Some(Impl::Zstdx) => {
                    let stats = measure_solo(|| {
                        for _ in 0..batch {
                            black_box(zstdx::bulk::compress(data, zstdx::Level::Fastest));
                        }
                    });
                    println!("{name:<16}{:>9.0}{:>9}", stats.mibs(bytes), "");
                },
                None => {
                    let report = ab.measure(
                        || {
                            for _ in 0..batch {
                                black_box(zstdx::bulk::compress(data, zstdx::Level::Fastest));
                            }
                        },
                        || {
                            for _ in 0..batch {
                                black_box(zstd::bulk::compress(data, 1).unwrap());
                            }
                        },
                    );
                    report.print(&name, bytes);
                },
            }
        }
    }
}
