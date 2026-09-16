//! Multithreaded slice compression: one frame assembled from independent
//! overlap jobs.
//!
//! The input is split into jobs; each job compresses its byte range with the
//! pooled slice machinery while borrowing an `overlap`-sized strip of the
//! preceding job as match history (`adopt_window` plus `prefill_window`, so
//! the strip's positions actually sit in the search tables), keeping the
//! assembled output a single regular zstd frame. Two invariants make the
//! jobs independent of each other (mirroring libzstd's zstdmt):
//! - every job starts from reset entropy tables, so its first block is fully self-describing (no
//!   Repeat modes across a job boundary);
//! - every job except the first gates repcode references until three literal-offset sequences have
//!   rewritten the repeated-offset history (see [`MatchGeneratorDriver::gate_repcodes`]): the
//!   decoder's history at a job boundary is unknown, but each literal offset shifts it down one
//!   slot, so after three of them the matcher and decoder agree again.
//!
//! The frame checksum is hashed over the whole input on the calling thread
//! while the jobs run, and the ordered assembly appends each job's blocks as
//! soon as they land, overlapping the final copy with the late jobs.

use alloc::vec::Vec;
use core::{
    ops::Range,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex};

use super::{
    Matcher, compress_fastest,
    frame_compressor::{
        CompressState, SliceChecksum, compress_job_blocks, compress_job_blocks_inner,
        new_slice_state, reset_slice_state, return_slice_state, take_slice_state,
    },
    frame_header::FrameHeader,
    match_generator::{LdmArming, MatchGeneratorDriver},
    reach_probe,
};
use crate::{Level, common::MAX_BLOCK_SIZE};

/// Below this size the thread spawn and the ordered assembly cost more than
/// the parallelism saves.
const MIN_MT_INPUT: usize = 2 * 1024 * 1024;
/// Job size floor: smaller jobs multiply the overlap duplication without
/// improving load balance.
pub(crate) const MIN_JOB_SIZE: usize = 1024 * 1024;
/// Job size ceiling: a huge known input would otherwise scale the job size
/// and with it the streaming burst buffer (which holds a whole burst) into
/// memory failure. libzstd's zstdmt caps its job size the same way.
pub(crate) const MAX_JOB_SIZE: usize = 1024 * 1024 * 1024;

/// Job size for an input of `len` bytes at `workers` threads: twice as many
/// jobs as workers keeps the tail balanced, the floor keeps the overlap
/// duplication negligible and scales with the level's overlap, and the
/// ceiling bounds the buffering. The bulk and streaming mt paths share this
/// so a pledged stream matches the bulk output byte for byte.
pub(crate) fn job_size_for(len: u64, workers: u32, overlap: usize) -> usize {
    debug_assert!(workers >= 2);
    len.div_ceil(workers as u64 * 2)
        .max(MIN_JOB_SIZE.max(overlap) as u64)
        .min(MAX_JOB_SIZE as u64) as usize
}

/// The encoder's exact MT job size for an input, with the overlap the
/// level implies — dev tooling (`emitframe`/`piecepipe`) cuts its analysis
/// pieces at these boundaries, so it must not re-derive the formula.
/// Hidden: not API-stable.
#[doc(hidden)]
pub fn mt_job_size_for(
    len: u64,
    workers: u32,
    level: Level,
    shape: crate::InputShape,
    head: &[u8],
) -> usize {
    // The frame's reach probe result (see reach_probe) decides the strip
    // with it.
    let choice = reach_probe::probe_reach_choice(head, level, shape);
    let overlap = MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize;
    job_size_for(len, workers.max(2), overlap)
}

