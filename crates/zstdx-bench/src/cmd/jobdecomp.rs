//! Per-job fixed-cost decomposition for the opt/ultra tiers: runs one file
//! through the bulk-mt / streaming-mt / single-threaded encoders and reports
//! the `job_trace` spans (strip tree-fill, ultra seed parse, stats/table
//! reset, LDM prefill, block-boundary refills) against the summed job time.
//!
//! Needs the trace hooks compiled in:
//! `cargo run --release -p zstdx-bench --features job_trace -- jobdecomp
//! bench/corpus/text.raw --level opt,ultra --workers 8`
//!
//! The summed `job` time exceeds the wall clock on the mt paths (jobs run
//! in parallel); component shares are taken against it. Wall-clock verdicts
//! (before/after a change) need the interleaved harness, not this tool —
//! this decomposes, it does not judge.

use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::corpus::LevelName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DecompMode {
    /// Bulk multithreaded encode (the pledged stream's byte-equal twin).
    BulkMt,
    /// Streaming multithreaded encode, 64 KiB output pulls (the matrix
    /// enc-stream shape; unpledged growing-grid jobs).
    StreamMt,
    /// Bulk single-threaded encode (no job boundaries; only the frame-start
    /// costs fire — the reference floor).
    St,
}

#[derive(Args)]
pub struct JobDecompArgs {
    /// Raw input file.
    pub file: PathBuf,
    /// Ladder levels to decompose.
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = vec![LevelName::Opt, LevelName::Ultra])]
    pub level: Vec<LevelName>,
    /// Worker count for the mt modes.
    #[arg(long, default_value_t = 8)]
    pub workers: u32,
    /// Timed iterations per cell (each iteration re-zeros the counters).
    #[arg(long, default_value_t = 3)]
    pub iters: u32,
    /// Encode paths to measure.
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = vec![DecompMode::BulkMt, DecompMode::StreamMt])]
    pub modes: Vec<DecompMode>,
}

pub fn run(args: &JobDecompArgs) {
    #[cfg(not(feature = "job_trace"))]
    {
        let _ = args;
        eprintln!(
            "error: jobdecomp needs the encoder trace hooks; rebuild with `--features job_trace`"
        );
    }
    #[cfg(feature = "job_trace")]
    trace::run_trace(args);
}

#[cfg(feature = "job_trace")]
mod trace {
    use std::{fs, io::Read as _, time::Instant};

    use zstdx::{EncoderOptions, Level, encoding::job_trace};

    use super::{DecompMode, JobDecompArgs};
    use crate::corpus::assert_roundtrip;

