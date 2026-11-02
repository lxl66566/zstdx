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

/// Fills at least this large count as strip ingestions rather than
/// block-boundary refills: the opt rows' job fills start mid-window (the
/// tail-half bound), so position cannot classify them.
pub const STRIP_MIN_BYTES: u64 = 1024 * 1024;

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
    /// The per-job head-table clear inside `prefill_window` (the
    /// strategy's heads, `clear_table`; a sub-span of `prefill_ns`).
    pub clear_ns: u64,
    /// The strategy grid fill inside `prefill_window` (chain/row/fast strip
    /// inserts; a sub-span of `prefill_ns`).
    pub grid_fill_ns: u64,
    /// The strip seed scan (`acquire_seed`; a sub-span of `prefill_ns`).
    pub seed_scan_ns: u64,
    /// The incompressibility probe's strip sample pass (`seed_gate_probe`;
    /// a sub-span of `prefill_ns`).
    pub gate_ns: u64,
    /// The `strip_is_uniform` whole-span scan (`windowed_ldm_prefill`'s
    /// window span plus the strip scans inside `fill_window_grid`; a
    /// sub-span of `prefill_ns`).
    pub uniform_ns: u64,
    /// The LDM strip fill inside `prefill_window` (a sub-span of
    /// `prefill_ns`).
    pub ldm_fill_ns: u64,
    /// The caller-side shared-prefix-fill build (`build_spf`'s segment
    /// loop plus its snapshot clone; the bulk path's
    /// `build_prefix_snapshot`). NOT a sub-span of `prefill_ns` — it runs
    /// on the posting thread, so without its own span the build is
    /// invisible to the decomposition (the r17 lesson).
    pub spf_build_ns: u64,
    /// The builder's LDM `fill_to_freeze` segments (a sub-span of
    /// `spf_build_ns`).
    pub spf_ldm_ns: u64,
    /// The builder's chain grid segments (a sub-span of `spf_build_ns`).
    pub spf_chain_ns: u64,
    /// The adopters' LDM remainder fills (`strip_fill_continue`).
    pub adopt_ldm_ns: u64,
    /// The adopters' chain grid remainders (`strip_fill_continue`).
    pub adopt_chain_ns: u64,
    /// The whole `finish` call — the finish tail's total span.
    pub fin_ns: u64,
    /// `settle_probe` inside the finish/flush path (carries the donation
    /// wait: `resolve_probe`'s waits for the two donated parses).
    pub fin_settle_ns: u64,
    /// The donation wait proper (`wait_donation` parking; a sub-span of
    /// `fin_settle_ns`).
    pub don_wait_ns: u64,
    /// The finish/flush post loop (posting the tail jobs).
    pub fin_post_ns: u64,
    /// `drain_all` inside the finish/flush path — the parallel tail span.
    pub fin_drain_ns: u64,
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
    /// The caller-side reach-probe donation (the keep-side probe parse,
    /// the shrink-side `parse_cost` and the verdict's feedback prefix):
    /// serial posting-thread work before any job starts, so without its
    /// own span the wall's largest single piece is invisible to the
    /// decomposition (the R25 lesson — the probe outruns every job).
    pub probe_ns: u64,
    /// DUBT compare calls (`DubtFinder::count`) and their summed match
    /// lengths — the compare volume the search pays, ST against job.
    pub cmp_calls: u64,
    pub cmp_bytes: u64,
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
    spf_ldm_ns,
    spf_chain_ns,
    adopt_ldm_ns,
    adopt_chain_ns,
    fin_ns,
    fin_settle_ns,
    don_wait_ns,
    fin_post_ns,
    fin_drain_ns,
    grid_fill_ns,
    seed_scan_ns,
    gate_ns,
    uniform_ns,
    ldm_fill_ns,
    spf_build_ns,
    job_ns,
    prefill_ns,
    clear_ns,
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
    probe_ns,
    cmp_calls,
    cmp_bytes,
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

/// Record one reach-probe job-zero donation span (`donate_job_zero_prefix`).
#[inline]
pub fn add_probe(started: Instant) {
    add_ns(&C.probe_ns, started.elapsed());
}

