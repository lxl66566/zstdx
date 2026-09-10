//! Compress every corpus shape into a directory so two builds can be
//! compared byte for byte: `dump <dir_a>` on each build, then `cmp -r`.
//! Byte equality is the regression gate for optimizations that must not
//! change the encoder's choices.
//!
//! Default dumps the Fastest level as `<shape>.raw.zst`; `--all-levels`
//! dumps the whole ladder as `<shape>.raw.l<zstd-level>.zst`.

use std::{fs, path::PathBuf};

use crate::corpus::{LADDER, LevelName, corpus_dir};

#[derive(clap::Args)]
pub struct Args {
    /// Output directory for the compressed snapshots
    pub out_dir: PathBuf,
    /// Dump every ladder level instead of Fastest only
    #[arg(long)]
    pub all_levels: bool,
}

pub fn run(args: &Args) {
    fs::create_dir_all(&args.out_dir).unwrap();
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(corpus_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        // corpus files are generated with a lowercase .raw suffix
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if name.ends_with(".raw") {
            names.push(name);
        }
    }
    names.sort();
    let levels: Vec<(LevelName, i32)> = if args.all_levels {
        LADDER.iter().map(|l| (*l, l.pair().1)).collect()
    } else {
        vec![(LevelName::Fastest, 1)]
    };
    for name in &names {
        let raw = fs::read(corpus_dir().join(name)).unwrap();
        for (level, z) in &levels {
            let comp = zstdx::encoding::compress_slice_to_vec(&raw[..], level.pair().0);
            let suffix = if args.all_levels {
                format!(".l{z}")
            } else {
                String::new()
            };
            fs::write(args.out_dir.join(format!("{name}{suffix}.zst")), &comp).unwrap();
        }
    }
}
