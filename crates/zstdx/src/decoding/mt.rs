//! Parallel decoding of complete in-memory inputs.
//!
//! A cheap pre-scan walks the block headers (3 bytes per block) and splits
//! the input into *segments* — units stage A stages in parallel and stage B
//! consumes one at a time. A segment start needs the entropy tables its
//! staging runs on. A *restart point* — every FSE stream out of Repeat
//! mode, and literals that rebuild the Huffman table (Compressed mode) or
//! ignore it entirely (Raw/RLE, only while no Treeless block precedes the
//! next table rebuild) — makes fresh tables provably suffice, and encoders
//! with jobs (libzstd's `-T` mode, this crate's [`crate::encoding::mt`])
//! emit such blocks at every job boundary. Any other block boundary works
//! as a start too, provided staging first re-establishes the *table
//! epoch*: the tables the last restart point's epoch carries there,
//! rebuilt from the description the pre-scan records per axis (see
//! [`TablePrime`]). The planner prefers restart points — a segment start
//! snaps to the first restart point past each target multiple when one
//! lies within 1.5x of it — so restart-dense frames segment exactly as a
//! restart-only scan would, and only restart-poor epochs (the shapes one
//! long staging step serializes on) split at block boundaries.
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
//! Dictionary frames, small inputs (unless the pledge routes them to the
//! piece executor), inputs without a second segment and single-core
//! processes fall back to the sequential decoder.
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
    num::NonZero,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::sync::{Condvar, Mutex};

#[cfg(feature = "hash")]
use super::frame_checksum::{FrameBoundaries, StreamingChecksum};
use super::{
    FrameDecoder,
    errors::{
        DecodeBlockContentError, DecodeSequenceError, DecompressBlockError, ExecuteSequencesError,
        FrameDecoderError,
    },
    frame,
    literals_section_decoder::decode_literals,
    scratch::{FSEScratch, HuffmanScratch},
    sequence_execution::wildcopy_match,
    sequence_section_decoder::decode_sequences_into,
};
use crate::blocks::{
    block::BlockType,
    literals_section::{LiteralsSection, LiteralsSectionType},
    sequence_section::{ModeType, Sequence, SequencesHeader},
};

/// Below this (compressed) input size the scan, spawn and hand-off overhead
/// dominates — unless the frame pledges an output in the piece executor's
/// regime (high-ratio shapes: the decode work scales with the pledge, not
/// the compressed bytes). The restart-point count is the real gate; this
/// only avoids spawning for inputs that could parallelize but not benefit.
const MIN_MT_INPUT: usize = 512 * 1024;
/// Segment-size floor: below it the staging buffers and hand-offs cost
/// more than the parallelism saves.
const MIN_SEGMENT_INPUT: usize = 256 * 1024;
/// Head floor of the ramped grid: the first cuts of a literal-dominated
/// frame double from here up to the regular target, cutting the consumer's
/// first wait behind staging.
const RAMP_HEAD_INPUT: usize = 128 * 1024;
/// The staged sequence record: the flat 12 B `Sequence` triple.
const SEQUENCE_RECORD_BYTES: u64 = 12;

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

/// A run of blocks between two segment starts.
pub(super) struct SegmentPlan {
    pub(super) blocks: Vec<ScannedBlock>,
    /// This segment starts a frame: execution resets the repcode history
    /// and stage A gets fresh tables (frame starts are restart points).
    pub(super) frame_start: bool,
    /// The frame's declared window: stage A bounds every block's output by
    /// `min(window, MAX_BLOCK_SIZE)` so a hostile claim cannot size the
    /// output buffer (same cap the sequential paths enforce).
    pub(super) window_size: usize,
    /// The table epoch a start inside a restart point's epoch primes stage
    /// A from. `None` at restart points and frame starts, where fresh
    /// tables are correct.
    pub(super) prime: Option<TablePrime>,
}

/// Carried entropy tables for a mid-epoch segment start: the input body
/// range of the block that last described each table axis since the last
/// restart point. A staging lease replays those descriptions — a table
/// build per axis at header-parse rate — and holds exactly the tables the
/// whole-epoch decode would carry at that boundary. `None` on an axis is
/// the restart point's fresh state: a Treeless or Repeat use inside the
/// segment then fails where the whole-epoch decode fails, with the same
/// error.
#[derive(Clone, Default)]
pub(super) struct TablePrime {
    /// Last block whose literals section is Compressed (rebuilds the
    /// Huffman table).
    huffman: Option<Range<usize>>,
    /// Last blocks whose sequence sections explicitly describe each FSE
    /// stream (Predefined, RLE or FSE_Compressed; Repeat carries).
    ll: Option<Range<usize>>,
    of: Option<Range<usize>>,
    ml: Option<Range<usize>>,
}

/// Scan output: the segment plans plus the expected content checksum per
/// frame (`None` for frames without one), in frame order.
pub(super) struct ScanPlan {
    pub(super) segments: Vec<SegmentPlan>,
    #[cfg(feature = "hash")]
    pub(super) checksums: Vec<Option<u32>>,
    /// Declared Frame_Content_Size per frame in scan order (`None` when the
    /// header carries none). The parallel executors never see a frame tail,
    /// so every frame's output length is verified against its entry after
    /// execution.
    pub(super) frame_pledges: Vec<Option<u64>>,
    /// The pledged content size when the input is exactly one frame
    /// carrying it (`None` for multi-frame/skippable inputs or frames
    /// without a size pledge). The piece-parallel executor needs it.
    pub(super) pledged_single: Option<u64>,
    /// Per-frame staged volume, the literals bytes plus sequence records
    /// at 12 B each, `None` when a block's headers failed to parse so the
    /// totals under-count. Plan-time pricing input for the frame-lane
    /// budget; nothing on the decode path reads it.
    pub(super) frame_staged: Vec<Option<u64>>,
}

/// How stage B walks a block whose stage A staging is done. Every variant
/// carries the block's exact output size (stage A accounting), which the
/// piece planner uses to map segment starts to output positions.
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

/// Reusable per-worker entropy state; reset between segments and then
/// primed from the segment's [`TablePrime`] when it starts mid-epoch.
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

/// How a compressed block's literals section treats the Huffman table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HuffmanState {
    /// Compressed literals: the block rebuilds the table.
    Rebuild,
    /// Treeless literals: the block decodes through the carried table.
    Reuse,
    /// Raw/RLE literals: the block ignores the table entirely.
    Ignore,
}

/// Per-block entropy-state classification from the pre-scan: how the block
/// treats each carried table axis. A restart point needs every axis
/// re-established (or ignored); a mid-epoch cut instead replays the
/// recorded descriptions.
struct RestartProbe {
    huffman: HuffmanState,
    /// Whether the block's sequence section explicitly describes each FSE
    /// stream (Predefined, RLE or FSE_Compressed). Repeat streams carry
    /// state across a cut; a zero-sequence block touches no stream.
    ll_explicit: bool,
    of_explicit: bool,
    ml_explicit: bool,
}

impl RestartProbe {
    /// Every FSE stream leaves Repeat mode (explicit table or the block
    /// carries none at all): a restart point's FSE axis.
    fn fse_rebuilt(&self) -> bool {
        self.ll_explicit && self.of_explicit && self.ml_explicit
    }

    /// A malformed block carries every axis: never a restart candidate,
    /// never a fold update, and the staging path reports the real error at
    /// the block itself.
    fn carried() -> Self {
        RestartProbe {
            huffman: HuffmanState::Reuse,
            ll_explicit: false,
            of_explicit: false,
            ml_explicit: false,
        }
    }
}

/// Payload bytes of a literals section: the tree description plus coded
/// streams, the single RLE byte, or the raw bytes themselves.
fn literals_payload_len(section: &LiteralsSection) -> usize {
    match section.compressed_size {
        Some(c) => c as usize,
        None => match section.ls_type {
            LiteralsSectionType::RLE => 1,
            _ => section.regenerated_size as usize,
        },
    }
}

/// Staged-volume accounting for one compressed block, off the same header
/// parse as the entropy probe: the literals the block stages (its
/// regenerated size) and its sequence count. Raw and RLE blocks stage
/// nothing, so they contribute no volume.
struct BlockVolumes {
    literals: u64,
    sequences: u32,
}

/// Per-frame staged-volume totals from the pre-scan: the literals bytes
/// staging writes and the sequence records at 12 B each.
struct FrameVolumes {
    literals: u64,
    sequences: u64,
    /// False when any compressed block's headers failed to parse: the
    /// totals then under-count and the ramp gate must not arm.
    complete: bool,
}

impl FrameVolumes {
    /// The head-ramp gate. A frame arms when its staged literals at least
    /// equal its staged sequence records, a literals share of one half:
    /// on such frames the consumer's exec runs ahead of staging and
    /// earlier first cuts pay, while sequence-dominated frames are
    /// exec-bound and a finer grid only adds wait. Frames at or below the
    /// MT floor and single-worker scans keep the target grid, and a
    /// block that failed to parse disarms the frame (its staging reports
    /// the real error).
    fn ramp_eligible(&self, workers: u32, frame_input_len: usize) -> bool {
        self.complete
            && workers > 1
            && frame_input_len > MIN_MT_INPUT
            && self.literals >= SEQUENCE_RECORD_BYTES * self.sequences
    }
}

/// Classify a compressed block's body for the restart-point scan.
fn probe_restart(body: &[u8]) -> (RestartProbe, Option<BlockVolumes>) {
    let mut section = LiteralsSection::new();
    let Ok(header_len) = section.parse_from_header(body) else {
        return (RestartProbe::carried(), None);
    };
    let Some(seq_raw) = body.get(header_len as usize + literals_payload_len(&section)..) else {
        return (RestartProbe::carried(), None);
    };
    let mut seq_header = SequencesHeader::new();
    if seq_header.parse_from_header(seq_raw).is_err() {
        return (RestartProbe::carried(), None);
    }
    let huffman = match section.ls_type {
        LiteralsSectionType::Compressed => HuffmanState::Rebuild,
        LiteralsSectionType::Treeless => HuffmanState::Reuse,
        LiteralsSectionType::Raw | LiteralsSectionType::RLE => HuffmanState::Ignore,
    };
    let (ll_explicit, of_explicit, ml_explicit) = match seq_header.modes {
        // A zero-sequence block leaves the FSE tables untouched, so the
        // next block could legally use Repeat; only explicit modes
        // re-establish a stream.
        None => (false, false, false),
        Some(modes) => (
            !matches!(modes.ll_mode(), ModeType::Repeat),
            !matches!(modes.of_mode(), ModeType::Repeat),
            !matches!(modes.ml_mode(), ModeType::Repeat),
        ),
    };
    (
        RestartProbe {
            huffman,
            ll_explicit,
            of_explicit,
            ml_explicit,
        },
        Some(BlockVolumes {
            literals: section.regenerated_size as u64,
            sequences: seq_header.num_sequences,
        }),
    )
}

/// The block indices a segment may start at. Besides blocks that rebuild
/// every table (Compressed literals + explicit FSE), a Raw/RLE-literals
/// block with explicit FSE tables qualifies when no Treeless-literals block
/// precedes the next table rebuild: until a rebuild (or the cut itself)
/// such a block would decode through the table stage A just reset.
fn restart_candidates(probes: &[RestartProbe]) -> Vec<usize> {
    let mut cut_ok = alloc::vec![false; probes.len()];
    let mut treeless_ahead = false;
    for i in (0..probes.len()).rev() {
        cut_ok[i] = !treeless_ahead;
        match probes[i].huffman {
            HuffmanState::Rebuild => treeless_ahead = false,
            HuffmanState::Reuse => treeless_ahead = true,
            HuffmanState::Ignore => {},
        }
    }
    probes
        .iter()
        .enumerate()
        .filter(|&(i, p)| {
            p.fse_rebuilt()
                && match p.huffman {
                    HuffmanState::Rebuild => true,
                    HuffmanState::Ignore => cut_ok[i],
                    HuffmanState::Reuse => false,
                }
        })
        .map(|(i, _)| i)
        .collect()
}

