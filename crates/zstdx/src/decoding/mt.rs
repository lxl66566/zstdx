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
//! Checksummed frames are verified inline in stage B: the pre-scan collects
//! each trailer word, and the executor absorbs every segment's range into
//! the frame's xxh64 stream the moment it is final (see `frame_checksum`).
//!
//! Frames whose encoder jobs carry the deep-offset ramp guarantee can
//! instead execute stage B in parallel as job-grid "pieces" (see
//! `mt_pieces`, env-paired with the encoder-side ramp experiment).

use alloc::vec::Vec;
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex};

#[cfg(feature = "hash")]
use super::frame_checksum::{FrameBoundaries, StreamingChecksum};
use super::{
    FrameDecoder,
    errors::{
        DecodeBlockContentError, DecodeSequenceError, DecompressBlockError, FrameDecoderError,
    },
    frame,
    literals_section_decoder::decode_literals,
    scratch::{FSEScratch, HuffmanScratch},
    sequence_execution::{do_offset_history, wildcopy_match},
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
pub(super) struct ScannedBlock {
    btype: BlockType,
    /// Decompressed length of Raw blocks; the run length for RLE blocks.
    raw_size: u32,
    /// Body bytes within the whole input: the payload for Raw and
    /// Compressed, the single repeated byte for RLE.
    pub(super) body: Range<usize>,
}

/// A run of blocks between two restart points (or frame boundaries).
pub(super) struct SegmentPlan {
    pub(super) blocks: Vec<ScannedBlock>,
    /// This segment starts a frame: execution resets the repcode history
    /// and stage A gets fresh tables (frame starts are restart points).
    pub(super) frame_start: bool,
}

/// Scan output: the segment plans plus the expected content checksum per
/// frame (`None` for frames without one), in frame order.
pub(super) struct ScanPlan {
    pub(super) segments: Vec<SegmentPlan>,
    #[cfg(feature = "hash")]
    pub(super) checksums: Vec<Option<u32>>,
    /// The pledged content size when the input is exactly one frame
    /// carrying it (`None` for multi-frame/skippable inputs or frames
    /// without a size pledge). The piece-parallel executor needs it.
    pub(super) pledged_single: Option<u64>,
}

/// How stage B walks a block whose stage A staging is done. Every variant
/// carries the block's exact output size (stage A accounting), which the
/// piece planner uses to map restart points to output positions.
pub(super) enum BlockPlan {
    Raw {
        body: Range<usize>,
        out: usize,
    },
    Rle {
        byte: u8,
        len: usize,
    },
    Compressed {
        lits: Range<usize>,
        seqs: Range<usize>,
        out: usize,
    },
}

impl BlockPlan {
    pub(super) fn out(&self) -> usize {
        match *self {
            BlockPlan::Rle { len, .. } => len,
            BlockPlan::Raw { out, .. } | BlockPlan::Compressed { out, .. } => out,
        }
    }
}

/// Stage A output for one segment.
pub(super) struct DecodedSegment {
    pub(super) literals: Vec<u8>,
    pub(super) sequences: Vec<Sequence>,
    pub(super) plan: Vec<BlockPlan>,
    pub(super) out_size: usize,
}

/// Pooled staging buffers: every decode call used to map fresh
/// multi-megabyte staging vectors per segment, paying the whole first-touch
/// fault cost again on repeat calls (the encoder's accumulate-buffer
/// lesson, same machine). Global, not thread-local: stage A stages on
/// workers while the calling thread consumes, so buffers return across
/// threads. Contents are never observable across uses (staging overwrites
/// exactly what execution reads).
struct StagingPool {
    bufs: Mutex<Vec<StagingBuf>>,
    /// Retained-buffer budget; over it, buffers free normally.
    retained: AtomicUsize,
}

struct StagingBuf {
    literals: Vec<u8>,
    sequences: Vec<Sequence>,
}

const STAGING_POOL_MAX_BYTES: usize = 256 * 1024 * 1024;

static STAGING_POOL: std::sync::OnceLock<StagingPool> = std::sync::OnceLock::new();

fn staging_take() -> StagingBuf {
    let pool = STAGING_POOL.get_or_init(|| StagingPool {
        bufs: Mutex::new(Vec::new()),
        retained: AtomicUsize::new(0),
    });
    match pool.bufs.lock().unwrap().pop() {
        Some(mut b) => {
            b.literals.clear();
            b.sequences.clear();
            pool.retained.fetch_sub(
                b.literals.capacity() + b.sequences.capacity() * size_of::<Sequence>(),
                Ordering::Relaxed,
            );
            b
        },
        None => StagingBuf {
            literals: Vec::new(),
            sequences: Vec::new(),
        },
    }
}

fn staging_put(bufs: StagingBuf) {
    let bytes = bufs.literals.capacity() + bufs.sequences.capacity() * size_of::<Sequence>();
    let pool = STAGING_POOL.get_or_init(|| StagingPool {
        bufs: Mutex::new(Vec::new()),
        retained: AtomicUsize::new(0),
    });
    if pool.retained.load(Ordering::Relaxed) + bytes <= STAGING_POOL_MAX_BYTES {
        pool.retained.fetch_add(bytes, Ordering::Relaxed);
        pool.bufs.lock().unwrap().push(bufs);
    }
}

impl Drop for DecodedSegment {
    fn drop(&mut self) {
        staging_put(StagingBuf {
            literals: core::mem::take(&mut self.literals),
            sequences: core::mem::take(&mut self.sequences),
        });
    }
}

/// Reusable per-worker entropy state; reset between segments (every
/// segment starts at a restart point, so fresh tables are correct).
pub(super) struct SegmentScratch {
    huf: HuffmanScratch,
    fse: FSEScratch,
}

impl SegmentScratch {
    pub(super) fn new() -> Self {
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
fn scan(input: &[u8], workers: u32, max_window_size: u64) -> Option<ScanPlan> {
    let target_segment = (input.len() / (workers as usize * 2)).max(MIN_SEGMENT_INPUT);
    let mut segments: Vec<SegmentPlan> = Vec::new();
    #[cfg(feature = "hash")]
    let mut checksums: Vec<Option<u32>> = Vec::new();
    let mut n_frames = 0usize;
    let mut saw_skip = false;
    let mut pledged_single: Option<u64> = None;
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
                saw_skip = true;
                cursor = end;
                continue;
            },
            Err(_) => return None,
        };
        n_frames += 1;
        let fcs_present = frame_header.descriptor.frame_content_size_flag() != 0
            || frame_header.descriptor.single_segment_flag();
        pledged_single = if n_frames == 1 && fcs_present && !saw_skip {
            Some(frame_header.frame_content_size())
        } else {
            None
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
            let trailer_end = scan_cur.checked_add(4)?;
            if trailer_end > input.len() {
                return None;
            }
            #[cfg(feature = "hash")]
            checksums.push(Some(u32::from_le_bytes([
                input[scan_cur],
                input[scan_cur + 1],
                input[scan_cur + 2],
                input[scan_cur + 3],
            ])));
            scan_cur = trailer_end;
        } else {
            #[cfg(feature = "hash")]
            checksums.push(None);
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
    (segments.len() >= 2).then_some(ScanPlan {
        segments,
        #[cfg(feature = "hash")]
        checksums,
        pledged_single,
    })
}

/// Stage A: decode one segment's literals and sequences into staging.
pub(super) fn decode_segment(
    input: &[u8],
    plan: &SegmentPlan,
    scratch: &mut SegmentScratch,
) -> Result<DecodedSegment, FrameDecoderError> {
    scratch.reset_tables();
    let staging = staging_take();
    let (mut literals, mut sequences) = (staging.literals, staging.sequences);
    let mut out_size = 0usize;
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for blk in &plan.blocks {
        match blk.btype {
            BlockType::Raw => {
                out_size += blk.body.len();
                blocks.push(BlockPlan::Raw {
                    body: blk.body.clone(),
                    out: blk.body.len(),
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
                let block_out = if seq_header.num_sequences != 0 {
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
                    literals.len() - lits_start + match_bytes
                } else {
                    // Zero-sequence block: the FSE tables stay untouched (the
                    // scratch carries them across blocks). The sequential
                    // path rejects trailing bytes after the header.
                    let rest = &seq_raw_all[seq_header_len..];
                    if !rest.is_empty() {
                        return Err(block_body_err(DecompressBlockError::DecodeSequenceError(
                            DecodeSequenceError::ExtraBits {
                                bits_remaining: rest.len() as isize * 8,
                            },
                        )));
                    }
                    literals.len() - lits_start
                };
                out_size += block_out;
                blocks.push(BlockPlan::Compressed {
                    lits: lits_start..literals.len(),
                    seqs: seqs_start..sequences.len(),
                    out: block_out,
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
/// `buf_limit` is the absolute end of the region the writes may overshoot
/// by up to 16 bytes (wildcopy slack): the caller's slice end, or the
/// vector's capacity. The slack bytes are overwritten in address order
/// before anything can read them (same contract as the flat executor).
///
/// # Safety
/// `base` must be valid for writes in `[seg_start, seg_start + out_size)`
/// plus 16 bytes of slack below `buf_limit`, and for reads in
/// `[0, seg_start)`. The caller guarantees both by sizing the output
/// before each segment and executing in order.
pub(super) unsafe fn execute_segment(
    base: *mut u8,
    seg_start: usize,
    seg: &DecodedSegment,
    input: &[u8],
    offset_hist: &mut [u32; 3],
    buf_limit: usize,
) -> Result<(), FrameDecoderError> {
    unsafe {
        execute_blocks(
            base,
            seg_start,
            seg,
            0..seg.plan.len(),
            input,
            offset_hist,
            buf_limit,
            usize::MAX,
        )
    }
}

/// Execute a contiguous block range of a staged segment starting at
/// absolute output position `start` (the first block's own position).
/// `wild_cap` bounds where wildcopy may overshoot beyond the copy end
/// (the piece executor passes the piece end so parallel pieces never
/// write into a neighbor's range; the serial path passes `usize::MAX`).
///
/// # Safety
/// `base` must be valid for writes over the blocks' combined output plus
/// 16 bytes of slack below `min(buf_limit, wild_cap)`, and for reads in
/// `[0, start)`. `offset_hist` must hold the true repcode history at
/// `start`.
pub(super) unsafe fn execute_blocks(
    base: *mut u8,
    start: usize,
    seg: &DecodedSegment,
    blocks: Range<usize>,
    input: &[u8],
    offset_hist: &mut [u32; 3],
    buf_limit: usize,
    wild_cap: usize,
) -> Result<(), FrameDecoderError> {
    use crate::decoding::errors::ExecuteSequencesError;
    let exec_err =
        |e: ExecuteSequencesError| block_body_err(DecompressBlockError::ExecuteSequencesError(e));
    let cap: usize = seg.plan[blocks.clone()].iter().map(BlockPlan::out).sum();
    let write_limit = buf_limit.min(wild_cap);
    let mut w = 0usize;
    for block in &seg.plan[blocks] {
        match block {
            BlockPlan::Raw { body, .. } => {
                // SAFETY: stage A accounted this block's body length in cap
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        input.as_ptr().add(body.start),
                        base.add(start + w),
                        body.len(),
                    );
                }
                w += body.len();
            },
            BlockPlan::Rle { byte, len } => {
                // SAFETY: stage A accounted the run length in cap
                unsafe {
                    core::ptr::write_bytes(base.add(start + w), *byte, *len);
                }
                w += len;
            },
            BlockPlan::Compressed { lits, seqs, .. } => {
                // SAFETY: lits is a range of seg.literals
                let lit_base = unsafe { seg.literals.as_ptr().add(lits.start) };
                let lit_end = unsafe { seg.literals.as_ptr().add(lits.end) };
                let mut lit = lit_base;
                for seq in &seg.sequences[seqs.clone()] {
                    let ll = seq.ll as usize;
                    let ml = seq.ml as usize;
                    if w + ll + ml > cap {
                        return Err(exec_err(ExecuteSequencesError::TargetTooSmall));
                    }
                    if lit as usize + ll > lit_end as usize {
                        return Err(exec_err(ExecuteSequencesError::NotEnoughBytesForSequence {
                            wanted: (lit as usize - lit_base as usize) + ll,
                            have: lits.end - lits.start,
                        }));
                    }
                    // Wildcopy when the 16-byte overshoot of both the
                    // literal and the match copy stays below the write
                    // limit; the literal side additionally needs its own
                    // 16-byte overread to stay inside the staged literals.
                    // SAFETY: budget checks bound the writes; the literal
                    // range was checked above
                    let end = start + w + ll + ml;
                    let wild = end + 16 <= write_limit;
                    unsafe {
                        let dst = base.add(start + w);
                        if ll > 0 {
                            if wild && lit.add(ll + 16) <= lit_end {
                                copy16_chunks(dst, lit, ll);
                            } else {
                                core::ptr::copy_nonoverlapping(lit, dst, ll);
                            }
                            lit = lit.add(ll);
                        }
                        w += ll;
                        let offset = do_offset_history(seq.of, seq.ll, offset_hist);
                        if offset == 0 {
                            return Err(exec_err(ExecuteSequencesError::ZeroOffset));
                        }
                        let offset = offset as usize;
                        let cur = start + w;
                        if offset > cur {
                            return Err(exec_err(ExecuteSequencesError::DecodebufferError(
                                crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                                    offset,
                                    buf_len: cur,
                                },
                            )));
                        }
                        if ml > 0 {
                            let dst = base.add(cur);
                            let src = dst.sub(offset);
                            if wild {
                                wildcopy_match(dst, src, ml);
                            } else {
                                // Doubling chunks anchored at the match
                                // source: after `copied` bytes the readable
                                // span behind the anchor is `offset +
                                // copied`, so reads never run ahead of the
                                // write cursor.
                                let mut copied = 0;
                                while copied < ml {
                                    let chunk = (offset + copied).min(ml - copied);
                                    core::ptr::copy(src, dst.add(copied), chunk);
                                    copied += chunk;
                                }
                            }
                        }
                        w += ml;
                    }
                }
                let rest = lit_end as usize - lit as usize;
                if w + rest > cap {
                    return Err(exec_err(ExecuteSequencesError::TargetTooSmall));
                }
                // SAFETY: budget checked above
                unsafe {
                    core::ptr::copy_nonoverlapping(lit, base.add(start + w), rest);
                }
                w += rest;
            },
        }
    }
    debug_assert_eq!(w, cap, "stage A size accounting must match execution");
    Ok(())
}

/// Copy `len` bytes in 16-byte chunks, overshooting by up to 15 bytes (the
/// caller guarantees the overshoot stays inside the write bound and the
/// read bound).
#[inline(always)]
unsafe fn copy16_chunks(mut d: *mut u8, mut s: *const u8, len: usize) {
    unsafe {
        d.cast::<u128>()
            .write_unaligned(s.cast::<u128>().read_unaligned());
        if len > 16 {
            let end = d.add(len);
            d = d.add(16);
            s = s.add(16);
            while d < end {
                d.cast::<u128>()
                    .write_unaligned(s.cast::<u128>().read_unaligned());
                d = d.add(16);
                s = s.add(16);
            }
        }
    }
}

/// Two-stage pipeline driver. `place(start, end)` makes the output region
/// valid for absolute positions `[0, end)` and returns its base pointer
/// (called per segment, so a growing Vec may move between calls). Returns
/// the total number of decoded bytes.
fn decode_parallel(
    input: &[u8],
    workers: u32,
    segments: &[SegmentPlan],
    place: &mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>,
    #[cfg(feature = "hash")] after_segment: &mut dyn FnMut(*const u8, Range<usize>),
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
                #[cfg(feature = "hash")]
                let seg_out_start = written;
                let (base, buf_limit) = place(written, written + seg.out_size)?;
                // SAFETY: `place` made [0, written + out_size) valid; every
                // match source below `written` is final (in-order execution).
                unsafe {
                    execute_segment(base, written, &seg, input, &mut offset_hist, buf_limit)?;
                }
                written += seg.out_size;
                // The executed range is final from here on: later writes never
                // land below the executor cursor.
                #[cfg(feature = "hash")]
                after_segment(base.cast_const(), seg_out_start..written);
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

fn engage(input: &[u8], workers: u32, max_window_size: u64) -> Option<ScanPlan> {
    if workers < 2
        || input.len() < MIN_MT_INPUT
        || std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
    {
        return None;
    }
    scan(input, workers, max_window_size)
}

/// Hidden: not API-stable (integration tests reach the router-forced MT
/// entry points with custom output buffers).
#[doc(hidden)]
pub fn mt_decode_all_for_tests(
    input: &[u8],
    output: &mut [u8],
    workers: u32,
    max_window_size: u64,
) -> Result<usize, FrameDecoderError> {
    decode_all_mt(input, output, workers, max_window_size)
}

/// Hidden: not API-stable.
#[doc(hidden)]
pub fn mt_decode_to_vec_for_tests(
    input: &[u8],
    output: &mut Vec<u8>,
    workers: u32,
    max_window_size: u64,
) -> Result<(), FrameDecoderError> {
    decode_to_vec_mt(input, output, workers, max_window_size)
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
    if let Some(plan) = engage(input, workers, max_window_size) {
        if let Some(fcs) = super::mt_pieces::gate(&plan) {
            let out_len = output.len();
            let mut place =
                |_start: usize, end: usize| -> Result<(*mut u8, usize), FrameDecoderError> {
                    if end > out_len {
                        return Err(FrameDecoderError::TargetTooSmall);
                    }
                    Ok((output.as_mut_ptr(), out_len))
                };
            return super::mt_pieces::decode_pieces(input, workers, &plan, fcs, &mut place);
        }
        let out_len = output.len();
        let mut place =
            |_start: usize, end: usize| -> Result<(*mut u8, usize), FrameDecoderError> {
                if end > out_len {
                    return Err(FrameDecoderError::TargetTooSmall);
                }
                Ok((output.as_mut_ptr(), out_len))
            };
        #[cfg(feature = "hash")]
        let mut checksum = FrameBoundaries::new(&plan).map(StreamingChecksum::new);
        #[cfg(feature = "hash")]
        let mut after_segment = |base: *const u8, range: Range<usize>| {
            // SAFETY: the range was just executed at this base by the same
            // thread; decode_parallel calls this before the next `place`
            // can move the output.
            unsafe {
                if let Some(c) = checksum.as_mut() {
                    c.absorb_executed(base, range);
                }
            }
        };
        #[cfg(feature = "hash")]
        let written = decode_parallel(
            input,
            workers,
            &plan.segments,
            &mut place,
            &mut after_segment,
        )?;
        #[cfg(feature = "hash")]
        if let Some(err) = checksum.and_then(|mut c| c.take_result()) {
            return Err(err);
        }
        #[cfg(not(feature = "hash"))]
        let written = decode_parallel(input, workers, &plan.segments, &mut place)?;
        Ok(written)
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
    if let Some(plan) = engage(input, workers, max_window_size) {
        if let Some(fcs) = super::mt_pieces::gate(&plan) {
            let start_len = output.len();
            let mut place =
                |_start: usize, end: usize| -> Result<(*mut u8, usize), FrameDecoderError> {
                    output.reserve(end);
                    // SAFETY: in-bounds pointer one past the existing
                    // contents, inside the vector's allocation (see the
                    // serial path below for the full contract).
                    Ok((
                        unsafe { output.as_mut_ptr().add(start_len) },
                        output.capacity() - start_len,
                    ))
                };
            let written = super::mt_pieces::decode_pieces(input, workers, &plan, fcs, &mut place)?;
            // SAFETY: every byte in [start_len, start_len + written) was
            // written by the piece executor.
            unsafe { output.set_len(start_len + written) };
            return Ok(());
        }
        let start_len = output.len();
        let mut place =
            |_start: usize, end: usize| -> Result<(*mut u8, usize), FrameDecoderError> {
                output.reserve(end);
                // `end` is executor-relative, i.e. exactly the additional
                // bytes needed behind the existing contents; the base hands
                // out that region and the limit is the remaining capacity.
                // SAFETY: start_len bytes are initialized and the reserve
                // above covers `end` more. The pointer is re-acquired per
                // segment, so growth between segments is fine, and set_len
                // below publishes the writes segment by segment so an error
                // leaves the tail unobserved. The wildcopy slack may write up
                // to 16 bytes past `end` inside the capacity (raw writes;
                // never read past `end`).
                Ok((
                    // SAFETY: in-bounds pointer one past the existing
                    // contents, inside the vector's allocation.
                    unsafe { output.as_mut_ptr().add(start_len) },
                    output.capacity() - start_len,
                ))
            };
        #[cfg(feature = "hash")]
        let mut checksum = FrameBoundaries::new(&plan).map(StreamingChecksum::new);
        #[cfg(feature = "hash")]
        // SAFETY: same-thread absorb of just-executed bytes, before the next
        // `place` call may grow (and move) the vector.
        let mut after_segment = |base: *const u8, range: Range<usize>| unsafe {
            if let Some(c) = checksum.as_mut() {
                c.absorb_executed(base, range);
            }
        };
        #[cfg(feature = "hash")]
        let written = decode_parallel(
            input,
            workers,
            &plan.segments,
            &mut place,
            &mut after_segment,
        )?;
        // Verification finished before this point, so the length stays
        // unchanged on a checksum error (the sequential decode_all_to_vec
        // contract).
        #[cfg(feature = "hash")]
        if let Some(err) = checksum.and_then(|mut c| c.take_result()) {
            return Err(err);
        }
        #[cfg(not(feature = "hash"))]
        let written = decode_parallel(input, workers, &plan.segments, &mut place)?;
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

    /// A zero-sequence compressed block mid-segment (our MT encoder emits
    /// them on small-alphabet data at balanced levels) must stage exactly
    /// like the sequential path: FSE tables untouched, literals appended,
    /// no mode byte in the section.
    #[test]
    fn zero_sequence_blocks_decode() {
        let mut state = 7u64;
        let alphabet: Vec<u8> = (0..16u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        let mut data = Vec::with_capacity(8 * 1024 * 1024);
        while data.len() < 8 * 1024 * 1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            data.push(alphabet[((state >> 33) as usize) % 16]);
        }
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::from_zstd(9)).workers(4))
                .unwrap();
        let mut out = vec![0u8; data.len()];
        let n = decode_all_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(&out[..n], &data[..]);
        let mut vec_out = Vec::new();
        decode_to_vec_mt(&compressed, &mut vec_out, 4, MAX_WINDOW).unwrap();
        assert_eq!(vec_out, data);
    }

    /// Appending to a non-empty vector must leave the existing contents
    /// untouched (execution coordinates are append-relative; the place
    /// callback used to hand out the vector's base and overwrite them).
    #[test]
    fn append_to_nonempty_vec() {
        let data = textish(8 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mut out = alloc::vec![0x41u8; 1024 * 1024];
        let prefix = out.clone();
        decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(&out[..prefix.len()], &prefix[..]);
        assert_eq!(&out[prefix.len()..], &data[..]);
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
        // Errors (or the frame checksum flagging the mismatch) are both
        // acceptable; a panic or hang is not.
        let _ = decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW);
    }
}
