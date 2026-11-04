//! Forced-tier decode differential: the decode hot loops' SIMD tier is
//! resolved once per process (`simd_tier`'s OnceLock), so the scalar and
//! BMI2 bodies cannot be run on the same inputs inside one process. The
//! gate is therefore cross-process: the parent re-execs this binary as a
//! child under `ZSTDX_DEC_SIMD_TIER=scalar` and `=bmi2`, and compares the
//! per-cell output (length plus hash) of every decode the child ran. Any
//! divergence — a length or hash mismatch, a child error or a child that
//! stops emitting lines — fails the run.

use std::{collections::BTreeMap, path::PathBuf, process::Command};

use zstdx::decoding::{FrameDecoder, StreamingDecoder};

/// The decode cells each child runs per file, one output line each. The
/// X2 interleaved loop and the executor's BMI2 tier run in all three; the
/// split keeps a regression attributable to one decode shape.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Cell {
    /// Streaming decoder (HEADROOM flat loop, windowed buffer).
    Stream,
    /// One-shot decode_all into an exact-size target: the staged executor.
    Flat,
    /// decode_all into a target with one block maximum plus pad, literals
    /// and tail of slack, so the in-dst staging gate can engage.
    InDst,
}

impl Cell {
    const ALL: [Cell; 3] = [Cell::Stream, Cell::Flat, Cell::InDst];

    const fn name(self) -> &'static str {
        match self {
            Cell::Stream => "stream",
            Cell::Flat => "flat",
            Cell::InDst => "indst",
        }
    }
}

/// The room the in-dst gate needs beyond the frame's output in the worst
/// case: one block maximum plus its pad, one more block maximum of
/// literals, plus the over-read tail.
const IN_DST_SLACK: usize = 2 * (128 * 1024) + 2 * 16;

/// FNV-1a 64: enough to catch any operand or template divergence in the
/// asm stretch, and dependency-free.
fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)) * 0x0000_0100_0000_01b3
    })
}

#[derive(clap::Args)]
pub struct Args {
    /// Files to decode. Defaults to every `.zst*` variant in bench/corpus
    /// and, when present, bench/big.
    pub files: Vec<PathBuf>,
    /// Child mode: decode every file in this process under the tier the
    /// parent forced, printing one `<len> <hash> <mode> <file>` line per
    /// cell. Never pass this by hand.
    #[arg(long, hide = true)]
    pub emit_hashes: bool,
}

/// The default cell set: the corpus `.zst*` variants plus the big payload
/// variants when `bench/gen_big.sh` has run.
fn default_files() -> Vec<PathBuf> {
    let mut dirs = vec![crate::corpus::corpus_dir()];
    let big = crate::corpus::corpus_dir()
        .parent()
        .map(|p| p.join("big"))
        .unwrap_or_default();
    if big.is_dir() {
        dirs.push(big);
    }
    let mut files: Vec<_> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap())
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.to_str().is_some_and(|x| x.starts_with("zst")))
        })
        .collect();
    files.sort();
    files
}

/// One child's answers, keyed by (file, cell).
fn run_child(tier: &str, files: &[PathBuf]) -> BTreeMap<(PathBuf, Cell), (usize, u64)> {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.env("ZSTDX_DEC_SIMD_TIER", tier)
        .arg("tierdiff")
        .arg("--emit-hashes")
        .args(files);
    let out = cmd.output().expect("spawn tierdiff child");
    assert!(
        out.status.success(),
        "tierdiff child (tier {tier}) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("child stdout utf8");
    let mut map = BTreeMap::new();
    for line in text.lines() {
        // <len> <hash hex> <mode> <file>
        let (len, rest) = line
            .split_once(' ')
            .unwrap_or_else(|| panic!("bad line {line}"));
        let (hash, rest) = rest
            .split_once(' ')
            .unwrap_or_else(|| panic!("bad line {line}"));
        let (mode, file) = rest
            .split_once(' ')
            .unwrap_or_else(|| panic!("bad line {line}"));
        let cell = match mode {
            "stream" => Cell::Stream,
            "flat" => Cell::Flat,
            "indst" => Cell::InDst,
            other => panic!("bad cell name {other} in {line}"),
        };
        map.insert(
            (PathBuf::from(file), cell),
            (
                len.parse().unwrap_or_else(|_| panic!("bad len in {line}")),
                u64::from_str_radix(hash, 16).unwrap_or_else(|_| panic!("bad hash in {line}")),
            ),
        );
    }
    map
}

/// Child half: decode `file` once per cell and print the hash lines.
fn emit_hashes(files: &[PathBuf]) {
    for path in files {
        let compressed = std::fs::read(path).unwrap();
        let mut streamed = Vec::new();
        {
            let mut dec = StreamingDecoder::new(&compressed[..]).unwrap();
            std::io::Read::read_to_end(&mut dec, &mut streamed).unwrap();
        }
        println!(
            "{} {:x} stream {}",
            streamed.len(),
            fnv1a64(&streamed),
            path.display()
        );
        // Exact-size target: the gate declines, the staged executor runs.
        let mut flat = vec![0u8; streamed.len()];
        let written = FrameDecoder::new()
            .decode_all(&compressed, &mut flat)
            .unwrap();
        assert_eq!(written, flat.len());
        println!(
            "{} {:x} flat {}",
            flat.len(),
            fnv1a64(&flat),
            path.display()
        );
        // Oversized target: room for one block maximum plus pad, literals
        // and tail beyond the frame's output, so blocks with enough
        // literals take the in-dst twin.
        let mut indst = vec![0u8; streamed.len() + IN_DST_SLACK];
        let written = FrameDecoder::new()
            .decode_all(&compressed, &mut indst)
            .unwrap();
        assert_eq!(written, streamed.len());
        println!(
            "{} {:x} indst {}",
            written,
            fnv1a64(&indst[..written]),
            path.display()
        );
    }
}

pub fn run(args: &Args) {
    if args.emit_hashes {
        emit_hashes(&args.files);
        return;
    }
    let files = if args.files.is_empty() {
        default_files()
    } else {
        args.files.clone()
    };
    assert!(!files.is_empty(), "no .zst* files found under bench/corpus");
    // The parent itself never decodes: the tier resolves once per process,
    // so the comparison must stay entirely inside the forced children.
    let scalar = run_child("scalar", &files);
    let bmi2 = run_child("bmi2", &files);
    let mut fail = 0usize;
    for path in &files {
        for cell in Cell::ALL {
            let key = (path.clone(), cell);
            let a = scalar.get(&key);
            let b = bmi2.get(&key);
            match (a, b) {
                (Some(a), Some(b)) if a == b => {},
                (a, b) => {
                    println!(
                        "DIFF {} {}: scalar {:?}, bmi2 {:?}",
                        path.display(),
                        cell.name(),
                        a,
                        b
                    );
                    fail += 1;
                },
            }
        }
        println!("done {}", path.display());
    }
    if fail == 0 {
        println!(
            "ALL OK ({} files, {} cells)",
            files.len(),
            files.len() * Cell::ALL.len()
        );
    } else {
        println!("{fail} FAILURES");
        std::process::exit(1);
    }
}