/// Segment-start block indices for one frame, ascending. For each target
/// multiple of the segment size: the first restart point past the target
/// wins when it lies within 1.5x of the target, otherwise the cut lands on
/// the block boundary nearest the target and staging primes from the
/// carried table epoch. Restart-dense frames therefore keep the
/// restart-point segmentation exactly; only restart-poor epochs split.
/// `ramp` starts the target ladder at [`RAMP_HEAD_INPUT`] doubling up to
/// the regular target (see [`FrameVolumes::ramp_eligible`]); with `ramp`
/// off the ladder is the plain target stride.
fn plan_frame_cuts(
    blocks: &[ScannedBlock],
    restarts: &[usize],
    target: usize,
    frame_input_len: usize,
    blocks_start: usize,
    ramp: bool,
) -> Vec<usize> {
    let boundary_input = |i: usize| blocks[i].body.start - blocks_start;
    let mut cuts = Vec::new();
    let mut seg_start = 0usize;
    let mut next_target = if ramp {
        RAMP_HEAD_INPUT.min(target)
    } else {
        target
    };
    let mut ri = 0usize;
    while next_target < frame_input_len {
        while ri < restarts.len() && boundary_input(restarts[ri]) < next_target {
            ri += 1;
        }
        if let Some(&r) = restarts.get(ri) {
            if r > seg_start && boundary_input(r) - next_target <= next_target / 2 {
                cuts.push(r);
                seg_start = r;
                ri += 1;
                next_target = advance_target(next_target, target);
                continue;
            }
        }
        // A restart snap can overshoot this target by up to half of it;
        // skip targets the current start already covers instead of cutting
        // a one-block segment behind it.
        if next_target <= boundary_input(seg_start) {
            next_target = advance_target(next_target, target);
            continue;
        }
        // Blocks are input-ordered, so the boundaries straddling the target
        // are the partition point and its predecessor; keep the earlier one
        // on a tie so the plan is input-determined.
        let pos = blocks.partition_point(|b| b.body.start - blocks_start < next_target);
        let mut cut = None;
        for cand in [pos.wrapping_sub(1), pos] {
            if cand < blocks.len()
                && cand > seg_start
                && cut.is_none_or(|c| {
                    boundary_input(cand).abs_diff(next_target)
                        < boundary_input(c).abs_diff(next_target)
                })
            {
                cut = Some(cand);
            }
        }
        let Some(c) = cut else { break };
        cuts.push(c);
        seg_start = c;
        next_target = advance_target(next_target, target);
    }
    cuts
}

/// Next rung of the target ladder: the ramped head doubles up to the
/// regular target, which then keeps its stride.
fn advance_target(next_target: usize, target: usize) -> usize {
    if next_target < target {
        next_target.saturating_mul(2).min(target)
    } else {
        next_target + target
    }
}

/// Which block last described each entropy axis since the last restart
/// point: the table epoch a mid-epoch segment start primes from.
#[derive(Default)]
struct EpochFold {
    huffman: Option<usize>,
    ll: Option<usize>,
    of: Option<usize>,
    ml: Option<usize>,
}

impl EpochFold {
    /// A restart point re-describes every axis, so the fold after one is
    /// fresh; an axis with no description since is legally usable only in
    /// explicit mode (its Repeat or Treeless uses fail identically in the
    /// whole-epoch decode).
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn observe(&mut self, i: usize, probe: &RestartProbe) {
        if probe.huffman == HuffmanState::Rebuild {
            self.huffman = Some(i);
        }
        if probe.ll_explicit {
            self.ll = Some(i);
        }
        if probe.of_explicit {
            self.of = Some(i);
        }
        if probe.ml_explicit {
            self.ml = Some(i);
        }
    }

    fn snapshot(&self, blocks: &[ScannedBlock]) -> TablePrime {
        let body = |slot: Option<usize>| slot.map(|i| blocks[i].body.clone());
        TablePrime {
            huffman: body(self.huffman),
            ll: body(self.ll),
            of: body(self.of),
            ml: body(self.ml),
        }
    }
}

