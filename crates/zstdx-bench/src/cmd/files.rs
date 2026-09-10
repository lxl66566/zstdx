//! Decode explicit .zst files with a time budget instead of a fixed
//! iteration count.
//!
//! Each file is decoded once for reference; if a raw counterpart is found
//! next to it (the extension-stripped path, or that path plus `.raw`, so
//! both `z000033.zst` and `json.zst3` conventions work), the reference is
//! verified against it. Timing then loops in-place decodes into an
//! exact-size buffer until the budget is spent.

use crate::common::{apply_budget, black_box, measure_solo};
use std::fs;
use std::path::{Path, PathBuf};
use zstdx::decoding::errors::FrameDecoderError;
use zstdx::decoding::FrameDecoder;

#[derive(clap::Args)]
pub struct Args {
    /// .zst files to decode
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Per-side measurement budget in milliseconds
    #[arg(long)]
    pub budget_ms: Option<f64>,
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
            }
            Err(e) => panic!("decode failed: {e}"),
        }
    }
}

/// Raw counterpart of a corpus-style path: strip a trailing `.zst*`
/// extension, then try the bare stem and `<stem>.raw` (so both the
/// `z000033.zst` and `json.zst3` naming conventions verify).
fn raw_counterpart(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.starts_with("zst"))
        .map(|(stem, _)| stem)?;
    let bare = path.with_file_name(stem);
    let with_raw = path.with_file_name(format!("{stem}.raw"));
    [bare, with_raw]
        .into_iter()
        .find(|p| p.is_file() && p != path)
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
    }
}
