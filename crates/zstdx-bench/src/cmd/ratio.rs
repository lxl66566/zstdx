//! Compression-ratio sweep: every ladder level × bulk/streaming ×
//! single-/multi-thread over the corpus shapes, exactly one pass per cell,
//! no timing loops.
//!
//! Sizes are deterministic, so two runs (or two builds) diff cleanly — the
//! regression gate for optimizations that may alter the encoder's output.
//! Every zstdx cell is roundtrip-gated (both decoders) before it is
//! reported. The libzstd reference side can be skipped with `--no-ref`.
//!
//! Cells are independent and only sizes are measured, so the sweep runs
//! cells concurrently on a rayon pool (`--parallel`, default 8; each cell
//! transiently holds a few 32 MiB-scale buffers, keep the width within
//! RAM). Single-threaded cells still exercise the single-threaded encoder
//! — the pool only runs many of them at once. MT modes engage per-call
//! worker pools on both sides (`--mt-workers`, default 4).
//!
//! A summary with geo-means is printed last so `tail` sees the verdict;
//! Δ% is scale-free (`zstdx_ratio / zstd_ratio - 1`, positive = denser
//! than libzstd), which keeps the zeros shape from dominating.

use std::{
    io::Read as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use rayon::prelude::*;
use zstdx::{EncoderOptions, Level, stream::read::Encoder as RuzStreamEncoder};

use crate::{
    common::want,
    corpus::{LADDER, LevelName, SHAPES, Shape, assert_roundtrip, load_raw},
};

/// Encode paths whose output sizes are compared. `Uncompressed` is not a
/// ladder level: its ratio is 1.0 by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RatioMode {
    /// One-shot bulk encode on the calling thread.
    BulkSt,
    /// One-shot bulk encode with a per-call worker pool.
    BulkMt,
    /// Streaming encoder (read side) on the calling thread.
    StreamSt,
    /// Streaming encoder (read side) with a worker pool.
    StreamMt,
}

impl RatioMode {
    pub const fn tag(self) -> &'static str {
        match self {
            RatioMode::BulkSt => "bulk-st",
            RatioMode::BulkMt => "bulk-mt",
            RatioMode::StreamSt => "stream-st",
            RatioMode::StreamMt => "stream-mt",
        }
    }

    const fn is_mt(self) -> bool {
        matches!(self, RatioMode::BulkMt | RatioMode::StreamMt)
    }
}

pub const MODES: [RatioMode; 4] = [
    RatioMode::BulkSt,
    RatioMode::BulkMt,
    RatioMode::StreamSt,
    RatioMode::StreamMt,
];

#[derive(clap::Args)]
pub struct Args {
    /// Corpus shapes to include (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    pub shape: Vec<Shape>,
    /// Encoder levels to include (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    pub level: Vec<LevelName>,
    /// Encode modes to include (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    pub mode: Vec<RatioMode>,
    /// Worker count for the mt modes (both sides)
    #[arg(long, default_value_t = 4)]
    pub mt_workers: u32,
    /// Concurrent cells in the sweep (0 = one per logical core); bounded by
    /// RAM, see the module docs
    #[arg(long, default_value_t = 8)]
    pub parallel: usize,
    /// Skip the libzstd reference side (sizes only, no Δ column)
    #[arg(long)]
    pub no_ref: bool,
}

struct Cell {
    shape: Shape,
    raw: Arc<Vec<u8>>,
    level: LevelName,
    mode: RatioMode,
}

struct Row {
    cell: Cell,
    raw_len: u64,
    ours: u64,
    theirs: Option<u64>,
}

impl Row {
    fn label(&self) -> String {
        format!(
            "{}.{}.{}",
            self.cell.shape.raw_name(),
            self.cell.level.tag(),
            self.cell.mode.tag()
        )
    }

    fn ours_ratio(&self) -> f64 {
        self.raw_len as f64 / self.ours as f64
    }

    fn theirs_ratio(&self) -> Option<f64> {
        self.theirs.map(|t| self.raw_len as f64 / t as f64)
    }

    /// `ours_ratio / theirs_ratio - 1`: positive = denser than libzstd.
    fn delta(&self) -> Option<f64> {
        self.theirs_ratio().map(|r| self.ours_ratio() / r - 1.0)
    }
}

