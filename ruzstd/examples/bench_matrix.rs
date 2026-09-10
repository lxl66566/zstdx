//! Cross-matrix benchmark: ruzstd vs the zstd crate over decode/encode ×
//! bulk/streaming × single-/multi-thread at the corpus shapes.
//!
//! Usage: `cargo run --release --example bench_matrix [--] [mode]` where mode
//! is one of `dec-st`, `dec-mt`, `enc-st`, `enc-mt`, `enc-stream` (default:
//! all in sequence). `BENCH_BUDGET_MS` sets the per-side budget (see
//! `examples/common`); every cell is correctness-gated (roundtrip against
//! the raw file) before anything is timed.
//!
//! Cells and verdicts:
//! - `dec-st` interleaves both sides' bulk and streaming (64 KiB) decoders
//!   over the zst1/zst3/zst9 corpus variants. The streaming rows carry the
//!   real comparison: the zstd crate's bulk API is a slow per-chunk wrapper.
//! - `dec-mt` scales our parallel decoder (`DecoderOptions::threads`) solo;
//!   libzstd exposes no multithreaded decode, so the zstd column is its
//!   single-threaded streaming speed as a reference line.
//! - `enc-st` pairs the four ladder levels with zstd levels 1/3/6/12,
//!   checksums off on both sides (our checksum overhead is measured in a
//!   dedicated row); sizes and ratios are printed per cell.
//! - `enc-mt` interleaves both sides at equal worker counts with fresh
//!   contexts per call (the zstd crate has no per-call pool API; a warm
//!   reused context is reported once as a reference line).
//! - `enc-stream` compares the streaming encoders with 64 KiB pulls:
//!   single-threaded ruzstd vs zstd interleaved, then multithreaded
//!   (8 workers) ruzstd vs zstd interleaved.

#[path = "common/mod.rs"]
mod common;

use common::{black_box, measure_solo, Ab};
use ruzstd::decoding::{FrameDecoder, StreamingDecoder};
use ruzstd::{DecoderOptions, EncoderOptions, Level};
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;

const LADDER: [(&str, Level, i32); 6] = [
    ("fastest", Level::Fastest, 1),
    ("fast", Level::Fast, 3),
    ("balanced", Level::Balanced, 6),
    ("best", Level::Best, 12),
    ("opt", Level::Opt, 16),
    ("ultra", Level::Ultra, 19),
];

const SHAPES: [&str; 5] = ["json", "text", "skewed", "random", "zeros"];

const DEC_FILES: [&str; 11] = [
    "json.zst1",
    "json.zst3",
    "json.zst9",
    "text.zst1",
    "text.zst3",
    "text.zst9",
    "skewed.zst1",
    "skewed.zst3",
    "skewed.zst9",
    "random.zst3",
    "zeros.zst3",
];

/// Report name padded past `AbReport::print`'s name column so the numeric
/// columns line up despite the longer mode-prefixed names.
fn pad(name: &str) -> String {
    format!("{name:<24}")
}

fn corpus_dir() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("../bench/corpus");
    dir
}

fn load(name: &str) -> (Vec<u8>, Vec<u8>) {
    let dir = corpus_dir();
    let compressed = fs::read(dir.join(name)).unwrap();
    let raw = fs::read(dir.join(format!("{}.raw", name.split('.').next().unwrap()))).unwrap();
    (compressed, raw)
}

fn load_raw(shape: &str) -> Vec<u8> {
    fs::read(corpus_dir().join(format!("{shape}.raw"))).unwrap()
}

fn gate_ruz_dec(compressed: &[u8], raw: &[u8], label: &str) {
    let mut out = vec![0u8; raw.len()];
    FrameDecoder::new()
        .decode_all(compressed, &mut out)
        .unwrap_or_else(|e| panic!("ruzstd decode gate failed for {label}: {e}"));
    assert_eq!(&out[..], raw, "ruzstd gate mismatch for {label}");
}

fn gate_ruz_mt_dec(compressed: &[u8], raw: &[u8], threads: u32, label: &str) {
    let mut out = vec![0u8; raw.len()];
    ruzstd::bulk::decompress_to_buffer_with(
        compressed,
        &mut out,
        &DecoderOptions::new().threads(threads),
    )
    .unwrap_or_else(|e| panic!("ruzstd MT decode gate failed for {label}: {e}"));
    assert_eq!(&out[..], raw, "ruzstd MT gate mismatch for {label}");
}

