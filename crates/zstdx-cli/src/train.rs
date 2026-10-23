//! `--train`: build a dictionary from sample files through the crate's
//! `dict` trainer (feature "dict_builder"), mirroring zstd's `--train`
//! surface: file-list samples, `-o` output (default `dictionary`),
//! `--maxdict=#` sizing, and the completion report on stderr.

#[cfg(feature = "dict_builder")]
use std::{fs, io::Write, path::PathBuf};

#[cfg(feature = "dict_builder")]
use zstdx::dict::{TrainConfig, TrainOutcome};

use crate::{Cli, PREFIX};
#[cfg(feature = "dict_builder")]
use crate::{Input, collect_input, read_filelist};

/// zstd's `g_defaultDictName`: the output when `-o` is absent.
#[cfg(feature = "dict_builder")]
const DEFAULT_DICT_NAME: &str = "dictionary";
/// zstd's `g_defaultMaxDictSize` (110K).
#[cfg(feature = "dict_builder")]
const DEFAULT_MAX_DICT: u64 = 112_640;
/// ZDICT's minimum dictionary capacity (`dictBufferCapacity must be at
/// least 256`).
#[cfg(feature = "dict_builder")]
const MIN_DICT: u64 = 256;

/// Entry point: train on the sample files and write the dictionary.
pub fn run(cli: &Cli) -> crate::Exit {
    // The gate comes first: a build without the trainer must say so for
    // every --train invocation, not just the ones with usable samples.
    #[cfg(not(feature = "dict_builder"))]
    {
        let _ = cli;
        eprintln!("{PREFIX}training mode not available (built without the dict_builder feature)");
        crate::Exit::Failure
    }
    #[cfg(feature = "dict_builder")]
    train_with_builder(cli)
}

#[cfg(feature = "dict_builder")]
fn train_with_builder(cli: &Cli) -> crate::Exit {
    if let Some(maxdict) = cli.maxdict
        && maxdict < MIN_DICT
    {
        eprintln!("{PREFIX}--maxdict must be at least {MIN_DICT} bytes");
        return crate::Exit::Failure;
    }

    let mut inputs = Vec::new();
    let mut ok = true;
    let filelist = match &cli.filelist {
        Some(path) => match read_filelist(path) {
            Ok(list) => list,
            Err(err) => {
                eprintln!("{PREFIX}{}: {err}", path.display());
                return crate::Exit::Failure;
            },
        },
        None => Vec::new(),
    };
    for file in cli.files.iter().chain(&filelist) {
        collect_input(
            file,
            cli.recursive,
            &mut inputs,
            &mut ok,
            crate::InputGathering::Raw,
        );
    }
    if inputs.iter().any(|i| matches!(i, Input::Stdin)) {
        eprintln!("{PREFIX}--train does not support reading from standard input ");
        return crate::Exit::Failure;
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
                return crate::Exit::Failure;
            },
        }
    }
    let total_bytes: usize = samples.iter().map(Vec::len).sum();
    // dibio.c refuses under five samples with its own EXM_THROW shape
    // (dibio's has no zstd: prefix and exits with the number).
    if samples.len() < 5 {
        eprintln!("Error 14 : nb of samples too low");
        return crate::Exit::Error(14);
    }
    // Below the trainer's 16-byte trainable minimum the engine itself
    // cannot produce a dictionary.
    if total_bytes < 16 {
        eprintln!("{PREFIX}nb of samples too low ");
        return crate::Exit::Failure;
    }

    let maxdict = cli.maxdict.unwrap_or(DEFAULT_MAX_DICT);
    // DiB_train's own small-corpus warning, before training runs; the
    // loading total caps each sample at 128 KiB (dibio's SAMPLESIZE_MAX).
    let load_total: u64 = samples
        .iter()
        .map(|sample| sample.len().min(128 * 1024) as u64)
        .sum();
    if cli.quiet == 0 && load_total < maxdict * 8 {
        eprintln!("!  Warning : data size of samples too small for target dictionary size ");
        eprintln!("!  Samples should be about 100x larger than target dictionary size ");
    }

    let out: PathBuf = cli
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DICT_NAME));
    let outcome = train_dict(&samples, maxdict);
    // COVER_warnOnSmallCorpus: fewer than 10 dmers per requested dictionary
    // byte means the corpus cannot fill the capacity (printed at display
    // level 1: hidden only under -qq).
    if cli.quiet < 2 {
        let ratio = outcome.dmers as f64 / maxdict as f64;
        if ratio < 10.0 {
            eprintln!(
                "WARNING: The maximum dictionary size {} is too large compared to the source size \
                 {}! size(source)/size(dictionary) = {ratio:.6}, but it should be >= 10! This may \
                 lead to a subpar dictionary! We recommend training on sources at least 10x, and \
                 preferably 100x the size of the dictionary! ",
                maxdict, outcome.dmers
            );
        }
    }
    let dict = outcome.dict;
    let mut file = match fs::File::create(&out) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("{PREFIX}{}: {err}", out.display());
            return crate::Exit::Failure;
        },
    };
    if let Err(err) = file.write_all(&dict).and_then(|()| file.flush()) {
        eprintln!("{PREFIX}{}: {err}", out.display());
        return crate::Exit::Failure;
    }
    // dibio.c reports the result at display level 2: visible by default,
    // hidden under -q.
    if cli.quiet == 0 {
        eprintln!(
            "Save dictionary of size {} into file {} ",
            dict.len(),
            out.display()
        );
    }
    crate::Exit::Success
}

/// The trainer call: `zstd --train`'s formatted shape (magic, dictID,
/// entropy tables, content), with the CLI k-grid defaulting to libzstd's
/// optimizer ladder.
#[cfg(feature = "dict_builder")]
fn train_dict(samples: &[Vec<u8>], maxdict: u64) -> TrainOutcome {
    let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
    zstdx::dict::train_formatted(&refs, maxdict as usize, &TrainConfig::default())
}
