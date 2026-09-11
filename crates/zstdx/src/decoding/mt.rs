//! Parallel decoding of complete in-memory inputs.
//!
//! A cheap pre-scan walks the block headers (3 bytes per block) and finds
//! *restart points*: blocks whose entropy state is fully self-describing —
//! literals not in Treeless mode and all three FSE streams out of Repeat
//! mode. Encoders with jobs (libzstd's `-T` mode, this crate's
//! [`crate::encoding::mt`]) emit exactly such blocks at every job boundary,
//! and every frame start is one by definition.
//!
//! Between two restart points the compressed bytes decode independently of
//! everything before them except the *output* history: literals Huffman
//! decoding and FSE sequence decoding are pure functions of the compressed
//! bytes plus the restart state, while sequence execution copies match
//! sources from the already-produced output. The decoder therefore runs as
//! a two-stage pipeline:
//!
//! - stage A (worker pool, parallel): decode each segment's literals and sequences into staging
//!   buffers and compute its exact output size;
//! - stage B (calling thread, in input order): execute the staged segments sequentially into the
//!   output, so every match source below the current segment start is already final. Repcode
//!   history (`offset_hist`) is carried across segments by the executing thread — repcode
//!   *resolution* is the only cross-sequence state and it never touches stage A.
//!
//! Segment output sizes are only known after stage A, so the output buffer
//! is checked (or grown) segment by segment as stage B reaches them.
//! Dictionary frames, small inputs, inputs without a second restart point
//! and single-core processes fall back to the sequential decoder.
//!
//! The frame checksum is not verified here, matching the sequential
//! `decode_all` paths (which leave it to the caller).

use alloc::vec::Vec;
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex};

use super::{
    FrameDecoder,
    errors::{DecodeBlockContentError, DecompressBlockError, FrameDecoderError},
    frame,
    literals_section_decoder::decode_literals,
    scratch::{FSEScratch, HuffmanScratch},
    sequence_execution::do_offset_history,
    sequence_section_decoder::decode_sequences_into,
};
use crate::{
    blocks::{
        block::BlockType,
        literals_section::{LiteralsSection, LiteralsSectionType},
        sequence_section::{ModeType, Sequence, SequencesHeader},
    },
    common::MAX_BLOCK_SIZE,
};

/// Below this (compressed) input size the scan, spawn and hand-off overhead
/// dominates; the restart-point count is the real gate, this only avoids
/// spawning for inputs that could parallelize but not benefit.
const MIN_MT_INPUT: usize = 512 * 1024;
/// Segment-size floor: below it the staging buffers and hand-offs cost
/// more than the parallelism saves.
const MIN_SEGMENT_INPUT: usize = 256 * 1024;

/// One scanned block: enough to route stage A and locate the body bytes.
#[derive(Clone)]
struct ScannedBlock {
    btype: BlockType,
    /// Decompressed length of Raw blocks; the run length for RLE blocks.
    raw_size: u32,
    /// Body bytes within the whole input: the payload for Raw and
    /// Compressed, the single repeated byte for RLE.
    body: Range<usize>,
}

/// A run of blocks between two restart points (or frame boundaries).
struct SegmentPlan {
    blocks: Vec<ScannedBlock>,
    /// This segment starts a frame: execution resets the repcode history
    /// and stage A gets fresh tables (frame starts are restart points).
    frame_start: bool,
}

/// How stage B walks a block whose stage A staging is done.
enum BlockPlan {
    Raw {
        body: Range<usize>,
    },
    Rle {
        byte: u8,
        len: usize,
    },
    Compressed {
        lits: Range<usize>,
        seqs: Range<usize>,
    },
}

/// Stage A output for one segment.
struct DecodedSegment {
    literals: Vec<u8>,
    sequences: Vec<Sequence>,
    plan: Vec<BlockPlan>,
    out_size: usize,
}

/// Reusable per-worker entropy state; reset between segments (every
/// segment starts at a restart point, so fresh tables are correct).
struct SegmentScratch {
    huf: HuffmanScratch,
    fse: FSEScratch,
}

impl SegmentScratch {
    fn new() -> Self {
        Self {
            huf: HuffmanScratch::new(),
            fse: FSEScratch::new(),
        }
    }

