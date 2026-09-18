extern crate zstdx;
mod progress;

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{ArgAction, Parser};
use progress::{ProgressMonitor, fmt_size};
use tracing::info;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
use zstdx::{DecoderOptions, EncoderOptions, Level};

/// Suffix added on compression and required (or stripped) on decompression.
const ZSTD_SUFFIX: &str = ".zst";
/// libzstd's CLI default compression level.
const DEFAULT_LEVEL: i32 = 3;
/// libzstd's CLI caps levels here unless `--ultra` is passed.
const MAX_LEVEL_WITHOUT_ULTRA: i32 = 19;
/// Message prefix, mirroring zstd's `zstd: ...` diagnostics.
const PREFIX: &str = "zstdx: ";

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mode {
    Compress,
    Decompress,
    /// Decompress to nowhere: integrity check only.
    Test,
}

// Boolean flags mirror zstd's CLI switches one-to-one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(
    version,
    about = "Compress or decompress files in the Zstandard format",
    long_about = "zstdx is a zstd-compatible command line for the pure-Rust zstdx engine: `zstdx \
                  file` writes file.zst, `zstdx -d file.zst` restores file, and with no FILES (or \
                  `-`) data streams from stdin to stdout."
)]
struct Cli {
    /// Files to process. Directories require -r. With no FILES, or for `-`,
    /// read stdin and write to stdout
    #[arg(value_name = "FILES")]
    files: Vec<PathBuf>,

    /// Decompress (compression is the default)
    #[arg(short = 'd', long, alias = "uncompress", conflicts_with = "compress")]
    decompress: bool,

    /// Compress; this is the default, the flag exists for explicitness
    #[arg(short = 'z', long)]
    compress: bool,

    /// Test the integrity of compressed files without writing output
    #[arg(short = 't', long, conflicts_with_all = ["compress", "output", "rm"])]
    test: bool,

    /// Write to stdout instead of a file
    #[arg(short = 'c', long)]
    stdout: bool,

    /// Write output to FILE (only valid with a single input)
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Keep input files (the default)
    #[arg(short = 'k', long)]
    keep: bool,

    /// Remove input files after successful (de)compression to a file
    #[arg(long, conflicts_with = "keep")]
    rm: bool,

    /// Overwrite existing output files without asking, and allow reading
    /// compressed input from / writing compressed output to a terminal
    #[arg(short = 'f', long)]
    force: bool,

    /// Recurse into directories given as FILES
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Suppress progress and informational output; repeat to also suppress
    /// errors
    #[arg(short = 'q', long, action = ArgAction::Count, conflicts_with = "verbose")]
    quiet: u8,

    /// Increase verbosity (repeatable)
    #[arg(short = 'v', long, action = ArgAction::Count)]
    verbose: u8,

    /// Worker threads for (de)compression; -T0 auto-detects the number of
    /// CPU cores
    #[arg(short = 'T', long, value_name = "N", default_value_t = 1)]
    threads: u32,

    /// Compression level 1-19 (0 stores uncompressed); the digit flags
    /// -1 .. -19 are the zstd-style alias
    #[arg(long, hide = true, value_name = "N", conflicts_with = "fast")]
    level: Option<i32>,

    /// Fast mode: --fast or --fast=N select negative levels (faster, larger
    /// output)
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "1", require_equals = true)]
    fast: Option<i32>,

    /// Allow levels 20-22 (slower, needs much memory; some decoders reject
    /// the result)
    #[arg(long)]
    ultra: bool,

    /// Zstd dictionary to (de)compress against (as produced by `zstd
    /// --train`)
    #[arg(short = 'D', long, value_name = "DICT")]
    dict: Option<PathBuf>,
}

/// One input to process: either stdin or a file on disk.
enum Input {
    Stdin,
    File(PathBuf),
}

/// Where one input's output goes.
enum Output {
    Stdout,
    File(PathBuf),
    /// `Mode::Test`: decode and discard.
    Sink,
}

