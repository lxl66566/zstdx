//! Multithreaded streaming frame core: the burst-shaped sibling of the
//! single-threaded [`super::encoder_core`] core.
//!
//! Input accumulates in one contiguous buffer — a strip of already-encoded
//! history followed by the bytes not yet encoded — and once enough
//! job-sized slices are pending, a burst encodes them in parallel through
//! the same pooled-job machinery as the bulk mt path (see
//! [`crate::encoding::mt`]), appending the assembled blocks to the output in
//! order. Jobs are cut on absolute job-size boundaries, so the frame bytes
//! depend only on the input and not on how it was written; a flush is the
//! one exception, re-gridding early because making data visible early is
//! its purpose. With a pledged size the job size follows the bulk formula,
//! so a stream written exactly to its pledge is byte-identical to the bulk
//! mt output; without a pledge the grid starts at the size floor and grows
//! toward the bulk formula as data accumulates.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use super::encoder_core::StreamChecksum;
use crate::{
    EncoderOptions, Level,
    blocks::block::BlockType,
    encoding::{
        block_header::BlockHeader,
        frame_compressor::{BlockChecksum as _, CompressState, FrameHasher, new_slice_state},
        frame_header::FrameHeader,
        match_generator::MatchGeneratorDriver,
        mt::{MIN_JOB_SIZE, job_size_for, run_job_with},
    },
};

/// Job boundary schedule. Both variants cut jobs on absolute stream offsets
/// alone, so without a flush the frame bytes never depend on how the input
/// was written.
enum JobGrid {
    /// Pledged stream: one fixed job size from the shared bulk formula over
    /// the pledge, so an exactly-pledged stream is byte-identical to the
    /// bulk mt output.
    Fixed(usize),
    /// Unpledged stream: the job starting at absolute offset `o` spans
    /// `job_size_for(4o, workers, overlap)` — the bulk formula against a
    /// geometric estimate of the final size (a stream that fed `o` bytes
    /// feeds a few times `o` more). Job sizes grow along the stream
    /// (~1 + 2/workers per job once past the floor), so long streams
    /// converge toward the bulk job count instead of paying a per-boundary
    /// entropy restart at the floor forever, while short streams keep the
    /// floor's parallelism and first-output latency.
    Growing,
}

pub(crate) struct MtEncoderCore {
    level: Level,
    checksum: bool,
    workers: u32,
    /// Absolute job schedule: job boundaries derive from absolute stream
    /// offsets alone (see [`JobGrid`]), so the frame bytes never depend on
    /// how the input was written. A flush re-grids by advancing job_start
    /// to the current end.
    job_start: u64,
    grid: JobGrid,
    /// History strip each job borrows (and fully indexes) as match window:
    /// the level's whole window, like the bulk mt path.
    overlap: usize,
    /// Jobs buffered before a burst fires: at least one full round of
    /// workers, amortizing the thread spawn.
    burst_jobs: usize,
    pledged: Option<u64>,
    hasher: StreamChecksum,
    header: Vec<u8>,
    /// [strip of already-encoded history for the next job's matches] followed
    /// by the bytes not yet encoded. `buf_base` is buf[0]'s absolute stream
    /// offset.
    buf: Vec<u8>,
    buf_base: u64,
    /// Total bytes fed.
    pos: u64,
    /// Checksum absorbed up to this absolute offset.
    hashed_end: u64,
    output: Vec<u8>,
    header_emitted: bool,
    finished: bool,
    /// Idle worker states owned by this encoder: burst threads are fresh
    /// every time (thread::scope), so the thread-local slice pool never
    /// carries anything across bursts — this one does, sparing every burst
    /// the hash-table allocations and their first-touch page faults.
    pool: Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
}

