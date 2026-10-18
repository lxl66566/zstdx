//! `-b#`: benchmark mode, a port of zstd's `benchzstd` (`programs/benchzstd.c`
//! on top of the `benchfn.c` timing core): whole files are loaded into
//! memory, cut into independent chunks (`-B#`), and every level runs timed
//! compression and decompression rounds until the `-i#`-second budget per
//! phase is spent, reporting the fastest measured round — the reference's
//! `BMK_benchTimedFn` aggregation. `-d` selects the decode-only benchmark
//! (`zstd -bd`, a hidden option in the reference); `-e#` extends `-b#` into
//! a level range. With no FILES, a synthetic "Lorem ipsum" corpus is
//! benchmarked instead: a faithful, byte-identical port of `programs/lorem.c`.
//!
//! Streams mirror the reference: the `\r` progress lines, results and the
//! level terminator go to stdout (`OUTPUTLEVEL`), the "Benchmarking ..."
//! header and every error go to stderr (`DISPLAYLEVEL`).

use std::{
    fs,
    io::{BufRead as _, Cursor, Read as _, Seek, SeekFrom, Write, stdout},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use zstdx::{
    DecoderOptions, EncoderDictionary, EncoderOptions, InputShape, Level,
    decoding::{Dictionary, errors::ReadFrameHeaderError, frame::read_frame_header},
};

use crate::{Cli, DEFAULT_LEVEL, MAX_WINDOW_LOG, MIN_WINDOW_LOG, PREFIX, read_filelist};

/// zstd's default benchmark seconds (`BMK_TIMETEST_DEFAULT_S`).
const DEFAULT_SECONDS: u64 = 3;
/// zstd's `ZSTD_maxCLevel()`: bench levels clamp here, without (or despite)
/// `--ultra` — benchmark mode never applies the CLI's 19 cap.
const MAX_LEVEL: i32 = 22;
/// A `-B#` cut below this many bytes is ignored (zstd's `chunkSizeMax >= 32`
/// guard); decode-only never cuts (one chunk per file).
const MIN_BLOCK_SIZE: u64 = 32;
/// The synthetic corpus size when no FILES are given (zstd's
/// `BMK_syntheticTest` default; `-B#` overrides it).
const SYNTHETIC_SIZE: usize = 10_000_000;
/// One timed measurement targets this long (`BMK_RUNTEST_DEFAULT_MS`); a
/// measurement below half of it is not reported (rounding-error guard).
const RUN_BUDGET: Duration = Duration::from_millis(1000);
/// zstd's `MB_UNIT`: speeds print in decimal megabytes per second.
const MB_UNIT: f64 = 1_000_000.0;
/// The name column width (`%-17.17s`); longer names keep their last 17
/// characters.
const NAME_WIDTH: usize = 17;
/// The progress mark cycling once per timed round (`NB_MARKS`).
const MARKS: [&str; 4] = [" |", " /", " =", " \\"];
/// zstd's 64-bit `maxMemory` (`1 << 33`): the input load ceiling.
const MAX_MEMORY: u64 = 1 << 33;
/// zstd's dict-size ceiling for `-D` in benchmark mode.
const MAX_DICT: u64 = 64 << 20;

/// Entry point: validate the flags bench mode cannot honor, then run the
/// level range over the loaded (or synthetic) corpus. Returns false on any
/// failure.
pub fn run(cli: &Cli) -> bool {
    match bench(cli) {
        Ok(()) => true,
        Err(err) => {
            if cli.quiet < 2 {
                eprintln!("{PREFIX}{err}");
            }
            false
        },
    }
}

/// Everything one benchmark invocation is configured with.
struct Config {
    /// `-d`: benchmark decompression only (`zstd -bd`).
    decode_only: bool,
    /// `-i#`: the per-phase time budget in seconds.
    seconds: u64,
    /// `-B#`/`--split=#`: the independent-chunk cut (None = no cut).
    block_size: Option<u64>,
    /// `-T#`: workers for the benchmarked compression (decode benches
    /// single-threaded, like the reference's single dctx).
    threads: u32,
    /// `--long=N`: forced window log.
    window_log: Option<u32>,
    /// zstd's displayLevel: 2 by default, -q lowers, -v raises.
    display: u8,
    dict_enc: Option<EncoderDictionary>,
    dict_dec: Option<Arc<Dictionary>>,
}

fn bench(cli: &Cli) -> Result<(), String> {
    // Bench mode fixes the semantics of every (de)compression knob, so the
    // flags that would silently do nothing are rejected up front, like the
    // other modes reject theirs.
    if let Some(err) = rejected_flag(cli) {
        return Err(err);
    }
    let display = 2u8
        .saturating_sub(cli.quiet)
        .saturating_add(cli.verbose)
        .min(4);

    let decode_only = cli.decompress;
    let mut start = cli.bench.unwrap_or(DEFAULT_LEVEL);
    let mut end = cli.bench_end.unwrap_or(start);
    if start > MAX_LEVEL {
        start = MAX_LEVEL;
    }
    if end > MAX_LEVEL {
        end = MAX_LEVEL;
    }
    if end < start {
        end = start;
    }
    // The decode-only benchmark has no level to vary (zstd forces 0).
    if decode_only {
        start = 0;
        end = 0;
    }

    let threads = if cli.single_thread || cli.threads == 1 {
        1
    } else if cli.threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
    } else {
        cli.threads
    };
    let window_log = match cli.long {
        None => None,
        Some(log) if log < MIN_WINDOW_LOG => {
            return Err(format!(
                "--long: window log must be at least {MIN_WINDOW_LOG} (got {log})"
            ));
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

    let (dict_enc, dict_dec) = match &cli.dict {
        Some(path) => {
            let meta = fs::metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
            if meta.len() > MAX_DICT {
                return Err(format!("dictionary file {} too large", path.display()));
            }
            let raw = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let enc = EncoderDictionary::parse(&raw)
                .map_err(|err| format!("invalid dictionary: {err}"))?;
            let dec = Arc::new(
                Dictionary::load(&raw).map_err(|err| format!("invalid dictionary: {err}"))?,
            );
            (Some(enc), Some(dec))
        },
        None => (None, None),
    };

    let config = Config {
        decode_only,
        seconds: cli.bench_seconds.unwrap_or(DEFAULT_SECONDS),
        block_size: cli.block_size,
        threads,
        window_log,
        display,
        dict_enc,
        dict_dec,
    };

    // zstd's dispatch header: level range and file count at level 3, the
    // thread count at level 2 when more than one is in play.
    let header_level = if threads > 1 {
        2
    } else {
        3
    };
    if display >= header_level {
        eprint!("Benchmarking ");
    }
    let files = gather_files(cli)?;
    if display >= 3 && files.len() > 1 {
        eprint!("{} files ", files.len());
    }
    if display >= 3 {
        if end > start {
            eprint!("from level {start} to {end} ");
        } else {
            eprint!("at level {start} ");
        }
    }
    if display >= header_level {
        eprintln!("using {threads} threads ");
    }
    if files.is_empty() {
        return bench_synthetic(&config, start, end);
    }
    let corpus = load_corpus(&files, &config)?;
    bench_levels(&corpus, &config, start, end)
}

/// The flags bench mode cannot honor: the (de)compression machinery never
/// runs, so its knobs must fail loudly instead of passing through.
fn rejected_flag(cli: &Cli) -> Option<String> {
    let (flag, why) = if cli.bench_end.is_some() && cli.bench.is_none() {
        ("-e", "the level range end only applies with -b#")
    } else if cli.bench_seconds.is_some() && cli.bench.is_none() {
        ("-i", "the benchmark duration only applies with -b#")
    } else if cli.block_size.is_some() && cli.bench.is_none() {
        ("-B/--split", "the block cut only applies with -b#")
    } else if cli.output.is_some() {
        ("-o/--output", "benchmark mode writes no output files")
    } else if cli.stdout {
        ("-c/--stdout", "benchmark mode writes no output")
    } else if cli.rm {
        ("--rm", "benchmark mode keeps its inputs")
    } else if cli.keep {
        ("-k/--keep", "benchmark mode always keeps its inputs")
    } else if cli.force {
        (
            "-f/--force",
            "benchmark mode writes no files and reads no terminals",
        )
    } else if cli.check || cli.no_check {
        (
            "--check/--no-check",
            "benchmark frames never carry a checksum (the library default, like zstd's bench)",
        )
    } else if cli.memory.is_some() {
        ("-M/--memory", "benchmark mode bounds no windows")
    } else if cli.stream_size.is_some() || cli.size_hint.is_some() {
        (
            "--stream-size/--size-hint",
            "benchmark inputs are files with known sizes",
        )
    } else if cli.progress || cli.no_progress {
        (
            "--progress/--no-progress",
            "benchmark mode shows no progress bar",
        )
    } else if cli.ignore_errors {
        (
            "--ignore-errors",
            "benchmark mode stops at the first failure",
        )
    } else if cli.level.is_some() || cli.fast.is_some() {
        (
            "--level/--fast (or a -N digit flag)",
            "-b# already selects the benchmark level",
        )
    } else if cli.ultra {
        (
            "--ultra",
            "benchmark mode already allows every level up to 22",
        )
    } else {
        return None;
    };
    Some(format!("{flag} is not supported here: {why}"))
}

/// Collect the files to bench. Like zstd, an argument that is no loadable
/// file is fatal ("Error loading files"): stdin and unexpanded directories
/// qualify; `-r` expands directory arguments.
fn gather_files(cli: &Cli) -> Result<Vec<PathBuf>, String> {
    let filelist = match &cli.filelist {
        Some(path) => read_filelist(path).map_err(|err| format!("{}: {err}", path.display()))?,
        None => Vec::new(),
    };
    let mut files = Vec::new();
    for file in cli.files.iter().chain(&filelist) {
        if file.as_os_str() == "-" || !file.is_file() && !file.is_dir() {
            return Err(format!("Error loading files: {}", file.display()));
        }
        if file.is_dir() {
            if !cli.recursive {
                return Err(format!("Error loading files: {}", file.display()));
            }
            let mut stack = vec![file.clone()];
            while let Some(dir) = stack.pop() {
                match fs::read_dir(&dir) {
                    Ok(entries) => {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if path.is_dir() {
                                stack.push(path);
                            } else {
                                files.push(path);
                            }
                        }
                    },
                    Err(err) => eprintln!("{PREFIX}{}: {err}", dir.display()),
                }
            }
            continue;
        }
        files.push(file.clone());
    }
    Ok(files)
}

/// A corpus resident in memory: the concatenated sources plus per-file
/// sizes, with the display name zstd would show.
struct Corpus {
    src: Vec<u8>,
    file_sizes: Vec<usize>,
    name: String,
}

/// Load every file whole (zstd's `BMK_loadFiles`): a failing stat aborts,
/// the total is capped by the memory probe, and a total of zero data is an
/// error ("no data to bench").
fn load_corpus(files: &[PathBuf], config: &Config) -> Result<Corpus, String> {
    let mut total: u64 = 0;
    for path in files {
        let len = fs::metadata(path)
            .map_err(|_| format!("Error loading files: {}", path.display()))?
            .len();
        total += len;
    }
    let benched = total.min(find_max_mem(total.saturating_mul(3)) / 3);
    if benched < total {
        eprintln!("Not enough memory; testing {} MB only...", benched >> 20);
    }

    let mut src = Vec::with_capacity(benched as usize);
    let mut file_sizes = Vec::with_capacity(files.len());
    for path in files {
        let take = fs::metadata(path)
            .map_err(|_| format!("cannot open file {}", path.display()))?
            .len()
            .min(benched.saturating_sub(src.len() as u64));
        if config.display >= 2 {
            print_out(&format!("Loading {}...       \r", path.display()));
        }
        if take > 0 {
            let mut file =
                fs::File::open(path).map_err(|_| format!("cannot open file {}", path.display()))?;
            let start = src.len();
            src.resize(start + take as usize, 0);
            file.read_exact(&mut src[start..])
                .map_err(|_| format!("invalid read {}", path.display()))?;
        }
        file_sizes.push(take as usize);
    }
    if src.is_empty() {
        return Err("no data to bench".into());
    }
    let name = if files.len() > 1 {
        format!(" {} files", files.len())
    } else {
        files[0].display().to_string()
    };
    Ok(Corpus {
        src,
        file_sizes,
        name: basename(&name).to_string(),
    })
}

/// The synthetic corpus path (`BMK_syntheticTest`): lorem ipsum of `-B#`
/// bytes when a cut is set, else 10 MB.
fn bench_synthetic(config: &Config, start: i32, end: i32) -> Result<(), String> {
    let size = match config.block_size {
        Some(size) if size > 0 => size as usize,
        _ => SYNTHETIC_SIZE,
    };
    let mut src = vec![0u8; size];
    lorem::gen_buffer(&mut src, 0);
    let corpus = Corpus {
        src,
        file_sizes: vec![size],
        name: "Lorem ipsum".to_string(),
    };
    bench_levels(&corpus, config, start, end)
}

/// zstd's `BMK_findMaxMem` arithmetic (the malloc probe replaced by the
/// deterministic ceiling: 8 GiB on 64-bit).
fn find_max_mem(required: u64) -> u64 {
    let required = ((required >> 26) + 1) << 26;
    required.saturating_add(64 << 20).min(MAX_MEMORY)
}

/// zstd's display name: the basename (either separator), keeping the last
/// 17 characters when longer.
fn basename(path: &str) -> &str {
    match path.rsplit_once(['/', '\\']) {
        Some((_, base)) => base,
        None => path,
    }
}

/// One level range over one corpus (`BMK_benchCLevels`' loop).
fn bench_levels(corpus: &Corpus, config: &Config, start: i32, end: i32) -> Result<(), String> {
    if config.display == 1 {
        print_out(&format!(
            "bench v{}: input {} bytes, {} seconds, {} KB chunks\n",
            env!("CARGO_PKG_VERSION"),
            corpus.src.len(),
            config.seconds,
            config.block_size.unwrap_or(0) >> 10
        ));
    }
    for level in start..=end {
        bench_level(level, corpus, config)?;
    }
    Ok(())
}

/// One independent block: a byte range of the corpus.
type ChunkRange = (usize, usize);

/// A level's chunk plan: the ranges plus, in decode-only mode, each whole
/// file input's decompressed size.
type ChunkPlan = (Vec<ChunkRange>, Vec<usize>);

/// The chunk plan for one level: files cut at the block size (decode-only
/// never cuts; zero-size files contribute no chunk). The decode plan also
/// resolves each whole-file input's decompressed size from its frame
/// headers, rejecting unknowns like `ZSTD_findDecompressedSize`.
fn plan_chunks(corpus: &Corpus, config: &Config) -> Result<ChunkPlan, String> {
    let cut = match config.block_size {
        Some(size) if size >= MIN_BLOCK_SIZE && !config.decode_only => size as usize,
        _ => corpus.src.len().max(1),
    };
    let mut chunks = Vec::new();
    let mut decoded_sizes = Vec::new();
    let mut pos = 0;
    for &size in &corpus.file_sizes {
        let file_end = pos + size;
        if config.decode_only {
            match find_decompressed_size(&corpus.src[pos..file_end])? {
                Some(decoded) => decoded_sizes.push(decoded as usize),
                None => {
                    return Err("Decompressed size cannot be determined: cannot benchmark".into())
                },
            }
            chunks.push((pos, file_end));
        } else {
            let mut begin = pos;
            while begin < file_end {
                let end = (begin + cut).min(file_end);
                chunks.push((begin, end));
                begin = end;
            }
        }
        pos = file_end;
    }
    Ok((chunks, decoded_sizes))
}

/// One level's benchmark: the port of `BMK_benchMemAdvancedNoAlloc`.
fn bench_level(level: i32, corpus: &Corpus, config: &Config) -> Result<(), String> {
    let decode_only = config.decode_only;
    let src = &corpus.src;
    let (chunks, decoded_sizes) = plan_chunks(corpus, config)?;

    // Decode-only benchmarks decompress into a window sized from the frame
    // headers; compression benchmarks decode back into a source-sized one.
    let src_size = if decode_only {
        decoded_sizes.iter().sum::<usize>()
    } else {
        src.len()
    };
    let mut result = vec![0u8; src_size];
    let mut compressed: Vec<Vec<u8>> = vec![Vec::new(); chunks.len()];

    // The compression core: one frame per chunk, checksum off (the library
    // default the reference benches with), the chunk size pledged.
    let mut enc_base = EncoderOptions::new(Level::from_zstd(level)).checksum(false);
    if let Some(log) = config.window_log {
        enc_base = enc_base.with_input_shape(InputShape::default().with_window_log(log));
    }
    if config.threads > 1 {
        enc_base = enc_base.workers(config.threads);
    }
    if let Some(dict) = &config.dict_enc {
        enc_base = enc_base.parsed_dictionary(dict);
    }

    // The decompression core's destination regions: chunk-sized windows
    // into the result buffer (whole-file decoded sizes in decode-only).
    let mut dec_base = DecoderOptions::new();
    if let Some(dict) = &config.dict_dec {
        dec_base = dec_base.parsed_dictionary(Arc::clone(dict));
    }
    let mut d_regions = Vec::with_capacity(chunks.len());
    let mut cursor = 0;
    for (i, &(begin, end)) in chunks.iter().enumerate() {
        let len = if decode_only {
            decoded_sizes[i]
        } else {
            end - begin
        };
        d_regions.push((cursor, cursor + len));
        cursor += len;
    }

    let name = display_name(&corpus.name);

    // The timed rounds: each phase spends its own `-i#` budget, the fastest
    // measured round wins, and the mark cycles per round.
    let mut c_state = TimedFnState::new(config.seconds);
    let mut d_state = TimedFnState::new(config.seconds);
    let (mut c_size, mut ratio) = if decode_only {
        (src.len() as u64, src_size as f64 / src.len() as f64)
    } else {
        (0, 0.0)
    };
    let (mut c_speed, mut d_speed) = (0.0f64, 0.0f64);
    let mut mark = 0usize;
    let mut compression_done = decode_only;
    let mut decompression_done = false;
    if config.display >= 2 {
        print_out(&format!("\r{}\r", " ".repeat(70)));
        print_out(&format!("{}-{name} :{src_size:>10} -> \r", MARKS[0]));
    }
    while !(compression_done && decompression_done) {
        if !compression_done {
            let mut pass = CompressPass {
                src,
                chunks: &chunks,
                enc_base: &enc_base,
                compressed: &mut compressed,
            };
            let (ns_per_run, sum) = bench_timed_fn(&mut c_state, &mut pass)?;
            c_size = sum;
            ratio = src_size as f64 / c_size as f64;
            c_speed = c_speed.max(src_size as f64 * 1e9 / ns_per_run);
            if config.display >= 2 {
                print_out(&format!(
                    "{}-{name} :{src_size:>10} ->{c_size:>10} (x{:5.*}), {:6.*} MB/s \r",
                    MARKS[mark],
                    ratio_digits(ratio),
                    ratio,
                    speed_digits(c_speed),
                    c_speed / MB_UNIT,
                ));
            }
            compression_done = c_state.completed();
        }
        if !decompression_done {
            let mut pass = DecompressPass {
                src,
                chunks: &chunks,
                regions: &d_regions,
                dec_base: &dec_base,
                compressed: &compressed,
                result: &mut result,
                decode_only,
            };
            let (ns_per_run, _) = bench_timed_fn(&mut d_state, &mut pass)?;
            d_speed = d_speed.max(src_size as f64 * 1e9 / ns_per_run);
            if config.display >= 2 {
                print_out(&format!(
                    "{}-{name} :{src_size:>10} ->{c_size:>10} (x{:5.*}), {:6.*} MB/s, {:6.1} \
                     MB/s\r",
                    MARKS[mark],
                    ratio_digits(ratio),
                    ratio,
                    speed_digits(c_speed),
                    c_speed / MB_UNIT,
                    d_speed / MB_UNIT,
                ));
            }
            decompression_done = d_state.completed();
        }
        mark = (mark + 1) % MARKS.len();
    }

    // Roundtrip check (compression mode): the decoded window must equal the
    // source; a mismatch warns instead of passing silently.
    if !decode_only && result != src[..] {
        let at = result
            .iter()
            .zip(src.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(src.len());
        eprintln!(
            "!!! WARNING !!! {:>14} : decoded output differs from the input at byte {at}   ",
            corpus.name
        );
    }

    // The -q contract line, then the level terminator.
    if config.display == 1 {
        print_out(&format!(
            "-{level:<3}{c_size:>11} ({ratio:5.3}) {:6.2} MB/s {:6.1} MB/s  {}\n",
            c_speed / MB_UNIT,
            d_speed / MB_UNIT,
            corpus.name,
        ));
    }
    if config.display >= 2 {
        print_out(&format!("{level:>2}#\n"));
    }
    Ok(())
}

/// The 17-wide display name: the basename's last 17 characters, padded.
fn display_name(name: &str) -> String {
    let count = name.chars().count();
    let tail: String = if count > NAME_WIDTH {
        name.chars().skip(count - NAME_WIDTH).collect()
    } else {
        name.to_string()
    };
    format!("{tail:<17.17}")
}

/// zstd's `ratioDigits`: three decimals below x10, two below x100, else one.
fn ratio_digits(ratio: f64) -> usize {
    1 + usize::from(ratio < 100.0) + usize::from(ratio < 10.0)
}

/// zstd's speed precision: two decimals below 10 MB/s, one above.
fn speed_digits(speed: f64) -> usize {
    usize::from(speed < 10.0 * MB_UNIT) + 1
}

/// A line on stdout, flushed (zstd's `OUTPUT` + `fflush`).
fn print_out(text: &str) {
    let mut out = stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// One timed phase over every chunk (`BMK_benchFunction`'s benchFn).
trait BenchPass {
    /// Erase the destination before a measurement (the decode window;
    /// the reference's 0xE5 memset).
    fn warm(&mut self) {}
    /// One pass over every chunk; the return is the pass's block-result
    /// sum (the compressed total, or the decoded byte count).
    fn run(&mut self) -> Result<u64, String>;
}

struct CompressPass<'a> {
    src: &'a [u8],
    chunks: &'a [ChunkRange],
    enc_base: &'a EncoderOptions,
    compressed: &'a mut [Vec<u8>],
}

impl BenchPass for CompressPass<'_> {
    fn run(&mut self) -> Result<u64, String> {
        let mut total = 0u64;
        for (slot, &(begin, end)) in self.chunks.iter().enumerate() {
            let options = self
                .enc_base
                .clone()
                .pledged_size(Some((end - begin) as u64));
            let frame = zstdx::bulk::compress_with(&self.src[begin..end], &options)
                .map_err(|err| format!("compression error: {err}"))?;
            total += frame.len() as u64;
            self.compressed[slot] = frame;
        }
        Ok(total)
    }
}

struct DecompressPass<'a> {
    src: &'a [u8],
    chunks: &'a [ChunkRange],
    regions: &'a [ChunkRange],
    dec_base: &'a DecoderOptions,
    compressed: &'a [Vec<u8>],
    result: &'a mut [u8],
    decode_only: bool,
}