fn gate_zstd_dec(compressed: &[u8], raw: &[u8], label: &str) {
    let mut decoded = Vec::new();
    zstd::stream::copy_decode(compressed, &mut decoded).unwrap();
    assert_eq!(&decoded[..], raw, "zstd gate mismatch for {label}");
}

fn gate_ruz_enc(raw: &[u8], level: Level, label: &str) -> Vec<u8> {
    let comp = ruzstd::bulk::compress_with(raw, &EncoderOptions::new(level).checksum(false));
    let mut back = Vec::with_capacity(raw.len() + 16);
    FrameDecoder::new()
        .decode_all_to_vec(&comp, &mut back)
        .unwrap();
    assert_eq!(&back[..], raw, "ruzstd encode gate mismatch for {label}");
    comp
}

fn gate_zstd_enc(raw: &[u8], level: i32, label: &str) -> Vec<u8> {
    let comp = zstd::bulk::compress(raw, level).unwrap();
    let mut back = Vec::new();
    zstd::stream::copy_decode(&comp[..], &mut back).unwrap();
    assert_eq!(&back[..], raw, "zstd encode gate mismatch for {label}");
    comp
}

fn assert_roundtrip(comp: &[u8], raw: &[u8], label: &str) {
    let mut back = Vec::new();
    zstd::stream::copy_decode(comp, &mut back).unwrap();
    assert_eq!(&back[..], raw, "roundtrip mismatch for {label}");
    let mut back2 = Vec::with_capacity(raw.len() + 16);
    FrameDecoder::new()
        .decode_all_to_vec(comp, &mut back2)
        .unwrap();
    assert_eq!(&back2[..], raw, "ruzstd roundtrip mismatch for {label}");
}

// ---------- decode, single-thread, bulk vs streaming ----------

