//! MT job-boundary parse-restart loss probes (seq_dump decode hook).
//!
//! Every MT job restarts its parse-side state at the boundary: repcodes
//! gate until three literal-offset sequences rewrite the decoder's
//! history (`MatchGeneratorDriver::gate_repcodes`, libzstd's
//! `ZSTD_invalidateRepCodes`), and match reach is bounded to the job's
//! strip plus window. This subcommand prices the two channels offline on
//! one bulk-mt frame (byte-identical to the pledged stream grid):
//!
//! - Probe A (repcode carry): replays the frame's wire sequences with the continuous decoder
//!   repcode triple. At each job boundary the actual terminal triple of job N is the carry job N+1
//!   could have started with; a counterfactual re-coding of job N+1's sequences (same matches,
//!   carried triple) prices the recoverable bits: sequences the gated encoder paid a full offset
//!   code for that a carried repcode would have covered. Matches the gated parse never emitted
//!   (repcode probes are skipped while `rep_pending` is armed) are invisible to this wire-level
//!   probe, so probe A is a lower bound.
//! - Probe B (match reach): for every match starting inside the first `--head` bytes of a job, an
//!   unconstrained hash-chain replay over the job head plus the frame's full window searches the
//!   same position twice — candidates bounded to the job's strip (constrained) and to the whole
//!   window below it (unconstrained), same table, same walk depth. The delta (longer match, or
//!   equal length at a shorter cross-boundary offset) is reach-attributable loss; deltas the
//!   constrained replay also finds are matcher-quality noise and are not counted.
//!
//! Bit-cost model: a full offset code for wire value v costs
//! `2 * floor(log2(v))` bits (FSE symbol plus the v's-code extra bits), a
//! repcode 1/2/3 costs 1/2/2 bits. Counts are printed alongside so the
//! table can be re-priced under any model.

#[cfg(feature = "seq_dump")]
use std::fs;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Raw input file
    input: PathBuf,
    /// Numeric levels (comma-separated, 1-22)
    #[arg(long, value_delimiter = ',', default_value = "1,3,9,19")]
    level: Vec<i32>,
    /// Worker counts (comma-separated)
    #[arg(long, value_delimiter = ',', default_value = "8,16")]
    workers: Vec<u32>,
    /// Job-head span probe B searches per job, in bytes
    #[arg(long, default_value_t = 64 * 1024)]
    head: u64,
    /// Hash-chain walk depth cap for probe B
    #[arg(long, default_value_t = 512)]
    depth: u32,
}

/// One sequence of the frame, resolved: match position, literal length,
/// match length, the wire offset value (1..3 = repcode) and the executed
/// offset.
#[cfg(feature = "seq_dump")]
struct Seq {
    pos: u64,
    ll: u32,
    ml: u32,
    wire_of: u32,
    off: u64,
}

/// Bit cost of a wire offset value under the probe's model.
#[cfg(feature = "seq_dump")]
fn of_cost_bits(wire_of: u32) -> u32 {
    match wire_of {
        1 => 1,
        2 | 3 => 2,
        v => 2 * (31 - v.leading_zeros()),
    }
}

/// Resolve a wire offset value against triple `t`, updating it in place
/// (RFC 8878 repeat-offset semantics, including the litLength == 0 index
/// shift). Mirrors `sequence_execution`'s decode loop.
#[cfg(feature = "seq_dump")]
fn resolve_wire(wire_of: u32, ll: u32, t: &mut [u64; 3]) -> u64 {
    if wire_of >= 4 {
        let off = u64::from(wire_of - 3);
        t[2] = t[1];
        t[1] = t[0];
        t[0] = off;
        return off;
    }
    let idx = wire_of as usize - 1 + usize::from(ll == 0);
    let off = match idx {
        0 | 1 => t[idx],
        2 => t[2],
        _ => t[0].saturating_sub(1),
    };
    match idx {
        0 => {},
        1 => {
            t[1] = t[0];
            t[0] = off;
        },
        _ => {
            t[2] = t[1];
            t[1] = t[0];
            t[0] = off;
        },
    }
    off
}

/// The repcode index the encoder could emit for `off` under triple `t`
/// (None = a full offset code is required). The ll == 0 shift maps wire
/// 1/2/3 to indices 1/2/3.
#[cfg(feature = "seq_dump")]
fn rep_idx(t: &[u64; 3], ll: u32, off: u64) -> Option<usize> {
    if ll == 0 {
        if off == t[1] {
            Some(1)
        } else if off == t[2] {
            Some(2)
        } else if t[0] > 1 && off == t[0] - 1 {
            Some(3)
        } else {
            None
        }
    } else if off == t[0] {
        Some(0)
    } else if off == t[1] {
        Some(1)
    } else if off == t[2] {
        Some(2)
    } else {
        None
    }
}

