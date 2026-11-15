//! Twin-distance spectrum probe for the deep-offset ramp's arming gate
//! (spec: docs/src/dev/bench/shapes.md, "Probe recommendation for the
//! arming gate").
//!
//! The tool prices, per MT job and per ramp depth D, the match mass a
//! ramped encode would cut: matches whose source lies within D bytes
//! below a job start. It reads the raw input only, so a shape's ramp
//! cost class is known before any ramped frame exists.
//!
//! The scan follows the encoder's far screen. A per-byte gear hash
//! triggers at content-relative positions (the trigger condition is a
//! function of the last 64 bytes alone), so every copy of a repeated
//! fragment triggers at matching offsets; a position grid would miss
//! every misaligned twin. Each trigger fingerprints its 64-byte window
//! and probes a table that stores each fingerprint's most recent earlier
//! position: a hit is a twin, and the stored position gives the twin's
//! distance. Distances land in power-of-two buckets.
//!
//! Three gates keep the reading honest. Uniform windows are dropped by
//! the entropy gate, because they twin at every distance and drown the
//! real structure. Twins at or below 1 KiB stay outside the arming bar,
//! because only the first kilobyte of each job can lose them. The
//! verdict itself keys on the cost-law damage: a twin at distance o
//! dies on the first width(o) bytes of each job (see the cost law in
//! shapes.md), so the damage share sums those widths over the job's
//! window sample and the raw band share stays a separate column.

use std::{collections::HashMap, fs, path::PathBuf};

use zstdx::{InputShape, Level, encoding::mt_job_size_for};

/// Fingerprint window, the far screen's own size (LDM's
/// MIN_MATCH_LENGTH).
const WIN: usize = 64;
/// Gear trigger: the state's top 6 bits all zero, about one trigger per
/// 64 bytes on wide-alphabet content.
const TRIGGER_MASK: u64 = u64::MAX << 58;
/// Twin distances at or below this stay outside the arming bar: a job
/// position at or past this distance above its start reaches such a
/// twin inside its own job, so only each job's first kilobyte can lose
/// them. This is where fasta, md and csv win.
const TINY_LAG: u64 = 1024;
/// Gear state seed, the far screen's own start state.
const GEAR_SEED: u64 = 0xffff_ffff_0000_0000;
/// Default arming bar, in percent of the cost-law damage share.
const BAR_DEFAULT: f64 = 1.0;

/// One random u64 per byte value (the far screen's splitmix64 table).
static GEAR_TAB: [u64; 256] = gear_table();

const fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

const fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut i = 0usize;
    while i < 256 {
        t[i] = mix64(i as u64 ^ 0x1d8e_4e27_c47d_124f);
        i += 1;
    }
    t
}

/// The four re-associated gear checkpoints over the bytes `w[0..4]` from
/// chain state `h` (the portable form of the LDM split pass's gear4).
fn gear4(tab: &[u64; 256], w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    debug_assert!(w.len() >= 4);
    let g0 = tab[w[0] as usize];
    let g1 = tab[w[1] as usize];
    let g2 = tab[w[2] as usize];
    let g3 = tab[w[3] as usize];
    let p2 = g1.wrapping_add(g0 << 1);
    let p3 = g2.wrapping_add(p2 << 1);
    let p4 = g3.wrapping_add(p3 << 1);
    (
        g0.wrapping_add(h << 1),
        p2.wrapping_add(h << 2),
        p3.wrapping_add(h << 3),
        p4.wrapping_add(h << 4),
    )
}