fn t1_dec_st(ab: &Ab) {
    println!("== T1 decode ST (interleaved A/B; MiB/s of raw; xslow = ruz_time/zstd_time) ==");
    for f in DEC_FILES {
        let (comp, raw) = load(f);
        let bytes = raw.len() as u64;
        gate_ruz_dec(&comp, &raw, f);
        gate_zstd_dec(&comp, &raw, f);

        // bulk / slice path
        let mut fr = FrameDecoder::new();
        let mut out = vec![0u8; raw.len()];
        ab.measure(
            || {
                fr.decode_all(&comp, &mut out).unwrap();
                black_box(&out);
            },
            || {
                black_box(zstd::bulk::decompress(&comp, raw.len()).unwrap());
            },
        )
        .print(&pad(&format!("{f}.bulk")), bytes, "", "");

        // streaming path, 64 KiB reads
        ab.measure(
            || {
                let mut dec = StreamingDecoder::new(&comp[..]).unwrap();
                let mut sink = vec![0u8; 64 * 1024];
                let mut total = 0usize;
                loop {
                    let n = dec.read(&mut sink).unwrap();
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                assert_eq!(total, raw.len());
            },
            || {
                let mut dec = zstd::stream::read::Decoder::new(&comp[..]).unwrap();
                let mut sink = vec![0u8; 64 * 1024];
                let mut total = 0usize;
                loop {
                    let n = dec.read(&mut sink).unwrap();
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                assert_eq!(total, raw.len());
            },
        )
        .print(&pad(&format!("{f}.stream")), bytes, "", "");
    }
}

// ---------- decode, our MT scaling (zstd has no MT decode API) ----------

fn t2_dec_mt() {
    println!("== T2 decode MT scaling (solo; MiB/s; zstd has no MT decode counterpart) ==");
    for f in ["json.zst3", "text.zst3", "skewed.zst9", "random.zst3"] {
        let (comp, raw) = load(f);
        let bytes = raw.len() as u64;
        gate_ruz_dec(&comp, &raw, f);
        gate_ruz_mt_dec(&comp, &raw, 4, f);
        gate_zstd_dec(&comp, &raw, f);

        let mut fr = FrameDecoder::new();
        let mut out = vec![0u8; raw.len()];
        let st = measure_solo(|| {
            fr.decode_all(&comp, &mut out).unwrap();
            black_box(&out);
        });
        println!(
            "{:<24}{:>8.0}  (ours ST bulk)",
            format!("{f}.st1"),
            st.mibs(bytes)
        );

        let zref = measure_solo(|| {
            let mut dec = zstd::stream::read::Decoder::new(&comp[..]).unwrap();
            let mut sink = vec![0u8; 64 * 1024];
            loop {
                if dec.read(&mut sink).unwrap() == 0 {
                    break;
                }
            }
        });
        println!(
            "{:<24}{:>8.0}  (zstd ST stream reference)",
            format!("{f}.zstdref"),
            zref.mibs(bytes)
        );
        let zref_mibs = zref.mibs(bytes);

        for threads in [2u32, 4, 8, 16] {
            let mut out = vec![0u8; raw.len()];
            let stats = measure_solo(|| {
                ruzstd::bulk::decompress_to_buffer_with(
                    &comp,
                    &mut out,
                    &DecoderOptions::new().threads(threads),
                )
                .unwrap();
                black_box(&out);
            });
            println!(
                "{:<24}{:>8.0}  ({:4.2}x ours ST, {:4.2}x zstd stream ST)",
                format!("{f}.mt{threads}"),
                stats.mibs(bytes),
                st.median / stats.median,
                stats.mibs(bytes) / zref_mibs,
            );
        }
    }
}

// ---------- encode, single-thread bulk, ladder vs zstd levels ----------

fn t3_enc_st(ab: &Ab) {
    println!("== T3 encode ST bulk, checksums off (interleaved A/B; MiB/s of raw) ==");
    for shape in SHAPES {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in LADDER {
            let rc = gate_ruz_enc(&raw, level, label);
            let zc = gate_zstd_enc(&raw, z, label);
            println!(
                "sizes {shape}.{label:<9} ruz {:>9} (r {:7.2})   zstd {:>9} (r {:7.2})",
                rc.len(),
                raw.len() as f64 / rc.len() as f64,
                zc.len(),
                raw.len() as f64 / zc.len() as f64,
            );
            ab.measure(
                || {
                    black_box(ruzstd::bulk::compress_with(
                        &raw,
                        &EncoderOptions::new(level).checksum(false),
                    ));
                },
                || {
                    black_box(zstd::bulk::compress(&raw, z).unwrap());
                },
            )
            .print(&label_name(shape, label), bytes, "", "");
        }
    }

    // checksum overhead on our side (A = off, B = on; ratio < 1 means on is slower)
    println!("-- checksum overhead (ours; A/B = off/on time ratio) --");
    for shape in ["json", "text"] {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        ab.measure(
            || {
                black_box(ruzstd::bulk::compress_with(
                    &raw,
                    &EncoderOptions::new(Level::Fast).checksum(false),
                ));
            },
            || {
                black_box(ruzstd::bulk::compress_with(
                    &raw,
                    &EncoderOptions::new(Level::Fast).checksum(true),
                ));
            },
        )
        .print(&pad(&format!("{shape}.fast.cksum-on/off")), bytes, "", "");
    }
}

fn label_name(shape: &str, level: &str) -> String {
    pad(&format!("{shape}.{level}"))
}

// ---------- encode, MT bulk ----------

fn zstd_mt_comp(raw: &[u8], z: i32, workers: u32) -> Vec<u8> {
    // fresh context per call: symmetric with ruzstd's per-call worker pool
    let mut c = zstd::bulk::Compressor::new(z).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(workers))
        .unwrap();
    c.compress(raw).unwrap()
}

fn t4_enc_mt(ab: &Ab) {
    println!("== T4 encode MT bulk, checksums off (interleaved A/B at equal worker counts) ==");

    // worker scaling on the two CPU-bound shapes
    for shape in ["json", "text"] {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        let rc = gate_ruz_enc(&raw, Level::Fast, "mt");
        let zc = gate_zstd_enc(&raw, 3, "mt");
        println!(
            "sizes {shape}.fast-mt   ruz {:>9} (r {:7.2})   zstd(st) {:>9} (r {:7.2})",
            rc.len(),
            raw.len() as f64 / rc.len() as f64,
            zc.len(),
            raw.len() as f64 / zc.len() as f64,
        );
        for w in [2u32, 4, 8, 16, 32] {
            let a = ruzstd::bulk::compress_with(
                &raw,
                &EncoderOptions::new(Level::Fast).checksum(false).workers(w),
            );
            let b = zstd_mt_comp(&raw, 3, w);
            assert_roundtrip(&a, &raw, "ruz-mt");
            assert_roundtrip(&b, &raw, "zstd-mt");
            println!(
                "sizes {shape}.fast-mt{w:<2} ruz {:>9} (r {:7.2})   zstd-mt {:>9} (r {:7.2})",
                a.len(),
                raw.len() as f64 / a.len() as f64,
                b.len(),
                raw.len() as f64 / b.len() as f64,
            );
            ab.measure(
                || {
                    black_box(ruzstd::bulk::compress_with(
                        &raw,
                        &EncoderOptions::new(Level::Fast).checksum(false).workers(w),
                    ));
                },
                || {
                    black_box(zstd_mt_comp(&raw, 3, w));
                },
            )
            .print(&pad(&format!("{shape}.fast.mt{w}")), bytes, "", "");
        }
    }

    // fixed 16 workers across shapes and levels
    for shape in ["json", "text", "skewed"] {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in LADDER.iter().take(3) {
            let a = ruzstd::bulk::compress_with(
                &raw,
                &EncoderOptions::new(*level).checksum(false).workers(16),
            );
            let b = zstd_mt_comp(&raw, *z, 16);
            assert_roundtrip(&a, &raw, label);
            assert_roundtrip(&b, &raw, label);
            println!(
                "sizes {shape}.{label}.mt16 ruz {:>9} (r {:7.2})   zstd-mt {:>9} (r {:7.2})",
                a.len(),
                raw.len() as f64 / a.len() as f64,
                b.len(),
                raw.len() as f64 / b.len() as f64,
            );
            ab.measure(
                || {
                    black_box(ruzstd::bulk::compress_with(
                        &raw,
                        &EncoderOptions::new(*level).checksum(false).workers(16),
                    ));
                },
                || {
                    black_box(zstd_mt_comp(&raw, *z, 16));
                },
            )
            .print(&pad(&format!("{shape}.{label}.mt16")), bytes, "", "");
        }
    }

    // reference: libzstd keeps its worker pool inside a reused context
    println!(
        "-- zstd warm-pool reference (context reused across rounds; ruzstd has no such API) --"
    );
    let raw = load_raw("json");
    let bytes = raw.len() as u64;
    let mut warm = zstd::bulk::Compressor::new(3).unwrap();
    warm.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(16))
        .unwrap();
    let stats = measure_solo(|| {
        black_box(warm.compress(&raw).unwrap());
    });
    println!(
        "{:<24}{:>8.0}  (zstd json.fast mt16 warm)",
        "ref",
        stats.mibs(bytes)
    );
}

