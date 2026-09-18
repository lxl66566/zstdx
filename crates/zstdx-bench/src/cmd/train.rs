//! Dictionary trainer tooling: train a raw-content dictionary with the
//! in-tree trainer, or extract the content section of a formatted dict
//! (e.g. from `zstd --train`) for A/B isolation of content quality vs
//! entropy-table seeding.

use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
};

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
    println!(
        "training on {} files, {} bytes -> {} ({} bytes)",
        sources.len(),
        total,
        args.out.display(),
        args.size
    );
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    let out = fs::File::create(&args.out).unwrap();
    let mut out = BufWriter::new(out);
    if args.formatted {
        zstdx::dict::create_formatted_dict_from_samples(&refs, &mut out, args.size);
    } else {
        zstdx::dict::create_raw_dict_from_samples(&refs, &mut out, args.size);
    }
    out.flush().unwrap();
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
