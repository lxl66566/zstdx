extern crate zstdx;
mod list;
mod progress;
mod train;

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{ArgAction, Parser};
use progress::{ProgressMonitor, fmt_size};
use tracing::info;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
use zstdx::{DecoderOptions, EncoderOptions, InputShape, Level};

/// Suffix added on compression and required (or stripped) on decompression.
const ZSTD_SUFFIX: &str = ".zst";
/// libzstd's CLI default compression level.
const DEFAULT_LEVEL: i32 = 3;
/// libzstd's CLI caps levels here unless `--ultra` is passed.
const MAX_LEVEL_WITHOUT_ULTRA: i32 = 19;
/// Lowest window log the frame format can express (`--long` lower bound,
/// mirroring libzstd's `ZSTD_WINDOWLOG_ABSOLUTEMIN`).
const MIN_WINDOW_LOG: u32 = 10;
/// Highest window log the engine implements. libzstd allows up to 30 on
/// 64-bit targets; `--long` above this warns and clamps.
const MAX_WINDOW_LOG: u32 = 27;
/// Smallest window a frame may request, in bytes: `-M` values below it
/// reject every frame (libzstd's out-of-bound parameter error).
const MIN_WINDOW_BYTES: u64 = 1 << MIN_WINDOW_LOG;
/// Message prefix, mirroring zstd's `zstd: ...` diagnostics.
const PREFIX: &str = "zstdx: ";

/// Version line printed by `-V/--version`: crate version plus the compat
/// note (the argument surface mirrors zstd 1.5.7's, in subset).
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (zstd v1.5.7-compatible argument surface; RFC 8878 frames)"
);

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mode {
    Compress,
    Decompress,
    /// Decompress to nowhere: integrity check only.
    Test,
    /// Inspect .zst files: frame counts, sizes, checksums, dictIDs --
    /// header parsing only, no decoding (`-l/--list`).
    List,
    /// Build a dictionary from sample files (`--train`).
    Train,
}

/// Byte multiplier of a size flag suffix, mirroring zstd's
/// `readU32FromChar` (binary multipliers, uppercase suffixes only).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum SizeUnit {
    Bytes,
    Kilo,
    Mega,
    Giga,
}

impl SizeUnit {
    /// The suffix as written after the digits; `B` alone is not a unit
    /// zstd accepts, so neither do we.
    fn parse(suffix: &str) -> Option<Self> {
        match suffix {
            "" => Some(Self::Bytes),
            "K" | "KB" | "KiB" => Some(Self::Kilo),
            "M" | "MB" | "MiB" => Some(Self::Mega),
            "G" | "GB" | "GiB" => Some(Self::Giga),
            _ => None,
        }
    }

    const fn multiplier(self) -> u64 {
        match self {
            Self::Bytes => 1,
            Self::Kilo => 1 << 10,
            Self::Mega => 1 << 20,
            Self::Giga => 1 << 30,
        }
    }
}

/// Parse `<digits>[K|M|G][i][B]` into a byte count (zstd's size syntax).
fn parse_size(text: &str) -> Result<u64, String> {
    // A manual byte scan, not str::split_once: on this repo's nightly
    // (1.100.0-nightly 8925ea358) split_once with a char-closure pattern
    // splits one char late ("128M" -> ("128", "")), while the byte
    // position is exact for any UTF-8 tail (digits are ASCII, and the
    // first non-digit byte of a multibyte char starts that char).
    let split = text
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, suffix) = text.split_at(split);
    if digits.is_empty() {
        return Err(format!("expected a size like 512K or 64M, got {text:?}"));
    }
    let unit = SizeUnit::parse(suffix)
        .ok_or_else(|| format!("unknown size suffix {suffix:?} in {text:?}"))?;
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("size out of range: {text:?}"))?;
    value
        .checked_mul(unit.multiplier())
        .ok_or_else(|| format!("size out of range: {text:?}"))
}