fn main() -> ExitCode {
    let cli = Cli::parse_from(normalize_args(std::env::args_os()));
    init_logging(&cli);
    if run(&cli) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Returns false when any input failed to process.
fn run(cli: &Cli) -> bool {
    let mode = if cli.test {
        Mode::Test
    } else if cli.decompress {
        Mode::Decompress
    } else {
        Mode::Compress
    };

    let threads = if cli.threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    } else {
        cli.threads
    };

    let mut level = cli.fast.map_or_else(
        || cli.level.unwrap_or(DEFAULT_LEVEL),
        |acceleration| -acceleration,
    );
    if mode == Mode::Compress && level > MAX_LEVEL_WITHOUT_ULTRA && !cli.ultra {
        eprintln!(
            "{PREFIX}Warning: compression level higher than max, reduced to \
             {MAX_LEVEL_WITHOUT_ULTRA} (use --ultra to allow 20-22)"
        );
        level = MAX_LEVEL_WITHOUT_ULTRA;
    }

    let dict = match &cli.dict {
        Some(path) => match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                eprintln!("{PREFIX}{}: {err}", path.display());
                return false;
            },
        },
        None => None,
    };

    let mut inputs = Vec::new();
    let mut ok = true;
    if cli.files.is_empty() {
        inputs.push(Input::Stdin);
    } else {
        for file in &cli.files {
            collect_input(file, cli.recursive, &mut inputs, &mut ok);
        }
    }

    if cli.output.is_some() && inputs.len() > 1 {
        eprintln!("{PREFIX}-o/--output cannot be used with multiple input files");
        return false;
    }

    let stdout_output =
        cli.stdout || cli.output.is_none() && inputs.iter().any(|i| matches!(i, Input::Stdin));
    if cli.rm && stdout_output {
        eprintln!("{PREFIX}Note: input files are not removed when output is stdout");
    }

    for input in &inputs {
        if let Err(err) = process(input, cli, mode, level, threads, dict.as_deref()) {
            if cli.quiet < 2 {
                let name = match input {
                    Input::Stdin => "stdin".to_string(),
                    Input::File(path) => path.display().to_string(),
                };
                eprintln!("{PREFIX}{name}: {err}");
            }
            ok = false;
        }
    }
    ok
}

/// Gather the file list, expanding directories when `recursive` is set.
fn collect_input(path: &Path, recursive: bool, inputs: &mut Vec<Input>, ok: &mut bool) {
    if path.as_os_str() == "-" {
        inputs.push(Input::Stdin);
        return;
    }
    if path.is_dir() {
        if !recursive {
            eprintln!("{PREFIX}{}: is a directory -- ignored", path.display());
            *ok = false;
            return;
        }
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries.filter_map(Result::ok) {
                        let entry_path = entry.path();
                        if entry_path.is_dir() {
                            stack.push(entry_path);
                        } else {
                            inputs.push(Input::File(entry_path));
                        }
                    }
                },
                Err(err) => {
                    eprintln!("{PREFIX}{}: {err}", dir.display());
                    *ok = false;
                },
            }
        }
        return;
    }
    inputs.push(Input::File(path.to_path_buf()));
}

