//! Decode-parity differential against the libzstd CLI: a deterministic
//! craft+mutate corpus (the R64/R65 classes) replayed through zstdx
//! (serial and MT arms) and `zstd -dcq`, comparing accept/reject and
//! output bytes per case. The R65 /tmp harness institutionalized
//! (todo 21b/21c); classes, OR-scan rationale and sweep history:
//! docs/src/dev/review/diffbench.md.
//!
//! Each case shells out to the CLI once (the reference is the CLI by
//! design); a full sweep is multi-core work — wrap wide runs in
//! `flock /root/programs/fork/zstd-bench.lock`. Exits nonzero on any
//! hard divergence so it can gate. The one class where the CLI
//! (streaming) and any one-shot decoder legitimately disagree
//! (window-violating offsets, R65 parity.md finding 7) is adjudicated by
//! libzstd's own one-shot (`ZSTD_decompress` via the zstd crate) and
//! reported as an asymmetry instead of failing the gate.

use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

use rayon::prelude::*;
use zstdx::{
    DecoderOptions, EncoderOptions, Level, bulk,
    decoding::errors::FrameDecoderError,
    parity::{self, CraftedName},
};

/// Deterministic payload seed (any fixed value; `--seed` overrides).
const DEFAULT_SEED: u64 = 0x0642_7a5e_0113_fd2f;

/// Frame headers are at most 18 bytes; the header OR scan covers exactly
/// that prefix (magic + descriptor + dict id + window + FCS).
const HEADER_SCAN: usize = 18;

/// OR masks for the header scan; `0x03` is also swept over every position
/// (the class that found the modes-byte reserved-bits hole XOR misses:
/// `mode |= 3` leaves the mode bits untouched).
const OR_MASKS: [u8; 4] = [0x01, 0x03, 0x80, 0xff];

/// XOR masks rotate by position (R65: 0xFF/0x01/0x80 rotated).
const XOR_MASKS: [u8; 5] = [0xff, 0x01, 0x80, 0xc0, 0x60];

/// Inserted-byte values, rotating by position.
const INSERT_BYTES: [u8; 3] = [0x00, 0xff, 0x80];

/// Position stride for the delete/insert classes (every byte would
/// roughly double the corpus for the least interesting classes).
const SPARSE_STRIDE: usize = 4;

/// Output-buffer ladder ceiling. A mutated case bounded by its input
/// length cannot legitimately decode past this (RLE blocks are capped at
/// min(window, 128 KiB) and consume >= 4 input bytes each); hitting the
/// ceiling means a runaway, reported as a zstdx-side reject.
const MAX_OUTPUT: usize = 512 << 20;

#[derive(clap::Args)]
pub struct Args {
    /// Corpus RNG seed (payload generation is a pure function of it).
    #[arg(long, default_value_t = DEFAULT_SEED)]
    pub seed: u64,
    /// Approximate case budget; 0 keeps the exhaustive default scale.
    /// Sampling is a deterministic stride that always keeps the clean
    /// replay cases.
    #[arg(long, default_value_t = 0)]
    pub cases: usize,
    /// Dictionary (raw or formatted) attached to both decoders; real
    /// seeds are encoded with it (dict-attached replay, todo 21a prep).
    #[arg(long)]
    pub dict: Option<PathBuf>,
    /// MT decode arms (worker counts) beyond the serial arm. With
    /// `--dict` these run the sequential dictionary path (the parallel
    /// decoder plans over raw frame bytes alone).
    #[arg(long, value_delimiter = ',', default_value = "4")]
    pub mt: Vec<u32>,
    /// Write every divergent case's bytes into this directory (one
    /// `<n>-<source>-<mutation>.bin` per case) for offline replay.
    #[arg(long)]
    pub dump: Option<PathBuf>,
    /// Debug helper: decode one file with the serial arm and print the
    /// verdict (replaying a `--dump` case). No corpus runs.
    #[arg(long)]
    pub decode: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// Payload families of the real-seed frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedShape {
    /// Word-pool text: dense short matches.
    Text,
    /// 4-bit alphabet: entropy-heavy but compressible.
    SemiRandom,
    /// Byte runs alternating with random bursts: RLE/mixed blocks.
    RleMixed,
    /// Small structured records.
    Records,
}

