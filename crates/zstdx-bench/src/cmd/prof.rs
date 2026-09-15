//! Solo profiling loops for `perf` attribution: no reference side, just a
//! tight loop over one file so samples land in one code path.
//!
//! Modes (the arguments after the mode mirror the old standalone tools):
//! - `prof dec <file.zst> [iters=5]`: streaming decode with 64 KiB reads.
//! - `prof enc <zstd-level> <iters> <file...>`: bulk encode; `RUZ_CKSUM` in the environment
//!   switches to the checksummed bulk path.
//! - `prof enc-stream <zstd-level> <iters> <file> [workers=1]`: streaming encode with 64 KiB
//!   writes; a worker count above one selects the mt streaming core.
//! - `prof enc-stream-read <zstd-level> <iters> <file> [workers=1]`: the read-shaped streaming
//!   encoder over an in-memory source with 64 KiB output pulls — the exact shape the matrix
//!   `enc-stream` cells time (checksums off).

use std::{
    fs,
    io::{Read as _, Write as _},
    time::Instant,
};

use zstdx::decoding::StreamingDecoder;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ProfMode {
    /// Streaming decode of one file.
    Dec,
    /// Bulk encode of one or more files.
    Enc,
    /// Streaming (write) encode of one file.
    EncStream,
    /// Streaming (read) encode of one file, the matrix enc-stream shape.
    EncStreamRead,
}

#[derive(clap::Args)]
pub struct Args {
    pub mode: ProfMode,
    /// Mode-specific arguments (see the module docs).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> &'a str {
    args.get(i)
        .unwrap_or_else(|| panic!("missing argument {i} ({what})"))
        .as_str()
}

fn num<T: std::str::FromStr>(args: &[String], i: usize, what: &str) -> T {
    arg(args, i, what)
        .parse()
        .unwrap_or_else(|_| panic!("bad {what}: {}", arg(args, i, what)))
}

fn run_dec(rest: &[String]) {
    let path = arg(rest, 0, "file");
    let iters: u64 = rest.get(1).map_or(5, |s| s.parse().unwrap());
    let compressed = fs::read(path).unwrap();
    let raw_len = {
        let mut d = StreamingDecoder::new(&compressed[..]).unwrap();
        let mut sink = vec![0u8; 64 * 1024];
        let mut total = 0usize;
        loop {
            let n = d.read(&mut sink).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    };
    let mut sink = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    for _ in 0..iters {
        let mut d = StreamingDecoder::new(&compressed[..]).unwrap();
        let mut acc = 0u64;
        loop {
            let n = d.read(&mut sink).unwrap();
            if n == 0 {
                break;
            }
            acc += n as u64;
        }
        assert_eq!(acc, raw_len as u64);
        std::hint::black_box(acc);
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "{path}: {iters} iters, {:.0} MiB/s",
        iters as f64 * raw_len as f64 / dt / (1 << 20) as f64
    );
}

fn run_enc(rest: &[String]) {
    let level = zstdx::Level::from_zstd(num::<i32>(rest, 0, "zstd level"));
    let iters: usize = num(rest, 1, "iters");
    for path in &rest[2..] {
        let raw = fs::read(path).unwrap();
        let mut acc = 0usize;
        let t = Instant::now();
        for _ in 0..iters {
            let c = if std::env::var("RUZ_CKSUM").is_ok() {
                zstdx::bulk::compress(&raw, level)
            } else {
                zstdx::encoding::compress_slice_opts(&raw, level, false)
            };
            acc ^= c.len();
        }
        let el = t.elapsed().as_secs_f64();
        println!(
            "{path}: {} bytes, xor={acc}, {:.0} MiB/s ({el:.3}s)",
            raw.len(),
            raw.len() as f64 / 1024.0 / 1024.0 / (el / iters as f64)
        );
    }
}

fn run_enc_stream(rest: &[String]) {
    let level = zstdx::Level::from_zstd(num::<i32>(rest, 0, "zstd level"));
    let iters: usize = num(rest, 1, "iters");
    let path = arg(rest, 2, "file");
    let workers: u32 = rest
        .get(3)
        .map_or(1, |w| w.parse().expect("workers must be a number"));
    let raw = fs::read(path).unwrap();
    let mut sink = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    for _ in 0..iters {
        let mut enc = zstdx::stream::write::Encoder::with_options(
            Vec::new(),
            zstdx::EncoderOptions::new(level).workers(workers),
        )
        .unwrap();
        let mut off = 0usize;
        loop {
            let n = chunk.len().min(raw.len() - off);
            chunk[..n].copy_from_slice(&raw[off..off + n]);
            off += n;
            enc.write_all(&chunk[..n]).unwrap();
            if off == raw.len() {
                break;
            }
        }
        sink = enc.finish().unwrap();
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "{path} L{level:?} w{workers}: {iters} iters, out={} bytes, {:.0} MiB/s",
        sink.len(),
        iters as f64 * raw.len() as f64 / dt / (1 << 20) as f64
    );
}

fn run_enc_stream_read(rest: &[String]) {
    let level = zstdx::Level::from_zstd(num::<i32>(rest, 0, "zstd level"));
    let iters: usize = num(rest, 1, "iters");
    let path = arg(rest, 2, "file");
    let workers: u32 = rest
        .get(3)
        .map_or(1, |w| w.parse().expect("workers must be a number"));
    let raw = fs::read(path).unwrap();
    let mut sink = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    let mut total_out = 0usize;
    for _ in 0..iters {
        let mut enc = zstdx::stream::read::Encoder::with_options(
            &raw[..],
            zstdx::EncoderOptions::new(level)
                .checksum(false)
                .workers(workers),
        )
        .unwrap();
        let mut acc = 0u64;
        loop {
            let n = enc.read(&mut sink).unwrap();
            if n == 0 {
                break;
            }
            acc += n as u64;
        }
        enc.finish();
        total_out = acc as usize;
        std::hint::black_box(acc);
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "{path} L{level:?} w{workers}: {iters} iters, out={total_out} bytes, {:.0} MiB/s",
        iters as f64 * raw.len() as f64 / dt / (1 << 20) as f64
    );
}

pub fn run(args: &Args) {
    match args.mode {
        ProfMode::Dec => run_dec(&args.rest),
        ProfMode::Enc => run_enc(&args.rest),
        ProfMode::EncStream => run_enc_stream(&args.rest),
        ProfMode::EncStreamRead => run_enc_stream_read(&args.rest),
    }
}
