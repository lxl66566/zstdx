extern crate zstdx;
mod adapt;
mod bench;
mod list;
mod patch;
mod progress;
mod train;

use std::{
    cell::Cell,
    ffi::OsString,
    fs::{self, File},
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
};

use clap::{ArgAction, CommandFactory, FromArgMatches, Parser};
use progress::{ProgressMonitor, fmt_size};
use tracing::info;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
use zstdx::{DecoderOptions, EncoderOptions, InputShape, Level};

/// Suffix added on compression and required (or stripped) on decompression.
const ZSTD_SUFFIX: &str = ".zst";
/// Tar-suffix zstd also strips on decompression (`.tzst` maps to `.tar`).
const TZSTD_SUFFIX: &str = ".tzst";
/// zstd's canonical stdin display name (fileio.h `stdinmark`): the literal
/// text its diagnostics print for `-`.
const STDIN_NAME: &str = "/*stdin*\\";
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
/// libzstd's format-level window log ceiling (`ZSTD_WINDOWLOG_MAX`), which
/// gates the extra `--long=`/`--memory=` hint line.
const ZSTD_WINDOWLOG_MAX: u32 = 30;
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

/// A fully rendered diagnostic: printed verbatim, one line per entry,
/// instead of the `zstdx: <input>: <message>` wrapper -- for the stderr
/// lines that mirror zstd's per-site formats byte for byte. `code` is
/// zstd's numeric exit status (fileio's EXM_THROW exits with the error
/// number); 0 means a plain failure (exit 1).
#[derive(Debug)]
struct Diagnostic {
    code: u8,
    lines: Vec<String>,
}

impl Diagnostic {
    fn plain(lines: Vec<String>) -> Self {
        Self { code: 0, lines }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.lines.join("\n"))
    }
}

impl std::error::Error for Diagnostic {}

/// zstd's exit statuses: fileio/dibio's EXM_THROW exits with the error
/// number (`Error`), every other failure exits 1, success 0.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Exit {
    Success,
    Failure,
    Error(u8),
}

impl From<bool> for Exit {
    fn from(ok: bool) -> Self {
        if ok {
            Exit::Success
        } else {
            Exit::Failure
        }
    }
}

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

/// strerror text of an io error: its Display without the `(os error N)`
/// suffix, matching the C library strings zstd prints.
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Validated, mode-scoped knobs for one invocation, derived once in `run`.
/// Every accepted flag lands in a field here; flags the engine cannot
/// honor are rejected in `run` instead, never silently dropped.
#[derive(Debug)]
struct Settings {
    level: i32,
    threads: u32,
    /// --adapt bounds: dynamic per-chunk level walking (None: off).
    adapt: Option<adapt::AdaptRange>,
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
    /// Whether the frame header declares the content size (--no-content-size
    /// clears it; the known size still reaches the row as a hint).
    content_size: bool,
    /// Whether the frame header declares the dictionary id (--no-dictID).
    include_dictid: bool,
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

    /// Benchmark files at compression level # (default 3); with `-d`,
    /// benchmark decompression only; with no FILES, a synthetic corpus
    #[arg(
        short = 'b',
        value_name = "#",
        conflicts_with_all = ["compress", "test", "list", "train", "output"]
    )]
    bench: Option<i32>,

    /// Benchmark all levels up to #; the starting level is `-b#` (bench
    /// only)
    #[arg(short = 'e', value_name = "#")]
    bench_end: Option<i32>,

    /// Minimum evaluation time of # seconds per phase (bench only)
    #[arg(short = 'i', value_name = "#")]
    bench_seconds: Option<u64>,

    /// Cut the input into independent blocks of size # (bench only; zstd
    /// 1.5.7 spells it -B only -- the --split long form is an unknown
    /// flag there)
    #[arg(short = 'B', value_name = "#", value_parser = parse_size)]
    block_size: Option<u64>,

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
    #[arg(long)]
    rm: bool,

    /// Overwrite existing output files without asking, and allow reading
    /// compressed input from / writing compressed output to a terminal
    #[arg(short = 'f', long)]
    force: bool,

    /// Recurse into directories given as FILES
    #[arg(short = 'r')]
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
    #[arg(short = 'D', value_name = "DICT")]
    dict: Option<PathBuf>,

    /// Compress/decompress as a diff against REF: compression treats REF
    /// as match history (a raw-content dictionary, no dictID in the
    /// frame), decompression needs the same REF. One input only, and
    /// stdin compression requires --stream-size; implies --ultra
    #[arg(long, value_name = "REF", conflicts_with_all = ["dict", "list", "train"])]
    patch_from: Option<PathBuf>,

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

    /// Dynamically adapt the compression level to I/O conditions
    /// (compression only): `min=`/`max=` bounds, comma-separated, either
    /// order, at least one (default 1-22, the engine's range; zstd's
    /// negative rungs do not exist here). The input compresses as
    /// independent 8 MiB chunk frames and a level step applies to every
    /// later chunk; `-T` workers apply within each chunk
    #[arg(
        long,
        value_name = "PARAMS",
        num_args = 0..=1,
        default_missing_value = "min=1,max=22",
        require_equals = true,
        value_parser = adapt::parse_range,
        conflicts_with_all = ["decompress", "test"]
    )]
    adapt: Option<adapt::AdaptRange>,

    /// Write a frame content checksum (the default)
    #[arg(short = 'C', long, conflicts_with = "no_check")]
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
    #[arg(
        short = 'M',
        long,
        alias = "memlimit",
        alias = "memlimit-decompress",
        value_name = "N",
        value_parser = parse_size
    )]
    memory: Option<u64>,

    /// Read the list of input files from LIST, one per line; a LIST of `-`
    /// reads the list itself from stdin
    #[arg(long, value_name = "LIST")]
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

    /// Removed in zstd 1.5.5+ (v1.5.7 rejects it as an incorrect
    /// parameter); kept parseable so the diagnostic matches zstd's
    #[arg(long, hide = true)]
    ignore_errors: bool,

    /// Copy inputs that are not zstd frames through unchanged instead of
    /// failing (decode modes only; default: on with -f when writing to
    /// stdout, gzip -df semantics)
    #[arg(long, conflicts_with = "no_pass_through")]
    pass_through: bool,

    /// Disable the pass-through default of -f with stdout output
    #[arg(long)]
    no_pass_through: bool,

    /// Omit the dictionary id from the frame header; the dictionary still
    /// applies in full, decoders must be given it explicitly (compression
    /// with -D or --patch-from)
    #[arg(long = "no-dictID")]
    no_dict_id: bool,

    /// Write the frame content size into the header (the default whenever
    /// the size is known)
    #[arg(long, conflicts_with = "no_content_size")]
    content_size: bool,

    /// Omit the frame content size from the header; a known size still
    /// shapes the encoder row as a hint
    #[arg(long)]
    no_content_size: bool,

    /// Skip files whose extension marks them as already compressed
    /// (.zst/.tzst plus the gz/xz/lz4 and media families; compression only)
    #[arg(long)]
    exclude_compressed: bool,

    /// Store outputs under their plain file name in DIR, which must exist;
    /// -o and stdout outputs are unaffected
    #[arg(long, value_name = "DIR")]
    output_dir_flat: Option<PathBuf>,

    /// Core-count basis for -T0: physical cores like zstd's default, or
    /// logical cores
    #[arg(long, hide = true, value_parser = ["physical", "logical"])]
    auto_threads: Option<String>,

    // Identity flags: zstd 1.5.7 accepts these and this build's behavior
    // with them is identical by construction, so they parse and do nothing.
    /// gzip-compat spelling: zstd stores no file name in frames either
    #[arg(short = 'n', hide = true)]
    no_name: bool,

    /// Async file IO request; zstd itself falls back to synchronous IO
    /// without an AIO backend, which is what this build always does
    #[arg(long, hide = true, conflicts_with = "no_asyncio")]
    asyncio: bool,

    /// See --asyncio
    #[arg(long, hide = true)]
    no_asyncio: bool,

    /// Dictionary memory-map request; dictionaries are read in full either
    /// way, which is observably identical
    #[arg(long, hide = true, conflicts_with = "no_mmap_dict")]
    mmap_dict: bool,

    /// See --mmap-dict
    #[arg(long, hide = true)]
    no_mmap_dict: bool,

    /// Select the zstd output format: the only format this build has, and
    /// the default (the reference without zlib/lzma/lz4 answers the other
    /// values as unknown flags)
    #[arg(long, hide = true, require_equals = true)]
    format: Option<String>,

    // Rejected flags: kept parseable so the diagnostic names the flag;
    // rejected in run() because the engine has no knob for them.
    /// Not supported: the engine has no rsync-friendly block splitting
    #[arg(long, hide = true)]
    rsyncable: bool,

    /// Not supported: literal entropy mode is chosen per block
    #[arg(long, hide = true)]
    compress_literals: bool,

    /// Not supported: see --compress-literals
    #[arg(long, hide = true)]
    no_compress_literals: bool,

    /// Not supported: the matchfinder is selected per level row
    #[arg(long, hide = true)]
    row_match_finder: bool,

    /// Not supported: see --row-match-finder
    #[arg(long, hide = true)]
    no_row_match_finder: bool,

    /// Not supported: no compressed-block-size target exists
    #[arg(
        long = "target-compressed-block-size",
        hide = true,
        value_name = "#",
        value_parser = parse_size,
        require_equals = true
    )]
    target_cblock_size: Option<u64>,

    /// Not supported: raw compression-parameter forcing is not exposed
    #[arg(
        long = "zstd",
        hide = true,
        value_name = "PARAMS",
        require_equals = true
    )]
    zstd_params: Option<String>,

    /// Not supported: needs raw compression-parameter forcing
    #[arg(long, hide = true)]
    max: bool,

    /// Not supported: the engine has no trace hooks
    #[arg(long, hide = true, value_name = "FILE")]
    trace: Option<PathBuf>,

    /// Not supported: see --trace
    #[arg(long, hide = true)]
    trace_file_stat: bool,

    /// Not supported: the engine's level parameters are not exposed for
    /// printing
    #[arg(long, hide = true)]
    show_default_cparams: bool,

    /// Not supported: no scheduler control is available
    #[arg(long, hide = true, require_equals = true)]
    priority: Option<String>,

    /// Not supported: the trainer has no selectivity knob
    #[arg(short = 's', hide = true, value_name = "#")]
    dict_select: Option<u32>,

    /// Not supported: the benchmark consolidates its files
    #[arg(short = 'S', hide = true)]
    separate_files: bool,

    /// Not supported: the pause hook is not available
    #[arg(short = 'p', hide = true)]
    pause: bool,

    /// Not supported: the synthetic benchmark corpus is the fixed lorem
    /// text
    #[arg(short = 'P', hide = true, value_name = "#")]
    compressibility: Option<u32>,

    /// Not supported: the console-detection test hooks are not available
    #[arg(long, hide = true)]
    fake_stdin_is_console: bool,

    /// Not supported: see --fake-stdin-is-console
    #[arg(long, hide = true)]
    fake_stdout_is_console: bool,

    /// Not supported: see --fake-stdin-is-console
    #[arg(long, hide = true)]
    fake_stderr_is_console: bool,

    /// Not supported: mirrored output directories are not implemented
    #[arg(long, hide = true, value_name = "DIR")]
    output_dir_mirror: Option<PathBuf>,

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

