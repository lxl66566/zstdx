//! Decode explicit .zst files with a time budget instead of a fixed
//! iteration count.
//!
//! Each file is decoded once for reference; if a raw counterpart is found
//! next to it (the extension-stripped path, or that path plus `.raw`, so
//! both `z000033.zst` and `json.zst3` conventions work), the reference is
//! verified against it. Timing then loops in-place decodes into an
//! exact-size buffer until the budget is spent.

use std::{fs, path::PathBuf};

use zstdx::decoding::{FrameDecoder, errors::FrameDecoderError};

use crate::{
    common::{apply_budget, black_box, measure_solo},
    corpus::raw_counterpart,
};

#[derive(clap::Args)]
pub struct Args {
    /// .zst files to decode
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Per-side measurement budget in milliseconds
    #[arg(long)]
    pub budget_ms: Option<f64>,
    /// Also time our MT decoder at these worker counts (zstd has no MT
    /// decode counterpart)
    #[arg(long, value_delimiter = ',')]
    pub threads: Vec<u32>,
}

/// decode_all_to_vec never grows the vector; retry with geometric growth.
fn decode_to_vec(fr: &mut FrameDecoder, compressed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut step = 1usize << 20;
    loop {
        match fr.decode_all_to_vec(compressed, &mut out) {
            Ok(()) => return out,
            Err(FrameDecoderError::TargetTooSmall) => {
                step *= 2;
                out.reserve(step);
            },
            Err(e) => panic!("decode failed: {e}"),
        }
    }
}

pub fn run(args: &Args) {
    apply_budget(args.budget_ms);
    let mut fr = FrameDecoder::new();
    for path in &args.files {
        let compressed = fs::read(path).unwrap();
        let reference = decode_to_vec(&mut fr, &compressed);

        if let Some(raw_path) = raw_counterpart(path) {
            let raw = fs::read(&raw_path).unwrap();
            assert_eq!(
                raw,
                reference,
                "decoded output mismatch for {}",
                path.display()
            );
        }

        let mut out = vec![0u8; reference.len()];
        let stats = measure_solo(|| {
            fr.decode_all(&compressed, &mut out).unwrap();
            black_box(&out);
        });
        assert_eq!(
            &out[..],
            &reference[..],
            "in-place decode mismatch for {}",
            path.display()
        );
        println!(
            "{}: {:.2} MiB raw, {:.0} MiB/s (med; min {:.0})",
            path.display(),
            reference.len() as f64 / (1024.0 * 1024.0),
            stats.mibs(reference.len() as u64),
            reference.len() as f64 / (1024.0 * 1024.0) / stats.max,
        );
        for threads in &args.threads {
            let stats = measure_solo(|| {
                zstdx::bulk::decompress_to_buffer_with(
                    &compressed,
                    &mut out,
                    &zstdx::DecoderOptions::new().threads(*threads),
                )
                .unwrap();
                black_box(&out);
            });
            assert_eq!(&out[..], &reference[..], "mt decode mismatch");
            println!(
                "  mt{threads}: {:.0} MiB/s (med; min {:.0})",
                stats.mibs(reference.len() as u64),
                reference.len() as f64 / (1024.0 * 1024.0) / stats.max,
            );
        }
    }
}
