//! Piece-pipeline critical-path analysis for parallel stage-B sequence
//! execution: decodes a frame through the `seq_dump` executor hook (every
//! match's absolute output position and resolved offset, exactly as
//! executed), then schedules the sequences as independently executing
//! "pieces" cut at uniform output boundaries. The unlimited-worker
//! schedule is the exact critical path under rate-1 execution (one output
//! byte per tick), so serial-wall / piece-wall upper-bounds what any
//! piece-parallel stage B could win on that frame — no thread machinery
//! needed.
//!
//! The per-piece binding statistic is the minimum CROSSING match offset
//! (a read below the piece start): a piece's completion-interval lower
//! bound is (piece_size - o_min), so the pipeline ceiling is piece_size /
//! (piece_size - o_min) — the number an encoder-side deep-offset ramp
//! would have to move.

#[cfg(feature = "seq_dump")]
use std::fs;
use std::path::PathBuf;

#[cfg(feature = "seq_dump")]
use zstdx::decoding::FrameDecoder;

#[derive(clap::Args)]
pub struct Args {
    /// Frame to analyze (raw counterpart resolved as <stem>.raw)
    file: PathBuf,
    /// Uniform piece size in bytes (the MT job size), default 4 MiB
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    job_size: u64,
    /// Worker counts for the bounded-lane schedule (unlimited always shown)
    #[arg(long, value_delimiter = ',', default_value = "2,4,8,16")]
    workers: Vec<u32>,
    /// Encoder-side depth guarantee to audit: count crossing reads whose
    /// depth below the piece start violates it (0 = skip the audit)
    #[arg(long, default_value_t = 0)]
    depth: u64,
}

/// One resolved sequence: match copy at output [match_pos, match_pos + ml)
/// reading source `src` (absolute output position).
#[cfg(feature = "seq_dump")]
struct Resolved {
    match_pos: u64,
    ml: u32,
    src: u64,
}

pub fn run(args: &Args) {
    #[cfg(not(feature = "seq_dump"))]
    {
        let _ = args;
        eprintln!(
            "error: piecepipe needs the decoder dump hook; rebuild with `--features seq_dump`"
        );
    }
    #[cfg(feature = "seq_dump")]
    run_dump(args);
}