/// 64-byte window fingerprint: an 8-lane multiply-xor fold, one lane
/// per 8-byte tile of the window, with a splitmix-style finalizer. The
/// fold must cover the whole window: the encoder's far screen folds
/// overlapping lanes from byte offsets 0..8, so its fingerprint reads
/// only the first 15 bytes and structured records that share a 15-byte
/// skeleton (csv and json field templates) collide on it wholesale.
/// For the screen's accept-only twin count that stayed inside its 50x
/// margins; a distance spectrum built on it would drown in skeleton
/// twins.
fn window_fp(win: &[u8]) -> u64 {
    debug_assert_eq!(win.len(), WIN);
    let lane = |k: usize| u64::from_le_bytes(win[k * 8..k * 8 + 8].try_into().unwrap());
    let acc = lane(0).wrapping_mul(0xa076_1d64_78bd_642f)
        ^ lane(1).wrapping_mul(0xe703_7ed1_a0b4_28db)
        ^ lane(2).wrapping_mul(0x8ebc_6af0_9c88_c6e3)
        ^ lane(3).wrapping_mul(0x5899_65cc_7587_4f13)
        ^ lane(4).wrapping_mul(0x1d8e_4e27_c47d_124f)
        ^ lane(5).wrapping_mul(0xeb44_acca_b455_d165)
        ^ lane(6).wrapping_mul(0xc685_6960_5a92_1e65)
        ^ lane(7).wrapping_mul(0x7394_4f5b_82d3_8245);
    mix64(acc)
}

/// The cost-law band width: the span of job positions, in bytes above a
/// job start, on which a twin at distance `o` dies under depth `d`. The
/// twin is lost where its source sits within `d` bytes below the job
/// start, i.e. for positions `x` in `(max(0, o-d), min(o, job))`; zero
/// once `o` passes `d + job` (shapes.md, the cost law).
fn band_width(o: u64, d: u64, job: u64) -> u64 {
    o.min(job).saturating_sub(o.saturating_sub(d))
}

/// Open-addressed fingerprint table. Slot 0 doubles as the empty
/// sentinel, so a fingerprint that hashes to 0 itself is skipped (a
/// 2^-64 event, deterministic either way). A probe hit updates the
/// stored position to the most recent trigger, so every later copy of
/// the window reads the distance to its nearest earlier twin.
struct TwinTable {
    fps: Vec<u64>,
    pos: Vec<u64>,
    live: usize,
}

impl TwinTable {
    fn new(sample_len: usize) -> Self {
        // One slot per ~48 bytes of input (expected inserts: one per
        // 64) keeps the load factor under two thirds before growth.
        let slots = (sample_len / 48).next_power_of_two().max(64);
        Self {
            fps: vec![0; slots],
            pos: vec![0; slots],
            live: 0,
        }
    }