/// Walk one frame's blocks from `blocks_start`: the scanned blocks, their
/// entropy probes and the frame's staged-volume totals. `None` on
/// malformed structure — the sequential path reports those properly.
fn scan_frame_blocks(
    input: &[u8],
    window_size: usize,
    blocks_start: usize,
) -> Option<(Vec<ScannedBlock>, Vec<RestartProbe>, FrameVolumes)> {
    let mut blocks: Vec<ScannedBlock> = Vec::new();
    let mut probes: Vec<RestartProbe> = Vec::new();
    let mut volumes = FrameVolumes {
        literals: 0,
        sequences: 0,
        complete: true,
    };
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
        // Every block's stored size is bounded by min(window, 128K)
        // (libzstd's header-stage blockSizeMax); a violation falls back
        // to the sequential path, which reports it.
        if size as usize > crate::common::max_block_output(window_size) {
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
        if btype == BlockType::Compressed {
            let (probe, staged) = probe_restart(&input[scan_cur + 3..body_end]);
            if let Some(v) = staged {
                volumes.literals += v.literals;
                volumes.sequences += v.sequences as u64;
            } else {
                volumes.complete = false;
            }
            probes.push(probe);
        } else {
            // Raw/RLE blocks touch no entropy state.
            probes.push(RestartProbe {
                huffman: HuffmanState::Ignore,
                ll_explicit: false,
                of_explicit: false,
                ml_explicit: false,
            });
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
    Some((blocks, probes, volumes))
}

/// Walk the input, validating the frame structure and splitting frames
/// into segments at restart points and mid-epoch block boundaries. `None`
/// means "do not parallelize": dictionary frames, malformed or truncated
/// input (the sequential path reports those properly), or too few
/// segments to be worth it.
fn scan(input: &[u8], workers: u32, max_window_size: u64) -> Option<ScanPlan> {
    let target_segment = (input.len() / (workers as usize * 2)).max(MIN_SEGMENT_INPUT);
    let mut segments: Vec<SegmentPlan> = Vec::new();
    #[cfg(feature = "hash")]
    let mut checksums: Vec<Option<u32>> = Vec::new();
    let mut frame_pledges: Vec<Option<u64>> = Vec::new();
    #[cfg(feature = "std")]
    let mut frame_staged_acc: Vec<Option<u64>> = Vec::new();
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
        #[cfg(feature = "std")]
        frame_staged_acc.push(None);
        let fcs_present = frame_header.descriptor.frame_content_size_flag() != 0
            || frame_header.descriptor.single_segment_flag();
        frame_pledges.push(fcs_present.then(|| frame_header.frame_content_size()));
        pledged_single = if n_frames == 1 && fcs_present && !saw_skip {
            Some(frame_header.frame_content_size())
        } else {
            None
        };
        if frame_header.dictionary_id().is_some() {
            return None; // dictionary state cannot be split across segments
        }
        let window_size = frame_header.window_size().ok()?;
        if window_size > max_window_size {
            return None; // sequential path rejects with the proper error
        }
        let window_size = window_size as usize;
        let checksummed = frame_header.descriptor.content_checksum_flag();
        let blocks_start = input.len() - reader.len();

        let (blocks, probes, volumes) = scan_frame_blocks(input, window_size, blocks_start)?;
        let restarts = restart_candidates(&probes);
        let frame_end = blocks.last()?.body.end;
        let ramp = volumes.ramp_eligible(workers, frame_end - blocks_start);
        #[cfg(feature = "std")]
        {
            let idx = frame_staged_acc.len() - 1;
            frame_staged_acc[idx] = (volumes.complete)
                .then(|| volumes.literals + SEQUENCE_RECORD_BYTES * volumes.sequences);
        }
        if checksummed {
            let trailer_end = frame_end.checked_add(4)?;
            if trailer_end > input.len() {
                return None;
            }
            #[cfg(feature = "hash")]
            checksums.push(Some(u32::from_le_bytes([
                input[frame_end],
                input[frame_end + 1],
                input[frame_end + 2],
                input[frame_end + 3],
            ])));
            cursor = trailer_end;
        } else {
            #[cfg(feature = "hash")]
            checksums.push(None);
            cursor = frame_end;
        }

        // One fold walk serves the whole frame: the tracked table epoch
        // resets at restart points, updates per block, and snapshots into
        // every cut boundary that is not itself a restart point.
        let cuts = plan_frame_cuts(
            &blocks,
            &restarts,
            target_segment,
            frame_end - blocks_start,
            blocks_start,
            ramp,
        );
        let mut fold = EpochFold::default();
        let mut open_start = 0usize;
        let mut open_prime: Option<TablePrime> = None;
        let mut cuts_iter = cuts.iter().copied();
        let mut next_cut = cuts_iter.next();
        for i in 0..blocks.len() {
            if i > 0 && restarts.binary_search(&i).is_ok() {
                fold.reset();
            }
            if next_cut == Some(i) {
                segments.push(SegmentPlan {
                    blocks: blocks[open_start..i].to_vec(),
                    frame_start: open_start == 0,
                    window_size,
                    prime: open_prime.take(),
                });
                open_start = i;
                // A restart cut starts fresh; a mid-epoch cut carries the
                // fold snapshot as of this boundary (blocks before i).
                open_prime = (restarts.binary_search(&i).is_err()).then(|| fold.snapshot(&blocks));
                next_cut = cuts_iter.next();
            }
            fold.observe(i, &probes[i]);
        }
        segments.push(SegmentPlan {
            blocks: blocks[open_start..].to_vec(),
            frame_start: open_start == 0,
            window_size,
            prime: open_prime,
        });
    }
    let plan = ScanPlan {
        segments,
        #[cfg(feature = "hash")]
        checksums,
        frame_pledges,
        pledged_single,
        #[cfg(feature = "std")]
        frame_staged: frame_staged_acc,
    };
    // A high-ratio frame (compressed input under the MT floor, pledge in
    // the piece regime) legitimately yields a single segment: stage A on a
    // sub-floor input is negligible either way and the piece cut is
    // output-gridded, not segment-gridded. Larger inputs keep the
    // two-segment bar so the piece router never stages a big single-segment
    // plan just to fail validation.
    if plan.segments.len() >= 2
        || (input.len() < MIN_MT_INPUT && super::mt_pieces::gate(&plan).is_some())
    {
        Some(plan)
    } else {
        None
    }
}

/// Re-establish the carried tables a mid-epoch start needs. The Huffman
/// axis replays one Compressed-literals description: the build is a pure
/// function of the description bytes and the describing block's own sizes
/// (which fix the X1/X2 shape), so it reproduces the in-sequence state
/// exactly. The FSE axes replay the last explicit description per stream,
/// in block order: a replayed block's other streams are carried (Repeat
/// is a no-op) or hold an older description that a later replay
/// overwrites, leaving each stream at its own last description. Every
/// replayed block belongs to an earlier segment whose staging decodes the
/// same bytes through the same functions, so a malformed description
/// errors that earlier segment first in the ordered consumer and the
/// primed segment's error is never observed.
fn prime_tables(
    input: &[u8],
    prime: &TablePrime,
    scratch: &mut SegmentScratch,
) -> Result<(), FrameDecoderError> {
    if let Some(body) = &prime.huffman {
        let body = &input[body.clone()];
        let mut section = LiteralsSection::new();
        let header_len = section
            .parse_from_header(body)
            .map_err(DecompressBlockError::from)
            .map_err(block_body_err)? as usize;
        let literals_len = literals_payload_len(&section);
        super::literals_section_decoder::prepare_section_table(
            LiteralsSectionType::Compressed,
            section.regenerated_size as usize,
            &mut scratch.huf,
            &body[header_len..header_len + literals_len],
        )
        .map_err(DecompressBlockError::from)
        .map_err(block_body_err)?;
    }
    let mut fse = [prime.ll.clone(), prime.of.clone(), prime.ml.clone()];
    fse.sort_unstable_by_key(|r| r.as_ref().map_or(0, |r| r.start));
    let mut replayed = usize::MAX;
    for body in fse.into_iter().flatten() {
        if body.start == replayed {
            continue; // one block can describe several streams
        }
        replayed = body.start;
        let body = &input[body];
        let mut section = LiteralsSection::new();
        let header_len = section
            .parse_from_header(body)
            .map_err(DecompressBlockError::from)
            .map_err(block_body_err)? as usize;
        let seq_raw_all = &body[header_len + literals_payload_len(&section)..];
        let mut seq_header = SequencesHeader::new();
        let seq_header_len = seq_header
            .parse_from_header(seq_raw_all)
            .map_err(DecompressBlockError::from)
            .map_err(block_body_err)? as usize;
        super::sequence_section_decoder::maybe_update_fse_tables(
            &seq_header,
            &seq_raw_all[seq_header_len..],
            &mut scratch.fse,
        )
        .map_err(DecompressBlockError::from)
        .map_err(block_body_err)?;
    }
    Ok(())
}

/// Stage A: decode one segment's literals and sequences into staging.
pub(super) fn decode_segment(
    input: &[u8],
    plan: &SegmentPlan,
    scratch: &mut SegmentScratch,
) -> Result<DecodedSegment, FrameDecoderError> {
    scratch.reset_tables();
    if let Some(prime) = &plan.prime {
        prime_tables(input, prime, scratch)?;
    }
    let staging = staging_take();
    let (mut literals, mut sequences) = (staging.literals, staging.sequences);
    // Same per-block output cap the sequential paths enforce (libzstd
    // blockSizeMax): stage A runs before the output buffer is sized, so an
    // unchecked block_out would drive `place` into a huge allocation.
    let block_out_max = crate::common::max_block_output(plan.window_size);
    let output_too_large =
        || block_body_err(DecompressBlockError::BlockOutputTooLarge { max: block_out_max });
    let mut out_size = 0usize;
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for blk in &plan.blocks {
        match blk.btype {
            BlockType::Raw => {
                if blk.body.len() > block_out_max {
                    return Err(output_too_large());
                }
                // Saturating: 32-bit targets must not wrap the segment
                // total (each block is capped at the block maximum, the
                // block count is not); a wrapped total would size the
                // stage-B output short while every per-block cap passed.
                // Saturation instead fails the region sizing cleanly.
                out_size = out_size.saturating_add(blk.body.len());
                blocks.push(BlockPlan::Raw {
                    body: blk.body.clone(),
                    out: blk.body.len(),
                });
            },
            BlockType::RLE => {
                if blk.raw_size as usize > block_out_max {
                    return Err(output_too_large());
                }
                out_size = out_size.saturating_add(blk.raw_size as usize);
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
                if section.regenerated_size as usize > block_out_max {
                    // Reject before decode_literals sizes the staging buffer
                    // from the claimed size (an RLE section can claim up to
                    // 1 MiB from one payload byte).
                    return Err(output_too_large());
                }
                let literals_len = literals_payload_len(&section);
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
                    // Saturating: 32-bit targets must not wrap the untrusted
                    // Σml (a hostile block can claim ~2^33 match bytes); a
                    // wrapped sum would slip past the block cap below.
                    let match_bytes: usize = sequences[seqs_start..]
                        .iter()
                        .map(|s| s.ml as usize)
                        .fold(0usize, usize::saturating_add);
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
                if block_out > block_out_max {
                    return Err(output_too_large());
                }
                out_size = out_size.saturating_add(block_out);
                blocks.push(BlockPlan::Compressed {
                    lits: lits_start..literals.len(),
                    seqs: seqs_start..sequences.len(),
                    out: block_out,
                });
            },
            BlockType::Reserved => unreachable!("scan rejects reserved blocks"),
        }
    }
    // 16 zero tail bytes: stage B's unconditional 16-byte literal copies
    // overread up to 15 bytes past a block's staged literals, which stays
    // inside the segment buffer (the next block's literals, or this tail).
    literals.extend_from_slice(&[0; 16]);
    Ok(DecodedSegment {
        literals,
        sequences,
        plan: blocks,
        out_size,
    })
}

/// The output region stage B executes into. Absolute output positions
/// index from `base`; `write_limit` is the absolute end of the memory the
/// writes may overshoot into by up to 16 bytes (wildcopy slack: the
/// caller's slice end, or the vector's capacity — the slack bytes are
/// overwritten in address order before anything can read them, same
/// contract as the flat executor); `wild_cap` additionally bounds how far
/// past its own end a copy may overshoot (`usize::MAX` when only the
/// memory end matters, a parallel piece's end so pieces never spill into a
/// neighbor's range). All the stage-B wildcopy-slack arithmetic lives in
/// [`OutRegion::wild_ok`].
#[derive(Clone, Copy)]
pub(super) struct OutRegion {
    base: NonNull<u8>,
    write_limit: usize,
    wild_cap: usize,
}

impl OutRegion {
    /// # Safety
    /// `base` must be non-null and valid for writes in `[0, write_limit)`
    /// and for reads of the final output below the executed watermark, as
    /// the executor contracts state.
    pub(super) unsafe fn new(base: *mut u8, write_limit: usize, wild_cap: usize) -> Self {
        // SAFETY: the caller's contract provides a valid, non-null base
        unsafe {
            OutRegion {
                base: NonNull::new_unchecked(base),
                write_limit,
                wild_cap,
            }
        }
    }

    pub(super) fn base(&self) -> *mut u8 {
        self.base.as_ptr()
    }
}

/// Stage B: execute one staged segment at `region.base()[seg_start..seg_start +
/// out_size)`, reading match sources at absolute positions below
/// `seg_start` (final output of earlier segments; stage B runs in order).
/// `frame_base` is the owning frame's absolute output start: match sources
/// may not reach below it (each frame is an independent history, as in
/// `ZSTD_decompress`'s per-frame window).
///
/// # Safety
/// `region.base()` must be valid for writes in `[seg_start, seg_start +
/// out_size)` plus 16 bytes of slack below the region's bounds, and for
/// reads in `[0, seg_start)`. The caller guarantees both by sizing the
/// output before each segment and executing in order.
pub(super) unsafe fn execute_segment(
    region: OutRegion,
    seg_start: usize,
    frame_base: usize,
    seg: &DecodedSegment,
    input: &[u8],
    offset_hist: &mut [u32; 3],
) -> Result<(), FrameDecoderError> {
    // SAFETY: forwards to `execute_blocks` under the caller's contract
    // documented above.
    unsafe {
        execute_blocks(
            region,
            seg_start,
            frame_base,
            seg,
            0..seg.plan.len(),
            input,
            offset_hist,
        )
    }
}

/// Execute a contiguous block range of a staged segment starting at
/// absolute output position `start` (the first block's own position).
/// `frame_base` is the owning frame's absolute output start; offsets must
/// stay within the frame (see `execute_segment`).
///
/// # Safety
/// `region.base()` must be valid for writes over the blocks' combined
/// output plus 16 bytes of slack below the region's bounds, and for reads
/// in `[0, start)`. `offset_hist` must hold the true repcode history at
/// `start`.
pub(super) unsafe fn execute_blocks(
    region: OutRegion,
    start: usize,
    frame_base: usize,
    seg: &DecodedSegment,
    blocks: Range<usize>,
    input: &[u8],
    offset_hist: &mut [u32; 3],
) -> Result<(), FrameDecoderError> {
    let exec_err =
        |e: ExecuteSequencesError| block_body_err(DecompressBlockError::ExecuteSequencesError(e));
    // Saturating: same 32-bit wrap concern as stage A's out_size; the
    // budget checks below read this cap directly.
    let cap: usize = seg.plan[blocks.clone()]
        .iter()
        .map(BlockPlan::out)
        .fold(0usize, usize::saturating_add);
    let base = region.base();
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
                // The sequence loop ports the ST executor's libzstd shape
                // (see `execute_decoded_flat_inner`): repcodes resolved in
                // locals off the staged wire value, one combined check that
                // sends every edge case to the out-of-line exact path, and
                // unconditional 16-byte copies on the hot path.
                // SAFETY: lits is a range of seg.literals
                let lit_base = unsafe { seg.literals.as_ptr().add(lits.start) };
                // SAFETY: as above, `lits.end` is inside `seg.literals`.
                let lit_end = unsafe { seg.literals.as_ptr().add(lits.end) };
                let mut lit = lit_base as usize;
                // The repcode history lives in locals for the block run and
                // is written back below; an error aborts the whole decode,
                // so the skipped write-back on the error paths is
                // unobservable.
                let mut rep0 = offset_hist[0] as usize;
                let mut rep1 = offset_hist[1] as usize;
                let mut rep2 = offset_hist[2] as usize;
                // Both wildcopy bounds (`OutRegion`'s write limit and wild
                // cap) are loop-invariant, so their min minus the 16-byte
                // overshoot is the hot path's single end check.
                let oend_w = region.write_limit.min(region.wild_cap).saturating_sub(16);
                // Stage A accounted the exact block budget; the cold path
                // re-checks it (the hot check against `oend_w` covers the
                // memory bounds, not the accounting). The region's write
                // limit folds in for executors that do not `place` each
                // segment (the frame lanes): every serial and piece caller
                // has write_limit >= budget, so this changes nothing there.
                let budget_end = (start + cap).min(region.write_limit);
                let mut op = start + w;
                for seq in &seg.sequences[seqs.clone()] {
                    let ll = seq.ll as usize;
                    let ml = seq.ml as usize;
                    // Offset resolution in libzstd's decode order: wire
                    // values above 3 are real offsets carrying the +3
                    // repcode bias, 1..=3 are repcode selectors (the mapping
                    // `do_offset_history` pins for its own callers). The
                    // resolution owns the zero-offset boundary: wire values
                    // past the bias are provably nonzero, a repcode
                    // resolving to zero is the malformed-history bug class
                    // rejected right here, and the copy arithmetic below
                    // takes the offset as NonZero.
                    let of = seq.of as usize;
                    let resolved = if of > 3 {
                        let actual = of - 3;
                        rep2 = rep1;
                        rep1 = rep0;
                        rep0 = actual;
                        debug_assert!(actual != 0);
                        // SAFETY: of > 3 proves actual = of - 3 >= 1.
                        Some(unsafe { NonZero::<usize>::new_unchecked(actual) })
                    } else if of == 1 {
                        // Code 0: rep0, or rep1 when the sequence carries
                        // no literals.
                        let ll0 = usize::from(ll == 0);
                        let temp = if ll0 != 0 {
                            rep1
                        } else {
                            rep0
                        };
                        rep1 = if ll0 != 0 {
                            rep0
                        } else {
                            rep1
                        };
                        rep0 = temp;
                        NonZero::new(temp)
                    } else {
                        // Codes 1 (wire 2..3): the low bit extends the
                        // selector to rep1, rep2 or the rep0-1 pseudo slot.
                        let ll0 = usize::from(ll == 0);
                        let idx = of - 1 + ll0;
                        let temp = match idx {
                            1 => rep1,
                            2 => rep2,
                            // A malformed dictionary can seed rep0 with 0;
                            // saturate so this lands on the ZeroOffset
                            // rejection below.
                            _ => rep0.saturating_sub(1),
                        };
                        if idx != 1 {
                            rep2 = rep1;
                        }
                        rep1 = rep0;
                        rep0 = temp;
                        NonZero::new(temp)
                    };
                    let Some(offset) = resolved else {
                        return Err(exec_err(ExecuteSequencesError::ZeroOffset));
                    };
                    let o_lit_end = op + ll;
                    let o_match_end = o_lit_end + ml;
                    let i_lit_end = lit + ll;
                    if i_lit_end > lit_end as usize || o_match_end > oend_w {
                        // SAFETY: the region contract covers writes over
                        // the block budget plus its 16-byte slack; the cold
                        // path re-checks every bound before copying.
                        op = unsafe {
                            exec_staged_end(
                                base,
                                op,
                                ll,
                                ml,
                                offset,
                                &mut lit,
                                lit_base as usize,
                                lit_end as usize,
                                budget_end,
                                frame_base,
                            )
                        }
                        .map_err(exec_err)?;
                        continue;
                    }
                    // SAFETY: the combined check keeps every 16-byte
                    // overshoot inside the region's bounds, and the staged
                    // literals carry a 16-byte zero tail so the source
                    // overread stays initialized (a zero-length run writes
                    // garbage the match overwrites, in address order).
                    unsafe {
                        copy16_chunks(base.add(op), lit as *const u8, ll);
                    }
                    lit = i_lit_end;
                    // Whole-frame history, one-shot semantics — but the
                    // frame's own output only: a concatenated stream's
                    // frames never share history.
                    if offset.get() > o_lit_end - frame_base {
                        return Err(exec_err(ExecuteSequencesError::DecodebufferError(
                            crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                                offset: offset.get(),
                                buf_len: o_lit_end - frame_base,
                            },
                        )));
                    }
                    // SAFETY: the offset check keeps the match source inside
                    // the frame's committed output, and the combined check
                    // covers the wildcopy overshoot. `ml` is always at least
                    // 3 (the staged match-length code floor), so the
                    // sub-16-offset path's 8-byte writes stay inside it.
                    unsafe {
                        let dst = base.add(o_lit_end);
                        wildcopy_match(dst, dst.sub(offset.get()), ml);
                    }
                    op = o_match_end;
                }
                offset_hist[0] = rep0 as u32;
                offset_hist[1] = rep1 as u32;
                offset_hist[2] = rep2 as u32;
                let rest = lit_end as usize - lit;
                if op + rest > budget_end {
                    return Err(exec_err(ExecuteSequencesError::TargetTooSmall));
                }
                // SAFETY: budget checked above; plain copy, no overshoot
                unsafe {
                    core::ptr::copy_nonoverlapping(lit as *const u8, base.add(op), rest);
                }
                op += rest;
                w = op - start;
            },
        }
    }
    debug_assert_eq!(w, cap, "stage A size accounting must match execution");
    Ok(())
}