/// Validated, mode-scoped knobs for one invocation, derived once in `run`.
/// Every accepted flag lands in a field here; flags the engine cannot
/// honor are rejected in `run` instead, never silently dropped.
#[derive(Debug)]
struct Settings {
    level: i32,
    threads: u32,
    /// Whether frames carry a content checksum (--no-check clears it).
    checksum: bool,
    /// Forced window log (--long).
    window_log: Option<u32>,
    /// Hard stdin size pledge (--stream-size); file inputs pledge their
    /// metadata size instead.
    stream_size: Option<u64>,
    /// Non-binding stdin size hint (--size-hint): sizes the encoder row
    /// without a header promise.
    size_hint: Option<u64>,
    /// Decode window bound (-M), the dominant decode memory allocation.
    max_window: Option<u64>,
}

// Boolean flags mirror zstd's CLI switches one-to-one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(
    version = VERSION,
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

    /// Print information about Zstandard-compressed files (frame counts,
    /// sizes, ratio, checksum, dictID) without decompressing them
    #[arg(
        short = 'l',
        long,
        conflicts_with_all = ["compress", "decompress", "test", "train", "output"]
    )]
    list: bool,

    /// Create a dictionary from a training set of files (writes `-o FILE`,
    /// default `dictionary`)
    #[arg(long, conflicts_with_all = ["compress", "decompress", "test", "list"])]
    train: bool,

    /// Limit the trained dictionary to # bytes (--train only; suffixes
    /// K/M/G accepted)
    #[arg(long, value_name = "#", value_parser = parse_size)]
    maxdict: Option<u64>,

    // Rejected --train knobs: kept parseable so the diagnostic is a clear
    // "unsupported" error instead of clap's unknown-argument text.
    /// Not supported: the trainer derives the dictionary ID from its
    /// content (deterministic; zstd's --dictID forcing has no equivalent)
    #[arg(long = "dictID", value_name = "#", hide = true)]
    dict_id: Option<u32>,

    /// Not supported: the in-tree trainer is the only algorithm
    #[arg(
        long = "train-cover",
        value_name = "PARAMS",
        hide = true,
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    train_cover: Option<String>,

    /// Not supported: the in-tree trainer is the only algorithm
    #[arg(
        long = "train-fastcover",
        value_name = "PARAMS",
        hide = true,
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    train_fastcover: Option<String>,

    /// Not supported: the in-tree trainer is the only algorithm
    #[arg(
        long = "train-legacy",
        value_name = "PARAMS",
        hide = true,
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    train_legacy: Option<String>,

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

    /// Enable long-distance matching by forcing the window log to N
    /// (default 27, the maximum; 10-27). A known source size still shrinks
    /// the window to the source, as zstd does
    ///
    /// Note: unlike zstd, this flag alone does not force the long-distance
    /// matcher on -- the engine arms LDM by level row once the effective
    /// window reaches 32 MiB
    #[arg(
        long,
        value_name = "N",
        num_args = 0..=1,
        default_missing_value = "27",
        require_equals = true,
        conflicts_with_all = ["decompress", "test"]
    )]
    long: Option<u32>,

    /// Write a frame content checksum (the default)
    #[arg(long, conflicts_with = "no_check")]
    check: bool,

    /// Omit the frame content checksum. Compression only: decoding always
    /// validates a present checksum
    #[arg(long)]
    no_check: bool,

    /// Pledge the exact byte size of the stdin stream (mismatching input
    /// fails, mirroring zstd); stdin only
    #[arg(long, value_name = "N", value_parser = parse_size, conflicts_with = "size_hint")]
    stream_size: Option<u64>,

    /// Size encoder tables for a stdin stream of approximately N bytes, a
    /// non-binding hint (no frame header promise); stdin only
    #[arg(long, value_name = "N", value_parser = parse_size)]
    size_hint: Option<u64>,

    /// Bound decode memory: frames whose window exceeds N bytes fail to
    /// decode (suffixes K/M/G accepted); decode modes only
    #[arg(short = 'M', long, value_name = "N", value_parser = parse_size)]
    memory: Option<u64>,

    /// Read the list of input files from LIST, one per line; a LIST of `-`
    /// reads the list itself from stdin
    #[arg(long, alias = "file", value_name = "LIST")]
    filelist: Option<PathBuf>,

    /// Force the progress bar on (it still needs a known input size, so
    /// stdin streams show none)
    #[arg(long, conflicts_with = "no_progress")]
    progress: bool,

    /// Hide the progress bar
    #[arg(long)]
    no_progress: bool,

    /// Compress on the calling thread; equivalent to -T1 here, whose
    /// in-line form already shares nothing with other encoders
    #[arg(long, conflicts_with = "threads")]
    single_thread: bool,

    /// Per-input failures (e.g. one unreadable file under -r) are reported
    /// but do not affect the exit code
    #[arg(long)]
    ignore_errors: bool,

    // Rejected flags: parsing them keeps the diagnostic a clear
    // "unsupported" error instead of clap's generic unknown-argument text.
    /// Not supported: output is always written dense (sparse TODO: decode
    /// the zero-run blocks into seeks)
    #[arg(long, hide = true)]
    sparse: bool,

    /// Not supported: output is always written dense (see --sparse)
    #[arg(long, hide = true, conflicts_with = "sparse")]
    no_sparse: bool,
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
    let mode = if cli.train {
        Mode::Train
    } else if cli.list {
        Mode::List
    } else if cli.test {
        Mode::Test
    } else if cli.decompress {
        Mode::Decompress
    } else {
        Mode::Compress
    };

    // Flags whose meaning the engine cannot honor are rejected up front:
    // every accepted flag changes behavior below, none is a silent no-op.
    if cli.sparse || cli.no_sparse {
        eprintln!("{PREFIX}--sparse/--no-sparse are not supported: output is always dense");
        return false;
    }
    if cli.dict_id.is_some() {
        eprintln!(
            "{PREFIX}--dictID is not supported: the trainer derives the dictionary ID from its \
             content"
        );
        return false;
    }
    if cli.train_cover.is_some() || cli.train_fastcover.is_some() || cli.train_legacy.is_some() {
        eprintln!(
            "{PREFIX}--train-cover/--train-fastcover/--train-legacy are not supported: the \
             in-tree trainer is the only algorithm"
        );
        return false;
    }
    if cli.maxdict.is_some() && mode != Mode::Train {
        eprintln!("{PREFIX}--maxdict only applies with --train");
        return false;
    }

    // The inspection and training modes are self-contained: they gather
    // their own inputs and never reach the (de)compression machinery.
    match mode {
        Mode::List => return list::run(cli),
        Mode::Train => return train::run(cli),
        Mode::Compress | Mode::Decompress | Mode::Test => {},
    }
    if (cli.check || cli.no_check) && mode != Mode::Compress {
        eprintln!(
            "{PREFIX}--check/--no-check affect compression only: decoding always validates a \
             present checksum"
        );
        return false;
    }
    if cli.memory.is_some() && mode == Mode::Compress {
        eprintln!("{PREFIX}-M/--memory bounds decoding only; it does not limit compression");
        return false;
    }
    if cli.memory.is_some_and(|limit| limit < MIN_WINDOW_BYTES) {
        eprintln!(
            "{PREFIX}-M values below {MIN_WINDOW_BYTES} reject every frame (the format's minimum \
             window); use at least 1K"
        );
        return false;
    }
    let window_log = match cli.long {
        None => None,
        Some(log) if log < MIN_WINDOW_LOG => {
            eprintln!("{PREFIX}--long: window log must be at least {MIN_WINDOW_LOG} (got {log})");
            return false;
        },
        Some(log) if log > MAX_WINDOW_LOG => {
            eprintln!(
                "{PREFIX}Warning: --long={log} above the engine's maximum {MAX_WINDOW_LOG}, \
                 reduced to {MAX_WINDOW_LOG}"
            );
            Some(MAX_WINDOW_LOG)
        },
        log => log,
    };

    let threads = if cli.single_thread || cli.threads == 1 {
        1
    } else if cli.threads == 0 {
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

    let mut inputs = Vec::new();
    let mut ok = true;
    if cli.files.is_empty() && filelist.is_empty() {
        inputs.push(Input::Stdin);
    } else {
        for file in cli.files.iter().chain(&filelist) {
            collect_input(file, cli.recursive, &mut inputs, &mut ok);
        }
    }

    // Size flags shape the stdin stream's encoder row; a file input already
    // carries its exact size as the pledge, so honoring a second, looser
    // number would silently do nothing (zstd ignores it there).
    let stdin_stream = matches!(inputs.as_slice(), [Input::Stdin]);
    if cli.stream_size.is_some() && !stdin_stream {
        eprintln!("{PREFIX}--stream-size only applies to stdin input");
        return false;
    }
    if cli.size_hint.is_some() && !stdin_stream {
        eprintln!(
            "{PREFIX}--size-hint only applies to stdin input (file sizes are pledged exactly)"
        );
        return false;
    }

    if cli.output.is_some() && inputs.len() > 1 {
        eprintln!("{PREFIX}-o/--output cannot be used with multiple input files");
        return false;
    }

    let settings = Settings {
        level,
        threads,
        checksum: !cli.no_check,
        window_log,
        stream_size: cli.stream_size,
        size_hint: cli.size_hint,
        max_window: cli.memory,
    };

    let stdout_output =
        cli.stdout || cli.output.is_none() && inputs.iter().any(|i| matches!(i, Input::Stdin));
    if cli.rm && stdout_output {
        eprintln!("{PREFIX}Note: input files are not removed when output is stdout");
    }

    for input in &inputs {
        if let Err(err) = process(input, cli, mode, &settings, dict.as_deref()) {
            if cli.quiet < 2 {
                let name = match input {
                    Input::Stdin => "stdin".to_string(),
                    Input::File(path) => path.display().to_string(),
                };
                eprintln!("{PREFIX}{name}: {err}");
            }
            // --ignore-errors keeps per-input failures out of the exit code.
            if !cli.ignore_errors {
                ok = false;
            }
        }
    }
    ok
}

