//! ST-vs-MT parse decomposition (seq_dump decode hook): diff a
//! single-threaded frame's sequences against the multithreaded frame's
//! over the same input, per job-relative position and per match-offset
//! band, and (with `cfg_dump`) line the ST entry's resolved matcher
//! configuration up against an MT job's. The output prices todo 34's
//! open decomposition: where inside the jobs the match-length deficit
//! sits, which offset bands the missed matches come from, and whether
//! any of it is boundary-localized.

use std::{fs, path::PathBuf};

use zstdx::Level;

/// Executed sequence as the decoder logged it: absolute match position,
/// match length, resolved offset.
type Exec = (u64, u32, u64);

#[derive(clap::Args)]
pub struct Args {
    /// Raw input file
    input: PathBuf,
    /// Numeric levels (comma-separated, 1-22)
    #[arg(long, value_delimiter = ',', default_value = "1,3")]
    level: Vec<i32>,
    /// Worker counts (comma-separated)
    #[arg(long, value_delimiter = ',', default_value = "8,16")]
    workers: Vec<u32>,
    /// Job-head span the head/body split cuts at, in bytes
    #[arg(long, default_value_t = 64 * 1024)]
    head: u64,
    /// Job-tail span the tail/body split cuts at, in bytes (the job-end
    /// pre-split clamp zone)
    #[arg(long, default_value_t = 128 * 1024)]
    tail: u64,
    /// Directory for per-job TSVs (one file per cell)
    #[arg(long)]
    dump_dir: Option<PathBuf>,
}

/// Bitmap of matched output positions, one bit per byte.
struct Cov {
    words: Vec<u64>,
}

impl Cov {
    fn new(len: u64) -> Self {
        Cov {
            words: vec![0u64; (len as usize).div_ceil(64)],
        }
    }

    fn set_span(&mut self, start: u64, len: u64) {
        let end = start + len;
        let fw = (start / 64) as usize;
        let lw = ((end - 1) / 64) as usize;
        if fw == lw {
            // A run of `len` ones at bit `start % 64`; the composed-shift
            // form misplaces the run (oracle test bit 16515).
            let lo = start % 64;
            let mask = if len >= 64 {
                u64::MAX
            } else {
                ((1u64 << len) << lo) - (1u64 << lo)
            };
            self.words[fw] |= mask;
            return;
        }
        // A word-aligned start makes the head mask `MAX`, so the guard
        // must not skip it (bit 76288 of the oracle test).
        self.words[fw] |= u64::MAX << (start % 64);
        for w in &mut self.words[fw + 1..lw] {
            *w = u64::MAX;
        }
        let tail = (end - lw as u64 * 64) as u32;
        self.words[lw] |= if tail < 64 {
            u64::MAX >> (64 - tail)
        } else {
            u64::MAX
        };
    }

    /// Positions in `[start, end)` where `a` matches and `b` does not:
    /// the parse-`a` deficit against `b`.
    fn count_def(a: &Cov, b: &Cov, start: u64, end: u64) -> u64 {
        if end <= start {
            return 0;
        }
        let fw = (start / 64) as usize;
        let lw = ((end - 1) / 64) as usize;
        let head_mask = u64::MAX << (start % 64);
        let tail_bits = (end - lw as u64 * 64) as u32;
        let tail_mask = if tail_bits < 64 {
            u64::MAX >> (64 - tail_bits)
        } else {
            u64::MAX
        };
        let mut n = 0u64;
        for w in fw..=lw {
            let m = if w == fw && w == lw {
                head_mask & tail_mask
            } else if w == fw {
                head_mask
            } else if w == lw {
                tail_mask
            } else {
                u64::MAX
            };
            n += (a.words[w] & !b.words[w] & m).count_ones() as u64;
        }
        n
    }

    /// First position in `[from, to)` where the two maps disagree.
    fn first_diff(a: &Cov, b: &Cov, from: u64, to: u64) -> Option<u64> {
        let mut i = from;
        while i < to {
            let w = (i / 64) as usize;
            let x = (a.words[w] ^ b.words[w]) << (i % 64);
            if x == 0 {
                i = (w as u64 + 1) * 64;
                continue;
            }
            let p = i + x.trailing_zeros() as u64;
            return (p < to).then_some(p);
        }
        None
    }
}

