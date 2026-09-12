//! Cross-matrix benchmark: zstdx vs the zstd crate over decode/encode ×
//! bulk/streaming × single-/multi-thread at the corpus shapes.
//!
//! This is the heavyweight tool; narrow it with `--shape`, `--level`,
//! `--workers` and `--mode` instead of running `all` while iterating.
//! `--budget-ms` sets the per-side budget (see `common`); every cell is
//! correctness-gated (roundtrip against the raw file) before anything is
//! timed.
//!
//! Sections and verdicts:
//! - `dec-st` interleaves both sides' bulk and streaming (64 KiB) decoders over the zst1/zst3/zst9
//!   corpus variants. The streaming rows carry the real comparison: the zstd crate's bulk API is a
//!   slow per-chunk wrapper.
//! - `dec-mt` scales our parallel decoder (`DecoderOptions::threads`) solo; libzstd exposes no
//!   multithreaded decode, so the zstd column is its single-threaded streaming speed as a reference
//!   line.
//! - `enc-st` pairs the ladder levels with libzstd at the same numeric levels (1/3/9/13/17/19),
//!   checksums off on both sides (our checksum overhead is measured in a dedicated row); sizes and
//!   ratios are printed per cell. `--full-ladder` swaps the axis to every numeric level 1-22 with
//!   both sides at the same number — extremely heavy, a release-gate pass, never daily iteration.
//! - `enc-mt` interleaves both sides at equal worker counts with fresh contexts per call (the zstd
//!   crate has no per-call pool API; a warm reused context is reported once as a reference line).
//! - `enc-stream` compares the streaming encoders with 64 KiB pulls: single-threaded zstdx vs zstd
//!   interleaved, then multithreaded (`--mt-workers`, default 8) zstdx vs zstd interleaved, each
//!   followed by our bulk mt path over the same bytes as the ceiling reference.

use std::io::Read as _;

use zstdx::{
    DecoderOptions, EncoderOptions, Level,
    decoding::{FrameDecoder, StreamingDecoder},
};

use crate::{
    common::{Ab, apply_budget, black_box, measure_solo, want},
    corpus::{
        LADDER, LevelName, LevelSel, SHAPES, Shape, assert_roundtrip, gate_ruz_dec, gate_ruz_enc,
        gate_ruz_mt_dec, gate_zstd_dec, gate_zstd_enc, load, load_raw, want_number, want_tier,
    },
};