fn zstdx_encode(raw: &[u8], level: Level, mode: RatioMode, mt: u32) -> Vec<u8> {
    let opts = EncoderOptions::new(level).checksum(false);
    let opts = if mode.is_mt() {
        opts.workers(mt)
    } else {
        opts
    };
    match mode {
        RatioMode::BulkSt | RatioMode::BulkMt => zstdx::bulk::compress_with(raw, &opts),
        RatioMode::StreamSt | RatioMode::StreamMt => {
            let mut enc = RuzStreamEncoder::with_options(raw, opts).unwrap();
            let mut out = Vec::new();
            enc.read_to_end(&mut out).unwrap();
            enc.finish();
            out
        },
    }
}

fn zstd_encode(raw: &[u8], z: i32, mode: RatioMode, mt: u32) -> Vec<u8> {
    match mode {
        RatioMode::BulkSt => zstd::bulk::compress(raw, z).unwrap(),
        RatioMode::BulkMt => {
            // fresh context per call: symmetric with zstdx's per-call pool
            let mut c = zstd::bulk::Compressor::new(z).unwrap();
            c.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(mt))
                .unwrap();
            c.compress(raw).unwrap()
        },
        RatioMode::StreamSt | RatioMode::StreamMt => {
            let mut enc = zstd::stream::read::Encoder::new(raw, z).unwrap();
            if mode.is_mt() {
                enc.multithread(mt).unwrap();
            }
            let mut out = Vec::new();
            enc.read_to_end(&mut out).unwrap();
            enc.finish();
            out
        },
    }
}

fn fmt_ratio(r: f64) -> String {
    if r < 10_000.0 {
        format!("{r:>9.3}")
    } else {
        format!("{r:>9.0}")
    }
}

fn fmt_delta(d: Option<f64>) -> String {
    d.map_or_else(|| "-".to_owned(), |d| format!("{:>+8.2}%", d * 100.0))
}

/// Geo-mean of Δ fractions, in percent: averaged over the `(1 + Δ)` factors
/// so exact zeros (`random` ties with libzstd) never hit `ln(0)`.
fn geomean_pct(ds: &[f64]) -> f64 {
    let factors = ds.iter().map(|d| (1.0 + d).ln()).sum::<f64>() / ds.len() as f64;
    (factors.exp() - 1.0) * 100.0
}

/// Geo-mean Δ summary lines for one grouping of the rows.
fn summary_group(title: &str, groups: &[(String, Vec<f64>)]) {
    println!("-- {title} (geo-mean Δ%, + = denser than zstd) --");
    for (name, deltas) in groups {
        if deltas.is_empty() {
            continue;
        }
        println!("{name:<24}{:+8.2}%", geomean_pct(deltas));
    }
}

fn print_rows(rows: &[Row]) {
    println!(
        "{:<28}{:>9}{:>10}{:>9}{:>10}{:>9}{:>9}",
        "cell", "raw", "zstdx", "r", "zstd", "r", "Δ%"
    );
    for row in rows {
        let (t_bytes, t_ratio, delta) = match (row.theirs, row.theirs_ratio(), row.delta()) {
            (Some(t), Some(r), Some(d)) => (format!("{t:>10}"), fmt_ratio(r), fmt_delta(Some(d))),
            _ => (
                format!("{:>10}", "-"),
                format!("{:>9}", "-"),
                fmt_delta(None),
            ),
        };
        // literal separators between the pre-formatted columns: a Δ wide
        // enough to fill its own padding would otherwise abut the ratio
        println!(
            "{:<28}{:>9}{:>10} {} {} {} {}",
            row.label(),
            row.raw_len,
            row.ours,
            fmt_ratio(row.ours_ratio()),
            t_bytes,
            t_ratio,
            delta,
        );
    }
}