/// Which history band an ST match's source position falls in, against
/// the MT job geometry at the match's position: 0 inside the emitting
/// job's span, 1 the dense strip the job prefills (`mt_frame_strip_for`),
/// 2 history the job adopts beyond the strip (capture/deep/feed classes;
/// empty on stock frames where overlap == strip), 3 the frame window
/// below the adopted history (far machinery territory for both parses),
/// 4 below the frame window (neither parse can reach).
fn domain_idx(src: u64, pos: u64, job_start: u64, strip: u64, overlap: u64, window: u64) -> usize {
    if src >= job_start {
        0
    } else if src + strip >= job_start {
        1
    } else if src + overlap >= job_start {
        2
    } else if src + window >= pos {
        3
    } else {
        4
    }
}

const OFF_BINS: usize = 30;

fn mib(v: u64) -> String {
    format!("{:.2} MiB", v as f64 / 1048576.0)
}

/// Compact byte count for histogram cells: MiB at field scale, raw
/// bytes below.
fn fb(v: u64) -> String {
    if v >= 1024 * 1024 {
        mib(v)
    } else {
        format!("{v} B")
    }
}

fn off_bin(off: u64) -> usize {
    off.ilog2() as usize
}

#[cfg(feature = "seq_dump")]
fn decode_exec(frame: &[u8], raw: &[u8], label: &str) -> Vec<Exec> {
    use zstdx::decoding::FrameDecoder;
    let mut out = vec![0u8; raw.len()];
    let mut dec = FrameDecoder::new();
    let n = dec
        .decode_all(frame, &mut out)
        .unwrap_or_else(|e| panic!("decode {label}: {e}")) as usize;
    assert_eq!(&out[..n], raw, "{label} frame must roundtrip");
    let wire = zstdx::decoding::seq_dump::take();
    let exec = zstdx::decoding::seq_dump::take_exec();
    assert_eq!(
        wire.len(),
        exec.len(),
        "{label}: wire and exec logs must cover the same sequences"
    );
    // Cross-check the resolved offsets against the wire values (the
    // jobrestart probe's discipline).
    let mut t = [1u64, 4, 8];
    for (i, w) in wire.iter().enumerate() {
        let (pos, ml, off_exec) = exec[i];
        assert_eq!(w.ml, ml, "{label}: wire/exec ml mismatch at sequence {i}");
        let off = resolve_wire(w.of, w.ll, &mut t);
        assert_eq!(
            off, off_exec,
            "{label}: wire/exec offset mismatch at sequence {i}"
        );
        assert!(pos + ml as u64 <= raw.len() as u64);
    }
    exec
}

/// RFC 8878 repeat-offset resolution (mirrors `jobrestart`).
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

#[cfg(feature = "seq_dump")]
fn sum_ml(exec: &[Exec]) -> u64 {
    exec.iter().map(|e| u64::from(e.1)).sum()
}

/// The per-job and per-band aggregates of one cell.
#[cfg(feature = "seq_dump")]
struct Cell {
    def_bins: [u64; OFF_BINS],
    gain_bins: [u64; OFF_BINS],
    def_domain: [u64; 5],
    def_matched_start: u64,
    def_missed_start: u64,
    ml_eq: u64,
    ml_st_longer: u64,
    ml_mt_longer: u64,
    ml_sum_st_minus_mt: i64,
    first_div: [u64; 7],
    /// Deficit bytes keyed by exact offset, for the top-distance table
    /// (the tiling-twin verdict reads its spikes here).
    def_exact: std::collections::HashMap<u64, u64>,
}

/// Per-job first-divergence offset-from-job-start bin names: sub-64 B,
/// sub-512, sub-4K, sub-64K, sub-1M, past 1M, no divergence.
#[cfg(feature = "seq_dump")]
const DIV_BINS: [&str; 7] = [
    "0-64", "64-512", "512-4K", "4K-64K", "64K-1M", ">1M", "none",
];