#[cfg(feature = "seq_dump")]
#[allow(clippy::many_single_char_names)]
fn run_dump(args: &Args) {
    let comp = fs::read(&args.file).expect("read frame");
    let stem = args
        .file
        .file_stem()
        .expect("file stem")
        .to_string_lossy()
        .into_owned();
    let raw_path = args.file.with_file_name(format!("{stem}.raw"));
    let raw = fs::read(&raw_path).unwrap_or_else(|_| {
        panic!(
            "raw counterpart {} missing (decode still needs it for sizing)",
            raw_path.display()
        )
    });

    let mut out = vec![0u8; raw.len()];
    let mut dec = FrameDecoder::new();
    let n = dec
        .decode_all(&comp, &mut out)
        .unwrap_or_else(|e| panic!("decode: {e}")) as u64;
    assert_eq!(&out[..n as usize], &raw[..], "frame must roundtrip");
    let exec = zstdx::decoding::seq_dump::take_exec();

    // The executor's ground-truth log gives every match's absolute output
    // position and resolved offset directly — no wire-value repcode
    // resolution, no per-block position re-anchoring (a greedy wire walk
    // mis-assigns sequences whose end coincides with a block end, and
    // json's repetitive bytes let the error hide behind coincidental
    // byte-verifies for thousands of sequences).
    let mut resolved: Vec<Resolved> = Vec::with_capacity(exec.len());
    for &(match_pos, ml, offset) in &exec {
        assert!(offset > 0 && offset <= match_pos, "bad exec record");
        resolved.push(Resolved {
            match_pos,
            ml,
            src: match_pos - offset,
        });
    }

    // Exact resolution gate: a correctly placed and offset-resolved match
    // copies bytes that are already in the output. Any mismatch means the
    // walk (positions or rep history) diverged from the decoder, and every
    // dependency derived below is garbage.
    for r in &resolved {
        let m = r.match_pos as usize;
        let s = r.src as usize;
        let l = r.ml as usize;
        assert!(
            out[s..s + l] == out[m..m + l],
            "resolved match at {m} (src {s}, ml {l}) does not verify against the output"
        );
    }

    let job = args.job_size.max(1);
    let n_pieces = n.div_ceil(job) as usize;
    let piece_of = |p: u64| ((p / job) as usize).min(n_pieces - 1);
    let piece_start = |k: usize| k as u64 * job;
    let piece_end = |k: usize| ((k as u64 + 1) * job).min(n);

    // Crossing-read statistics: the shallowest crossing offset per piece.
    let mut per_piece_min = vec![u64::MAX; n_pieces];
    let mut crossing = 0usize;
    let mut depth_violations = 0usize;
    for r in &resolved {
        let k = piece_of(r.match_pos);
        let start = piece_start(k);
        if r.src < start {
            crossing += 1;
            per_piece_min[k] = per_piece_min[k].min(r.match_pos - r.src);
            if args.depth != 0 && start - r.src < args.depth {
                depth_violations += 1;
                if std::env::var("ZSTDX_PIECEPIPE_VERBOSE").is_ok() {
                    println!(
                        "  depth violation: piece {k} match_pos {} (t {}) ml {} src {} depth {} \
                         off {}",
                        r.match_pos,
                        r.match_pos - start,
                        r.ml,
                        r.src,
                        start - r.src,
                        r.match_pos - r.src,
                    );
                }
            }
        }
    }
    if std::env::var("ZSTDX_PIECEPIPE_VERBOSE").is_ok() {
        for (k, &m) in per_piece_min.iter().enumerate() {
            if m != u64::MAX {
                println!(
                    "  piece {k} start {} min-crossing-offset {m}",
                    piece_start(k)
                );
            }
        }
    }
    let mut sorted: Vec<u64> = per_piece_min
        .iter()
        .filter(|&&m| m != u64::MAX)
        .copied()
        .collect();
    sorted.sort_unstable();
    let pct = |q: f64| -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[i.min(sorted.len() - 1)]
    };
    let o_min = sorted.first().copied();
    if std::env::var("ZSTDX_PIECEPIPE_VERBOSE").is_ok()
        && let Some(m) = o_min
    {
        for r in &resolved {
            let k = piece_of(r.match_pos);
            if r.src < piece_start(k) && r.match_pos - r.src == m {
                println!(
                    "  o_min read: piece {} match_pos {} (t {}) ml {} src {} ll {}",
                    k,
                    r.match_pos,
                    r.match_pos - piece_start(k),
                    r.ml,
                    r.src,
                    r.match_pos
                );
                break;
            }
        }
    }

    // One forward pass over pieces in order; earlier pieces' schedules are
    // final when later pieces query them (dependencies only point back).
    // `lanes`: None = unlimited workers (every piece starts at 0); Some(w)
    // dispatches pieces in order to the earliest-free lane.
    let simulate = |lanes: Option<u32>| -> u64 {
        let w = lanes.map_or(0, |w| w as usize);
        let mut lane_free = vec![0u64; w.max(1)];
        let mut bps: Vec<Vec<(u64, u64)>> = vec![Vec::new(); n_pieces];
        let mut finishes = vec![0u64; n_pieces];
        let mut starts = vec![0u64; n_pieces];
        for k in 0..n_pieces {
            let start = if w == 0 {
                0
            } else {
                let (lane, free) = lane_free[..w]
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, &f)| f)
                    .map(|(i, &f)| (i, f))
                    .unwrap();
                lane_free[lane] = u64::MAX; // occupied until the finish below
                starts[k] = free;
                free
            };
            let _ = start;
            let b0 = piece_start(k);
            let b1 = piece_end(k);
            let mut pos = b0;
            let mut t = starts[k];
            let mut bp = vec![(b0, t)];
            for r in resolved.iter().filter(|r| piece_of(r.match_pos) == k) {
                t += r.match_pos - pos; // literals at rate 1
                let ready = if r.src < b0 {
                    // The copy reads [src, src + ml) at rate 1 starting at
                    // t_copy, so every source byte y' = src + i needs
                    // pub(y') - i <= t_copy. Rate-1 segments cancel, but a
                    // stall breakpoint inside the span shifts its tail:
                    // fold every breakpoint in the span to its src-relative
                    // time.
                    let m = piece_of(r.src);
                    let bpm = &bps[m];
                    let base = match bpm.binary_search_by_key(&r.src, |&(p, _)| p) {
                        Ok(i) => bpm[i].1,
                        Err(0) => bpm
                            .first()
                            .map_or(0, |&(p, tt)| tt + r.src.saturating_sub(p)),
                        Err(i) => {
                            let &(p, tt) = &bpm[i - 1];
                            tt + (r.src - p)
                        },
                    };
                    let span_end = r.src + r.ml as u64;
                    let mut ready = base;
                    let mut j = bpm.partition_point(|&(p, _)| p <= r.src);
                    while j < bpm.len() && bpm[j].0 < span_end {
                        // the breakpoint's position was written at its time;
                        // bytes after it shift by the stall
                        ready = ready.max(bpm[j].1 - (bpm[j].0 - r.src));
                        j += 1;
                    }
                    ready
                } else {
                    0
                };
                if ready > t {
                    bp.push((r.match_pos, ready));
                    t = ready;
                }
                t += r.ml as u64;
                pos = r.match_pos + r.ml as u64;
            }
            t += b1 - pos; // trailing literals / raw tail
            bps[k] = bp;
            finishes[k] = t;
            if w != 0 {
                let lane = (0..w).find(|&i| lane_free[i] == u64::MAX).unwrap();
                lane_free[lane] = t;
            }
        }
        finishes.iter().copied().max().unwrap_or(0)
    };

    println!("file                 {}", args.file.display());
    println!(
        "out {n} B, {} seqs, {n_pieces} pieces of {job} B, crossing reads {crossing} ({:.1}% of \
         seqs)",
        resolved.len(),
        100.0 * crossing as f64 / resolved.len().max(1) as f64
    );
    println!(
        "per-piece min crossing offset: p10 {} p50 {} p90 {} max {}",
        pct(0.10),
        pct(0.50),
        pct(0.90),
        sorted.last().copied().unwrap_or(0)
    );
    if args.depth != 0 {
        println!(
            "depth-guarantee audit (D={}): {depth_violations} violations of {crossing} crossing \
             reads",
            args.depth
        );
    }
    match o_min {
        Some(m) if m < job => {
            let ceiling = job as f64 / (job as f64 - m as f64);
            println!("o_min {m}  -> interval ceiling s/(s-o_min) = {ceiling:.3}x");
        },
        Some(m) => println!("o_min {m} >= piece size: no previous-piece constraint"),
        None => println!("no crossing reads: piece-parallel unbounded"),
    }
    println!("serial wall {n} ticks");
    let wall = simulate(None);
    println!(
        "piece-parallel (unlimited) {wall}  -> {:.3}x",
        n as f64 / wall as f64
    );
    for w in &args.workers {
        let wall = simulate(Some(*w));
        println!(
            "piece-parallel (w{w} lanes) {wall}  -> {:.3}x",
            n as f64 / wall as f64
        );
    }
}
