//! Dictionary trainer tooling: train a raw-content dictionary with the
//! in-tree trainer, or extract the content section of a formatted dict
//! (e.g. from `zstd --train`) for A/B isolation of content quality vs
//! entropy-table seeding.

use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
};

use zstdx::dict::{DmerHash, KGrid, MetricMode, TrainConfig};

#[derive(clap::Args)]
pub struct Args {
    /// Training files (or directories, recursed)
    pub files: Vec<PathBuf>,
    /// Output dictionary path
    #[arg(short, long)]
    pub out: PathBuf,
    /// Target dictionary size in bytes (default 16 KiB)
    #[arg(short, long, default_value_t = 16384)]
    pub size: usize,
    /// Do not train; write the content section of this formatted dict
    #[arg(long)]
    pub content_of: Option<PathBuf>,
    /// Emit a formatted dictionary (entropy tables + content, the
    /// `zstd --train` shape) instead of raw content
    #[arg(long)]
    pub formatted: bool,
    /// Segment-size candidate grid: compact, cli (libzstd's optimizer
    /// steps=4 ladder), or a forced k (default: the library default)
    #[arg(long)]
    pub grid: Option<String>,
    /// Selection metric the k sweep scores: raw (the content itself) or
    /// formatted (the finalized dict, like libzstd's optimizer)
    #[arg(long)]
    pub metric: Option<String>,
    /// Frequency-table hashing: exact 64-bit fingerprints or libzstd's
    /// f-bit colliding buckets
    #[arg(long)]
    pub hash: Option<String>,
    /// Count k-mers spanning sample boundaries (cross) or skip them like
    /// libzstd's per-sample frequency loop (stop)
    #[arg(long)]
    pub boundary: Option<String>,
    /// Print the sweep's chosen parameters
    #[arg(short, long)]
    pub verbose: bool,
}

fn parse_grid(s: &str) -> KGrid {
    match s {
        "compact" => KGrid::Compact,
        "cli" => KGrid::LibzstdCli,
        other => KGrid::Fixed(other.parse().expect("grid: compact|cli|<k>")),
    }
}

fn parse_metric(s: &str) -> MetricMode {
    match s {
        "raw" => MetricMode::RawContent,
        "formatted" => MetricMode::Formatted,
        other => panic!("metric: raw|formatted, got {other}"),
    }
}

fn parse_hash(s: &str) -> DmerHash {
    match s {
        "exact" => DmerHash::Exact,
        "buckets" => DmerHash::LibzstdBuckets,
        other => panic!("hash: exact|buckets, got {other}"),
    }
}

fn parse_boundary(s: &str) -> bool {
    match s {
        "cross" => true,
        "stop" => false,
        other => panic!("boundary: cross|stop, got {other}"),
    }
}

pub fn run(args: &Args) {
    if let Some(dict_path) = &args.content_of {
        let raw = fs::read(dict_path).unwrap();
        let dict = zstdx::decoding::Dictionary::decode_dict(&raw).unwrap();
        fs::write(&args.out, &dict.dict_content).unwrap();
        println!(
            "content of {}: {} bytes (id 0x{:08x}) -> {}",
            dict_path.display(),
            dict.dict_content.len(),
            dict.id,
            args.out.display()
        );
        return;
    }
    let mut sources: Vec<PathBuf> = Vec::new();
    for path in &args.files {
        collect(path, &mut sources);
    }
    sources.sort();
    assert!(!sources.is_empty(), "no training files given");
    let samples: Vec<Vec<u8>> = sources.iter().map(|p| fs::read(p).unwrap()).collect();
    let total: usize = samples.iter().map(Vec::len).sum();
    let mut config = TrainConfig::default();
    if let Some(grid) = &args.grid {
        config.k_grid = parse_grid(grid);
    }
    if let Some(metric) = &args.metric {
        config.metric = parse_metric(metric);
    }
    if let Some(hash) = &args.hash {
        config.hash = parse_hash(hash);
    }
    if let Some(boundary) = &args.boundary {
        config.cross_sample_kmers = parse_boundary(boundary);
    }
    println!(
        "training on {} files, {} bytes -> {} ({} bytes)",
        sources.len(),
        total,
        args.out.display(),
        args.size
    );
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();

    let outcome = if args.formatted {
        zstdx::dict::train_formatted(&refs, args.size, &config)
    } else {
        zstdx::dict::train_raw(&refs, args.size, &config)
    };
    let out = fs::File::create(&args.out).unwrap();
    let mut out = BufWriter::new(out);
    out.write_all(&outcome.dict).unwrap();
    out.flush().unwrap();
    if args.verbose {
        println!(
            "grid={:?} metric={:?} hash={:?} cross_sample={} -> k={}",
            config.k_grid, config.metric, config.hash, config.cross_sample_kmers, outcome.k
        );
        for &(k, score) in &outcome.sweep {
            println!("  k={k} metric={score}");
        }
    }
    println!(
        "trained dict: {} bytes",
        fs::metadata(&args.out).unwrap().len()
    );
}

fn collect(path: &std::path::Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            collect(&entry.unwrap().path(), out);
        }
    } else {
        out.push(path.to_path_buf());
    }
}