impl BenchPass for DecompressPass<'_> {
    fn warm(&mut self) {
        self.result.fill(0xe5);
    }

    fn run(&mut self) -> Result<u64, String> {
        let mut decoded = 0u64;
        for (i, &(begin, end)) in self.chunks.iter().enumerate() {
            let input: &[u8] = if self.decode_only {
                &self.src[begin..end]
            } else {
                &self.compressed[i]
            };
            let (r_begin, r_end) = self.regions[i];
            let written = zstdx::bulk::decompress_to_buffer_with(
                input,
                &mut self.result[r_begin..r_end],
                self.dec_base,
            )
            .map_err(|err| format!("decompression error: {err}"))?;
            decoded += written as u64;
        }
        Ok(decoded)
    }
}

/// The per-phase scheduler, zstd's `BMK_timedFnState_t`.
struct TimedFnState {
    time_spent_ns: u128,
    time_budget_ns: u128,
    run_budget_ns: u128,
    nb_loops: u32,
}

impl TimedFnState {
    fn new(seconds: u64) -> Self {
        // zstd's `!total_ms` guard: a zero budget still measures one round.
        let total_ms = seconds.saturating_mul(1000).max(1);
        Self {
            time_spent_ns: 0,
            time_budget_ns: total_ms as u128 * 1_000_000,
            run_budget_ns: RUN_BUDGET.as_nanos(),
            nb_loops: 1,
        }
    }