/// Slow path for staged sequences near the wildcopy limit or past their
/// literals: exact copies with the bounds re-checked in the staged
/// executor's order (the ST loop's `exec_sequence_end` shape). The offset
/// arrives resolved; the `NonZero` type is the hot loop's zero check
/// carried into the signature.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
unsafe fn exec_staged_end(
    base: *mut u8,
    op: usize,
    ll: usize,
    ml: usize,
    offset: NonZero<usize>,
    lit: &mut usize,
    lit_base: usize,
    lit_end: usize,
    budget_end: usize,
    frame_base: usize,
) -> Result<usize, ExecuteSequencesError> {
    let o_lit_end = op + ll;
    let o_match_end = o_lit_end + ml;
    if o_match_end > budget_end {
        return Err(ExecuteSequencesError::TargetTooSmall);
    }
    if *lit + ll > lit_end {
        return Err(ExecuteSequencesError::NotEnoughBytesForSequence {
            wanted: *lit - lit_base + ll,
            have: lit_end - lit_base,
        });
    }
    // SAFETY: both ranges were bounds-checked above
    unsafe { core::ptr::copy_nonoverlapping(*lit as *const u8, base.add(op), ll) };
    *lit += ll;
    if offset.get() > o_lit_end - frame_base {
        return Err(ExecuteSequencesError::DecodebufferError(
            crate::decoding::errors::DecodeBufferError::OffsetTooBig {
                offset: offset.get(),
                buf_len: o_lit_end - frame_base,
            },
        ));
    }
    // SAFETY: the source sits `offset` bytes behind the destination inside
    // the frame's committed output; doubling chunks anchored at the match
    // source keep reads behind the write cursor.
    unsafe {
        let dst = base.add(o_lit_end);
        let src = dst.sub(offset.get());
        let mut copied = 0;
        while copied < ml {
            let chunk = (offset.get() + copied).min(ml - copied);
            core::ptr::copy(src, dst.add(copied), chunk);
            copied += chunk;
        }
    }
    Ok(o_match_end)
}

/// Copy `len` bytes in 16-byte chunks, overshooting by up to 15 bytes (the
/// caller guarantees the overshoot stays inside the write bound and the
/// read bound).
#[inline(always)]
unsafe fn copy16_chunks(mut d: *mut u8, mut s: *const u8, len: usize) {
    // SAFETY: the caller guarantees `len` readable bytes at `s` and `len`
    // writable bytes at `d`, keeping the 16-byte chunk overshoot inside
    // both bounds.
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

/// Verify every frame's executed output length against its declared
/// Frame_Content_Size (the sequential paths run the same check at the frame
/// tail, which the parallel executors never see). Frames decode
/// sequentially, so a frame's output is the gap between consecutive frame
/// starts (`starts`, aligned with `pledges` in scan order); frames without
/// a pledge are skipped.
pub(super) fn verify_frame_content_sizes(
    pledges: &[Option<u64>],
    starts: &[usize],
    total: usize,
) -> Result<(), FrameDecoderError> {
    debug_assert_eq!(pledges.len(), starts.len(), "every frame records one start");
    for (i, pledged) in pledges.iter().enumerate() {
        let Some(declared) = *pledged else { continue };
        let end = starts.get(i + 1).copied().unwrap_or(total);
        let actual = (end - starts[i]) as u64;
        if actual != declared {
            return Err(FrameDecoderError::ContentSizeMismatch { declared, actual });
        }
    }
    Ok(())
}

/// Per-call state shared between the calling thread (stage B) and the
/// leased pool workers (stage A). The input and segment plans are frozen
/// borrows: a worker touches them only while serving the lease, and
/// `decode_parallel` waits every lease out before returning, so the
/// borrows outlive the freeze (the encoder-side `FrozenSrc` contract).
pub(super) struct StageAJob {
    input: *const [u8],
    segments: *const [SegmentPlan],
    n_segments: usize,
    /// Leased worker count: the staging backpressure bound, so staging
    /// memory stays proportional to the worker count, not the input.
    threads: usize,
    pub(super) slots: Vec<Mutex<Option<Result<DecodedSegment, FrameDecoderError>>>>,
    pub(super) ready: Condvar,
    next_job: AtomicUsize,
    consumed: AtomicUsize,
    /// Executed-segment count (any lane), the lane mode's backpressure
    /// bound: lanes consume out of input order, so the prefix counter above
    /// does not describe them.
    pub(super) consumed_count: AtomicUsize,
    /// `Some(window)` in lane mode: stage at most `window` segments beyond
    /// the executed count. `None` keeps the serial prefix bound.
    lane_window: Option<usize>,
    pub(super) abort: AtomicBool,
    /// Worker panic payload; doubles as the backpressure sleep token.
    pub(super) poison: Mutex<Option<alloc::boxed::Box<dyn std::any::Any + Send>>>,
    /// Published per-segment outcome for the frame lanes (lane mode only):
    /// 0 = not staged, `out_size + 1` = staged, `usize::MAX` = stage error.
    /// The lanes read their predecessors' sizes off this array because a
    /// consumed slot cannot be distinguished from an unstaged one.
    pub(super) seg_sizes: Vec<AtomicUsize>,
}

impl StageAJob {
    /// Build the job for the multi-frame lane executor: same fields as
    /// `decode_parallel`'s, plus the lane-mode backpressure window and the
    /// size-publish array.
    pub(super) fn for_lanes(
        input: &[u8],
        segments: &[SegmentPlan],
        threads: usize,
        window: usize,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(StageAJob {
            input,
            segments,
            n_segments: segments.len(),
            threads,
            slots: (0..segments.len()).map(|_| Mutex::new(None)).collect(),
            ready: Condvar::new(),
            next_job: AtomicUsize::new(0),
            consumed: AtomicUsize::new(0),
            consumed_count: AtomicUsize::new(0),
            lane_window: Some(window),
            abort: AtomicBool::new(false),
            poison: Mutex::new(None),
            seg_sizes: segments.iter().map(|_| AtomicUsize::new(0)).collect(),
        })
    }
}

// SAFETY: the frozen slices are dereferenced only by a thread serving the
// job's lease, and every lease is waited out (`mt_pool::wait_leave`) before
// the leasing `decode_parallel` returns, inside the caller's borrow. The
// mutexes, atomics and condvar are thread-safe by construction.
unsafe impl Send for StageAJob {}
// SAFETY: Sync follows Send here: the frozen slices are read-only past
// construction, and every mutable field is a mutex, atomic or condvar.
unsafe impl Sync for StageAJob {}

/// Test injection: the input base address and segment id whose stage-A
/// decode panics instead of running (null / usize::MAX disable). Keyed to
/// one input's address so a concurrently running test's decode never trips
/// it in the shared-process test harness. Compiled out of non-test builds.
#[cfg(test)]
static STAGE_PANIC_INPUT: core::sync::atomic::AtomicPtr<u8> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
#[cfg(test)]
static STAGE_PANIC_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Sentinel arming the panic injection on the job's last segment, so a
/// test can shape the geometry where the poison fill is the final event
/// of the whole decode.
#[cfg(test)]
static STAGE_PANIC_LAST: AtomicBool = AtomicBool::new(false);

/// One lease's stage-A service on a pool worker: claim segments, stage each
/// one's literals and sequences into its slot. Returns whether the thread
/// must retire (a job panicked on it; the scratch is dropped in that case).
pub(super) fn serve_stagea(job: &StageAJob, scratch: &mut Option<SegmentScratch>) -> bool {
    // SAFETY: this thread serves the job's lease, so the frozen slices are
    // live for the whole call.
    let input: &[u8] = unsafe { &*job.input };
    // SAFETY: same lease discipline as the input slice above.
    let segments: &[SegmentPlan] = unsafe { &*job.segments };
    loop {
        if job.abort.load(Ordering::Relaxed) || job.poison.lock().unwrap().is_some() {
            return false;
        }
        let id = job.next_job.fetch_add(1, Ordering::Relaxed);
        if id >= job.n_segments {
            return false;
        }
        // Backpressure sleep while this segment runs too far ahead of the
        // consumer: beyond the consumed prefix in serial mode, beyond the
        // executed count in lane mode (lanes consume out of input order).
        // The poison mutex is only the sleep token; the predicate is atomic.
        let ahead = || match job.lane_window {
            Some(window) => id > job.consumed_count.load(Ordering::Acquire) + window,
            None => id > job.consumed.load(Ordering::Acquire) + job.threads,
        };
        while ahead() {
            if job.abort.load(Ordering::Relaxed) {
                break;
            }
            let guard = job.poison.lock().unwrap();
            let (_guard, _timeout) = job
                .ready
                .wait_timeout(guard, core::time::Duration::from_millis(50))
                .unwrap();
        }
        if job.abort.load(Ordering::Relaxed) {
            return false;
        }
        if scratch.is_none() {
            *scratch = Some(SegmentScratch::new());
        }
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if (id == STAGE_PANIC_AT.load(Ordering::Relaxed)
                || (STAGE_PANIC_LAST.load(Ordering::Relaxed) && id + 1 == job.n_segments))
                && STAGE_PANIC_INPUT.load(Ordering::Relaxed).cast_const() == input.as_ptr()
            {
                panic!("injected stage-A panic");
            }
            decode_segment(input, &segments[id], scratch.as_mut().unwrap())
        }));
        // Every filled slot must wake the ordered consumer on the way out
        // of this match, so no fill arm may return past the notify: a
        // poison fill without one parks the consumer forever once no later
        // segment completes (review 2026-11-03).
        let mut retire = false;
        match attempt {
            Ok(res) => {
                if job.lane_window.is_some() {
                    let published = match &res {
                        Ok(seg) => seg.out_size.wrapping_add(1),
                        Err(_) => usize::MAX,
                    };
                    job.seg_sizes[id].store(published, Ordering::Release);
                }
                *job.slots[id].lock().unwrap() = Some(res);
            },
            Err(payload) => {
                *job.poison.lock().unwrap() = Some(payload);
                // Release the slot so the ordered consumer can run to
                // completion; this error is never observed (resume_unwind
                // replaces it after the leases are waited out).
                *job.slots[id].lock().unwrap() = Some(Err(FrameDecoderError::NotYetInitialized));
                // The scratch may be mid-decode garbage: drop it rather
                // than reuse, and retire this thread (never reuse a thread
                // that has seen an unwind).
                *scratch = None;
                retire = true;
            },
        }
        job.ready.notify_all();
        if retire {
            return true;
        }
    }
}

/// End-of-lease guard, the join equivalent of the old `thread::scope`: any
/// exit path — stage B error, caller panic, worker panic resume — aborts
/// the workers and waits every lease out, so no worker can touch the frozen
/// input or segment plans after the borrows end.
pub(super) struct LeaseOut<'a> {
    pub(super) job: &'a StageAJob,
    pub(super) slots: &'a [(
        std::sync::Arc<super::mt_pool::WorkerSlot>,
        std::sync::Arc<super::mt_pool::LeaseJob>,
    )],
}

impl Drop for LeaseOut<'_> {
    fn drop(&mut self) {
        self.job.abort.store(true, Ordering::Release);
        self.job.ready.notify_all();
        for (slot, lease) in self.slots {
            super::mt_pool::wait_leave(slot, lease);
        }
    }
}