    /// Insert `(fp, pos)`; `Some(prev)` when an equal fingerprint already
    /// sat in the table (a twin at distance `pos - prev`).
    fn insert(&mut self, fp: u64, pos: u64) -> Option<u64> {
        if fp == 0 {
            return None;
        }
        if self.live * 3 >= self.fps.len() * 2 {
            self.grow();
        }
        let mask = self.fps.len() - 1;
        let mut slot = fp as usize & mask;
        loop {
            let e = self.fps[slot];
            if e == fp {
                let prev = self.pos[slot];
                self.pos[slot] = pos;
                return Some(prev);
            }
            if e == 0 {
                self.fps[slot] = fp;
                self.pos[slot] = pos;
                self.live += 1;
                return None;
            }
            slot = (slot + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let old = core::mem::take(&mut self.fps);
        let old_pos = core::mem::take(&mut self.pos);
        self.fps = vec![0; old.len() * 2];
        self.pos = vec![0; old.len() * 2];
        self.live = 0;
        for (i, &fp) in old.iter().enumerate() {
            if fp != 0 {
                self.insert_raw(fp, old_pos[i]);
            }
        }
    }

    fn insert_raw(&mut self, fp: u64, pos: u64) {
        let mask = self.fps.len() - 1;
        let mut slot = fp as usize & mask;
        while self.fps[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.fps[slot] = fp;
        self.pos[slot] = pos;
        self.live += 1;
    }
}

/// Per-job accumulation of the probe's readings.
struct JobStats {
    /// Window samples: triggers whose window exists (the share
    /// denominator).
    samples: u64,
    /// Uniform windows dropped by the entropy gate.
    gated: u64,
    /// All twins including tiny-lag.
    twins: u64,
    /// Twins at or below TINY_LAG.
    tiny: u64,
    /// Non-tiny twins per power-of-two distance bucket.
    buckets: Vec<u64>,
    /// Per depth: non-tiny twins with distance inside (0, D+J].
    in_band: Vec<u64>,
    /// Per depth: summed cost-law band widths of those twins, in bytes
    /// of job span.
    damage: Vec<u64>,
    /// Exact-distance histogram of the non-tiny twins.
    distances: HashMap<u64, u64>,
}

impl JobStats {
    fn new(nbuckets: usize, ndepths: usize) -> Self {
        Self {
            samples: 0,
            gated: 0,
            twins: 0,
            tiny: 0,
            buckets: vec![0; nbuckets],
            in_band: vec![0; ndepths],
            damage: vec![0; ndepths],
            distances: HashMap::new(),
        }
    }

    /// The job's window mass: samples minus gated.
    fn mass(&self) -> u64 {
        self.samples - self.gated
    }

    /// Share of the window mass twinning inside the band (0, D+J], in
    /// percent.
    fn band_share(&self, di: usize) -> f64 {
        pct(self.in_band[di], self.mass())
    }

    /// Cost-law damage share of the band twins, in percent of the job
    /// span: the summed band widths over the job's window mass times
    /// the job size.
    fn damage_share(&self, di: usize, job: u64) -> f64 {
        100.0 * self.damage[di] as f64 / (self.mass().max(1) as f64 * job as f64)
    }
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

/// The per-job arming verdict at one depth: arm when the cost-law damage
/// the ramp would cut stays at or below the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Arm,
    Hold,
}

impl Verdict {
    fn decide(damage_pct: f64, bar: f64) -> Self {
        if damage_pct <= bar {
            Verdict::Arm
        } else {
            Verdict::Hold
        }
    }
}

impl core::fmt::Display for Verdict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Verdict::Arm => "arm",
            Verdict::Hold => "hold",
        })
    }
}

/// Run the probe over `data` on the `job_size` lattice for each depth in
/// `depths`, accumulating one `JobStats` per job.
fn probe(data: &[u8], job_size: u64, depths: &[u64]) -> Vec<JobStats> {
    let job_size = job_size.max(1);
    let njobs = data.len().div_ceil(job_size as usize).max(1);
    let nbuckets = (data.len().max(1) as u64).ilog2() as usize + 2;
    let mut jobs = (0..njobs)
        .map(|_| JobStats::new(nbuckets, depths.len()))
        .collect::<Vec<_>>();
    let mut table = TwinTable::new(data.len());
    let mut h = GEAR_SEED;

    let visit = |p: usize, table: &mut TwinTable, jobs: &mut Vec<JobStats>| {
        let s = &mut jobs[p / job_size as usize];
        if p + 1 < WIN {
            return;
        }
        s.samples += 1;
        let win = &data[p + 1 - WIN..=p];
        if win.iter().all(|&b| b == win[0]) {
            s.gated += 1;
            return;
        }
        let Some(prev) = table.insert(window_fp(win), p as u64) else {
            return;
        };
        let o = p as u64 - prev;
        s.twins += 1;
        if o <= TINY_LAG {
            s.tiny += 1;
            return;
        }
        s.buckets[o.ilog2() as usize] += 1;
        *s.distances.entry(o).or_insert(0) += 1;
        for (di, &d) in depths.iter().enumerate() {
            if o <= d + job_size {
                s.in_band[di] += 1;
                s.damage[di] += band_width(o, d, job_size);
            }
        }
    };

    let mut i = 0usize;
    while i + 4 <= data.len() {
        let (c1, c2, c3, c4) = gear4(&GEAR_TAB, &data[i..], h);
        h = c4;
        if c1 & TRIGGER_MASK == 0 {
            visit(i, &mut table, &mut jobs);
        }
        if c2 & TRIGGER_MASK == 0 {
            visit(i + 1, &mut table, &mut jobs);
        }
        if c3 & TRIGGER_MASK == 0 {
            visit(i + 2, &mut table, &mut jobs);
        }
        if c4 & TRIGGER_MASK == 0 {
            visit(i + 3, &mut table, &mut jobs);
        }
        i += 4;
    }
    for p in data.len() & !3..data.len() {
        h = (h << 1).wrapping_add(GEAR_TAB[data[p] as usize]);
        if h & TRIGGER_MASK == 0 {
            visit(p, &mut table, &mut jobs);
        }
    }
    jobs
}