    fn completed(&self) -> bool {
        self.time_spent_ns >= self.time_budget_ns
    }
}

/// One measurement: `nb_loops` passes over every chunk (`BMK_benchFunction`),
/// the destination erased first. The return is (nanoseconds per pass, the
/// first pass's return sum).
fn bench_function(pass: &mut dyn BenchPass, nb_loops: u32) -> Result<(f64, u64), String> {
    let nb_loops = nb_loops.max(1);
    pass.warm();
    let start = Instant::now();
    let mut sum = 0;
    for loop_nb in 0..nb_loops {
        let ret = pass.run()?;
        if loop_nb == 0 {
            sum = ret;
        }
    }
    let elapsed = start.elapsed().as_nanos() as f64;
    Ok((elapsed / f64::from(nb_loops), sum))
}

/// zstd's `BMK_benchTimedFn`: measure until one round spans at least half
/// the run budget (discarding shorter ones as rounding-prone), keep the
/// fastest, and bank the spent time against the phase budget.
fn bench_timed_fn(
    state: &mut TimedFnState,
    pass: &mut dyn BenchPass,
) -> Result<(f64, u64), String> {
    let run_time_min_ns = state.run_budget_ns / 2;
    let mut best = (f64::INFINITY, u64::MAX);
    loop {
        let new = bench_function(pass, state.nb_loops)?;
        let loop_ns = new.0 * f64::from(state.nb_loops);
        state.time_spent_ns += loop_ns as u128;
        // Estimate nbLoops for the next round to last ~1 second.
        if loop_ns > state.run_budget_ns as f64 / 50.0 {
            let fastest = best.0.min(new.0);
            state.nb_loops = (state.run_budget_ns as f64 / fastest) as u32 + 1;
        } else {
            state.nb_loops = state.nb_loops.saturating_mul(10);
        }
        if loop_ns < run_time_min_ns as f64 {
            continue;
        }
        if new.0 < best.0 {
            best = new;
        }
        break;
    }
    Ok(best)
}