/// zstd's display name for an input source (`stdinmark` for stdin).
fn input_name(input: &Input) -> String {
    match input {
        Input::Stdin => STDIN_NAME.to_string(),
        Input::File(path) => path.display().to_string(),
    }
}

/// Where one input's output goes.
enum Output {
    Stdout,
    File(PathBuf),
    /// `Mode::Test`: decode and discard.
    Sink,
}

fn main() -> ExitCode {
    if let Some(code) = early_exit(std::env::args_os()) {
        return code;
    }
    let args = match normalize_args(std::env::args_os()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        },
    };
    // zstdcli.c assigns `removeSrcFile` per occurrence of --rm / -k/--keep:
    // the flag that appears last wins, so `--rm -k` keeps the input.
    let rm = rm_flag_last_wins(&args);
    let command = Cli::command();
    let matches = match command.try_get_matches_from(args) {
        Ok(matches) => matches,
        // zstdcli.c's parser: an unknown flag is `Incorrect parameter` and
        // exit 1, never clap's usage block (the usage text differs from
        // zstd's either way; zstd prints its own only at -v and up).
        Err(err) if err.kind() == clap::error::ErrorKind::UnknownArgument => {
            let arg = err
                .get(clap::error::ContextKind::InvalidArg)
                .and_then(|value| match value {
                    clap::error::ContextValue::String(text) => Some(text.clone()),
                    clap::error::ContextValue::Strings(list) => list.first().cloned(),
                    _ => None,
                });
            eprintln!(
                "Incorrect parameter: {} ",
                arg.as_deref().unwrap_or_default()
            );
            return ExitCode::FAILURE;
        },
        Err(err) => err.exit(),
    };
    let cli = Cli::from_arg_matches(&matches).expect("try_get_matches validated the args");
    init_logging(&cli);
    match run(&cli, rm) {
        Exit::Success => ExitCode::SUCCESS,
        Exit::Failure => ExitCode::FAILURE,
        Exit::Error(code) => ExitCode::from(code),
    }
}

/// Whether `--rm` or `-k/--keep` appeared last on the command line: clap's
/// occurrence indices carry default-value entries, so the raw argv is the
/// reliable witness. Bundled shorts (`-fk`) are clap's domain; a `k` inside
/// a bundle does not count here (zstdcli.c's char loop does count it).
fn rm_flag_last_wins(args: &[OsString]) -> bool {
    let mut rm = false;
    for arg in args.iter().skip(1) {
        if arg == "--" {
            break;
        }
        if arg == "--rm" {
            rm = true;
        } else if arg == "-k" || arg == "--keep" {
            rm = false;
        }
    }
    rm
}

/// The process exit status: `Exit::Error` where zstd's EXM_THROW exits
/// with the error number, `Exit::Failure` (exit 1) otherwise.
fn run(cli: &Cli, rm: bool) -> Exit {
    // zstd 1.5.7 removed --ignore-errors (v1.5.5 deprecation turned into
    // a hard parser rejection, before any mode runs).
    if cli.ignore_errors {
        eprintln!("Incorrect parameter: --ignore-errors ");
        return Exit::Failure;
    }

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
        return Exit::Failure;
    }
    if cli.dict_id.is_some() {
        eprintln!(
            "{PREFIX}--dictID is not supported: the trainer derives the dictionary ID from its \
             content"
        );
        return Exit::Failure;
    }
    if cli.train_cover.is_some() || cli.train_fastcover.is_some() || cli.train_legacy.is_some() {
        eprintln!(
            "{PREFIX}--train-cover/--train-fastcover/--train-legacy are not supported: the \
             in-tree trainer is the only algorithm"
        );
        return Exit::Failure;
    }
    if cli.maxdict.is_some() && mode != Mode::Train {
        eprintln!("{PREFIX}--maxdict only applies with --train");
        return Exit::Failure;
    }
    // Parser-shaped rejections come first: a value the reference's own
    // parser never spells is an unknown flag there.
    if let Some(value) = cli.format.as_deref().filter(|format| *format != "zstd") {
        eprintln!("Incorrect parameter: --format={value} ");
        return Exit::Failure;
    }
    if let Some(value) = cli.priority.as_deref().filter(|value| *value != "rt") {
        eprintln!("Incorrect parameter: --priority={value} ");
        return Exit::Failure;
    }
    // Flags with no engine knob reject loudly with their reason; the
    // identity flags never appear here (see their definitions).
    if let Some((flag, reason)) = unsupported_flag(cli) {
        eprintln!("{PREFIX}{flag} is not supported: {reason}");
        return Exit::Failure;
    }

    // Benchmark mode is self-contained: it gathers its own inputs and
    // rejects the flags it cannot honor (its own orphan knobs included).
    if cli.bench.is_some()
        || cli.bench_end.is_some()
        || cli.bench_seconds.is_some()
        || cli.block_size.is_some()
    {
        return bench::run(cli);
    }

    // The inspection and training modes are self-contained: they gather
    // their own inputs and never reach the (de)compression machinery.
    match mode {
        Mode::List => return list::run(cli).into(),
        Mode::Train => return train::run(cli),
        Mode::Compress | Mode::Decompress | Mode::Test => {},
    }
    if (cli.check || cli.no_check) && mode != Mode::Compress {
        eprintln!(
            "{PREFIX}--check/--no-check affect compression only: decoding always validates a \
             present checksum"
        );
        return Exit::Failure;
    }
    if cli.memory.is_some() && mode == Mode::Compress {
        eprintln!("{PREFIX}-M/--memory bounds decoding only; it does not limit compression");
        return Exit::Failure;
    }
    if (cli.pass_through || cli.no_pass_through) && mode == Mode::Compress {
        eprintln!("{PREFIX}--pass-through affects decoding only");
        return Exit::Failure;
    }
    if cli.memory.is_some_and(|limit| limit < MIN_WINDOW_BYTES) {
        eprintln!(
            "{PREFIX}-M values below {MIN_WINDOW_BYTES} reject every frame (the format's minimum \
             window); use at least 1K"
        );
        return Exit::Failure;
    }
    let window_log = match cli.long {
        None => None,
        // Below the format minimum zstd rejects with the engine parameter
        // error and its exit code.
        Some(log) if log < MIN_WINDOW_LOG => {
            eprintln!("{PREFIX}error 11 : Parameter is out of bound ");
            return Exit::Error(11);
        },
        // Above the maximum zstd clamps silently (to its 30); the engine
        // ceiling here is lower, still clamped silently.
        Some(log) if log > MAX_WINDOW_LOG => Some(MAX_WINDOW_LOG),
        log => log,
    };

    let threads = if cli.single_thread || cli.threads == 1 {
        1
    } else if cli.threads == 0 {
        // zstd's -T0: physical cores by default, --auto-threads=logical
        // switches the basis, announced at display level 3.
        let physical = cli.auto_threads.as_deref() != Some("logical");
        let count = if physical {
            count_physical_cores()
        } else {
            std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
        };
        if cli.verbose >= 1 {
            info!(
                "Note: {count} {} core(s) detected ",
                if physical {
                    "physical"
                } else {
                    "logical"
                }
            );
        }
        count
    } else {
        cli.threads
    };

    let mut level = cli.fast.map_or_else(
        || cli.level.unwrap_or(DEFAULT_LEVEL),
        |acceleration| -acceleration,
    );
    // --patch-from implies --ultra (zstdcli.c sets it with the flag).
    if mode == Mode::Compress
        && level > MAX_LEVEL_WITHOUT_ULTRA
        && !cli.ultra
        && cli.patch_from.is_none()
    {
        info!(
            "Warning: compression level higher than max, reduced to {MAX_LEVEL_WITHOUT_ULTRA} \
             (use --ultra to allow 20-22) "
        );
        level = MAX_LEVEL_WITHOUT_ULTRA;
    }

    // --adapt clamps the starting level into its bounds (zstdcli.c applies
    // the same two clamps after the ultra reduction).
    if let Some(range) = &cli.adapt {
        if range.min > level {
            level = range.min;
        }
        if range.max < level {
            level = range.max;
        }
    }

    // --patch-from loads its reference through the same raw-dictionary
    // path as -D (the flags conflict, so at most one is set).
    let dict = match cli.dict.as_ref().or(cli.patch_from.as_ref()) {
        Some(path) => match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                eprintln!(
                    "{PREFIX}error 31 : Stat failed on dictionary file {}: {} ",
                    path.display(),
                    strerror(&err)
                );
                return Exit::Error(31);
            },
        },
        None => None,
    };

    let filelist = if let Some(path) = &cli.filelist {
        let Ok(list) = read_filelist(path) else {
            eprintln!("{PREFIX}error reading {} ", path.display());
            return Exit::Failure;
        };
        list
    } else {
        Vec::new()
    };

    let mut inputs = Vec::new();
    let mut ok = true;
    if cli.files.is_empty() && filelist.is_empty() {
        inputs.push(Input::Stdin);
    } else {
        for file in cli.files.iter().chain(&filelist) {
            collect_input(
                file,
                cli.recursive,
                &mut inputs,
                &mut ok,
                InputGathering::Stat,
            );
        }
    }
    // zstdcli.c's post-expansion check: explicit inputs that expanded to
    // nothing (empty directories) decline with this line at display level 1
    // and still exit 0. A collection error already decided the failure.
    if inputs.is_empty() && ok {
        if cli.quiet < 2 {
            eprintln!("please provide correct input file(s) or non-empty directories -- ignored ");
        }
        return Exit::Success;
    }

    // Size flags shape the stdin stream's encoder row; a file input already
    // carries its exact size as the pledge, so honoring a second, looser
    // number would silently do nothing (zstd ignores it there).
    let stdin_stream = matches!(inputs.as_slice(), [Input::Stdin]);
    if cli.stream_size.is_some() && !stdin_stream {
        eprintln!("{PREFIX}--stream-size only applies to stdin input");
        return Exit::Failure;
    }
    if cli.adapt.is_some() && (cli.stream_size.is_some() || cli.size_hint.is_some()) {
        eprintln!(
            "{PREFIX}--stream-size/--size-hint cannot combine with --adapt: chunk frames pledge \
             their own sizes exactly"
        );
        return Exit::Failure;
    }
    if cli.size_hint.is_some() && !stdin_stream {
        eprintln!(
            "{PREFIX}--size-hint only applies to stdin input (file sizes are pledged exactly)"
        );
        return Exit::Failure;
    }

    if cli.output.is_some() && inputs.len() > 1 {
        eprintln!("{PREFIX}-o/--output cannot be used with multiple input files");
        return Exit::Failure;
    }

    // --patch-from derives its own window (overriding --long) and decode
    // bound; the reference's exact rules live in patch.rs.
    let (patch_window_log, patch_max_window) = match &cli.patch_from {
        Some(_) => match patch::adjust(cli, mode, &inputs, dict.as_deref()) {
            Ok(adjustments) => (adjustments.window_log, adjustments.max_window),
            Err(msg) => {
                eprintln!("{PREFIX}{msg}");
                return Exit::Failure;
            },
        },
        None => (None, None),
    };

    let settings = Settings {
        level,
        threads,
        adapt: cli.adapt,
        checksum: !cli.no_check,
        window_log: patch_window_log.or(window_log),
        stream_size: cli.stream_size,
        size_hint: cli.size_hint,
        max_window: patch_max_window.or(cli.memory),
        content_size: !cli.no_content_size,
        include_dictid: !cli.no_dict_id,
    };

    let stdout_output =
        cli.stdout || cli.output.is_none() && inputs.iter().any(|i| matches!(i, Input::Stdin));
    if rm && stdout_output {
        info!("Note: src files are not removed when output is stdout ");
    }

    let mut exit_code: Option<u8> = None;
    for input in &inputs {
        if let Err(err) = process(input, cli, mode, &settings, dict.as_deref(), rm) {
            let diagnostic = err.downcast_ref::<Diagnostic>();
            if cli.quiet < 2 {
                if let Some(diagnostic) = diagnostic {
                    for line in &diagnostic.lines {
                        eprintln!("{line}");
                    }
                } else {
                    eprintln!("{PREFIX}{}: {err}", input_name(input));
                }
            }
            // EXM_THROW-class failures decide the exit status even when
            // their lines are -qq-hidden; the first one wins (zstd exits
            // at the first).
            if let Some(code) = diagnostic
                .map(|diagnostic| diagnostic.code)
                .filter(|c| *c != 0)
            {
                exit_code.get_or_insert(code);
            }
            ok = false;
        }
    }
    if ok {
        Exit::Success
    } else {
        exit_code.map_or(Exit::Failure, Exit::Error)
    }
}