/// Two-stage pipeline driver. `place(start, end)` makes the output region
/// valid for absolute positions `[0, end)` and returns its base pointer
/// (called per segment, so a growing Vec may move between calls). Returns
/// the total number of decoded bytes. Stage A runs on leased pool workers
/// (see `mt_pool`), so repeated decodes pay a slot handoff instead of a
/// worker spawn+join per call.
fn decode_parallel(
    input: &[u8],
    workers: u32,
    segments: &[SegmentPlan],
    frame_pledges: &[Option<u64>],
    place: &mut dyn FnMut(usize, usize) -> Result<(*mut u8, usize), FrameDecoderError>,
    #[cfg(feature = "hash")] after_segment: &mut dyn FnMut(*const u8, Range<usize>),
) -> Result<usize, FrameDecoderError> {
    let n_segments = segments.len();
    let threads = (workers as usize).min(n_segments);
    let job = std::sync::Arc::new(StageAJob {
        input,
        segments,
        n_segments,
        threads,
        slots: (0..n_segments).map(|_| Mutex::new(None)).collect(),
        ready: Condvar::new(),
        next_job: AtomicUsize::new(0),
        consumed: AtomicUsize::new(0),
        consumed_count: AtomicUsize::new(0),
        lane_window: None,
        abort: AtomicBool::new(false),
        poison: Mutex::new(None),
        seg_sizes: Vec::new(),
    });
    let lease = std::sync::Arc::new(super::mt_pool::LeaseJob::StageA(job.clone()));
    let leased: Vec<_> = super::mt_pool::lease_workers(threads, &lease)
        .into_iter()
        .map(|s| (s, lease.clone()))
        .collect();
    let out_guard = LeaseOut {
        job: &job,
        slots: &leased,
    };

    let mut offset_hist = [1u32, 4, 8];
    let exec_result = (|| -> Result<usize, FrameDecoderError> {
        let mut written = 0usize;
        // Output offset where each frame began, in scan order.
        let mut frame_starts = Vec::with_capacity(frame_pledges.len());
        let mut frame_base = 0usize;
        for (id, seg_plan) in segments.iter().enumerate() {
            let staged = {
                let mut guard = job.slots[id].lock().unwrap();
                while guard.is_none() {
                    guard = job.ready.wait(guard).unwrap();
                }
                guard.take().unwrap()
            };
            let seg = staged?;
            if seg_plan.frame_start {
                offset_hist = [1, 4, 8];
                frame_starts.push(written);
                frame_base = written;
            }
            #[cfg(feature = "hash")]
            let seg_out_start = written;
            let (base, buf_limit) = place(written, written + seg.out_size)?;
            // SAFETY: `place` made [0, written + out_size) valid; every
            // match source below `written` is final (in-order execution).
            let region = unsafe { OutRegion::new(base, buf_limit, usize::MAX) };
            // SAFETY: region's contract above.
            unsafe {
                execute_segment(region, written, frame_base, &seg, input, &mut offset_hist)?;
            }
            written += seg.out_size;
            // The executed range is final from here on: later writes never
            // land below the executor cursor.
            #[cfg(feature = "hash")]
            after_segment(region.base().cast_const(), seg_out_start..written);
            job.consumed.store(id + 1, Ordering::Release);
            job.ready.notify_all();
        }
        verify_frame_content_sizes(frame_pledges, &frame_starts, written)?;
        Ok(written)
    })();
    // The guard is the scope-join equivalent: workers stop touching the
    // frozen input and segment plans before any panic resumes.
    drop(out_guard);
    if let Some(payload) = job.poison.lock().unwrap().take() {
        std::panic::resume_unwind(payload);
    }
    exec_result
}

/// Below [`MIN_MT_INPUT`] the compressed size says nothing about the decode
/// work: a high-ratio frame (text 32 MiB -> ~100 KB compressed) would never
/// parallelize. Peek the leading frame header and let the scan run when a
/// single pledged frame reaches the piece executor's size regime; the scan
/// cost stays proportional to the compressed bytes.
fn pledges_piece_frame(input: &[u8]) -> bool {
    let mut reader = input;
    match frame::read_frame_header(&mut reader) {
        Ok((header, _)) => {
            (header.descriptor.frame_content_size_flag() != 0
                || header.descriptor.single_segment_flag())
                && header.frame_content_size() >= super::mt_pieces::MIN_PIECE_FRAME as u64
        },
        Err(_) => false,
    }
}