pub fn add_grid_fill(started: Instant) {
    add_ns(&C.grid_fill_ns, started.elapsed());
}

/// Record one strip seed-scan span inside `prefill_window`.
#[inline]
pub fn add_seed_scan(started: Instant) {
    add_ns(&C.seed_scan_ns, started.elapsed());
}

/// Record one probe sample-pass span inside `prefill_window`.
#[inline]
pub fn add_gate(started: Instant) {
    add_ns(&C.gate_ns, started.elapsed());
}

/// Record one `strip_is_uniform` span (the window-span scan of
/// `windowed_ldm_prefill`, the strip scans inside `fill_window_grid`).
#[inline]
pub fn add_uniform(started: Instant) {
    add_ns(&C.uniform_ns, started.elapsed());
}

/// Record one LDM strip fill span inside `prefill_window`.
#[inline]
pub fn add_ldm_fill(started: Instant) {
    add_ns(&C.ldm_fill_ns, started.elapsed());
}

/// Record one caller-side shared-prefix-fill build span (the stream core's
/// `build_spf` fill loop plus its snapshot clone, the bulk path's
/// `build_prefix_snapshot`, the serial LDM feed's whole generation pass —
/// see [`Snapshot::spf_build_ns`]).
#[inline]
pub fn add_spf_build(started: Instant) {
    add_ns(&C.spf_build_ns, started.elapsed());
}

/// Record one builder LDM segment span (`strip_fill_segment`'s
/// `fill_to_freeze`).
#[inline]
pub fn add_spf_ldm(started: Instant) {
    add_ns(&C.spf_ldm_ns, started.elapsed());
}

/// Record one builder chain-grid segment span (`strip_fill_segment`'s
/// `chain_grid_fill`).
#[inline]
pub fn add_spf_chain(started: Instant) {
    add_ns(&C.spf_chain_ns, started.elapsed());
}

/// Record one adopter LDM remainder span (`strip_fill_continue`).
#[inline]
pub fn add_adopt_ldm(started: Instant) {
    add_ns(&C.adopt_ldm_ns, started.elapsed());
}

/// Record one adopter chain-grid remainder span (`strip_fill_continue`).
#[inline]
pub fn add_adopt_chain(started: Instant) {
    add_ns(&C.adopt_chain_ns, started.elapsed());
}

/// Record one `finish` call span.
#[inline]
pub fn add_fin(started: Instant) {
    add_ns(&C.fin_ns, started.elapsed());
}

/// Record one finish-path `settle_probe` span.
#[inline]
pub fn add_fin_settle(started: Instant) {
    add_ns(&C.fin_settle_ns, started.elapsed());
}

/// Record one donation-wait parking span (`wait_donation`).
#[inline]
pub fn add_don_wait(started: Instant) {
    add_ns(&C.don_wait_ns, started.elapsed());
}

/// Record one finish-path post-loop span.
#[inline]
pub fn add_fin_post(started: Instant) {
    add_ns(&C.fin_post_ns, started.elapsed());
}

/// Record one finish-path `drain_all` span.
#[inline]
pub fn add_fin_drain(started: Instant) {
    add_ns(&C.fin_drain_ns, started.elapsed());
}

