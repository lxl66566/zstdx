//! Per-job fixed-cost decomposition for the deep tiers (dev feature
//! `job_trace`, consumed by `zstdx-bench jobdecomp`). Wall-clock spans
//! around each job's fixed components accumulate into process-global
//! atomics; jobs run on pool workers, so a global accumulator is the
//! simplest harvest point for the bench to read between iterations.
//!
//! Compiled out entirely unless the feature is enabled. When enabled, the
//! steady-path cost is one threshold compare per fill call; only fills of
//! [`FILL_MIN_BYTES`] or more (job strip ingestion, block-boundary
//! refills) pay two clock reads.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// Fill spans below this many bytes are parse-inherent incremental work (a
/// few positions per search); only larger ones are timed, bounding the
/// untimed residue by the threshold.
pub const FILL_MIN_BYTES: u64 = 8 * 1024;

/// Component totals since the last [`reset`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Snapshot {
    /// Jobs run (`compress_job_blocks` calls).
    pub jobs: u64,
    /// Summed job wall time — exceeds the stream wall clock because jobs
    /// run in parallel; component shares are taken against it.
    pub job_ns: u64,
    /// `prefill_window`: table clears plus the LDM strip pass.
    pub prefill_ns: u64,
    /// Per-job `reset_slice_state` (state, stats and table reset).
    pub reset_ns: u64,
    /// Ultra's seed parse (`ZSTD_initStats_ultra` port).
    pub seed_ns: u64,
    /// Lazy tree fill of the job strip (an `update_tree` starting at the
    /// window base).
    pub strip_fill_ns: u64,
    pub strip_fill_bytes: u64,
    pub strip_fill_calls: u64,
    /// In-job block-boundary refills (the fill-lag mechanism's lag).
    pub lag_fill_ns: u64,
    pub lag_fill_bytes: u64,
    pub lag_fill_calls: u64,
    /// The hash3 table's strip ingestion (`find_hash3` bulk stretches).
    pub hash3_ns: u64,
    pub hash3_bytes: u64,
}

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        struct Counters {
            $($name: AtomicU64,)*
        }
        static C: Counters = Counters {
            $($name: AtomicU64::new(0),)*
        };

        /// Zero every counter.
        pub fn reset() {
            $(C.$name.store(0, Ordering::Relaxed);)*
        }

        /// Load the totals accumulated since the last [`reset`].
        pub fn snapshot() -> Snapshot {
            Snapshot {
                $( $name: C.$name.load(Ordering::Relaxed), )*
            }
        }
    };
}

counters! {
    jobs,
    job_ns,
    prefill_ns,
    reset_ns,
    seed_ns,
    strip_fill_ns,
    strip_fill_bytes,
    strip_fill_calls,
    lag_fill_ns,
    lag_fill_bytes,
    lag_fill_calls,
    hash3_ns,
    hash3_bytes,
}

#[inline]
fn add_ns(field: &AtomicU64, elapsed: std::time::Duration) {
    field.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
}

/// Record one completed job span.
#[inline]
pub fn add_job(started: Instant) {
    add_ns(&C.job_ns, started.elapsed());
    C.jobs.fetch_add(1, Ordering::Relaxed);
}

/// Record one `prefill_window` span.
#[inline]
pub fn add_prefill(started: Instant) {
    add_ns(&C.prefill_ns, started.elapsed());
}

/// Record one state-reset span.
#[inline]
pub fn add_reset(started: Instant) {
    add_ns(&C.reset_ns, started.elapsed());
}

/// Record one seed-parse span.
#[inline]
pub fn add_seed(started: Instant) {
    add_ns(&C.seed_ns, started.elapsed());
}

/// Record one large `update_tree` fill: `at_base` distinguishes the job
/// strip ingestion (fill starting at the window base) from an in-job
/// block-boundary refill.
#[inline]
pub fn add_fill(started: Instant, at_base: bool, bytes: u64) {
    let elapsed = started.elapsed();
    if at_base {
        add_ns(&C.strip_fill_ns, elapsed);
        C.strip_fill_bytes.fetch_add(bytes, Ordering::Relaxed);
        C.strip_fill_calls.fetch_add(1, Ordering::Relaxed);
    } else {
        add_ns(&C.lag_fill_ns, elapsed);
        C.lag_fill_bytes.fetch_add(bytes, Ordering::Relaxed);
        C.lag_fill_calls.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one large `find_hash3` fill.
#[inline]
pub fn add_hash3(started: Instant, bytes: u64) {
    add_ns(&C.hash3_ns, started.elapsed());
    C.hash3_bytes.fetch_add(bytes, Ordering::Relaxed);
}

/// Start a fill span of `bytes` if it crosses the trace threshold;
/// otherwise `None`, and the call site skips all recording.
#[inline]
pub fn fill_start(bytes: u64) -> Option<Instant> {
    (bytes >= FILL_MIN_BYTES).then(Instant::now)
}