/// Compress `src` into one frame using up to `workers` threads.
///
/// Falls back to the single-threaded slice path (byte-identical to
/// [`super::compress_slice_to_vec` modulo the checksum flag`) when the input
/// is too small, only one worker is requested, the level stores raw blocks,
/// or the process has a single usable core.
pub fn compress_slice_mt(
    src: &[u8],
    level: Level,
    checksum: bool,
    workers: u32,
    window_log: Option<u32>,
) -> Vec<u8> {
    let checksum = checksum && cfg!(feature = "hash");
    if workers < 2
        || src.len() < MIN_MT_INPUT
        || level == Level::Uncompressed
        || std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
    {
        return super::compress_slice_shaped(src, level, checksum, crate::InputShape {
            len: None,
            window_log,
        });
    }

    // Twice as many jobs as workers keeps the tail balanced; the floor keeps
    // the overlap duplication negligible, and scales with the level's
    // overlap so deep-search levels don't pay it per job.
    let shape = crate::InputShape {
        len: Some(src.len() as u64),
        window_log,
    };
    // The dense matchers' domain as strip: the strip is fully indexed (see
    // `prefill_window`), so matches reach across job borders as far as the
    // dense search does. A shorter strip caps the ratio at repeats that
    // fit inside it — the text corpus's ~800K period against a 1 MiB window
    // collapsed the multithreaded ratio by an order of magnitude. LDM rows
    // decouple the two (a wide frame window for far reach, a narrow strip):
    // a full-window strip would scale the job floor with the LDM reach and
    // starve parallelism.
    let window = MatchGeneratorDriver::window_for_level(level, shape);
    // The frame's head decides its chain reach (see reach_probe), and with
    // it the strip: a shrunk search domain shrinks the strip to match.
    // Eligible frames donate the probe's keep side into job zero (see
    // `donate_job_zero_prefix`): the first span blocks run as job zero's
    // own blocks on the calling thread through the job machinery, so only
    // a Shrink verdict pays a re-parse. The keep grid must leave job zero
    // work beyond the span for the continuation to take over.
    let mut donation = None;
    let choice;
    if reach_probe::eligible(level, shape)
        && job_size_for(
            src.len() as u64,
            workers.max(2),
            MatchGeneratorDriver::strip_for_choice(level, shape, reach_probe::ReachChoice::Keep)
                as usize,
        ) >= reach_probe::PROBE_SPAN
    {
        let (donated_choice, state, prefix) = donate_job_zero_prefix(src, level, shape);
        choice = donated_choice;
        if choice == reach_probe::ReachChoice::Keep {
            donation = Some((state, prefix));
        } else {
            // The shrink verdict discards the donation; job zero parses
            // from its own pooled state like an undonated frame.
            return_slice_state(state);
        }
    } else {
        choice = reach_probe::probe_reach_choice(src, level, shape);
    }
    let donation = Mutex::new(donation);
    let overlap = MatchGeneratorDriver::strip_for_choice(level, shape, choice) as usize;
    let job_size = job_size_for(src.len() as u64, workers, overlap);
    let n_jobs = src.len().div_ceil(job_size);
    let threads = (workers as usize).min(n_jobs);

    #[cfg(feature = "hash")]
    let mut frame_hash = checksum.then(|| crate::xxh64::Xxh64::new(0));

    let block_overhead = 3 * (src.len() / MAX_BLOCK_SIZE as usize + 1);
    let mut output = Vec::with_capacity(src.len() + block_overhead + 32);
    FrameHeader {
        // The whole input is known here, so pledge it: decoders preallocate
        // exactly and the parallel decode path can partition up front.
        frame_content_size: Some(src.len() as u64),
        single_segment: false,
        content_checksum: checksum,
        dictionary_id: None,
        window_size: Some(window),
    }
    .serialize(&mut output);

    let slots: Vec<Mutex<Option<Vec<u8>>>> = (0..n_jobs).map(|_| Mutex::new(None)).collect();
    let ready = Condvar::new();
    let next_job = AtomicUsize::new(0);
    let poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if poison.lock().unwrap().is_some() {
                        break;
                    }
                    let id = next_job.fetch_add(1, Ordering::Relaxed);
                    if id >= n_jobs {
                        break;
                    }
                    let start = id * job_size;
                    let end = (start + job_size).min(src.len());
                    // The first job starts where the decoder's repeated-offset
                    // history is still the format default [1, 4, 8].
                    let gate = id > 0;
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if id == 0
                            && let Some((mut dstate, prefix)) = donation.lock().unwrap().take()
                        {
                            // The donated state already carries job zero's
                            // reset, strip prefill and parsed span — the
                            // continuation emits from where it stopped.
                            let out = compress_job_blocks_inner(
                                &mut dstate,
                                src,
                                start..end,
                                overlap,
                                end == src.len(),
                                reach_probe::PROBE_SPAN,
                                prefix,
                            );
                            return_slice_state(dstate);
                            return out;
                        }
                        run_job(
                            src,
                            start..end,
                            overlap,
                            end == src.len(),
                            level,
                            gate,
                            shape,
                            choice,
                        )
                    }));
                    match attempt {
                        Ok(bytes) => *slots[id].lock().unwrap() = Some(bytes),
                        Err(payload) => {
                            *poison.lock().unwrap() = Some(payload);
                            // Release the slot so the ordered assembly below can
                            // run to completion before the panic is resumed.
                            *slots[id].lock().unwrap() = Some(Vec::new());
                        },
                    }
                    ready.notify_all();
                }
            });
        }

        // The frame checksum is independent of the job split; hash it while
        // the workers are still busy.
        #[cfg(feature = "hash")]
        if let Some(hash) = frame_hash.as_mut() {
            hash.write(src);
        }

        // Ordered assembly on the calling thread: each job's blocks append as
        // soon as they land.
        for slot in &slots {
            let mut guard = slot.lock().unwrap();
            while guard.is_none() {
                guard = ready.wait(guard).unwrap();
            }
            let bytes = guard.take().unwrap();
            output.extend_from_slice(&bytes);
        }
    });
    if let Some(payload) = poison.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
    #[cfg(feature = "hash")]
    if let Some(hash) = frame_hash.as_mut() {
        output.extend_from_slice(&(hash.finish() as u32).to_le_bytes());
    }
    output
}