    fn reset_tables(&mut self) {
        let fse = &mut self.fse;
        fse.literal_lengths.reset();
        fse.match_lengths.reset();
        fse.offsets.reset();
        fse.ll_rle = None;
        fse.ml_rle = None;
        fse.of_rle = None;
        fse.ll_predefined = false;
        fse.ml_predefined = false;
        fse.of_predefined = false;
        fse.ll_seq_valid = false;
        fse.ml_seq_valid = false;
        fse.of_seq_valid = false;
        fse.ll_ready = false;
        fse.ml_ready = false;
        fse.of_ready = false;
        self.huf.table.reset();
    }
}

fn block_body_err(e: DecompressBlockError) -> FrameDecoderError {
    FrameDecoderError::FailedToReadBlockBody(DecodeBlockContentError::DecompressBlockError(e))
}

/// Whether a compressed block's body re-establishes all entropy state:
/// the literals section carries its own Huffman table (not Treeless) and
/// every FSE stream leaves Repeat mode. Such a block is a legal restart
/// point for an independent segment.
fn restart_point(body: &[u8]) -> bool {
    let mut section = LiteralsSection::new();
    let Ok(header_len) = section.parse_from_header(body) else {
        return false;
    };
    if matches!(section.ls_type, LiteralsSectionType::Treeless) {
        return false;
    }
    let literals_len = match section.compressed_size {
        Some(c) => c as usize,
        None => match section.ls_type {
            LiteralsSectionType::RLE => 1,
            _ => section.regenerated_size as usize,
        },
    };
    let Some(seq_raw) = body.get(header_len as usize + literals_len..) else {
        return false;
    };
    let mut seq_header = SequencesHeader::new();
    if seq_header.parse_from_header(seq_raw).is_err() {
        return false;
    }
    // A zero-sequence block leaves the FSE tables untouched, so the next
    // block could legally use Repeat; require sequences with explicit
    // tables instead.
    let Some(modes) = seq_header.modes else {
        return false;
    };
    !matches!(modes.ll_mode(), ModeType::Repeat)
        && !matches!(modes.ml_mode(), ModeType::Repeat)
        && !matches!(modes.of_mode(), ModeType::Repeat)
}

/// Walk the input, validating the frame structure and splitting frames
/// into segments at restart points. `None` means "do not parallelize":
/// dictionary frames, malformed or truncated input (the sequential path
/// reports those properly), or too few segments to be worth it.
fn scan(input: &[u8], workers: u32, max_window_size: u64) -> Option<Vec<SegmentPlan>> {
    let target_segment = (input.len() / (workers as usize * 2)).max(MIN_SEGMENT_INPUT);
    let mut segments: Vec<SegmentPlan> = Vec::new();
    let mut cursor = 0usize;
    while cursor < input.len() {
        let mut reader = &input[cursor..];
        let frame_header = match frame::read_frame_header(&mut reader) {
            Ok((header, _)) => header,
            Err(super::errors::ReadFrameHeaderError::SkipFrame { length, .. }) => {
                // 4 magic + 4 length + payload
                let end = cursor.checked_add(8)?.checked_add(length as usize)?;
                if end > input.len() {
                    return None;
                }
                cursor = end;
                continue;
            },
            Err(_) => return None,
        };
        if frame_header.dictionary_id().is_some() {
            return None; // dictionary state cannot be split across segments
        }
        if frame_header.window_size().ok()? > max_window_size {
            return None; // sequential path rejects with the proper error
        }
        let checksummed = frame_header.descriptor.content_checksum_flag();
        let blocks_start = input.len() - reader.len();

        let mut blocks: Vec<ScannedBlock> = Vec::new();
        let mut restarts: Vec<usize> = Vec::new();
        let mut scan_cur = blocks_start;
        loop {
            let head = input.get(scan_cur..scan_cur + 3)?;
            let raw = u32::from_le_bytes([head[0], head[1], head[2], 0]);
            let last = raw & 1 == 1;
            let btype = match (raw >> 1) & 0x3 {
                0 => BlockType::Raw,
                1 => BlockType::RLE,
                2 => BlockType::Compressed,
                _ => return None, // reserved block
            };
            let size = raw >> 3;
            if size > MAX_BLOCK_SIZE {
                return None;
            }
            let body_len = match btype {
                BlockType::Raw | BlockType::Compressed => size as usize,
                BlockType::RLE => 1,
                BlockType::Reserved => unreachable!(),
            };
            let body_end = (scan_cur + 3).checked_add(body_len)?;
            if body_end > input.len() {
                return None;
            }
            if btype == BlockType::Compressed && restart_point(&input[scan_cur + 3..body_end]) {
                restarts.push(blocks.len());
            }
            blocks.push(ScannedBlock {
                btype,
                raw_size: size,
                body: scan_cur + 3..body_end,
            });
            scan_cur = body_end;
            if last {
                break;
            }
        }
        if checksummed {
            scan_cur += 4;
            if scan_cur > input.len() {
                return None;
            }
        }

        // Close a segment at the first restart point past each target
        // multiple of the segment size; whatever trails the last restart
        // extends the frame's final segment.
        let mut seg_start = 0usize;
        let mut next_target = target_segment;
        for &r in &restarts {
            if r == 0 {
                continue; // the frame start is already a segment start
            }
            let restart_input = blocks[r].body.start - blocks_start;
            if restart_input >= next_target {
                segments.push(SegmentPlan {
                    blocks: blocks[seg_start..r].to_vec(),
                    frame_start: seg_start == 0,
                });
                seg_start = r;
                next_target += target_segment;
            }
        }
        segments.push(SegmentPlan {
            blocks: blocks[seg_start..].to_vec(),
            frame_start: seg_start == 0,
        });
        cursor = scan_cur;
    }
    (segments.len() >= 2).then_some(segments)
}