/// Read `--filelist`: one path per line, blank lines skipped. zstd has no
/// stdin file-list form: `-` is a literal path there (and fails as
/// `error reading -`), so it is here too.
fn read_filelist(path: &Path) -> AnyResult<Vec<PathBuf>> {
    let reader = BufReader::new(File::open(path)?);
    let mut paths = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.is_empty() {
            paths.push(PathBuf::from(line));
        }
    }
    Ok(paths)
}

/// Whether the metadata is a named pipe, one of zstd's accepted non-regular
/// input kinds (Unix-only; other platforms have no FIFOs).
#[cfg(unix)]
fn is_fifo(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    meta.file_type().is_fifo()
}

#[cfg(not(unix))]
fn is_fifo(_meta: &fs::Metadata) -> bool {
    false
}

/// The first flag on the command line the engine cannot honor, as its
/// display form and the rejection reason.
fn unsupported_flag(cli: &Cli) -> Option<(&'static str, &'static str)> {
    [
        (
            cli.rsyncable,
            "--rsyncable",
            "the engine has no rsync-friendly block splitting",
        ),
        (
            cli.compress_literals,
            "--compress-literals",
            "literal entropy mode is chosen per block by the encoder",
        ),
        (
            cli.no_compress_literals,
            "--no-compress-literals",
            "literal entropy mode is chosen per block by the encoder",
        ),
        (
            cli.row_match_finder,
            "--row-match-finder",
            "the matchfinder is selected per level row",
        ),
        (
            cli.no_row_match_finder,
            "--no-row-match-finder",
            "the matchfinder is selected per level row",
        ),
        (
            cli.target_cblock_size.is_some(),
            "--target-compressed-block-size",
            "no compressed-block-size target exists",
        ),
        (
            cli.zstd_params.is_some(),
            "--zstd=",
            "raw compression-parameter forcing is not exposed",
        ),
        (
            cli.max,
            "--max",
            "maximum-parameter compression needs raw parameter forcing",
        ),
        (
            cli.trace.is_some(),
            "--trace",
            "the engine has no trace hooks",
        ),
        (
            cli.trace_file_stat,
            "--trace-file-stat",
            "the engine has no trace hooks",
        ),
        (
            cli.show_default_cparams,
            "--show-default-cparams",
            "the engine's level parameters are not exposed for printing",
        ),
        (
            cli.priority.is_some(),
            "--priority=rt",
            "no scheduler control is available",
        ),
        (
            cli.dict_select.is_some(),
            "-s",
            "the trainer has no selectivity knob",
        ),
        (
            cli.separate_files,
            "-S",
            "the benchmark consolidates its files",
        ),
        (cli.pause, "-p", "the pause hook is not available"),
        (
            cli.compressibility.is_some(),
            "-P",
            "the synthetic benchmark corpus is the fixed lorem text",
        ),
        (
            cli.fake_stdin_is_console,
            "--fake-stdin-is-console",
            "the console-detection test hooks are not available",
        ),
        (
            cli.fake_stdout_is_console,
            "--fake-stdout-is-console",
            "the console-detection test hooks are not available",
        ),
        (
            cli.fake_stderr_is_console,
            "--fake-stderr-is-console",
            "the console-detection test hooks are not available",
        ),
        (
            cli.output_dir_mirror.is_some(),
            "--output-dir-mirror",
            "mirrored output directories are not implemented",
        ),
    ]
    .into_iter()
    .find(|(set, ..)| *set)
    .map(|(_, flag, reason)| (flag, reason))
}

/// Distinct (physical id, core id) pairs from /proc/cpuinfo, zstd's
/// UTIL_countPhysicalCores rule for -T0 and --auto-threads=physical; the
/// logical core count stands in when the file is missing or empty-handed.
fn count_physical_cores() -> u32 {
    let logical = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let Ok(text) = fs::read_to_string("/proc/cpuinfo") else {
        return logical;
    };
    let mut cores = std::collections::HashSet::new();
    let (mut physical, mut core) = (None, None);
    for line in text.lines() {
        let value = || -> Option<u32> { line.split_once(':')?.1.trim().parse().ok() };
        if line.starts_with("physical id") {
            physical = value();
        } else if line.starts_with("core id") {
            core = value();
        } else if line.is_empty() {
            if let (Some(p), Some(c)) = (physical, core) {
                cores.insert((p, c));
            }
            physical = None;
            core = None;
        }
    }
    if let (Some(p), Some(c)) = (physical, core) {
        cores.insert((p, c));
    }
    if cores.is_empty() {
        logical
    } else {
        cores.len() as u32
    }
}

/// fileio.c's compressedFileExtensions table, verbatim: --exclude-compressed
/// skips an input whose name ends in one of these.
const COMPRESSED_EXTENSIONS: &[&str] = &[
    ".zst", ".tzst", ".gz", ".tgz", ".lzma", ".xz", ".txz", ".lz4", ".tlz4", ".7z", ".aa3", ".aac",
    ".aar", ".ace", ".alac", ".ape", ".apk", ".apng", ".arc", ".archive", ".arj", ".ark", ".asf",
    ".avi", ".avif", ".ba", ".br", ".bz2", ".cab", ".cdx", ".chm", ".cr2", ".divx", ".dmg", ".dng",
    ".docm", ".docx", ".dotm", ".dotx", ".dsft", ".ear", ".eftx", ".emz", ".eot", ".epub", ".f4v",
    ".flac", ".flv", ".gho", ".gif", ".gifv", ".gnp", ".iso", ".jar", ".jpeg", ".jpg", ".jxl",
    ".lz", ".lzh", ".m4a", ".m4v", ".mkv", ".mov", ".mp2", ".mp3", ".mp4", ".mpa", ".mpc", ".mpe",
    ".mpeg", ".mpg", ".mpl", ".mpv", ".msi", ".odp", ".ods", ".odt", ".ogg", ".ogv", ".otp",
    ".ots", ".ott", ".pea", ".png", ".pptx", ".qt", ".rar", ".s7z", ".sfx", ".sit", ".sitx",
    ".sqx", ".svgz", ".swf", ".tbz2", ".tib", ".tlz", ".vob", ".war", ".webm", ".webp", ".wma",
    ".wmv", ".woff",
];

/// Whether the name's extension is one of fileio's compressed extensions
/// (its UTIL_getFileExtension rule: the last dot's tail, and a dot at
/// position zero is no extension).
fn is_compressed_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match name.rfind('.') {
        Some(at) if at > 0 => COMPRESSED_EXTENSIONS.contains(&&name[at..]),
        _ => false,
    }
}

/// How [`collect_input`] vets each path: the (de)compression modes stat up
/// front and report unreachable inputs with zstd's collection diagnostics;
/// the inspector (`-l`) and trainer take raw paths and print their own
/// per-file errors instead.
#[derive(Copy, Clone, PartialEq, Eq)]
enum InputGathering {
    Stat,
    Raw,
}

/// Gather the file list, expanding directories when `recursive` is set.
fn collect_input(
    path: &Path,
    recursive: bool,
    inputs: &mut Vec<Input>,
    ok: &mut bool,
    gather: InputGathering,
) {
    if path.as_os_str() == "-" {
        inputs.push(Input::Stdin);
        return;
    }
    if gather == InputGathering::Raw {
        if path.is_dir() && recursive {
            expand_dir(path, inputs, ok);
        } else {
            inputs.push(Input::File(path.to_path_buf()));
        }
        return;
    }
    match fs::metadata(path) {
        Err(err) => {
            eprintln!(
                "{PREFIX}can't stat {} : {} -- ignored ",
                path.display(),
                strerror(&err)
            );
            *ok = false;
        },
        Ok(meta) if meta.is_dir() => {
            if !recursive {
                eprintln!("{PREFIX}{} is a directory -- ignored ", path.display());
                *ok = false;
                return;
            }
            expand_dir(path, inputs, ok);
        },
        // zstd accepts regular files and FIFOs as inputs; anything else
        // (sockets, devices) is skipped with this diagnostic.
        Ok(meta) if !meta.is_file() && !is_fifo(&meta) => {
            eprintln!(
                "{PREFIX}{} is not a regular file -- ignored ",
                path.display()
            );
            *ok = false;
        },
        Ok(_) => inputs.push(Input::File(path.to_path_buf())),
    }
}

/// Walk a directory tree, collecting every non-directory entry.
fn expand_dir(path: &Path, inputs: &mut Vec<Input>, ok: &mut bool) {
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
}