/// Apply the repcode at `idx` to triple `t` (the counterfactual wire
/// update); `off` is the resolved offset it stands for.
#[cfg(feature = "seq_dump")]
fn apply_rep(t: &mut [u64; 3], idx: usize, off: u64) {
    match idx {
        0 => {},
        1 => {
            t[1] = t[0];
            t[0] = off;
        },
        _ => {
            t[2] = t[1];
            t[1] = t[0];
            t[0] = off;
        },
    }
}

/// Probe A report: conversions and net saved bits, split into the job's
/// repcode-gate window (the span up to and including the job's third
/// full-offset sequence — the only sequences a real carry could re-code,
/// since the carried and actual triples agree once three fresh offsets
/// have been pushed) and the post-gate remainder (triple-content
/// divergence under the encodable-everywhere model: an upper bound a
/// wire-only carry would not realize).
#[cfg(feature = "seq_dump")]
#[derive(Default)]
struct RepReport {
    gate_conv: u64,
    gate_bits: i64,
    late_conv: u64,
    late_bits: i64,
}

/// Probe A: per boundary, re-code the next job's sequences against the
/// carried terminal triple.
#[cfg(feature = "seq_dump")]
fn probe_repcodes(seqs: &[Seq], job_size: u64, num_jobs: u64) -> RepReport {
    let mut rep = RepReport::default();
    let mut start = 0usize;
    // Triple after job k-1's last sequence: the continuous replay's state
    // at the first sequence of job k.
    let mut carried = [1u64, 4, 8];
    for k in 1..num_jobs {
        let bound = k * job_size;
        while start < seqs.len() && seqs[start].pos < bound {
            let s = &seqs[start];
            resolve_wire(s.wire_of, s.ll, &mut carried);
            start += 1;
        }
        let mut t = carried;
        // The actual gate lifts once three full-offset sequences have been
        // emitted (their pushes rewrite the decoder's history).
        let mut full_seen = 0u32;
        for s in &seqs[start..] {
            if s.pos >= (k + 1) * job_size {
                break;
            }
            let gated = full_seen < 3;
            if s.wire_of >= 4 {
                full_seen += 1;
            }
            let actual = i64::from(of_cost_bits(s.wire_of));
            let cf = match rep_idx(&t, s.ll, s.off) {
                Some(idx) => {
                    // Wire value idx+1: 1 costs 1 bit, 2 and 3 cost 2.
                    let bits = if idx == 0 {
                        1
                    } else {
                        2
                    };
                    apply_rep(&mut t, idx, s.off);
                    bits
                },
                None => {
                    let bits = of_cost_bits(s.off as u32 + 3);
                    t[2] = t[1];
                    t[1] = t[0];
                    t[0] = s.off;
                    bits
                },
            };
            let conv = u64::from(s.wire_of >= 4 && cf <= 2);
            let delta = actual - i64::from(cf);
            if gated {
                rep.gate_conv += conv;
                rep.gate_bits += delta;
            } else {
                rep.late_conv += conv;
                rep.late_bits += delta;
            }
        }
    }
    rep
}

/// Probe B's hash-chain window over one job's head region.
#[cfg(feature = "seq_dump")]
struct ChainProbe<'a> {
    raw: &'a [u8],
    head: Vec<i32>,
    prev: Vec<i32>,
    /// Absolute position of `prev[0]`.
    lo: u64,
    /// Next position to insert.
    cur: u64,
}

#[cfg(feature = "seq_dump")]
impl<'a> ChainProbe<'a> {
    const HBITS: u32 = 20;

    fn new(raw: &'a [u8], lo: u64, hi: u64) -> Self {
        let len = (hi - lo) as usize;
        ChainProbe {
            raw,
            head: vec![-1; 1 << Self::HBITS],
            prev: vec![-1; len],
            lo,
            cur: lo,
        }
    }

    fn hash(&self, p: u64) -> usize {
        let b = &self.raw[p as usize..p as usize + 4];
        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        (v.wrapping_mul(0x9e37_79b1) >> (32 - Self::HBITS)) as usize
    }

