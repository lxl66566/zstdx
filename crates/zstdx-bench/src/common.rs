//! Shared measurement harness for the bench subcommands.
//!
//! Design goals for machines with noticeable performance drift:
//! - **Interleaving**: A and B alternate round by round, so slow drift (thermal, clocks, background
//!   load) hits both sides equally; the per-round ratio is the primary output.
//! - **Warmup**: one unmeasured round each before timing starts. A round slow enough that no cache
//!   holds the working set between rounds makes that warmup round wasted wall time, so the first
//!   round is then kept as a sample instead.
//! - **Time budget**: rounds accumulate until each side ran for `min_secs` (default 0.5 s, override
//!   with `--budget-ms` or the `BENCH_BUDGET_MS` env var), so the total wall time is bounded
//!   without fixing an iteration count. A side that is far faster than its counterpart only needs
//!   `FAST_SIDE_BUDGET_FRAC` of the budget once the slow side is done, because every extra round
//!   then buys one full slow-side round for a handful of fast-side samples.
//! - **Robust stats**: median/min/max plus the median absolute deviation of the ratios; medians
//!   absorb spikes, min approximates the noise floor.
//!
//! One round should take at least a few milliseconds: batch enough
//! iterations inside the closures when the payload is tiny, otherwise the
//! per-round `Instant::now()` overhead pollutes the sample.

use std::time::Instant;

/// Fraction of `min_secs` that suffices for the fast side of a strongly
/// asymmetric cell (deep-level random rows, where the libzstd side runs
/// seconds per round and ours runs milliseconds): once the slow side spent
/// its full budget, the fast side stops at this fraction.
const FAST_SIDE_BUDGET_FRAC: f64 = 0.125;

/// A round at least this slow is kept as the first sample instead of being
/// discarded as warmup. Below this bound the payload plausibly stays in a
/// cache across rounds and the first round reads cold, so the discard
/// stands. Derived from the budget, not a wall-clock constant, so
/// `--budget-ms` moves both together.
const SLOW_ROUND_FRAC: f64 = 1.0 / 3.0;

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub median: f64,
    pub min: f64,
    pub max: f64,
    /// Median absolute deviation.
    pub mad: f64,
}

impl Stats {
    pub fn from_samples(mut samples: Vec<f64>) -> Self {
        assert!(!samples.is_empty(), "stats need at least one sample");
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = mid_statistic(&samples);
        let mut devs: Vec<f64> = samples.iter().map(|s| (s - median).abs()).collect();
        devs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Self {
            median,
            min: samples[0],
            max: samples[samples.len() - 1],
            mad: mid_statistic(&devs),
        }
    }

    pub fn mibs(&self, bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0) / self.median
    }
}

/// Median of a sorted sample: the middle element for odd lengths, the mean
/// of the two middle elements for even lengths (an even round count is
/// common because the budget rule appends rounds one at a time).
fn mid_statistic(sorted: &[f64]) -> f64 {
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    }
}

pub struct AbReport {
    pub a: Stats,
    pub b: Stats,
    /// Per-round duration ratios (a/b); the median is the drift-robust
    /// verdict, `> 1.0` means A is slower.
    pub ratios: Stats,
    pub rounds: usize,
}

impl AbReport {
    /// One line: `<name>  a  b  ratio ±mad [min..max]  n=rounds`.
    pub fn print(&self, name: &str, bytes: u64) {
        println!(
            "{name:<16}{a_mibs:>9.0} {b_mibs:>9.0}  x{ratio:>6.3} ±{mad:.3}  [{lo:.3}..{hi:.3}]  \
             n={rounds}",
            a_mibs = self.a.mibs(bytes),
            b_mibs = self.b.mibs(bytes),
            ratio = self.ratios.median,
            mad = self.ratios.mad,
            lo = self.ratios.min,
            hi = self.ratios.max,
            rounds = self.rounds,
        );
    }