/// Whether the progress bar may draw for one input. `--[no-]progress`
/// overrides the auto rule (interactive stderr, not quiet, no stdout
/// output); every form still needs a known input size, so stdin streams
/// never show a bar.
// Three independent CLI-state facts; a bundling struct would just relay
// them one-to-one from the (also bool-flagged) call sites.
#[allow(clippy::fn_params_excessive_bools)]
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
    rm: bool,
) -> AnyResult<()> {
    // --exclude-compressed: an input whose extension marks it as already
    // compressed is skipped silently (zstd notes it at display level 4).
    if mode == Mode::Compress
        && cli.exclude_compressed
        && let Input::File(path) = input
        && is_compressed_name(path)
    {
        return Ok(());
    }
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
                let mut out = default_output_name(path, mode)?;
                // --output-dir-flat relocates derived names under DIR.
                if let Some(dir) = &cli.output_dir_flat
                    && let Some(name) = out.file_name()
                {
                    out = dir.join(name);
                }
                Output::File(out)
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
        return Err(Diagnostic::plain(vec![
            "{PREFIX}Refusing to open an output file which will overwrite the input file ".into(),
        ])
        .into());
    }
    // The overwrite check guards every file output, no matter where the
    // input comes from: piped stdin clobbers an `-o` target just as well as
    // a file input does (the same-file check above is inherently File-only).
    if let Output::File(out_path) = &output {
        check_overwrite(out_path, cli)?;
    }

    let (reader, source_size) = open_input(input)?;
    let read_bytes = Rc::new(Cell::new(0u64));
    let reader = CountingReader::new(reader, Rc::clone(&read_bytes));
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
        Mode::Compress => match compress(reader, &output, settings, source_size, dict) {
            Ok(written) => written,
            // A --stream-size pledge the input did not meet is zstd's
            // parameter-class Src size error (and exit code).
            Err(err) if pledged_size_mismatch(err.as_ref()) => {
                return Err(Diagnostic {
                    code: 11,
                    lines: vec![format!("{PREFIX}error 11 : Src size is incorrect ")],
                }
                .into())
            },
            Err(err) => return Err(err),
        },
        // List and train are dispatched before per-input processing and
        // never reach this point.
        Mode::Decompress | Mode::Test => {
            // fileio's pass-through default: overwrite flag set and the
            // destination is stdout.
            let pass_through = cli.pass_through
                || !cli.no_pass_through && cli.force && matches!(output, Output::Stdout);
            let name = input_name(input);
            match decompress(reader, &output, settings, dict, &name, pass_through) {
                Ok(written) => written,
                Err(err) => {
                    let mapped = decode_failure_lines(input, read_bytes.get(), err.as_ref())
                        .map_or_else(|| err, |lines| Diagnostic::plain(lines).into());
                    return Err(mapped);
                },
            }
        },
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

    if rm
        && matches!(output, Output::File(_))
        && let Input::File(path) = input
    {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// libzstd's output naming: compress appends `.zst`, decompress requires
/// and strips it (or `.tzst`, mapping to `.tar`).
fn default_output_name(input: &Path, mode: Mode) -> AnyResult<PathBuf> {
    match mode {
        Mode::Compress => Ok(add_extension(input, ZSTD_SUFFIX)),
        // List and train never reach name derivation (dispatched earlier).
        Mode::Decompress | Mode::Test | Mode::List | Mode::Train => {
            let name = input.to_str().and_then(|s| {
                s.strip_suffix(ZSTD_SUFFIX)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        s.strip_suffix(TZSTD_SUFFIX)
                            .map(|stem| format!("{stem}.tar"))
                    })
            });
            match name {
                Some(name) => Ok(PathBuf::from(name)),
                None => Err(Diagnostic::plain(vec![format!(
                    "{PREFIX}{}: unknown suffix (.zst/.tzst expected). Can't derive the output \
                     file name. Specify it with -o dstFileName. Ignoring.",
                    input.display()
                )])
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
    // zstd's displayLevel<=1 path: no interaction is possible, so the file
    // is declined without a prompt.
    if cli.quiet >= 1 {
        return Err(Diagnostic::plain(vec![format!(
            "{PREFIX}{} already exists; not overwritten  ",
            path.display()
        )])
        .into());
    }
    // zstd's interactive path: prompt on stderr, answer from the
    // controlling terminal (never stdin, which carries the payload); a
    // miss (no terminal, EOF, non-y) declines.
    eprint!(
        "{PREFIX}{} already exists; overwrite (y/n) ? ",
        path.display()
    );
    io::stderr().flush()?;
    if let Ok(mut tty) = File::open("/dev/tty").map(BufReader::new) {
        let mut answer = String::new();
        if tty.read_line(&mut answer).is_ok() && answer.trim().starts_with(['y', 'Y']) {
            return Ok(());
        }
    }
    eprintln!("Not overwritten  ");
    Err(Diagnostic::plain(vec![]).into())
}

/// Render a decode failure as zstd's per-site stderr lines, when the engine
/// error maps onto one of zstd's categories: the literal-(36) Decoding
/// error lines (libzstd's error names), premature-end read errors, and the
/// nothing-read special case for empty sources.
fn decode_failure_lines(
    input: &Input,
    read_bytes: u64,
    err: &(dyn std::error::Error + 'static),
) -> Option<Vec<String>> {
    use zstdx::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};

    // Construction failures surface as the bare engine error; read-pump
    // failures ride inside an io::Error payload.
    fn as_frame(payload: &zstdx::Error) -> Option<&FrameDecoderError> {
        match payload {
            zstdx::Error::Frame(frame) => Some(frame),
            _ => None,
        }
    }
    let frame = if let Some(error) = err.downcast_ref::<zstdx::Error>() {
        as_frame(error)?
    } else {
        let io_err = err.downcast_ref::<io::Error>()?;
        as_frame(io_err.get_ref()?.downcast_ref::<zstdx::Error>()?)?
    };
    let name = input_name(input);
    let lines = match frame {
        FrameDecoderError::ChecksumMismatch { .. } => vec![format!(
            "{name} : Decoding error (36) : Restored data doesn't match checksum "
        )],
        FrameDecoderError::ContentSizeMismatch { .. } => vec![format!(
            "{name} : Decoding error (36) : Src size is incorrect "
        )],
        FrameDecoderError::WindowSizeTooBig { requested, max } => {
            // FIO_zstdErrorHelp: the sizes line always, then the hint when
            // the frame's window log fits the format maximum.
            let mut lines = vec![
                format!(
                    "{name} : Decoding error (36) : Frame requires too much memory for decoding "
                ),
                format!("{name} : Window size larger than maximum : {requested} > {max} "),
            ];
            let log = 63 - requested.leading_zeros() + u32::from(requested & (requested - 1) != 0);
            if log <= ZSTD_WINDOWLOG_MAX {
                let mb = (requested >> 20) + u64::from(requested & ((1 << 20) - 1) != 0);
                lines.push(format!("{name} : Use --long={log} or --memory={mb}MB "));
            } else {
                lines.push(format!(
                    "{name} : Window log larger than ZSTD_WINDOWLOG_MAX={ZSTD_WINDOWLOG_MAX}; not \
                     supported "
                ));
            }
            lines
        },
        FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::BadMagicNumber(_)) => {
            vec![format!("{PREFIX}{name}: unsupported format ")]
        },
        FrameDecoderError::DictNotProvided { .. } | FrameDecoderError::DictionaryDecodeError(_) => {
            vec![format!(
                "{name} : Decoding error (36) : Dictionary mismatch "
            )]
        },
        // FIO_decompressDstFile's frame loop: a magic read that hit EOF
        // never saw a full magic number -- zero bytes is an empty source,
        // 1-3 bytes cannot even be checked for magic.
        FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::MagicNumberReadError(
            err,
        )) if err.kind() == io::ErrorKind::UnexpectedEof => {
            if read_bytes == 0 {
                vec![format!("{PREFIX}{name}: unexpected end of file ")]
            } else {
                vec![format!("{PREFIX}{name}: unknown header ")]
            }
        },
        frame if frame_eof(frame) => {
            vec![format!("{name} : Read error (39) : premature end ")]
        },
        _ => vec![format!(
            "{name} : Decoding error (36) : Data corruption detected "
        )],
    };
    Some(lines)
}

/// Whether the failure is the input running out mid-frame: an io
/// UnexpectedEof, or a section header claiming more bytes than the
/// truncated stream still holds (zstd's streamer reports both as a
/// premature end).
fn frame_eof(frame: &zstdx::decoding::errors::FrameDecoderError) -> bool {
    use zstdx::decoding::errors::{
        BlockHeaderReadError, DecodeBlockContentError, DecompressBlockError, FrameDecoderError,
        ReadFrameHeaderError,
    };

    let io_eof = |err: &io::Error| err.kind() == io::ErrorKind::UnexpectedEof;
    match frame {
        FrameDecoderError::ReadFrameHeaderError(
            ReadFrameHeaderError::MagicNumberReadError(err)
            | ReadFrameHeaderError::FrameDescriptorReadError(err)
            | ReadFrameHeaderError::WindowDescriptorReadError(err)
            | ReadFrameHeaderError::DictionaryIdReadError(err)
            | ReadFrameHeaderError::FrameContentSizeReadError(err),
        )
        | FrameDecoderError::FailedToReadChecksum(err)
        | FrameDecoderError::FailedToDrainDecodebuffer(err) => io_eof(err),
        FrameDecoderError::FailedToReadBlockHeader(BlockHeaderReadError::ReadError(err)) => {
            io_eof(err)
        },
        FrameDecoderError::FailedToReadBlockBody(
            DecodeBlockContentError::ReadError { source, .. }
            | DecodeBlockContentError::DecompressBlockError(
                DecompressBlockError::BlockContentReadError(source),
            ),
        ) => io_eof(source),
        // Section headers that ran past the end of a truncated stream, or a
        // skippable frame whose declared content ran past the input.
        FrameDecoderError::FailedToReadBlockBody(
            DecodeBlockContentError::DecompressBlockError(
                DecompressBlockError::MalformedSectionHeader { .. },
            ),
        )
        | FrameDecoderError::FailedToSkipFrame => true,
        _ => false,
    }
}

/// Input-side byte counter for zstd's `readSomething` rule: whether the
/// source yielded any byte at all decides the empty-source diagnostics,
/// and 1-3 bytes mean the magic could not even be checked (stdin has no
/// stat-able size, so the runtime count is the only witness).
struct CountingReader<R: Read> {
    inner: R,
    count: Rc<Cell<u64>>,
}

impl<R: Read> CountingReader<R> {
    fn new(inner: R, count: Rc<Cell<u64>>) -> Self {
        Self { inner, count }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.count.set(self.count.get() + read as u64);
        Ok(read)
    }
}

/// Whether the failure is the encoder rejecting a --stream-size pledge
/// the input did not meet (both error-chain shapes: bare and io-wrapped).
fn pledged_size_mismatch(err: &(dyn std::error::Error + 'static)) -> bool {
    fn is_mismatch(payload: &zstdx::Error) -> bool {
        matches!(payload, zstdx::Error::PledgedSizeMismatch { .. })
    }
    if let Some(error) = err.downcast_ref::<zstdx::Error>() {
        return is_mismatch(error);
    }
    err.downcast_ref::<io::Error>()
        .and_then(|io_err| io_err.get_ref())
        .and_then(|payload| payload.downcast_ref::<zstdx::Error>())
        .is_some_and(is_mismatch)
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
    if settings.adapt.is_some() {
        return adapt::compress(reader, output, settings, dict);
    }
    let mut options =
        EncoderOptions::new(Level::from_zstd(settings.level)).checksum(settings.checksum);
    // A file input pledges its metadata size; a stdin stream pledges
    // --stream-size or nothing. --no-content-size keeps the size out of the
    // header while it still shapes the row through the hint.
    let known_size = settings
        .stream_size
        .or((source_size > 0).then_some(source_size as u64));
    options = options.pledged_size(settings.content_size.then_some(known_size).flatten());
    // --long forces the window, --size-hint sizes the row; a known pledge
    // still shrinks the window to the source inside the engine, as zstd
    // does.
    let mut shape = InputShape::default();
    if let Some(log) = settings.window_log {
        shape = shape.with_window_log(log);
    }
    let hint = settings
        .size_hint
        .or((!settings.content_size).then_some(known_size).flatten());
    if let Some(hint) = hint {
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
    if !settings.include_dictid {
        options = options.include_dictid(false);
    }

    // A suppressed --stream-size pledge still binds: the read byte count is
    // checked here, with the encoder's own mismatch diagnostic.
    if !settings.content_size
        && let Some(pledged) = settings.stream_size
    {
        let count = Rc::new(Cell::new(0u64));
        let reader = Box::new(CountingReader::new(reader, Rc::clone(&count)));
        return compress_counted(reader, output, options, pledged, &count);
    }

    let (writer, out_file) = open_output(output)?;
    // The whole write phase runs in a closure so the output handles are
    // dropped before the partial output is removed on failure.
    let written: AnyResult<u64> = (|| {
        if let Some(file) = out_file {
            let mut encoder = zstdx::stream::write::Encoder::with_options(writer, options)?;
            io::copy(&mut reader, &mut encoder)?;
            encoder.finish()?;
            Ok(file.metadata()?.len())
        } else {
            // stdout and the test-mode sink have no file to stat; count the
            // bytes pushed through the writer instead
            let mut out = CountingWriter::new(writer);
            let mut encoder = zstdx::stream::write::Encoder::with_options(&mut out, options)?;
            io::copy(&mut reader, &mut encoder)?;
            encoder.finish()?;
            Ok(out.count)
        }
    })();
    if written.is_err() {
        remove_partial_output(output);
    }
    written
}

/// The counted tail of [`compress`] for a suppressed --stream-size pledge:
/// the encoder carries no pledge, so the mismatch check runs here on the
/// read counter and fails with the engine's error-11 line.
fn compress_counted(
    mut reader: Box<dyn Read>,
    output: &Output,
    options: EncoderOptions,
    pledged: u64,
    count: &Rc<Cell<u64>>,
) -> AnyResult<u64> {
    let (writer, out_file) = open_output(output)?;
    let written: AnyResult<u64> = (|| {
        if let Some(file) = out_file {
            let mut encoder = zstdx::stream::write::Encoder::with_options(writer, options)?;
            io::copy(&mut reader, &mut encoder)?;
            encoder.finish()?;
            Ok(file.metadata()?.len())
        } else {
            let mut out = CountingWriter::new(writer);
            let mut encoder = zstdx::stream::write::Encoder::with_options(&mut out, options)?;
            io::copy(&mut reader, &mut encoder)?;
            encoder.finish()?;
            Ok(out.count)
        }
    })();
    if written.is_err() {
        remove_partial_output(output);
        return written;
    }
    if count.get() != pledged {
        remove_partial_output(output);
        return Err(Diagnostic {
            code: 11,
            lines: vec![format!("{PREFIX}error 11 : Src size is incorrect ")],
        }
        .into());
    }
    written
}

/// Returns the number of bytes written.
///
/// The frame loop mirrors fileio's `FIO_decompressFrames`: each iteration
/// peeks the next magic number, skips skippable frames, and decodes one
/// data frame. A non-frame remainder is an error -- unless pass-through is
/// armed, which copies it out verbatim (gzip -df semantics) instead.
fn decompress(
    mut source: Box<dyn Read>,
    output: &Output,
    settings: &Settings,
    dict: Option<&[u8]>,
    name: &str,
    pass_through: bool,
) -> AnyResult<u64> {
    let (mut writer, _out_file) = open_output(output)?;
    // The whole write phase runs in a closure so the output handles are
    // dropped before the partial output is removed on failure.
    let written: AnyResult<u64> = (|| {
        let mut total = 0u64;
        let mut read_something = false;
        loop {
            let magic = match peek_magic(source.as_mut())? {
                // fileio's `readSomething` rule: a source that yielded no
                // byte at all is empty (invalid), a partial magic is an
                // unchecked header.
                Peeked::Eof if !read_something => {
                    return Err(Diagnostic::plain(vec![format!(
                        "{PREFIX}{name}: unexpected end of file "
                    )])
                    .into());
                },
                Peeked::Eof => break,
                Peeked::Short { bytes, len } => {
                    if pass_through {
                        total += copy_through(&bytes[..len], source.as_mut(), writer.as_mut())?;
                        break;
                    }
                    return Err(
                        Diagnostic::plain(vec![format!("{PREFIX}{name}: unknown header ")]).into(),
                    );
                },
                Peeked::Magic(magic) => magic,
            };
            read_something = true;
            if is_skippable(magic) {
                skip_skippable(source.as_mut())?;
                continue;
            }
            if magic != ZSTD_MAGIC {
                // fileio checks the foreign-format magics before the
                // pass-through arm; this build decodes none of them (like
                // a libzstd-free zstd), so each refuses with its own line.
                if let Some(format) = foreign_format(magic) {
                    return Err(Diagnostic::plain(vec![format!(
                        "{PREFIX}{name}: {} file cannot be uncompressed (zstd compiled without \
                         HAVE_{}) -- ignored ",
                        format.name(),
                        format.missing_lib()
                    )])
                    .into());
                }
                if pass_through {
                    total += copy_through(&magic, source.as_mut(), writer.as_mut())?;
                    break;
                }
                return Err(
                    Diagnostic::plain(vec![format!("{PREFIX}{name}: unsupported format ")]).into(),
                );
            }
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
            let framed = magic.as_slice().chain(source.as_mut());
            let mut frame =
                zstdx::stream::read::Decoder::with_options(framed, options)?.single_frame();
            total += io::copy(&mut frame, writer.as_mut())?;
        }
        writer.flush()?;
        Ok(total)
    })();
    if written.is_err() {
        remove_partial_output(output);
    }
    written
}

/// A zstd data frame's magic number as serialized (little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Whether a peeked magic number opens a skippable frame
/// (0x184D2A50-0x184D2A5F little-endian).
fn is_skippable(magic: [u8; 4]) -> bool {
    u32::from_le_bytes(magic) & 0xffff_fff0 == 0x184d_2a50
}

/// A foreign compressed format fileio recognizes by magic but this build
/// cannot decode.
#[derive(Copy, Clone)]
enum ForeignFormat {
    Gzip,
    XzLzma,
    Lz4,
}

impl ForeignFormat {
    /// The name fileio's refusal line prints.
    fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::XzLzma => "xz/lzma",
            Self::Lz4 => "lz4",
        }
    }

    /// The library whose absence the line blames.
    fn missing_lib(self) -> &'static str {
        match self {
            Self::Gzip => "ZLIB",
            Self::XzLzma => "LZMA",
            Self::Lz4 => "LZ4",
        }
    }
}