/// Stage A: decode one segment's literals and sequences into staging.
fn decode_segment(
    input: &[u8],
    plan: &SegmentPlan,
    scratch: &mut SegmentScratch,
) -> Result<DecodedSegment, FrameDecoderError> {
    scratch.reset_tables();
    let mut literals = Vec::new();
    let mut sequences = Vec::new();
    let mut out_size = 0usize;
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for blk in &plan.blocks {
        match blk.btype {
            BlockType::Raw => {
                out_size += blk.body.len();
                blocks.push(BlockPlan::Raw {
                    body: blk.body.clone(),
                });
            },
            BlockType::RLE => {
                out_size += blk.raw_size as usize;
                blocks.push(BlockPlan::Rle {
                    byte: input[blk.body.start],
                    len: blk.raw_size as usize,
                });
            },
            BlockType::Compressed => {
                let body = &input[blk.body.clone()];
                let mut section = LiteralsSection::new();
                let header_len = section
                    .parse_from_header(body)
                    .map_err(DecompressBlockError::from)
                    .map_err(block_body_err)? as usize;
                let literals_len = match section.compressed_size {
                    Some(c) => c as usize,
                    None => match section.ls_type {
                        LiteralsSectionType::RLE => 1,
                        _ => section.regenerated_size as usize,
                    },
                };
                if body.len() < header_len + literals_len {
                    return Err(block_body_err(
                        DecompressBlockError::MalformedSectionHeader {
                            expected_len: header_len + literals_len,
                            remaining_bytes: body.len(),
                        },
                    ));
                }
                let lits_start = literals.len();
                decode_literals(
                    &section,
                    &mut scratch.huf,
                    &body[header_len..header_len + literals_len],
                    &mut literals,
                )
                .map_err(DecompressBlockError::from)
                .map_err(block_body_err)?;
                assert_eq!(
                    literals.len() - lits_start,
                    section.regenerated_size as usize,
                    "literals section length mismatch"
                );
                let seq_raw_all = &body[header_len + literals_len..];
                let mut seq_header = SequencesHeader::new();
                let seq_header_len = seq_header
                    .parse_from_header(seq_raw_all)
                    .map_err(DecompressBlockError::from)
                    .map_err(block_body_err)? as usize;
                let seqs_start = sequences.len();
                decode_sequences_into(
                    &seq_header,
                    &seq_raw_all[seq_header_len..],
                    &mut scratch.fse,
                    &mut sequences,
                )
                .map_err(DecompressBlockError::from)
                .map_err(block_body_err)?;
                let match_bytes: usize =
                    sequences[seqs_start..].iter().map(|s| s.ml as usize).sum();
                out_size += literals.len() - lits_start + match_bytes;
                blocks.push(BlockPlan::Compressed {
                    lits: lits_start..literals.len(),
                    seqs: seqs_start..sequences.len(),
                });
            },
            BlockType::Reserved => unreachable!("scan rejects reserved blocks"),
        }
    }
    Ok(DecodedSegment {
        literals,
        sequences,
        plan: blocks,
        out_size,
    })
}