#[derive(clap::Args)]
pub struct Args {
    /// Raw input file
    file: PathBuf,
    /// MT job size in bytes; 0 derives the encoder's exact lattice for
    /// the level and worker count below (the emitframe job_size)
    #[arg(long, default_value_t = 0)]
    job_size: u64,
    /// Worker count of the encode whose ramp is priced (lattice
    /// derivation only)
    #[arg(long, default_value_t = 4)]
    workers: u32,
    /// Numeric encoder level of the lattice derivation (1-22); the ramp
    /// is the balanced row's own lever
    #[arg(long, default_value_t = 3)]
    level: i32,
    /// Ramp depths to price, in bytes (comma-separated)
    #[arg(long, value_delimiter = ',', default_value = "524288,1048576,2097152")]
    depth: Vec<u64>,
    /// Arming bar, in percent of the cost-law damage share; a job arms
    /// at a depth when its damage stays at or below the bar
    #[arg(long, default_value_t = BAR_DEFAULT)]
    bar: f64,
}

fn parse_level(n: i32) -> Level {
    assert!((1..=22).contains(&n), "level {n} out of range 1-22");
    Level::from_zstd(n)
}

/// Human-readable depth label for table headers.
fn dlabel(d: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * 1024;
    if d % M == 0 {
        format!("{}M", d / M)
    } else if d % K == 0 {
        format!("{}K", d / K)
    } else {
        format!("{d}")
    }
}