impl SeedShape {
    const fn tag(self) -> &'static str {
        match self {
            SeedShape::Text => "text",
            SeedShape::SemiRandom => "semirandom",
            SeedShape::RleMixed => "rlemixed",
            SeedShape::Records => "records",
        }
    }

    fn generate(self, len: usize, rng: &mut Rng) -> Vec<u8> {
        const WORDS: [&str; 12] = [
            "frame", "match", "distance", "window", "encode", "stream", "prefix", "check", "level",
            "table", "block", "hash",
        ];
        let mut out = Vec::with_capacity(len + 16);
        match self {
            SeedShape::Text => {
                while out.len() < len {
                    out.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
                    out.push(b' ');
                }
            },
            SeedShape::SemiRandom => {
                while out.len() < len {
                    out.push(rng.byte() & 0x0f);
                }
            },
            SeedShape::RleMixed => {
                while out.len() < len {
                    let byte = rng.byte();
                    for _ in 0..(4 + rng.below(36)) {
                        out.push(byte);
                    }
                    for _ in 0..(4 + rng.below(12)) {
                        out.push(rng.byte());
                    }
                }
            },
            SeedShape::Records => {
                let mut i = 0u64;
                while out.len() < len {
                    let word = WORDS[rng.below(WORDS.len())];
                    out.extend_from_slice(
                        format!("{{\"k\":{i},\"v\":\"{word}\",\"f\":{}}}\n", i % 2).as_bytes(),
                    );
                    i += 1;
                }
            },
        }
        out.truncate(len);
        out
    }
}

/// Payload plan: family and size. Three levels and a checksum flip per
/// (family, level) give 24 real frames.
const SEED_PLAN: [(SeedShape, usize); 8] = [
    (SeedShape::Text, 600),
    (SeedShape::Text, 2400),
    (SeedShape::SemiRandom, 300),
    (SeedShape::SemiRandom, 1600),
    (SeedShape::RleMixed, 500),
    (SeedShape::RleMixed, 2000),
    (SeedShape::Records, 320),
    (SeedShape::Records, 1200),
];
const SEED_LEVELS: [i32; 3] = [1, 3, 19];

/// Where a case's bytes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Seed {
        shape: SeedShape,
        size: usize,
        level: i32,
        checksum: bool,
    },
    Crafted(CraftedName),
}

impl Source {
    fn tag(self) -> String {
        match self {
            Source::Seed {
                shape,
                size,
                level,
                checksum,
            } => format!(
                "{}.{}.l{}{}",
                shape.tag(),
                size,
                level,
                if checksum {
                    ".chk"
                } else {
                    ""
                }
            ),
            Source::Crafted(name) => format!("crafted.{}", name.tag()),
        }
    }
}

/// Small deterministic RNG for payload generation (xorshift64*).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() >> 32) as u8
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() >> 32) as usize % n
    }
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// Keep the first `len` bytes (exhaustive truncation ladder).
    Truncate {
        len: usize,
    },
    XorFlip {
        pos: usize,
        mask: u8,
    },
    /// OR scan restricted to the header prefix.
    OrHeader {
        pos: usize,
        mask: u8,
    },
    /// OR scan over every position (the modes-byte class).
    OrScan {
        pos: usize,
    },
    Delete {
        pos: usize,
    },
    Insert {
        pos: usize,
        byte: u8,
    },
}

impl Mutation {
    const fn class(self) -> Class {
        match self {
            Mutation::Truncate { .. } => Class::Truncate,
            Mutation::XorFlip { .. } => Class::XorFlip,
            Mutation::OrHeader { .. } => Class::OrHeader,
            Mutation::OrScan { .. } => Class::OrScan,
            Mutation::Delete { .. } => Class::Delete,
            Mutation::Insert { .. } => Class::Insert,
        }
    }

    fn apply(self, base: &[u8]) -> Vec<u8> {
        match self {
            Mutation::Truncate { len } => base[..len].to_vec(),
            Mutation::XorFlip { pos, mask } => {
                let mut out = base.to_vec();
                out[pos] ^= mask;
                out
            },
            Mutation::OrHeader { pos, mask } => {
                let mut out = base.to_vec();
                out[pos] |= mask;
                out
            },
            Mutation::OrScan { pos } => {
                let mut out = base.to_vec();
                out[pos] |= 0x03;
                out
            },
            Mutation::Delete { pos } => {
                let mut out = base.to_vec();
                out.remove(pos);
                out
            },
            Mutation::Insert { pos, byte } => {
                let mut out = base.to_vec();
                out.insert(pos, byte);
                out
            },
        }
    }
}