#[cfg(feature = "seq_dump")]
#[allow(clippy::too_many_arguments)]
fn analyze(
    len: u64,
    st: &[Exec],
    mt: &[Exec],
    cov_st: &Cov,
    cov_mt: &Cov,
    job_size: u64,
    n_jobs: u64,
    strip: u64,
    overlap: u64,
    window: u64,
) -> Cell {
    let mut c = Cell {
        def_bins: [0; OFF_BINS],
        gain_bins: [0; OFF_BINS],
        def_domain: [0; 5],
        def_matched_start: 0,
        def_missed_start: 0,
        ml_eq: 0,
        ml_st_longer: 0,
        ml_mt_longer: 0,
        ml_sum_st_minus_mt: 0,
        first_div: [0; 7],
        def_exact: std::collections::HashMap::new(),
    };
    // Common-start ml comparison: both logs are strictly position-ordered.
    let (mut i, mut j) = (0usize, 0usize);
    while i < st.len() && j < mt.len() {
        match st[i].0.cmp(&mt[j].0) {
            std::cmp::Ordering::Equal => {
                let d = i64::from(st[i].1) - i64::from(mt[j].1);
                if d == 0 {
                    c.ml_eq += 1;
                } else if d > 0 {
                    c.ml_st_longer += 1;
                } else {
                    c.ml_mt_longer += 1;
                }
                c.ml_sum_st_minus_mt += d;
                i += 1;
                j += 1;
            },
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    // Deficit/gain attribution per sequence.
    for &(pos, ml, off) in st {
        let job = (pos / job_size).min(n_jobs - 1);
        let job_start = job * job_size;
        let dom = domain_idx(pos - off, pos, job_start, strip, overlap, window);
        let bin = off_bin(off.min((1 << OFF_BINS as u64) - 1));
        let def = Cov::count_def(cov_st, cov_mt, pos, pos + ml as u64);
        if def > 0 {
            c.def_bins[bin] += def;
            c.def_domain[dom] += def;
            if mt.binary_search_by(|probe| probe.0.cmp(&pos)).is_ok() {
                c.def_matched_start += def;
            } else {
                c.def_missed_start += def;
            }
            *c.def_exact.entry(off).or_default() += def;
        }
    }
    for &(pos, ml, off) in mt {
        let bin = off_bin(off.min((1 << OFF_BINS as u64) - 1));
        let gain = Cov::count_def(cov_mt, cov_st, pos, pos + ml as u64);
        if gain > 0 {
            c.gain_bins[bin] += gain;
        }
    }
    // Per-job first divergence (jobs >= 1: job 0 shares the frame head
    // with ST).
    for k in 1..n_jobs {
        let ks = k * job_size;
        let ke = ((k + 1) * job_size).min(len);
        match Cov::first_diff(cov_st, cov_mt, ks, ke) {
            None => c.first_div[6] += 1,
            Some(p) => {
                let b = match p - ks {
                    0..=63 => 0,
                    64..=511 => 1,
                    512..=4095 => 2,
                    4096..=65535 => 3,
                    65536..=1_048_575 => 4,
                    _ => 5,
                };
                c.first_div[b] += 1;
            },
        }
    }
    c
}

#[cfg(feature = "seq_dump")]
fn run_cell(
    raw: &[u8],
    level_num: i32,
    workers: u32,
    head: u64,
    tail: u64,
    input: &str,
    st_cache: &mut Option<(i32, Vec<Exec>, Cov, usize)>,
    dump_dir: Option<&PathBuf>,
) {
    let level = Level::from_zstd(level_num);
    let len = raw.len() as u64;
    let shape = zstdx::InputShape {
        len: Some(len),
        window_log: None,
    };

    // ST side: encode once per (input, level); the parse is
    // worker-independent.
    if st_cache.is_none() || st_cache.as_ref().unwrap().0 != level_num {
        #[cfg(feature = "cfg_dump")]
        let _ = zstdx::encoding::cfg_dump::take();
        let opts = zstdx::EncoderOptions::new(level).checksum(false);
        let frame = zstdx::bulk::compress_with(raw, &opts).expect("st encode");
        #[cfg(feature = "cfg_dump")]
        print_cfg_st(level_num, &zstdx::encoding::cfg_dump::take(), frame.len());
        let exec = decode_exec(&frame, raw, "st");
        let mut cov = Cov::new(len);
        for &(pos, ml, _) in &exec {
            cov.set_span(pos, ml as u64);
        }
        *st_cache = Some((level_num, exec, cov, frame.len()));
    }
    let (_, st_exec, cov_st, st_size) = st_cache.as_ref().unwrap();

    #[cfg(feature = "cfg_dump")]
    let _ = zstdx::encoding::cfg_dump::take();
    let opts = zstdx::EncoderOptions::new(level)
        .checksum(false)
        .workers(workers);
    let frame = zstdx::bulk::compress_with(raw, &opts).expect("mt encode");
    #[cfg(feature = "cfg_dump")]
    print_cfg_mt(level_num, workers);

    let mt_exec = decode_exec(&frame, raw, "mt");
    let mut cov_mt = Cov::new(len);
    for &(pos, ml, _) in &mt_exec {
        cov_mt.set_span(pos, ml as u64);
    }

    let job_size = zstdx::encoding::mt_job_size_for(len, workers, level, shape, raw) as u64;
    let overlap = zstdx::encoding::mt_job_overlap_for(len, workers, level, shape, raw) as u64;
    let window = zstdx::encoding::mt_frame_window_for(level, shape);
    let strip = zstdx::encoding::mt_frame_strip_for(level, shape, raw);
    let n_jobs = len.div_ceil(job_size);

    let (st_nseq, st_m) = (st_exec.len() as u64, sum_ml(st_exec));
    let (mt_nseq, mt_m) = (mt_exec.len() as u64, sum_ml(&mt_exec));
    let total_def = Cov::count_def(cov_st, &cov_mt, 0, len);
    let total_gain = Cov::count_def(&cov_mt, cov_st, 0, len);
    println!(
        "{input} l{level_num} w{workers}: st {} B (nseq {st_nseq}, avg_ml {:.2}), mt {} B (nseq \
         {mt_nseq}, avg_ml {:.2}), delta +{:.2}%",
        st_size,
        st_m as f64 / st_nseq as f64,
        frame.len(),
        mt_m as f64 / mt_nseq as f64,
        100.0 * (frame.len() as f64 / *st_size as f64 - 1.0),
    );
    println!(
        "  jobs {n_jobs} x {job_size} B, overlap {}, strip {} (window {}); matched-B st {} vs mt \
         {}, deficit {} ({:.3}% of raw), gain {}",
        mib(overlap),
        mib(strip),
        mib(window),
        mib(st_m),
        mib(mt_m),
        mib(total_def),
        100.0 * total_def as f64 / len as f64,
        mib(total_gain),
    );

    // Head/tail/body split of the deficit, byte-accurate over the job
    // spans: the tail span is the job-end pre-split clamp zone (the
    // last block's `window_end` clamps to the job end, so its split
    // decision and the per-job savings restart both sit there).
    let mut def_head = 0u64;
    let mut def_tail = 0u64;
    let mut per_job_def = Vec::with_capacity(n_jobs as usize);
    for k in 0..n_jobs {
        let ks = k * job_size;
        let ke = ((k + 1) * job_size).min(len);
        let he = (ks + head).min(ke);
        let te = ke.saturating_sub(tail).max(he);
        def_head += Cov::count_def(cov_st, &cov_mt, ks, he);
        def_tail += Cov::count_def(cov_st, &cov_mt, te, ke);
        per_job_def.push(Cov::count_def(cov_st, &cov_mt, ks, ke));
    }
    let pct = |v: u64| 100.0 * v as f64 / total_def.max(1) as f64;

    let c = analyze(
        len, st_exec, &mt_exec, cov_st, &cov_mt, job_size, n_jobs, strip, overlap, window,
    );
    // Runtime invariant, not a debug assert: a coverage-mask bug here
    // ships silently otherwise (the set_span run-mask bug did).
    assert_eq!(
        c.def_bins.iter().sum::<u64>(),
        total_def,
        "bin sums must cover the deficit exactly"
    );
    println!(
        "  deficit head(<{head} B of job) {} ({:.1}%) tail(>{tail} B from job end) {} ({:.1}%) \
         body {} ({:.1}%); starts: matched {} vs missed {}",
        mib(def_head),
        pct(def_head),
        mib(def_tail),
        pct(def_tail),
        mib(total_def - def_head - def_tail),
        pct(total_def - def_head - def_tail),
        mib(c.def_matched_start),
        mib(c.def_missed_start),
    );
    println!(
        "  common-start seqs: ml equal {} / st longer {} / mt longer {}, sum(ml_st-ml_mt) {}",
        c.ml_eq, c.ml_st_longer, c.ml_mt_longer, c.ml_sum_st_minus_mt,
    );
    print!("  first divergence in job (jobs 1..): ");
    for (b, name) in DIV_BINS.iter().enumerate() {
        if c.first_div[b] > 0 {
            print!("{name}x{} ", c.first_div[b]);
        }
    }
    println!("(of {} jobs)", n_jobs - 1);
    for (d, n) in c.def_domain.iter().zip(DOMAIN_NAMES) {
        if *d > 0 {
            println!("    source domain {n}: {}", mib(*d));
        }
    }
    let geo = format!(
        "  [geometry: strip=2^{} overlap=2^{} window=2^{}]",
        strip.ilog2(),
        overlap.ilog2(),
        window.ilog2()
    );
    print!("  deficit offset bins (pow2):{geo}");
    for (b, v) in c.def_bins.iter().enumerate() {
        if *v > 0 {
            print!(" 2^{b}:{}", fb(*v));
        }
    }
    println!();
    let mut top: Vec<(u64, u64)> = c.def_exact.into_iter().collect();
    top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    print!("  top deficit offsets:");
    for (off, v) in top.iter().take(8) {
        // Raw offsets: the twin verdict keys on exact distances (e.g.
        // 104857600 on a 4x tile).
        print!(" {off}:{}", fb(*v));
    }
    println!();
    print!("  gain offset bins (pow2):   ");
    for (b, v) in c.gain_bins.iter().enumerate() {
        if *v > 0 {
            print!(" 2^{b}:{}", fb(*v));
        }
    }
    println!();

    if let Some(dir) = dump_dir {
        let stem = input.rsplit('/').next().unwrap_or("input");
        let name = format!("{}/{stem}-l{level_num}-w{workers}.tsv", dir.display());
        let mut tsv =
            String::from("job\tstart\tst_nseq\tmt_nseq\tst_avg_ml\tmt_avg_ml\tdeficit\tgain\n");
        let (mut si, mut mi) = (0usize, 0usize);
        for k in 0..n_jobs {
            let ks = k * job_size;
            let ke = ((k + 1) * job_size).min(len);
            let mut st_n = 0u64;
            let mut st_ml = 0u64;
            while si < st_exec.len() && st_exec[si].0 < ke {
                if st_exec[si].0 >= ks {
                    st_n += 1;
                    st_ml += st_exec[si].1 as u64;
                }
                si += 1;
            }
            let mut mt_n = 0u64;
            let mut mt_ml = 0u64;
            while mi < mt_exec.len() && mt_exec[mi].0 < ke {
                if mt_exec[mi].0 >= ks {
                    mt_n += 1;
                    mt_ml += mt_exec[mi].1 as u64;
                }
                mi += 1;
            }
            let gain = Cov::count_def(&cov_mt, cov_st, ks, ke);
            tsv.push_str(&format!(
                "{k}\t{ks}\t{st_n}\t{mt_n}\t{:.2}\t{:.2}\t{}\t{gain}\n",
                st_ml as f64 / st_n.max(1) as f64,
                mt_ml as f64 / mt_n.max(1) as f64,
                per_job_def[k as usize],
            ));
        }
        fs::write(name, tsv).expect("write tsv");
    }
}

#[cfg(feature = "seq_dump")]
const DOMAIN_NAMES: [&str; 5] = ["injob", "strip", "overlap", "window", "beyond"];

/// Print the ST entry's configuration records (the records were drained
/// right after the encode).
#[cfg(feature = "cfg_dump")]
fn print_cfg_st(level: i32, records: &[zstdx::encoding::cfg_dump::Record], size: usize) {
    use zstdx::encoding::cfg_dump::Record;
    println!("cfg ST l{level} (frame {size} B):");
    for r in records {
        match r {
            Record::StEntry(e) => println!(
                "  entry: bal_row={} reach_keep={} arming={:?}",
                e.bal_row, e.reach_keep, e.arming
            ),
            Record::Reset(r) => println!("  reset: {}", fmt_cfg(&r.cfg)),
            Record::Flip(f) => println!("  flip {}: {}", f.site, fmt_cfg(&f.cfg)),
            Record::Job(_) | Record::Frame(_) => {},
        }
    }
}

/// Print the MT frame's geometry record and the jobs' configurations.
#[cfg(feature = "cfg_dump")]
fn print_cfg_mt(level: i32, workers: u32) {
    use zstdx::encoding::cfg_dump::{Record, take};
    let records = take();
    println!("cfg MT l{level} w{workers}:");
    for r in &records {
        match r {
            Record::Frame(f) => println!(
                "  frame: jobs {} x {} B, overlap {}, strip {}, fast_window {:?}, capture_window \
                 {:?}, deep_ldm {:?}, opt_feed {:?}, job_prefix_ldm {}, header_window {}, \
                 reach_keep {}, donated {}, spf {:?}",
                f.n_jobs,
                f.job_size,
                mib(f.job_overlap),
                mib(f.strip),
                f.fast_window.map(mib),
                f.capture_window.map(mib),
                f.deep_ldm.map(mib),
                f.opt_feed.map(mib),
                f.job_prefix_ldm,
                mib(f.header_window),
                f.reach_keep,
                f.donated,
                f.spf,
            ),
            Record::Job(j) => println!(
                "  job@{}: rep_gated={} ramp={} {}",
                j.start,
                j.rep_gated,
                j.ramp_depth,
                fmt_cfg(&j.cfg)
            ),
            Record::Reset(r) => println!("  reset: {}", fmt_cfg(&r.cfg)),
            Record::Flip(f) => println!("  flip {}: {}", f.site, fmt_cfg(&f.cfg)),
            Record::StEntry(_) => {},
        }
    }
}

#[cfg(feature = "cfg_dump")]
fn fmt_cfg(c: &zstdx::encoding::cfg_dump::DriverCfg) -> String {
    use zstdx::encoding::cfg_dump::StrategyTag;
    let strat = match c.strategy {
        StrategyTag::Fast => "fast".to_string(),
        StrategyTag::Dfast(l) => format!("dfast(s{l})"),
        StrategyTag::Chain(l) => format!("chain(c{l})"),
        StrategyTag::Row(l) => format!("row(r{l})"),
        StrategyTag::Opt { bt_log, hash3_log } => format!("opt(bt{bt_log},h3-{hash3_log})"),
        StrategyTag::BtLazy { bt_log, hash3_log } => format!("btlazy(bt{bt_log},h3-{hash3_log})"),
    };
    let ldm = c.ldm.as_ref().map_or("off".to_string(), |l| {
        format!(
            "w{} buckets={} ents={} fed={} arm={} stale={}",
            l.window, l.buckets, l.ents_per_bucket, l.fed, l.arm, l.stale
        )
    });
    format!(
        "{strat} hash_log={} window={} reach={:?} depth={} lazy={} min_match={} small_src={} \
         light={} store={:?} ldm[{ldm}] rep_pending={} seed={}",
        c.hash_log,
        c.window,
        c.chain_reach,
        c.search_depth,
        c.lazy_depth,
        c.min_match,
        c.small_src,
        c.row_attempts_light,
        c.store,
        c.rep_pending,
        c.seed_offset,
    )
}

pub fn run(args: &Args) {
    #[cfg(not(feature = "seq_dump"))]
    {
        let _ = args;
        eprintln!("error: stmdiff needs the decoder dump hook; rebuild with `--features seq_dump`");
        std::process::exit(1);
    }
    #[cfg(feature = "seq_dump")]
    {
        let raw = fs::read(&args.input).expect("read input");
        if let Some(dir) = &args.dump_dir {
            fs::create_dir_all(dir).expect("create dump dir");
        }
        for &level in &args.level {
            assert!(
                (1..=Level::MAX.as_i32()).contains(&level),
                "level {level} out of range"
            );
            let mut st_cache: Option<(i32, Vec<Exec>, Cov, usize)> = None;
            for &workers in &args.workers {
                run_cell(
                    &raw,
                    level,
                    workers,
                    args.head,
                    args.tail,
                    &args.input.display().to_string(),
                    &mut st_cache,
                    args.dump_dir.as_ref(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cov;

    /// The bitmap must equal a byte-vec oracle over random non-overlapping
    /// monotone spans (the parse shape the probe feeds it).
    #[test]
    fn cov_matches_oracle() {
        let len = 1_000_003u64;
        let mut spans = Vec::new();
        let mut p = 0u64;
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state >> 33
        };
        while p < len - 100 {
            let skip = next() % 500;
            let ml = 3 + next() % 3000;
            p += skip;
            let ml = ml.min(len - p);
            spans.push((p, ml));
            p += ml;
        }
        let mut cov = Cov::new(len);
        let mut oracle = vec![false; len as usize];
        for &(s, l) in &spans {
            cov.set_span(s, l);
            for b in oracle.iter_mut().skip(s as usize).take(l as usize) {
                *b = true;
            }
        }
        for i in 0..len {
            assert_eq!(
                cov.words[(i / 64) as usize] >> (i % 64) & 1,
                oracle[i as usize] as u64,
                "bit {i}"
            );
        }
        let mut cov2 = Cov::new(len);
        for &(s, l) in &spans {
            cov2.set_span(s, l);
        }
        let d = Cov::count_def(&cov, &cov2, 0, len);
        assert_eq!(d, 0);
    }
}