/// `ZSTD_findDecompressedSize` over a buffer: the sum of every frame's
/// declared content size, `None` when any frame leaves it out, an error
/// when the walk hits invalid framing.
fn find_decompressed_size(data: &[u8]) -> Result<Option<u64>, String> {
    let mut cursor = Cursor::new(data);
    let mut total = 0u64;
    loop {
        let available = cursor.fill_buf().map_err(|err| err.to_string())?.len();
        if available == 0 {
            return if data.is_empty() {
                Err("data may be invalid".into())
            } else {
                Ok(Some(total))
            };
        }
        let header = match read_frame_header(&mut cursor) {
            Ok((header, _)) => header,
            Err(ReadFrameHeaderError::SkipFrame { length, .. }) => {
                cursor
                    .seek(SeekFrom::Current(i64::from(length)))
                    .map_err(|err| err.to_string())?;
                continue;
            },
            Err(_) => return Err("data may be invalid".into()),
        };
        if !header.descriptor.declares_content_size() {
            return Ok(None);
        }
        total += header.frame_content_size();
        // Hop the 3-byte block headers to the next frame boundary.
        loop {
            let mut block_header = [0u8; 3];
            cursor
                .read_exact(&mut block_header)
                .map_err(|_| "data may be invalid".to_string())?;
            let block = u32::from(block_header[0])
                | u32::from(block_header[1]) << 8
                | u32::from(block_header[2]) << 16;
            let last = block & 1 == 1;
            let block_type = (block >> 1) & 0x3;
            if block_type == 3 {
                return Err("data may be invalid".into());
            }
            // An RLE block stores one byte regardless of the coded size.
            let size = if block_type == 1 {
                1
            } else {
                u64::from(block >> 3)
            };
            cursor
                .seek(SeekFrom::Current(size as i64))
                .map_err(|err| err.to_string())?;
            if last {
                break;
            }
        }
        if header.descriptor.content_checksum_flag() {
            cursor
                .seek(SeekFrom::Current(4))
                .map_err(|err| err.to_string())?;
        }
    }
}