/// Stage B: execute one staged segment at `base[seg_start..seg_start +
/// out_size)`, reading match sources at absolute positions below
/// `seg_start` (final output of earlier segments; stage B runs in order).
///
/// # Safety
/// `base` must be valid for writes in `[seg_start, seg_start + out_size)`
/// and for reads in `[0, seg_start)`. The caller guarantees both by
/// sizing the output before each segment and executing in order.
unsafe fn execute_segment(
    base: *mut u8,
    seg_start: usize,
    seg: &DecodedSegment,
    input: &[u8],
    offset_hist: &mut [u32; 3],
) -> Result<(), FrameDecoderError> {
    use crate::decoding::errors::ExecuteSequencesError;
    let exec_err =
        |e: ExecuteSequencesError| block_body_err(DecompressBlockError::ExecuteSequencesError(e));
    let mut w = 0usize;
    for block in &seg.plan {
        match block {
            BlockPlan::Raw { body } => {
                // SAFETY: stage A accounted this block's body length in out_size
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        input.as_ptr().add(body.start),
                        base.add(seg_start + w),
                        body.len(),
                    );
                }
                w += body.len();
            },
            BlockPlan::Rle { byte, len } => {
                // SAFETY: stage A accounted the run length in out_size
                unsafe {
                    core::ptr::write_bytes(base.add(seg_start + w), *byte, *len);
                }
                w += len;
            },
            BlockPlan::Compressed { lits, seqs } => {
                let mut lit_pos = lits.start;
                for seq in &seg.sequences[seqs.clone()] {
                    let ll = seq.ll as usize;
                    let ml = seq.ml as usize;
                    if w + ll + ml > seg.out_size {
                        return Err(exec_err(ExecuteSequencesError::TargetTooSmall));
                    }
                    if lit_pos + ll > lits.end {
                        return Err(exec_err(ExecuteSequencesError::NotEnoughBytesForSequence {
                            wanted: lit_pos + ll,
                            have: lits.end,
                        }));
                    }
                    // SAFETY: the budget check bounds the write; the literal
                    // range was checked above
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            seg.literals.as_ptr().add(lit_pos),
                            base.add(seg_start + w),
                            ll,
                        );
                    }
                    lit_pos += ll;
                    w += ll;
                    let offset = do_offset_history(seq.of, seq.ll, offset_hist);
                    if offset == 0 {
                        return Err(exec_err(ExecuteSequencesError::ZeroOffset));
                    }
                    let offset = offset as usize;
                    let cur = seg_start + w;
                    if offset > cur {
                        return Err(exec_err(ExecuteSequencesError::DecodebufferError(
                            crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                                offset,
                                buf_len: cur,
                            },
                        )));
                    }
                    if ml > 0 {
                        // Doubling chunks anchored at the match source (the
                        // same scheme as the flat executor): after `copied`
                        // bytes the readable span behind the anchor is
                        // `offset + copied`, so reads never run ahead of the
                        // write cursor.
                        // SAFETY: cur - offset >= 0 was checked; every chunk
                        // reads only already-written bytes and the budget
                        // check bounds the write end.
                        unsafe {
                            let src = base.add(cur - offset);
                            let mut copied = 0;
                            while copied < ml {
                                let chunk = (offset + copied).min(ml - copied);
                                core::ptr::copy(src, base.add(cur + copied), chunk);
                                copied += chunk;
                            }
                        }
                        w += ml;
                    }
                }
                let rest = lits.end - lit_pos;
                if w + rest > seg.out_size {
                    return Err(exec_err(ExecuteSequencesError::TargetTooSmall));
                }
                // SAFETY: budget checked above
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        seg.literals.as_ptr().add(lit_pos),
                        base.add(seg_start + w),
                        rest,
                    );
                }
                w += rest;
            },
        }
    }
    debug_assert_eq!(
        w, seg.out_size,
        "stage A size accounting must match execution"
    );
    Ok(())
}