impl std::fmt::Display for Mutation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mutation::Truncate { len } => write!(f, "truncate->{len}"),
            Mutation::XorFlip { pos, mask } => write!(f, "xor @{pos} mask={mask:#04x}"),
            Mutation::OrHeader { pos, mask } => write!(f, "or-hdr @{pos} mask={mask:#04x}"),
            Mutation::OrScan { pos } => write!(f, "or @{pos} mask=0x03"),
            Mutation::Delete { pos } => write!(f, "delete @{pos}"),
            Mutation::Insert { pos, byte } => write!(f, "insert @{pos} byte={byte:#04x}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Clean,
    Truncate,
    XorFlip,
    OrHeader,
    OrScan,
    Delete,
    Insert,
}

impl Class {
    const ALL: [Class; 7] = [
        Class::Clean,
        Class::Truncate,
        Class::XorFlip,
        Class::OrHeader,
        Class::OrScan,
        Class::Delete,
        Class::Insert,
    ];

    const fn tag(self) -> &'static str {
        match self {
            Class::Clean => "clean",
            Class::Truncate => "truncate",
            Class::XorFlip => "xor",
            Class::OrHeader => "or-header",
            Class::OrScan => "or-scan",
            Class::Delete => "delete",
            Class::Insert => "insert",
        }
    }
}

#[derive(Clone, Copy)]
struct Case {
    frame: usize,
    mutation: Option<Mutation>,
}

/// Expand one base frame into its mutation descriptors (the clean case
/// itself lives in the case list, not here).
fn expand(frame: usize, len: usize) -> Vec<Case> {
    let mut out = Vec::new();
    for pos in 0..len {
        out.push(Case {
            frame,
            mutation: Some(Mutation::Truncate { len: pos }),
        });
        out.push(Case {
            frame,
            mutation: Some(Mutation::XorFlip {
                pos,
                mask: XOR_MASKS[pos % XOR_MASKS.len()],
            }),
        });
        out.push(Case {
            frame,
            mutation: Some(Mutation::OrScan { pos }),
        });
    }
    for pos in 0..len.min(HEADER_SCAN) {
        for mask in OR_MASKS {
            out.push(Case {
                frame,
                mutation: Some(Mutation::OrHeader { pos, mask }),
            });
        }
    }
    for pos in (0..len).step_by(SPARSE_STRIDE) {
        out.push(Case {
            frame,
            mutation: Some(Mutation::Delete { pos }),
        });
    }
    for pos in (0..=len).step_by(SPARSE_STRIDE) {
        out.push(Case {
            frame,
            mutation: Some(Mutation::Insert {
                pos,
                byte: INSERT_BYTES[pos % INSERT_BYTES.len()],
            }),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Verdict {
    Accept(Vec<u8>),
    Reject(String),
}

impl Verdict {
    fn tag(&self) -> String {
        match self {
            Verdict::Accept(out) => format!("Accept({})", out.len()),
            Verdict::Reject(e) => format!("Reject({e})"),
        }
    }
}

fn cli_verdict(input: &[u8], dict: Option<&Path>) -> Verdict {
    let mut cmd = Command::new("zstd");
    cmd.arg("-dcq")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dict) = dict {
        cmd.arg("-D").arg(dict);
    }
    let mut child = cmd.spawn().expect("zstd CLI in PATH");
    // Inputs are far below the pipe buffer, so writing stdin to completion
    // before reading stdout cannot deadlock.
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write CLI stdin");
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut out)
        .expect("read CLI stdout");
    if child.wait().expect("wait CLI").success() {
        Verdict::Accept(out)
    } else {
        Verdict::Reject("cli".into())
    }
}

/// zstdx verdict through the public one-shot entry with an output ladder
/// (the bulk entry's own growth loop is uncapped). Panics count as
/// crashes: reported as divergences, never swallowed.
fn zstdx_verdict(input: &[u8], options: &DecoderOptions) -> Verdict {
    let mut cap = 64 << 10;
    loop {
        let mut out = vec![0u8; cap];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bulk::decompress_to_buffer_with(input, &mut out, options)
        }));
        match result {
            Ok(Ok(n)) => return Verdict::Accept(out[..n].to_vec()),
            Ok(Err(e)) => match &e {
                zstdx::Error::Frame(FrameDecoderError::TargetTooSmall) if cap < MAX_OUTPUT => {
                    cap *= 4;
                },
                _ => return Verdict::Reject(e.to_string()),
            },
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                return Verdict::Reject(format!("PANIC: {msg}"));
            },
        }
    }
}