pub fn run(args: &Args) {
    assert!(!args.depth.is_empty(), "at least one depth required");
    let raw = fs::read(&args.file).expect("read input");
    let level = parse_level(args.level);
    let job_size = if args.job_size > 0 {
        args.job_size
    } else if raw.is_empty() {
        4 << 20
    } else {
        let shape = InputShape {
            len: Some(raw.len() as u64),
            window_log: None,
        };
        mt_job_size_for(raw.len() as u64, args.workers, level, shape, &raw) as u64
    };
    let jobs = probe(&raw, job_size, &args.depth);

    let mut agg = JobStats::new(
        (raw.len().max(1) as u64).ilog2() as usize + 2,
        args.depth.len(),
    );
    for s in &jobs {
        agg.samples += s.samples;
        agg.gated += s.gated;
        agg.twins += s.twins;
        agg.tiny += s.tiny;
        for (b, &m) in s.buckets.iter().enumerate() {
            agg.buckets[b] += m;
        }
        for di in 0..args.depth.len() {
            agg.in_band[di] += s.in_band[di];
            agg.damage[di] += s.damage[di];
        }
        for (&o, &n) in &s.distances {
            *agg.distances.entry(o).or_insert(0) += n;
        }
    }

    println!(
        "twinspect {}: {} B, job {} x {} (level {}, workers {}, bar {}%)",
        args.file.display(),
        raw.len(),
        job_size,
        jobs.len(),
        args.level,
        args.workers,
        args.bar
    );
    println!(
        "window samples {}, entropy-gated {}, twins {} (tiny-lag {})",
        agg.samples, agg.gated, agg.twins, agg.tiny
    );
    let mass = agg.mass();
    for (b, &m) in agg.buckets.iter().enumerate() {
        if m > 0 {
            println!(
                "bucket [2^{b}, 2^{}): {} ({:.3}% of mass, {:.3}% of twins)",
                b + 1,
                m,
                pct(m, mass),
                pct(m, agg.twins - agg.tiny)
            );
        }
    }
    let mut top: Vec<(u64, u64)> = core::mem::take(&mut agg.distances).into_iter().collect();
    top.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top_list = top
        .iter()
        .take(6)
        .map(|(o, n)| format!("{o} x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("top distances: {top_list}");
    for (di, &d) in args.depth.iter().enumerate() {
        println!(
            "depth {}: band (0, {}]: in-band {} ({:.3}% of mass), damage {:.3}% of job span -> {}",
            d,
            d + job_size,
            agg.in_band[di],
            agg.band_share(di),
            agg.damage_share(di, job_size),
            Verdict::decide(agg.damage_share(di, job_size), args.bar)
        );
    }

    println!("per job (sample mass, band share, damage share of job span):");
    for (k, s) in jobs.iter().enumerate() {
        let cells = args
            .depth
            .iter()
            .enumerate()
            .map(|(di, &d)| {
                format!(
                    "{}: in {:.3}% dmg {:.3}% {}",
                    dlabel(d),
                    s.band_share(di),
                    s.damage_share(di, job_size),
                    Verdict::decide(s.damage_share(di, job_size), args.bar)
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        if k == 0 {
            println!(
                "job 0 (never arms): {} samples, {} twins | {cells}",
                s.samples, s.twins
            );
        } else {
            println!(
                "job {k}: {} samples, {} twins | {cells}",
                s.samples, s.twins
            );
        }
    }
    for (di, &d) in args.depth.iter().enumerate() {
        let held = jobs[1..]
            .iter()
            .filter(|s| Verdict::decide(s.damage_share(di, job_size), args.bar) == Verdict::Hold)
            .count();
        println!(
            "depth {}: {} of {} jobs arm",
            d,
            jobs.len() - 1 - held,
            jobs.len() - 1
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes: a 64-bit LCG whose low byte is
    /// mixed once more, so the trigger density lands near one per 64
    /// bytes like real wide-alphabet content.
    fn lcg(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                mix64(s) as u8
            })
            .collect()
    }

    fn total_twins(jobs: &[JobStats]) -> u64 {
        jobs.iter().map(|s| s.twins).sum()
    }

    fn verdicts(jobs: &[JobStats], di: usize, job: u64, bar: f64) -> Vec<Verdict> {
        jobs.iter()
            .map(|s| Verdict::decide(s.damage_share(di, job), bar))
            .collect()
    }

    /// Empty and sub-window inputs carry no window samples and arm at
    /// every depth.
    #[test]
    fn empty_input_arms() {
        for data in [&b""[..], b"abc", &vec![7u8; 32][..]] {
            let jobs = probe(data, 4096, &[1024, 4096]);
            assert_eq!(total_twins(&jobs), 0);
            for di in 0..2 {
                assert!(
                    verdicts(&jobs, di, 4096, 1.0)
                        .iter()
                        .all(|v| *v == Verdict::Arm)
                );
            }
        }
    }

    /// A constant run either never triggers (its steady gear state keeps
    /// the top bits set) or triggers at every byte; in the second case
    /// the entropy gate must drop the flood before any twin counts, and
    /// the shape arms on an empty band either way.
    #[test]
    fn uniform_windows_are_gated() {
        // The steady state of a constant byte b is -GEAR_TAB[b]; it
        // triggers steadily when that value keeps the top six bits zero.
        let hot = (0..=255u8)
            .find(|&b| GEAR_TAB[b as usize].wrapping_neg() & TRIGGER_MASK == 0)
            .expect("some byte value triggers steadily");
        let jobs = probe(&vec![hot; 256 * 1024], 64 * 1024, &[16384]);
        assert!(
            jobs.iter().all(|s| s.gated > 0),
            "uniform triggers must be gated"
        );
        assert_eq!(total_twins(&jobs), 0);
        assert_eq!(jobs.iter().map(|s| s.in_band[0]).sum::<u64>(), 0);
        // Zeros sit in the other class: the steady state never triggers,
        // so the shape never reaches the fingerprint at all.
        let jobs = probe(&vec![0u8; 256 * 1024], 64 * 1024, &[16384]);
        assert_eq!(total_twins(&jobs), 0);
        assert!(jobs.iter().all(|s| s.samples == 0));
        assert!(
            verdicts(&jobs, 0, 64 * 1024, 1.0)
                .iter()
                .all(|v| *v == Verdict::Arm)
        );
    }

    /// Wide-alphabet random content reads collision-level: 64-bit
    /// fingerprints over 256 KiB produce no twins at all.
    #[test]
    fn random_content_reads_collision_free() {
        let data = lcg(256 * 1024, 0x5eed_1234);
        let jobs = probe(&data, 64 * 1024, &[16384]);
        assert_eq!(total_twins(&jobs), 0);
        assert!(jobs.iter().all(|s| s.samples > 0));
    }

    /// A periodic tail localizes per job: the random head arms, the jobs
    /// whose content repeats hold, and the repeat's distance lands in
    /// one bucket.
    #[test]
    fn periodic_tail_localizes_per_job() {
        let block = lcg(16 * 1024, 0xabc);
        let mut data = lcg(128 * 1024, 0x77);
        for _ in 0..8 {
            data.extend_from_slice(&block);
        }
        let jobs = probe(&data, 64 * 1024, &[16384]);
        let v = verdicts(&jobs, 0, 64 * 1024, 1.0);
        assert_eq!(v[1], Verdict::Arm, "random head job arms");
        assert_eq!(v[2], Verdict::Hold, "first periodic job holds");
        assert_eq!(v[3], Verdict::Hold, "second periodic job holds");
        // Job 2 holds the repeat's first instance, so its band mass is
        // strictly below job 3's.
        assert!(jobs[2].in_band[0] < jobs[3].in_band[0]);
        // The repeat sits at 16 KiB: every non-tiny twin is in bucket
        // 2^14 and inside the (0, 80 KiB] band.
        assert_eq!(jobs[3].buckets[14], jobs[3].twins - jobs[3].tiny);
        assert_eq!(jobs[3].in_band[0], jobs[3].twins - jobs[3].tiny);
    }

    /// A 512-byte repeat produces nothing but tiny-lag twins; they stay
    /// outside the bar and every job arms.
    #[test]
    fn tiny_lag_stays_outside_the_bar() {
        let block = lcg(512, 0x31);
        let mut data = Vec::new();
        for _ in 0..512 {
            data.extend_from_slice(&block);
        }
        let jobs = probe(&data, 64 * 1024, &[16384]);
        assert!(total_twins(&jobs) > 0);
        assert!(
            jobs.iter().all(|s| s.twins == s.tiny),
            "all twins are tiny-lag"
        );
        assert!(jobs.iter().all(|s| s.in_band[0] == 0));
        assert!(
            verdicts(&jobs, 0, 64 * 1024, 1.0)
                .iter()
                .all(|v| *v == Verdict::Arm)
        );
    }

    /// The band width follows the cost law exactly: the twin dies on the
    /// first `min(o, J) - max(0, o-D)` bytes of each job, zero above
    /// D+J.
    #[test]
    fn band_width_follows_the_cost_law() {
        let (j, d) = (4u64 << 20, 2u64 << 20);
        // o at or below D: width o.
        assert_eq!(band_width(441_226, d, j), 441_226);
        // D < o <= J: width D.
        assert_eq!(band_width(3 << 20, d, j), d);
        // J < o <= D+J: width J+D-o.
        assert_eq!(band_width(5 << 20, d, j), j + d - (5 << 20));
        // above D+J: zero, and the boundary itself is empty.
        assert_eq!(band_width(d + j, d, j), 0);
        assert_eq!(band_width(7 << 20, d, j), 0);
        // o above J but below D: the whole job span loses the twin.
        assert_eq!(band_width(9 << 20, 16 << 20, j), j);
    }

    /// The verdict keys on the damage bar alone: a share at the bar arms,
    /// a hair above holds.
    #[test]
    fn verdict_bands_at_the_bar() {
        assert_eq!(Verdict::decide(1.0, 1.0), Verdict::Arm);
        assert_eq!(Verdict::decide(1.001, 1.0), Verdict::Hold);
        assert_eq!(Verdict::decide(0.0, 1.0), Verdict::Arm);
    }
}