    /// Insert every position in `[cur, upto)`.
    fn fill_to(&mut self, upto: u64) {
        let limit = upto.min((self.raw.len() as u64).saturating_sub(4));
        while self.cur < limit {
            let h = self.hash(self.cur);
            self.prev[(self.cur - self.lo) as usize] = self.head[h];
            self.head[h] = self.cur as i32;
            self.cur += 1;
        }
    }

    /// Common prefix length of `a` and `b` past the first 4 verified
    /// bytes.
    fn extend(&self, a: u64, b: u64) -> usize {
        let n = self.raw.len();
        let (a, b) = (a as usize, b as usize);
        let mut l = 4;
        while b + l < n && a + l < n && self.raw[a + l] == self.raw[b + l] {
            l += 1;
        }
        l
    }

    /// Best match at `pos` over candidates down to `floor`, returning
    /// (ml, off); ties keep the shorter offset. Returns (0, 0) when no
    /// candidate reaches 4 bytes.
    fn best(&self, pos: u64, floor: u64, depth: u32) -> (usize, u64) {
        if pos + 4 > self.raw.len() as u64 {
            return (0, 0);
        }
        let mut best_len = 0usize;
        let mut best_off = 0u64;
        let mut cand = self.head[self.hash(pos)];
        let mut steps = 0u32;
        while cand >= 0 && steps < depth {
            let c = cand as u64;
            if c < floor {
                break; // chains descend monotonically
            }
            debug_assert!(c < pos);
            let off = pos - c;
            if self.raw[c as usize..c as usize + 4] == self.raw[pos as usize..pos as usize + 4] {
                let ml = self.extend(c, pos);
                if ml > best_len || (ml == best_len && off < best_off) {
                    best_len = ml;
                    best_off = off;
                }
            }
            cand = self.prev[(c - self.lo) as usize];
            steps += 1;
        }
        if best_len >= 4 {
            (best_len, best_off)
        } else {
            (0, 0)
        }
    }
}

/// Probe B: length bytes and offset bits recoverable at job heads if the
/// full window below the job start were searchable.
/// The constrained pass searches the job's own domain (down to
/// `job_start - overlap`, `mt_job_overlap_for`), the unconstrained pass
/// the frame-continuous dense strip (`job_start - strip`,
/// `mt_frame_strip_for`); the band between them is what the job split
/// takes away from the dense search. Wider matches ride the LDM's sparse
/// twin screen on both paths and cannot be priced by a dense probe. A
/// greedy coverage cursor keeps overlapping extensions from being
/// counted twice.
#[cfg(feature = "seq_dump")]
#[allow(clippy::too_many_arguments)]
fn probe_reach(
    raw: &[u8],
    exec: &[(u64, u32, u64)],
    job_size: u64,
    num_jobs: u64,
    overlap: u64,
    strip: u64,
    head_span: u64,
    depth: u32,
) -> (u64, u64) {
    let len = raw.len() as u64;
    let mut len_gain = 0u64;
    let mut off_gain_bits = 0u64;
    let mut m = 0usize;
    for k in 1..num_jobs {
        let job_start = k * job_size;
        let hi = (job_start + head_span).min(len);
        let con_floor = job_start.saturating_sub(overlap);
        let lo = job_start.saturating_sub(strip);
        if lo == con_floor {
            continue; // the job already reaches the frame window
        }
        let mut probe = ChainProbe::new(raw, lo, hi);
        let mut covered = job_start;
        while m < exec.len() && exec[m].0 < hi {
            let (pos, ml, off) = exec[m];
            m += 1;
            if pos < job_start {
                continue;
            }
            probe.fill_to(pos);
            let (ml_u, off_u) = probe.best(pos, lo, depth);
            if ml_u == 0 {
                continue;
            }
            let (ml_c, _off_c) = probe.best(pos, con_floor, depth);
            let cross = off_u > pos - con_floor;
            if !cross {
                continue; // the unconstrained best stays in the job's domain
            }
            let base_end = pos + (ml as usize).max(ml_c) as u64;
            let unc_end = pos + ml_u as u64;
            let from = base_end.max(covered);
            if unc_end > from {
                len_gain += unc_end - from;
                covered = unc_end;
            } else if ml_u as u64 >= (ml as usize).max(ml_c) as u64 && off_u < off {
                off_gain_bits += u64::from(
                    of_cost_bits(off as u32 + 3).saturating_sub(of_cost_bits(off_u as u32 + 3)),
                );
            }
        }
    }
    (len_gain, off_gain_bits)
}

