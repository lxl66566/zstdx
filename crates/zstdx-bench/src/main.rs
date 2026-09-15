//! Benchmark and dev-tool harness for the zstdx crate, kept out of the
//! library so dev-only dependencies never leak into it.
//!
//! Every timing subcommand shares the interleaved A/B harness in `common`
//! and is correctness-gated before anything is timed. The corpus lives in
//! `bench/corpus` at the repository root (`bench/gen_corpus.sh` regenerates
//! it). Time control: `--budget-ms` on the timing subcommands (default 500
//! ms per side; the `BENCH_BUDGET_MS` env var of the old examples still
//! works).

mod cmd;
mod common;
mod corpus;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zstdx-bench", about = "zstdx benchmark and dev tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Cross-matrix vs the zstd crate: decode/encode x bulk/stream x st/mt.
    /// The heavyweight tool; run sections or single shapes/levels instead of
    /// `all` when iterating. `--full-ladder` adds the numeric 1-22 enc-st
    /// A/B (release-gate only).
    Matrix(cmd::matrix::Args),
    /// Compression-ratio sweep: every level x bulk/stream x st/mt, one
    /// deterministic pass per cell (parallel), geo-mean summary at the end.
    Ratio(cmd::ratio::Args),
    /// Small-payload encode (1 KiB - 1 MiB) vs the zstd crate, per-call
    /// throughput including allocator effects.
    Small(cmd::small::Args),
    /// Decode explicit .zst files with a time budget, verifying against the
    /// raw counterpart when one is found next to the file.
    Files(cmd::files::Args),
    /// Solo profiling loops for `perf` attribution: decode, bulk encode, or
    /// streaming encode of one file, no reference side.
    Prof(cmd::prof::Args),
    /// Compress every corpus shape so two builds can be compared byte for
    /// byte (`cmp -r`); the regression gate for optimizations that must not
    /// change the encoder's output.
    Dump(cmd::dump::Args),
    /// Emit one corpus file as a zstdx frame (bulk/stream, st/mt) for the
    /// downstream analysis tools.
    Emitframe(cmd::emitframe::Args),
    /// Random-corruption smoke test: flipped bytes must error, never panic.
    Corrupt(cmd::corrupt::Args),
    /// Multithreaded decode validation: every file in a directory decoded
    /// with several worker counts, byte-compared against the zstd CLI.
    Mtcheck(cmd::mtcheck::Args),
    /// Sequence statistics and entropy lower bound of our matcher on one
    /// corpus file.
    Seqstats(cmd::seqstats::Args),
    /// Piece-pipeline critical-path analysis for parallel stage-B
    /// execution on one frame (seq_dump decode hook).
    Piecepipe(cmd::piecepipe::Args),
    /// Micro-benchmark of the matcher's `prefill_window` per strategy.
    Prefill(cmd::prefill::Args),
    /// Train a raw-content dictionary with the in-tree trainer, or extract
    /// the content section of a formatted dict.
    Train(cmd::train::Args),
}

fn main() {
    match Cli::parse().command {
        Command::Matrix(args) => cmd::matrix::run(&args),
        Command::Ratio(args) => cmd::ratio::run(&args),
        Command::Small(args) => cmd::small::run(&args),
        Command::Files(args) => cmd::files::run(&args),
        Command::Prof(args) => cmd::prof::run(&args),
        Command::Dump(args) => cmd::dump::run(&args),
        Command::Emitframe(args) => cmd::emitframe::run(&args),
        Command::Corrupt(args) => cmd::corrupt::run(&args),
        Command::Mtcheck(args) => cmd::mtcheck::run(&args),
        Command::Seqstats(args) => cmd::seqstats::run(&args),
        Command::Piecepipe(args) => cmd::piecepipe::run(&args),
        Command::Prefill(args) => cmd::prefill::run(&args),
        Command::Train(args) => cmd::train::run(&args),
    }
}
