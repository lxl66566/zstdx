//! Deterministic strip-fill replay: the streaming chain row's per-job
//! prefill cost (`Matcher::reset` + `prefill_window`) at the strip lengths
//! the far-class schedule picks, on the dll shape the far-alive class runs
//! on. The wall attribution of the streaming l9 cells (growing-grid jobs
//! refilling a whole-window strip per job) lives in
//! `docs/src/dev/bench/ffi-stream-gap.md`; this bench is its deterministic
//! Ir gate: strip-length sweeps and fill-path changes read here without
//! scheduler noise.

// gungraun's macros emit `::`-rooted paths into the generated harness,
// which trips the workspace `unused_qualifications` lint at the expansion
// spans.
#![allow(unused_qualifications)]

use std::{fs, hint::black_box, io::Write as _, path::PathBuf};

use gungraun::prelude::*;
use zstdx::{
    EncoderOptions, InputShape, LdmMode, Level,
    encoding::{MatchGeneratorDriver, Matcher},
};

/// The replayed window: 64 MiB (the chain row's whole-window strip at the
/// l9 row window) taken at a mid-stream offset of `dll100.raw`, so every
/// strip arm is the suffix of the same window — the tail the seed scan and
/// the gate probe read stays byte-identical across arms.
const WIN: usize = 64 << 20;
const BASE: u64 = 36 << 20;
/// Job prefills replayed per benchmark: one driver, reset between strips
/// exactly the way the streaming jobs pool and reset their states.
const REPLAYS: usize = 8;

fn dll_window() -> Vec<u8> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../bench/big/dll100.raw");
    let raw = fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read corpus file {}: {e}; regenerate with bench/gen_big.sh",
            path.display()
        )
    });
    assert!(
        raw.len() >= BASE as usize + WIN,
        "dll100.raw is smaller than the {WIN}-byte window at offset {BASE}"
    );
    raw[BASE as usize..BASE as usize + WIN].to_vec()
}

/// Replay `REPLAYS` job prefills of the `strip_mib`-MiB suffix of the
/// window (the streaming job's strip is the history immediately below its
/// start).
fn replay(win: &[u8], strip_mib: usize) {
    let len = strip_mib << 20;
    let strip = &win[WIN - len..];
    let base = BASE + (WIN - len) as u64;
    let mut m = MatchGeneratorDriver::new_direct();
    for _ in 0..REPLAYS {
        m.reset(Level::from_zstd(9));
        m.prefill_window(strip, base);
    }
    // The tables are the fill's only observable; keep the driver alive
    // across the barrier so no fill can be elided.
    black_box(&mut m);
}

#[library_benchmark]
#[bench::m4((dll_window(), 4))]
#[bench::m12((dll_window(), 12))]
#[bench::m24((dll_window(), 24))]
#[bench::m48((dll_window(), 48))]
#[bench::m64((dll_window(), 64))]
fn strip_fill((win, strip_mib): (Vec<u8>, usize)) {
    replay(&win, strip_mib)
}

/// The FFI caller shape of `ffi-stream-gap` end to end: the whole
/// `dll100.raw` fed as an unpledged stream in 128 KiB continues with the
/// total as the src-size hint, l9, eight workers. The far-alive arm runs
/// the far class on (Auto; dll's repeats dominate) so every Growing-grid
/// job refills a whole-window strip; the far-dead arm runs the same bytes
/// with LDM Disable, which starts the frame far-dead — strips cap at the
/// 4 MiB reach and job LDM disarms. The content is constant, so the arms'
/// Ir difference is the far-alive strip machinery's whole share of the
/// stream. Thread-scheduled, so treat single-digit-percent drift between
/// repeat runs as noise when comparing.
fn stream_ffishape(raw: &[u8], ldm: LdmMode) -> usize {
    let opts = EncoderOptions::new(Level::from_zstd(9))
        .workers(8)
        .with_input_shape(InputShape {
            len: Some(raw.len() as u64),
            window_log: None,
        })
        .long_distance_matching(ldm);
    let mut sink = Vec::with_capacity(raw.len() / 2 + 128 * 1024);
    {
        let mut enc = zstdx::stream::write::Encoder::with_options(&mut sink, opts).unwrap();
        for chunk in raw.chunks(128 * 1024) {
            enc.write_all(chunk).unwrap();
        }
        enc.finish().unwrap();
    }
    black_box(sink.len())
}

fn dll100() -> Vec<u8> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../bench/big/dll100.raw");
    fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read corpus file {}: {e}; regenerate with bench/gen_big.sh",
            path.display()
        )
    })
}

#[library_benchmark]
#[bench::far_alive((dll100(), LdmMode::Auto))]
#[bench::far_dead((dll100(), LdmMode::Disable))]
fn stream_l9((raw, ldm): (Vec<u8>, LdmMode)) -> usize {
    stream_ffishape(&raw, ldm)
}

library_benchmark_group!(name = stripfill, benchmarks = [strip_fill, stream_l9]);

// Invoked at item position: the macro expands to the `fn main` itself.
main!(library_benchmark_groups = [stripfill]);