#[cfg(feature = "seq_dump")]
fn run_cell(raw: &[u8], level_num: i32, workers: u32, head_span: u64, depth: u32, input: &str) {
    use zstdx::decoding::FrameDecoder;

    let level = zstdx::Level::from_zstd(level_num);
    let opts = zstdx::EncoderOptions::new(level).workers(workers);
    let frame = zstdx::bulk::compress_with(raw, &opts).expect("bulk encode");
    let shape = zstdx::InputShape {
        len: Some(raw.len() as u64),
        window_log: None,
    };
    let job_size =
        zstdx::encoding::mt_job_size_for(raw.len() as u64, workers, level, shape, raw) as u64;
    let overlap =
        zstdx::encoding::mt_job_overlap_for(raw.len() as u64, workers, level, shape, raw) as u64;
    let window = zstdx::encoding::mt_frame_window_for(level, shape);
    let strip = zstdx::encoding::mt_frame_strip_for(level, shape, raw);
    let num_jobs = (raw.len() as u64).div_ceil(job_size);

    let mut out = vec![0u8; raw.len()];
    let mut dec = FrameDecoder::new();
    let n = dec
        .decode_all(&frame, &mut out)
        .unwrap_or_else(|e| panic!("decode: {e}")) as usize;
    assert_eq!(&out[..n], raw, "frame must roundtrip");
    let wire = zstdx::decoding::seq_dump::take();
    let exec = zstdx::decoding::seq_dump::take_exec();
    assert_eq!(
        wire.len(),
        exec.len(),
        "wire and exec logs must cover the same sequences"
    );

    // Continuous repcode replay: resolves every wire value and
    // cross-checks it against the executor's ground truth.
    let mut t = [1u64, 4, 8];
    let mut seqs = Vec::with_capacity(wire.len());
    for (i, w) in wire.iter().enumerate() {
        let (pos, ml, off_exec) = exec[i];
        assert_eq!(w.ml, ml, "wire/exec ml mismatch at sequence {i}");
        let off = resolve_wire(w.of, w.ll, &mut t);
        assert_eq!(off, off_exec, "wire/exec offset mismatch at sequence {i}");
        seqs.push(Seq {
            pos,
            ll: w.ll,
            ml,
            wire_of: w.of,
            off,
        });
    }

    let rep = probe_repcodes(&seqs, job_size, num_jobs);
    let (len_b, off_b) = probe_reach(
        raw, &exec, job_size, num_jobs, overlap, strip, head_span, depth,
    );
    let pct = |bits: i64| 100.0 * (bits as f64 / 8.0) / frame.len() as f64;
    let a_bytes = (rep.gate_bits + rep.late_bits) / 8;
    let b_bytes = len_b + off_b / 8;
    println!(
        "{input} l{level_num} w{workers}: frame {} B, jobs {num_jobs} x {job_size} B, overlap \
         {overlap} B, strip {strip} B (window {window} B)",
        frame.len()
    );
    println!(
        "  A repcode-carry: gate {} seqs {} bits ({:.4}%), late {} seqs {} bits ({:.4}%)",
        rep.gate_conv,
        rep.gate_bits,
        pct(rep.gate_bits),
        rep.late_conv,
        rep.late_bits,
        pct(rep.late_bits)
    );
    println!(
        "  B match-reach: +{len_b} B match len, {off_b} off bits = {b_bytes} B ({:.4}% of frame)",
        100.0 * b_bytes as f64 / frame.len() as f64
    );
    println!(
        "  A-gate+B: {} B ({:.4}% of frame);  A-total+B: {} B ({:.4}%)",
        rep.gate_bits / 8 + b_bytes as i64,
        pct(rep.gate_bits) + 100.0 * b_bytes as f64 / frame.len() as f64,
        a_bytes + b_bytes as i64,
        100.0 * (a_bytes + b_bytes as i64) as f64 / frame.len() as f64
    );
}

#[cfg(feature = "seq_dump")]
fn run_dump(args: &Args) {
    let raw = fs::read(&args.input).expect("read input");
    for &level in &args.level {
        assert!(
            (1..=zstdx::Level::MAX.as_i32()).contains(&level),
            "level {level} out of range"
        );
        for &workers in &args.workers {
            run_cell(
                &raw,
                level,
                workers,
                args.head,
                args.depth,
                &args.input.display().to_string(),
            );
        }
    }
}

pub fn run(args: &Args) {
    #[cfg(not(feature = "seq_dump"))]
    {
        let _ = args;
        eprintln!(
            "error: jobrestart needs the decoder dump hook; rebuild with `--features seq_dump`"
        );
        std::process::exit(1);
    }
    #[cfg(feature = "seq_dump")]
    run_dump(args);
}