/// Record one head-table clear span inside `prefill_window`.
#[inline]
pub fn add_clear(started: Instant) {
    add_ns(&C.clear_ns, started.elapsed());
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

/// Record one large `update_tree` fill: `strip` distinguishes the job's
/// strip ingestion (see [`STRIP_MIN_BYTES`]) from an in-job block-boundary
/// refill.
#[inline]
pub fn add_fill(started: Instant, strip: bool, bytes: u64) {
    let elapsed = started.elapsed();
    if strip {
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

/// Record one DUBT-tree compare (`dubt::count` and the opt rows'
/// tree-search sites): the call count and the matched length separate
/// compare volume from compare count.
#[inline]
pub fn add_cmp(bytes: usize) {
    C.cmp_calls.fetch_add(1, Ordering::Relaxed);
    C.cmp_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Start a fill span of `bytes` if it crosses the trace threshold;
/// otherwise `None`, and the call site skips all recording.
#[inline]
pub fn fill_start(bytes: u64) -> Option<Instant> {
    (bytes >= FILL_MIN_BYTES).then(Instant::now)
}

/// The bulk path's schedule timeline, this module's build only: with
/// `ZSTDX_TRACE_TIMELINE` set, one stderr line per job claim and
/// completion, in microseconds since the first claim. The span sums
/// above cannot answer makespan questions (which job sets the wall, how
/// wide the per-job cost spread is), and the stream core's probe of the
/// same variable does not cover the bulk path.
mod bulk_timeline {
    use std::{eprintln, sync::OnceLock, time::Instant};

    static ON: OnceLock<bool> = OnceLock::new();
    static ORIGIN: OnceLock<Instant> = OnceLock::new();

    fn on() -> bool {
        *ON.get_or_init(|| std::env::var("ZSTDX_TRACE_TIMELINE").is_ok())
    }

    fn rel_us() -> u128 {
        ORIGIN.get_or_init(Instant::now).elapsed().as_micros()
    }

    pub fn claim(id: usize, start: usize, end: usize) {
        if on() {
            eprintln!("bulk claim {id} {start}..{end} @{}", rel_us());
        }
    }

    pub fn done(id: usize, started: Instant) {
        if on() {
            eprintln!(
                "bulk done {id} took {} @{}",
                started.elapsed().as_micros(),
                rel_us()
            );
        }
    }
}

/// Timeline hook: a worker claimed bulk job `id` over `[start, end)`.
pub fn bulk_claim(id: usize, start: usize, end: usize) {
    bulk_timeline::claim(id, start, end);
}

/// Timeline hook: bulk job `id` finished (started at `started`).
pub fn bulk_done(id: usize, started: Instant) {
    bulk_timeline::done(id, started);
}

/// Sorted-descent event-stream capture for out-of-tree kernel probes
/// (`ZSTDX_DESCENT_CAPTURE=<path>,<pos_threshold>,<max_records>`). The
/// first `find_best` call at or past `pos_threshold` snapshots the finder
/// (window bytes, hash heads, tree ring, geometry, cursor), and every
/// following `find_best`/`fill_to` call is recorded with its inputs and
/// outputs. Replaying the records over the snapshot reproduces the exact
/// call sequence over the exact evolving state, so two descent
/// implementations can be diffed per call and per final state. Recording
/// stops at `max_records` and the file is left for the probe.
pub mod descent_capture {
    use std::{
        eprintln,
        fs::File,
        io::{BufWriter, Write},
        string::String,
        sync::{
            Mutex,
            atomic::{AtomicU8, Ordering},
        },
    };

    const MAGIC: u32 = 0x53444b33;

    #[repr(C)]
    pub struct Header {
        pub magic: u32,
        pub table_len: u32,
        pub bt_len: u32,
        pub table_log: u32,
        pub bt_mask: u32,
        pub nb_compares: u32,
        pub min_match: u32,
        pub sufficient_len: u32,
        pub max_window: u64,
        pub win_len: u64,
        pub win_base: u64,
        pub next_update: u64,
        pub block_end: u32,
    }

    /// One recorded call; `kind` 0 is `fill_to`, 1 is `find_best`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CallRecord {
        pub kind: u8,
        pub idx: u32,
        pub target_idx: u32,
        pub block_end: u32,
        pub ll0: u8,
        pub rep_gated: u8,
        pub rep: [u32; 3],
        pub ltb: u32,
        pub scale: i32,
        pub win_base: u64,
        pub nu_before: u64,
        pub nu_after: u64,
        pub rep_len: u32,
        pub rep_off: u32,
        pub cand_score: i32,
        pub cand_off: u32,
        pub cand_len: u32,
    }

    impl CallRecord {
        #[allow(clippy::too_many_arguments)]
        pub fn find_best(
            idx: usize,
            block_end: usize,
            rep: &[u32; 3],
            ll0: u32,
            gated: bool,
            ltb: usize,
            scale: i32,
            win_base: u64,
            nu: u64,
        ) -> Self {
            Self {
                kind: 1,
                idx: idx as u32,
                target_idx: 0,
                block_end: block_end as u32,
                ll0: ll0 as u8,
                rep_gated: gated as u8,
                rep: *rep,
                ltb: ltb as u32,
                scale,
                win_base,
                nu_before: nu,
                nu_after: nu,
                rep_len: 0,
                rep_off: 0,
                cand_score: i32::MIN,
                cand_off: 0,
                cand_len: 0,
            }
        }

        pub fn fill(target_idx: usize, win_base: u64, nu: u64) -> Self {
            Self {
                kind: 0,
                idx: 0,
                target_idx: target_idx as u32,
                block_end: 0,
                ll0: 0,
                rep_gated: 0,
                rep: [0; 3],
                ltb: 0,
                scale: 0,
                win_base,
                nu_before: nu,
                nu_after: nu,
                rep_len: 0,
                rep_off: 0,
                cand_score: i32::MIN,
                cand_off: 0,
                cand_len: 0,
            }
        }
    }

    // 0 idle, 1 disabled, 2 armed, 3 done (record budget spent).
    static STATE: AtomicU8 = AtomicU8::new(0);
    static WRITER: Mutex<Option<BufWriter<File>>> = Mutex::new(None);
    static REMAINING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn env_cfg() -> Option<&'static (String, u64, u64)> {
        static CFG: std::sync::OnceLock<Option<(String, u64, u64)>> = std::sync::OnceLock::new();
        CFG.get_or_init(|| {
            let v = std::env::var("ZSTDX_DESCENT_CAPTURE").ok()?;
            let mut it = v.split(',');
            let path = String::from(it.next()?);
            let pos: u64 = it.next()?.parse().ok()?;
            let max: u64 = it.next().unwrap_or("1000000000").parse().ok()?;
            Some((path, pos, max))
        })
        .as_ref()
    }

    /// Arm at the first `find_best` at or past the configured position:
    /// snapshot the finder state, then records flow. No-op without the env.
    #[allow(clippy::too_many_arguments)]
    pub fn maybe_arm(
        win: &[u8],
        win_base: u64,
        table: &[u32],
        table_log: u32,
        bt: &[u32],
        bt_mask: usize,
        nb_compares: usize,
        min_match: usize,
        sufficient_len: usize,
        max_window: u64,
        block_end: usize,
        next_update: u64,
        pos: u64,
    ) {
        if STATE.load(Ordering::Relaxed) != 0 {
            return;
        }
        let Some((path, threshold, max)) = env_cfg() else {
            STATE.store(1, Ordering::Relaxed);
            return;
        };
        if pos < *threshold {
            // Keep polling until the threshold is crossed (env cached).
            return;
        }
        let file = File::create(path.as_str()).expect("descent capture: create");
        let mut w = BufWriter::with_capacity(1 << 20, file);
        let hdr = Header {
            magic: MAGIC,
            table_len: table.len() as u32,
            bt_len: bt.len() as u32,
            table_log,
            bt_mask: bt_mask as u32,
            nb_compares: nb_compares as u32,
            min_match: min_match as u32,
            sufficient_len: sufficient_len as u32,
            max_window,
            win_len: win.len() as u64,
            win_base,
            next_update,
            block_end: block_end as u32,
        };
        w.write_all(hdr_as_bytes(&hdr)).unwrap();
        w.write_all(win).unwrap();
        w.write_all(slice_as_u8(table)).unwrap();
        w.write_all(slice_as_u8(bt)).unwrap();
        *WRITER.lock().unwrap() = Some(w);
        REMAINING.store(*max, Ordering::Relaxed);
        STATE.store(2, Ordering::Relaxed);
        eprintln!("descent capture armed at pos {pos} -> {path}");
    }

    /// Record one call; a no-op until armed, ends the capture at budget.
    pub fn record(rec: CallRecord) {
        if STATE.load(Ordering::Relaxed) != 2 {
            return;
        }
        let left = REMAINING.fetch_sub(1, Ordering::Relaxed);
        if left == 0 {
            if let Some(mut w) = WRITER.lock().unwrap().take() {
                let _ = w.flush();
            }
            STATE.store(3, Ordering::Relaxed);
            eprintln!("descent capture done");
            return;
        }
        let mut guard = WRITER.lock().unwrap();
        if let Some(w) = guard.as_mut() {
            w.write_all(rec_as_bytes(&rec)).unwrap();
        }
    }

    fn hdr_as_bytes(h: &Header) -> &[u8] {
        // SAFETY: Header is repr(C); read-only view.
        unsafe { std::slice::from_raw_parts(std::ptr::addr_of!(*h).cast(), size_of::<Header>()) }
    }

    fn rec_as_bytes(r: &CallRecord) -> &[u8] {
        // SAFETY: CallRecord is repr(C); read-only view.
        unsafe {
            std::slice::from_raw_parts(std::ptr::addr_of!(*r).cast(), size_of::<CallRecord>())
        }
    }

    fn slice_as_u8(s: &[u32]) -> &[u8] {
        // SAFETY: u32 slice reinterpreted as bytes; alignment of u8 is 1.
        unsafe { std::slice::from_raw_parts(s.as_ptr().cast(), s.len() * 4) }
    }
}

/// Whole-block fast-scan capture for out-of-tree scan-kernel probes
/// (`ZSTDX_SCAN_CAPTURE=<path>,<pos_threshold>,<max_records>`). The first
/// `start_matching_fast` entry at or past `pos_threshold` (absolute
/// position) arms the file; each of the following fast-scan block
/// invocations becomes one record: the scan's full entry state (window
/// slice, head table, cursor/repcode/seed/ramp state, instantiation
/// flags) written ahead of the body, and its full result (literals,
/// packed sequences, post table, post driver state) after it. Restoring
/// the entry state and re-running the same instantiation must reproduce
/// the recorded result bit-exactly — the replay harness's exactness gate
/// — and timing the re-runs gives the per-position tsc figure kernel
/// probes compare against. Single-threaded encodes only: pool workers
/// share the recorder and would interleave records.
pub mod scan_capture {
    use std::{
        eprintln,
        fs::File,
        io::{BufWriter, Write},
        string::String,
        sync::{
            Mutex,
            atomic::{AtomicU8, Ordering},
        },
    };

    use crate::encoding::SeqWord;

    const FILE_MAGIC: u32 = 0x53434631;
    const RECORD_MAGIC: u32 = 0x53435231;

    /// Instantiation flags of the recorded scan body (the const-generic
    /// axes of `start_matching_fast` plus derived state).
    pub mod flags {
        pub const RAMPED: u32 = 1 << 0;
        pub const DENSE: u32 = 1 << 1;
        pub const SMALL_WIDE: u32 = 1 << 2;
        pub const LDM: u32 = 1 << 3;
        /// `HASH_LOG` is a real const, not the runtime-log marker.
        pub const CONST_LOG: u32 = 1 << 4;
        /// The block carries unconsumed LDM candidates inside its span
        /// (replay through the stock single-segment body requires this
        /// clear: a live candidate ends a segment early).
        pub const LDM_CAND: u32 = 1 << 5;
    }

    /// `CoveredFill` wire codes (the fast scan sees the first two).
    pub mod fill {
        pub const DENSE: u32 = 0;
        pub const STRIDED: u32 = 1;
        pub const DICT_DENSE: u32 = 2;
    }

    /// Entry state of one recorded scan block, extracted by the call
    /// site. Cursor fields are rebased (`abs - fast_shift`) exactly as
    /// the scan body derives them; `fast_shift` rebuilds the trues.
    #[allow(clippy::struct_excessive_bools)]
    pub struct EntryState<'a> {
        pub flags: u32,
        pub hash_log: u32,
        pub covered_fill: u32,
        pub win: &'a [u8],
        pub table: &'a [u32],
        pub lit_lens: &'a [u8; 256],
        pub win_base: u64,
        pub fast_shift: u64,
        pub block_end: u64,
        pub pos: u64,
        pub anchor: u64,
        pub insert_max: u64,
        pub max_window: u64,
        pub miss_count: u64,
        /// Rebased `start`, `end`, `depth`.
        pub ramp: [u64; 3],
        pub rep: [u32; 3],
        pub rep_pending: u32,
        pub seed_offset: u32,
        pub seed_hits: u32,
        pub seed_budget: u32,
        pub lits_len: u32,
        pub seqs_len: u32,
    }

    #[repr(C)]
    struct EntryHeader {
        magic: u32,
        flags: u32,
        hash_log: u32,
        table_len: u32,
        covered_fill: u32,
        win_len: u64,
        win_base: u64,
        fast_shift: u64,
        block_end: u64,
        pos: u64,
        anchor: u64,
        insert_max: u64,
        max_window: u64,
        miss_count: u64,
        ramp: [u64; 3],
        rep: [u32; 3],
        rep_pending: u32,
        seed_offset: u32,
        seed_hits: u32,
        seed_budget: u32,
        lits_len: u32,
        seqs_len: u32,
    }

    /// Post-scan driver state of one recorded scan block. `pos` and
    /// `anchor` are the save-back values (both `self.block_end`, true
    /// coordinates); the covered-fill and scan-density picks for the next
    /// block arrive through the next record's entry header.
    pub struct ExitState {
        pub pos: u64,
        pub miss_count: u64,
        pub rep: [u32; 3],
        pub rep_pending: u32,
        pub seed_offset: u32,
        pub seed_hits: u32,
        pub seed_budget: u32,
    }

    #[repr(C)]
    struct ExitHeader {
        magic: u32,
        pos: u64,
        miss_count: u64,
        rep: [u32; 3],
        rep_pending: u32,
        seed_offset: u32,
        seed_hits: u32,
        seed_budget: u32,
        lits_len: u32,
        seqs_len: u32,
    }

    #[repr(C)]
    struct FileHeader {
        magic: u32,
        version: u32,
    }

    // 0 idle, 1 disabled, 2 armed, 3 done (record budget spent). OPEN
    // pairs the two hooks: an entry record without budget must not leave
    // its exit hook writing an orphan half-record.
    static STATE: AtomicU8 = AtomicU8::new(0);
    static OPEN: AtomicU8 = AtomicU8::new(0);
    static WRITER: Mutex<Option<BufWriter<File>>> = Mutex::new(None);
    static REMAINING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn env_cfg() -> Option<&'static (String, u64, u64)> {
        static CFG: std::sync::OnceLock<Option<(String, u64, u64)>> = std::sync::OnceLock::new();
        CFG.get_or_init(|| {
            let v = std::env::var("ZSTDX_SCAN_CAPTURE").ok()?;
            let mut it = v.split(',');
            let path = String::from(it.next()?);
            let pos: u64 = it.next()?.parse().ok()?;
            let max: u64 = it.next().unwrap_or("8").parse().ok()?;
            Some((path, pos, max))
        })
        .as_ref()
    }

    /// Scan-entry hook: arm at the configured position, then write one
    /// entry record per fast-scan block while the budget lasts. A no-op
    /// without the env or after the budget.
    pub fn at_entry(e: &EntryState<'_>) {
        let state = STATE.load(Ordering::Relaxed);
        if state == 1 || state == 3 {
            return;
        }
        if state == 0 {
            let Some((path, threshold, max)) = env_cfg() else {
                STATE.store(1, Ordering::Relaxed);
                return;
            };
            if e.pos + e.fast_shift < *threshold {
                // Keep polling until the threshold is crossed (env cached).
                return;
            }
            let file = File::create(path.as_str()).expect("scan capture: create");
            let mut w = BufWriter::with_capacity(1 << 20, file);
            let hdr = FileHeader {
                magic: FILE_MAGIC,
                version: 1,
            };
            // SAFETY: FileHeader is repr(C); read-only view.
            w.write_all(unsafe {
                std::slice::from_raw_parts(std::ptr::addr_of!(hdr).cast(), size_of::<FileHeader>())
            })
            .unwrap();
            *WRITER.lock().unwrap() = Some(w);
            REMAINING.store(*max, Ordering::Relaxed);
            STATE.store(2, Ordering::Relaxed);
            eprintln!(
                "scan capture armed at pos {} -> {}",
                e.pos + e.fast_shift,
                path
            );
        }
        if REMAINING.load(Ordering::Relaxed) == 0 {
            close();
            return;
        }
        REMAINING.fetch_sub(1, Ordering::Relaxed);
        OPEN.store(1, Ordering::Relaxed);
        let hdr = EntryHeader {
            magic: RECORD_MAGIC,
            flags: e.flags,
            hash_log: e.hash_log,
            table_len: e.table.len() as u32,
            covered_fill: e.covered_fill,
            win_len: e.win.len() as u64,
            win_base: e.win_base,
            fast_shift: e.fast_shift,
            block_end: e.block_end,
            pos: e.pos,
            anchor: e.anchor,
            insert_max: e.insert_max,
            max_window: e.max_window,
            miss_count: e.miss_count,
            ramp: e.ramp,
            rep: e.rep,
            rep_pending: e.rep_pending,
            seed_offset: e.seed_offset,
            seed_hits: e.seed_hits,
            seed_budget: e.seed_budget,
            lits_len: e.lits_len,
            seqs_len: e.seqs_len,
        };
        let mut guard = WRITER.lock().unwrap();
        if let Some(w) = guard.as_mut() {
            // SAFETY: EntryHeader is repr(C); read-only view.
            w.write_all(unsafe {
                std::slice::from_raw_parts(std::ptr::addr_of!(hdr).cast(), size_of::<EntryHeader>())
            })
            .unwrap();
            w.write_all(e.win).unwrap();
            w.write_all(slice_as_u8(e.table)).unwrap();
            w.write_all(e.lit_lens).unwrap();
        }
    }

    /// Scan-exit hook: complete the armed record with the block's output
    /// and post state.
    pub fn at_exit(table: &[u32], literals: &[u8], seqs: &[SeqWord], x: &ExitState) {
        if OPEN.swap(0, Ordering::Relaxed) == 0 {
            return;
        }
        let hdr = ExitHeader {
            magic: RECORD_MAGIC,
            pos: x.pos,
            miss_count: x.miss_count,
            rep: x.rep,
            rep_pending: x.rep_pending,
            seed_offset: x.seed_offset,
            seed_hits: x.seed_hits,
            seed_budget: x.seed_budget,
            lits_len: literals.len() as u32,
            seqs_len: seqs.len() as u32,
        };
        let mut guard = WRITER.lock().unwrap();
        if let Some(w) = guard.as_mut() {
            // SAFETY: ExitHeader is repr(C); read-only view.
            w.write_all(unsafe {
                std::slice::from_raw_parts(std::ptr::addr_of!(hdr).cast(), size_of::<ExitHeader>())
            })
            .unwrap();
            w.write_all(literals).unwrap();
            for word in seqs {
                w.write_all(&word.codes.to_le_bytes()).unwrap();
                w.write_all(&word.add.to_le_bytes()).unwrap();
                w.write_all(&word.add_nb.to_le_bytes()).unwrap();
            }
            w.write_all(slice_as_u8(table)).unwrap();
        }
        if REMAINING.load(Ordering::Relaxed) == 0 {
            drop(guard);
            close();
        }
    }

    fn close() {
        if let Some(mut w) = WRITER.lock().unwrap().take() {
            let _ = w.flush();
        }
        if STATE.load(Ordering::Relaxed) == 2 {
            STATE.store(3, Ordering::Relaxed);
            eprintln!("scan capture done");
        }
    }

    fn slice_as_u8(s: &[u32]) -> &[u8] {
        // SAFETY: u32 slice reinterpreted as bytes; alignment of u8 is 1.
        unsafe { std::slice::from_raw_parts(s.as_ptr().cast(), s.len() * 4) }
    }
}
