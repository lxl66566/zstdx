//! Dictionary multithreading wall A/B (R48): the dictionary's ST slice
//! entry against the mt job path (job zero seeded from the dictionary),
//! plus plain-frame sanity cells — the no-dict mt walls must not move
//! between builds (the byte gate is `dump`; this is the wall gate).
//!
//! One raw input and one formatted dictionary; every cell roundtrips
//! through our decoder with the dictionary before anything is timed.

use std::{fs, path::PathBuf};

use clap::Parser;

use crate::common::{Ab, black_box};

#[derive(Parser)]
pub struct Args {
    /// Raw input file
    pub file: PathBuf,
    /// Formatted dictionary file (`zstdx-bench train --formatted`)
    #[arg(long)]
    pub dict: PathBuf,
    /// Ladder levels (comma-separated; tier names or numeric 1-22)
    #[arg(long, default_value = "fastest,fast,balanced,best,opt,ultra")]
    pub level: String,
    /// Worker counts for the mt cells (comma-separated)
    #[arg(long, default_value = "4,8")]
    pub workers: String,
    /// Per-side time budget in ms
    #[arg(long)]
    pub budget_ms: Option<f64>,
    /// Skip the plain-frame sanity cells (dict-only run)
    #[arg(long)]
    pub no_sanity: bool,
}

fn parse_levels(spec: &str) -> Vec<zstdx::Level> {
    spec.split(',')
        .map(|s| {
            let s = s.trim();
            match s {
                "fastest" => zstdx::Level::Fastest,
                "fast" => zstdx::Level::Fast,
                "balanced" => zstdx::Level::Balanced,
                "best" => zstdx::Level::Best,
                "opt" => zstdx::Level::Opt,
                "ultra" => zstdx::Level::Ultra,
                n => zstdx::Level::from_zstd(n.parse::<i32>().expect("numeric level")),
            }
        })
        .collect()
}

pub fn run(args: &Args) {
    let data = fs::read(&args.file).expect("raw input readable");
    let dict = fs::read(&args.dict).expect("dictionary readable");
    crate::common::apply_budget(args.budget_ms);
    let levels = parse_levels(&args.level);
    let workers: Vec<u32> = args
        .workers
        .split(',')
        .map(|w| w.trim().parse().expect("worker count"))
        .collect();
    let roundtrip = |frame: &[u8]| {
        let back = zstdx::bulk::decompress_with(
            frame,
            data.len(),
            &zstdx::DecoderOptions::new().dictionary(&dict),
        )
        .expect("dict frame roundtrips");
        assert_eq!(back, data);
    };
    println!(
        "input {} bytes, dict {} bytes, budget {:.0} ms/side",
        data.len(),
        dict.len(),
        crate::common::budget_secs() * 1000.0,
    );
    for level in levels {
        let opts = |workers: u32| {
            zstdx::EncoderOptions::new(level)
                .checksum(false)
                .pledged_size(Some(data.len() as u64))
                .workers(workers)
                .dictionary(&dict)
        };
        let plain = |workers: u32| {
            zstdx::EncoderOptions::new(level)
                .checksum(false)
                .pledged_size(Some(data.len() as u64))
                .workers(workers)
        };
        // Sizes first (deterministic): dict st, dict mt per count, and the
        // plain-frame references for the size delta the job split costs.
        let dict_st = zstdx::bulk::compress_with(&data, &opts(1)).unwrap();
        roundtrip(&dict_st);
        println!("level {level:?}");
        println!("  size dict-st {:>10}", dict_st.len());
        for &w in &workers {
            let dict_mt = zstdx::bulk::compress_with(&data, &opts(w)).unwrap();
            roundtrip(&dict_mt);
            println!(
                "  size dict-mt{w:<2}{:>10}  delta vs st {:+7} ({:+.3}%)",
                dict_mt.len(),
                dict_mt.len() as i64 - dict_st.len() as i64,
                (dict_mt.len() as f64 / dict_st.len() as f64 - 1.0) * 100.0
            );
        }
        if !args.no_sanity {
            let plain_st = zstdx::bulk::compress_with(&data, &plain(1)).unwrap();
            let plain_mt: Vec<(u32, usize)> = workers
                .iter()
                .map(|&w| {
                    let f = zstdx::bulk::compress_with(&data, &plain(w)).unwrap();
                    (w, f.len())
                })
                .collect();
            println!("  size plain-st {:>10}", plain_st.len());
            for (w, n) in plain_mt {
                println!("  size plain-mt{w:<2}{n:>10}");
            }
            let ab = Ab::default();
            let mut a_frame = Vec::new();
            let mut b_frame = Vec::new();
            let report = ab.measure(
                || {
                    a_frame = zstdx::bulk::compress_with(&data, &plain(1)).unwrap();
                    black_box(&a_frame);
                },
                || {
                    b_frame = zstdx::bulk::compress_with(&data, &plain(workers[0])).unwrap();
                    black_box(&b_frame);
                },
            );
            report.print(&format!("plain st|mt{}", workers[0]), data.len() as u64);
        }
        // Walls: dict st against each dict mt count (interleaved pairs).
        for &w in &workers {
            let ab = Ab::default();
            let mut a_frame = Vec::new();
            let mut b_frame = Vec::new();
            let report = ab.measure(
                || {
                    a_frame = zstdx::bulk::compress_with(&data, &opts(1)).unwrap();
                    black_box(&a_frame);
                },
                || {
                    b_frame = zstdx::bulk::compress_with(&data, &opts(w)).unwrap();
                    black_box(&b_frame);
                },
            );
            report.print(&format!("dict st|mt{w}"), data.len() as u64);
        }
    }
}