    /// Same line as `print`, returned for sections that buffer cell output
    /// and print it in section order.
    pub fn line(&self, name: &str, bytes: u64) -> String {
        format!(
            "{name:<16}{a_mibs:>9.0} {b_mibs:>9.0}  x{ratio:>6.3} ±{mad:.3}  [{lo:.3}..{hi:.3}]  \
             n={rounds}",
            a_mibs = self.a.mibs(bytes),
            b_mibs = self.b.mibs(bytes),
            ratio = self.ratios.median,
            mad = self.ratios.mad,
            lo = self.ratios.min,
            hi = self.ratios.max,
            rounds = self.rounds,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Ab {
    pub warmup_rounds: usize,
    pub min_rounds: usize,
    pub min_secs: f64,
}

impl Default for Ab {
    fn default() -> Self {
        Self {
            warmup_rounds: 1,
            min_rounds: 3,
            min_secs: budget_secs(),
        }
    }
}

/// Per-side measurement budget in seconds; the `--budget-ms` flag maps here
/// via `apply_budget`, `BENCH_BUDGET_MS` overrides the default of 500 ms
/// (e.g. 2000 for a longer, quieter run, or 100 for a quick smoke pass).
pub fn budget_secs() -> f64 {
    std::env::var("BENCH_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.5, |ms| ms / 1000.0)
}

/// Turn a `--budget-ms` flag into the env-based budget consumed by
/// `budget_secs`. Must run before any `Ab` is constructed.
pub fn apply_budget(ms: Option<f64>) {
    if let Some(ms) = ms {
        assert!(ms > 0.0, "--budget-ms must be positive");
        // SAFETY: single-threaded startup, before any bench thread exists.
        unsafe { std::env::set_var("BENCH_BUDGET_MS", format!("{ms}")) };
    }
}

/// Calibration override: `ZSTDX_BENCH_WARMUP=always` forces the
/// unconditional warmup discard of the older harness, for A/B-ing the
/// slow-round skip on the same build.
fn warmup_forced() -> bool {
    std::env::var("ZSTDX_BENCH_WARMUP").as_deref() == Ok("always")
}

impl Ab {
    /// Interleaved A/B measurement of two round closures.
    pub fn measure<A, B>(&self, mut a: A, mut b: B) -> AbReport
    where
        A: FnMut(),
        B: FnMut(),
    {
        let mut a_times = Vec::new();
        let mut b_times = Vec::new();
        for _ in 0..self.warmup_rounds {
            let t = Instant::now();
            a();
            let ta = t.elapsed().as_secs_f64();
            let t = Instant::now();
            b();
            let tb = t.elapsed().as_secs_f64();
            // A round this slow cannot warm anything the next round reads,
            // so it counts as a sample; fast rounds are the warmup discard.
            let slow =
                ta >= self.min_secs * SLOW_ROUND_FRAC || tb >= self.min_secs * SLOW_ROUND_FRAC;
            if slow && !warmup_forced() {
                a_times.push(ta);
                b_times.push(tb);
            }
        }
        let fast_secs = self.min_secs * FAST_SIDE_BUDGET_FRAC;
        let mut rounds = a_times.len();
        loop {
            let t = Instant::now();
            a();
            a_times.push(t.elapsed().as_secs_f64());
            let t = Instant::now();
            b();
            b_times.push(t.elapsed().as_secs_f64());
            rounds += 1;
            let a_total: f64 = a_times.iter().sum();
            let b_total: f64 = b_times.iter().sum();
            // both sides at the full budget, or one side at the full budget
            // and the other clearly the fast side of an asymmetric cell
            let done = rounds >= self.min_rounds
                && ((a_total >= self.min_secs
                    && (b_total >= self.min_secs || b_total >= fast_secs))
                    || (b_total >= self.min_secs && a_total >= fast_secs));
            // safety cap for closures that are too fast to ever accumulate
            // the budget (see the module docs about batching)
            if done || rounds >= 100_000 {
                break;
            }
        }
        let ratios: Vec<f64> = a_times
            .iter()
            .zip(b_times.iter())
            .map(|(x, y)| x / y)
            .collect();
        AbReport {
            a: Stats::from_samples(a_times),
            b: Stats::from_samples(b_times),
            ratios: Stats::from_samples(ratios),
            rounds,
        }
    }
}

/// Single-sided measurement with the same warmup/budget/stats treatment,
/// for benches that have no natural counterpart.
pub fn measure_solo<F: FnMut()>(mut f: F) -> Stats {
    f(); // warmup
    let mut times = Vec::new();
    let budget = budget_secs();
    loop {
        let t = Instant::now();
        f();
        times.push(t.elapsed().as_secs_f64());
        let done = times.len() >= 3 && times.iter().sum::<f64>() >= budget;
        if done || times.len() >= 100_000 {
            break;
        }
    }
    Stats::from_samples(times)
}

/// Selection filter: empty selection means "no restriction".
pub fn want<T: PartialEq>(selected: &[T], v: &T) -> bool {
    selected.is_empty() || selected.contains(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_averages_even_samples() {
        // even count: mean of the two middle samples, for the samples
        // themselves and for the MAD computed off that median
        let s = Stats::from_samples(vec![4.0, 1.0, 3.0, 2.0]);
        assert_eq!(s.median, 2.5);
        // deviations [1.5, 0.5, 0.5, 1.5] -> even-length median 1.0
        assert_eq!(s.mad, 1.0);
        // odd count: the middle sample
        let s = Stats::from_samples(vec![5.0, 1.0, 3.0]);
        assert_eq!(s.median, 3.0);
        assert_eq!(s.mad, 2.0);
    }

    #[test]
    fn min_max_survive_median_averaging() {
        let s = Stats::from_samples(vec![10.0, 0.0, 7.0, 3.0]);
        assert_eq!(s.min, 0.0);
        assert_eq!(s.max, 10.0);
        assert_eq!(s.median, 5.0);
    }
}

#[inline(never)]
pub fn black_box<T>(v: T) -> T {
    std::hint::black_box(v)
}