    fn ms(ns: u64) -> f64 {
        ns as f64 / 1e6
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    fn encode_bulk(raw: &[u8], level: Level, workers: u32) -> Vec<u8> {
        zstdx::bulk::compress_with(
            raw,
            &EncoderOptions::new(level).checksum(false).workers(workers),
        )
        .unwrap()
    }

    fn encode_stream(raw: &[u8], level: Level, workers: u32) -> Vec<u8> {
        let mut enc = zstdx::stream::read::Encoder::with_options(
            raw,
            EncoderOptions::new(level).checksum(false).workers(workers),
        )
        .unwrap();
        let mut out = Vec::with_capacity(raw.len() / 4);
        let mut sink = vec![0u8; 64 * 1024];
        loop {
            let n = enc.read(&mut sink).unwrap();
            out.extend_from_slice(&sink[..n]);
            if n == 0 {
                break;
            }
        }
        enc.finish();
        out
    }

    fn share(ns: u64, job_ns: u64) -> f64 {
        if job_ns == 0 {
            0.0
        } else {
            100.0 * ns as f64 / job_ns as f64
        }
    }

    fn report(s: &job_trace::Snapshot) {
        let jobs = s.jobs.max(1);
        let rest = s.job_ns.saturating_sub(
            s.prefill_ns + s.reset_ns + s.seed_ns + s.strip_fill_ns + s.lag_fill_ns + s.hash3_ns,
        );
        println!(
            "  strip_fill {:>8.2} ms ({:4.1}% of job, {:>6.1} MiB in {} fills, {:>6.0} MiB/s)  \
             lag_fill {:>7.2} ms ({:4.1}%, {:>6.1} MiB)  seed {:>7.2} ms ({:4.1}%)",
            ms(s.strip_fill_ns),
            share(s.strip_fill_ns, s.job_ns),
            mib(s.strip_fill_bytes),
            s.strip_fill_calls,
            s.strip_fill_bytes as f64 * 1e9 / s.strip_fill_ns.max(1) as f64 / (1024.0 * 1024.0),
            ms(s.lag_fill_ns),
            share(s.lag_fill_ns, s.job_ns),
            mib(s.lag_fill_bytes),
            ms(s.seed_ns),
            share(s.seed_ns, s.job_ns),
        );
        println!(
            "  hash3 {:>10.2} ms ({:4.1}%)  prefill {:>10.2} ms ({:4.1}%)  reset {:>7.2} ms \
             ({:4.1}%)  parse+emit {:>8.2} ms ({:4.1}%)",
            ms(s.hash3_ns),
            share(s.hash3_ns, s.job_ns),
            ms(s.prefill_ns),
            share(s.prefill_ns, s.job_ns),
            ms(s.reset_ns),
            share(s.reset_ns, s.job_ns),
            ms(rest),
            share(rest, s.job_ns),
        );
        let fixed = s.strip_fill_ns + s.seed_ns + s.reset_ns + s.prefill_ns;
        println!(
            "  per job: fixed {:>7.3} ms of {:>7.3} ms job time ({:4.1}%)  [{} strips / {} jobs]",
            ms(fixed) / jobs as f64,
            ms(s.job_ns) / jobs as f64,
            share(fixed, s.job_ns),
            s.strip_fill_calls,
            jobs,
        );
    }

    fn run_cell(label: &str, raw: &[u8], encode: &mut dyn FnMut() -> Vec<u8>, iters: u32) {
        // Correctness gate before any timing.
        let comp = encode();
        assert_roundtrip(&comp, raw, label);
        println!("cell {label}  size {}", comp.len());

        let mut wall_sum = 0u128;
        let mut acc = job_trace::Snapshot::default();
        for _ in 0..iters {
            job_trace::reset();
            let t0 = Instant::now();
            let comp = encode();
            let wall = t0.elapsed();
            std::hint::black_box(&comp);
            wall_sum += wall.as_nanos();
            let s = job_trace::snapshot();
            println!(
                "  wall {:>8.2} ms   jobs {:>3}   job_sum {:>8.2} ms",
                ms(wall.as_nanos() as u64),
                s.jobs,
                ms(s.job_ns),
            );
            report(&s);
            accumulate(&mut acc, &s);
        }
        let mean_wall = wall_sum / iters as u128;
        println!(
            "  mean wall {:>8.2} ms  ({:>5.0} MiB/s)",
            ms(mean_wall as u64),
            raw.len() as f64 / (1024.0 * 1024.0) / (mean_wall as f64 / 1e9),
        );
        report(&acc);
        println!();
    }

    fn accumulate(acc: &mut job_trace::Snapshot, s: &job_trace::Snapshot) {
        let job_trace::Snapshot {
            jobs,
            job_ns,
            prefill_ns,
            reset_ns,
            seed_ns,
            strip_fill_ns,
            strip_fill_bytes,
            strip_fill_calls,
            lag_fill_ns,
            lag_fill_bytes,
            lag_fill_calls,
            hash3_ns,
            hash3_bytes,
        } = s;
        acc.jobs += jobs;
        acc.job_ns += job_ns;
        acc.prefill_ns += prefill_ns;
        acc.reset_ns += reset_ns;
        acc.seed_ns += seed_ns;
        acc.strip_fill_ns += strip_fill_ns;
        acc.strip_fill_bytes += strip_fill_bytes;
        acc.strip_fill_calls += strip_fill_calls;
        acc.lag_fill_ns += lag_fill_ns;
        acc.lag_fill_bytes += lag_fill_bytes;
        acc.lag_fill_calls += lag_fill_calls;
        acc.hash3_ns += hash3_ns;
        acc.hash3_bytes += hash3_bytes;
    }

    fn mode_name(mode: DecompMode) -> &'static str {
        match mode {
            DecompMode::BulkMt => "bulk-mt",
            DecompMode::StreamMt => "stream-mt",
            DecompMode::St => "st",
        }
    }

    pub fn run_trace(args: &JobDecompArgs) {
        let raw = fs::read(&args.file).unwrap();
        let workers = args.workers;
        for mode in &args.modes {
            for name in &args.level {
                let level = name.pair().0;
                let label = format!("{} {}", mode_name(*mode), name.tag());
                match mode {
                    DecompMode::BulkMt => {
                        run_cell(
                            &label,
                            &raw,
                            &mut || encode_bulk(&raw, level, workers),
                            args.iters,
                        );
                    },
                    DecompMode::StreamMt => {
                        run_cell(
                            &label,
                            &raw,
                            &mut || encode_stream(&raw, level, workers),
                            args.iters,
                        );
                    },
                    DecompMode::St => {
                        run_cell(
                            &label,
                            &raw,
                            &mut || encode_bulk(&raw, level, 1),
                            args.iters,
                        );
                    },
                }
            }
        }
    }
}