/// Whether a peeked magic opens one of fileio's foreign formats, checked
/// before pass-through (and before the unsupported-format refusal).
fn foreign_format(magic: [u8; 4]) -> Option<ForeignFormat> {
    match magic {
        [0x1f, 0x8b, ..] => Some(ForeignFormat::Gzip),
        [0xfd, 0x37, ..] | [0x5d, 0x00, ..] => Some(ForeignFormat::XzLzma),
        [0x04, 0x22, 0x4d, 0x18] => Some(ForeignFormat::Lz4),
        _ => None,
    }
}

/// What the frame loop's magic peek saw.
enum Peeked {
    /// The source ended before its first byte.
    Eof,
    /// The source ended mid-magic: `len` of `bytes` (1-3) are valid.
    Short { bytes: [u8; 4], len: usize },
    /// Four bytes of magic.
    Magic([u8; 4]),
}

/// Fill up to four bytes for the next frame's magic number. Read failures
/// replay as the decoder's magic-read error so the caller's diagnostics
/// keep the engine's shape.
fn peek_magic(source: &mut dyn Read) -> AnyResult<Peeked> {
    let mut magic = [0u8; 4];
    let mut filled = 0;
    while filled < magic.len() {
        match source.read(&mut magic[filled..]) {
            Ok(0) => {
                return Ok(if filled == 0 {
                    Peeked::Eof
                } else {
                    Peeked::Short {
                        bytes: magic,
                        len: filled,
                    }
                });
            },
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
            Err(err) => return Err(magic_read_error(err)),
        }
    }
    Ok(Peeked::Magic(magic))
}

/// Consume a skippable frame behind its peeked magic: the 4-byte content
/// size, then the content itself. An exhausted source mid-frame surfaces as
/// the engine's skip error (zstd's premature end).
fn skip_skippable(source: &mut dyn Read) -> AnyResult<()> {
    let mut header = [0u8; 4];
    if fill(source, &mut header).is_err() {
        return Err(skip_eof_error());
    }
    let mut remaining = u32::from_le_bytes(header) as u64;
    let mut trash = [0u8; 8 * 1024];
    while remaining > 0 {
        let take = (remaining.min(trash.len() as u64)) as usize;
        if fill(source, &mut trash[..take]).is_err() {
            return Err(skip_eof_error());
        }
        remaining -= take as u64;
    }
    Ok(())
}

/// An input that ran out inside a skippable frame: the engine's skip error,
/// which the stderr mapping reports as zstd's premature end.
fn skip_eof_error() -> Box<dyn std::error::Error> {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        zstdx::Error::Frame(zstdx::decoding::errors::FrameDecoderError::FailedToSkipFrame),
    )
    .into()
}

/// Read exactly `buf.len()` bytes; an exhausted source surfaces as the
/// decoder's magic-read error.
fn fill(source: &mut dyn Read, buf: &mut [u8]) -> AnyResult<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(magic_read_error(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected end of file",
                )))
            },
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {},
            Err(err) => return Err(magic_read_error(err)),
        }
    }
    Ok(filled)
}

/// Wrap a frame-boundary read failure as the decoder's magic-read error, so
/// the per-site stderr mapping treats it like the engine would.
fn magic_read_error(err: io::Error) -> Box<dyn std::error::Error> {
    io::Error::new(
        err.kind(),
        zstdx::Error::Frame(
            zstdx::decoding::errors::FrameDecoderError::ReadFrameHeaderError(
                zstdx::decoding::errors::ReadFrameHeaderError::MagicNumberReadError(err),
            ),
        ),
    )
    .into()
}

/// Copy a pass-through remainder out verbatim: the peeked bytes, then the
/// rest of the source (`FIO_passThrough`).
fn copy_through(head: &[u8], source: &mut dyn Read, writer: &mut dyn Write) -> AnyResult<u64> {
    writer.write_all(head)?;
    let mut total = io::copy(source, writer)?;
    total += head.len() as u64;
    Ok(total)
}