fn print_summary(rows: &[Row], args: &Args, t0: Instant) {
    println!("\n== summary ==");
    let mut by_mode: Vec<(String, Vec<f64>)> = Vec::new();
    for mode in MODES {
        let deltas: Vec<f64> = rows
            .iter()
            .filter(|r| r.cell.mode == mode)
            .filter_map(Row::delta)
            .collect();
        if deltas.is_empty() {
            continue;
        }
        println!(
            "-- per level, mode {} (geo-mean Δ% over shapes) --",
            mode.tag()
        );
        for level in LADDER {
            let deltas: Vec<f64> = rows
                .iter()
                .filter(|r| r.cell.mode == mode && r.cell.level == level)
                .filter_map(Row::delta)
                .collect();
            if deltas.is_empty() {
                continue;
            }
            println!("{:<24}{:+8.2}%", level.tag(), geomean_pct(&deltas));
        }
        by_mode.push((mode.tag().to_owned(), deltas));
    }
    summary_group("per mode (geo-mean Δ% over cells)", &by_mode);

    let by_shape: Vec<(String, Vec<f64>)> = SHAPES
        .iter()
        .filter(|s| want(&args.shape, s))
        .map(|s| {
            (
                s.raw_name().to_owned(),
                rows.iter()
                    .filter(|r| r.cell.shape == *s)
                    .filter_map(Row::delta)
                    .collect(),
            )
        })
        .collect();
    summary_group("per shape (geo-mean Δ% over cells)", &by_shape);

    let all: Vec<f64> = rows.iter().filter_map(Row::delta).collect();
    if !all.is_empty() {
        let mut ranked: Vec<(String, f64)> = rows
            .iter()
            .filter_map(|r| r.delta().map(|d| (r.label(), d)))
            .collect();
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (worst, w_d) = ranked.first().unwrap();
        let (best, b_d) = ranked.last().unwrap();
        println!(
            "geo-mean Δ over {} comparisons: {:+.2}% | worst {} ({:+.2}%) best {} ({:+.2}%) | \
             wall {:.1}s (parallel {}, mt-workers {})",
            all.len(),
            geomean_pct(&all),
            worst,
            w_d * 100.0,
            best,
            b_d * 100.0,
            t0.elapsed().as_secs_f64(),
            args.parallel,
            args.mt_workers,
        );
    }
    println!(
        "sizes are deterministic: diff two runs (or two builds) to spot regressions{}",
        if args.no_ref {
            " (--no-ref: zstd side skipped)"
        } else {
            ""
        },
    );
}

pub fn run(args: &Args) {
    let t0 = Instant::now();
    let shapes: Vec<(Shape, Arc<Vec<u8>>)> = SHAPES
        .iter()
        .copied()
        .filter(|s| want(&args.shape, s))
        .map(|s| (s, Arc::new(load_raw(s))))
        .collect();
    let levels: Vec<LevelName> = LADDER
        .iter()
        .copied()
        .filter(|l| want(&args.level, l))
        .collect();
    let modes: Vec<RatioMode> = MODES
        .iter()
        .copied()
        .filter(|m| want(&args.mode, m))
        .collect();
    assert!(!shapes.is_empty(), "--shape selected nothing");
    assert!(!levels.is_empty(), "--level selected nothing");
    assert!(!modes.is_empty(), "--mode selected nothing");

    let mut cells = Vec::new();
    for (shape, raw) in &shapes {
        for level in &levels {
            for mode in &modes {
                cells.push(Cell {
                    shape: *shape,
                    raw: Arc::clone(raw),
                    level: *level,
                    mode: *mode,
                });
            }
        }
    }
    println!(
        "# ratio sweep: {} cells ({} shapes x {} levels x {} modes), checksums off, parallel {}, \
         mt-workers {}, ref {}",
        cells.len(),
        shapes.len(),
        levels.len(),
        modes.len(),
        args.parallel,
        args.mt_workers,
        if args.no_ref {
            "off"
        } else {
            "zstd crate"
        },
    );

    let total = cells.len();
    let done = AtomicUsize::new(0);
    let rows: Vec<Row> = rayon::ThreadPoolBuilder::new()
        .num_threads(args.parallel)
        .build()
        .unwrap()
        .install(|| {
            cells
                .into_par_iter()
                .map(|cell| {
                    let (level, z) = cell.level.pair();
                    let ours = zstdx_encode(&cell.raw, level, cell.mode, args.mt_workers);
                    assert_roundtrip(
                        &ours,
                        &cell.raw,
                        &format!(
                            "{}.{}.{}",
                            cell.shape.raw_name(),
                            cell.level.tag(),
                            cell.mode.tag()
                        ),
                    );
                    let theirs = if args.no_ref {
                        None
                    } else {
                        Some(zstd_encode(&cell.raw, z, cell.mode, args.mt_workers).len() as u64)
                    };
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    eprintln!(
                        "[{n}/{total}] {}.{}.{} ours {} ref {}",
                        cell.shape.raw_name(),
                        cell.level.tag(),
                        cell.mode.tag(),
                        ours.len(),
                        theirs.unwrap_or(0),
                    );
                    let raw_len = cell.raw.len() as u64;
                    Row {
                        cell,
                        raw_len,
                        ours: ours.len() as u64,
                        theirs,
                    }
                })
                .collect()
        });

    print_rows(&rows);
    print_summary(&rows, args, t0);
}
