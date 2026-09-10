//! Micro-benchmark of the matcher's `prefill_window` per strategy: time the
//! strip fill on window-sized corpus strips (random = worst-case seed scan,
//! text = seed found fast).

use crate::common::want;
use crate::corpus::{corpus_dir, LevelName, Shape};
use std::time::Instant;
use zstdx::encoding::{MatchGeneratorDriver, Matcher};

#[derive(clap::Args)]
pub struct Args {
    /// Corpus shapes to cover
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = [Shape::Random, Shape::Text, Shape::Json])]
    pub shape: Vec<Shape>,
    /// Strategies to time
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = [LevelName::Fastest, LevelName::Fast, LevelName::Balanced])]
    pub level: Vec<LevelName>,
}

pub fn run(args: &Args) {
    for shape in [Shape::Random, Shape::Text, Shape::Json] {
        if !want(&args.shape, &shape) {
            continue;
        }
        let name = shape.raw_name();
        let raw = std::fs::read(corpus_dir().join(format!("{name}.raw"))).unwrap();
        let strip_len = 1 << 20;
        let strip = &raw[raw.len() - strip_len - 8..raw.len() - 8];
        for level_name in &args.level {
            let level = level_name.pair().0;
            let mut m = MatchGeneratorDriver::new_direct();
            m.reset(level);
            let n = 200;
            // warmup
            m.prefill_window(strip, 0);
            let t = Instant::now();
            for _ in 0..n {
                m.prefill_window(strip, 0);
            }
            let dt = t.elapsed();
            println!(
                "{name:12} {level_name:>10?} strip {strip_len}: {:>8.1} us/prefill  ({:>6.0} MiB/s)",
                dt.as_secs_f64() * 1e6 / n as f64,
                strip_len as f64 * n as f64 / dt.as_secs_f64() / (1 << 20) as f64
            );
        }
    }
}