/// Read `--filelist`: one path per line, blank lines skipped.
fn read_filelist(path: &Path) -> AnyResult<Vec<PathBuf>> {
    let reader: Box<dyn BufRead> = if path.as_os_str() == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(BufReader::new(File::open(path)?))
    };
    let mut paths = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.is_empty() {
            paths.push(PathBuf::from(line));
        }
    }
    Ok(paths)
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

/// Whether the progress bar may draw for one input. `--[no-]progress`
/// overrides the auto rule (interactive stderr, not quiet, no stdout
/// output); every form still needs a known input size, so stdin streams
/// never show a bar.
fn progress_visible(cli: &Cli, sized: bool, stdout_output: bool, interactive: bool) -> bool {
    if !sized {
        return false;
    }
    if cli.no_progress {
        return false;
    }
    if cli.progress {
        return true;
    }
    cli.quiet == 0 && interactive && !stdout_output
}

/// Process a single input; errors are reported by the caller.
fn process(
    input: &Input,
    cli: &Cli,
    mode: Mode,
    settings: &Settings,
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
    if let (Input::File(in_path), Output::File(out_path)) = (input, &output)
        && in_path == out_path
    {
        return Err("input and output cannot be the same file".into());
    }
    // The overwrite check guards every file output, no matter where the
    // input comes from: piped stdin clobbers an `-o` target just as well as
    // a file input does (the same-file check above is inherently File-only).
    if let Output::File(out_path) = &output {
        check_overwrite(out_path, cli)?;
    }

    let (reader, source_size) = open_input(input)?;
    let show_progress = progress_visible(
        cli,
        source_size > 0,
        matches!(output, Output::Stdout),
        io::stderr().is_terminal(),
    );
    let reader: Box<dyn Read> = if show_progress {
        Box::new(ProgressMonitor::new(reader, source_size, true))
    } else {
        Box::new(reader)
    };

    let written = match mode {
        Mode::Compress => compress(reader, &output, settings, source_size, dict)?,
        // List and train are dispatched before per-input processing and
        // never reach this point.
        Mode::Decompress | Mode::Test => decompress(reader, &output, settings, dict)?,
        Mode::List | Mode::Train => unreachable!("dispatched in run"),
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
            Mode::List | Mode::Train => unreachable!("dispatched in run"),
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
        // List and train never reach name derivation (dispatched earlier).
        Mode::Decompress | Mode::Test | Mode::List | Mode::Train => {
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
/// The answer is read from the controlling terminal, never from stdin: piped
/// stdin is input data that must not be consumed as an answer. Without a
/// terminal there is nobody to ask, so the file is not overwritten.
fn check_overwrite(path: &Path, cli: &Cli) -> AnyResult<()> {
    if cli.force || !path.exists() {
        return Ok(());
    }
    if let Ok(mut tty) = File::open("/dev/tty").map(BufReader::new) {
        eprint!(
            "{PREFIX}{}: already exists; overwrite (y/N)? ",
            path.display()
        );
        io::stderr().flush()?;
        let mut answer = String::new();
        if tty.read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes") {
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
    settings: &Settings,
    source_size: usize,
    dict: Option<&[u8]>,
) -> AnyResult<u64> {
    let mut options =
        EncoderOptions::new(Level::from_zstd(settings.level)).checksum(settings.checksum);
    // A file input pledges its metadata size; a stdin stream pledges
    // --stream-size or nothing.
    let pledged = settings
        .stream_size
        .or((source_size > 0).then_some(source_size as u64));
    options = options.pledged_size(pledged);
    // --long forces the window, --size-hint sizes the row; a known pledge
    // still shrinks the window to the source inside the engine, as zstd
    // does.
    let mut shape = InputShape::default();
    if let Some(log) = settings.window_log {
        shape = shape.with_window_log(log);
    }
    if let Some(hint) = settings.size_hint {
        shape = shape.with_len(hint);
    }
    options = options.with_input_shape(shape);
    // 0/1 worker runs on the calling thread; more engages the job pool.
    if settings.threads > 1 {
        options = options.workers(settings.threads);
    }
    if let Some(dict) = dict {
        options = options.dictionary(dict);
    }

    let (writer, out_file) = open_output(output)?;
    // The whole write phase runs in a closure so the output handles are
    // dropped before the partial output is removed on failure.
    let written: AnyResult<u64> = (|| {
        match out_file {
            Some(file) => {
                let mut encoder = zstdx::stream::write::Encoder::with_options(writer, options)?;
                io::copy(&mut reader, &mut encoder)?;
                encoder.finish()?;
                Ok(file.metadata()?.len())
            },
            // stdout and the test-mode sink have no file to stat; count the
            // bytes pushed through the writer instead
            None => {
                let mut out = CountingWriter::new(writer);
                let mut encoder = zstdx::stream::write::Encoder::with_options(&mut out, options)?;
                io::copy(&mut reader, &mut encoder)?;
                encoder.finish()?;
                Ok(out.count)
            },
        }
    })();
    if written.is_err() {
        remove_partial_output(output);
    }
    written
}

/// Returns the number of bytes written.
fn decompress(
    reader: Box<dyn Read>,
    output: &Output,
    settings: &Settings,
    dict: Option<&[u8]>,
) -> AnyResult<u64> {
    let mut options = DecoderOptions::new();
    if settings.threads > 1 {
        options = options.threads(settings.threads);
    }
    if let Some(max) = settings.max_window {
        options = options.max_window_size(max);
    }
    if let Some(dict) = dict {
        options = options.dictionary(dict);
    }

    // Decoder construction precedes output creation: a failure here must
    // not touch the output path at all.
    let mut decoder = zstdx::stream::read::Decoder::with_options(reader, options)?;
    let (mut writer, out_file) = open_output(output)?;
    // The whole write phase runs in a closure so the output handles are
    // dropped before the partial output is removed on failure.
    let written: AnyResult<u64> = (|| {
        let copied = io::copy(&mut decoder, &mut writer)?;
        writer.flush()?;
        match out_file {
            Some(file) => Ok(file.metadata()?.len()),
            // stdout and the test-mode sink have no file to stat; the copied
            // count is the decompressed size either way
            None => Ok(copied),
        }
    })();
    if written.is_err() {
        remove_partial_output(output);
    }
    written
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

/// A passthrough writer that tallies the bytes accepted by the inner writer,
/// so output sizes can be reported where no file exists to stat.
struct CountingWriter<W: Write> {
    inner: W,
    count: u64,
}

impl<W: Write> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Remove the output file a failed operation created, so no corrupt partial
/// output is left behind (mirrors zstd). Only `Output::File` names a file we
/// created; removal errors must not mask the original failure.
fn remove_partial_output(output: &Output) {
    if let Output::File(path) = output {
        let _ = fs::remove_file(path);
    }
}

fn init_logging(cli: &Cli) {
    let to_stdout = cli.stdout || cli.files.is_empty() && cli.filelist.is_none();
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
        // strip_prefix keeps the slicing boundary-safe: an argument starting
        // with a multibyte character (a non-ASCII path) must pass through
        // instead of panicking on a mid-character slice.
        let digit_level = arg.to_str().and_then(|s| {
            let digits = s.strip_prefix('-')?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| OsString::from(format!("--level={digits}")))
        });
        match digit_level {
            Some(level) => normalized.push(level),
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
    use std::{
        ffi::OsString,
        io::{Cursor, Write as _},
        path::PathBuf,
    };

    use clap::Parser as _;
    use zstdx::Level;

    use crate::{
        Cli, Mode, Output, Settings, add_extension, compress, decompress, default_output_name,
        normalize_args, parse_size, progress_visible,
    };

    /// A per-test temp path, pre-cleaned so reruns start from nothing.
    fn temp_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("zstdx-cli-{}-{tag}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A `--`-less default CLI for the pure decision functions.
    fn plain_cli() -> Cli {
        Cli::try_parse_from(["zstdx"]).unwrap()
    }

    /// A valid multi-block frame.
    fn compressed_payload() -> Vec<u8> {
        let payload = vec![7u8; 300 * 1024];
        let mut encoder =
            zstdx::stream::write::Encoder::new(Vec::new(), Level::from_zstd(3)).unwrap();
        encoder.write_all(&payload).unwrap();
        encoder.finish().unwrap()
    }

    /// A decompress failure after the output file was created must not leave
    /// the partial output on disk.
    #[test]
    fn failed_decompress_removes_partial_output() {
        let mut compressed = compressed_payload();
        compressed.truncate(compressed.len() - 8);
        let out = temp_path("decompress-partial");
        let result = decompress(
            Box::new(Cursor::new(compressed)),
            &Output::File(out.clone()),
            &Settings {
                level: 3,
                threads: 1,
                checksum: true,
                window_log: None,
                stream_size: None,
                size_hint: None,
                max_window: None,
            },
            None,
        );
        assert!(result.is_err());
        assert!(!out.exists(), "partial output must be removed");
    }

    /// A compress failure (the dictionary magic alone does not parse) after
    /// the output file was created must not leave the output on disk.
    #[test]
    fn failed_compress_removes_partial_output() {
        let out = temp_path("compress-partial");
        let result = compress(
            Box::new(&b"payload"[..]),
            &Output::File(out.clone()),
            &Settings {
                level: 3,
                threads: 1,
                checksum: true,
                window_log: None,
                stream_size: None,
                size_hint: None,
                max_window: None,
            },
            0,
            Some(&[0x37, 0xa4, 0x30, 0xec]),
        );
        assert!(result.is_err());
        assert!(!out.exists(), "empty output must be removed");
    }

    /// Compressing to a sink (the stdout-shaped output) must report the real
    /// compressed byte count, matching a reference encoding of the same
    /// input and options.
    #[test]
    fn compress_to_sink_reports_compressed_bytes() {
        let data = vec![7u8; 300 * 1024];
        let written = compress(
            Box::new(Cursor::new(data.clone())),
            &Output::Sink,
            &Settings {
                level: 3,
                threads: 1,
                checksum: true,
                window_log: None,
                stream_size: None,
                size_hint: None,
                max_window: None,
            },
            data.len(),
            None,
        )
        .unwrap();
        assert!(written > 0, "reported compressed size must not be zero");

        let options =
            zstdx::EncoderOptions::new(Level::from_zstd(3)).pledged_size(Some(data.len() as u64));
        let mut reference = Vec::new();
        let mut encoder =
            zstdx::stream::write::Encoder::with_options(&mut reference, options).unwrap();
        encoder.write_all(&data).unwrap();
        encoder.finish().unwrap();
        assert_eq!(written, reference.len() as u64);
    }

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

    /// Arguments whose first byte is not ASCII must pass through untouched:
    /// slicing `s[1..]` on them panics inside a multibyte character
    /// (regression: `zstdx -d 中文文件.txt.zst` aborted with exit 101).
    #[test]
    fn non_ascii_args_pass_through() {
        let args = [
            OsString::from("zstdx"),
            OsString::from("-d"),
            OsString::from("中文文件.txt.zst"),
            OsString::from("-é3"),
        ];
        // nothing after argv[0] is a digit flag, so everything is unchanged
        assert_eq!(normalize_args(args.to_vec()), args.to_vec());
    }

    /// Boundary shapes of the `-<digits>` recognition.
    #[test]
    fn digit_flag_boundaries() {
        let normalize = |args: &[&str]| {
            normalize_args(
                std::iter::once("zstdx")
                    .chain(args.iter().copied())
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
            )
            .into_iter()
            .skip(1)
            .map(|arg| arg.into_string().unwrap())
            .collect::<Vec<_>>()
        };
        assert_eq!(normalize(&["-0"]), ["--level=0"]);
        assert_eq!(normalize(&["-1"]), ["--level=1"]);
        assert_eq!(normalize(&["-123"]), ["--level=123"]);
        // bare dash, a non-digit tail and a plain number stay untouched
        assert_eq!(normalize(&["-"]), ["-"]);
        assert_eq!(normalize(&["-12a"]), ["-12a"]);
        assert_eq!(normalize(&["12"]), ["12"]);
    }

    /// zstd's size syntax: bare digits are bytes, K/M/G are binary
    /// multipliers, i and B are optional decorations.
    #[test]
    fn sizes_parse_with_suffixes() {
        assert_eq!(parse_size("1000000").unwrap(), 1_000_000);
        assert_eq!(parse_size("1K").unwrap(), 1 << 10);
        assert_eq!(parse_size("512KB").unwrap(), 512 << 10);
        assert_eq!(parse_size("64MiB").unwrap(), 64 << 20);
        assert_eq!(parse_size("1G").unwrap(), 1 << 30);
        assert_eq!(parse_size("0").unwrap(), 0);
        for bad in ["", "K", "1Q", "1 KiB", "1B", "-5"] {
            assert!(parse_size(bad).is_err(), "{bad} must not parse");
        }
    }

    /// The auto rule needs a known size, an interactive stderr and no
    /// stdout output; --[no-]progress overrides it either way.
    #[test]
    fn progress_visibility_matrix() {
        let mut cli = plain_cli();
        assert!(progress_visible(&cli, true, false, true));
        assert!(!progress_visible(&cli, false, false, true), "unknown size");
        assert!(!progress_visible(&cli, true, false, false), "piped stderr");
        assert!(!progress_visible(&cli, true, true, true), "stdout output");

        cli.quiet = 1;
        assert!(!progress_visible(&cli, true, false, true), "quiet");
        cli.progress = true;
        assert!(progress_visible(&cli, true, true, false), "--progress wins");
        assert!(
            !progress_visible(&cli, false, true, false),
            "still needs size"
        );
        cli.no_progress = true;
        assert!(!progress_visible(&cli, true, false, true), "--no-progress");
    }

    /// The new flags must parse with their zstd spellings, including the
    /// `--file` alias of `--filelist`.
    #[test]
    fn parity_flags_parse() {
        let cli = Cli::try_parse_from([
            "zstdx",
            "--long=20",
            "--no-check",
            "--size-hint=4K",
            "--single-thread",
            "--no-progress",
            "--ignore-errors",
            "--file",
            "list.txt",
        ])
        .unwrap();
        assert_eq!(cli.long, Some(20));
        assert!(cli.no_check);
        assert_eq!(cli.size_hint, Some(4096));
        assert!(cli.single_thread);
        assert!(cli.no_progress);
        assert!(cli.ignore_errors);
        assert_eq!(cli.filelist, Some(PathBuf::from("list.txt")));

        let cli = Cli::try_parse_from(["zstdx", "--long"]).unwrap();
        assert_eq!(cli.long, Some(27), "bare --long defaults to 27");
        let cli = Cli::try_parse_from(["zstdx", "-t", "-M", "512K"]).unwrap();
        assert_eq!(cli.memory, Some(512 << 10));

        // --long stops at the engine's ceiling: larger values clamp in run,
        // smaller ones are rejected there; here only the parse matters.
        assert!(Cli::try_parse_from(["zstdx", "--long=31"]).is_ok());
        assert!(Cli::try_parse_from(["zstdx", "--long=9"]).is_ok());
    }

    /// The list/train surface parses with zstd's spellings, and the mode
    /// flags conflict with each other and the (de)compression modes.
    #[test]
    fn list_train_flags_parse() {
        let cli =
            Cli::try_parse_from(["zstdx", "--train", "-r", "samples", "--maxdict=16K"]).unwrap();
        assert!(cli.train);
        assert_eq!(cli.maxdict, Some(16 << 10));

        let cli = Cli::try_parse_from(["zstdx", "-l", "a.zst"]).unwrap();
        assert!(cli.list);
        let cli = Cli::try_parse_from(["zstdx", "--list", "a.zst"]).unwrap();
        assert!(cli.list);

        // the rejected --train knobs still parse (rejection happens in
        // run, with the reason)
        assert!(Cli::try_parse_from(["zstdx", "--train", "--train-cover", "s"]).is_ok());
        assert!(Cli::try_parse_from(["zstdx", "--train", "--train-cover=k=48,d=8", "s"]).is_ok());
        assert!(Cli::try_parse_from(["zstdx", "--train", "--dictID=5", "s"]).is_ok());
        // a value without `=` is a positional file: the flag itself still
        // carries the empty default and is rejected in run
        let cli = Cli::try_parse_from(["zstdx", "--train", "--train-legacy", "s"]).unwrap();
        assert_eq!(cli.train_legacy.as_deref(), Some(""));
        assert_eq!(cli.files, [PathBuf::from("s")]);

        for bad in [
            vec!["zstdx", "-l", "-d", "a.zst"],
            vec!["zstdx", "-l", "-t", "a.zst"],
            vec!["zstdx", "-l", "--train"],
            vec!["zstdx", "--train", "-d", "a.zst"],
        ] {
            assert!(
                Cli::try_parse_from(bad.as_slice()).is_err(),
                "{bad:?} must conflict"
            );
        }
    }
}
