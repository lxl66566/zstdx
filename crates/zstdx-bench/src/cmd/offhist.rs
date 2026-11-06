//! Match-offset spectrum and ramp-band damage predictor for one frame
//! (seq_dump decode hook): decodes a stock frame through the executor's
//! ground-truth match log and reports (a) the matched-byte mass per
//! power-of-two offset bucket plus the dominant exact offsets — the
//! shape's period spectrum — and (b) per candidate depth D the matched
//! bytes a ramp on the given job lattice would reject: matches whose
//! source sits within D bytes below a job start, the cost law's band.
//! The predictor prices a shape's ramp cost class before any ramped
//! encode: tiny-offset shapes read near zero, period-in-band shapes read
//! their whole match mass.
//!
//! Job zero never arms the ramp, so matches inside the first job are
//! always kept (see `MatchGeneratorDriver::arm_ramp`).

use std::path::PathBuf;
#[cfg(feature = "seq_dump")]
use std::{collections::HashMap, fs};

#[cfg(feature = "seq_dump")]
use zstdx::decoding::FrameDecoder;

#[derive(clap::Args)]
pub struct Args {
    /// Frame to analyze (raw counterpart resolved as <stem>.raw)
    file: PathBuf,
    /// Job size in bytes of the encode whose ramp is priced (emitframe
    /// prints it); cuts sit at multiples of this
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    job_size: u64,
    /// Ramp depths to price, in bytes (comma-separated)
    #[arg(long, value_delimiter = ',', default_value = "524288,1048576,2097152")]
    depth: Vec<u64>,
    /// Group the per-depth rejected bytes by regions of this many bytes
    /// (0 = off); locates damage in mixed payloads
    #[arg(long, default_value_t = 0)]
    region: u64,
}

/// Whether a match at distance `x` above its job start, reading at offset
/// `off`, falls in the rejected band: source in `(job - D, job)`, i.e.
/// `off` in `(x, x + D)`.
#[cfg(feature = "seq_dump")]
fn rejected(off: u64, x: u64, depth: u64) -> bool {
    off > x && off < x + depth
}

#[cfg(feature = "seq_dump")]
fn run_dump(args: &Args) {
    let comp = fs::read(&args.file).expect("read frame");
    let stem = args
        .file
        .file_stem()
        .expect("file stem")
        .to_string_lossy()
        .into_owned();
    let raw_path = args.file.with_file_name(format!("{stem}.raw"));
    let raw = fs::read(&raw_path)
        .unwrap_or_else(|_| panic!("raw counterpart {} missing", raw_path.display()));

    let mut out = vec![0u8; raw.len()];
    let mut dec = FrameDecoder::new();
    let n = dec
        .decode_all(&comp, &mut out)
        .unwrap_or_else(|e| panic!("decode: {e}")) as u64;
    assert_eq!(&out[..n as usize], &raw[..], "frame must roundtrip");
    let exec = zstdx::decoding::seq_dump::take_exec();

    // Same exact-resolution gate as piecepipe: a resolved match that does
    // not verify against the output means positions or offsets diverged
    // and every derived statistic is garbage.
    let mut matched = 0u64;
    let mut buckets = [0u64; 33];
    let mut per_offset: HashMap<u64, u64> = HashMap::new();
    for &(pos, ml, off) in &exec {
        assert!(off > 0 && off <= pos, "bad exec record");
        let (pos, ml) = (pos as usize, ml as usize);
        let src = pos - off as usize;
        assert!(
            out[src..src + ml] == out[pos..pos + ml],
            "resolved match at {pos} (src {src}, ml {ml}) does not verify"
        );
        matched += ml as u64;
        buckets[off.ilog2() as usize] += ml as u64;
        *per_offset.entry(off).or_insert(0) += ml as u64;
    }
    let lits = n.saturating_sub(matched);

    println!(
        "frame {}: matches {} matched {} lits {} (frame {} B, match cover {:.1}%)",
        args.file.display(),
        exec.len(),
        matched,
        lits,
        n,
        100.0 * matched as f64 / n as f64
    );
    for (k, &mass) in buckets.iter().enumerate() {
        if mass > 0 {
            println!(
                "bucket 2^{}: {} B ({:.2}% of matched)",
                k,
                mass,
                100.0 * mass as f64 / matched as f64
            );
        }
    }
    let mut top: Vec<(u64, u64)> = per_offset.into_iter().collect();
    top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (off, bytes) in top.iter().take(6) {
        println!(
            "top offset {}: {} B ({:.2}% of matched)",
            off,
            bytes,
            100.0 * *bytes as f64 / matched as f64
        );
    }

    let job = args.job_size.max(1);
    for &depth in &args.depth {
        let mut rej = 0u64;
        let mut rej_seq = 0u64;
        let mut regions: HashMap<u64, u64> = HashMap::new();
        for &(pos, ml, off) in &exec {
            let k = pos / job;
            if k == 0 {
                continue; // job zero never arms the ramp
            }
            let x = pos - k * job;
            if rejected(off, x, depth) {
                rej += ml as u64;
                rej_seq += 1;
                if let Some(region) = pos.checked_div(args.region) {
                    *regions.entry(region).or_insert(0) += ml as u64;
                }
            }
        }
        println!(
            "depth {}: rejected {} B, {} seqs ({:.2}% of matched, {:.2}% of frame)",
            depth,
            rej,
            rej_seq,
            100.0 * rej as f64 / matched as f64,
            100.0 * rej as f64 / n as f64
        );
        if args.region > 0 {
            let mut rows: Vec<(u64, u64)> = regions.into_iter().collect();
            rows.sort_unstable_by_key(|r| r.0);
            for (r, bytes) in rows {
                println!("depth {depth} region {r}: {bytes} B");
            }
        }
    }
}

pub fn run(args: &Args) {
    #[cfg(not(feature = "seq_dump"))]
    {
        let _ = args;
        eprintln!("error: offhist needs the decoder dump hook; rebuild with `--features seq_dump`");
        std::process::exit(1);
    }
    #[cfg(feature = "seq_dump")]
    run_dump(args);
}

#[cfg(all(test, feature = "seq_dump"))]
mod tests {
    use super::rejected;

    #[test]
    fn band_is_half_open_above_the_depth() {
        // depth 10, match at distance x=5: offsets 6..14 reject, 5 and 15 keep.
        assert!(!rejected(5, 5, 10));
        assert!(rejected(6, 5, 10));
        assert!(rejected(14, 5, 10));
        assert!(!rejected(15, 5, 10));
        // In-job sources (offset <= x) never reject, any depth.
        assert!(!rejected(5, 5, 1 << 20));
        assert!(!rejected(1, 5, 10));
    }
}