/// Process a single input; errors are reported by the caller.
fn process(
    input: &Input,
    cli: &Cli,
    mode: Mode,
    level: i32,
    threads: u32,
    dict: Option<&[u8]>,
) -> AnyResult<()> {
    let output = match mode {
        Mode::Test => Output::Sink,
        _ => {
            if let Some(path) = &cli.output {
                Output::File(path.clone())
            } else if cli.stdout || matches!(input, Input::Stdin) {
                Output::Stdout
            } else {
                let Input::File(path) = input else {
                    unreachable!()
                };
                Output::File(default_output_name(path, mode)?)
            }
        },
    };

    if matches!(input, Input::Stdin) && io::stdin().is_terminal() && !cli.force {
        return Err("stdin is a terminal, aborting (use -f to force)".into());
    }
    if mode == Mode::Compress
        && matches!(output, Output::Stdout)
        && io::stdout().is_terminal()
        && !cli.force
    {
        return Err("refusing to write compressed data to a terminal (use -f to force)".into());
    }
    if let (Input::File(in_path), Output::File(out_path)) = (input, &output) {
        if in_path == out_path {
            return Err("input and output cannot be the same file".into());
        }
        check_overwrite(out_path, cli)?;
    }

    let (reader, source_size) = open_input(input)?;
    let show_progress = source_size > 0
        && !matches!(output, Output::Stdout)
        && cli.quiet == 0
        && io::stderr().is_terminal();
    let reader: Box<dyn Read> = if show_progress {
        Box::new(ProgressMonitor::new(reader, source_size, true))
    } else {
        Box::new(reader)
    };

    let written = match mode {
        Mode::Compress => compress(reader, &output, level, threads, source_size, dict)?,
        Mode::Decompress | Mode::Test => decompress(reader, &output, threads, dict)?,
    };

    if cli.quiet == 0 {
        match mode {
            Mode::Compress => {
                let ratio = if source_size == 0 {
                    0.0
                } else {
                    written as f64 / source_size as f64 * 100.0
                };
                info!(
                    "{} ——> {} ({ratio:.2}%)",
                    fmt_size(source_size as f64),
                    fmt_size(written as f64)
                );
            },
            Mode::Decompress => info!(
                "{} ——> {}",
                fmt_size(source_size as f64),
                fmt_size(written as f64)
            ),
            Mode::Test => info!("{} tested ok", fmt_size(written as f64)),
        }
    }

    if cli.rm
        && !cli.keep
        && matches!(output, Output::File(_))
        && let Input::File(path) = input
    {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// libzstd's output naming: compress appends `.zst`, decompress requires and
/// strips it.
fn default_output_name(input: &Path, mode: Mode) -> AnyResult<PathBuf> {
    match mode {
        Mode::Compress => Ok(add_extension(input, ZSTD_SUFFIX)),
        Mode::Decompress | Mode::Test => {
            let name = input
                .to_str()
                .and_then(|s| s.strip_suffix(ZSTD_SUFFIX))
                .filter(|s| !s.is_empty());
            match name {
                Some(stripped) => Ok(PathBuf::from(stripped)),
                None => Err(format!(
                    "unknown suffix ({ZSTD_SUFFIX} expected). Can't derive the output file name. \
                     Specify it with -o. Ignoring."
                )
                .into()),
            }
        },
    }
}

/// Refuse to clobber an existing output file unless forced or confirmed.
fn check_overwrite(path: &Path, cli: &Cli) -> AnyResult<()> {
    if cli.force || !path.exists() {
        return Ok(());
    }
    if io::stdin().is_terminal() {
        eprint!(
            "{PREFIX}{}: already exists; overwrite (y/N)? ",
            path.display()
        );
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(());
        }
    }
    Err(format!("{}: already exists; not overwritten", path.display()).into())
}

fn open_input(input: &Input) -> AnyResult<(Box<dyn Read>, usize)> {
    match input {
        Input::Stdin => Ok((Box::new(io::stdin()), 0)),
        Input::File(path) => {
            let file = File::open(path)?;
            let size = file.metadata()?.len() as usize;
            Ok((Box::new(BufReader::new(file)), size))
        },
    }
}

/// Returns the number of bytes written.
fn compress(
    mut reader: Box<dyn Read>,
    output: &Output,
    level: i32,
    threads: u32,
    source_size: usize,
    dict: Option<&[u8]>,
) -> AnyResult<u64> {
    let mut options = EncoderOptions::new(Level::from_zstd(level));
    if source_size > 0 {
        options = options.pledged_size(Some(source_size as u64));
    }
    // 0/1 worker runs on the calling thread; more engages the job pool.
    if threads > 1 {
        options = options.workers(threads);
    }
    if let Some(dict) = dict {
        options = options.dictionary(dict);
    }

    let (writer, out_file) = open_output(output)?;
    let mut encoder = zstdx::stream::write::Encoder::with_options(writer, options)?;
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;
    match out_file {
        Some(file) => Ok(file.metadata()?.len()),
        None => Ok(0),
    }
}

/// Returns the number of bytes written.
fn decompress(
    reader: Box<dyn Read>,
    output: &Output,
    threads: u32,
    dict: Option<&[u8]>,
) -> AnyResult<u64> {
    let mut options = DecoderOptions::new();
    if threads > 1 {
        options = options.threads(threads);
    }
    if let Some(dict) = dict {
        options = options.dictionary(dict);
    }

    let mut decoder = zstdx::stream::read::Decoder::with_options(reader, options)?;
    let (mut writer, out_file) = open_output(output)?;
    io::copy(&mut decoder, &mut writer)?;
    writer.flush()?;
    match out_file {
        Some(file) => Ok(file.metadata()?.len()),
        None => Ok(0),
    }
}

/// Open the output writer; the `File` is kept alongside so its final size can
/// be reported and stdout/sink outputs share the type.
fn open_output(output: &Output) -> AnyResult<(Box<dyn Write>, Option<File>)> {
    match output {
        Output::Stdout => Ok((Box::new(io::stdout().lock()), None)),
        Output::Sink => Ok((Box::new(io::sink()), None)),
        Output::File(path) => {
            let file = File::create(path)?;
            Ok((Box::new(file.try_clone()?), Some(file)))
        },
    }
}

fn init_logging(cli: &Cli) {
    let to_stdout = cli.stdout || cli.files.is_empty();
    let effective_quiet = cli.quiet + u8::from(to_stdout && cli.verbose == 0);
    let filter = match (effective_quiet, cli.verbose) {
        (2.., _) => tracing::level_filters::LevelFilter::ERROR,
        (1, _) => tracing::level_filters::LevelFilter::WARN,
        (0, 0) => tracing::level_filters::LevelFilter::INFO,
        (0, 1) => tracing::level_filters::LevelFilter::DEBUG,
        (0, _) => tracing::level_filters::LevelFilter::TRACE,
    };
    let indicatif_layer = IndicatifLayer::new();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .without_time()
                .with_filter(filter),
        )
        .with(indicatif_layer)
        .init();
}