/// Deep-offset ramp depth for gated jobs (decode-parallelism experiment):
/// `ZSTDX_MT_RAMP_BYTES` env var, parsed once. Zero (unset) keeps the job
/// parse unconstrained; see `match_generator::RampGate` for the semantics.
static RAMP_TEST_OVERRIDE: AtomicU64 = AtomicU64::new(u64::MAX);

/// Test-only ramp override (env parsing is process-global; tests must not
/// race other tests' encodes through `set_var`). Hidden: not API-stable.
#[doc(hidden)]
pub fn set_mt_ramp_depth_for_tests(depth: u64) {
    RAMP_TEST_OVERRIDE.store(depth, Ordering::Relaxed);
}

fn ramp_depth_from_env() -> u64 {
    static DEPTH: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let over = RAMP_TEST_OVERRIDE.load(Ordering::Relaxed);
    if over != u64::MAX {
        return over;
    }
    *DEPTH.get_or_init(|| {
        std::env::var("ZSTDX_MT_RAMP_BYTES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// Run the reach probe's keep side as job zero's own first span blocks
/// through `state`: the state is job-zero-shaped exactly (reset at the job
/// arming, empty-strip prefill with its table clear and head arming), the
/// emit mirrors `compress_job_blocks`, and the matcher accumulates the
/// probe cost while the blocks become the frame's own output. `head` is the
/// frame's first [`reach_probe::PROBE_SPAN`] bytes. Returns the measured
/// keep cost and the encoded span; the caller takes the verdict (the
/// shrink side measures separately — bulk serially on the calling thread,
/// the stream core on a second pool worker) and disposes of the state (a
/// Shrink verdict's returns to its pool, a Keep's becomes job zero's
/// continuation state).
pub(crate) fn donate_keep_span(
    state: &mut CompressState<MatchGeneratorDriver>,
    head: &[u8],
    level: Level,
    shape: crate::InputShape,
) -> (f64, Vec<u8>) {
    debug_assert_eq!(head.len(), reach_probe::PROBE_SPAN);
    reset_slice_state(
        state,
        level,
        shape,
        reach_probe::ReachChoice::Keep,
        LdmArming::Job,
    );
    state.matcher.prefill_job_strip(&head[..0], 0);
    let block_size = state.matcher.block_size();
    let max_window = state.matcher.window_size() as usize;
    let span = reach_probe::PROBE_SPAN;
    let mut output = Vec::with_capacity(span + 3 * (span / block_size + 1) + 8);
    let mut hasher = SliceChecksum::new(0, false);
    state.matcher.begin_probe_stats();
    let mut cursor = 0usize;
    while cursor < span {
        let block_end = (cursor + block_size).min(span);
        let hist = cursor.saturating_sub(max_window);
        state
            .matcher
            .adopt_window(&head[hist..block_end], hist as u64);
        state.matcher.set_block(cursor as u64, block_end as u64);
        compress_fastest(state, false, &mut output, &mut hasher);
        cursor = block_end;
    }
    let keep = state
        .matcher
        .take_probe_cost()
        .expect("donation began the stats");
    (keep, output)
}

/// [`donate_keep_span`] wrapped for the bulk path: returns the verdict plus the
/// state and encoded prefix for job zero's continuation (a Shrink verdict
/// discards both — the caller returns the state to the pool and job zero
/// parses from its own pooled state).
fn donate_job_zero_prefix(
    src: &[u8],
    level: Level,
    shape: crate::InputShape,
) -> (
    reach_probe::ReachChoice,
    alloc::boxed::Box<CompressState<MatchGeneratorDriver>>,
    Vec<u8>,
) {
    let mut state = take_slice_state(level, shape, reach_probe::ReachChoice::Keep, LdmArming::Job);
    let (keep, output) =
        donate_keep_span(&mut state, &src[..reach_probe::PROBE_SPAN], level, shape);
    let shrink = reach_probe::parse_cost(
        &src[..reach_probe::PROBE_SPAN],
        level,
        shape,
        reach_probe::ReachChoice::Shrink,
        reach_probe::ProbeFeedback::Approx,
    );
    let choice = reach_probe::decide_donated(keep, shrink, src, level, shape);
    (choice, state, output)
}

/// Pooled donation kit for the streaming core: its pool workers are
/// ephemeral (spawned per encoder, joined at drop), so a donation running
/// there inherits no warm tables — a fresh state and probe driver measured
/// 4-5x the warm parse cost (first-touch faults on ~50 MiB of tables, per
/// stream). The kit outlives encoders and hands the donation the same
/// steady-thread warmth the bulk paths get from their thread-local pools;
/// depth one (a frame has one donation, and the retained tables are the
/// price).
#[cfg(feature = "std")]
static DONATION_KIT: std::sync::OnceLock<
    Mutex<(
        Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>,
        Option<alloc::boxed::Box<MatchGeneratorDriver>>,
    )>,
> = std::sync::OnceLock::new();

#[cfg(feature = "std")]
fn donation_kit() -> &'static Mutex<(
    Option<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>,
    Option<alloc::boxed::Box<MatchGeneratorDriver>>,
)> {
    DONATION_KIT.get_or_init(|| Mutex::new((None, None)))
}

/// Take the kit's state (a fresh one when the pool is cold).
#[cfg(feature = "std")]
pub(crate) fn take_donation_state() -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
    donation_kit()
        .lock()
        .unwrap()
        .0
        .take()
        .unwrap_or_else(|| alloc::boxed::Box::new(new_slice_state()))
}

/// Return a donation state to the kit.
#[cfg(feature = "std")]
pub(crate) fn return_donation_state(state: alloc::boxed::Box<CompressState<MatchGeneratorDriver>>) {
    donation_kit().lock().unwrap().0 = Some(state);
}

/// Take the kit's probe driver (a fresh one when the pool is cold).
#[cfg(feature = "std")]
pub(crate) fn take_donation_probe() -> alloc::boxed::Box<MatchGeneratorDriver> {
    donation_kit()
        .lock()
        .unwrap()
        .1
        .take()
        .unwrap_or_else(|| alloc::boxed::Box::new(MatchGeneratorDriver::new_direct()))
}

/// Return a donation probe driver to the kit.
#[cfg(feature = "std")]
pub(crate) fn return_donation_probe(probe: alloc::boxed::Box<MatchGeneratorDriver>) {
    donation_kit().lock().unwrap().1 = Some(probe);
}

/// Compress one job through `state`, resetting it for the job: fresh
/// entropy tables, and the repcode gate unless the job starts the frame
/// (the decoder's repeated-offset history is the format default only there).
/// `shape` is the whole frame's declared shape (length known for bulk,
/// pledge or none for streaming; jobs share it so tables and the header
/// window agree).
pub(crate) fn run_job_with(
    state: &mut CompressState<MatchGeneratorDriver>,
    src: &[u8],
    job: Range<usize>,
    overlap: usize,
    is_last_job: bool,
    level: Level,
    gate: bool,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
) -> Vec<u8> {
    #[cfg(feature = "job_trace")]
    let trace_reset = std::time::Instant::now();
    reset_slice_state(state, level, shape, choice, LdmArming::Job);
    #[cfg(feature = "job_trace")]
    super::job_trace::add_reset(trace_reset);
    if gate {
        state.matcher.gate_repcodes();
        let depth = ramp_depth_from_env();
        if depth > 0 {
            state.matcher.arm_ramp(job.start as u64, depth);
        }
    }
    compress_job_blocks(state, src, job, overlap, is_last_job)
}

/// Compress one job on the calling (worker) thread through the per-thread
/// pooled state, so steady-state jobs reuse their hash table allocation.
/// Shared by the streaming burst driver.
pub(crate) fn run_job(
    src: &[u8],
    job: Range<usize>,
    overlap: usize,
    is_last_job: bool,
    level: Level,
    gate: bool,
    shape: crate::InputShape,
    choice: reach_probe::ReachChoice,
) -> Vec<u8> {
    let mut state = take_slice_state(level, shape, choice, LdmArming::Job);
    let output = run_job_with(
        &mut state,
        src,
        job,
        overlap,
        is_last_job,
        level,
        gate,
        shape,
        choice,
    );
    return_slice_state(state);
    output
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::compress_slice_mt;
    use crate::{Level, decoding::FrameDecoder};

    fn lcg(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..len).map(|_| (rand() & 0xff) as u8).collect()
    }

    /// Text-like data: repeating vocabulary with variation, so matches,
    /// repcodes and entropy coding all engage across job boundaries.
    fn textish(len: usize) -> Vec<u8> {
        let words: Vec<&[u8]> = vec![
            b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ",
            b"lorem ", b"ipsum ", b"dolor ", b"sit ", b"amet ",
        ];
        let mut state = 7u64;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = words[((state >> 33) as usize) % words.len()];
            let take = w.len().min(len - out.len());
            out.extend_from_slice(&w[..take]);
        }
        out
    }

    fn shapes() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("text", textish(5 * 1024 * 1024 + 123)),
            ("zeros", vec![0u8; 4 * 1024 * 1024]),
            ("random", lcg(2 * 1024 * 1024 + 7)),
            ("exact-jobs", textish(4 * 1024 * 1024)),
        ]
    }

    #[test]
    fn mt_frames_decode_identically() {
        for (name, data) in shapes() {
            for workers in [2u32, 4] {
                for checksum in [true, false] {
                    let compressed =
                        compress_slice_mt(&data, Level::Fastest, checksum, workers, None);
                    // our decoder, exact-size output slice
                    let mut out = vec![0u8; data.len()];
                    let mut decoder = FrameDecoder::new();
                    let n = decoder
                        .decode_all(&compressed, &mut out)
                        .unwrap_or_else(|e| panic!("{name}/{workers}/{checksum}: {e}"));
                    assert_eq!(
                        (n, &out[..n]),
                        (data.len(), &data[..]),
                        "{name}/{workers}/{checksum}"
                    );
                    // libzstd decodes the assembled frame too
                    let mut decoded = Vec::new();
                    zstd::stream::copy_decode(compressed.as_slice(), &mut decoded).unwrap();
                    assert_eq!(decoded, data, "{name}/{workers}/{checksum}");
                }
            }
        }
    }

    #[test]
    fn mt_output_is_deterministic() {
        let data = textish(3 * 1024 * 1024);
        let a = compress_slice_mt(&data, Level::Fastest, true, 3, None);
        let b = compress_slice_mt(&data, Level::Fastest, true, 3, None);
        assert_eq!(a, b);
    }

    /// The overlap strip must keep the multithreaded ratio close to the
    /// single-threaded one on data whose repeats fit inside a window.
    #[test]
    fn mt_ratio_stays_close_to_single_thread() {
        let data = textish(6 * 1024 * 1024);
        let st = compress_slice_mt(&data, Level::Fastest, true, 1, None);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4, None);
        assert!(
            mt.len() <= st.len() + st.len() / 50,
            "single {} vs multi {}",
            st.len(),
            mt.len()
        );
    }

    /// The chain levels must ride the job path too: the repcode gate plus a
    /// fresh-table job start interact with the deeper search.
    #[test]
    fn mt_chain_levels_roundtrip() {
        let data = textish(5 * 1024 * 1024);
        for level in [Level::Fast, Level::Balanced] {
            let compressed = compress_slice_mt(&data, level, true, 4, None);
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder.decode_all(&compressed, &mut out).unwrap();
            assert_eq!((n, &out[..n]), (data.len(), &data[..]));
            let st = crate::encoding::compress_slice_to_vec(&data, level);
            assert!(
                compressed.len() <= st.len() + st.len() / 50,
                "mt ratio must stay near single-thread: {} vs {}",
                compressed.len(),
                st.len()
            );
        }
    }

    /// A repeat period that fits the level's window must keep matching
    /// across job borders: the strip is indexed, so the only ratio loss left
    /// is the job-start entropy restarts. Data whose period exceeds the old
    /// un-indexed strip collapsed the ratio by an order of magnitude.
    #[test]
    fn mt_periodic_repeat_matches_across_jobs() {
        let unit = textish(300 * 1024);
        let data: Vec<u8> = (0..24).flat_map(|_| unit.iter().copied()).collect();
        for level in [Level::Fastest, Level::Fast, Level::Balanced] {
            let st = compress_slice_mt(&data, level, true, 1, None);
            let mt = compress_slice_mt(&data, level, true, 4, None);
            assert!(
                mt.len() <= st.len() + st.len() / 50,
                "{level:?}: periodic data must match across jobs, mt {} vs st {}",
                mt.len(),
                st.len()
            );
        }
    }

    /// Inputs below the engagement threshold must fall back to the
    /// single-thread path byte-for-byte.
    #[test]
    fn small_inputs_fall_back_identically() {
        let data = textish(64 * 1024);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4, None);
        let st = crate::encoding::compress_slice_to_vec(&data, Level::Fastest);
        assert_eq!(mt, st);
    }
}
