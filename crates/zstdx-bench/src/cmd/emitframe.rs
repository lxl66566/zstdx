//! Emit one corpus file as a zstdx frame (bulk or streaming, st or mt) for
//! downstream tools (`piecepipe`, `files`, `mtcheck`): our own MT frames
//! with a chosen worker count are what the deep-offset-ramp experiments
//! analyze.

use std::{fs, path::PathBuf};

use zstdx::{EncoderOptions, Level, bulk};

#[derive(clap::Args)]
pub struct Args {
    /// Raw input file
    input: PathBuf,
    /// Output frame path
    output: PathBuf,
    /// Numeric level (1-22) or tier name (fastest/fast/balanced/best/opt/ultra)
    #[arg(long, default_value = "3")]
    level: String,
    /// Worker count (1 = single-threaded)
    #[arg(long, default_value_t = 4)]
    workers: u32,
    /// Streaming encode path instead of bulk
    #[arg(long)]
    stream: bool,
    /// Pledge the input size on the stream path (mid-band far-class
    /// screens key on the declared length)
    #[arg(long)]
    pledge: bool,
}

fn parse_level(s: &str) -> Level {
    match s.parse::<i32>() {
        Ok(n) => Level::from_zstd(n),
        Err(_) => match s {
            "fastest" => Level::Fastest,
            "fast" => Level::Fast,
            "balanced" => Level::Balanced,
            "best" => Level::Best,
            "opt" => Level::Opt,
            "ultra" => Level::Ultra,
            other => panic!("unknown level {other}"),
        },
    }
}

pub fn run(args: &Args) {
    let raw = fs::read(&args.input).expect("read input");
    let level = parse_level(&args.level);
    let mut opts = EncoderOptions::new(level)
        .workers(args.workers)
        .checksum(true);
    if args.pledge {
        opts = opts.pledged_size(Some(raw.len() as u64));
    }
    let frame = if args.stream {
        let mut enc =
            zstdx::stream::write::Encoder::with_options(Vec::new(), opts).expect("stream encoder");
        std::io::Write::write_all(&mut enc, &raw).expect("stream write");
        enc.finish().expect("stream finish")
    } else {
        bulk::compress_with(&raw, &opts).expect("bulk encode")
    };
    // Roundtrip gate before the file lands anywhere.
    let back = bulk::decompress(&frame, raw.len()).expect("roundtrip decode");
    assert_eq!(back, raw, "roundtrip mismatch");
    fs::write(&args.output, &frame).expect("write frame");
    println!(
        "{} -> {} ({} B, ratio {:.3}, level {} workers {}{})",
        args.input.display(),
        args.output.display(),
        frame.len(),
        raw.len() as f64 / frame.len() as f64,
        args.level,
        args.workers,
        if args.stream {
            ", stream"
        } else {
            ""
        }
    );
    if args.workers >= 2 && raw.len() >= 2 * 1024 * 1024 {
        // The piecepipe analysis must cut pieces at the real encode job
        // boundaries — a uniform guess drifts (input not divisible by
        // 2*workers) and manufactures depth-guarantee violations.
        let shape = zstdx::InputShape {
            len: Some(raw.len() as u64),
            window_log: None,
        };
        let job =
            zstdx::encoding::mt_job_size_for(raw.len() as u64, args.workers, level, shape, &raw);
        println!("job_size {job}");
    }
}
