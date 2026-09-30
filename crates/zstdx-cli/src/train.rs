//! `--train`: build a dictionary from sample files through the crate's
//! `dict` trainer (feature "dict_builder"), mirroring zstd's `--train`
//! surface: file-list samples, `-o` output (default `dictionary`),
//! `--maxdict=#` sizing, and the completion report on stderr.

#[cfg(feature = "dict_builder")]
use std::{fs, io::Write, path::PathBuf};

#[cfg(feature = "dict_builder")]
use zstdx::dict::TrainConfig;

use crate::{Cli, PREFIX};
#[cfg(feature = "dict_builder")]
use crate::{Input, collect_input, read_filelist};

/// zstd's `g_defaultDictName`: the output when `-o` is absent.
const DEFAULT_DICT_NAME: &str = "dictionary";
/// zstd's `g_defaultMaxDictSize` (110K).
const DEFAULT_MAX_DICT: u64 = 112_640;
/// ZDICT's minimum dictionary capacity (`dictBufferCapacity must be at
/// least 256`).
const MIN_DICT: u64 = 256;

/// Entry point: train on the sample files and write the dictionary.
/// Returns false on any failure.
pub fn run(cli: &Cli) -> bool {
    // The gate comes first: a build without the trainer must say so for
    // every --train invocation, not just the ones with usable samples.
    #[cfg(not(feature = "dict_builder"))]
    {
        let _ = cli;
        eprintln!("{PREFIX}training mode not available (built without the dict_builder feature)");
        return false;
    }
    #[cfg(feature = "dict_builder")]
    train_with_builder(cli)
}

#[cfg(feature = "dict_builder")]
fn train_with_builder(cli: &Cli) -> bool {
    if let Some(maxdict) = cli.maxdict
        && maxdict < MIN_DICT
    {
        eprintln!("{PREFIX}--maxdict must be at least {MIN_DICT} bytes");
        return false;
    }

    let mut inputs = Vec::new();
    let mut ok = true;
    let filelist = match &cli.filelist {
        Some(path) => match read_filelist(path) {
            Ok(list) => list,
            Err(err) => {
                eprintln!("{PREFIX}{}: {err}", path.display());
                return false;
            },
        },
        None => Vec::new(),
    };
    for file in cli.files.iter().chain(&filelist) {
        collect_input(file, cli.recursive, &mut inputs, &mut ok);
    }
    if inputs.iter().any(|i| matches!(i, Input::Stdin)) {
        eprintln!("{PREFIX}--train does not support reading from standard input ");
        return false;
    }

    let mut samples = Vec::new();
    for input in &inputs {
        let Input::File(path) = input else {
            unreachable!("stdin inputs were rejected above");
        };
        match fs::read(path) {
            Ok(sample) => samples.push(sample),
            Err(err) => {
                eprintln!("{PREFIX}{}: {err}", path.display());
                return false;
            },
        }
    }
    let total_bytes: usize = samples.iter().map(Vec::len).sum();
    // zstd refuses with "Error 14 : nb of samples too low"; our trainer
    // would happily emit content, but a near-empty sample set (less than
    // the trainer's 16-byte trainable minimum) is never a useful
    // dictionary either.
    if inputs.is_empty() || total_bytes < 16 {
        eprintln!("{PREFIX}nb of samples too low ");
        return false;
    }

    let out: PathBuf = cli
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DICT_NAME));
    let dict = train_dict(&samples, cli.maxdict.unwrap_or(DEFAULT_MAX_DICT));
    let mut file = match fs::File::create(&out) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("{PREFIX}{}: {err}", out.display());
            return false;
        },
    };
    if let Err(err) = file.write_all(&dict).and_then(|()| file.flush()) {
        eprintln!("{PREFIX}{}: {err}", out.display());
        return false;
    }
    // zstd overwrites the dictionary target without prompting, and reports
    // the samples while loading and the result on completion.
    eprintln!(
        "training on {} samples, {} bytes, target {} bytes",
        samples.len(),
        total_bytes,
        cli.maxdict.unwrap_or(DEFAULT_MAX_DICT)
    );
    eprintln!(
        "Save dictionary of size {} into file {} ",
        dict.len(),
        out.display()
    );
    true
}

/// The trainer call: `zstd --train`'s formatted shape (magic, dictID,
/// entropy tables, content), with the CLI k-grid defaulting to libzstd's
/// optimizer ladder.
#[cfg(feature = "dict_builder")]
fn train_dict(samples: &[Vec<u8>], maxdict: u64) -> Vec<u8> {
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    zstdx::dict::train_formatted(&refs, maxdict as usize, &TrainConfig::default()).dict
}