// ---------- encode, streaming, single-thread ----------

fn t5_enc_stream(ab: &Ab) {
    println!("== T5 encode streaming ST, checksums off (interleaved A/B; 64 KiB pulls) ==");
    for shape in ["json", "text"] {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in [
            ("fastest", Level::Fastest, 1),
            ("fast", Level::Fast, 3),
            ("best", Level::Best, 12),
        ] {
            // gate: both sides' streaming outputs must roundtrip to the raw input
            let mut comp = Vec::new();
            let mut enc = ruzstd::stream::read::Encoder::with_options(
                &raw[..],
                EncoderOptions::new(level).checksum(false),
            )
            .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish();
            assert_roundtrip(&comp, &raw, label);
            let mut zcomp = Vec::new();
            let mut zenc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
            zenc.read_to_end(&mut zcomp).unwrap();
            zenc.finish();
            assert_roundtrip(&zcomp, &raw, label);
            println!(
                "sizes {shape}.{label}.stream  ruz {}   zstd {}",
                comp.len(),
                zcomp.len()
            );

            ab.measure(
                || {
                    let mut enc = ruzstd::stream::read::Encoder::with_options(
                        &raw[..],
                        EncoderOptions::new(level).checksum(false),
                    )
                    .unwrap();
                    let mut sink = vec![0u8; 64 * 1024];
                    loop {
                        if enc.read(&mut sink).unwrap() == 0 {
                            break;
                        }
                    }
                    enc.finish();
                },
                || {
                    let mut enc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
                    let mut sink = vec![0u8; 64 * 1024];
                    loop {
                        if enc.read(&mut sink).unwrap() == 0 {
                            break;
                        }
                    }
                    enc.finish();
                },
            )
            .print(&pad(&format!("{shape}.{label}.stream")), bytes, "", "");
        }
    }

    // multithreaded streaming, both sides
    println!("== T5b encode streaming MT(8), checksums off (interleaved A/B; 64 KiB pulls) ==");
    for shape in ["json", "text"] {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in [
            ("fastest", Level::Fastest, 1),
            ("fast", Level::Fast, 3),
            ("balanced", Level::Balanced, 6),
            ("best", Level::Best, 12),
        ] {
            // gate: both sides' multithreaded streaming outputs must roundtrip
            let mut comp = Vec::new();
            let mut enc = ruzstd::stream::read::Encoder::with_options(
                &raw[..],
                EncoderOptions::new(level).checksum(false).workers(8),
            )
            .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish();
            assert_roundtrip(&comp, &raw, label);
            let mut zcomp = Vec::new();
            let mut zenc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
            zenc.multithread(8).unwrap();
            zenc.read_to_end(&mut zcomp).unwrap();
            zenc.finish();
            assert_roundtrip(&zcomp, &raw, label);
            println!(
                "sizes {shape}.{label}.stream-mt8  ruz {}   zstd {}",
                comp.len(),
                zcomp.len()
            );

            ab.measure(
                || {
                    let mut enc = ruzstd::stream::read::Encoder::with_options(
                        &raw[..],
                        EncoderOptions::new(level).checksum(false).workers(8),
                    )
                    .unwrap();
                    let mut sink = vec![0u8; 64 * 1024];
                    loop {
                        if enc.read(&mut sink).unwrap() == 0 {
                            break;
                        }
                    }
                    enc.finish();
                },
                || {
                    let mut enc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
                    enc.multithread(8).unwrap();
                    let mut sink = vec![0u8; 64 * 1024];
                    loop {
                        if enc.read(&mut sink).unwrap() == 0 {
                            break;
                        }
                    }
                    enc.finish();
                },
            )
            .print(&pad(&format!("{shape}.{label}.stream-mt8")), bytes, "", "");
        }

        // ceiling reference: our bulk mt path over the same bytes (the
        // streaming burst pipeline should approach it)
        for (label, level) in [
            ("fastest", Level::Fastest),
            ("fast", Level::Fast),
            ("balanced", Level::Balanced),
            ("best", Level::Best),
        ] {
            let comp = ruzstd::bulk::compress_with(
                &raw,
                &EncoderOptions::new(level).checksum(false).workers(8),
            );
            assert_roundtrip(&comp, &raw, label);
            let stats = measure_solo(|| {
                black_box(ruzstd::bulk::compress_with(
                    &raw,
                    &EncoderOptions::new(level).checksum(false).workers(8),
                ));
            });
            println!(
                "{:<32}{:>8.0}  (ruzstd {shape}.{label} bulk mt8 ceiling)",
                "ref",
                stats.mibs(bytes)
            );
        }
    }
}

fn main() {
    println!(
        "# bench_matrix: ruzstd vs zstd crate (libzstd {}, binding {}), {} cores",
        zstd::zstd_safe::version_string(),
        zstd::zstd_safe::version_number(),
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
    );
    let mode = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let ab = Ab::default();
    match mode.as_str() {
        "dec-st" => t1_dec_st(&ab),
        "dec-mt" => t2_dec_mt(),
        "enc-st" => t3_enc_st(&ab),
        "enc-mt" => t4_enc_mt(&ab),
        "enc-stream" => t5_enc_stream(&ab),
        "all" => {
            t1_dec_st(&ab);
            println!();
            t2_dec_mt();
            println!();
            t3_enc_st(&ab);
            println!();
            t4_enc_mt(&ab);
            println!();
            t5_enc_stream(&ab);
        }
        m => panic!("unknown mode {m}"),
    }
}