/// libzstd's own one-shot (`ZSTD_decompress`): the adjudicator for the
/// documented CLI-streaming vs one-shot asymmetry class. `None` = could
/// not decide (the ladder ceiling was reached).
fn libzstd_oneshot(input: &[u8]) -> Option<Verdict> {
    let mut cap = 64 << 20;
    loop {
        let mut out = vec![0u8; cap];
        match zstd::zstd_safe::decompress(&mut out, input) {
            Ok(n) => return Some(Verdict::Accept(out[..n].to_vec())),
            Err(code) => {
                let msg = code.to_string();
                // Destination-too-small is the ladder signal; anything
                // else is a genuine one-shot rejection.
                if msg.to_lowercase().contains("too small") && cap < MAX_OUTPUT {
                    cap *= 4;
                    continue;
                }
                return Some(Verdict::Reject(msg));
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mismatch {
    /// zstdx accepts what the CLI rejects.
    AcceptsWhatRejects,
    /// zstdx rejects what the CLI accepts.
    RejectsWhatAccepts,
    /// Both accept, lengths differ.
    Length { zstdx: usize, cli: usize },
    /// Both accept, bytes differ (first differing offset).
    Bytes { first: usize },
}

impl Mismatch {
    fn describe(&self) -> String {
        match self {
            Mismatch::AcceptsWhatRejects => "zstdx accepts, cli rejects".into(),
            Mismatch::RejectsWhatAccepts => "zstdx rejects, cli accepts".into(),
            Mismatch::Length { zstdx, cli } => format!("length {zstdx} vs {cli}"),
            Mismatch::Bytes { first } => format!("bytes first-diff @{first}"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// zstdx disagrees with libzstd under matched APIs: a real bug.
    Hard,
    /// zstdx matches libzstd's one-shot and only the CLI's streaming
    /// verdict differs: the documented API-pair asymmetry. Informational.
    OneshotAsymmetry,
}

struct Divergence {
    case: Case,
    source: Source,
    arm: String,
    kind: Kind,
    mismatch: Mismatch,
    zstdx: Verdict,
    cli: Verdict,
    case_bytes: Vec<u8>,
}

/// Compare a zstdx verdict against the CLI verdict. `None` = agree.
fn mismatch(zstdx: &Verdict, cli: &Verdict) -> Option<Mismatch> {
    match (zstdx, cli) {
        (Verdict::Accept(a), Verdict::Accept(b)) => {
            if a.len() != b.len() {
                return Some(Mismatch::Length {
                    zstdx: a.len(),
                    cli: b.len(),
                });
            }
            a.iter()
                .zip(b.iter())
                .position(|(x, y)| x != y)
                .map(|first| Mismatch::Bytes { first })
        },
        (Verdict::Accept(_), Verdict::Reject(_)) => Some(Mismatch::AcceptsWhatRejects),
        (Verdict::Reject(_), Verdict::Accept(_)) => Some(Mismatch::RejectsWhatAccepts),
        (Verdict::Reject(_), Verdict::Reject(_)) => None,
    }
}

/// Classify a mismatch: hard unless zstdx's verdict (bytes included)
/// matches libzstd's one-shot and only the CLI differs.
fn classify(zstdx: &Verdict, cli: &Verdict, bytes: &[u8]) -> Kind {
    if matches!(zstdx, Verdict::Reject(e) if e.starts_with("PANIC:")) {
        return Kind::Hard;
    }
    let Some(reference) = libzstd_oneshot(bytes) else {
        return Kind::Hard;
    };
    let matches_reference = match (zstdx, &reference) {
        (Verdict::Accept(a), Verdict::Accept(b)) => a == b,
        (Verdict::Reject(_), Verdict::Reject(_)) => true,
        _ => false,
    };
    let reference_differs_from_cli = mismatch(&reference, cli).is_some();
    if matches_reference && reference_differs_from_cli {
        Kind::OneshotAsymmetry
    } else {
        Kind::Hard
    }
}

fn decode_options(threads: u32, dict: Option<&[u8]>) -> DecoderOptions {
    let mut opts = DecoderOptions::new().threads(threads);
    if let Some(dict) = dict {
        opts = opts.dictionary(dict);
    }
    opts
}

pub fn run(args: &Args) {
    if let Some(file) = &args.decode {
        let bytes = std::fs::read(file).expect("read --decode file");
        let dict_bytes = args
            .dict
            .as_deref()
            .map(std::fs::read)
            .map(|r| r.expect("read --dict"));
        let verdict = zstdx_verdict(&bytes, &decode_options(0, dict_bytes.as_deref()));
        println!("{}: {}", file.display(), verdict.tag());
        if let Verdict::Accept(out) = verdict {
            println!("first 32: {:02x?}", &out[..out.len().min(32)]);
        }
        let reference = match libzstd_oneshot(&bytes) {
            Some(v) => v.tag(),
            None => "inconclusive".to_string(),
        };
        println!("libzstd one-shot: {reference}");
        println!("cli: {}", cli_verdict(&bytes, args.dict.as_deref()).tag());
        return;
    }

    let dict_bytes = args
        .dict
        .as_deref()
        .map(std::fs::read)
        .map(|r| r.expect("read --dict"));
    let dict = dict_bytes.as_deref();

    // --- real seeds (payload + frame + roundtrip gate)
    let mut frames: Vec<(Source, Vec<u8>)> = Vec::new();
    let serial_opts = decode_options(0, dict);
    for (i, (shape, size)) in SEED_PLAN.iter().enumerate() {
        let mut payload_rng = Rng::new(args.seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let payload = shape.generate(*size, &mut payload_rng);
        for (j, level) in SEED_LEVELS.iter().enumerate() {
            let checksum = (i + j) % 2 == 0;
            let mut opts = EncoderOptions::new(Level::from_zstd(*level)).checksum(checksum);
            if let Some(dict) = dict {
                opts = opts.dictionary(dict);
            }
            let frame = bulk::compress_with(&payload, &opts).expect("seed encode");
            // Corpus sanity gate: every seed must roundtrip through our
            // own decoder, else the corpus itself is broken.
            match zstdx_verdict(&frame, &serial_opts) {
                Verdict::Accept(out) => assert!(
                    out == payload,
                    "seed roundtrip mismatch ({} B frame)",
                    frame.len()
                ),
                Verdict::Reject(e) => panic!("seed does not decode: {e}"),
            }
            frames.push((
                Source::Seed {
                    shape: *shape,
                    size: *size,
                    level: *level,
                    checksum,
                },
                frame,
            ));
        }
    }
    let seed_bytes: usize = frames.iter().map(|(_, f)| f.len()).sum();
    let seed_count = frames.len();

    // --- crafted frames
    for (name, frame) in parity::crafted_frames() {
        frames.push((Source::Crafted(name), frame));
    }

    // --- case list: clean replay first, then every mutation
    let mut cases: Vec<Case> = frames
        .iter()
        .enumerate()
        .map(|(frame, _)| Case {
            frame,
            mutation: None,
        })
        .collect();
    cases.extend(
        frames
            .iter()
            .enumerate()
            .flat_map(|(frame, (_, base))| expand(frame, base.len())),
    );

    let mut sampled_note = "";
    if args.cases > 0 && cases.len() > args.cases {
        let stride = cases.len().div_ceil(args.cases);
        let mut kept = Vec::with_capacity(args.cases + frames.len());
        for (i, case) in cases.drain(..).enumerate() {
            if i % stride == 0 || case.mutation.is_none() {
                kept.push(case);
            }
        }
        cases = kept;
        sampled_note = " (stride-sampled)";
    }

    // --- arms: serial plus every distinct MT worker count > 1
    let arms: Vec<(String, DecoderOptions)> = {
        let mut arms = vec![("serial".to_string(), decode_options(0, dict))];
        let mut seen: Vec<u32> = Vec::new();
        for workers in &args.mt {
            if *workers > 1 && !seen.contains(workers) {
                seen.push(*workers);
                arms.push((format!("mt{workers}"), decode_options(*workers, dict)));
            }
        }
        arms
    };

    // --- replay
    let progress = AtomicUsize::new(0);
    let total = cases.len();
    let divergences: Vec<Divergence> = cases
        .par_iter()
        .filter_map(|case| {
            let (source, base) = &frames[case.frame];
            let bytes = match case.mutation {
                None => base.clone(),
                Some(m) => m.apply(base),
            };
            let cli = cli_verdict(&bytes, args.dict.as_deref());
            let mut found = None;
            for (arm, options) in &arms {
                let zstdx = zstdx_verdict(&bytes, options);
                if let Some(m) = mismatch(&zstdx, &cli) {
                    found = Some(Divergence {
                        case: *case,
                        source: *source,
                        arm: arm.clone(),
                        kind: classify(&zstdx, &cli, &bytes),
                        mismatch: m,
                        zstdx,
                        cli: cli.clone(),
                        case_bytes: bytes.clone(),
                    });
                    break;
                }
            }
            let n = progress.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(4096) {
                eprintln!("[{n}/{total}] cases");
            }
            found
        })
        .collect();

    // --- summary
    let mut class_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        let class = case.mutation.map_or(Class::Clean, Mutation::class).tag();
        *class_counts.entry(class).or_insert(0) += 1;
    }
    let classes = Class::ALL
        .iter()
        .filter_map(|c| {
            class_counts
                .get(c.tag())
                .map(|n| format!("{} {}", c.tag(), n))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let arm_names = arms
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join("+");
    let hard = divergences.iter().filter(|d| d.kind == Kind::Hard).count();
    let asym = divergences
        .iter()
        .filter(|d| d.kind == Kind::OneshotAsymmetry)
        .count();

    println!(
        "parity{sampled_note}: {} cases ({} real seeds, {} B frames + {} crafted)",
        cases.len(),
        seed_count,
        seed_bytes,
        frames.len() - seed_count,
    );
    println!("classes: {classes}");
    println!(
        "arms: {arm_names} (1 CLI spawn + {} zstdx decodes per case)",
        arms.len()
    );
    let hard_shown = divergences.iter().filter(|d| d.kind == Kind::Hard).count();
    let asym_shown = 4;
    let mut hard_left = hard_shown;
    let mut asym_left = asym_shown;
    for d in &divergences {
        let label = match d.kind {
            Kind::Hard => "DIVERGENCE",
            Kind::OneshotAsymmetry => "oneshot-asymmetry",
        };
        let left = match d.kind {
            Kind::Hard => &mut hard_left,
            Kind::OneshotAsymmetry => &mut asym_left,
        };
        if *left == 0 {
            continue;
        }
        *left -= 1;
        let mutation = match d.case.mutation {
            Some(m) => m.to_string(),
            None => "clean".to_string(),
        };
        println!(
            "{label} [{}] {} {}: {} (zstdx {}, cli {})",
            d.arm,
            d.source.tag(),
            mutation,
            d.mismatch.describe(),
            d.zstdx.tag(),
            d.cli.tag()
        );
        if let Some(dir) = &args.dump {
            let file = format!(
                "{}-{}-{}.bin",
                divergences
                    .iter()
                    .position(|x| std::ptr::eq(x, d))
                    .unwrap_or(0),
                d.source.tag(),
                mutation.replace([' ', '@', '=', '/', '>'], "-")
            );
            let path = dir.join(&file);
            if let Err(e) = std::fs::write(&path, &d.case_bytes) {
                println!("  (dump failed: {e})");
            } else {
                println!("  len {} dumped {}", d.case_bytes.len(), path.display());
            }
        } else {
            let head = &d.case_bytes[..d.case_bytes.len().min(48)];
            let mut hex = String::with_capacity(2 * head.len());
            for b in head {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
            }
            println!(
                "  len {} bytes[0..{}]={}",
                d.case_bytes.len(),
                head.len(),
                hex
            );
        }
    }
    if hard_shown > hard_shown.min(1000) {
        println!("... and {} more hard", hard_shown - 1000);
    }
    if asym_shown < asym {
        println!("... and {} more oneshot-asymmetry", asym - asym_shown);
    }
    println!("divergences: {hard} hard, {asym} oneshot-asymmetry");
    if hard > 0 {
        println!("PARITY FAILED");
        std::process::exit(1);
    }
    println!("PARITY OK");
}