/// A faithful port of zstd's `programs/lorem.c`: the fixed word pool, the
/// length-weighted distribution and the `prime1/prime2` PRNG, so the
/// synthetic benchmark corpus is byte-identical to the reference's for the
/// same size and seed.
mod lorem {
    const WORDS: [&str; 255] = [
        "lorem",
        "ipsum",
        "dolor",
        "sit",
        "amet",
        "consectetur",
        "adipiscing",
        "elit",
        "sed",
        "do",
        "eiusmod",
        "tempor",
        "incididunt",
        "ut",
        "labore",
        "et",
        "dolore",
        "magna",
        "aliqua",
        "dis",
        "lectus",
        "vestibulum",
        "mattis",
        "ullamcorper",
        "velit",
        "commodo",
        "a",
        "lacus",
        "arcu",
        "magnis",
        "parturient",
        "montes",
        "nascetur",
        "ridiculus",
        "mus",
        "mauris",
        "nulla",
        "malesuada",
        "pellentesque",
        "eget",
        "gravida",
        "in",
        "dictum",
        "non",
        "erat",
        "nam",
        "voluptat",
        "maecenas",
        "blandit",
        "aliquam",
        "etiam",
        "enim",
        "lobortis",
        "scelerisque",
        "fermentum",
        "dui",
        "faucibus",
        "ornare",
        "at",
        "elementum",
        "eu",
        "facilisis",
        "odio",
        "morbi",
        "quis",
        "eros",
        "donec",
        "ac",
        "orci",
        "purus",
        "turpis",
        "cursus",
        "leo",
        "vel",
        "porta",
        "consequat",
        "interdum",
        "varius",
        "vulputate",
        "aliquet",
        "pharetra",
        "nunc",
        "auctor",
        "urna",
        "id",
        "metus",
        "viverra",
        "nibh",
        "cras",
        "mi",
        "unde",
        "omnis",
        "iste",
        "natus",
        "error",
        "perspiciatis",
        "voluptatem",
        "accusantium",
        "doloremque",
        "laudantium",
        "totam",
        "rem",
        "aperiam",
        "eaque",
        "ipsa",
        "quae",
        "ab",
        "illo",
        "inventore",
        "veritatis",
        "quasi",
        "architecto",
        "beatae",
        "vitae",
        "dicta",
        "sunt",
        "explicabo",
        "nemo",
        "ipsam",
        "quia",
        "voluptas",
        "aspernatur",
        "aut",
        "odit",
        "fugit",
        "consequuntur",
        "magni",
        "dolores",
        "eos",
        "qui",
        "ratione",
        "sequi",
        "nesciunt",
        "neque",
        "porro",
        "quisquam",
        "est",
        "dolorem",
        "adipisci",
        "numquam",
        "eius",
        "modi",
        "tempora",
        "incidunt",
        "magnam",
        "quaerat",
        "ad",
        "minima",
        "veniam",
        "nostrum",
        "ullam",
        "corporis",
        "suscipit",
        "laboriosam",
        "nisi",
        "aliquid",
        "ex",
        "ea",
        "commodi",
        "consequatur",
        "autem",
        "eum",
        "iure",
        "voluptate",
        "esse",
        "quam",
        "nihil",
        "molestiae",
        "illum",
        "fugiat",
        "quo",
        "pariatur",
        "vero",
        "accusamus",
        "iusto",
        "dignissimos",
        "ducimus",
        "blanditiis",
        "praesentium",
        "voluptatum",
        "deleniti",
        "atque",
        "corrupti",
        "quos",
        "quas",
        "molestias",
        "excepturi",
        "sint",
        "occaecati",
        "cupiditate",
        "provident",
        "similique",
        "culpa",
        "officia",
        "deserunt",
        "mollitia",
        "animi",
        "laborum",
        "dolorum",
        "fuga",
        "harum",
        "quidem",
        "rerum",
        "facilis",
        "expedita",
        "distinctio",
        "libero",
        "tempore",
        "cum",
        "soluta",
        "nobis",
        "eligendi",
        "optio",
        "cumque",
        "impedit",
        "minus",
        "quod",
        "maxime",
        "placeat",
        "facere",
        "possimus",
        "assumenda",
        "repellendus",
        "temporibus",
        "quibusdam",
        "officiis",
        "debitis",
        "saepe",
        "eveniet",
        "voluptates",
        "repudiandae",
        "recusandae",
        "itaque",
        "earum",
        "hic",
        "tenetur",
        "sapiente",
        "delectus",
        "reiciendis",
        "cillum",
        "maiores",
        "alias",
        "perferendis",
        "doloribus",
        "asperiores",
        "repellat",
        "minim",
        "nostrud",
        "exercitation",
        "ullamco",
        "laboris",
        "aliquip",
        "duis",
        "aute",
        "irure",
    ];
    /// Word-length weights (index 5 covers every longer word).
    const WEIGHTS: [u32; 6] = [0, 8, 6, 4, 3, 2];

