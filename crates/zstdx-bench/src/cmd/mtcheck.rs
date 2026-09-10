//! Multithreaded decode validation: decode every `.zst*` file in a
//! directory with several worker counts and byte-compare against the zstd
//! CLI output.

use std::path::PathBuf;
use zstdx::bulk::decompress_with;
use zstdx::options::DecoderOptions;

#[derive(clap::Args)]
pub struct Args {
    /// Directory containing .zst / .zstN files
    pub dir: PathBuf,
    /// Worker counts to validate
    #[arg(long, value_delimiter = ',', default_value = "2,4,8,16")]
    pub workers: Vec<u32>,
}

pub fn run(args: &Args) {
    let mut files: Vec<_> = std::fs::read_dir(&args.dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.to_str().unwrap().starts_with("zst"))
        })
        .collect();
    files.sort();
    let mut fail = 0usize;
    for f in &files {
        let comp = std::fs::read(f).unwrap();
        for workers in &args.workers {
            let opts = DecoderOptions::new().threads(*workers);
            let out = decompress_with(&comp[..], 1 << 20, &opts);
            match out {
                Ok(out) => {
                    let name = f.file_name().unwrap().to_str().unwrap();
                    let raw = std::process::Command::new("zstd")
                        .arg("-dcqf")
                        .arg(f)
                        .output()
                        .unwrap()
                        .stdout;
                    if out != raw {
                        println!("BAD  {name} t{workers}");
                        fail += 1;
                    }
                }
                Err(e) => {
                    println!("ERR  {} t{workers}: {e}", f.display());
                    fail += 1;
                }
            }
        }
        println!("done {}", f.display());
    }
    if fail == 0 {
        println!("ALL OK");
    } else {
        println!("{fail} FAILURES");
        std::process::exit(1);
    }
}
