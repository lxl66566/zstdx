//! Multithreaded slice compression: one frame assembled from independent
//! overlap jobs.
//!
//! The input is split into jobs; each job compresses its byte range with the
//! pooled slice machinery while borrowing an `overlap`-sized strip of the
//! preceding job as match history (`adopt_window`), so the assembled output
//! stays a single regular zstd frame. Two invariants make the jobs
//! independent of each other (mirroring libzstd's zstdmt):
//! - every job starts from reset entropy tables, so its first block is fully
//!   self-describing (no Repeat modes across a job boundary);
//! - every job except the first gates repcode references until three
//!   literal-offset sequences have rewritten the repeated-offset history
//!   (see [`MatchGeneratorDriver::gate_repcodes`]): the decoder's history at
//!   a job boundary is unknown, but each literal offset shifts it down one
//!   slot, so after three of them the matcher and decoder agree again.
//!
//! The frame checksum is hashed over the whole input on the calling thread
//! while the jobs run, and the ordered assembly appends each job's blocks as
//! soon as they land, overlapping the final copy with the late jobs.

use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use super::frame_compressor::{compress_job_blocks, return_slice_state, take_slice_state};
use super::frame_header::FrameHeader;
use super::match_generator::MatchGeneratorDriver;
use crate::common::MAX_BLOCK_SIZE;
use crate::Level;

/// Below this size the thread spawn and the ordered assembly cost more than
/// the parallelism saves.
const MIN_MT_INPUT: usize = 2 * 1024 * 1024;
/// Job size floor: smaller jobs multiply the overlap duplication without
/// improving load balance.
const MIN_JOB_SIZE: usize = 1024 * 1024;

/// Compress `src` into one frame using up to `workers` threads.
///
/// Falls back to the single-threaded slice path (byte-identical to
/// [`super::compress_slice_to_vec` modulo the checksum flag`) when the input
/// is too small, only one worker is requested, the level stores raw blocks,
/// or the process has a single usable core.
pub fn compress_slice_mt(src: &[u8], level: Level, checksum: bool, workers: u32) -> Vec<u8> {
    let checksum = checksum && cfg!(feature = "hash");
    if workers < 2
        || src.len() < MIN_MT_INPUT
        || level == Level::Uncompressed
        || std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
    {
        return super::compress_slice_opts(src, level, checksum);
    }

    // Twice as many jobs as workers keeps the tail balanced; the floor keeps
    // the overlap duplication negligible, and scales with the level's
    // overlap so deep-search levels don't pay it per job.
    let window = MatchGeneratorDriver::window_for_level(level);
    // libzstd's overlap ladder adapted: window/4 for the chain levels
    // (their deeper searches reach further back) and window/8 for the fast
    // hash probe — matches reaching beyond the strip simply do not happen,
    // at a ratio cost bounded by the strip size.
    let overlap = match level {
        Level::Fastest => window as usize / 8,
        _ => window as usize / 4,
    };
    let job_size = src
        .len()
        .div_ceil(workers as usize * 2)
        .max(MIN_JOB_SIZE.max(overlap * 4));
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
        window_size: Some(MatchGeneratorDriver::window_for_level(level)),
    }
    .serialize(&mut output);

    let slots: Vec<Mutex<Option<Vec<u8>>>> = (0..n_jobs).map(|_| Mutex::new(None)).collect();
    let ready = Condvar::new();
    let next_job = AtomicUsize::new(0);
    let poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
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
                    run_job(src, start..end, overlap, end == src.len(), level, gate)
                }));
                match attempt {
                    Ok(bytes) => *slots[id].lock().unwrap() = Some(bytes),
                    Err(payload) => {
                        *poison.lock().unwrap() = Some(payload);
                        // Release the slot so the ordered assembly below can
                        // run to completion before the panic is resumed.
                        *slots[id].lock().unwrap() = Some(Vec::new());
                    }
                }
                ready.notify_all();
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

/// Compress one job on the calling (worker) thread through the per-thread
/// pooled state, so steady-state jobs reuse their hash table allocation.
fn run_job(
    src: &[u8],
    job: Range<usize>,
    overlap: usize,
    is_last_job: bool,
    level: Level,
    gate: bool,
) -> Vec<u8> {
    let mut state = take_slice_state(level);
    if gate {
        state.matcher.gate_repcodes();
    }
    let output = compress_job_blocks(&mut state, src, job, overlap, is_last_job);
    return_slice_state(state);
    output
}

#[cfg(test)]
mod tests {
    use super::compress_slice_mt;
    use crate::decoding::FrameDecoder;
    use crate::Level;
    use alloc::vec;
    use alloc::vec::Vec;

    fn lcg(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..len).map(|_| (rand() & 0xFF) as u8).collect()
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
                    let compressed = compress_slice_mt(&data, Level::Fastest, checksum, workers);
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
        let a = compress_slice_mt(&data, Level::Fastest, true, 3);
        let b = compress_slice_mt(&data, Level::Fastest, true, 3);
        assert_eq!(a, b);
    }

    /// The overlap strip must keep the multithreaded ratio close to the
    /// single-threaded one on data whose repeats fit inside a window.
    #[test]
    fn mt_ratio_stays_close_to_single_thread() {
        let data = textish(6 * 1024 * 1024);
        let st = compress_slice_mt(&data, Level::Fastest, true, 1);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4);
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
        for level in [crate::Level::Fast, crate::Level::Balanced] {
            let compressed = compress_slice_mt(&data, level, true, 4);
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

    /// Inputs below the engagement threshold must fall back to the
    /// single-thread path byte-for-byte.
    #[test]
    fn small_inputs_fall_back_identically() {
        let data = textish(64 * 1024);
        let mt = compress_slice_mt(&data, Level::Fastest, true, 4);
        let st = crate::encoding::compress_slice_to_vec(&data, Level::Fastest);
        assert_eq!(mt, st);
    }
}