    struct LoremGen<'a> {
        buf: &'a mut [u8],
        pos: usize,
        rand_root: u32,
    }

    impl LoremGen<'_> {
        fn rand(&mut self, range: u32) -> u32 {
            const PRIME1: u32 = 2654435761;
            const PRIME2: u32 = 2246822519;
            let mut rand32 = self.rand_root;
            rand32 = rand32.wrapping_mul(PRIME1);
            rand32 ^= PRIME2;
            rand32 = rand32.rotate_left(13);
            self.rand_root = rand32;
            ((rand32 as u64 * u64::from(range)) >> 32) as u32
        }

        fn about(&mut self, target: u32) -> i32 {
            (self.rand(target) + self.rand(target) + 1) as i32
        }

        fn write_last_characters(&mut self) {
            let last = self.buf.len() - self.pos;
            if last == 0 {
                return;
            }
            self.buf[self.pos] = b'.';
            self.pos += 1;
            if last > 2 {
                self.buf[self.pos..self.pos + last - 2].fill(b' ');
            }
            if last > 1 {
                let end = self.buf.len() - 1;
                self.buf[end] = b'\n';
            }
            self.pos = self.buf.len();
        }

        fn generate_word(&mut self, word: &str, separator: &str, up_case: bool) {
            let len = word.len() + separator.len();
            if self.pos + len > self.buf.len() {
                self.write_last_characters();
                return;
            }
            self.buf[self.pos..self.pos + word.len()].copy_from_slice(word.as_bytes());
            if up_case {
                // upcasing: +32 (b'A' - b'a'), expressed without overflow
                self.buf[self.pos] = self.buf[self.pos].wrapping_sub(b'a' - b'A');
            }
            self.pos += word.len();
            self.buf[self.pos..self.pos + separator.len()].copy_from_slice(separator.as_bytes());
            self.pos += separator.len();
        }

        fn generate_sentence(&mut self, nb_words: i32, distrib: &[u32]) {
            let comma_pos = self.about(9);
            let comma2 = comma_pos + self.about(7);
            let qmark = self.rand(11) == 7;
            let end_sep = if qmark {
                "? "
            } else {
                ". "
            };
            for i in 0..nb_words {
                let word_id = distrib[self.rand(distrib.len() as u32) as usize];
                let mut sep = " ";
                if i == comma_pos || i == comma2 {
                    sep = ", ";
                }
                if i == nb_words - 1 {
                    sep = end_sep;
                }
                self.generate_word(WORDS[word_id as usize], sep, i == 0);
            }
        }

        fn generate_paragraph(&mut self, nb_sentences: i32, distrib: &[u32]) {
            for _ in 0..nb_sentences {
                let words_per_sentence = self.about(11);
                self.generate_sentence(words_per_sentence, distrib);
            }
            if self.pos < self.buf.len() {
                self.buf[self.pos] = b'\n';
                self.pos += 1;
            }
            if self.pos < self.buf.len() {
                self.buf[self.pos] = b'\n';
                self.pos += 1;
            }
        }

        /// The canonical lorem-ipsum opener (18 words, commas after
        /// "amet" and "elit").
        fn generate_first_sentence(&mut self) {
            for (i, word) in WORDS.iter().take(18).enumerate() {
                let sep = if i == 4 || i == 7 {
                    ", "
                } else {
                    " "
                };
                self.generate_word(word, sep, i == 0);
            }
            self.generate_word(WORDS[18], ". ", false);
        }
    }

    /// `LOREM_genBuffer`: fill `buf` exactly, starting with the canonical
    /// first sentence (`first = 1`, `fill = 1`).
    pub fn gen_buffer(buf: &mut [u8], seed: u32) {
        let mut distrib = Vec::new();
        for (id, word) in WORDS.iter().enumerate() {
            let len = word.len().min(WEIGHTS.len() - 1);
            for _ in 0..WEIGHTS[len] {
                distrib.push(id as u32);
            }
        }
        let mut generator = LoremGen {
            buf,
            pos: 0,
            rand_root: seed,
        };
        generator.generate_first_sentence();
        while generator.pos < generator.buf.len() {
            let sentence_per_paragraph = generator.about(7);
            generator.generate_paragraph(sentence_per_paragraph, &distrib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lorem corpus starts with the canonical sentence and is
    /// deterministic per seed.
    #[test]
    fn lorem_starts_canonical_and_is_deterministic() {
        let expected_head = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
                              eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
        let mut a = vec![0u8; 4096];
        lorem::gen_buffer(&mut a, 0);
        assert_eq!(&a[..expected_head.len()], expected_head);
        let mut b = vec![0u8; 4096];
        lorem::gen_buffer(&mut b, 0);
        assert_eq!(a, b);
        let mut c = vec![0u8; 4096];
        lorem::gen_buffer(&mut c, 1);
        assert_ne!(a, c, "different seeds must differ");
    }

    /// The timed scheduler reports the fastest round and completes once the
    /// budget is spent (mirrors BMK_benchTimedFn's shape).
    #[test]
    fn timed_fn_reports_fastest_and_completes() {
        struct Spin {
            calls: u32,
        }
        impl BenchPass for Spin {
            fn run(&mut self) -> Result<u64, String> {
                self.calls += 1;
                let spin = Instant::now();
                while spin.elapsed() < Duration::from_millis(60) {}
                Ok(42)
            }
        }
        let mut state = TimedFnState::new(0);
        let mut pass = Spin { calls: 0 };
        let (ns_per_run, sum) = bench_timed_fn(&mut state, &mut pass).unwrap();
        assert_eq!(sum, 42);
        assert!(ns_per_run > 0.0);
        assert!(
            state.completed(),
            "a 0-second budget is spent after one round"
        );
        assert!(
            pass.calls >= 2,
            "the loop must re-measure with a larger count"
        );
    }

    /// Display helpers: zstd's digit rules.
    #[test]
    fn digit_rules_match_zstd() {
        assert_eq!(ratio_digits(5.29), 3);
        assert_eq!(ratio_digits(50.0), 2);
        assert_eq!(ratio_digits(500.0), 1);
        assert_eq!(speed_digits(5_000_000.0), 2);
        assert_eq!(speed_digits(50_000_000.0), 1);
        assert_eq!(basename("/a/b/corpus.raw"), "corpus.raw");
        assert_eq!(basename("corpus.raw"), "corpus.raw");
        assert_eq!(display_name("corpus.raw"), "corpus.raw       ");
        assert_eq!(
            display_name("a-very-long-corpus-name.raw"),
            "g-corpus-name.raw"
        );
    }

    /// The chunk plan cuts at the block size and the decode-size walk reads
    /// real frame headers.
    #[test]
    fn chunk_plan_and_decode_size_walk() {
        let data = vec![7u8; 100_000];
        let compressed = zstdx::bulk::compress_with(
            &data,
            &EncoderOptions::new(Level::Fastest).pledged_size(Some(100_000)),
        )
        .unwrap();
        assert_eq!(
            find_decompressed_size(&compressed).unwrap(),
            Some(100_000),
            "a one-shot frame declares its size"
        );
        // A streaming frame without the declaration reads as unknown.
        let mut stream = Vec::new();
        let mut encoder = zstdx::stream::write::Encoder::new(&mut stream, Level::Fastest).unwrap();
        encoder.write_all(&data).unwrap();
        encoder.finish().unwrap();
        assert_eq!(find_decompressed_size(&stream).unwrap(), None);
        assert!(find_decompressed_size(b"not zstd").is_err());
        assert!(find_decompressed_size(b"").is_err());

        // The cut: two files, a block size splitting the first.
        let corpus = Corpus {
            src: vec![0u8; 100],
            file_sizes: vec![60, 40],
            name: " 2 files".to_string(),
        };
        let config = Config {
            decode_only: false,
            seconds: 0,
            block_size: Some(50),
            threads: 1,
            window_log: None,
            display: 0,
            dict_enc: None,
            dict_dec: None,
        };
        let (chunks, _) = plan_chunks(&corpus, &config).unwrap();
        assert_eq!(
            chunks,
            vec![(0, 50), (50, 60), (60, 100)],
            "each file cuts independently"
        );
    }
}