fn engage(input: &[u8], workers: u32, max_window_size: u64) -> Option<ScanPlan> {
    if workers < 2 || std::thread::available_parallelism().map_or(true, |n| n.get() < 2) {
        return None;
    }
    if input.len() < MIN_MT_INPUT && !pledges_piece_frame(input) {
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
/// processes and inputs without a second segment.
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
        // Multi-frame input: stage B runs one lane per frame (grouped when
        // frames outnumber the worker budget) on the same pool. The fixed
        // buffer is valid for its whole length up front, so the lanes start
        // immediately and staging overlaps execution as before.
        let n_frames = plan.segments.iter().filter(|s| s.frame_start).count();
        if n_frames >= 2 {
            // A too-small target surfaces from the executor's own
            // write-limit checks.
            let out_len = output.len();
            let written = super::mt_frames::decode_frames_fixed(
                input,
                workers,
                &plan,
                output.as_mut_ptr(),
                out_len,
            )?;
            return Ok(written);
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
            &plan.frame_pledges,
            &mut place,
            &mut after_segment,
        )?;
        #[cfg(feature = "hash")]
        if let Some(err) = checksum.and_then(|mut c| c.take_result()) {
            return Err(err);
        }
        #[cfg(not(feature = "hash"))]
        let written = decode_parallel(
            input,
            workers,
            &plan.segments,
            &plan.frame_pledges,
            &mut place,
        )?;
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
        // Multi-frame input: the growing vector's base may move on growth,
        // so staging completes first and one place call covers the total
        // before any lane writes (the piece executor's staging model).
        let n_frames = plan.segments.iter().filter(|s| s.frame_start).count();
        if n_frames >= 2 {
            let start_len = output.len();
            let mut place =
                |_start: usize, end: usize| -> Result<(*mut u8, usize), FrameDecoderError> {
                    output.reserve(end);
                    // SAFETY: in-bounds pointer one past the existing
                    // contents, inside the vector's allocation.
                    Ok((
                        unsafe { output.as_mut_ptr().add(start_len) },
                        output.capacity() - start_len,
                    ))
                };
            let written =
                super::mt_frames::decode_frames_growing(input, workers, &plan, &mut place)?;
            // SAFETY: every byte in [start_len, start_len + written) was
            // written by the lanes.
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
            &plan.frame_pledges,
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
        let written = decode_parallel(
            input,
            workers,
            &plan.segments,
            &plan.frame_pledges,
            &mut place,
        )?;
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

    /// The injection atomics are process-global, so the injection tests
    /// must not overlap: a concurrent test's `STAGE_PANIC_INPUT` store
    /// would re-key the other's injection to its own input and the probe
    /// decode would run clean (the "must surface" assert then fires on a
    /// healthy decode). The guard spans store-to-restore in every test
    /// that arms the injection.
    static INJECT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// Eight MiB of semi-structured records: libzstd emits Huffman-coded
    /// literals in back-to-back blocks, so segments stage several Huffman
    /// sections into one accumulated literals buffer (the absolute-length
    /// check in `decompress_literals` used to reject the second one), and
    /// the -9 frame carries table epochs far longer than the segment
    /// target (the mid-epoch priming class).
    fn semi_structured_records() -> Vec<u8> {
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
        data
    }

    /// The records frame decodes byte-exact through both parallel entry
    /// points at every level.
    #[test]
    fn zstd_crate_output_decodes_semi_structured() {
        let data = semi_structured_records();
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
    /// segment (single small frame) fall back byte-exact.
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

    use super::{
        BlockPlan, DecodedSegment, FrameVolumes, HuffmanState, RAMP_HEAD_INPUT, RestartProbe,
        ScannedBlock, SegmentPlan, SegmentScratch, TablePrime, decode_segment, plan_frame_cuts,
        probe_restart, restart_candidates, scan, scan_frame_blocks,
    };
    use crate::blocks::block::BlockType;

    /// A single frame carrying its declared content size (the one-shot
    /// compressors emit no FCS field).
    fn pledged_frame(data: &[u8]) -> Vec<u8> {
        use crate::io::Write;
        let mut sink = Vec::new();
        let mut enc = crate::stream::write::Encoder::with_options(
            &mut sink,
            EncoderOptions::new(Level::Fastest).pledged_size(Some(data.len() as u64)),
        )
        .unwrap();
        enc.write_all(data).unwrap();
        enc.finish().unwrap();
        sink
    }

    /// Byte offset of a pledged frame's Frame_Content_Size field (our
    /// encoder always emits the window descriptor and no dictionary id, so
    /// the field starts right behind them).
    fn fcs_at(frame: &[u8]) -> usize {
        let fhd = frame[4];
        assert_eq!(fhd & 3, 0, "no dictionary id expected");
        assert_eq!((fhd >> 5) & 1, 0, "no single-segment flag expected");
        assert!(fhd >> 6 != 0, "no FCS field present");
        5 + 1
    }

    /// A frame's declared Frame_Content_Size is a contract on every path:
    /// the parallel executor never sees a frame tail, so a two-frame input
    /// whose first frame's pledge is patched one byte high must fail exactly
    /// like the sequential decoder.
    #[test]
    fn content_size_mismatch_is_an_error() {
        let a = textish(3 * 1024 * 1024);
        let b: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 61) as u8).collect();
        let mut frame_a = pledged_frame(&a);
        let at = fcs_at(&frame_a);
        frame_a[at] += 1;
        let mut input = frame_a;
        input.extend_from_slice(&pledged_frame(&b));

        // Sequential baseline: the mismatch is reported.
        let mut out = vec![0u8; a.len() + b.len()];
        assert!(matches!(
            FrameDecoder::new().decode_all(input.as_slice(), &mut out),
            Err(crate::decoding::errors::FrameDecoderError::ContentSizeMismatch {
                declared,
                actual
            }) if declared == a.len() as u64 + 1 && actual == a.len() as u64
        ));

        // Both parallel entry points must agree, frame-scoped: the second
        // frame's untouched pledge is not what trips.
        let err = decode_to_vec_mt(&input, &mut Vec::new(), 2, MAX_WINDOW).unwrap_err();
        assert!(matches!(
            err,
            crate::decoding::errors::FrameDecoderError::ContentSizeMismatch {
                declared,
                actual
            } if declared == a.len() as u64 + 1 && actual == a.len() as u64
        ));
        let mut placed = vec![0u8; a.len() + b.len() + 1];
        let err = decode_all_mt(&input, &mut placed, 2, MAX_WINDOW).unwrap_err();
        assert!(matches!(
            err,
            crate::decoding::errors::FrameDecoderError::ContentSizeMismatch { .. }
        ));

        // Control: the unpatched pair decodes through the parallel path.
        let mut clean = pledged_frame(&a);
        clean.extend_from_slice(&pledged_frame(&b));
        let mut expect = a.clone();
        expect.extend_from_slice(&b);
        let mut vec_out = Vec::new();
        decode_to_vec_mt(&clean, &mut vec_out, 2, MAX_WINDOW).unwrap();
        assert_eq!(vec_out, expect);
    }

    /// One sequence with all three FSE streams in RLE mode (ll code 4 = 4
    /// literals, of code 0 = offset 1, ml code 0 = match 3): explicit tables
    /// without Repeat, empty bitstream carrying only the mandatory end
    /// marker.
    /// One sequence with all three FSE streams in RLE mode (ll code 4 = 4
    /// literals, of code 0 = offset 1, ml code 0 = match 3): explicit tables
    /// without Repeat, empty bitstream carrying only the mandatory end
    /// marker.
    fn rle_seq_body() -> Vec<u8> {
        vec![0x01, 0x54, 0x04, 0x00, 0x00, 0x01]
    }

    /// One sequence with `modes` and the RLE description bytes `descs`,
    /// carrying only the mandatory end marker (the carried or described
    /// tables read zero add bits). `modes` packs LL/OF/ML as 2-bit fields
    /// high to low with the reserved low bits zero.
    fn seq_body(modes: u8, descs: &[u8]) -> Vec<u8> {
        let mut b = vec![0x01, modes];
        b.extend_from_slice(descs);
        b.push(0x01);
        b
    }

    fn probe(huffman: HuffmanState, ll: bool, of: bool, ml: bool) -> RestartProbe {
        RestartProbe {
            huffman,
            ll_explicit: ll,
            of_explicit: of,
            ml_explicit: ml,
        }
    }

    /// The pre-scan must classify literals sections by how they treat the
    /// Huffman table and record per-stream FSE explicitness: a Repeat
    /// stream carries state across a cut, and a zero-sequence block leaves
    /// every stream untouched. These four axes are the table-epoch fold.
    #[test]
    fn probe_restart_classifies_entropy_state() {
        // Compressed literals (direct weights [1, 1]) rebuild the table.
        let mut compressed = vec![0x82, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01];
        compressed.extend_from_slice(&rle_seq_body());
        let (p, staged) = probe_restart(&compressed);
        assert_eq!(p.huffman, HuffmanState::Rebuild);
        assert!(p.fse_rebuilt() && p.ll_explicit && p.of_explicit && p.ml_explicit);
        assert_eq!(staged.map(|v| (v.literals, v.sequences)), Some((8, 1)));

        // Raw literals ignore the table but leave it intact.
        let mut raw = vec![0x20, 0x30, 0x31, 0x32, 0x33];
        raw.extend_from_slice(&rle_seq_body());
        let (p, staged) = probe_restart(&raw);
        assert_eq!(p.huffman, HuffmanState::Ignore);
        assert!(p.fse_rebuilt());
        assert_eq!(staged.map(|v| (v.literals, v.sequences)), Some((4, 1)));

        // RLE literals: same.
        let mut rle = vec![0x21, 0xaa];
        rle.extend_from_slice(&rle_seq_body());
        let (p, _) = probe_restart(&rle);
        assert_eq!(p.huffman, HuffmanState::Ignore);
        assert!(p.fse_rebuilt());

        // Treeless literals decode through the carried table.
        let mut treeless = vec![0x43, 0x40, 0x00, 0x1f];
        treeless.extend_from_slice(&rle_seq_body());
        let (p, _) = probe_restart(&treeless);
        assert_eq!((p.huffman, p.fse_rebuilt()), (HuffmanState::Reuse, true));

        // Compressed literals mid-epoch with all-Repeat FSE: the Huffman
        // axis re-establishes while every FSE stream carries (modes 0xFC).
        let mut repeat = vec![0x82, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01];
        repeat.extend_from_slice(&seq_body(0xfc, &[]));
        let (p, _) = probe_restart(&repeat);
        assert_eq!(p.huffman, HuffmanState::Rebuild);
        assert!(!p.ll_explicit && !p.of_explicit && !p.ml_explicit);
        assert!(!p.fse_rebuilt());

        // Partial FSE rebuilds: a single explicit stream per block (LL-only
        // 0x7C, OF-only 0xDC, ML-only 0xF4), each re-described through one
        // RLE byte while the other two carry.
        let ll_only = seq_body(0x7c, &[0x04]);
        let of_only = seq_body(0xdc, &[0x00]);
        let ml_only = seq_body(0xf4, &[0x00]);
        for (body, stream) in [(&ll_only[..], 0), (&of_only[..], 1), (&ml_only[..], 2)] {
            let mut blk = vec![0x82, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01];
            blk.extend_from_slice(body);
            let (p, _) = probe_restart(&blk);
            assert_eq!(p.huffman, HuffmanState::Rebuild);
            assert_eq!(
                (p.ll_explicit, p.of_explicit, p.ml_explicit),
                match stream {
                    0 => (true, false, false),
                    1 => (false, true, false),
                    _ => (false, false, true),
                }
            );
        }

        // A zero-sequence block leaves the FSE tables untouched, even
        // with Compressed literals rebuilding the Huffman table (the
        // trailing 0x00 is the whole sequence section: zero sequences).
        let zseq = [0x82, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01, 0x00];
        let (p, _) = probe_restart(&zseq);
        assert_eq!(p.huffman, HuffmanState::Rebuild);
        assert!(!p.fse_rebuilt());
        let (p, staged) = probe_restart(&[0x20, 0x30, 0x31, 0x32, 0x33, 0x00]);
        assert_eq!(p.huffman, HuffmanState::Ignore);
        assert!(!p.fse_rebuilt());
        assert_eq!(staged.map(|v| (v.literals, v.sequences)), Some((4, 0)));
    }

    /// A Raw/RLE-literals cut is legal only while no Treeless block precedes
    /// the next table rebuild: stage A resets the tables, and a Treeless
    /// block behind the cut would decode through the reset table. Treeless
    /// and Repeat-carrying blocks are never cuts.
    #[test]
    fn restart_candidates_respect_huffman_carry() {
        use HuffmanState::{Ignore, Rebuild, Reuse};

        // Everything rebuilt: cut anywhere explicit.
        let probes = [
            probe(Rebuild, true, true, true),
            probe(Ignore, false, false, false),
            probe(Ignore, true, true, true),
        ];
        assert_eq!(restart_candidates(&probes), vec![0, 2]);

        // Review's failing shape: Raw-literals candidate followed by a
        // Treeless block before any rebuild - no cut allowed at 1.
        let probes = [
            probe(Rebuild, true, true, true),
            probe(Ignore, false, false, false),
            probe(Ignore, true, true, true),
            probe(Reuse, true, true, true),
        ];
        assert_eq!(restart_candidates(&probes), vec![0]);

        // Treeless behind the cut only after a rebuild: cut stands. Cutting
        // after the Treeless block is legal again (it sits in the previous
        // segment).
        let probes = [
            probe(Ignore, true, true, true),
            probe(Rebuild, true, true, true),
            probe(Reuse, true, true, true),
            probe(Ignore, true, true, true),
        ];
        assert_eq!(restart_candidates(&probes), vec![0, 1, 3]);

        // Treeless first: candidates before any rebuild stay forbidden (a
        // cut at 0 would leave the Treeless block on reset tables). The
        // block after the Treeless one is cuttable again - and the frame
        // start before it is no candidate, so that unsound span only ever
        // decodes as a whole frame from frame-start state, where a legal
        // frame's Treeless block is preceded by its rebuild anyway and an
        // illegal one fails identically in both paths.
        let probes = [
            probe(Ignore, true, true, true),
            probe(Reuse, true, true, true),
            probe(Ignore, true, true, true),
        ];
        assert_eq!(restart_candidates(&probes), vec![2]);
    }

    /// A legal frame whose second segment would start at a Raw-literals
    /// block (explicit FSE tables, no Huffman rebuild) followed by a
    /// Treeless-literals block: the segment cut used to report
    /// UninitializedHuffmanTable on every worker count while sequential
    /// decoding succeeded.
    #[test]
    fn mt_segment_start_needs_huffman_rebuild() {
        let rle_seq = rle_seq_body();
        // Block A: rebuilds every entropy table. Huffman literals with
        // direct weights [1, 1] (symbols 0/1 at 2 bits, symbol 2 at 1 bit),
        // 8x symbol 2, then one explicit-table sequence.
        let block_a = {
            let mut b = vec![0x82, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01];
            b.extend_from_slice(&rle_seq);
            b
        };
        // Block B: Raw literals + explicit FSE tables - the false restart
        // point (no Huffman rebuild, but passes "not Treeless, no Repeat").
        let block_b = {
            let mut b = vec![0x20, 0x30, 0x31, 0x32, 0x33];
            b.extend_from_slice(&rle_seq);
            b
        };
        // Block C: Treeless literals over block A's table.
        let block_c = {
            let mut b = vec![0x43, 0x40, 0x00, 0x1f];
            b.extend_from_slice(&rle_seq);
            b
        };

        let push_block = |frame: &mut Vec<u8>, last: bool, btype: u32, body: &[u8]| {
            let raw = ((body.len() as u32) << 3) | (btype << 1) | u32::from(last);
            frame.extend_from_slice(&raw.to_le_bytes()[..3]);
            frame.extend_from_slice(body);
        };

        // Output: A = 8 literals + offset-4 match; 4x max-size Raw blocks
        // pad the input past the MT floor and every segment-size target;
        // B = 4 raw literals + offset-1 match; C = 4 treeless literals +
        // offset-4 match.
        let mut expect = vec![0x02u8; 11];
        expect.resize(11 + 4 * 128 * 1024, 0xaa);
        expect.extend_from_slice(&[0x30, 0x31, 0x32, 0x33, 0x33, 0x33, 0x33]);
        expect.extend(std::iter::repeat_n(0x02, 7));

        let pad = vec![0xaa; 128 * 1024];
        let mut frame = Vec::new();
        frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
        frame.push(0xa0); // 4-byte frame content size + single segment
        frame.extend_from_slice(&(expect.len() as u32).to_le_bytes());
        push_block(&mut frame, false, 2, &block_a);
        for _ in 0..4 {
            push_block(&mut frame, false, 0, &pad);
        }
        push_block(&mut frame, false, 2, &block_b);
        push_block(&mut frame, true, 2, &block_c);

        for workers in [2u32, 4, 8] {
            let mut out = vec![0u8; expect.len()];
            let n = decode_all_mt(&frame, &mut out, workers, MAX_WINDOW).unwrap();
            assert_eq!((n, &out[..n]), (expect.len(), &expect[..]), "{workers}");

            let mut vec_out = Vec::new();
            decode_to_vec_mt(&frame, &mut vec_out, workers, MAX_WINDOW).unwrap();
            assert_eq!(vec_out, expect, "{workers}");
        }
    }

    /// Synthetic blocks at a fixed input stride, for the planner rules.
    fn synth_blocks(n: usize, stride: usize, blocks_start: usize) -> Vec<ScannedBlock> {
        (0..n)
            .map(|i| {
                let start = blocks_start + i * stride;
                ScannedBlock {
                    btype: BlockType::Raw,
                    raw_size: (stride - 3) as u32,
                    body: start..start + stride - 3,
                }
            })
            .collect()
    }

    #[test]
    fn plan_cuts_snap_to_restart_within_one_and_a_half() {
        let bs = 1000usize;
        let blocks = synth_blocks(20, 100, bs);
        let frame_len = 20 * 100;

        // Restart-dense: the snap keeps exactly the restart-point
        // segmentation a restart-only planner produces (first restart at
        // or past each target).
        let restarts: Vec<usize> = (0..20).collect();
        let target = 250usize;
        let mut expect = Vec::new();
        let mut t = target;
        while t < frame_len {
            expect.push(restarts.iter().copied().find(|&r| r * 100 >= t).unwrap());
            t += target;
        }
        assert_eq!(
            plan_frame_cuts(&blocks, &restarts, target, frame_len, bs, false),
            expect
        );

        // Restart-poor: no restart past the target, so cuts land on the
        // nearest block boundary, ties to the earlier one.
        let restarts = vec![0usize];
        assert_eq!(
            plan_frame_cuts(&blocks, &restarts, target, frame_len, bs, false),
            vec![2, 5, 7, 10, 12, 15, 17]
        );

        // A restart past 1.5x of the target loses to the boundary; the
        // same restart wins once the target comes within 1.5x of it
        // (block 10 at input 1000 against t=750), and later targets go
        // back to the nearest boundary.
        let restarts = vec![0usize, 10];
        assert_eq!(
            plan_frame_cuts(&blocks, &restarts, target, frame_len, bs, false),
            vec![2, 5, 10, 12, 15, 17]
        );

        // A restart at exactly 1.5x of the target still wins (inclusive
        // edge), and beats an equally near boundary.
        let blocks6 = synth_blocks(6, 100, bs);
        let restarts = vec![0usize, 3];
        assert_eq!(
            plan_frame_cuts(&blocks6, &restarts, 200, 6 * 100, bs, false),
            vec![3, 4]
        );
    }

    /// The ramped ladder doubles from the head floor to the target and
    /// keeps the target stride after it; with the ramp off, or a target
    /// at or below the head floor, the grid is the plain target stride.
    /// A restart snaps inside 1.5x of the ramped target, so an early
    /// restart point can take the head cut over the boundary.
    #[test]
    fn plan_cuts_ramp_doubles_head_until_target() {
        const K64: usize = 64 * 1024;
        let bs = 1000usize;
        let blocks = synth_blocks(16, K64, bs);
        let frame_len = 16 * K64;
        let target = 6 * K64;

        // Head 128 KiB, then 256 KiB, then the 384 KiB target, then the
        // target stride: blocks 2, 4, 6, 12.
        assert_eq!(
            plan_frame_cuts(&blocks, &[0], target, frame_len, bs, true),
            vec![2, 4, 6, 12]
        );
        assert_eq!(
            plan_frame_cuts(&blocks, &[0], target, frame_len, bs, false),
            vec![6, 12]
        );

        // A target at or below the head floor leaves the grid untouched.
        assert_eq!(
            plan_frame_cuts(&blocks, &[0], RAMP_HEAD_INPUT, frame_len, bs, true),
            plan_frame_cuts(&blocks, &[0], RAMP_HEAD_INPUT, frame_len, bs, false)
        );

        // The snap window prices the ramped target: restart at block 3
        // (input 192 KiB) lies within 1.5x of the 128 KiB head, so the
        // head cut snaps to it ahead of the nearer block boundary, and the
        // next ramped target lands on block 4's exact boundary.
        assert_eq!(
            plan_frame_cuts(&blocks, &[0, 3], target, frame_len, bs, true),
            vec![3, 4, 6, 12]
        );
    }

    /// Craft one frame: the class-block bodies interleaved with Raw pads
    /// past the MT floor, so the parallel path engages and the planner cuts
    /// mid-epoch inside the layout.
    fn craft_frame(bodies: &[&[u8]], pad_len: usize) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&crate::common::MAGIC_NUM.to_le_bytes());
        frame.push(0x00); // no FCS, no checksum, no dictionary id
        frame.push(0x50); // window descriptor: 1 MiB
        let pad = vec![0x5au8; pad_len];
        let push = |frame: &mut Vec<u8>, last: bool, btype: u32, body: &[u8]| {
            let raw = ((body.len() as u32) << 3) | (btype << 1) | u32::from(last);
            frame.extend_from_slice(&raw.to_le_bytes()[..3]);
            frame.extend_from_slice(body);
        };
        for body in bodies {
            push(&mut frame, false, 2, body);
            push(&mut frame, false, 0, &pad);
        }
        push(&mut frame, true, 0, &pad);
        frame
    }

    /// Crafted frames over the fold classes. The shared table-rebuilding
    /// block (Compressed literals over direct weights [1, 1], every FSE
    /// stream in explicit RLE mode) establishes an epoch's tables; the
    /// other blocks carry (all-Repeat FSE, Treeless literals), re-describe
    /// one axis on a later block, or drop the sequence section entirely.
    /// `raw_explicit` is a Raw-literals restart, the class whose first
    /// mid-epoch cuts carry no Huffman description at all.
    fn fold_class_frames() -> Vec<(&'static str, Vec<u8>)> {
        let compressed = [0x82u8, 0x00, 0x01, 0x81, 0x11, 0xff, 0x01];
        let treeless = [0x43u8, 0x40, 0x00, 0x1f];
        let raw = [0x20u8, 0x30, 0x31, 0x32, 0x33];
        let rle = [0x21u8, 0xaa];
        let lits_seq = |lits: &[u8], seq: &[u8]| {
            let mut b = lits.to_vec();
            b.extend_from_slice(seq);
            b
        };
        let rebuild = lits_seq(&compressed, &rle_seq_body());
        let raw_explicit = lits_seq(&raw, &rle_seq_body());
        let all_rep = seq_body(0xfc, &[]);
        let comp_rep = lits_seq(&compressed, &all_rep);
        let treeless_rep = lits_seq(&treeless, &all_rep);
        let raw_rep = lits_seq(&raw, &all_rep);
        let rle_rep = lits_seq(&rle, &all_rep);
        let ll_only = lits_seq(&raw, &seq_body(0x7c, &[0x04]));
        let of_only = lits_seq(&raw, &seq_body(0xdc, &[0x00]));
        let ml_only = lits_seq(&raw, &seq_body(0xf4, &[0x00]));
        let treeless_of = lits_seq(&treeless, &seq_body(0xdc, &[0x00]));
        let zero_seq_raw = lits_seq(&raw, &[0x00]);
        let zero_seq_compressed = lits_seq(&compressed, &[0x00]);

        let build = |bodies: &[Vec<u8>]| -> Vec<u8> {
            craft_frame(
                &bodies.iter().map(|b| b.as_slice()).collect::<Vec<_>>(),
                96 * 1024,
            )
        };
        vec![
            (
                "all_repeat_chain",
                build(&[
                    rebuild.clone(),
                    comp_rep.clone(),
                    comp_rep.clone(),
                    treeless_rep.clone(),
                    treeless_rep.clone(),
                    raw_rep.clone(),
                    rle_rep.clone(),
                    treeless_rep.clone(),
                ]),
            ),
            (
                "partial_fse",
                build(&[
                    rebuild.clone(),
                    ll_only.clone(),
                    of_only.clone(),
                    ml_only.clone(),
                    comp_rep.clone(),
                    treeless_of.clone(),
                    ml_only.clone(),
                    treeless_rep.clone(),
                ]),
            ),
            (
                "zero_seq",
                build(&[
                    rebuild.clone(),
                    zero_seq_raw.clone(),
                    zero_seq_compressed.clone(),
                    treeless_rep.clone(),
                    zero_seq_raw.clone(),
                    comp_rep.clone(),
                ]),
            ),
            (
                "raw_restart_epoch",
                build(&[
                    raw_explicit.clone(),
                    comp_rep.clone(),
                    treeless_rep.clone(),
                    rebuild.clone(),
                    treeless_rep.clone(),
                ]),
            ),
        ]
    }

    /// The crafted fold-class frames must decode byte-identically through
    /// the parallel path, and the planner must actually route them through
    /// primed mid-epoch segments (else the test would pass through the
    /// sequential fallback without exercising the fold).
    #[test]
    fn mid_epoch_segments_decode_identically() {
        for (name, frame) in fold_class_frames() {
            let expect = bulk::decompress(&frame, 0).unwrap();
            assert!(!expect.is_empty());
            for workers in [2u32, 4, 8] {
                let plan = scan(&frame, workers, MAX_WINDOW).expect("frame must scan");
                assert!(
                    plan.segments.iter().any(|s| s.prime.is_some()),
                    "{name}: no mid-epoch segment at {workers} workers"
                );
                let mut out = Vec::new();
                decode_to_vec_mt(&frame, &mut out, workers, MAX_WINDOW).unwrap();
                assert_eq!(out, expect, "{name} at {workers} workers");
                let mut placed = vec![0u8; expect.len()];
                let n = decode_all_mt(&frame, &mut placed, workers, MAX_WINDOW).unwrap();
                assert_eq!(&placed[..n], &expect[..], "{name} at {workers} workers");
            }
        }
    }

    /// The head-ramp gate's terms: the literals share of the staged
    /// volume arms at exactly one half, and the worker count, the MT floor
    /// and a block that failed to parse all disarm.
    #[test]
    fn ramp_gate_terms() {
        let floor = super::MIN_MT_INPUT;
        let armed = |literals: u64, sequences: u64, complete: bool| FrameVolumes {
            literals,
            sequences,
            complete,
        };
        assert!(armed(12_000, 1_000, true).ramp_eligible(4, floor + 1));
        assert!(!armed(11_999, 1_000, true).ramp_eligible(4, floor + 1));
        assert!(!armed(12_000, 1_000, true).ramp_eligible(1, floor + 1));
        assert!(!armed(12_000, 1_000, true).ramp_eligible(4, floor));
        assert!(!armed(12_000, 1_000, false).ramp_eligible(4, floor + 1));
    }

    /// Scan-level wiring of the head ramp: a literal-dominated frame above
    /// the MT floor starts its grid on the head floor at two workers,
    /// while the same shape with sequence-dominated blocks, a one-worker
    /// scan, or an unparseable block keeps the target grid.
    #[test]
    fn head_ramp_arms_on_staged_volumes() {
        // 1024 raw literals plus one sequence: 1024 B staged literals
        // against a 12 B record, a 0.988 share.
        let big_lits = {
            let mut b = vec![0x04, 0x40];
            b.extend(std::iter::repeat_n(0x61, 1024));
            b.extend_from_slice(&rle_seq_body());
            b
        };
        // 4 raw literals against 100 declared sequences: a 0.003 share.
        let seq_heavy = vec![0x20, 0x30, 0x31, 0x32, 0x33, 0x64, 0x54];
        // Raw-literals header claiming a 20-bit size in a 1-byte body:
        // the pre-scan cannot parse it, so the frame disarms.
        let unparseable = vec![0x0c];

        let craft = |bodies: &[Vec<u8>]| {
            craft_frame(
                &bodies.iter().map(|b| b.as_slice()).collect::<Vec<_>>(),
                120 * 1024,
            )
        };
        let first_seg_blocks = |frame: &[u8], workers: u32, name: &str| {
            let plan = scan(frame, workers, MAX_WINDOW)
                .unwrap_or_else(|| panic!("{name} must scan at {workers} workers"));
            assert!(
                plan.segments.len() >= 2,
                "{name} must segment at {workers} workers"
            );
            plan.segments[0].blocks.len()
        };

        let armed = craft(&vec![big_lits.clone(); 4]);
        // The head cut lands three blocks in, just past the 128 KiB head
        // on the nearest boundary.
        assert_eq!(first_seg_blocks(&armed, 2, "armed"), 3);
        // One worker never ramps.
        assert!(first_seg_blocks(&armed, 1, "armed-1w") > 2);
        // A sequence-dominated shape of the same craft keeps the target grid.
        let gated = craft(&vec![seq_heavy.clone(); 4]);
        assert!(first_seg_blocks(&gated, 2, "gated") > 2);
        // An unparseable block disarms the frame.
        let broken = craft(&vec![
            big_lits.clone(),
            unparseable,
            big_lits.clone(),
            big_lits,
        ]);
        assert!(first_seg_blocks(&broken, 2, "broken") > 2);
    }

    /// The committed decode_mt fuzz seed compresses `semi_structured_records`
    /// at libzstd -9 and parks the frame once per process; this pins that
    /// the constructed class actually routes through primed mid-epoch
    /// segments (a restart-poor frame above the MT floor whose cuts land
    /// inside table-carrying epochs) instead of falling back to the
    /// sequential path.
    #[test]
    fn libzstd_nine_records_route_through_primed_segments() {
        let data = semi_structured_records();
        let frame = zstd::bulk::compress(&data, 9).unwrap();
        let plan = scan(&frame, 4, MAX_WINDOW).expect("the records frame must scan");
        assert!(
            plan.segments.len() >= 2,
            "the -9 records frame must segment"
        );
        assert!(
            plan.segments.iter().any(|s| s.prime.is_some()),
            "the -9 records class must cut mid-epoch"
        );
        let mut out = Vec::new();
        decode_to_vec_mt(&frame, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(out, data);
    }

    /// Scan one frame's blocks, probes and staged volumes (single-frame
    /// inputs).
    fn frame_blocks(input: &[u8]) -> Option<(Vec<ScannedBlock>, Vec<RestartProbe>, FrameVolumes)> {
        let mut reader = input;
        let (header, _) = crate::decoding::frame::read_frame_header(&mut reader).ok()?;
        let blocks_start = input.len() - reader.len();
        let window = header.window_size().ok()? as usize;
        scan_frame_blocks(input, window, blocks_start)
    }

    /// The table epoch at boundary `b` of the epoch `[e0, ..)`, folded
    /// independently of the scan's tracking: the last block describing each
    /// axis since the epoch start.
    fn boundary_prime(
        blocks: &[ScannedBlock],
        probes: &[RestartProbe],
        e0: usize,
        b: usize,
    ) -> TablePrime {
        let mut huf = None;
        let mut ll = None;
        let mut of = None;
        let mut ml = None;
        for j in e0..b {
            if probes[j].huffman == HuffmanState::Rebuild {
                huf = Some(j);
            }
            if probes[j].ll_explicit {
                ll = Some(j);
            }
            if probes[j].of_explicit {
                of = Some(j);
            }
            if probes[j].ml_explicit {
                ml = Some(j);
            }
        }
        let body = |slot: Option<usize>| slot.map(|j| blocks[j].body.clone());
        TablePrime {
            huffman: body(huf),
            ll: body(ll),
            of: body(of),
            ml: body(ml),
        }
    }

    fn assert_staged_suffix(whole: &DecodedSegment, skip: usize, part: &DecodedSegment, ctx: &str) {
        assert_eq!(
            whole.plan.len() - skip,
            part.plan.len(),
            "{ctx}: plan length"
        );
        let prefix_out: usize = whole.plan[..skip].iter().map(BlockPlan::out).sum();
        assert_eq!(
            whole.out_size - prefix_out,
            part.out_size,
            "{ctx}: out size"
        );
        for (k, (wp, pp)) in whole.plan[skip..].iter().zip(&part.plan).enumerate() {
            match (wp, pp) {
                (BlockPlan::Raw { body: wb, out: wo }, BlockPlan::Raw { body: pb, out: po }) => {
                    assert_eq!((wb.clone(), *wo), (pb.clone(), *po), "{ctx}: raw block {k}");
                },
                (BlockPlan::Rle { byte: wb, len: wl }, BlockPlan::Rle { byte: pb, len: pl }) => {
                    assert_eq!((*wb, *wl), (*pb, *pl), "{ctx}: rle block {k}");
                },
                (
                    BlockPlan::Compressed {
                        lits: wl,
                        seqs: ws,
                        out: wo,
                    },
                    BlockPlan::Compressed {
                        lits: pl,
                        seqs: ps,
                        out: po,
                    },
                ) => {
                    assert_eq!(*wo, *po, "{ctx}: block {k} out");
                    assert_eq!(
                        &whole.literals[wl.clone()],
                        &part.literals[pl.clone()],
                        "{ctx}: block {k} literals"
                    );
                    let want = &whole.sequences[ws.clone()];
                    let got = &part.sequences[ps.clone()];
                    assert_eq!(want.len(), got.len(), "{ctx}: block {k} sequence count");
                    for (a, b) in want.iter().zip(got) {
                        assert_eq!((a.ll, a.ml, a.of), (b.ll, b.ml, b.of), "{ctx}: block {k}");
                    }
                },
                _ => panic!("{ctx}: block {k} plan kind mismatch"),
            }
        }
    }

    /// Every candidate sub-segment start inside every restart epoch must
    /// stage byte-identically to the whole-epoch staging: the recorded
    /// table epoch reproduces, at each boundary, exactly the tables the
    /// epoch-start decode carries there. Crafted frames cover the fold
    /// classes with deep epochs; real encoder frames add whatever classes
    /// the encoders emit in the wild.
    #[test]
    fn subsegment_staging_matches_whole_epoch() {
        let mut frames: Vec<(&'static str, Vec<u8>)> = fold_class_frames();
        frames.push((
            "ours_lvl9",
            bulk::compress(&textish(1024 * 1024), Level::from_zstd(9)),
        ));
        let mut state = 12345u64;
        let mut data = Vec::with_capacity(1024 * 1024);
        let mut id = 0u64;
        while data.len() < 1024 * 1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let user = (state >> 33) % 5000;
            data.extend_from_slice(
                format!(
                    "{{\"id\":{id},\"user\":\"user_{user}\",\"ts\":{}}}",
                    1700000000 + id
                )
                .as_bytes(),
            );
            id += 1;
        }
        frames.push(("zstd_lvl3", zstd::bulk::compress(&data, 3).unwrap()));
        let mut alphabet = Vec::with_capacity(512 * 1024);
        let table: Vec<u8> = (0..16u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        state = 7u64;
        while alphabet.len() < 512 * 1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            alphabet.push(table[((state >> 33) as usize) % 16]);
        }
        frames.push((
            "ours_zero_seq",
            bulk::compress_with(
                &alphabet,
                &EncoderOptions::new(Level::from_zstd(9)).workers(4),
            )
            .unwrap(),
        ));

        let mut compared = 0usize;
        for (name, frame) in &frames {
            let Some((blocks, probes, _volumes)) = frame_blocks(frame) else {
                panic!("{name}: frame must scan");
            };
            let restarts = restart_candidates(&probes);
            let mut starts = vec![0usize];
            starts.extend(restarts.iter().copied().filter(|&r| r > 0));
            for (w, &e0) in starts.iter().enumerate() {
                let e1 = starts.get(w + 1).copied().unwrap_or(blocks.len());
                if e1 - e0 < 2 {
                    continue;
                }
                let whole_plan = SegmentPlan {
                    blocks: blocks[e0..e1].to_vec(),
                    frame_start: e0 == 0,
                    window_size: 128 * 1024,
                    prime: None,
                };
                let mut scratch = SegmentScratch::new();
                let whole = decode_segment(frame, &whole_plan, &mut scratch)
                    .unwrap_or_else(|e| panic!("{name}: whole epoch {e0} failed: {e}"));
                for b in e0 + 1..e1 {
                    let part_plan = SegmentPlan {
                        blocks: blocks[b..e1].to_vec(),
                        frame_start: false,
                        window_size: 128 * 1024,
                        prime: Some(boundary_prime(&blocks, &probes, e0, b)),
                    };
                    let mut scratch = SegmentScratch::new();
                    let part = decode_segment(frame, &part_plan, &mut scratch)
                        .unwrap_or_else(|e| panic!("{name}: boundary {b} failed: {e}"));
                    assert_staged_suffix(&whole, b - e0, &part, &format!("{name} boundary {b}"));
                    compared += 1;
                }
            }
        }
        assert!(
            compared > 40,
            "expected a meaningful boundary sample, got {compared}"
        );
    }

    /// Repeated decodes over alternating inputs must stay byte-exact: the
    /// pool reuses worker threads and staging buffers across calls, and
    /// every reuse re-initializes what execution reads (scratch tables
    /// reset per segment, staging overwritten exactly).
    #[test]
    fn repeated_decodes_stay_identical() {
        let a = textish(6 * 1024 * 1024);
        let b: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let ca = bulk::compress_with(&a, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let cb = bulk::compress_with(&b, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        for round in 0..6 {
            let (src, expect) = if round % 2 == 0 {
                (&ca, &a)
            } else {
                (&cb, &b)
            };
            let mut out = Vec::new();
            decode_to_vec_mt(src, &mut out, 4, MAX_WINDOW).unwrap();
            assert_eq!(out[..], expect[..], "round {round}");
        }
    }

    /// A panic inside a stage-A worker must surface to the decode caller
    /// (the poison payload resumes once every lease is waited out), and the
    /// retired worker's slot must not stall the next decode: a fresh thread
    /// serves it, byte-exact.
    #[test]
    fn stage_a_panic_surfaces_and_pool_recovers() {
        let data = textish(8 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mut out = vec![0u8; data.len()];
        let inject_guard = INJECT_LOCK.lock().unwrap();
        super::STAGE_PANIC_INPUT.store(
            compressed.as_ptr().cast_mut(),
            core::sync::atomic::Ordering::Relaxed,
        );
        super::STAGE_PANIC_AT.store(3, core::sync::atomic::Ordering::Relaxed);
        let blew = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = decode_all_mt(&compressed, &mut out, 4, MAX_WINDOW);
        }));
        super::STAGE_PANIC_AT.store(usize::MAX, core::sync::atomic::Ordering::Relaxed);
        super::STAGE_PANIC_INPUT
            .store(core::ptr::null_mut(), core::sync::atomic::Ordering::Relaxed);
        drop(inject_guard);
        let payload = blew.expect_err("the worker panic must surface");
        let msg = payload.downcast_ref::<&str>().copied().or_else(|| {
            payload
                .downcast_ref::<alloc::string::String>()
                .map(std::string::String::as_str)
        });
        assert_eq!(msg, Some("injected stage-A panic"));

        out.fill(0);
        let n = decode_all_mt(&compressed, &mut out, 4, MAX_WINDOW).unwrap();
        assert_eq!(&out[..n], &data[..]);
    }

    /// The poison fill of the job's last segment is the decode's final
    /// event, so a notify lost on the panic arm parks the ordered consumer
    /// forever. The watchdog bounds the call: a lost wakeup fails the
    /// suite instead of hanging it.
    #[test]
    fn stage_a_panic_on_last_segment_surfaces() {
        let data = textish(6 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let inject_guard = INJECT_LOCK.lock().unwrap();
        super::STAGE_PANIC_INPUT.store(
            compressed.as_ptr().cast_mut(),
            core::sync::atomic::Ordering::Relaxed,
        );
        super::STAGE_PANIC_LAST.store(true, core::sync::atomic::Ordering::Relaxed);
        let n = data.len();
        let handle = std::thread::spawn(move || {
            let mut out = vec![0u8; n];
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = decode_all_mt(&compressed, &mut out[..], 4, MAX_WINDOW);
            }))
        });
        let blew = handle.join().expect("the decode must terminate, not hang");
        super::STAGE_PANIC_LAST.store(false, core::sync::atomic::Ordering::Relaxed);
        super::STAGE_PANIC_INPUT
            .store(core::ptr::null_mut(), core::sync::atomic::Ordering::Relaxed);
        drop(inject_guard);
        let payload = blew.expect_err("the worker panic must surface");
        let msg = payload.downcast_ref::<&str>().copied().or_else(|| {
            payload
                .downcast_ref::<alloc::string::String>()
                .map(std::string::String::as_str)
        });
        assert_eq!(msg, Some("injected stage-A panic"));
    }

    /// The pool's parked threads retire when the pool drains, so a decode
    /// leaves no threads outside the bounded idle window. Linux counts the
    /// named workers through /proc, so other tests' threads never pollute
    /// the count.
    #[test]
    #[cfg(target_os = "linux")]
    fn pool_threads_retire_after_decode() {
        fn dec_pool_threads() -> usize {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter_map(|e| {
                    let path = e.unwrap().path();
                    let comm = std::fs::read_to_string(format!("{}/comm", path.display())).ok()?;
                    (comm.trim() == "zstdx-dec-pool").then_some(1)
                })
                .sum()
        }
        let data = textish(8 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mut out = vec![0u8; data.len()];
        let n = decode_all_mt(&compressed, &mut out, 8, MAX_WINDOW).unwrap();
        assert_eq!(&out[..n], &data[..]);
        // The workers served the lease and parked back in the pool.
        assert!(dec_pool_threads() > 0, "decode must lease pool threads");
        // Draining retires every parked worker; each exits at the flag. A
        // worker between lease-leave and repark misses the first drain, so
        // re-drain while the count settles.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while dec_pool_threads() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "parked pool threads must retire when drained"
            );
            super::super::mt_pool::retire_all_for_test();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The staged executor's inline offset selector against
    /// `do_offset_history`: both implement the one offset-code spec table
    /// in two shapes, and the inline shape was pinned only end-to-end
    /// before (the sequence-loop review recorded the gap). Each case runs
    /// one segment through `execute_blocks` and against a reference that
    /// resolves every sequence through `do_offset_history`, over the full
    /// repcode grid (wire 1..=3 at both ll0 flags), real offsets past the
    /// +3 bias, and a zero-seeded history whose slots must error or
    /// resolve per the saturation rule.
    #[test]
    fn staged_selector_matches_offset_history() {
        use super::{BlockPlan, DecodedSegment, OutRegion, execute_blocks};
        use crate::{
            blocks::sequence_section::Sequence, decoding::sequence_execution::do_offset_history,
        };

        fn run_case(seed: [u32; 3], seqs: &[(u32, u32, u32)], lit_len: usize) {
            // Distinct literal bytes make every match copy's source
            // window identify the offset it read.
            let alphabet: Vec<u8> = (0..48u8).map(|b| b + 1).collect();
            // Expected bytes and history from the reference resolution;
            // the match is the semantic overlapping repeat at the
            // resolved offset.
            let mut ref_hist = seed;
            let mut expect: Vec<u8> = Vec::new();
            let mut expect_err = false;
            let mut lit = 0usize;
            for &(ll, ml, of) in seqs {
                expect.extend_from_slice(&alphabet[lit..lit + ll as usize]);
                lit += ll as usize;
                let o_lit_end = expect.len();
                if let Some(r) = do_offset_history(of, ll, &mut ref_hist) {
                    let r = r.get() as usize;
                    assert!(
                        r <= o_lit_end,
                        "case must keep sources inside the frame: r={r} end={o_lit_end}"
                    );
                    for _ in 0..ml {
                        let b = expect[expect.len() - r];
                        expect.push(b);
                    }
                } else {
                    expect_err = true;
                    break;
                }
            }

            // The staged segment: real literals plus the 16-byte zero tail
            // the hot path's unconditional 16-byte copies overread into.
            let mut literals = alphabet[..lit_len].to_vec();
            literals.extend_from_slice(&[0u8; 16]);
            let out_total: usize = seqs.iter().map(|&(l, m, _)| l as usize + m as usize).sum();
            let seg = DecodedSegment {
                literals,
                sequences: seqs
                    .iter()
                    .map(|&(ll, ml, of)| Sequence { ll, ml, of })
                    .collect(),
                plan: vec![BlockPlan::Compressed {
                    lits: 0..lit_len,
                    seqs: 0..seqs.len(),
                    out: out_total,
                }],
                out_size: out_total,
            };
            let mut buf = vec![0u8; out_total + 32];
            let mut hist = seed;
            let result = {
                // SAFETY: the region spans the whole buffer (every write
                // plus the 16-byte wildcopy slack), and the reference above
                // proved every offset and budget inside the frame; the
                // input slice is never read (no Raw blocks).
                unsafe {
                    let region = OutRegion::new(buf.as_mut_ptr(), buf.len(), usize::MAX);
                    execute_blocks(region, 0, 0, &seg, 0..1, &[], &mut hist)
                }
            };
            if expect_err {
                assert!(
                    result.is_err(),
                    "case {seed:?} {seqs:?} must reject the zero offset"
                );
                return;
            }
            result.expect("case must execute");
            assert_eq!(
                &buf[..out_total],
                &expect[..],
                "bytes of case {seed:?} {seqs:?}"
            );
            assert_eq!(hist, ref_hist, "history of case {seed:?} {seqs:?}");
        }

        // One real-offset lead sequence builds output and rotates the
        // history, then the probe lands the case's selector.
        for seed in [[1u32, 4, 8], [7, 3, 9], [0, 5, 9]] {
            for of in 1u32..=7 {
                for ll in [0u32, 1, 2] {
                    let seqs = [(12u32, 4u32, 11u32), (ll, 3, of)];
                    run_case(seed, &seqs, 12 + ll as usize);
                }
            }
        }
        // A repcode chain exercises the rotations and the carry across a
        // block's sequence run.
        run_case(
            [1, 4, 8],
            &[
                (12, 4, 11),
                (0, 3, 1),
                (0, 3, 1),
                (1, 3, 1),
                (0, 3, 2),
                (2, 3, 3),
                (0, 3, 3),
            ],
            15,
        );
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