/// Curated decode set: the informative level/shape combinations (the
/// omitted variants are redundant with their neighbours).
const DEC_FILES: [(&str, Shape); 11] = [
    ("json.zst1", Shape::Json),
    ("json.zst3", Shape::Json),
    ("json.zst9", Shape::Json),
    ("text.zst1", Shape::Text),
    ("text.zst3", Shape::Text),
    ("text.zst9", Shape::Text),
    ("skewed.zst1", Shape::Skewed),
    ("skewed.zst3", Shape::Skewed),
    ("skewed.zst9", Shape::Skewed),
    ("random.zst3", Shape::Random),
    ("zeros.zst3", Shape::Zeros),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum MatrixMode {
    All,
    DecSt,
    DecMt,
    EncSt,
    EncMt,
    EncStream,
}

#[derive(clap::Args)]
pub struct Args {
    /// Matrix section to run
    #[arg(long, value_enum, default_value_t = MatrixMode::All)]
    pub mode: MatrixMode,
    /// Corpus shapes to include (comma-separated)
    #[arg(long, value_enum, value_delimiter = ',')]
    pub shape: Vec<Shape>,
    /// Encoder levels to include: ladder tiers or numeric 1-22 (a number
    /// off the tier levels requires --full-ladder)
    #[arg(long, value_delimiter = ',', value_parser = clap::value_parser!(LevelSel))]
    pub level: Vec<LevelSel>,
    /// Worker counts for the scaling sweeps
    #[arg(long, value_delimiter = ',')]
    pub workers: Vec<u32>,
    /// Fixed worker count for the mt and streaming-mt sections
    #[arg(long, default_value_t = 8)]
    pub mt_workers: u32,
    /// enc-st over every numeric level 1-22, both sides at the same number;
    /// extremely heavy (release-gate only, never for daily iteration)
    #[arg(long)]
    pub full_ladder: bool,
    /// Per-side measurement budget in milliseconds
    #[arg(long)]
    pub budget_ms: Option<f64>,
}

/// Report name padded past `AbReport::print`'s name column so the numeric
/// columns line up despite the longer mode-prefixed names.
fn pad(name: &str) -> String {
    format!("{name:<24}")
}

fn shapes_selected(args: &Args) -> Vec<Shape> {
    SHAPES
        .iter()
        .copied()
        .filter(|s| want(&args.shape, s))
        .collect()
}

/// One level cell of an encode section: row tag, our level, libzstd level.
type LevelCell = (String, Level, i32);

/// The tier axis (default): a section's ladder subset filtered by `--level`.
fn ladder_axis(args: &Args, of: &[LevelName]) -> Vec<LevelCell> {
    of.iter()
        .filter(|l| want_tier(&args.level, l))
        .map(|l| {
            let (ours, z) = l.pair();
            (l.tag().to_owned(), ours, z)
        })
        .collect()
}

/// The numeric axis (`--full-ladder`): every level 1-22, both sides at the
/// same number, filtered by `--level` (tier names select their own number).
fn numeric_axis(args: &Args) -> Vec<LevelCell> {
    (Level::Fastest.as_i32()..=Level::MAX.as_i32())
        .filter(|n| want_number(&args.level, *n))
        .map(|n| (format!("l{n}"), Level::from_zstd(n), n))
        .collect()
}

/// The enc-st level axis: numeric 1-22 with `--full-ladder`, the tier
/// ladder otherwise. The mt/stream sections keep their curated tier subsets.
fn enc_axis(args: &Args) -> Vec<LevelCell> {
    if args.full_ladder {
        numeric_axis(args)
    } else {
        ladder_axis(args, &LADDER)
    }
}

/// A numeric `--level` that misses every tier would silently select nothing
/// on the tier axis; point at `--full-ladder` instead.
fn validate_levels(args: &Args) {
    if args.full_ladder {
        return;
    }
    let tiers = LADDER
        .iter()
        .map(|t| t.pair().0.as_i32().to_string())
        .collect::<Vec<_>>()
        .join("/");
    for sel in &args.level {
        assert!(
            sel.tier().is_some(),
            "--level {}: no ladder tier sits at this number (tiers: {tiers}); pass --full-ladder \
             for the numeric 1-22 axis",
            sel.numeric().as_i32()
        );
    }
}

// ---------- decode, single-thread, bulk vs streaming ----------

fn t1_dec_st(ab: &Ab, args: &Args) {
    println!("== T1 decode ST (interleaved A/B; MiB/s of raw; xslow = ruz_time/zstd_time) ==");
    for (f, shape) in DEC_FILES {
        if !want(&args.shape, &shape) {
            continue;
        }
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
        .print(&pad(&format!("{f}.bulk")), bytes);

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
        .print(&pad(&format!("{f}.stream")), bytes);
    }
}

// ---------- decode, our MT scaling (zstd has no MT decode API) ----------

fn t2_dec_mt(args: &Args) {
    println!("== T2 decode MT scaling (solo; MiB/s; zstd has no MT decode counterpart) ==");
    let workers: Vec<u32> = [2u32, 4, 8, 16]
        .into_iter()
        .filter(|w| want(&args.workers, w))
        .collect();
    for (f, shape) in [
        ("json.zst3", Shape::Json),
        ("text.zst3", Shape::Text),
        ("skewed.zst9", Shape::Skewed),
        ("random.zst3", Shape::Random),
    ] {
        if !want(&args.shape, &shape) {
            continue;
        }
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

        for threads in &workers {
            let mut out = vec![0u8; raw.len()];
            let stats = measure_solo(|| {
                zstdx::bulk::decompress_to_buffer_with(
                    &comp,
                    &mut out,
                    &DecoderOptions::new().threads(*threads),
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

fn t3_enc_st(ab: &Ab, args: &Args) {
    println!("== T3 encode ST bulk, checksums off (interleaved A/B; MiB/s of raw) ==");
    for shape in shapes_selected(args) {
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (tag, level, z) in enc_axis(args) {
            let rc = gate_ruz_enc(&raw, level, &tag);
            let zc = gate_zstd_enc(&raw, z, &tag);
            println!(
                "sizes {}.{}  ruz {:>9} (r {:7.2})   zstd {:>9} (r {:7.2})",
                shape.raw_name(),
                tag,
                rc.len(),
                raw.len() as f64 / rc.len() as f64,
                zc.len(),
                raw.len() as f64 / zc.len() as f64,
            );
            ab.measure(
                || {
                    black_box(compress_with_ok(
                        &raw,
                        &EncoderOptions::new(level).checksum(false),
                    ));
                },
                || {
                    black_box(zstd::bulk::compress(&raw, z).unwrap());
                },
            )
            .print(&pad(&format!("{}.{}", shape.raw_name(), tag)), bytes);
        }
    }

    if !want_tier(&args.level, &LevelName::Fast) {
        return;
    }
    // checksum overhead on our side (A = off, B = on; ratio < 1 means on is slower)
    println!("-- checksum overhead (ours; A/B = off/on time ratio) --");
    for shape in shapes_selected(args) {
        if !matches!(shape, Shape::Json | Shape::Text) {
            continue;
        }
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        ab.measure(
            || {
                black_box(compress_with_ok(
                    &raw,
                    &EncoderOptions::new(Level::Fast).checksum(false),
                ));
            },
            || {
                black_box(compress_with_ok(
                    &raw,
                    &EncoderOptions::new(Level::Fast).checksum(true),
                ));
            },
        )
        .print(
            &pad(&format!("{}.fast.cksum-on/off", shape.raw_name())),
            bytes,
        );
    }
}

// ---------- encode, MT bulk ----------

fn compress_with_ok(raw: &[u8], options: &EncoderOptions) -> Vec<u8> {
    zstdx::bulk::compress_with(raw, options).unwrap()
}

fn zstd_mt_comp(raw: &[u8], z: i32, workers: u32) -> Vec<u8> {
    // fresh context per call: symmetric with zstdx's per-call worker pool
    let mut c = zstd::bulk::Compressor::new(z).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(workers))
        .unwrap();
    c.compress(raw).unwrap()
}

fn t4_enc_mt(ab: &Ab, args: &Args) {
    println!("== T4 encode MT bulk, checksums off (interleaved A/B at equal worker counts) ==");
    let sweep: Vec<u32> = [2u32, 4, 8, 16, 32]
        .into_iter()
        .filter(|w| want(&args.workers, w))
        .collect();
    let mt = args.mt_workers;

    // worker scaling on the two CPU-bound shapes
    for shape in [Shape::Json, Shape::Text] {
        if !want(&args.shape, &shape) {
            continue;
        }
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        let rc = gate_ruz_enc(&raw, Level::Fast, "mt");
        let zc = gate_zstd_enc(&raw, 3, "mt");
        println!(
            "sizes {}.fast-mt   ruz {:>9} (r {:7.2})   zstd(st) {:>9} (r {:7.2})",
            shape.raw_name(),
            rc.len(),
            raw.len() as f64 / rc.len() as f64,
            zc.len(),
            raw.len() as f64 / zc.len() as f64,
        );
        for w in &sweep {
            let a = compress_with_ok(
                &raw,
                &EncoderOptions::new(Level::Fast).checksum(false).workers(*w),
            );
            let b = zstd_mt_comp(&raw, 3, *w);
            assert_roundtrip(&a, &raw, "ruz-mt");
            assert_roundtrip(&b, &raw, "zstd-mt");
            println!(
                "sizes {}.fast-mt{w:<2} ruz {:>9} (r {:7.2})   zstd-mt {:>9} (r {:7.2})",
                shape.raw_name(),
                a.len(),
                raw.len() as f64 / a.len() as f64,
                b.len(),
                raw.len() as f64 / b.len() as f64,
            );
            ab.measure(
                || {
                    black_box(compress_with_ok(
                        &raw,
                        &EncoderOptions::new(Level::Fast).checksum(false).workers(*w),
                    ));
                },
                || {
                    black_box(zstd_mt_comp(&raw, 3, *w));
                },
            )
            .print(&pad(&format!("{}.fast.mt{w}", shape.raw_name())), bytes);
        }
    }

    // fixed worker count across shapes and levels
    for shape in [Shape::Json, Shape::Text, Shape::Skewed] {
        if !want(&args.shape, &shape) {
            continue;
        }
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in ladder_axis(args, &LADDER[..3]) {
            let a = compress_with_ok(
                &raw,
                &EncoderOptions::new(level).checksum(false).workers(mt),
            );
            let b = zstd_mt_comp(&raw, z, mt);
            assert_roundtrip(&a, &raw, &label);
            assert_roundtrip(&b, &raw, &label);
            println!(
                "sizes {}.{label}.mt{mt} ruz {:>9} (r {:7.2})   zstd-mt {:>9} (r {:7.2})",
                shape.raw_name(),
                a.len(),
                raw.len() as f64 / a.len() as f64,
                b.len(),
                raw.len() as f64 / b.len() as f64,
            );
            ab.measure(
                || {
                    black_box(compress_with_ok(
                        &raw,
                        &EncoderOptions::new(level).checksum(false).workers(mt),
                    ));
                },
                || {
                    black_box(zstd_mt_comp(&raw, z, mt));
                },
            )
            .print(&pad(&format!("{}.{label}.mt{mt}", shape.raw_name())), bytes);
        }
    }

    // reference: libzstd keeps its worker pool inside a reused context
    println!(
        "-- zstd warm-pool reference (context reused across rounds; zstdx has no such API) --"
    );
    let raw = load_raw(Shape::Json);
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

// ---------- encode, streaming ----------

fn t5_enc_stream(ab: &Ab, args: &Args) {
    println!("== T5 encode streaming ST, checksums off (interleaved A/B; 64 KiB pulls) ==");
    for shape in [Shape::Json, Shape::Text] {
        if !want(&args.shape, &shape) {
            continue;
        }
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in ladder_axis(args, &[
            LevelName::Fastest,
            LevelName::Fast,
            LevelName::Best,
        ]) {
            // gate: both sides' streaming outputs must roundtrip to the raw input
            let mut comp = Vec::new();
            let mut enc = zstdx::stream::read::Encoder::with_options(
                &raw[..],
                EncoderOptions::new(level).checksum(false),
            )
            .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish();
            assert_roundtrip(&comp, &raw, &label);
            let mut zcomp = Vec::new();
            let mut zenc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
            zenc.read_to_end(&mut zcomp).unwrap();
            zenc.finish();
            assert_roundtrip(&zcomp, &raw, &label);
            println!(
                "sizes {}.{label}.stream  ruz {}   zstd {}",
                shape.raw_name(),
                comp.len(),
                zcomp.len()
            );

            ab.measure(
                || {
                    let mut enc = zstdx::stream::read::Encoder::with_options(
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
            .print(&pad(&format!("{}.{label}.stream", shape.raw_name())), bytes);
        }
    }

    // multithreaded streaming, both sides
    let mt = args.mt_workers;
    println!("== T5b encode streaming MT({mt}), checksums off (interleaved A/B; 64 KiB pulls) ==");
    for shape in [Shape::Json, Shape::Text] {
        if !want(&args.shape, &shape) {
            continue;
        }
        let raw = load_raw(shape);
        let bytes = raw.len() as u64;
        for (label, level, z) in ladder_axis(args, &[
            LevelName::Fastest,
            LevelName::Fast,
            LevelName::Balanced,
            LevelName::Best,
        ]) {
            // gate: both sides' multithreaded streaming outputs must roundtrip
            let mut comp = Vec::new();
            let mut enc = zstdx::stream::read::Encoder::with_options(
                &raw[..],
                EncoderOptions::new(level).checksum(false).workers(mt),
            )
            .unwrap();
            enc.read_to_end(&mut comp).unwrap();
            enc.finish();
            assert_roundtrip(&comp, &raw, &label);
            let mut zcomp = Vec::new();
            let mut zenc = zstd::stream::read::Encoder::new(&raw[..], z).unwrap();
            zenc.multithread(mt).unwrap();
            zenc.read_to_end(&mut zcomp).unwrap();
            zenc.finish();
            assert_roundtrip(&zcomp, &raw, &label);
            println!(
                "sizes {}.{label}.stream-mt{mt}  ruz {}   zstd {}",
                shape.raw_name(),
                comp.len(),
                zcomp.len()
            );

            ab.measure(
                || {
                    let mut enc = zstdx::stream::read::Encoder::with_options(
                        &raw[..],
                        EncoderOptions::new(level).checksum(false).workers(mt),
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
                    enc.multithread(mt).unwrap();
                    let mut sink = vec![0u8; 64 * 1024];
                    loop {
                        if enc.read(&mut sink).unwrap() == 0 {
                            break;
                        }
                    }
                    enc.finish();
                },
            )
            .print(
                &pad(&format!("{}.{label}.stream-mt{mt}", shape.raw_name())),
                bytes,
            );
        }

        // ceiling reference: our bulk mt path over the same bytes (the
        // streaming burst pipeline should approach it)
        for (label, level, _) in ladder_axis(args, &[
            LevelName::Fastest,
            LevelName::Fast,
            LevelName::Balanced,
            LevelName::Best,
        ]) {
            let comp = compress_with_ok(
                &raw,
                &EncoderOptions::new(level).checksum(false).workers(mt),
            );
            assert_roundtrip(&comp, &raw, &label);
            let stats = measure_solo(|| {
                black_box(compress_with_ok(
                    &raw,
                    &EncoderOptions::new(level).checksum(false).workers(mt),
                ));
            });
            println!(
                "{:<32}{:>8.0}  (zstdx {}.{label} bulk mt{mt} ceiling)",
                "ref",
                stats.mibs(bytes),
                shape.raw_name()
            );
        }
    }
}

pub fn run(args: &Args) {
    apply_budget(args.budget_ms);
    validate_levels(args);
    println!(
        "# bench matrix: zstdx vs zstd crate (libzstd {}, binding {}), {} cores",
        zstd::zstd_safe::version_string(),
        zstd::zstd_safe::version_number(),
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
    );
    let ab = Ab::default();
    match args.mode {
        MatrixMode::DecSt => t1_dec_st(&ab, args),
        MatrixMode::DecMt => t2_dec_mt(args),
        MatrixMode::EncSt => t3_enc_st(&ab, args),
        MatrixMode::EncMt => t4_enc_mt(&ab, args),
        MatrixMode::EncStream => t5_enc_stream(&ab, args),
        MatrixMode::All => {
            t1_dec_st(&ab, args);
            println!();
            t2_dec_mt(args);
            println!();
            t3_enc_st(&ab, args);
            println!();
            t4_enc_mt(&ab, args);
            println!();
            t5_enc_stream(&ab, args);
        },
    }
}