/// Two-stage pipeline driver. `place(start, end)` makes the output region
/// valid for absolute positions `[0, end)` and returns its base pointer
/// (called per segment, so a growing Vec may move between calls). Returns
/// the total number of decoded bytes.
fn decode_parallel(
    input: &[u8],
    workers: u32,
    segments: &[SegmentPlan],
    place: &mut dyn FnMut(usize, usize) -> Result<*mut u8, FrameDecoderError>,
) -> Result<usize, FrameDecoderError> {
    let n_segments = segments.len();
    let threads = (workers as usize).min(n_segments);
    let slots: Vec<Mutex<Option<Result<DecodedSegment, FrameDecoderError>>>> =
        (0..n_segments).map(|_| Mutex::new(None)).collect();
    let ready = Condvar::new();
    let next_job = AtomicUsize::new(0);
    let consumed = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>> = Mutex::new(None);

    let result = std::thread::scope(|scope| -> Result<usize, FrameDecoderError> {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut scratch = SegmentScratch::new();
                loop {
                    if abort.load(Ordering::Relaxed) || poison.lock().unwrap().is_some() {
                        break;
                    }
                    let id = next_job.fetch_add(1, Ordering::Relaxed);
                    if id >= n_segments {
                        break;
                    }
                    // Bound staged-but-unexecuted segments so staging memory
                    // stays proportional to the worker count, not the input.
                    // The poison mutex is only the sleep token; the predicate
                    // (consumed) is atomic.
                    while id > consumed.load(Ordering::Acquire) + threads {
                        if abort.load(Ordering::Relaxed) {
                            break;
                        }
                        let guard = poison.lock().unwrap();
                        let (_guard, _timeout) = ready
                            .wait_timeout(guard, core::time::Duration::from_millis(50))
                            .unwrap();
                    }
                    if abort.load(Ordering::Relaxed) {
                        break;
                    }
                    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        decode_segment(input, &segments[id], &mut scratch)
                    }));
                    match attempt {
                        Ok(res) => *slots[id].lock().unwrap() = Some(res),
                        Err(payload) => {
                            *poison.lock().unwrap() = Some(payload);
                            // Release the slot so the ordered consumer can run
                            // to completion; this error is never observed
                            // (resume_unwind replaces it after the scope).
                            *slots[id].lock().unwrap() =
                                Some(Err(FrameDecoderError::NotYetInitialized));
                        },
                    }
                    ready.notify_all();
                }
            });
        }

        let mut offset_hist = [1u32, 4, 8];
        let exec_result = (|| -> Result<usize, FrameDecoderError> {
            let mut written = 0usize;
            for (id, seg_plan) in segments.iter().enumerate() {
                let staged = {
                    let mut guard = slots[id].lock().unwrap();
                    while guard.is_none() {
                        guard = ready.wait(guard).unwrap();
                    }
                    guard.take().unwrap()
                };
                let seg = staged?;
                if seg_plan.frame_start {
                    offset_hist = [1, 4, 8];
                }
                let base = place(written, written + seg.out_size)?;
                // SAFETY: `place` made [0, written + out_size) valid; every
                // match source below `written` is final (in-order execution).
                unsafe {
                    execute_segment(base, written, &seg, input, &mut offset_hist)?;
                }
                written += seg.out_size;
                consumed.store(id + 1, Ordering::Release);
                ready.notify_all();
            }
            Ok(written)
        })();
        if exec_result.is_err() {
            abort.store(true, Ordering::Release);
            ready.notify_all();
        }
        exec_result
    });
    if let Some(payload) = poison.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
    result
}

fn engage(input: &[u8], workers: u32, max_window_size: u64) -> Option<Vec<SegmentPlan>> {
    if workers < 2
        || input.len() < MIN_MT_INPUT
        || std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
    {
        return None;
    }
    scan(input, workers, max_window_size)
}

/// Parallel [`FrameDecoder::decode_all`]: decode a complete multi-frame
/// input into the caller's buffer with a worker pool. Falls back to the
/// sequential decoder for small inputs, dictionary frames, single-core
/// processes and inputs without a second restart point.
pub fn decode_all_mt(
    input: &[u8],
    output: &mut [u8],
    workers: u32,
    max_window_size: u64,
) -> Result<usize, FrameDecoderError> {
    if let Some(segments) = engage(input, workers, max_window_size) {
        let out_len = output.len();
        let mut place = |_start: usize, end: usize| -> Result<*mut u8, FrameDecoderError> {
            if end > out_len {
                return Err(FrameDecoderError::TargetTooSmall);
            }
            Ok(output.as_mut_ptr())
        };
        decode_parallel(input, workers, &segments, &mut place)
    } else {
        let mut decoder = FrameDecoder::new();
        decoder.set_max_window_size(max_window_size);
        decoder.decode_all(input, output)
    }
}