impl MtEncoderCore {
    pub(crate) fn new(options: &EncoderOptions) -> Self {
        let checksum = options.checksum && cfg!(feature = "hash");
        let window = MatchGeneratorDriver::window_for_level(options.level);
        let overlap = window as usize;
        // A pledge sizes the grid like the bulk path (byte-identical output
        // when the input matches the pledge); an open-ended stream grows
        // its jobs along the stream (see JobGrid).
        let grid = match options.pledged_size {
            Some(n) => JobGrid::Fixed(job_size_for(n, options.workers, overlap)),
            None => JobGrid::Growing,
        };
        let initial_job = match grid {
            JobGrid::Fixed(size) => size,
            JobGrid::Growing => MIN_JOB_SIZE.max(overlap),
        };
        let header = FrameHeader {
            frame_content_size: options.pledged_size,
            single_segment: false,
            content_checksum: checksum,
            dictionary_id: None,
            window_size: Some(window),
        };
        let mut serialized = Vec::with_capacity(18);
        header.serialize(&mut serialized);
        Self {
            level: options.level,
            checksum,
            workers: options.workers,
            job_start: 0,
            grid,
            overlap,
            burst_jobs: (options.workers as usize).max(2),
            pledged: options.pledged_size,
            hasher: if checksum {
                StreamChecksum::On(FrameHasher::new())
            } else {
                StreamChecksum::Off
            },
            header: serialized,
            buf: Vec::new(),
            buf_base: 0,
            pos: 0,
            hashed_end: 0,
            output: Vec::with_capacity(initial_job + 64),
            header_emitted: false,
            finished: false,
            pool: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn write(&mut self, data: &[u8]) {
        debug_assert!(!self.finished);
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        self.pos += data.len() as u64;
        let (jobs, hi) = self.pending_jobs();
        if jobs >= self.burst_jobs {
            self.encode_jobs(hi, false);
        }
    }

    /// Close the frame: the pending jobs (or an empty block) become the last
    /// block and the checksum, if enabled, is appended.
    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        if self.pos > self.job_start {
            self.encode_jobs(self.pos, true);
        } else {
            // Everything fed is already encoded (or nothing was fed): the
            // frame still needs a last block, and an empty raw one is the
            // shape the single-threaded streaming path emits.
            self.emit_header();
            BlockHeader {
                last_block: true,
                block_type: BlockType::Raw,
                block_size: 0,
            }
            .serialize(&mut self.output);
        }
        debug_assert_eq!(self.hashed_end, self.pos);
        if self.checksum {
            let checksum = self.hasher.finish32();
            self.output.extend_from_slice(&checksum.to_le_bytes());
        }
        self.finished = true;
    }

    /// Emit the pending bytes early as non-last jobs. A no-op when nothing
    /// is pending. The short trailing job re-grids the job boundaries, so
    /// the output of a flushed stream depends on the flush points (making
    /// data visible early is what a flush is for).
    pub(crate) fn flush_block(&mut self) {
        debug_assert!(!self.finished);
        if self.pos > self.job_start {
            self.encode_jobs(self.pos, false);
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(crate) fn has_output(&self) -> bool {
        !self.output.is_empty()
    }

    /// Hand the encoded bytes to `w`, keeping the output buffer's allocation.
    pub(crate) fn write_output_to(
        &mut self,
        w: &mut impl crate::io::Write,
    ) -> Result<(), crate::io::Error> {
        let res = w.write_all(&self.output);
        self.output.clear();
        res
    }

    /// Serve encoded bytes without deallocating the output buffer.
    pub(crate) fn split_output(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.output.len());
        buf[..n].copy_from_slice(&self.output[..n]);
        self.output.drain(..n);
        n
    }

    /// End offset of the job starting at absolute offset `start`.
    fn job_end(&self, start: u64) -> u64 {
        let size = match self.grid {
            JobGrid::Fixed(size) => size as u64,
            JobGrid::Growing => {
                job_size_for(start.saturating_mul(4), self.workers, self.overlap) as u64
            },
        };
        start + size
    }

    /// Complete jobs on the current schedule, and the boundary after the
    /// last one. A pledged grid holds back the pledged final job for
    /// finish() (an exact pledge then emits it with the last-block flag,
    /// like the bulk path does). The hold-back lapses once the input
    /// outgrows the pledge — the header is already wrong at that point.
    fn pending_jobs(&self) -> (usize, u64) {
        debug_assert!(self.pos >= self.job_start);
        match self.grid {
            JobGrid::Fixed(size) => {
                let size = size as u64;
                let complete = ((self.pos - self.job_start) / size) as usize;
                let jobs = match self.pledged {
                    Some(n) if self.pos <= n => {
                        let pledged_jobs = (n - self.job_start).div_ceil(size);
                        complete.min(pledged_jobs.saturating_sub(1) as usize)
                    },
                    _ => complete,
                };
                (jobs, self.job_start + jobs as u64 * size)
            },
            JobGrid::Growing => {
                // The boundaries are a function of absolute offset alone, so
                // walking them here cannot depend on the write chunking.
                let mut end = self.job_start;
                let mut jobs = 0;
                loop {
                    let next = self.job_end(end);
                    if next > self.pos {
                        break;
                    }
                    end = next;
                    jobs += 1;
                }
                (jobs, end)
            },
        }
    }

    /// Encode the jobs covering [job_start, hi) and advance the grid: the job
    /// ending at `hi` carries the frame's last-block flag when
    /// `last_frame_block` is set (only finish passes true). `hi` must not
    /// exceed `pos`; when it lands mid-job (flush, or the finish tail) the
    /// last job is short.
    fn encode_jobs(&mut self, hi: u64, last_frame_block: bool) {
        debug_assert!(self.job_start < hi && hi <= self.pos);
        self.emit_header();
        // Job boundaries between job_start and hi, on the absolute schedule.
        let mut bounds = Vec::with_capacity(self.burst_jobs + 2);
        bounds.push(self.job_start);
        while *bounds.last().unwrap() < hi {
            let next = self.job_end(*bounds.last().unwrap()).min(hi);
            bounds.push(next);
        }
        let n_jobs = bounds.len() - 1;
        // The shared job view starts at the next job's strip (the previous
        // job's tail), which is exactly what the buffer retained.
        let strip_lo = self.job_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= strip_lo);
        let first = (self.job_start - strip_lo) as usize;

        if n_jobs == 1 {
            // One short tail job (small inputs, a flush, or a finish without
            // a burst behind it): inline on the calling thread, no pool
            // spun up.
            self.hash_to(hi);
            let last_len = (bounds[1] - bounds[0]) as usize;
            let mut state = Self::take_pooled_state(&self.pool);
            let bytes = run_job_with(
                &mut state,
                &self.buf[..(hi - self.buf_base) as usize],
                first..first + last_len,
                self.overlap,
                last_frame_block,
                self.level,
                self.job_start > 0,
            );
            self.pool.lock().unwrap().push(state);
            self.output.extend_from_slice(&bytes);
        } else {
            let src = &self.buf[..(hi - self.buf_base) as usize];
            let threads = (self.workers as usize).min(n_jobs);
            let slots: Vec<Mutex<Option<Vec<u8>>>> =
                (0..n_jobs).map(|_| Mutex::new(None)).collect();
            let ready = Condvar::new();
            let next_job = AtomicUsize::new(0);
            let poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>> =
                Mutex::new(None);
            let overlap = self.overlap;
            let level = self.level;
            let job_start = self.job_start;
            let bounds = &bounds[..];

            // Disjoint field borrows: the workers share `src` and the state
            // pool while the calling thread runs the checksum absorb and the
            // ordered assembly.
            let output = &mut self.output;
            let hasher = &mut self.hasher;
            let pool = &self.pool;
            let unhashed = &self.buf
                [(self.hashed_end - self.buf_base) as usize..(hi - self.buf_base) as usize];

            std::thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        let mut state = Self::take_pooled_state(pool);
                        loop {
                            if poison.lock().unwrap().is_some() {
                                break;
                            }
                            let id = next_job.fetch_add(1, Ordering::Relaxed);
                            if id >= n_jobs {
                                break;
                            }
                            let start = first + (bounds[id] - job_start) as usize;
                            let end = first + (bounds[id + 1] - job_start) as usize;
                            // Every job except the frame's first starts where the
                            // decoder's repcode history is unknown (see the bulk
                            // mt path); a flush-rebased grid starts mid-frame
                            // too.
                            let gate = bounds[id] > 0;
                            let attempt =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    run_job_with(
                                        &mut state,
                                        src,
                                        start..end,
                                        overlap,
                                        last_frame_block && id + 1 == n_jobs,
                                        level,
                                        gate,
                                    )
                                }));
                            match attempt {
                                Ok(bytes) => *slots[id].lock().unwrap() = Some(bytes),
                                Err(payload) => {
                                    *poison.lock().unwrap() = Some(payload);
                                    // Release the slot so the ordered assembly
                                    // below can run to completion before the
                                    // panic is resumed. The state may be
                                    // mid-compress garbage, so it is dropped
                                    // rather than pooled (the early return
                                    // skips the push below).
                                    *slots[id].lock().unwrap() = Some(Vec::new());
                                    ready.notify_all();
                                    return;
                                },
                            }
                            ready.notify_all();
                        }
                        pool.lock().unwrap().push(state);
                    });
                }
                hasher.hash_tail(unhashed);
                // Ordered assembly on the calling thread: each job's blocks
                // append as soon as they land.
                for slot in &slots {
                    let mut guard = slot.lock().unwrap();
                    while guard.is_none() {
                        guard = ready.wait(guard).unwrap();
                    }
                    output.extend_from_slice(&guard.take().unwrap());
                }
            });
            self.hashed_end = hi;
            if let Some(payload) = poison.into_inner().unwrap() {
                std::panic::resume_unwind(payload);
            }
        }
        self.job_start = hi;
        self.trim_buf();
    }

    /// Retain exactly the strip the next job will prefill from, dropping
    /// everything older: the pending bytes slide to the buffer head, no
    /// separate window copy.
    fn trim_buf(&mut self) {
        let keep = self.job_start.saturating_sub(self.overlap as u64);
        debug_assert!(self.buf_base <= keep);
        let drop = (keep - self.buf_base) as usize;
        if drop > 0 {
            self.buf.drain(..drop);
            self.buf_base = keep;
        }
    }

    /// Absorb [hashed_end, end) into the frame checksum.
    fn hash_to(&mut self, end: u64) {
        debug_assert!(self.hashed_end <= end && end <= self.pos);
        let range = (self.hashed_end - self.buf_base) as usize..(end - self.buf_base) as usize;
        self.hasher.hash_tail(&self.buf[range]);
        self.hashed_end = end;
    }

    /// Take a worker state from the pool, or build a fresh one.
    fn take_pooled_state(
        pool: &Mutex<Vec<alloc::boxed::Box<CompressState<MatchGeneratorDriver>>>>,
    ) -> alloc::boxed::Box<CompressState<MatchGeneratorDriver>> {
        pool.lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| alloc::boxed::Box::new(new_slice_state()))
    }

    fn emit_header(&mut self) {
        if !self.header_emitted {
            self.output.extend_from_slice(&self.header);
            self.header_emitted = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::decoding::FrameDecoder;

    fn textish(len: usize) -> Vec<u8> {
        let words: [&[u8]; 13] = [
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

    /// The unpledged schedule must grow job sizes along the stream while a
    /// pledged one keeps the fixed bulk grid.
    #[test]
    fn growing_grid_scales_jobs_with_stream() {
        let unpledged = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
        let floor = unpledged.job_end(0);
        assert_eq!(floor, MIN_JOB_SIZE.max(unpledged.overlap) as u64);
        // The geometric estimate quadruples the fed size, so the job at
        // 64 MiB is sized for a 256 MiB input: 256 MiB / (4 workers * 2).
        assert_eq!(
            unpledged.job_end(64 * 1024 * 1024) - 64 * 1024 * 1024,
            32 * 1024 * 1024
        );

        let pledged = MtEncoderCore::new(
            &EncoderOptions::new(Level::Fastest)
                .workers(4)
                .pledged_size(Some(64 * 1024 * 1024)),
        );
        assert_eq!(
            pledged.job_end(0),
            pledged.job_end(32 * 1024 * 1024) - 32 * 1024 * 1024
        );
    }

    /// A long unpledged stream crosses many growth points and burst/tail
    /// shapes; the frame must still decode to the fed bytes.
    #[test]
    fn growing_grid_roundtrip() {
        let data = textish(20 * 1024 * 1024 + 31);
        for chunk in [1024 * 1024, 300 * 1024, usize::MAX] {
            let mut core = MtEncoderCore::new(&EncoderOptions::new(Level::Fastest).workers(4));
            for piece in data.chunks(chunk) {
                core.write(piece);
            }
            core.finish();
            let mut out = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            let n = decoder.decode_all(&core.output, &mut out).unwrap();
            assert_eq!((n, &out[..n]), (data.len(), &data[..]), "chunk {chunk}");
        }
    }
}