/// Open the output writer; the `File` is kept alongside so its final size can
/// be reported and stdout/sink outputs share the type.
fn open_output(output: &Output) -> AnyResult<(Box<dyn Write>, Option<File>)> {
    match output {
        Output::Stdout => Ok((Box::new(io::stdout().lock()), None)),
        Output::Sink => Ok((Box::new(io::sink()), None)),
        Output::File(path) => {
            // FIO_openDstFile's refusal: the destination path with
            // strerror, no input-name prefix.
            let file = File::create(path).map_err(|err| {
                Diagnostic::plain(vec![format!(
                    "{PREFIX}{}: {}",
                    path.display(),
                    strerror(&err)
                )])
            })?;
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

/// Flags whose value zstd takes as a separate argv entry through its
/// NEXT_FIELD macro: a following entry starting with `-` (the stdin mark
/// included) is a hard parse error there, never a value.
const SEPARATE_VALUE_FLAGS: &[&str] = &[
    "-b",
    "-e",
    "-i",
    "-B",
    "-o",
    "--output",
    "-D",
    "-T",
    "--threads",
    "-M",
    "--memory",
    "--memlimit",
    "--memlimit-decompress",
    "--filelist",
    "--patch-from",
    "--stream-size",
    "--size-hint",
    "--maxdict",
    "--dictID",
    "--target-compressed-block-size",
    "--trace",
];

/// The flags zstdcli.c answers inside its own argv scan, before any mode
/// runs: -V/--version and the help trio. They exit at their argv position
/// with the verbosity seen so far, so `-q -V` differs from `-V -q`.
fn early_exit<I: IntoIterator<Item = OsString>>(args: I) -> Option<ExitCode> {
    let mut args = args.into_iter();
    args.next()?;
    let mut level: i32 = 2;
    for arg in args {
        let Some(text) = arg.to_str() else {
            continue;
        };
        if text == "--" {
            return None;
        }
        if let Some(long) = text.strip_prefix("--") {
            match long.split('=').next().unwrap_or(long) {
                "version" => {
                    print_version(level);
                    return Some(ExitCode::SUCCESS);
                },
                "help" => {
                    print_help();
                    return Some(ExitCode::SUCCESS);
                },
                "quiet" => level -= 1,
                "verbose" => level += 1,
                _ => {},
            }
            continue;
        }
        for c in text.strip_prefix('-')?.chars() {
            match c {
                'V' => {
                    print_version(level);
                    return Some(ExitCode::SUCCESS);
                },
                'h' | 'H' => {
                    print_help();
                    return Some(ExitCode::SUCCESS);
                },
                'q' => level -= 1,
                'v' => level += 1,
                _ => {},
            }
        }
    }
    None
}

/// printVersion's shape: the bare version string below the default display
/// level, the banner line otherwise, plus the format-support line from -v
/// up. The text is zstdx's own; printing zstd's banner would claim false
/// provenance.
fn print_version(level: i32) {
    if level < 2 {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    println!(
        "*** zstdx command line interface (64-bit) v{}, pure-Rust Zstandard ***",
        env!("CARGO_PKG_VERSION")
    );
    if level >= 3 {
        println!("*** supports: zstd");
    }
}

/// The help entries -h, -H and --help all share: zstd splits them into a
/// short and a long usage, this build prints its full help for each.
fn print_help() {
    let mut command = Cli::command();
    let help = command.render_help();
    println!("{help}");
}

/// Translate zstd's `-#` level flags (`-1` .. `-19`) into the hidden
/// `--level` option so clap can parse them, rejecting separated flag values
/// the way zstd's NEXT_FIELD does. Everything after `--`, and any
/// non-digit flag, passes through untouched.
fn normalize_args<I: IntoIterator<Item = OsString>>(args: I) -> Result<Vec<OsString>, String> {
    let mut normalized = Vec::new();
    let mut literal_args = false;
    // zstdcli.c checks this pair after parsing; the line and the exit
    // status are its own.
    let mut has_dict = false;
    let mut has_patch = false;
    let mut args = args.into_iter().enumerate().peekable();
    while let Some((index, arg)) = args.next() {
        if index == 0 || literal_args {
            normalized.push(arg);
            continue;
        }
        if arg == "--" {
            literal_args = true;
            normalized.push(arg);
            continue;
        }
        if let Some(text) = arg.to_str() {
            if text == "-D" {
                has_dict = true;
            }
            if text == "--patch-from" || text.starts_with("--patch-from=") {
                has_patch = true;
            }
        }
        if has_dict && has_patch {
            return Err("error : can't use -D and --patch-from=# at the same time ".into());
        }
        if let Some(text) = arg.to_str()
            && SEPARATE_VALUE_FLAGS.contains(&text)
        {
            match args.peek() {
                None => return Err("error: missing command argument ".into()),
                Some((_, next)) => {
                    if next.to_str().is_some_and(|text| text.starts_with('-')) {
                        return Err("error: command cannot be separated from its argument by \
                                    another command "
                            .into());
                    }
                },
            }
        }
        // A bare -b carries zstd's default benchmark level; attaching it
        // here keeps the next argument free to be a file.
        if arg == "-b" {
            normalized.push(OsString::from("-b3"));
            continue;
        }
        // zstd's bundled decode-only benchmark cluster (`-bd`, `-b3d`): the
        // digits after b are the level (3 when absent), the d switches the
        // benchmark to decompression, and a trailing level is decorative
        // there (the decode benchmark runs at 0).
        if let Some(text) = arg.to_str()
            && let Some(rest) = text.strip_prefix("-b")
            && let Some(digits) = rest.strip_suffix('d').or_else(|| {
                rest.strip_suffix(|c: char| c.is_ascii_digit())
                    .and_then(|r| r.strip_suffix('d'))
            })
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            let level = if digits.is_empty() {
                "3"
            } else {
                digits
            };
            normalized.push(OsString::from(format!("-b{level}")));
            normalized.push(OsString::from("-d"));
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
    // zstdcli.c overwrites a value flag's variable on every occurrence, so
    // the last occurrence wins; clap rejects repeats, so earlier ones are
    // dropped here together with their separated value argument. Anything
    // at or after `--` is a file name.
    let literal_at = normalized
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(normalized.len());
    let mut last_index = std::collections::HashMap::new();
    for (index, arg) in normalized[..literal_at].iter().enumerate() {
        if let Some(key) = value_flag_key(arg) {
            last_index.insert(key, index);
        }
    }
    let mut dropped = std::collections::HashSet::new();
    for (index, arg) in normalized[..literal_at].iter().enumerate() {
        let Some(key) = value_flag_key(arg) else {
            continue;
        };
        if last_index[&key] != index {
            dropped.insert(index);
            // a bare NEXT_FIELD spelling owns the following argument as its
            // value; an `=`-attached one carries it inside
            if arg
                .to_str()
                .is_some_and(|text| SEPARATE_VALUE_FLAGS.contains(&text))
                && index + 1 < literal_at
            {
                dropped.insert(index + 1);
            }
        }
    }
    normalized = normalized
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !dropped.contains(index))
        .map(|(_, arg)| arg)
        .collect();
    Ok(normalized)
}

/// Value-flag spellings whose repetition means last-wins in zstdcli.c
/// (every NEXT_FIELD and readU32FromChar variable is a plain overwrite).
const REPEATED_VALUE_FLAGS: &[&str] = &[
    "--level",
    "--fast",
    "--adapt",
    "--long",
    "--zstd",
    "--priority",
    "--auto-threads",
    "--format",
    "-b",
    "-e",
    "-i",
    "-B",
    "-o",
    "--output",
    "-D",
    "-T",
    "--threads",
    "-M",
    "--memory",
    "--memlimit",
    "--memlimit-decompress",
    "--filelist",
    "--patch-from",
    "--stream-size",
    "--size-hint",
    "--maxdict",
    "--dictID",
    "--target-compressed-block-size",
    "--trace",
    "-s",
    "-P",
    "--output-dir-flat",
    "--output-dir-mirror",
];

/// The repeat-identity of one normalized argument: the long spelling up to
/// `=`, or a value-flag short letter with a value-looking tail (digits and
/// the size suffix letters) for `-oFILE`-style spellings. Bundled flag
/// clusters and files never identify.
fn value_flag_key(arg: &OsString) -> Option<String> {
    let text = arg.to_str()?;
    if !text.starts_with('-') || text == "-" {
        return None;
    }
    if let Some(long) = text.strip_prefix("--") {
        let head = long.split('=').next().unwrap_or(long);
        let spelled = format!("--{head}");
        return REPEATED_VALUE_FLAGS
            .contains(&spelled.as_str())
            .then_some(spelled);
    }
    let mut chars = text.chars();
    let letter = chars.nth(1)?;
    let tail: String = chars.collect();
    let short = format!("-{letter}");
    if !REPEATED_VALUE_FLAGS.contains(&short.as_str()) {
        return None;
    }
    let value_like = tail
        .chars()
        .all(|c| c.is_ascii_digit() || "KMGiB".contains(c));
    (tail.is_empty() || value_like).then_some(short)
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
        path::{Path, PathBuf},
    };

    use clap::Parser as _;
    use zstdx::Level;

    use crate::{
        Cli, Diagnostic, Input, Mode, Output, Settings, add_extension, compress,
        decode_failure_lines, decompress, default_output_name, early_exit, is_compressed_name,
        normalize_args, parse_size, progress_visible, rm_flag_last_wins,
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

    /// The default decode settings.
    fn plain_settings() -> Settings {
        Settings {
            level: 3,
            threads: 1,
            checksum: true,
            window_log: None,
            stream_size: None,
            size_hint: None,
            max_window: None,
            adapt: None,
            content_size: true,
            include_dictid: true,
        }
    }

    /// A valid multi-block frame.
    fn compressed_payload() -> Vec<u8> {
        let payload = vec![7u8; 300 * 1024];
        let options = zstdx::EncoderOptions::new(Level::from_zstd(3)).checksum(true);
        let mut encoder = zstdx::stream::write::Encoder::with_options(Vec::new(), options).unwrap();
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
            &plain_settings(),
            None,
            "in",
            false,
        );
        assert!(result.is_err());
        assert!(!out.exists(), "partial output must be removed");
    }

    /// A compress failure (a magic-prefixed dictionary with broken tables;
    /// a bare magic is raw content under the auto content type) after the
    /// output file was created must not leave the output on disk.
    #[test]
    fn failed_compress_removes_partial_output() {
        let broken = [
            0x37u8, 0xa4, 0x30, 0xec, 0xde, 0xad, 0xbe, 0xef, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        ];
        let out = temp_path("compress-partial");
        let result = compress(
            Box::new(&b"payload"[..]),
            &Output::File(out.clone()),
            &plain_settings(),
            0,
            Some(&broken),
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
            &plain_settings(),
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

    /// `.tzst` decompresses to `.tar`, zstd's tar-suffix rule.
    #[test]
    fn tzst_strips_to_tar() {
        assert_eq!(
            default_output_name(&PathBuf::from("archive.tzst"), Mode::Decompress).unwrap(),
            PathBuf::from("archive.tar")
        );
        // an empty stem is fine for .tzst: the whole name becomes .tar
        assert_eq!(
            default_output_name(&PathBuf::from(".tzst"), Mode::Decompress).unwrap(),
            PathBuf::from(".tar")
        );
    }

    /// The unknown-suffix diagnostic carries zstd's exact text.
    #[test]
    fn unknown_suffix_diagnostic_text() {
        let err = default_output_name(&PathBuf::from("data.bin"), Mode::Decompress).unwrap_err();
        let diagnostic = err.downcast_ref::<Diagnostic>().unwrap();
        assert_eq!(diagnostic.lines, ["zstdx: data.bin: unknown suffix \
                                       (.zst/.tzst expected). Can't derive \
                                       the output file name. Specify it with \
                                       -o dstFileName. Ignoring."]);
    }

    #[test]
    fn rm_keep_last_flag_wins() {
        let rm_of = |args: &[&str]| {
            rm_flag_last_wins(
                &(std::iter::once("zstdx")
                    .chain(args.iter().copied())
                    .map(OsString::from)
                    .collect::<Vec<_>>()),
            )
        };
        assert!(!rm_of(&["f"]), "default keeps");
        assert!(rm_of(&["--rm", "f"]));
        assert!(rm_of(&["-k", "--rm", "f"]), "rm after keep");
        assert!(!rm_of(&["--rm", "-k", "f"]), "keep after rm");
        assert!(!rm_of(&["--rm", "--keep", "f"]), "--keep after --rm");
        // after `--`, flags are files
        assert!(rm_of(&["--rm", "--", "-k"]));
    }

    /// Decode failures map onto zstd's stderr lines: the literal-(36)
    /// Decoding error with libzstd's error name, the window help block,
    /// premature end, unsupported format, and the empty-source case.
    #[test]
    fn decode_failure_lines_match_zstd() {
        // checksum mismatch: flip the trailing 4-byte frame checksum
        let mut frame = compressed_payload();
        let last = frame.len() - 1;
        frame[last] ^= 0xff;
        let err = decompress(
            Box::new(Cursor::new(frame)),
            &Output::Sink,
            &plain_settings(),
            None,
            "corrupt.zst",
            false,
        )
        .unwrap_err();
        assert_eq!(
            decode_failure_lines(
                &Input::File(PathBuf::from("corrupt.zst")),
                123,
                err.as_ref()
            )
            .unwrap(),
            ["corrupt.zst : Decoding error (36) : Restored data doesn't match checksum "]
        );

        // truncation mid-frame: premature end
        let mut frame = compressed_payload();
        frame.truncate(frame.len() / 2);
        let err = decompress(
            Box::new(Cursor::new(frame)),
            &Output::Sink,
            &plain_settings(),
            None,
            "trunc.zst",
            false,
        )
        .unwrap_err();
        assert_eq!(
            decode_failure_lines(&Input::File(PathBuf::from("trunc.zst")), 500, err.as_ref())
                .unwrap(),
            ["trunc.zst : Read error (39) : premature end "]
        );

        // the boundary shapes are decided by the frame loop itself, as
        // Diagnostics: empty source, partial magic, bad magic
        let boundary = |bytes: Vec<u8>, name: &str| -> Vec<String> {
            decompress(
                Box::new(Cursor::new(bytes)),
                &Output::Sink,
                &plain_settings(),
                None,
                name,
                false,
            )
            .unwrap_err()
            .downcast_ref::<Diagnostic>()
            .unwrap()
            .lines
            .clone()
        };
        assert_eq!(boundary(Vec::new(), "empty.zst"), ["zstdx: empty.zst: \
                                                        unexpected end of \
                                                        file "]);
        assert_eq!(boundary(b"zz".to_vec(), "hdr.bin"), [
            "zstdx: hdr.bin: unknown header "
        ]);
        assert_eq!(
            boundary(b"garbage data, not a frame".to_vec(), "badmagic.bin"),
            ["zstdx: badmagic.bin: unsupported format "]
        );
    }

    /// Pass-through (gzip -df semantics): non-frame input copies through
    /// verbatim, a frame's trailing garbage copies after the decoded bytes,
    /// and skippable frames stay transparent in every position.
    #[test]
    fn pass_through_copies_non_frames() {
        let dir = std::env::temp_dir().join("zstdx-cli-pass-through");
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("out.bin");
        let run = |bytes: Vec<u8>, pass_through: bool| -> super::AnyResult<u64> {
            decompress(
                Box::new(Cursor::new(bytes)),
                &Output::File(out_path.clone()),
                &plain_settings(),
                None,
                "in",
                pass_through,
            )
        };

        // plain text: rejected without pass-through, copied with it
        let plain = b"plain text, no frame".to_vec();
        let err = run(plain.clone(), false).unwrap_err();
        assert_eq!(err.downcast_ref::<Diagnostic>().unwrap().lines, [
            "zstdx: in: unsupported format "
        ]);
        let written = run(plain.clone(), true).unwrap();
        assert_eq!(std::fs::read(&out_path).unwrap(), plain);
        assert_eq!(written as usize, plain.len());

        // a short (<4 byte) source copies too, an empty one still fails
        run(b"abc".to_vec(), true).unwrap();
        assert_eq!(std::fs::read(&out_path).unwrap(), b"abc");
        assert!(run(Vec::new(), true).is_err());

        // frame + trailing garbage: decoded bytes then the garbage verbatim
        let frame = compressed_payload();
        let mut mixed = frame.clone();
        mixed.extend_from_slice(b"trailing garbage");
        let written = run(mixed.clone(), true).unwrap();
        let mut expected = vec![7u8; 300 * 1024];
        expected.extend_from_slice(b"trailing garbage");
        assert_eq!(std::fs::read(&out_path).unwrap(), expected);
        assert_eq!(written as usize, expected.len());
        // without pass-through the same input is unsupported format
        assert!(run(mixed, false).is_err());

        // skippable frames stay transparent around a data frame
        let content = b"SKIP".repeat(6);
        let skippable = [
            &0x184d_2a50u32.to_le_bytes()[..],
            &(content.len() as u32).to_le_bytes()[..],
            &content[..],
        ]
        .concat();
        let truncated = skippable[..skippable.len() - 2].to_vec();
        let wrapped = [skippable.clone(), frame, skippable].concat();
        let written = run(wrapped, false).unwrap();
        assert_eq!(std::fs::read(&out_path).unwrap(), vec![7u8; 300 * 1024]);
        assert_eq!(written as usize, 300 * 1024);

        // a skippable frame truncated mid-content is zstd's premature end
        let err = run(truncated, false).unwrap_err();
        assert_eq!(
            decode_failure_lines(&Input::File(PathBuf::from("in")), 9, err.as_ref()).unwrap(),
            ["in : Read error (39) : premature end "]
        );

        // foreign formats refuse before pass-through, each with its own line
        for (magic, line) in [
            (
                [0x1fu8, 0x8b, 0x08, 0x00],
                "gzip file cannot be uncompressed (zstd compiled without HAVE_ZLIB) -- ignored ",
            ),
            (
                [0xfdu8, 0x37, 0x7a, 0x58],
                "xz/lzma file cannot be uncompressed (zstd compiled without HAVE_LZMA) -- ignored ",
            ),
            (
                [0x04u8, 0x22, 0x4d, 0x18],
                "lz4 file cannot be uncompressed (zstd compiled without HAVE_LZ4) -- ignored ",
            ),
        ] {
            let err = run(magic.to_vec(), true).unwrap_err();
            assert_eq!(err.downcast_ref::<Diagnostic>().unwrap().lines, [format!(
                "zstdx: in: {line}"
            )]);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A frame whose window exceeds -M renders zstd's three-line help block
    /// (FIO_zstdErrorHelp), including the rounded-up memory hint.
    #[test]
    fn window_too_big_lines_match_zstd() {
        // magic + FHD(0: no fcs, checksum off, single-segment off) +
        // window descriptor 0xA0 (log 30) + one empty final raw block
        let frame = [0x28u8, 0xb5, 0x2f, 0xfd, 0x00, 0xa0, 0x09, 0x00, 0x00, 0x00];
        let mut settings = plain_settings();
        settings.max_window = Some(64 << 20);
        let err = decompress(
            Box::new(Cursor::new(frame.to_vec())),
            &Output::Sink,
            &settings,
            None,
            "w30.zst",
            false,
        )
        .unwrap_err();
        assert_eq!(
            decode_failure_lines(
                &Input::File(PathBuf::from("w30.zst")),
                frame.len() as u64,
                err.as_ref()
            )
            .unwrap(),
            [
                "w30.zst : Decoding error (36) : Frame requires too much memory for decoding ",
                "w30.zst : Window size larger than maximum : 1073741824 > 67108864 ",
                "w30.zst : Use --long=30 or --memory=1024MB ",
            ]
        );
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
        assert_eq!(normalize_args(args).unwrap(), vec![
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
        assert_eq!(normalize_args(args.to_vec()).unwrap(), args.to_vec());
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
            .unwrap()
            .into_iter()
            .skip(1)
            .map(|arg| arg.into_string().unwrap())
            .collect::<Vec<_>>()
        };
        assert_eq!(normalize(&["-0"]), ["--level=0"]);
        assert_eq!(normalize(&["-1"]), ["--level=1"]);
        assert_eq!(normalize(&["-123"]), ["--level=123"]);
        // the benchmark cluster: -bd / -b3d / -bd5 all decode-bench
        assert_eq!(normalize(&["-bd"]), ["-b3", "-d"]);
        assert_eq!(normalize(&["-b3d"]), ["-b3", "-d"]);
        assert_eq!(normalize(&["-bd5"]), ["-b3", "-d"]);
        // bare dash, a non-digit tail and a plain number stay untouched
        assert_eq!(normalize(&["-"]), ["-"]);
        assert_eq!(normalize(&["-12a"]), ["-12a"]);
        assert_eq!(normalize(&["12"]), ["12"]);
    }

    /// zstd's NEXT_FIELD: a separated flag value may not start with `-`,
    /// and a value flag as the last argument is a missing argument.
    #[test]
    fn separated_values_reject_dashes() {
        let normalize = |args: &[&str]| {
            normalize_args(
                std::iter::once("zstdx")
                    .chain(args.iter().copied())
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
            )
            .map(|args| args.into_iter().skip(1).collect::<Vec<_>>())
        };
        // regular separated values pass
        assert_eq!(normalize(&["-o", "out.zst", "f"]).unwrap(), [
            OsString::from("-o"),
            OsString::from("out.zst"),
            OsString::from("f")
        ]);
        // a dash (or any flag) as the value is zstd's parser error
        assert_eq!(
            normalize(&["--filelist", "-"]).unwrap_err(),
            "error: command cannot be separated from its argument by another command "
        );
        assert_eq!(
            normalize(&["-o", "-f", "f"]).unwrap_err(),
            "error: command cannot be separated from its argument by another command "
        );
        assert_eq!(
            normalize(&["-T", "-2"]).unwrap_err(),
            "error: command cannot be separated from its argument by another command "
        );
        // the `-b` default rewrite still leaves the file free
        assert_eq!(normalize(&["-b", "file"]).unwrap(), [
            OsString::from("-b3"),
            OsString::from("file")
        ]);
        // a trailing value flag is a missing argument
        assert_eq!(
            normalize(&["-D"]).unwrap_err(),
            "error: missing command argument "
        );
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
            "--filelist",
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

        // --rm and -k coexist: run resolves them by position, like zstd
        assert!(Cli::try_parse_from(["zstdx", "--rm", "-k", "f"]).is_ok());
    }

    /// The R75 surface parses: -C, --no-dictID, --[no-]content-size,
    /// --exclude-compressed, --memlimit aliases, --output-dir-flat,
    /// --auto-threads, the identity flags, and the rejected spellings
    /// (rejection happens in run, with the reason).
    #[test]
    fn r75_flags_parse() {
        let cli = Cli::try_parse_from(["zstdx", "-C", "f"]).unwrap();
        assert!(cli.check);
        let cli = Cli::try_parse_from(["zstdx", "--no-dictID", "f"]).unwrap();
        assert!(cli.no_dict_id);
        let cli = Cli::try_parse_from(["zstdx", "--content-size", "f"]).unwrap();
        assert!(cli.content_size);
        let cli = Cli::try_parse_from(["zstdx", "--no-content-size", "f"]).unwrap();
        assert!(cli.no_content_size);
        assert!(Cli::try_parse_from(["zstdx", "--content-size", "--no-content-size"]).is_err());
        let cli = Cli::try_parse_from(["zstdx", "--exclude-compressed", "f"]).unwrap();
        assert!(cli.exclude_compressed);
        let cli = Cli::try_parse_from(["zstdx", "--memlimit=1M", "f"]).unwrap();
        assert_eq!(cli.memory, Some(1 << 20));
        let cli = Cli::try_parse_from(["zstdx", "--memlimit-decompress", "512K", "f"]).unwrap();
        assert_eq!(cli.memory, Some(512 << 10));
        let cli = Cli::try_parse_from(["zstdx", "--output-dir-flat", "out", "f"]).unwrap();
        assert_eq!(cli.output_dir_flat, Some(PathBuf::from("out")));
        let cli = Cli::try_parse_from(["zstdx", "--auto-threads=logical", "f"]).unwrap();
        assert_eq!(cli.auto_threads.as_deref(), Some("logical"));
        let cli = Cli::try_parse_from(["zstdx", "--auto-threads", "physical", "f"]).unwrap();
        assert_eq!(cli.auto_threads.as_deref(), Some("physical"));
        assert!(Cli::try_parse_from(["zstdx", "--auto-threads=hybrid"]).is_err());
        for identity in [
            vec!["zstdx", "-n", "f"],
            vec!["zstdx", "--asyncio", "f"],
            vec!["zstdx", "--no-asyncio", "f"],
            vec!["zstdx", "--mmap-dict", "f"],
            vec!["zstdx", "--no-mmap-dict", "f"],
            vec!["zstdx", "--format=zstd", "f"],
        ] {
            assert!(Cli::try_parse_from(identity.as_slice()).is_ok());
        }
        for rejected in [
            vec!["zstdx", "--rsyncable", "f"],
            vec!["zstdx", "--compress-literals", "f"],
            vec!["zstdx", "--no-compress-literals", "f"],
            vec!["zstdx", "--row-match-finder", "f"],
            vec!["zstdx", "--no-row-match-finder", "f"],
            vec!["zstdx", "--target-compressed-block-size=4K", "f"],
            vec!["zstdx", "--zstd=wlog=20", "f"],
            vec!["zstdx", "--max", "f"],
            vec!["zstdx", "--trace", "t.log", "f"],
            vec!["zstdx", "--trace=trace.log", "f"],
            vec!["zstdx", "--trace-file-stat", "f"],
            vec!["zstdx", "--show-default-cparams", "f"],
            vec!["zstdx", "--priority=rt", "f"],
            vec!["zstdx", "-s9", "f"],
            vec!["zstdx", "-S", "f"],
            vec!["zstdx", "-p", "f"],
            vec!["zstdx", "-P50", "f"],
            vec!["zstdx", "--fake-stdin-is-console"],
            vec!["zstdx", "--fake-stdout-is-console"],
            vec!["zstdx", "--fake-stderr-is-console"],
            vec!["zstdx", "--output-dir-mirror", "out", "f"],
        ] {
            assert!(
                Cli::try_parse_from(rejected.as_slice()).is_ok(),
                "{rejected:?} must stay parseable for the run-time rejection"
            );
        }
        // the aliases zstd 1.5.7 lacks fall back to unknown flags
        for missing in [
            vec!["zstdx", "--file", "list"],
            vec!["zstdx", "--dict", "d.dict", "f"],
            vec!["zstdx", "--recursive", "dir"],
            vec!["zstdx", "--patch-apply", "ref", "f"],
            vec!["zstdx", "--jobsize=1M", "f"],
            vec!["zstdx", "--split=64K", "f"],
        ] {
            assert!(Cli::try_parse_from(missing.as_slice()).is_err());
        }
    }

    /// Repeated value flags keep only the last occurrence, like zstdcli.c's
    /// plain-variable overwrites; bundled flags and files never dedup.
    #[test]
    fn repeated_value_flags_last_wins() {
        let normalize = |args: &[&str]| {
            normalize_args(
                std::iter::once("zstdx")
                    .chain(args.iter().copied())
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>()
        };
        // separated, attached and long= spellings all collapse to the last
        assert_eq!(normalize(&["-D", "a", "-D", "b", "f"]), [
            OsString::from("-D"),
            OsString::from("b"),
            OsString::from("f")
        ]);
        assert_eq!(normalize(&["-T4", "-T8", "f"]), [
            OsString::from("-T8"),
            OsString::from("f")
        ]);
        assert_eq!(normalize(&["--level=3", "--level=9", "f"]), [
            OsString::from("--level=9"),
            OsString::from("f")
        ]);
        assert_eq!(normalize(&["-o", "a", "-o", "b", "f"]), [
            OsString::from("-o"),
            OsString::from("b"),
            OsString::from("f")
        ]);
        // after `--` every dash word is a file, and bundles stay whole
        assert_eq!(normalize(&["-D", "a", "--", "-D", "b"]), [
            OsString::from("-D"),
            OsString::from("a"),
            OsString::from("--"),
            OsString::from("-D"),
            OsString::from("b")
        ]);
        assert_eq!(normalize(&["-fk", "-D", "a", "f"]), [
            OsString::from("-fk"),
            OsString::from("-D"),
            OsString::from("a"),
            OsString::from("f")
        ]);
        // -D together with --patch-from is zstd's own parser error
        assert_eq!(
            normalize_args(
                ["zstdx", "-D", "d", "--patch-from", "r", "f"]
                    .iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            )
            .unwrap_err(),
            "error : can't use -D and --patch-from=# at the same time "
        );
    }

    /// fileio's compressed-extension table governs --exclude-compressed,
    /// with the leading-dot rule of UTIL_getFileExtension.
    #[test]
    fn compressed_name_rules() {
        assert!(is_compressed_name(Path::new("a.zst")));
        assert!(is_compressed_name(Path::new("b.tar.gz")));
        assert!(is_compressed_name(Path::new("c.mp4")));
        assert!(is_compressed_name(Path::new("d.woff")));
        assert!(!is_compressed_name(Path::new("e.txt")));
        assert!(!is_compressed_name(Path::new(".zst")), "dotfile stem");
        assert!(!is_compressed_name(Path::new("noext")));
    }

    /// -V answers at its argv position with the verbosity seen so far, and
    /// -h/-H/--help print the full help; nothing else intercepts.
    #[test]
    fn early_exit_flags() {
        assert!(early_exit([OsString::from("zstdx")]).is_none());
        assert!(early_exit([OsString::from("zstdx"), OsString::from("f")]).is_none());
        assert!(
            early_exit([
                OsString::from("zstdx"),
                OsString::from("--"),
                OsString::from("-V")
            ])
            .is_none()
        );
        assert!(early_exit([OsString::from("zstdx"), OsString::from("-d")]).is_none());
        assert!(early_exit([OsString::from("zstdx"), OsString::from("-V")]).is_some());
        assert!(early_exit([OsString::from("zstdx"), OsString::from("-qV")]).is_some());
        assert!(early_exit([OsString::from("zstdx"), OsString::from("-H")]).is_some());
        assert!(early_exit([OsString::from("zstdx"), OsString::from("--help")]).is_some());
    }

    /// --adapt parses with zstd's spellings: bare, either key alone, both
    /// keys in either order; malformed values and the decode modes reject.
    #[test]
    fn adapt_flags_parse() {
        use crate::adapt::AdaptRange;
        let cli = Cli::try_parse_from(["zstdx", "--adapt"]).unwrap();
        assert_eq!(cli.adapt, Some(AdaptRange { min: 1, max: 22 }));
        let cli = Cli::try_parse_from(["zstdx", "--adapt=max=9"]).unwrap();
        assert_eq!(cli.adapt, Some(AdaptRange { min: 1, max: 9 }));
        let cli = Cli::try_parse_from(["zstdx", "--adapt=max=19,min=3"]).unwrap();
        assert_eq!(cli.adapt, Some(AdaptRange { min: 3, max: 19 }));
        assert!(Cli::try_parse_from(["zstdx", "--adapt=x"]).is_err());
        assert!(Cli::try_parse_from(["zstdx", "--adapt=min=9,max=2"]).is_err());
        assert!(Cli::try_parse_from(["zstdx", "--adapt", "-d", "f.zst"]).is_err());
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

    /// The benchmark surface parses with zstd's spellings, and `-d` turns
    /// it into the decode-only benchmark instead of a conflict.
    #[test]
    fn bench_flags_parse() {
        let cli = Cli::try_parse_from(["zstdx", "-b3", "file"]).unwrap();
        assert_eq!(cli.bench, Some(3));
        let cli = Cli::try_parse_from(["zstdx", "-b1", "-e5", "-i0", "file"]).unwrap();
        assert_eq!(cli.bench, Some(1));
        assert_eq!(cli.bench_end, Some(5));
        assert_eq!(cli.bench_seconds, Some(0));
        let cli = Cli::try_parse_from(["zstdx", "-b1", "-B", "64K", "file"]).unwrap();
        assert_eq!(cli.block_size, Some(64 << 10));
        let cli = Cli::try_parse_from(["zstdx", "-b1", "-B1M", "file"]).unwrap();
        assert_eq!(cli.block_size, Some(1 << 20));
        // zstd 1.5.7 has no --split long form: an unknown argument.
        assert!(Cli::try_parse_from(["zstdx", "-b1", "--split=1M", "file"]).is_err());
        let cli = Cli::try_parse_from(["zstdx", "-b3", "-d", "f.zst"]).unwrap();
        assert!(cli.decompress && cli.bench.is_some());

        for bad in [
            vec!["zstdx", "-b3", "-t", "f"],
            vec!["zstdx", "-b3", "-l", "f"],
            vec!["zstdx", "-b3", "--train", "f"],
            vec!["zstdx", "-b3", "-z", "f"],
            vec!["zstdx", "-b3", "-o", "out", "f"],
        ] {
            assert!(
                Cli::try_parse_from(bad.as_slice()).is_err(),
                "{bad:?} must conflict"
            );
        }
    }

    /// A bare -b carries the default level so the next argument stays a file.
    #[test]
    fn bare_bench_flag_defaults_level() {
        let args = [
            OsString::from("zstdx"),
            OsString::from("-b"),
            OsString::from("file"),
        ];
        let cli = Cli::parse_from(normalize_args(args).unwrap());
        assert_eq!(cli.bench, Some(3));
        assert_eq!(cli.files, [PathBuf::from("file")]);
    }

    /// The benchmark mode runs end to end on a tiny file with a zero-second
    /// budget (the per-round timing minimum still applies) and succeeds.
    #[test]
    fn bench_smoke_run() {
        let path = temp_path("bench-smoke");
        std::fs::write(&path, vec![7u8; 64 * 1024]).unwrap();
        let cli = Cli::try_parse_from(["zstdx", "-b1", "-i0", path.to_str().unwrap()]).unwrap();
        assert_eq!(
            crate::bench::run(&cli),
            crate::Exit::Success,
            "the smoke benchmark must succeed"
        );
        let _ = std::fs::remove_file(&path);
    }
}