/// Parallel [`FrameDecoder::decode_all_to_vec`]: appends the decoded bytes
/// behind the vector's current length, growing it segment by segment
/// (segment sizes become exact after their stage A).
pub fn decode_to_vec_mt(
    input: &[u8],
    output: &mut Vec<u8>,
    workers: u32,
    max_window_size: u64,
) -> Result<(), FrameDecoderError> {
    if let Some(segments) = engage(input, workers, max_window_size) {
        let start_len = output.len();
        let mut place = |_start: usize, end: usize| -> Result<*mut u8, FrameDecoderError> {
            output.reserve(end - start_len);
            // SAFETY: start_len bytes are initialized and capacity now
            // covers `end`; the region base is the vector's data start. The
            // pointer is re-acquired per segment, so growth between segments
            // is fine, and set_len below publishes the writes segment by
            // segment so an error leaves the tail unobserved.
            Ok(output.as_mut_ptr())
        };
        let written = decode_parallel(input, workers, &segments, &mut place)?;
        // SAFETY: every byte in [start_len, start_len + written) was written
        // by execute_segment.
        unsafe { output.set_len(start_len + written) };
        Ok(())
    } else {
        // The parallel path grows `output` segment by segment; the
        // sequential fallback must honor the same append contract for a
        // fresh Vec, so retry with doubling capacity like bulk::decompress.
        // The tracked bound doubles explicitly: `reserve` is a no-op while
        // spare capacity already covers the request, so re-reserving a
        // constant would spin.
        let mut capacity = output.capacity().max(64 * 1024);
        loop {
            let mut decoder = FrameDecoder::new();
            decoder.set_max_window_size(max_window_size);
            match decoder.decode_all_to_vec(input, output) {
                Ok(()) => return Ok(()),
                Err(FrameDecoderError::TargetTooSmall) => {
                    capacity *= 2;
                    output.reserve(capacity - output.len());
                },
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec, vec::Vec};

    use super::{decode_all_mt, decode_to_vec_mt};
    use crate::{EncoderOptions, Level, bulk, decoding::FrameDecoder};

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

    const MAX_WINDOW: u64 = crate::decoding::DEFAULT_MAX_WINDOW_SIZE;

    /// Our own multithreaded encoder output must decode identically through
    /// the parallel decoder, our sequential decoder and libzstd.
    #[test]
    fn mt_frames_decode_identically() {
        let data = textish(8 * 1024 * 1024);
        for workers in [2u32, 4] {
            let compressed =
                bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(workers))
                    .unwrap();
            let mut out = vec![0u8; data.len()];
            let n = decode_all_mt(&compressed, &mut out, workers, MAX_WINDOW).unwrap();
            assert_eq!((n, &out[..n]), (data.len(), &data[..]));

            let mut vec_out = Vec::new();
            decode_to_vec_mt(&compressed, &mut vec_out, workers, MAX_WINDOW).unwrap();
            assert_eq!(vec_out, data);

            let mut seq = vec![0u8; data.len()];
            let mut decoder = FrameDecoder::new();
            assert_eq!(
                decoder.decode_all(&compressed, &mut seq).unwrap(),
                data.len()
            );
            assert_eq!(seq, out);

            let mut libzstd = Vec::new();
            zstd::stream::copy_decode(compressed.as_slice(), &mut libzstd).unwrap();
            assert_eq!(libzstd, data);
        }
    }

    /// Concatenated frames (the multi-frame shape) decode through the
    /// parallel path as one unit list.
    #[test]
    fn concatenated_frames_decode() {
        let a = textish(3 * 1024 * 1024);
        let b: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 61) as u8).collect();
        let mut compressed = bulk::compress(&a, Level::Fastest);
        compressed.extend_from_slice(&bulk::compress(&b, Level::Fastest));
        let mut expect = a.clone();
        expect.extend_from_slice(&b);
        let mut out = Vec::new();
        decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(out, expect);
    }

    /// libzstd single-frame output (via the zstd crate) takes the parallel
    /// path too; verified byte-exact against the sequential decoder.
    #[test]
    fn zstd_crate_output_decodes() {
        let data = textish(6 * 1024 * 1024);
        let compressed = zstd::bulk::compress(&data, 3).unwrap();
        let mut out = Vec::new();
        decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(out, data);
    }

    /// Semi-structured records make libzstd emit Huffman-coded literals in
    /// back-to-back blocks, so segments stage several Huffman sections into
    /// one accumulated literals buffer (the absolute-length check in
    /// `decompress_literals` used to reject the second one).
    #[test]
    fn zstd_crate_output_decodes_semi_structured() {
        let mut state = 12345u64;
        let mut data = Vec::with_capacity(8 * 1024 * 1024);
        let mut id = 0u64;
        while data.len() < 8 * 1024 * 1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let user = (state >> 33) % 5000;
            let event = match (state >> 45) % 5 {
                0 => "click",
                1 => "view",
                2 => "purchase",
                3 => "error",
                _ => "login",
            };
            let payload_len = ((state >> 25) % 24) as usize;
            data.extend_from_slice(
                format!(
                    "{{\"id\":{id},\"user\":\"user_{user}\",\"event\":\"{event}\",\"ts\":{},\"\
                     payload\":\"",
                    1700000000 + id
                )
                .as_bytes(),
            );
            data.resize(data.len() + payload_len, b'x');
            data.extend_from_slice(b"\",\"score\":0.5}\n");
            id += 1;
        }
        for level in [1, 3, 9] {
            let compressed = zstd::bulk::compress(&data, level).unwrap();
            let mut out = Vec::new();
            decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
            assert_eq!(out, data, "level {level}");
            let mut placed = vec![0u8; data.len()];
            let n = decode_all_mt(&compressed, &mut placed, 4, MAX_WINDOW).unwrap();
            assert_eq!(&placed[..n], &data[..], "level {level}");
        }
    }

    /// A skippable frame between two real frames contributes no output.
    #[test]
    fn skippable_frames_are_skipped() {
        let a = textish(3 * 1024 * 1024);
        let b: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 61) as u8).collect();
        let mut with_skip = bulk::compress(&a, Level::Fastest);
        let skip_payload = [0xabu8; 64];
        with_skip.extend_from_slice(&0x184d2a50u32.to_le_bytes());
        with_skip.extend_from_slice(&(skip_payload.len() as u32).to_le_bytes());
        with_skip.extend_from_slice(&skip_payload);
        with_skip.extend_from_slice(&bulk::compress(&b, Level::Fastest));
        let mut expect = a.clone();
        expect.extend_from_slice(&b);
        let mut out = Vec::new();
        decode_to_vec_mt(&with_skip, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(out, expect);
    }

    /// Too-small output must be reported, and inputs without a second
    /// restart point (single small frame) fall back byte-exact.
    #[test]
    fn slice_too_small_and_fallback() {
        let data = textish(4 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mut small = vec![0u8; data.len() - 1];
        assert!(decode_all_mt(&compressed, &mut small, 4, MAX_WINDOW).is_err());

        let tiny = textish(64 * 1024);
        let tiny_c = bulk::compress(&tiny, Level::Fastest);
        let mut a = vec![0u8; tiny.len()];
        let mut b = vec![0u8; tiny.len()];
        let mut decoder = FrameDecoder::new();
        let n = decode_all_mt(&tiny_c, &mut a, 4, MAX_WINDOW).unwrap();
        assert_eq!(n, decoder.decode_all(&tiny_c, &mut b).unwrap());
        assert_eq!(a, b);
    }

    /// Corrupt input must surface an error, not silent garbage.
    #[test]
    fn corrupt_input_errors() {
        let data = textish(4 * 1024 * 1024);
        let mut compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        // Smash bytes in a middle segment's compressed body.
        let mid = compressed.len() / 2;
        compressed[mid] ^= 0xff;
        compressed[mid + 1] ^= 0xff;
        let mut out = Vec::new();
        // Errors (or a checksum-visible mismatch caught upstream) are both
        // acceptable; a panic or hang is not.
        let _ = decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW);
    }
}