/// Translate zstd's `-#` level flags (`-1` .. `-19`) into the hidden
/// `--level` option so clap can parse them. Everything after `--`, and any
/// non-digit flag, passes through untouched.
fn normalize_args<I: IntoIterator<Item = OsString>>(args: I) -> Vec<OsString> {
    let mut normalized = Vec::new();
    let mut literal_args = false;
    for (index, arg) in args.into_iter().enumerate() {
        if index == 0 || literal_args {
            normalized.push(arg);
            continue;
        }
        if arg == "--" {
            literal_args = true;
            normalized.push(arg);
            continue;
        }
        let digit_level = arg.to_str().filter(|s| {
            let digits = &s[1..];
            s.len() >= 2
                && s.starts_with('-')
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
        });
        match digit_level {
            Some(s) => normalized.push(OsString::from(format!("--level={}", &s[1..]))),
            None => normalized.push(arg),
        }
    }
    normalized
}

/// A temporary utility function that appends a file extension
/// to the provided path buf.
///
/// Pending removal when our MSRV reaches 1.91 so we can use
///
/// <https://doc.rust-lang.org/std/path/struct.PathBuf.html#method.add_extension>
fn add_extension<P: AsRef<Path>>(path: &Path, extension: P) -> PathBuf {
    let mut output = path.to_path_buf().into_os_string();
    output.push(extension.as_ref().as_os_str());

    output.into()
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use crate::{Mode, add_extension, default_output_name, normalize_args};

    #[test]
    fn extension_added() {
        let filename = PathBuf::from("README.md");
        assert_eq!(
            add_extension(&filename, ".zst"),
            PathBuf::from("README.md.zst")
        );
    }

    #[test]
    fn default_names() {
        let file = PathBuf::from("data.bin");
        assert_eq!(
            default_output_name(&file, Mode::Compress).unwrap(),
            PathBuf::from("data.bin.zst")
        );
        assert_eq!(
            default_output_name(&PathBuf::from("data.bin.zst"), Mode::Decompress).unwrap(),
            PathBuf::from("data.bin")
        );
        assert!(default_output_name(&PathBuf::from("data.bin"), Mode::Decompress).is_err());
        assert!(default_output_name(&PathBuf::from(".zst"), Mode::Decompress).is_err());
    }

    #[test]
    fn digit_flags_become_level() {
        let args = vec![
            OsString::from("zstdx"),
            OsString::from("-d"),
            OsString::from("-19"),
            OsString::from("--"),
            OsString::from("-3"),
        ];
        assert_eq!(normalize_args(args), vec![
            OsString::from("zstdx"),
            OsString::from("-d"),
            OsString::from("--level=19"),
            OsString::from("--"),
            OsString::from("-3"),
        ]);
    }
}
