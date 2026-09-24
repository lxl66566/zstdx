//! Matching algorithm used to find repeated parts of the original data
//!
//! The Zstd format relies on finding repeated sequences of data and compressing these
//! sequences as instructions for the decoder. A sequence basically tells the decoder
//! "Go back X bytes and copy Y bytes to the end of your decode buffer".
//!
//! This is a port of the official zstd matcher family: one contiguous
//! window buffer, single-probe hash tables (newest-wins overwrite
//! semantics, dfast's second short-hash table for the Fast level), hash
//! chains for the levels above, forward match extension in u64 chunks plus
//! backward extension into pending literals. Positions are tracked as
//! absolute u64 offsets. The fast/dfast/chain table slots are u32: the
//! position biased by one and truncated (zero stays the never-written
//! sentinel), with the high bits reconstructed from the scanning position
//! and a window-range check at read time — half the working set of the
//! former u64 slots. The opt strategies' tree tables are u32 too: a
//! biased position on per-state monotone coordinates whose origin a reset
//! advances instead of clearing the tables (see [`super::opt`]).

use alloc::vec::Vec;

use super::{
    Matcher, SeqWord, Sequence,
    btlazy::{LazyScratch, LazyStep},
    ldm::{LdmSeq, LdmState},
    opt::{OptKnobs, OptScratch, OptState},
    reach_probe::{KEEP_REACH, ProbeStats, ReachChoice, SHRINK_REACH},
    seq_codes::decode_packed,
};
// Shared with the decoder so both sides agree on offset-history semantics.
use crate::{InputShape, Level};

/// Shortest match worth encoding; matches the format's MINMATCH range.
pub(super) const MIN_MATCH: usize = 4;
/// The hash reads a full u64, so insertable/scannable positions need this
/// many window bytes ahead to stay in bounds.
pub(super) const HASH_READ: usize = 8;
/// Hash table size as a power of two.
const HASH_LOG: u32 = 15;
/// Prefill grid spacing (libzstd's `fastHashFillStep`, also what its
/// dictionary-content load uses for fast/dfast): the strip's mid-distance
/// match coverage survives a 3x coarser grid, at a third of the fill cost.
/// Periodic-repeat locking is NOT the grid's job — see `seed_offset`; a
/// dense grid cannot provide it anyway, because on clumped data the twin
/// slot's newest entry is always a recent same-hash recurrence, burying any
/// period-old twin. The chain strategy deviates from libzstd's dense
/// dictionary fill here: its strip is the full window (libzstd's job prefix
/// is window>>3), so a dense fill would cost half the job's scan time; the
/// grid keeps the head table's first hop and the chain links (grid
/// positions link to the previous same-hash grid position) while unwritten
/// chain slots simply read as dead or stale entries, which the walk's
/// domain check already discards.
const PREFILL_STRIDE: usize = 3;
/// Tail fraction of a [`Strategy::Fast`] strip that the grid fill covers, in
/// units of `slots * PREFILL_STRIDE` bytes. The strategy's table holds one
/// candidate per slot, newest-wins, and a full-window strip contributes
/// `strip / (slots * stride)` grid inserts per slot — every insert before the
/// last few per slot is overwritten before the job's scan ever probes, so
/// filling beyond the retention horizon is churn on random-class data. The
/// dropped head entries are NOT all dead, though: on a period≈window corpus
/// each period-old twin hashes its slot once per period and survives the
/// whole fill (measured: text tiles at period 441226 ride exactly these
/// entries), so the cap is only safe because the confirmed seed
/// ([`SEED_CONFIRMS`]) carries that class — never cap one without the other.
/// Fast only: dfast's long table is 2^17 (a 2 MiB strip barely reaches one
/// horizon), and chain links make the whole strip walkable.
const PREFILL_RETAIN_HORIZONS: usize = 2;
/// Which fill geometry a history window is indexed under.
#[derive(Clone, Copy, PartialEq)]
enum FillGrid {
    /// The multithreaded job strip's tuned sparse grid ([`PREFILL_STRIDE`],
    /// Fast's retention-tail cap): a strip is as long as the window, so a
    /// dense fill would rival the job's own scan cost while mid-distance
    /// coverage survives the grid.
    Strip,
    /// The dictionary load, mirroring libzstd's `ZSTD_loadDictionaryContent`
    /// as the CLI's CDict-attach route fills it (`ZSTD_dtlm_full`): the
    /// payload is parsed mostly against the dictionary, where candidate
    /// coverage dominates, and the content is at most one window long —
    /// fast/dfast take the stride grid plus every skipped position whose
    /// slot is still empty, chain takes every position densely, and the
    /// tree rows keep the lazy whole-content fill.
    Dictionary,
}
/// Whether [`MatchGeneratorDriver::fill_window_grid`] ingests the strip
/// into the LDM state itself or leaves it to the caller (the windowed
/// capture band's split prefill — see `prefill_job_strip_chain`).
#[derive(Clone, Copy, PartialEq)]
enum LdmStripFill {
    Ingest,
    Defer,
}

/// History kept for matching; also the window size declared in the frame header.
const MAX_WINDOW: usize = 0xc0000;

// The flags below are independent sticky per-frame/per-block latches (gate
// hold, DUBT staleness, LDM latch, strip parse, dictionary chain) with
// different clear points; they do not collapse into one state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct MatchGeneratorDriver {
    /// Contiguous history: `win[0]` is absolute position `win_base`.
    /// Capacity holds two windows plus one block, so compaction (which keeps
    /// MAX_WINDOW) runs at ~1x data volume amortized; blocks are read
    /// directly into the spare tail.
    win: Vec<u8>,
    /// Direct-window mode: the window points into caller-owned memory
    /// instead of `win` (see [`MatchGeneratorDriver::adopt_window`]).
    ext: Option<ExtWindow>,
    win_base: u64,
    /// Absolute end of committed data; matching runs up to `block_end`.
    pos: u64,
    block_end: u64,
    /// Start of the literals not yet covered by a sequence.
    anchor: u64,
    /// Absolute start of the last committed block (for `get_last_space`).
    block_start: u64,
    /// The fast/dfast/chain strategies' two tables in one allocation —
    /// `tables[..second]` the head hash table, `tables[second..]` the
    /// second table (the dfast short hash or the chain links; see
    /// [`Strategy`]), u32 entries (see [`pack_pos`]). One buffer keeps a
    /// single base pointer live in the scan loops: the second table's
    /// accesses are displacements off it instead of a second hot pointer.
    tables: Vec<u32>,
    /// Length of the head table inside `tables` (== `tables.len()` when
    /// the strategy has no second table).
    second: usize,
    /// The row strategy's per-row insertion heads (one byte per row,
    /// libzstd's `ZSTD_row_nextIndex` state): the byte names the slot the
    /// row's last insert took, the next insert takes its predecessor, so a
    /// full row evicts its oldest entry and the search's age order is the
    /// head rotation. One byte per row keeps it 32-64 KiB (L2-resident
    /// against the multi-MiB entry table) and off the entry line, so the
    /// fill loop's head load never serializes behind its own cold-line
    /// stores. Cleared per job with the head table (see
    /// [`Self::fill_window_grid`]): slot choice depends on it, so residue
    /// would leak the worker-claim order into the bytes.
    row_heads: Vec<u8>,
    /// Head hash table for the opt strategies, u32 origin-biased entries
    /// (see [`super::opt`]).
    opt_table: Vec<u32>,
    /// The opt strategies' binary-tree ring: two u32 link slots per ring
    /// position; empty outside opt.
    bt: Vec<u32>,
    /// Single-probe 3-byte table for the opt strategies with `min_match == 3`
    /// (libzstd's hashTable3); empty otherwise.
    hash3: Vec<u32>,
    /// The DUBT finder's hash heads for the btlazy2 strategy: u32
    /// position entries (see [`super::dubt`]), like libzstd's btlazy2
    /// tables. The random-access working set is window-sized, so entry
    /// width dominates its cache/TLB behavior.
    dubt_table: Vec<u32>,
    /// The DUBT finder's two-slot tree ring for the btlazy2 strategy, u32
    /// entries like `dubt_table`; empty outside btlazy2.
    dubt_bt: Vec<u32>,
    /// Content-tag table of the incompressibility gate (see
    /// [`Matcher::skip_if_incompressible`]): slot = high hash bits of a
    /// sampled 8-byte window, entry = its low 32 bits. Entries are
    /// position-independent content tags, so the table is never reset: a
    /// stale tag can only produce a spurious hit, which selects the
    /// conservative path.
    probe: Vec<u32>,
    /// Sticky incompressibility hold: set once a block clears the exact
    /// entropy pass, cleared by any gate rejection. While held, a block
    /// that passes the strided repeat probe (no repeated 8-gram, >= 208
    /// distinct sampled bytes) gates without re-running the exact
    /// histogram. The probe still bars every matchable block; what the
    /// skip accepts is the residual risk of a repeat-free block with
    /// skewed bytes (entropy < 7.97) following a gated one — its loss is
    /// the literals-only huffman saving the matcher would never have
    /// improved on, bounded by the flat ~210-256-symbol alphabet the
    /// probe's distinct screen forces (<= ~2%, synthetic shapes only;
    /// natural incompressible data sits at ~8 bits/B). Same precedent as
    /// the literals gate's `literals_gate_hold`.
    gate_hold: bool,
    /// Price statistics for the opt strategies, persisting across blocks.
    opt_state: OptState,
    /// DP scratch for the opt strategies (~130 KiB; allocated on demand).
    opt_scratch: Option<OptScratch>,
    /// Candidate scratch for the btlazy2 strategy (~32 KiB; on demand).
    lazy_scratch: Option<LazyScratch>,
    /// Tree fill point for the opt strategies: positions below it are
    /// already inserted into the binary tree.
    next_update: u64,
    /// Earliest absolute position not yet offered to the search tables
    /// (u64::MAX = none pending). An incompressibility-gated block opens
    /// the gap at its start; the next scanning block's dense catch-up
    /// ([`Self::catch_up_insertions`]) fills it and closes it — the table
    /// strategies' counterpart of the opt tree's lazy `next_update` fill.
    /// Tracked from the gate side only, so the scan loops themselves never
    /// write it (their bodies must stay byte-identical to the ungated
    /// build; the fast tiers pay pure layout noise for any growth).
    gap_start: u64,
    /// Coordinate origin of the opt tables' entries (see [`super::opt`]):
    /// every reset advances it past every position the state has written,
    /// retiring all stale entries at read time instead of clearing.
    opt_origin: u64,
    /// Coordinate origin of the chain strategy's head entries (the opt
    /// tables' scheme applied to `tables[..second]`): each strip prefill —
    /// every MT job, including job zero's empty strip — advances it past
    /// every position the state has written since the last advance, so
    /// stale entries rebuild as beyond-reach distances at read time and
    /// the per-job head clear disappears (an 8 MiB H21 clear that dominated
    /// fast-compressing shapes: zeros.balanced stream-mt measured the
    /// clear at ~35% of summed job time). ST frames never advance it:
    /// their cross-frame residue semantics (window-domain validation plus
    /// byte verification) are the documented don't-clear design and stay
    /// bit-exact. The fast/dfast/row tables never advance it either —
    /// their per-job clears stand (the row's 24-bit packed positions
    /// cannot carry a 32-bit shift, and their table sizes make the clear
    /// cheap).
    head_origin: u64,
    /// High-water mark of the positions this state has written into the
    /// chain heads since the last origin advance, stashed at each reset
    /// (before the cursor zeroes; `max` because the pooled bulk path
    /// resets twice and the second reset would overwrite the mark with 0)
    /// and at each strip fill (fill sites do not run the scan cursor).
    /// The next strip prefill advances `head_origin` past it — the
    /// invalidation invariant "advance exceeds every written position"
    /// (`chain_search`'s stale-entry rejection) needs the mark to cover
    /// exactly the writes since the last advance.
    strip_mark: u64,
    miss_count: usize,
    /// Short-match interior fill policy for the current block's fast scan,
    /// decided from the previous block's parse density: at least
    /// `COVERED_GATE_MIN_SEQS` sequences and `COVERED_GATE_AVG_LL` literal
    /// bytes per sequence marks dll-class sparse content, whose redundant
    /// copies keep stride-2 fill's phase coverage at ~0.04% size for half
    /// the inserts; denser parses keep every position (their shifted
    /// repeats have no duplicate copy to hit instead). Set at fast-strategy
    /// block boundaries only; reset per frame.
    covered_fill: CoveredFill,
    /// Scan instantiation of the next fast block (see [`ScanDensity`]);
    /// decided at fast-strategy block boundaries, reset per frame.
    scan_density: ScanDensity,
    params: LevelParams,
    /// Repeated-offset history, kept in lockstep with the decoder's
    /// `offset_hist` so repcode probes see the same candidates it will.
    rep: [u32; 3],
    /// Literal-offset sequences still required before repcode references
    /// are decodable: 0 means the history above is known to match the
    /// decoder's. A job that starts mid-frame (see the mt module) cannot
    /// know the decoder's history; three literal offsets shift it fully
    /// into known territory because the update is a plain 3-slot shift.
    rep_pending: u8,
    /// Huffman code lengths of the table that encoded the previous block's
    /// literals (fed back by the block encoder via `note_literal_costs`;
    /// uncovered symbols priced at the 11-bit cap — they force a rebuild
    /// and land at the longest codes). The chain strategy's store gate
    /// prices the literals a match displaces against them. Reset per
    /// frame/job so output bytes stay a function of the frame/job content
    /// alone.
    lit_lens: [u8; 256],
    /// Long-repeat offset probed directly at a multithreaded job's start
    /// (see [`Self::prefill_window`]): 0 = inactive. The fast strategy's
    /// single-probe table cannot hold a periodic repeat's twin — on clumped
    /// data every slot's newest entry is a recent same-hash recurrence, so
    /// the period is unreachable through the hash path until the scan has
    /// self-indexed a full period. The seed sidesteps the table: bytes are
    /// compared at the exact offset, so three seed matches both encode
    /// legally (literal offsets, no repcode gate) and rotate `rep` until it
    /// holds the period, which the repcode probes then ride.
    seed_offset: u32,
    /// Seed matches emitted so far; the seed retires after
    /// [`SEED_MATCHES`].
    seed_hits: u8,
    /// Probe attempts left before an unused seed retires; bounds the cost
    /// of a seed that never matches.
    seed_budget: u32,
    slice_size: usize,
    /// Caller-declared input shape (see [`Matcher::set_input_shape`]);
    /// applied at the next `reset` via [`params_for`].
    shape: InputShape,
    /// Cached small-band alphabet screen (see [`win_small_wide`]),
    /// keyed by the window it was computed from: a pooled state sees
    /// the same caller buffer across repeat calls (the small-payload
    /// pattern), and the screen is the one pass over it those calls
    /// would otherwise pay every time. A stale verdict (same buffer,
    /// mutated content) only shifts parse style, never safety.
    small_wide: Option<bool>,
    small_wide_key: (usize, usize),
    /// The frame's reach probe result (see [`super::reach_probe`]); applied
    /// inside `apply_level`, so entry points set it before `reset` or
    /// re-apply through [`Matcher::consider_reach_probe`].
    reach_choice: ReachChoice,
    /// LDM arming context (see [`LdmArming`]); applied inside
    /// [`Self::apply_level`]'s size gate, so entry points set it before
    /// `reset`.
    ldm_arming: LdmArming,
    /// Cold-start DUBT head lifecycle (see [`HeadPhase`]).
    dubt_head: HeadPhase,
    /// Cold-head probe-step phase for the btlazy2 rows (see
    /// [`BtStepPhase`]).
    bt_step: BtStepPhase,
    /// Live reach-probe accumulator while a donating entry point runs the
    /// frame's own keep-side blocks as the probe's keep measurement (see
    /// [`super::reach_probe`]); `None` on every other path, so the
    /// accumulation itself costs one branch per block.
    probe_stats: Option<alloc::boxed::Box<ProbeStats>>,
    /// The DUBT tables carry a previous frame's positions and must be
    /// zeroed before this frame's first search (their u32 entries carry no
    /// coordinate origin). Set at `reset`, consumed at the first btlazy
    /// block that searches: a frame of RLE/raw blocks never touches the
    /// tables, so it never pays the clear.
    dubt_stale: bool,
    /// Gear-hash long-distance matcher state for chain rows with
    /// [`LevelParams::ldm`] (see [`super::ldm`]); `None` elsewhere.
    ldm: Option<LdmState>,
    /// The alphabet gate's parking spot for [`Self::ldm`]: the state is
    /// disabled for the frame, not dropped — a pooled state on a
    /// low-alphabet shape once re-allocated the table on every frame.
    ldm_parked: Option<LdmState>,
    /// Long-distance candidates of the block being scanned (regenerated
    /// per block by [`Self::ldm_generate`]).
    ldm_seqs: Vec<LdmSeq>,
    /// Consecutive winless scanned blocks past the reach horizon (see
    /// [`LDM_QUIET`]); counts toward the shutoff latch.
    ldm_quiet: u8,
    /// Shutoff: generation stops after [`LDM_QUIET`] quiet blocks and
    /// drops to one canary block per [`LDM_CANARY`] — shapes whose repeats
    /// all sit inside the chain's domain never pay the full split pass
    /// again, and a canary that finds the far class revives it. Reset per
    /// frame and job.
    ldm_dead: bool,
    /// Blocks since the last canary while shut off.
    ldm_canary: u8,
    /// Whether the mid-size population's alphabet gate already ran (once
    /// per frame, at the first parsed or filled block).
    ldm_checked: bool,
    /// Set by [`Self::prefill_window`]: this driver parses job strips (or a
    /// dictionary prefill), not a frame-continuous stream. Gates the opt
    /// fill-lag clamp, whose output-neutrality is proven for
    /// frame-continuous parses only (a job-restart parse measurably used
    /// the lagged region's candidates).
    strip_parse: bool,
    /// Dictionary frame on a chain or dfast row ([`Self::load_dictionary`]):
    /// the parse switches to libzstd's small-table dict-row semantics — chain
    /// rows take the 4-byte hash width, accept bar 4, no offset-pays gate,
    /// deeper walk; dfast rows take the 4-byte short-hash width (the long
    /// probe stays 8-byte). Cleared by [`Self::reset`]; a no-dict frame never
    /// touches it.
    dict_row: bool,
    /// Armed deep-offset ramp for the current job ([`RampGate`]); OFF on
    /// every single-job path.
    ramp: RampGate,
    /// Chain-scan catch-up cursor for dictionary rows (libzstd's
    /// `nextToUpdate`): the first position the next probe's catch-up fill
    /// has not inserted. `u64::MAX` disables the fill — the no-dict rows
    /// keep their tuned sparse insert grid (byte-load-bearing output).
    chain_filled: u64,
}

/// Borrowed window for the slice-compression path. The caller guarantees the
/// buffer stays alive and unmodified until the matcher is reset or dropped;
/// only the single thread that called `adopt_window` touches it.
struct ExtWindow {
    data: *const u8,
    len: usize,
}

// The pointer never escapes the matcher, and the compress_slice entry points
// keep the borrowed buffer on the same thread's stack for the whole call.
unsafe impl Send for MatchGeneratorDriver {}

#[cfg(feature = "std")]
use super::ldm::LdmSnapshot;

/// Payload ceiling for the small-dictionary strategy swap (libzstd's
/// <= 16 KiB clevels table boundary, keyed on the payload alone on the
/// loadDictionary route — see `load_dictionary`).
const SMALL_DICT_PAYLOAD_MAX: u64 = 16 * 1024;
/// Hash log of the <= 16 KiB table's chain rows, R12 geometry: the table's
/// H14 dilutes 4-byte candidate classes once the dictionary's content and
/// the payload share one table (~20 KiB of positions over 16 KiB of
/// buckets — hot 4-grams bury their older dict twins past any walk depth;
/// H15/H16 measure byte-identical, H14 loses ~11 B on the fixture).
const SMALL_DICT_HASH_LOG: u32 = 15;
/// Chain ring log of the same rows, R12 geometry: the ring must cover the
/// dict content plus the payload (up to 32 KiB — exactly C15). At the
/// table's C14 the payload's inserts alias the dict head's chain slots and
/// sever every walk into it (libzstd never runs this shape: its chainLog
/// clamps through `ZSTD_dictAndWindowLog`, and the CDict route keeps the
/// dict in its own match state).
const SMALL_DICT_CHAIN_LOG: u32 = 15;
/// Smallest largest prefix strip that engages the shared prefix fill;
/// below it the tail jobs' own parallel fills are the cheaper schedule.
#[cfg(feature = "std")]
pub(crate) const SPF_MIN_PREFIX: u64 = 8 * 1024 * 1024;
/// Shared-prefix-fill segment length: the soft bound between build polls
/// and the slack past the target boundary a segment may run while
/// searching for the next exact (batch-freeze) boundary.
#[cfg(feature = "std")]
pub(crate) const SPF_SEG: u64 = 1024 * 1024;

/// Ceiling for the chain heads' coordinate origin (`head_origin`): the
/// u64-domain entry resolve needs `pos + origin + 1 < 2^32` for live
/// entries, so the advance degrades into a real clear once the origin
/// would pass this bar — once per ~1 GiB of cumulative positions on one
/// pooled state, keeping frames below 3 GiB on exact semantics.
const HEAD_ORIGIN_CAP: u64 = 1 << 30;

/// Captured strip-fill state of a [`MatchGeneratorDriver`] (the streaming
/// core's shared prefix fill): the chain row's head/chain grid tables plus
/// the LDM fill state after a fill covering `[base, base + upto)` —
/// `upto` an LDM batch-freeze point, the one boundary kind where a fill
/// may be split and resumed exactly (see `LdmState::fill_to_freeze`).
/// Tail jobs whose strip extends the same prefix adopt it via
/// [`MatchGeneratorDriver::adopt_strip_snapshot`] instead of re-running
/// the whole strip fill.
#[cfg(feature = "std")]
pub(crate) struct StripSnapshot {
    /// The fused head+chain buffer; the split matches the capturing
    /// driver's `second`.
    tables: Vec<u32>,
    second: usize,
    ldm: Option<LdmSnapshot>,
    /// The builder's head coordinate origin: the adopted entries resolve
    /// against it, so the adopter must take it with the tables (its own
    /// origin is unrelated history).
    origin: u64,
    /// Absolute stream offset the fill covered.
    upto: u64,
}

/// Captured LDM fill state for the windowed capture band (the full-window
/// population of [`MatchGeneratorDriver::prefix_ldm_window`]): the LDM
/// table after a sequential fill of `[0, upto)` stopped at a batch-freeze
/// point. A windowed job adopts it and continues the fill to its own
/// start — and the adoption is byte-invariant in `upto`: a freeze-point
/// continuation is exact, so restoring any snapshot with `upto <= start`
/// and filling the remainder produces the same usable candidates as the
/// from-scratch fill of `[0, start)` (entries older than the job's window
/// die on the distance filter, and the round-robin bucket holds the
/// newest inserts either way). Which snapshot a job gets is therefore
/// pure scheduling, never output.
#[cfg(feature = "std")]
pub(crate) struct LdmPrefixSnapshot {
    pub(super) ldm: LdmSnapshot,
    /// Absolute stream offset the fill covered (the freeze point).
    pub(crate) upto: u64,
}

/// Resolve the active window (owned or borrowed). A free function so callers
/// can split-borrow `win`/`ext` against `&mut table`.
fn window_slice<'a>(win: &'a [u8], ext: Option<&'a ExtWindow>) -> &'a [u8] {
    match ext {
        None => win,
        // SAFETY: the ExtWindow invariant (caller buffer alive and unchanged
        // until reset) makes this slice valid for as long as the matcher is.
        Some(e) => unsafe { core::slice::from_raw_parts(e.data, e.len) },
    }
}

impl MatchGeneratorDriver {
    /// The window size frames compressed at `level` (and, when known, the
    /// source length) declare in their header; usable without constructing
    /// an instance. Must agree with the matcher's own reset call.
    pub fn window_for_level(level: Level, shape: InputShape) -> u64 {
        params_for(level, shape).window as u64
    }

    /// The opt family's tree-filled share of a job strip: libzstd zstdmt's
    /// default overlap for btopt/btultra is half the window
    /// (`ZSTDMT_overlapLog_default` 8); btultra2 keeps the whole window
    /// (9). The strip tree-fill is the tier's largest per-job cost
    /// (40-73% of summed job time across shapes; see dev/perf), so the
    /// non-ultra rows' lazy tree fill covers only the tail half of the
    /// strip while the LDM table (the far class's server on armed frames)
    /// still ingests the whole strip — the full-strip tree form is
    /// dll100-gated (see dev/negative). Ultra keeps the full domain —
    /// the density flagship's row, C's btultra2 parity.
    #[cfg(feature = "std")]
    fn opt_tree_strip(p: &LevelParams) -> u64 {
        let reach = p.chain_reach.unwrap_or(p.window) as u64;
        match p.strategy {
            Strategy::Opt(knobs) if !knobs.ultra => reach / 2,
            _ => reach,
        }
    }

    /// The multithreaded job strip: the dense matchers' search domain.
    /// LDM rows keep a wide frame window (their far reach) but only the
    /// chain's domain needs cross-job strip coverage — within a job the
    /// LDM table accumulates over the job's own span, and distances beyond
    /// the strip are a bounded per-job loss against the strip-fill and
    /// job-size cost a full-window strip would charge.
    pub fn strip_for_level(level: Level, shape: InputShape) -> u64 {
        let p = params_for(level, shape);
        p.chain_reach.unwrap_or(p.window) as u64
    }

    /// The streaming-MT job's history strip: the level's whole window for
    /// the chain rows (their LDM far class rides cross-job through the
    /// adopted window), the search domain for the opt and btlazy2 rows
    /// (their LDM restarts per job exactly like the bulk path, so a
    /// full-window job floor would only serialize sub-window streams
    /// without buying reach).
    pub fn stream_overlap_for(level: Level, shape: InputShape) -> u64 {
        let p = params_for(level, shape);
        match p.strategy {
            Strategy::Opt(_) | Strategy::BtLazy(_) => p.chain_reach.unwrap_or(p.window) as u64,
            _ => p.window as u64,
        }
    }

    /// Whether the row's strip fill is the chain grid plus the LDM split
    /// pass — the shape [`StripSnapshot`]'s build/adopt machinery
    /// implements. The opt/btlazy rows fill different tables (their
    /// `chain` stays empty) and must not engage the shared prefix fill.
    #[cfg(feature = "std")]
    pub(crate) fn spf_strip_fill(level: Level, shape: InputShape) -> bool {
        matches!(params_for(level, shape).strategy, Strategy::Chain(_))
    }

    /// The window whose jobs run cross-job LDM history under
    /// [`LdmArming::JobPrefix`] (the bulk path's LDM capture): the chain
    /// row at its stock reach, a Keep verdict (a shrunk frame abandons
    /// LDM) and a window at least the mid-size bar. Two bands share the
    /// arming: `[LDM_MIDSIZE_WINDOW, LDM_FULL_WINDOW)` runs whole-prefix
    /// strips (every job's window strip is a frame prefix), while
    /// `LDM_FULL_WINDOW` and beyond runs the windowed model (see
    /// [`Self::windowed_ldm_capture`]) — chain tables over the reach tail,
    /// LDM over the whole window through the shared prefix build's
    /// snapshots. `None` keeps the per-job [`LdmArming::Job`] model.
    #[cfg(feature = "std")]
    pub(crate) fn prefix_ldm_window(
        level: Level,
        shape: InputShape,
        choice: ReachChoice,
    ) -> Option<u64> {
        let p = params_for(level, shape);
        (Self::spf_strip_fill(level, shape)
            && p.ldm
            && choice == ReachChoice::Keep
            && p.window >= LDM_MIDSIZE_WINDOW)
            .then_some(p.window as u64)
    }

    /// Whether the capture window runs the windowed job model (the
    /// full-window band of [`Self::prefix_ldm_window`]): beyond
    /// [`LDM_FULL_WINDOW`] a job's window strip reaches back past the
    /// frame prefix into mid-stream history, so the job keeps its chain
    /// strip at the reach (the beyond-reach strip has exactly one reader,
    /// LDM) and takes its LDM history from the shared prefix build's
    /// snapshots instead (see `LdmPrefixSnapshot`).
    #[cfg(feature = "std")]
    pub(crate) fn windowed_ldm_capture(window: u64) -> bool {
        window >= LDM_FULL_WINDOW as u64
    }

    /// Whether the shape-adaptive reach probe (see `super::reach_probe`)
    /// may decide this frame's reach: only the Balanced chain row with its
    /// stock reach still in place — the opt rows share the `chain_reach`
    /// field (their tree search domain) without being probe subjects, and a
    /// shape-clamped reach means the window itself is small, with nothing
    /// left to trade away.
    pub fn reach_probe_eligible(level: Level, shape: InputShape) -> bool {
        let p = params_for(level, shape);
        matches!(p.strategy, Strategy::Chain(_)) && p.chain_reach == Some(KEEP_REACH)
    }

    /// [`Self::strip_for_level`] under the frame's reach probe result: a
    /// shrunk chain domain shrinks the strip with it — the strip needs to
    /// cover the dense search, and the LDM far class rides within-job
    /// history (the probe's own measurement priced the near-local shape
    /// without cross-strip far matches mattering).
    #[cfg(feature = "std")]
    pub(crate) fn strip_for_choice(level: Level, shape: InputShape, choice: ReachChoice) -> u64 {
        let p = params_for(level, shape);
        if choice == ReachChoice::Shrink && p.chain_reach == Some(KEEP_REACH) {
            SHRINK_REACH as u64
        } else {
            p.chain_reach.unwrap_or(p.window) as u64
        }
    }

    /// Declare the frame's reach probe result (see [`super::reach_probe`])
    /// before `reset`; the choice survives resets until changed.
    pub(crate) fn set_reach_choice(&mut self, choice: ReachChoice) {
        self.reach_choice = choice;
    }

    /// Clear the parse tables for a fresh deterministic parse on a driver
    /// whose allocations persist across reach flips: the probe's pooled
    /// driver parses keep and shrink sides (and the feedback prefix)
    /// back-to-back over the same bytes, and a reach flip no longer
    /// reallocates the tables (see [`Self::apply_level`]) — same-bytes
    /// residue would otherwise alias as candidates.
    pub(crate) fn clear_parse_tables(&mut self) {
        if !self.tables.is_empty() {
            clear_table(&mut self.tables);
        }
        if matches!(self.params.strategy, Strategy::BtLazy(_)) {
            self.dubt_table.fill(0);
        }
    }

    /// Donation mode (see [`Self::probe_stats`]): the real parse feeds the
    /// reach measurement while it runs, so the keep side's cost is the
    /// frame's own parse — entropy feedback, LDM and the gate included —
    /// not a re-parse without them. One branch per block otherwise.
    pub(crate) fn absorb_probe_stats(&mut self, literals: &[u8], seqs: &[SeqWord]) {
        let Some(stats) = &mut self.probe_stats else {
            return;
        };
        if seqs.is_empty() {
            // A zero-sequence block's literals are the block itself.
            // Direct field reads: the stats borrow keeps this disjoint.
            let start = (self.block_start - self.win_base) as usize;
            let win = window_slice(&self.win, self.ext.as_ref());
            stats.absorb_literals(&win[start..]);
        } else {
            stats.absorb_literals(literals);
            stats.absorb_seqs(seqs);
        }
    }

    /// Declare the driver's LDM arming context (see [`LdmArming`]) before
    /// `reset`; like the reach choice it survives resets until changed.
    pub(crate) fn set_ldm_arming(&mut self, arming: LdmArming) {
        self.ldm_arming = arming;
    }

    /// Largest block the frame may carry: the format caps blocks at the
    /// declared window (RFC 8878: Block_Maximum_Size = min(window, 128K)),
    /// so a forced or downsized window below 128 KiB shrinks the blocks.
    pub fn block_size(&self) -> usize {
        (crate::common::MAX_BLOCK_SIZE as usize).min(self.params.window)
    }

    /// The staged tail region behind `block_tail`'s current extension:
    /// the bytes the Read-path fill wrote, for the pre-split decision
    /// (owned-window drivers only).
    pub(crate) fn staged_window(&self) -> &[u8] {
        debug_assert!(self.ext.is_none());
        &self.win[self.win.len() - self.slice_size..]
    }

    /// The pre-split detector's effort level for the active row (see
    /// [`super::pre_split`]): libzstd's `splitLevels[]` strategy mapping —
    /// the lazy family walks chunk fingerprints (greedy and lazy share a
    /// row). Three exemptions, all measured on dll100/dll32 (see
    /// docs/src/dev/perf/encoding.md): the single-probe rows (fast/dfast)
    /// pay 4-6% of their throughput for noise-level size deltas, the
    /// tree-fill rows (btlazy2/opt) restart their per-block tree state on
    /// every cut (dll100 best −10% wall for a +0.06% size, opt +1.0%
    /// size), and the opt rows additionally carry the post-parse splitter
    /// on the same inputs.
    pub(crate) fn pre_split_level(&self) -> Option<super::pre_split::SplitLevel> {
        use super::pre_split::SplitLevel;
        match self.params.strategy {
            Strategy::Fast | Strategy::Dfast(_) | Strategy::BtLazy(_) | Strategy::Opt(_) => None,
            Strategy::Row(_) | Strategy::Chain(_) => match self.params.lazy_depth {
                0 | 1 => Some(SplitLevel::chunked(1)),
                _ => Some(SplitLevel::chunked(2)),
            },
        }
    }

    /// Create a matcher whose blocks hold `slice_size` bytes of input (the
    /// zstd block maximum is 128 KiB).
    pub fn new(slice_size: usize) -> Self {
        // Two windows of capacity: compacting down to MAX_WINDOW then only
        // after another MAX_WINDOW of input means the copy_within runs at
        // ~1x data volume instead of once per block.
        Self {
            win: Vec::with_capacity(2 * MAX_WINDOW + slice_size),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            tables: alloc::vec![0u32; 1usize << HASH_LOG],
            second: 1usize << HASH_LOG,
            row_heads: Vec::new(),
            opt_table: Vec::new(),
            bt: Vec::new(),
            hash3: Vec::new(),
            dubt_table: Vec::new(),
            dubt_bt: Vec::new(),
            probe: Vec::new(),
            gate_hold: false,
            opt_state: OptState::new(),
            opt_scratch: None,
            lazy_scratch: None,
            next_update: 0,
            gap_start: u64::MAX,
            // Epoch 0 is the never-valid state of a zeroed table.
            opt_origin: 0,
            head_origin: 0,
            strip_mark: 0,
            miss_count: 0,
            covered_fill: CoveredFill::Dense,
            scan_density: ScanDensity::Plain,
            params: LEVEL_PARAMS[1],
            shape: InputShape::default(),
            small_wide: None,
            small_wide_key: (0, 0),
            reach_choice: ReachChoice::Keep,
            ldm_arming: LdmArming::Frame,
            dubt_head: HeadPhase::Off,
            bt_step: BtStepPhase::Off,
            probe_stats: None,
            dubt_stale: false,
            ldm: None,
            ldm_parked: None,
            ldm_seqs: Vec::new(),
            ldm_quiet: 0,
            ldm_dead: false,
            ldm_canary: 0,
            ldm_checked: false,
            strip_parse: false,
            dict_row: false,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size,
            ramp: RampGate::OFF,
            chain_filled: u64::MAX,
        }
    }

    /// Create a matcher for direct-window mode: no owned window is allocated
    /// and blocks are handed over through [`Self::adopt_window`] instead of
    /// [`Matcher::block_tail`].
    pub fn new_direct() -> Self {
        Self {
            win: Vec::new(),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            tables: alloc::vec![0u32; 1usize << HASH_LOG],
            second: 1usize << HASH_LOG,
            row_heads: Vec::new(),
            opt_table: Vec::new(),
            bt: Vec::new(),
            hash3: Vec::new(),
            dubt_table: Vec::new(),
            dubt_bt: Vec::new(),
            probe: Vec::new(),
            gate_hold: false,
            opt_state: OptState::new(),
            opt_scratch: None,
            lazy_scratch: None,
            next_update: 0,
            gap_start: u64::MAX,
            opt_origin: 0,
            head_origin: 0,
            strip_mark: 0,
            miss_count: 0,
            covered_fill: CoveredFill::Dense,
            scan_density: ScanDensity::Plain,
            params: LEVEL_PARAMS[1],
            shape: InputShape::default(),
            small_wide: None,
            small_wide_key: (0, 0),
            reach_choice: ReachChoice::Keep,
            ldm_arming: LdmArming::Frame,
            dubt_head: HeadPhase::Off,
            bt_step: BtStepPhase::Off,
            probe_stats: None,
            dubt_stale: false,
            ldm: None,
            ldm_parked: None,
            ldm_seqs: Vec::new(),
            ldm_quiet: 0,
            ldm_dead: false,
            ldm_canary: 0,
            ldm_checked: false,
            strip_parse: false,
            dict_row: false,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size: 0,
            ramp: RampGate::OFF,
            chain_filled: u64::MAX,
        }
    }

    /// The small-band alphabet screen over the adopted window (see
    /// [`small_dense_wide`]), cached by window identity (the field's
    /// contract). True only for wide-alphabet (text-class) frames.
    fn win_small_wide(&mut self) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let key = (win.as_ptr() as usize, win.len());
        if self.small_wide.is_none() || self.small_wide_key != key {
            self.small_wide = Some(small_dense_wide(win));
            self.small_wide_key = key;
        }
        self.small_wide.unwrap()
    }

    /// The dense bars' verdict for the fast/dfast rows: the alphabet
    /// screen, or a tiny frame joined in unconditionally
    /// ([`SMALL_DENSE_TINY_MAX`]; the cached key's length is the screened
    /// window's).
    fn win_small_wide_fast(&mut self) -> bool {
        self.win_small_wide() || self.small_wide_key.1 <= SMALL_DENSE_TINY_MAX
    }

    /// Size the search tables for `params` — the table-family body of
    /// [`Self::apply_level`], shared with the dictionary row swap in
    /// [`Self::load_dictionary`] (a swapped frame re-keys its tables the
    /// same way a level change does). Exactly one table family is live
    /// per strategy; switching families drops the other's buffers. Within
    /// a family the reallocation keys on the tables' lengths, not on the
    /// whole params struct: a params-only change (the reach probe's
    /// chain_reach switch, a window clamp that leaves the logs) must not
    /// drop and re-zero same-sized tables — the probe's Keep/Shrink
    /// alternation once re-zeroed the chain family on every parse, a
    /// fixed cost that dwarfed RLE-class frames (zeros.balanced's 2.9x
    /// regression). Kept tables carry the previous parse's entries;
    /// candidates are window-guarded and byte-verified, the same
    /// accepted residue as the equal-params path (which never cleared
    /// either).
    fn resize_tables(&mut self, params: &LevelParams) {
        let heads_len = 1usize << params.hash_log;
        let heads_kept = self.second == heads_len;
        // The fused buffer preserves the two-table keep/refresh
        // semantics exactly: a kept head with a resized second table
        // truncates and re-extends (resize zeroes only the new tail),
        // which is today's fresh-second-table behavior; any head-size
        // change drops the whole buffer like the old two allocations.
        match params.strategy {
            Strategy::Fast => {
                if !heads_kept {
                    self.tables = alloc::vec![0u32; heads_len];
                    self.second = heads_len;
                } else if self.tables.len() != heads_len {
                    self.tables.truncate(heads_len);
                }
            },
            Strategy::Dfast(small_log) => {
                let total = heads_len + (1usize << small_log);
                if heads_kept && self.tables.len() == total {
                    // both kept
                } else if heads_kept {
                    self.tables.truncate(heads_len);
                    self.tables.resize(total, 0);
                } else {
                    self.tables = alloc::vec![0u32; total];
                    self.second = heads_len;
                }
            },
            Strategy::Chain(chain_log) => {
                let total = heads_len + (1usize << chain_log);
                if heads_kept && self.tables.len() == total {
                    // both kept
                } else if heads_kept {
                    self.tables.truncate(heads_len);
                    self.tables.resize(total, 0);
                } else {
                    self.tables = alloc::vec![0u32; total];
                    self.second = heads_len;
                }
            },
            Strategy::Row(row_log) => {
                // The row layout is structurally different from the
                // single-head families (packed tag+position entries,
                // cycling insertion), so a kept same-sized buffer
                // would carry heads-family residue: reallocate on any
                // switch, keep only a same-row resize. The head bytes
                // follow the same keep/refresh rule over the row
                // count (a row_log change inside one hash_log
                // resizes them).
                let rows = 1usize << params.hash_log.saturating_sub(row_log);
                let rows_kept = matches!(self.params.strategy, Strategy::Row(_))
                    && self.row_heads.len() == rows;
                if !rows_kept {
                    self.tables = alloc::vec![0u32; heads_len];
                    self.second = heads_len;
                    self.row_heads = alloc::vec![0u8; rows];
                } else if self.tables.len() != heads_len {
                    self.tables.truncate(heads_len);
                    self.tables.resize(heads_len, 0);
                    self.row_heads.truncate(rows);
                    self.row_heads.resize(rows, 0);
                }
            },
            Strategy::Opt(knobs) => {
                if self.opt_table.len() != 1usize << params.hash_log {
                    self.opt_table = alloc::vec![EMPTY; 1usize << params.hash_log];
                }
                // The tree ring: two link slots per ring position.
                if self.bt.len() != 2usize << knobs.bt_log {
                    self.bt = alloc::vec![EMPTY; 2usize << knobs.bt_log];
                }
                let want_h3 = usize::from(knobs.hash3_log > 0) << knobs.hash3_log;
                if self.hash3.len() != want_h3 {
                    self.hash3 = if knobs.hash3_log > 0 {
                        alloc::vec![EMPTY; 1usize << knobs.hash3_log]
                    } else {
                        Vec::new()
                    };
                }
                self.tables = Vec::new();
                self.second = 0;
            },
            Strategy::BtLazy(knobs) => {
                if self.dubt_table.len() != 1usize << params.hash_log {
                    self.dubt_table = alloc::vec![0u32; 1usize << params.hash_log];
                }
                if self.dubt_bt.len() != 2usize << knobs.bt_log {
                    self.dubt_bt = alloc::vec![0u32; 2usize << knobs.bt_log];
                }
                self.opt_table = Vec::new();
                self.bt = Vec::new();
                self.hash3 = Vec::new();
                self.tables = Vec::new();
                self.second = 0;
            },
        }
        if !matches!(params.strategy, Strategy::Opt(_) | Strategy::BtLazy(_)) {
            self.opt_table = Vec::new();
            self.bt = Vec::new();
            self.hash3 = Vec::new();
        }
        if !matches!(params.strategy, Strategy::BtLazy(_)) {
            self.dubt_table = Vec::new();
            self.dubt_bt = Vec::new();
        }
        if matches!(params.strategy, Strategy::Opt(_)) && self.opt_scratch.is_none() {
            self.opt_scratch = Some(OptScratch::new());
        }
        if matches!(params.strategy, Strategy::BtLazy(_)) && self.lazy_scratch.is_none() {
            self.lazy_scratch = Some(LazyScratch::new());
        }
    }

    /// Size the search tables for `level` (no-op when unchanged), so pooled
    /// states re-size at most once per level or hint change.
    fn apply_level(&mut self, level: Level) {
        let mut params = params_for(level, self.shape);
        // The frame's reach probe result: only the row's stock reach is
        // replaceable, never a shape-clamped one.
        if self.reach_choice == ReachChoice::Shrink && params.chain_reach == Some(KEEP_REACH) {
            params.chain_reach = Some(SHRINK_REACH);
        }
        // Fast/dfast LDM arming (R19): the row's stock window (768 KiB-2 MiB)
        // is the SCAN's domain; a frame-continuous caller whose declared
        // length reaches the full far window widens the declared window to
        // the far domain (the buffer, frame header and LDM reach) with the
        // scan domain preserved through `chain_reach` — the opt rows'
        // window/domain split. The length bar keeps every smaller frame's
        // header and parse byte-identical (the 32 MiB corpus population
        // included), and the arming-context check keeps MT jobs stock
        // (their per-job LDM restart would need the capture machinery;
        // recorded residue). A forced window at or past the far domain arms
        // on its own geometry. R20: a frame-continuous caller that ran the
        // pre-header far-class screen (`FrameScreened`, see
        // `fast_row_screen_pending`) arms the mid-size band too — the
        // screen, not the header-time geometry, separated the far-class
        // source from the same-size random one.
        let declared = |bar: u64| matches!(self.shape.len, Some(n) if n >= bar);
        if params.ldm
            && matches!(params.strategy, Strategy::Fast | Strategy::Dfast(_))
            && params.window < LDM_FULL_WINDOW
            && match self.ldm_arming {
                LdmArming::Frame => declared(LDM_FULL_WINDOW as u64),
                LdmArming::FrameScreened => declared(LDM_MIDSIZE_WINDOW as u64),
                _ => false,
            }
        {
            params.chain_reach = Some(params.window);
            params.window = LDM_FULL_WINDOW;
        }
        // The reach change alone must not re-derive anything: the tables'
        // sizes are reach-independent, and the reach flips twice per probe
        // (keep, shrink, then the executed choice) plus once per shrunk
        // restart — a full-table realloc on each flip is pure page-fault
        // churn. Only table-shaping fields (strategy with its logs, hash
        // log, window) re-derive; every reach consumer reads `self.params`
        // live, and the reach-flipping sites clear their residue
        // explicitly (`restart_shrunk`, the probe parse below).
        let tables_change = params.strategy != self.params.strategy
            || params.hash_log != self.params.hash_log
            || params.window != self.params.window;
        if tables_change {
            self.resize_tables(&params);
            // The owned window (streaming path) compacts down to the level's
            // window; grow the buffer so block_tail's set_len stays inside
            // the capacity. Direct-window matchers (slice size zero) never
            // use block_tail.
            if self.slice_size > 0 {
                let need = 2 * params.window + self.slice_size;
                if self.win.capacity() < need {
                    self.win.reserve(need - self.win.len());
                }
            }
        }
        self.params = params;
        // The size gate: the split pass costs real cycles per byte even
        // cheapened (see ldm.rs), so LDM arms only where the window the
        // source clamp left is worth it —
        // the bar depends on the driver's arming context ([`LdmArming`]),
        // so this check runs on every apply, not only on params changes
        // (a pooled driver can re-arm between identical-param frames). A
        // shrunk frame abandons the far domain entirely: the probe decided
        // near-locality from a parse without LDM (its candidates cannot
        // surface in the probe span at the stock reach), and the executed
        // shrunk parse matches that measurement exactly.
        let ldm_wanted = params.ldm
            && params.chain_reach != Some(SHRINK_REACH)
            && matches!(
                params.strategy,
                Strategy::Fast
                    | Strategy::Dfast(_)
                    | Strategy::Chain(_)
                    | Strategy::Opt(_)
                    | Strategy::BtLazy(_)
            )
            && ldm_min_window(self.ldm_arming).is_some_and(|bar| params.window >= bar);
        let ldm_sized = self
            .ldm
            .as_ref()
            .is_some_and(|l| l.window() == params.window as u64);
        self.ldm = match (ldm_wanted, ldm_sized) {
            (true, true) => self.ldm.take(),
            (true, false) => match self.ldm_parked.take() {
                Some(parked) if parked.window() == params.window as u64 => Some(parked),
                _ => Some(LdmState::new(
                    (params.window as u64).ilog2(),
                    params.window as u64,
                )),
            },
            (false, _) => None,
        };
    }

    /// Point the window at caller-owned memory: `data` holds the bytes at
    /// absolute offset `base`. The caller must then declare the block to
    /// match with [`Self::set_block`]. `data` must stay alive and unmodified
    /// until the next `adopt_window` or `reset`.
    pub fn adopt_window(&mut self, data: &[u8], base: u64) {
        debug_assert!(self.ext.is_some() || self.win.is_empty());
        debug_assert_ne!(data, &[][..]);
        self.ext = Some(ExtWindow {
            data: data.as_ptr(),
            len: data.len(),
        });
        self.win_base = base;
    }

    /// Load dictionary content as the frame's match history, for the
    /// owned-window path (call after `reset`, before the first
    /// `block_tail`). The dictionary occupies positions `[0, n)`; frame
    /// data starts at `n`. `rep` is the dictionary's repeated-offset
    /// history (already validated against the content length by the
    /// caller). Content beyond the level's window is unreachable and
    /// dropped. The prefill indexes the surviving content under the
    /// dictionary grid (see `FillGrid::Dictionary`; seed detection
    /// included), so matches into the dictionary cost nothing extra at
    /// scan time.
    pub fn load_dictionary(&mut self, content: &[u8], rep: [u32; 3], level: Level) {
        debug_assert!(self.ext.is_none() && self.win.is_empty() && self.pos == 0);
        let dict_len = content.len() as u64;
        let keep = content.len().min(self.params.window);
        let content = &content[content.len() - keep..];
        self.win.clear();
        self.win.extend_from_slice(content);
        self.win_base = 0;
        self.pos = keep as u64;
        self.anchor = keep as u64;
        self.block_start = keep as u64;
        self.block_end = keep as u64;
        self.rep = rep;
        self.rep_pending = 0;
        // Chain and dfast rows parsing over dictionary content switch to
        // libzstd's small-table dict-row semantics; the knobs alone
        // measurably do nothing (or lose), the combinations convert
        // (fixture grid, 54-file systemd holdout, summed bytes, zstdx-raw
        // dict at -5/-9):
        // - hash width 4: libzstd hashes minMatch bytes, and its small-input tables select width-4
        //   rows for the lazy family at levels 5-12 and dfast at 4 (payload+dict <= 128 KiB);
        //   near-duplicate dict lines diverging at byte 5 are candidates there, invisible to our
        //   5-byte width. Alone the dilution loses (8708 -> 8736 / 8442 -> 8451 B).
        // - accept bar 4: greedy/lazy/lazy2 store any candidate >= 4 (the row's searchLength only
        //   shapes the hash); ours is min_match 5. Worth 103/166 B on top of the rest; without the
        //   width change it does nothing (the 4-byte twins are never candidates).
        // - no offset-pays gate: the far-dict matches the gate rejects (5 bits/byte literal bar)
        //   are exactly the dict's payload — bypassing it is worth 49/95 B on top of the two above.
        // - walk depth x8 of the no-dict budget: the 4-byte table is dilute; the newest-first walk
        //   needs the extra attempts to reach older dict twins (x2: 8640/8278; x4: 8551/8269; x8:
        //   8518/8259, already past the zstd CLI's 8272; x16: 8499/8259).
        // Combined at x8: -5 raw 8708 -> 8518 (CLI 8230), -9 raw
        // 8442 -> 8259. The dfast row's width-4 short hash carries no extra
        // knobs (its accept bar is already 4 and its probes are
        // single-candidate); the long probe stays 8-byte — libzstd's dfast
        // hashes 8 there whatever the row. Dict frames are not a hot path
        // (1.4 ms/file on the fixture at -5/-9 either way); no-dict params
        // and widths stay put (byte-identical no-dict output).
        if matches!(
            self.params.strategy,
            Strategy::Chain(_) | Strategy::Dfast(_) | Strategy::Row(_)
        ) {
            self.dict_row = true;
        }
        if matches!(self.params.strategy, Strategy::Chain(_) | Strategy::Row(_)) {
            self.params.min_match = MIN_MATCH as u32;
        }
        if matches!(self.params.strategy, Strategy::Chain(_)) {
            self.params.search_depth *= 8;
        }
        // libzstd resolves a copied dictionary frame's parameters by the
        // PAYLOAD's size alone — the dict's bytes are match-state content,
        // not counted (`ZSTD_getCParamRowSize` on the loadDictionary route):
        // a payload <= 16 KiB selects the <= 16 KiB clevels table (verified
        // per knob on the fixture: overriding the CLI's default -4/-12/-16
        // row value by value — wlog/clog/hlog/slog/mml/tlen — leaves its
        // output byte-identical; its rows 4-8 are plain-chain greedy/lazy/
        // lazy2 at W14/H14/C14, 9-10 btlazy2, 11-12 btopt, 13-15 btultra,
        // 16+ btultra2, all at C15). Our ladder keeps the large-table
        // strategy classes with only window/hash clamps — the -4..-8 and
        // -11..-17 dict residues — so the frame adopts the small-table row
        // wholesale: the chain rows 4-8 and the opt rows 11-22 (the tree
        // rows 9-10 stay ours; the fixture has them at or past the CLI).
        // The frame window keeps its shape-clamped size (>= libzstd's
        // W14): the swap targets the strategy class and table geometry,
        // not the reach. Unknown payload sizes (unpledged streams) keep
        // the large-table row, like libzstd's CONTENTSIZE_UNKNOWN.
        let payload = self.shape.len.map(|n| n.saturating_sub(dict_len));
        if let Some(payload) = payload
            && payload <= SMALL_DICT_PAYLOAD_MAX
        {
            match small_dict_row(level.as_i32(), self.params.window, self.shape) {
                // Chain depths are tuned past libzstd's 1<<S
                // (16/8/16/64/256): the dict's dense twin field converts
                // extra attempts at 4-6 (fixture sweep 1x/4x/8x: -4
                // +1.6/+0.9/+0.8%, -5 +3.0/+1.4/+1.2%, -6 +2.4/+2.1/flat,
                // 7-8 flat from 64/256 up — re-confirmed saturated under
                // the R12 ring/hash/fill geometry).
                Some(SmallDictRow::Chain { depth, lazy }) => {
                    let mut row = self.params;
                    row.hash_log = SMALL_DICT_HASH_LOG;
                    row.strategy = Strategy::Chain(SMALL_DICT_CHAIN_LOG);
                    row.search_depth = depth;
                    row.lazy_depth = lazy;
                    row.min_match = MIN_MATCH as u32;
                    self.dict_row = true;
                    // The adopted row starts from fresh tables whatever
                    // the previous family left (a kept dfast/row buffer
                    // would seed the head with foreign-layout entries on
                    // <= 8 KiB frames, where the logs clamp to 14).
                    self.second = 0;
                    self.resize_tables(&row);
                    self.params = row;
                },
                // The opt rows keep the table's searchLog and hashLog
                // (ring C15) with the price model, searchLength and
                // targetLength tuned past the table rows on the fixture
                // (see `small_dict_opt_row`): the early-stop targetLength
                // and btopt's integer prices each lose 1-2.5 pp raw here,
                // and the knobs saturate — every level 11-22 parses to the
                // same bytes. The tree indexes the dictionary lazily from
                // `next_update` 0 (the `fill_window_grid` arm below),
                // matching libzstd's load-time `ZSTD_updateTree` coverage.
                Some(SmallDictRow::Opt(row)) => {
                    self.resize_tables(&row);
                    self.params = row;
                },
                None => {},
            }
        }
        // The owned window is filled above; prefill over the caller's slice
        // avoids the self-borrow (identical bytes). The dictionary grid:
        // libzstd fills dictionary content more densely than an mt strip
        // (see FillGrid::Dictionary) — small payloads parse against this
        // window, so candidate coverage dominates.
        self.fill_window_grid(content, 0, FillGrid::Dictionary, LdmStripFill::Ingest);
    }

    /// Declare `[start, end)` (absolute offsets inside the adopted window)
    /// as the block to match next. Matching restarts at `start`; the history
    /// below it is only a match source.
    pub fn set_block(&mut self, start: u64, end: u64) {
        debug_assert!(start >= self.win_base);
        debug_assert!(end <= self.win_base + self.ext.as_ref().map_or(0, |e| e.len) as u64);
        self.pos = start;
        self.block_start = start;
        self.block_end = end;
        self.anchor = start;
    }

    /// Forbid repcode references until three literal-offset sequences have
    /// rewritten the repeated-offset history. Used by multithreaded jobs
    /// that start mid-frame: the decoder's history there is unknown, and a
    /// repcode reference to a stale value would copy from the wrong place.
    /// Mirrors libzstd's `ZSTD_invalidateRepCodes`.
    pub fn gate_repcodes(&mut self) {
        self.rep_pending = 3;
    }

    /// Arm the deep-offset ramp for a job that starts at `job_start`
    /// (see `RampGate`); `depth == 0` disarms it. Job zero needs no ramp:
    /// no output exists below the frame start, so cross-boundary reads are
    /// impossible there.
    ///
    /// Panics on the tree rows (`BtLazy`, `Opt`): their parses never read
    /// the gate, so an armed ramp would be silently ignored — the frame
    /// stays legal, but a piece-decode experiment on it measures nothing
    /// (crossing reads of unbounded shallowness fail the executor's
    /// validation or, worse, slip past it). Fail fast instead. The wiring
    /// points for a real integration: in `dubt::find_best` the rep loop
    /// and the sorted descent (band rejection per candidate); in
    /// `btlazy::run_block_lazy` the depth-0 and offset-2 `rep_probe`s, the
    /// literal-offset backward extension (floor at the boundary, like
    /// `RampGate::ext_floor`) and the LDM far-candidate validation; the
    /// opt family needs the same over its DP collection walk.
    pub fn arm_ramp(&mut self, job_start: u64, depth: u64) {
        assert!(
            !matches!(self.params.strategy, Strategy::BtLazy(_) | Strategy::Opt(_)),
            "the deep-offset ramp is not wired into the btlazy2/opt parses (it holds for the \
             fast/dfast/chain rows only); refusing to arm",
        );
        self.ramp = if depth == 0 {
            RampGate::OFF
        } else {
            RampGate {
                start: job_start,
                end: job_start.saturating_add(depth),
                depth,
            }
        };
    }

    /// Prepare the matcher for a multithreaded job and index a strip of
    /// borrowed history — `data` holds the bytes at absolute offset `base` —
    /// into the search tables, so a scan that starts after it can match into
    /// it (the job path adopts the previous job's tail as match window;
    /// without this pass the tables hold no position inside that window and
    /// sequences never reference it). Grid positions are linked
    /// oldest-to-newest, keeping the newest-wins order the walks rely on.
    /// The opt strategies need no explicit fill: their tree fills lazily
    /// from `next_update`, so rewinding it to the strip start makes the
    /// first search index the strip through the regular tree-fill path.
    ///
    /// The u32 head tables are zeroed first, even for an empty strip: the
    /// pooled state carries whatever earlier jobs — this frame or a
    /// previous one — left, and a leftover entry that decodes into the
    /// window with matching bytes is a legal-looking candidate whose
    /// presence depends on which worker ran which job. Clearing makes the
    /// candidates a function of this job's strip and scan alone, so the
    /// frame bytes stay reproducible; libzstd's job path clears its tables
    /// per job for the same reason. The chain strategy's link table needs
    /// no clear: a chain slot is only ever read at a candidate position,
    /// and candidates arise only from the cleared head table or from head
    /// inserts this job links on the spot (`insert_linked_at` — the
    /// link-less form once let a pooled previous job's slot resolve a
    /// head-hopped candidate, making the output claim-order-dependent);
    /// slots below this job's inserts stay stale and unreachable. The
    /// dfast `chain` buffer is a second probed head table and is cleared.
    /// The opt tables need no clear (their entries carry the coordinate
    /// origin, advanced by the per-job reset).
    /// An MT job adopting `data` as its history strip at absolute offset
    /// `base`: [`Self::prefill_window`]'s full semantics, with the opt
    /// rows' tree fill bounded to the strip's tail half
    /// ([`Self::opt_tree_strip`]) — an assignment, not a further min: a
    /// fresh state's cursor sits at 0 and a pooled one at the previous
    /// job's end, and both must land on the bound, not under it. The LDM
    /// block inside `prefill_window` still ingests the whole strip (the
    /// far class's server on armed frames; the whole-strip-halved form is
    /// dll100-gated, see dev/negative).
    #[cfg(feature = "std")]
    pub(crate) fn prefill_job_strip(&mut self, data: &[u8], base: u64) {
        self.prefill_window(data, base);
        if matches!(self.params.strategy, Strategy::Opt(_)) {
            let tree_back = Self::opt_tree_strip(&self.params);
            let bound = base + (data.len() as u64).saturating_sub(tree_back);
            self.next_update = self.next_update.max(bound);
        }
    }

    /// Capture the current strip-fill state (see [`StripSnapshot`]):
    /// valid right after `prefill_window`/`strip_fill_continue` on a
    /// chain-row driver, where the strip fill is the head/chain grid plus
    /// the LDM split pass.
    #[cfg(feature = "std")]
    pub(crate) fn snapshot_strip_fill(&self, upto: u64) -> StripSnapshot {
        debug_assert!(matches!(self.params.strategy, Strategy::Chain(_)));
        StripSnapshot {
            tables: self.tables.clone(),
            second: self.second,
            ldm: self.ldm.as_ref().map(LdmState::snapshot),
            origin: self.head_origin,
            upto,
        }
    }

    /// Continue this driver's own strip fill from `[base, base + from)`
    /// to `[base, data.len())` — the adopter's half of the shared prefix
    /// fill (see [`StripSnapshot`]). No clears, no seed: exactly the work
    /// a from-scratch `prefill_window` over the longer span would still
    /// do after having filled the shorter one, resumed at a freeze point
    /// (see `strip_fill_segment`).
    #[cfg(feature = "std")]
    pub(crate) fn strip_fill_continue(&mut self, data: &[u8], base: u64, from: u64) {
        debug_assert!(matches!(self.params.strategy, Strategy::Chain(_)));
        if let Some(ldm) = self.ldm.as_mut() {
            debug_assert_eq!(ldm.fed(), base + from);
            if data.len() as u64 > from {
                ldm.fill(data, base, base + from, base + data.len() as u64);
            }
        }
        if data.len() < HASH_READ {
            return;
        }
        let last = data.len() - HASH_READ;
        self.chain_grid_fill(data, base, from, last);
    }

    /// The builder's half of the shared prefix fill: advance the strip
    /// fill from `[base, base + from)` to the first LDM batch-freeze at
    /// or beyond `soft` (bounded by `data.len()`), returning the absolute
    /// freeze point — the boundary a snapshot may be taken at (see
    /// `LdmState::fill_to_freeze` for why freeze points are the exact
    /// boundaries). `None` when no freeze lands before `data.len()`.
    #[cfg(feature = "std")]
    pub(crate) fn strip_fill_segment(
        &mut self,
        data: &[u8],
        base: u64,
        from: u64,
        soft: u64,
    ) -> Option<u64> {
        debug_assert!(matches!(self.params.strategy, Strategy::Chain(_)));
        let upto = self.ldm.as_mut().and_then(|ldm| {
            debug_assert_eq!(ldm.fed(), base + from);
            if data.len() as u64 > from {
                ldm.fill_to_freeze(data, base, base + from, base + data.len() as u64, soft)
            } else {
                None
            }
        })?;
        // The grid fill covers the same prefix [0, upto): its stride
        // alignment resumes from `from` and its extent is the freeze
        // point's HASH_READ margin, exactly as a from-scratch fill of
        // that prefix would cover.
        if upto > base + HASH_READ as u64 {
            self.chain_grid_fill(data, base, from, (upto - base - HASH_READ as u64) as usize);
        }
        Some(upto)
    }

    /// Adopt a prefix snapshot as this job's strip prefill and continue
    /// the fill to this job's own strip end: `data` is the job's whole
    /// strip at absolute offset `base`, `snap.upto` the prefix the
    /// snapshot covers. Table contents afterwards are bit-identical to
    /// `prefill_window(data, base)`; the non-table fields mirror it too,
    /// and the job-start seed scan runs per job (it reads the strip tail).
    #[cfg(feature = "std")]
    pub(crate) fn adopt_strip_snapshot(&mut self, snap: &StripSnapshot, data: &[u8], base: u64) {
        debug_assert!(matches!(self.params.strategy, Strategy::Chain(_)));
        debug_assert_eq!(self.tables.len(), snap.tables.len());
        debug_assert_eq!(self.second, snap.second);
        debug_assert!(snap.upto <= base + data.len() as u64);
        self.dubt_head = HeadPhase::Off;
        self.bt_step = BtStepPhase::Off;
        self.tables.clone_from(&snap.tables);
        // The snapshot's entries carry the builder's origin; the full
        // table copy leaves no adopter residue, so no invalidation is
        // needed — the origins simply swap.
        self.head_origin = snap.origin;
        self.gap_start = u64::MAX;
        self.gate_hold = false;
        self.strip_parse = true;
        self.ldm_quiet = 0;
        self.ldm_dead = false;
        self.ldm_canary = 0;
        match (&mut self.ldm, &snap.ldm) {
            (Some(ldm), Some(snap_ldm)) => ldm.restore(snap_ldm),
            (None, None) => {},
            _ => unreachable!("same row params arm LDM identically"),
        }
        // The adopter's probe walk collapses on a uniform strip exactly
        // like the from-scratch fill's (the remainder fill below stays on
        // the stock loop — snapshot continuations are alive-frame
        // exotica, not the zeros-class fast path).
        let uniform = strip_is_uniform(data);
        self.seed_gate_probe(data, uniform);
        self.strip_fill_continue(data, base, snap.upto - base);
        if data.len() < HASH_READ {
            return;
        }
        let last = data.len() - HASH_READ;
        self.acquire_seed(data, last);
    }

    /// Reset the incompressibility gate's repeat probe to exactly the
    /// prefilled history: the per-block sample pass `skip_if_incompressible`
    /// accumulates, replayed over the strip up front — same block-sized
    /// segments, same per-segment stride — so the filter ends where a
    /// from-scratch scan of the strip would have left it (block-phase
    /// included: a repeat at a block-multiple displacement finds its twin
    /// on the segment grid a scan would have planted, which a single
    /// whole-strip stride misses for most phases). A block whose only
    /// twins sit inside the job's matchable history — near twins in a
    /// reach strip, far twins in a window strip the armed LDM serves —
    /// then probes a hit instead of gating. The clear matters as much as
    /// the replay: a pooled matcher's probe otherwise carries whatever
    /// earlier jobs sampled, so the gate's verdicts (and the frame bytes)
    /// depended on which worker ran which job.
    fn seed_gate_probe(&mut self, data: &[u8], uniform: bool) {
        if self.probe.is_empty() {
            if data.len() < GATE_MIN_BLOCK {
                return;
            }
            self.probe.resize(1 << GATE_PROBE_LOG, 0);
        } else {
            self.probe.fill(0);
            // The clear is the whole replay for a strip that cannot hold
            // one gate block: the uniform sample below would read off a
            // zero-length strip's dangling pointer (a pooled probe warm
            // from a previous job met an empty segment at a freeze
            // boundary and faulted at address 1).
            if data.len() < GATE_MIN_BLOCK {
                return;
            }
        }
        if uniform {
            // Every sample of a uniform strip hashes to one slot with one
            // tag: the walk's final table is the clear plus a single
            // store (any position reads the same 8 bytes).
            let h = read8(data, 0).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
            self.probe[(h >> (64 - GATE_PROBE_LOG)) as usize] = h as u32;
            return;
        }
        #[cfg(feature = "job_trace")]
        let trace_gate = std::time::Instant::now();
        let block = self.block_size();
        let mut off = 0;
        while off < data.len() {
            let seg = (data.len() - off).min(block);
            if seg >= GATE_MIN_BLOCK {
                let stride = (seg >> 11) | 1;
                let mut i = off;
                while i + HASH_READ <= off + seg {
                    let h = read8(data, i).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
                    self.probe[(h >> (64 - GATE_PROBE_LOG)) as usize] = h as u32;
                    i += stride;
                }
            }
            off += seg;
        }
        #[cfg(feature = "job_trace")]
        super::job_trace::add_gate(trace_gate);
    }

    /// The chain row's stride-3 grid fill over `[resume, last)` (see
    /// `prefill_window`'s Chain arm — the same loop, entry point split so
    /// a continuation resumes at the aligned stride position the
    /// from-scratch pass would next take).
    #[cfg(feature = "std")]
    fn chain_grid_fill(&mut self, data: &[u8], base: u64, from: u64, last: usize) {
        let hash_log = self.params.hash_log;
        let (table, chain) = self.tables.split_at_mut(self.second);
        let chain_mask = chain.len() - 1;
        // The from-scratch loop runs idx = 0, 3, 6, ... while idx < last;
        // a prefix fill to `from` exited at the first aligned idx >=
        // from - HASH_READ, which is exactly where this resumes.
        let mut idx = (from as usize)
            .saturating_sub(HASH_READ)
            .div_ceil(PREFILL_STRIDE)
            * PREFILL_STRIDE;
        let origin = self.head_origin;
        while idx < last {
            let abs = base + idx as u64;
            // SAFETY: the hash masks to hash_log bits, the absolute
            // position to the chain size (absolute key; see emit_chain's
            // note on the walk side's indexing).
            unsafe {
                let h = hash_at_log(data, idx, hash_log);
                let head = *table.get_unchecked(h);
                *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                *table.get_unchecked_mut(h) = pack_head(abs, origin);
            }
            idx += PREFILL_STRIDE;
        }
        // A fill writer the scan cursor never sees (spf builders and
        // adopters run fills without scanning): fold its extent into the
        // origin's write mark.
        self.strip_mark = self.strip_mark.max(base + last as u64);
    }

    pub fn prefill_window(&mut self, data: &[u8], base: u64) {
        self.fill_window_grid(data, base, FillGrid::Strip, LdmStripFill::Ingest);
    }

    /// The windowed capture band's job prefill, chain side only: exactly
    /// [`Self::prefill_window`] over the job's reach tail (heads, links,
    /// gate replay and seed scan identical to a stock job's), except the
    /// LDM state is left untouched — [`Self::windowed_ldm_prefill`] owns
    /// it, filling the whole window span instead of the strip.
    #[cfg(feature = "std")]
    pub(crate) fn prefill_job_strip_chain(&mut self, data: &[u8], base: u64) {
        self.fill_window_grid(data, base, FillGrid::Strip, LdmStripFill::Defer);
    }

    /// The windowed capture band's LDM half (see
    /// [`MatchGeneratorDriver::windowed_ldm_capture`]): seed the LDM state
    /// for a job starting at `job_start` whose LDM window is
    /// `[ldm_base, job_start)` inside `ldm_win`, either from scratch
    /// (`snap = None`; `ldm_base` must be 0, so the restart-and-fill
    /// reproduces the shared build's own `[0, job_start)` fill bit for
    /// bit) or by adopting a snapshot and continuing the fill from its
    /// freeze point (byte-invariant in `upto`; a snapshot older than
    /// `ldm_base` re-arms there — the byte-derived rolling state equals
    /// the continuous one, and the entries the gap leaves missing sit
    /// beyond the job's window and die on the distance filter).
    #[cfg(feature = "std")]
    pub(crate) fn windowed_ldm_prefill(
        &mut self,
        ldm_win: &[u8],
        ldm_base: u64,
        job_start: u64,
        snap: Option<&LdmPrefixSnapshot>,
    ) {
        debug_assert_eq!(ldm_base + ldm_win.len() as u64, job_start);
        let Some(ldm) = self.ldm.as_mut() else {
            debug_assert!(snap.is_none(), "same row params arm LDM identically");
            return;
        };
        self.ldm_quiet = 0;
        self.ldm_dead = false;
        self.ldm_canary = 0;
        match snap {
            Some(s) => ldm.restore(&s.ldm),
            None => {
                debug_assert_eq!(ldm_base, 0, "stock windowed jobs are prefix strips");
                ldm.restart(0);
            },
        }
        #[cfg(feature = "job_trace")]
        let trace_ldm = std::time::Instant::now();
        let from = ldm.fed().max(ldm_base);
        if from < job_start {
            ldm.fill(ldm_win, ldm_base, from, job_start);
        }
        #[cfg(feature = "job_trace")]
        super::job_trace::add_ldm_fill(trace_ldm);
        // The job-start screens must see the history the LDM serves, not
        // just the chain's reach tail: the incompressibility gate's probe
        // replay and the periodic seed scan run over the window span
        // (replacing the tail replay the chain prefill did — the same
        // whole-strip seeding the mid-size capture's jobs get, and
        // load-bearing for exactly its reason: a fresh worker's gate
        // otherwise kills far-only blocks before LDM sees them, and a
        // repeat period between the reach and the window loses its seed).
        let uniform = strip_is_uniform(ldm_win);
        self.seed_gate_probe(ldm_win, uniform);
        if ldm_win.len() >= HASH_READ {
            self.acquire_seed(ldm_win, ldm_win.len() - HASH_READ);
        }
    }

    /// The windowed capture band's builder half (see
    /// `LdmPrefixSnapshot`): advance this driver's LDM fill from
    /// `[base, base + from)` to the first batch-freeze at or beyond
    /// `soft` (bounded by `data.len()`), chain tables untouched. `None`
    /// when no freeze lands before the end.
    #[cfg(feature = "std")]
    pub(crate) fn ldm_fill_segment(
        &mut self,
        data: &[u8],
        base: u64,
        from: u64,
        soft: u64,
    ) -> Option<u64> {
        let ldm = self.ldm.as_mut()?;
        debug_assert_eq!(ldm.fed(), base + from);
        if data.len() as u64 > from {
            ldm.fill_to_freeze(data, base, base + from, base + data.len() as u64, soft)
        } else {
            None
        }
    }

    /// Capture the LDM fill state for [`Self::windowed_ldm_prefill`];
    /// valid right after `ldm_fill_segment` (or the empty-strip restart
    /// of `prefill_window`).
    #[cfg(feature = "std")]
    pub(crate) fn ldm_snapshot(&self, upto: u64) -> LdmPrefixSnapshot {
        let ldm = self.ldm.as_ref().expect("the capture class arms LDM");
        LdmPrefixSnapshot {
            ldm: ldm.snapshot(),
            upto,
        }
    }

    /// Index a history window into the search tables under one of the two
    /// fill geometries (see [`FillGrid`]): the shared prologue and strategy
    /// dispatch of [`Self::prefill_window`] and the dictionary load.
    /// `ldm_fill` defers the strip's LDM ingestion to the caller (the
    /// windowed capture band's split prefill).
    fn fill_window_grid(&mut self, data: &[u8], base: u64, grid: FillGrid, ldm_fill: LdmStripFill) {
        // The head applies only to a genuinely cold start: a non-empty
        // strip (mt jobs with history, dictionary content) is warm, while
        // the empty strip is mt job zero — a frame start, so the head
        // arms exactly as at reset (pooled states arrive here directly).
        self.dubt_head = if data.is_empty() && self.head_eligible() {
            HeadPhase::Armed
        } else {
            HeadPhase::Off
        };
        // The same cold-start rule for the btlazy2 rows' probe step: the
        // empty strip is a genuine frame start (mt job zero, or reset),
        // anything prefilled is warm.
        self.bt_step = if data.is_empty() && matches!(self.params.strategy, Strategy::BtLazy(_)) {
            BtStepPhase::Armed
        } else {
            BtStepPhase::Off
        };
        if matches!(self.params.strategy, Strategy::Chain(_)) && grid == FillGrid::Strip {
            // The chain row's head invalidation is the coordinate-origin
            // advance (see `head_origin`), not the clear: `strip_mark`
            // covers every position this state has written since the last
            // advance, so every stale entry rebuilds as a beyond-reach
            // distance — exactly the clear's semantics, at O(1) cost
            // instead of an 8 MiB NT clear per job. The resolve runs in
            // the u64 domain, where the empty sentinel dies by arithmetic
            // (`q >= pos+1 > reach`) — that needs a live entry's
            // `pos + origin + 1` below 2^32, so the origin stays capped:
            // once it would pass [`HEAD_ORIGIN_CAP`] the advance becomes a
            // real clear and the origin restarts (one clear per ~1 GiB of
            // cumulative positions on a pooled state, amortized to noise
            // against the per-job clear it replaced; frames past 3 GiB
            // degrade exactly where today's u32 positions already wrap).
            // Dictionary loads keep the clear below: the dtlm backfills
            // read the empty sentinel off the slots themselves.
            if self.head_origin + self.strip_mark + 1 > HEAD_ORIGIN_CAP {
                clear_table(&mut self.tables[..self.second]);
                self.head_origin = 0;
            } else {
                self.head_origin += self.strip_mark + 1;
            }
            self.strip_mark = 0;
        } else {
            #[cfg(feature = "job_trace")]
            let trace_clear = std::time::Instant::now();
            clear_table(&mut self.tables[..self.second]);
            #[cfg(feature = "job_trace")]
            super::job_trace::add_clear(trace_clear);
        }
        // The row heads choose slots, so they carry the same per-job
        // determinism contract as the head table (a stale head would make
        // this job's slot layout depend on the pooled state's history).
        self.row_heads.fill(0);
        // The DUBT finder's entries carry no epoch tag, so a job restarts
        // its tree from scratch regardless of strip length. The eager
        // heads clear is deferred to the first searching block instead
        // (`dubt_stale`): nothing between the prefill and it reads the
        // heads (the BtLazy arm prefills nothing into the tree — the fill
        // is the finder's own, from `next_update`), so the deferred clear
        // is the only one, and the ring needs no clear at all (see the
        // BtLazy arm's provenance note).
        if matches!(self.params.strategy, Strategy::BtLazy(_)) {
            self.dubt_stale = true;
        }
        if !matches!(self.params.strategy, Strategy::Chain(_)) {
            self.tables[self.second..].fill(0);
        }
        // The strip (grid fill below, or nothing for opt) is the tables'
        // entire content: any gap a previous job left open ends here, and
        // the job's own blocks re-open it as they gate.
        self.gap_start = u64::MAX;
        self.gate_hold = false;
        // LDM job boundary: entries from earlier jobs must not shape this
        // one's output (pooled matchers), and the strip is the job's
        // candidate history — same content the grid fill indexes below.
        // The opt and btlazy2 rows sample the strip's alphabet first: the
        // fill is an unconditional split pass, and a low-alphabet strip
        // (json-class source above the full-window bar) never yields a
        // paying far class (the frame-start alphabet gate's rule, applied
        // to the job's own history). The chain row keeps its frozen
        // ungated flow.
        self.strip_parse = true;
        // (the opt rows' tree-fill bound lives in `prefill_job_strip`)
        if ldm_fill == LdmStripFill::Defer {
            // The windowed capture band: `windowed_ldm_prefill` owns the
            // LDM state (whole-window span, snapshot adoption).
        } else if let Some(ldm) = &mut self.ldm {
            ldm.restart(base);
            self.ldm_quiet = 0;
            self.ldm_dead = false;
            self.ldm_canary = 0;
            let gated = matches!(self.params.strategy, Strategy::Opt(_) | Strategy::BtLazy(_))
                && sampled_distinct(data, 0, data.len()) < LDM_SYMS_MIN;
            if data.len() >= super::ldm::MIN_MATCH_LENGTH && !gated {
                #[cfg(feature = "job_trace")]
                let trace_ldm = std::time::Instant::now();
                ldm.fill(data, base, base, base + data.len() as u64);
                #[cfg(feature = "job_trace")]
                super::job_trace::add_ldm_fill(trace_ldm);
            }
        }
        // A fully uniform strip (see `strip_is_uniform`) collapses both the
        // probe walk above and the strategy grid fills below to O(1)
        // table state; non-uniform strips exit the check on the first
        // 64-byte block. Dictionary loads keep the stock paths (cold,
        // once per dictionary).
        let uniform = grid == FillGrid::Strip && strip_is_uniform(data);
        self.seed_gate_probe(data, uniform);
        if data.len() < HASH_READ {
            return;
        }
        let last = data.len() - HASH_READ;
        match self.params.strategy {
            Strategy::Opt(_) => {
                self.next_update = self.next_update.min(base);
            },
            Strategy::BtLazy(_) => {
                self.next_update = base;
            },
            Strategy::Fast => {
                // Sparse grid, oldest-to-newest, newest-wins per slot — the
                // single-strategy table has no chain to walk, so a buried
                // twin is unreachable (see PREFILL_STRIDE).
                let table = &mut self.tables[..self.second];
                let log = self.params.hash_log;
                match grid {
                    FillGrid::Strip => {
                        // Only the strip's retention tail is inserted
                        // (PREFILL_RETAIN_HORIZONS); the seed scan below
                        // still sees the whole strip, so period-long repeats
                        // keep their reach.
                        let retain = table.len() * PREFILL_STRIDE * PREFILL_RETAIN_HORIZONS;
                        let mut idx = last.saturating_sub(retain);
                        while idx < last {
                            insert_at(data, table, idx, base + idx as u64, log);
                            idx += PREFILL_STRIDE;
                        }
                    },
                    FillGrid::Dictionary => {
                        // Whole content on the stride grid, then every
                        // skipped position whose slot is still empty claims
                        // it (libzstd's dtlm_full backfill): the oldest
                        // twin of a collision survives, where the strip's
                        // retention cap would drop dictionary-head entries.
                        let mut idx = 0;
                        while idx < last {
                            insert_at(data, table, idx, base + idx as u64, log);
                            backfill_empty(table, data, base, idx, log, last);
                            idx += PREFILL_STRIDE;
                        }
                    },
                }
                self.acquire_seed(data, last);
            },
            Strategy::Dfast(small_log) => {
                let long_log = self.params.hash_log;
                // The short table keys 4-byte windows on dictionary frames
                // (the scan's `MM4` row; see `dict_row`).
                let width = ChainHashWidth::of(self.dict_row);
                let (long, small) = self.tables.split_at_mut(self.second);
                let mut idx = 0;
                while idx < last {
                    // SAFETY: both hashes are masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(base + idx as u64);
                        *long.get_unchecked_mut(hash8_at_log(data, idx, long_log)) = entry;
                        *small.get_unchecked_mut(hash_at_width(data, idx, small_log, width)) =
                            entry;
                        if grid == FillGrid::Dictionary && idx + 2 < last {
                            // Beyond libzstd's dict fill, whose small table
                            // stays on the stride grid (off-grid twins stay
                            // reachable only through the long probe's
                            // empty-slot backfill, mml 8): dense small
                            // writes make off-grid 4-class dict twins
                            // candidates — 13/15 of the remaining missed
                            // sources in the fixture decomposition.
                            *small.get_unchecked_mut(hash_at_width(
                                data,
                                idx + 1,
                                small_log,
                                width,
                            )) = pack_pos(base + idx as u64 + 1);
                            *small.get_unchecked_mut(hash_at_width(
                                data,
                                idx + 2,
                                small_log,
                                width,
                            )) = pack_pos(base + idx as u64 + 2);
                        }
                    }
                    if grid == FillGrid::Dictionary {
                        // The long table takes the empty-slot backfill
                        // (libzstd's fillDoubleHashTableForCDict).
                        backfill8_empty(long, data, base, idx, long_log, last);
                    }
                    idx += PREFILL_STRIDE;
                }
                // The double table is as burial-prone as the fast one for
                // period-long twins: both probes are single-candidate.
                self.acquire_seed(data, last);
            },
            Strategy::Chain(_) => {
                let hash_log = self.params.hash_log;
                let width = ChainHashWidth::of(self.dict_row);
                let (table, chain) = self.tables.split_at_mut(self.second);
                let chain_mask = chain.len() - 1;
                // Strip: the stride grid (PREFILL_STRIDE's note — a windowed
                // strip cannot afford a dense fill). Dictionary: every
                // position, oldest-to-newest, linked — libzstd's dict load
                // (`ZSTD_insertAndFindFirstIndex`) indexes the whole content
                // this way, and a small payload's parse rides the chain
                // walk, where a stride grid leaves two thirds of the
                // dictionary unreachable as candidates.
                let stride = if grid == FillGrid::Strip {
                    PREFILL_STRIDE
                } else {
                    1
                };
                let origin = self.head_origin;
                #[cfg(feature = "job_trace")]
                let trace_grid = std::time::Instant::now();
                // Uniform strip: every grid position hashes to one slot
                // and links to its predecessor, so the stock fill's
                // observable final state is the newest grid position in
                // that head slot plus the top of its chain column. A walk
                // reads one chain slot per admitted candidate and admits
                // at most `search_depth` candidates, and every candidate
                // below the strip's top can only be reached through those
                // top links (heads are identical to the stock fill's, and
                // every other slot holds the same stale entries the stock
                // fill leaves in place — uniformity keeps it from writing
                // any other head), so links below depth+1 are unreachable
                // state. The guard keeps strips whose grid is shallower
                // than the written links on the stock loop (there the
                // column bottoms out at the first grid position's stale
                // link and the stock path is cheap anyway).
                let depth = self.params.search_depth as usize;
                if grid == FillGrid::Strip && uniform && last > (depth + 2) * PREFILL_STRIDE {
                    // SAFETY: the hash masks to hash_log bits, the
                    // positions to the chain size (absolute key).
                    unsafe {
                        let h0 = hash_at_width(data, 0, hash_log, width);
                        let newest = base + (((last - 1) / PREFILL_STRIDE) * PREFILL_STRIDE) as u64;
                        for k in 0..=depth {
                            let pos = newest - k as u64 * PREFILL_STRIDE as u64;
                            *chain.get_unchecked_mut(pos as usize & chain_mask) =
                                pack_head(pos - PREFILL_STRIDE as u64, origin);
                        }
                        *table.get_unchecked_mut(h0) = pack_head(newest, origin);
                    }
                } else {
                    let mut idx = 0;
                    while idx < last {
                        let abs = base + idx as u64;
                        // SAFETY: the hash masks to hash_log bits, the absolute
                        // position to the chain size (absolute key; see
                        // emit_chain's note on the walk side's indexing).
                        unsafe {
                            let h = hash_at_width(data, idx, hash_log, width);
                            let head = *table.get_unchecked(h);
                            *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                            *table.get_unchecked_mut(h) = pack_head(abs, origin);
                        }
                        idx += stride;
                    }
                }
                #[cfg(feature = "job_trace")]
                super::job_trace::add_grid_fill(trace_grid);
                // The fill is a table writer the scan cursor never sees
                // (fill-only states, spf builders): fold its extent into
                // the origin's write mark.
                self.strip_mark = self.strip_mark.max(base + last as u64);
                // The head table's first hop is as burial-prone as the fast
                // strategy's single probe; seed the walk-independent path.
                // (The dictionary grid's dense links make every position a
                // walkable candidate, so the seed is strip-only there.)
                if grid == FillGrid::Strip {
                    self.acquire_seed(data, last);
                }
                // The catch-up cursor for dictionary-row scans starts at
                // the fill's frontier: everything below it is table
                // content (dense on the dictionary grid, the tuned stride
                // grid on strips — whose sparse layout stands).
                self.chain_filled = base + last as u64;
            },
            Strategy::Row(row_log) => {
                let hash_log = self.params.hash_log;
                let width = ChainHashWidth::of(self.dict_row);
                let table = self.tables.as_mut_ptr();
                let heads = self.row_heads.as_mut_ptr();
                // Same geometry decision as the chain arm: the strip is a
                // full window, so it takes the stride grid; dictionary
                // content is at most one window and parses against it, so
                // every position enters a row (the eviction age is the
                // only reach a row has — a strided dict fill would leave
                // two thirds of it unreachable).
                let stride = if grid == FillGrid::Strip {
                    PREFILL_STRIDE
                } else {
                    1
                };
                let mut idx = 0;
                while idx < last {
                    let abs = base + idx as u64;
                    let h = row_hash_at(data, idx, hash_log, row_log, width);
                    match row_log {
                        4 => row_insert_fill::<4>(table, heads, h, abs),
                        5 => row_insert_fill::<5>(table, heads, h, abs),
                        _ => row_insert_fill::<6>(table, heads, h, abs),
                    }
                    idx += stride;
                }
                // The row's candidates evict at 15/31/63 same-bucket
                // inserts: a period-long repeat riding the strip survives
                // in its row only when the bucket stays quiet, so the
                // strip keeps the seed path (dictionary fills are dense).
                if grid == FillGrid::Strip {
                    self.acquire_seed(data, last);
                }
            },
        }
    }

    #[inline(always)]
    fn idx_of(&self, abs: u64) -> usize {
        (abs - self.win_base) as usize
    }

    /// Scan back from the strip's tail for the nearest position whose 8
    /// bytes equal the tail's, whose preceding [`SEED_AGREE`] bytes agree,
    /// and whose [`SEED_CONFIRMS`] anchors agree, and install its distance
    /// as the job-start seed offset (see `seed_offset`). Word-level repeats
    /// die at ~15 agreeing bytes and local block repeats at the confirms, so
    /// only a genuine long repeat — a period riding the strip, or a
    /// duplicated block — qualifies. The offset is by construction inside
    /// the strip, hence inside the window, so seed matches are always
    /// encodable. `last` is the strip's final insertable index; the anchor
    /// bytes `[last, last + 8)` abut the job start.
    fn acquire_seed(&mut self, data: &[u8], last: usize) {
        #[cfg(feature = "job_trace")]
        let trace_seed = std::time::Instant::now();
        let a8 = read8(data, last);
        if let Some(u) = seed_scan(data, last, a8) {
            self.seed_offset = (last - u) as u32;
            self.seed_hits = 0;
            self.seed_budget = SEED_BUDGET;
        }
        #[cfg(feature = "job_trace")]
        super::job_trace::add_seed_scan(trace_seed);
    }
}

impl Matcher for MatchGeneratorDriver {
    /// The effort row through the trait's u8 routing (see `pre_split`).
    fn pre_split_effort(&self) -> u8 {
        match self.pre_split_level() {
            None => u8::MAX,
            Some(super::pre_split::SplitLevel::Borders) => 0,
            Some(super::pre_split::SplitLevel::Chunks { rate, .. }) => match rate {
                43 => 1,
                11 => 2,
                5 => 3,
                _ => 4,
            },
        }
    }

    fn staged_tail(&self) -> &[u8] {
        self.staged_window()
    }

    fn set_input_shape(&mut self, shape: InputShape) {
        self.shape = shape;
    }

    /// Decide the frame's reach from its first bytes (see
    /// `super::reach_probe`). Called before any block of the frame is
    /// matched; no-op for heads below the probe span.
    /// Start accumulating a reach-probe cost off this driver's real parses
    /// (donation mode; see `probe_stats`). No effect on the parse.
    fn begin_probe_stats(&mut self) {
        self.probe_stats = Some(alloc::boxed::Box::default());
    }

    /// Take the accumulated probe cost in bits (order-0 literals plus the
    /// sequence-code estimate); `None` when no accumulation was begun.
    fn take_probe_cost(&mut self) -> Option<f64> {
        self.probe_stats.take().map(|stats| stats.cost_bits())
    }

    /// Rebuild this state as a fresh shrunk-reach frame after a donated
    /// keep parse measured Shrink. The keep parse's table residue covers
    /// the same bytes at the same positions, so unlike cross-frame pool
    /// residue the byte-verify cannot arbitrate it — the parse tables and
    /// the gate's sampled-hash history return to the fresh-state content.
    /// (`apply_level` already reallocates both tables zeroed on the reach
    /// change; the explicit clears keep this correct even where the params
    /// compare equal.)
    fn restart_shrunk(&mut self, level: Level) {
        self.set_reach_choice(ReachChoice::Shrink);
        self.reset(level);
        if !self.tables.is_empty() {
            clear_table(&mut self.tables[..self.second]);
            // Same fresh-state contract for the row heads (see
            // fill_window_grid).
            self.row_heads.fill(0);
        }
        if !self.tables[self.second..].is_empty()
            && matches!(self.params.strategy, Strategy::Chain(_))
        {
            // The dfast small table doubles as `chain`; its strategies never
            // reach here, but the clear stays strategy-exact regardless.
            self.tables[self.second..].fill(0);
        }
        self.probe.clear();
    }

    fn consider_reach_probe(&mut self, head: &[u8], level: Level) {
        let choice = super::reach_probe::probe_reach_choice(head, level, self.shape);
        if choice != self.reach_choice {
            self.reach_choice = choice;
            // No block has been matched yet, so re-deriving the params is
            // the whole state change; the tables' sizes are
            // reach-independent. A shrunk frame disarms the head here too
            // (the reset path checks the same condition at arm time).
            self.apply_level(level);
            if choice == ReachChoice::Shrink && self.dubt_head == HeadPhase::Armed {
                self.dubt_head = HeadPhase::Off;
            }
        }
    }

    /// See the inherent [`MatchGeneratorDriver::load_dictionary`] — the
    /// trait view.
    fn load_dictionary(&mut self, content: &[u8], rep: [u32; 3], level: Level) {
        self.load_dictionary(content, rep, level);
    }

    fn reset(&mut self, level: Level) {
        self.apply_level(level);
        // Stale opt-table entries from previous frames decode below the
        // window floor once the origin advances past them (read before the
        // cursor zeroes below); the u32 tables need no reset. The DUBT
        // finder carries no origin either, but its clear is deferred to
        // the first searching block (`dubt_stale`): frame positions
        // restart at zero and old absolute positions would alias the new
        // window, while RLE/raw-only frames never read the tables at all.
        // The deferred clear covers the heads only — the ring is
        // unreachable residue once the heads are zeroed (see the BtLazy
        // arm's provenance note).
        self.opt_origin += self.pos + 1;
        // Stash the chain heads' write high-water before the cursor zeroes
        // (see `strip_mark`): the next strip prefill advances `head_origin`
        // past it.
        self.strip_mark = self.strip_mark.max(self.pos);
        self.ext = None;
        self.win.clear();
        self.win_base = 0;
        self.pos = 0;
        self.block_end = 0;
        self.anchor = 0;
        self.block_start = 0;
        self.dubt_stale = matches!(self.params.strategy, Strategy::BtLazy(_));
        self.bt_step = if matches!(self.params.strategy, Strategy::BtLazy(_)) {
            BtStepPhase::Armed
        } else {
            BtStepPhase::Off
        };
        self.miss_count = 0;
        self.covered_fill = CoveredFill::Dense;
        self.scan_density = ScanDensity::Plain;
        self.ramp = RampGate::OFF;
        // Matches the decoder's per-frame offset_hist reset.
        self.rep = [1, 4, 8];
        self.rep_pending = 0;
        self.lit_lens = DEFAULT_LIT_LENS;
        self.seed_offset = 0;
        self.seed_hits = 0;
        self.seed_budget = 0;
        // The opt parser re-seeds its statistics and re-fills its tree.
        self.next_update = 0;
        self.gap_start = u64::MAX;
        self.gate_hold = false;
        self.dubt_head = if self.head_eligible() {
            HeadPhase::Armed
        } else {
            HeadPhase::Off
        };
        if let Some(ldm) = &mut self.ldm {
            ldm.restart(0);
        }
        // The gate's repeat probe is history, not capacity: a pooled state
        // starts each frame as a fresh one (see `seed_gate_probe`).
        if !self.probe.is_empty() {
            self.probe.fill(0);
        }
        self.ldm_seqs.clear();
        self.ldm_quiet = 0;
        self.ldm_dead = false;
        self.ldm_canary = 0;
        self.ldm_checked = false;
        self.strip_parse = false;
        self.dict_row = false;
        self.chain_filled = u64::MAX;
        self.opt_state.reset();
    }

    fn window_size(&self) -> u64 {
        self.params.window as u64
    }

    fn repcode_snapshot(&self) -> [u32; 3] {
        self.rep
    }

    fn restore_repcode(&mut self, rep: [u32; 3]) {
        self.rep = rep;
    }

    fn note_literal_costs(&mut self, lengths: &[u8; 256]) {
        // Uncovered symbols price at the format's 11-bit cap: emitting one
        // forces a table rebuild and lands it at the longest codes.
        for (i, &nb) in lengths.iter().enumerate() {
            self.lit_lens[i] = if nb == 0 {
                11
            } else {
                nb
            };
        }
    }

    fn block_splitting_enabled(&self) -> bool {
        // libzstd's ZSTD_resolveBlockSplitterMode auto rule: the opt
        // strategies with a window of at least 2^17 (the row as adjusted
        // to the declared input shape).
        matches!(self.params.strategy, Strategy::Opt(_)) && self.params.window >= (1 << 17)
    }

    fn block_tail(&mut self) -> &mut [u8] {
        debug_assert!(self.ext.is_none(), "block_tail is owned-window only");
        if self.win.len() + self.slice_size > self.win.capacity() {
            let keep = self.win.len().min(self.params.window);
            let drop = self.win.len() - keep;
            self.win.copy_within(drop.., 0);
            self.win.truncate(keep);
            self.win_base += drop as u64;
        }
        let old = self.win.len();
        // SAFETY: the capacity check above guarantees old + slice_size fits.
        // The caller writes the block into the returned slice and passes the
        // written count to commit_block, which shrinks the length back to
        // exactly that, so the never-written tail bytes are never observed.
        unsafe { self.win.set_len(old + self.slice_size) };
        &mut self.win[old..]
    }

    fn get_last_space(&mut self) -> &[u8] {
        &window_slice(&self.win, self.ext.as_ref())[self.idx_of(self.block_start)..]
    }

    fn commit_block(&mut self, read_bytes: usize) {
        debug_assert!(self.ext.is_none(), "commit_block is owned-window only");
        let old_len = self.win.len() - self.slice_size;
        // SAFETY: the caller wrote read_bytes bytes into the tail slice
        // handed out by block_tail.
        unsafe { self.win.set_len(old_len + read_bytes) };
        // Matching restarts at the head of the freshly committed block; the
        // window behind it is only a match source.
        self.pos = self.win_base + old_len as u64;
        self.block_start = self.pos;
        self.block_end = self.win_base + self.win.len() as u64;
        self.anchor = self.pos;
    }

    fn start_matching(&mut self, mut handle_sequence: impl for<'a> FnMut(Sequence<'a>)) {
        let mut literals = Vec::new();
        let mut seqs: Vec<SeqWord> = Vec::new();
        self.start_matching_codes(&mut literals, &mut seqs);
        // Rebuild the interleaved callback order from the collected
        // buffers: each sequence's ll literals came right before it.
        // Zero-sequence blocks staged nothing; their literals are the block
        // itself, handed over straight from the window.
        let zero_seq = seqs.is_empty();
        let mut offset = 0usize;
        for &word in &seqs {
            let (ll, ml, of) = decode_packed(word.codes, word.add);
            let lits = &literals[offset..offset + ll as usize];
            offset += ll as usize;
            handle_sequence(Sequence::Triple {
                literals: lits,
                offset: of as usize,
                match_len: ml as usize,
            });
        }
        if zero_seq {
            handle_sequence(Sequence::Literals {
                literals: self.get_last_space(),
            });
        } else {
            handle_sequence(Sequence::Literals {
                literals: &literals[offset..],
            });
        }
    }

    fn start_matching_codes(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        if self.head_block() {
            // The head parses through the btlazy2 driver; its bytes stay
            // LDM-indexed so later chain blocks can match far into them.
            // The head itself consumes no candidates (row 9's parse is
            // frozen; `reset` already emptied the block's set, this covers
            // a pooled state mid-frame) and its blocks never count quiet.
            self.ldm_fill_block(LdmFill::Head);
            self.ldm_seqs.clear();
            self.start_matching_btlazy(HEAD_KNOBS, literals, seqs, LazyStep::Ramp);
            self.finish_head();
            self.absorb_probe_stats(literals, seqs);
            return;
        }
        if !matches!(self.params.strategy, Strategy::Opt(_) | Strategy::BtLazy(_)) {
            self.catch_up_insertions();
            // The reopened gap is now table content (the helper filled it
            // up to its HASH_READ tail margin); the dictionary rows'
            // catch-up cursor must not re-insert the range — a re-linked
            // position resurfaces as the bucket's newest (phantom
            // recency) — and continues from the helper's own bound.
            if self.dict_row {
                self.chain_filled = self.block_start.saturating_sub(HASH_READ as u64);
            }
        }
        match self.params.strategy {
            Strategy::Fast => {
                // The LDM candidate set is generated ahead of the scan
                // (the chain row's order: alphabet gate first, then
                // generate); the consuming axis is compile-time below.
                self.ldm_alphabet_gate();
                self.ldm_generate();
                // The instantiation pair is compile-time: plain blocks run
                // a body with every dense-mode lever folded out.
                // The log axis instantiates the row's full value (HASH_LOG,
                // everything at or above ~32 KiB of input) for the dense
                // body only: its re-roll measured json −3.4% Ir while the
                // plain body's re-roll measured dll +1.4% (the register
                // freed by the constant shift is a lottery per body — the
                // negative notes' lesson), so the plain body keeps the
                // runtime-log codegen byte for byte. Clamped-window shapes
                // (hash log below the row) take the [`RUNTIME_LOG`]
                // instantiation either way. The LDM axis follows the chain
                // row's pattern: the empty candidate set (LDM off, gated,
                // latched, far-less) takes the `LDM = false` instantiation,
                // which binds the empty slice constant and folds back to
                // the stock single-segment scan.
                macro_rules! scan_fast {
                    ($ldm:literal) => {
                        match (self.ramp.is_armed(), self.scan_density) {
                            (false, ScanDensity::Plain) => {
                                if self.params.small_src && self.win_small_wide_fast() {
                                    self.start_matching_fast::<false, false, RUNTIME_LOG, true, $ldm>(
                                        literals, seqs,
                                    );
                                } else {
                                    self.start_matching_fast::<false, false, RUNTIME_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                }
                            },
                            (true, ScanDensity::Plain) => {
                                if self.params.small_src && self.win_small_wide_fast() {
                                    self.start_matching_fast::<true, false, RUNTIME_LOG, true, $ldm>(
                                        literals, seqs,
                                    );
                                } else {
                                    self.start_matching_fast::<true, false, RUNTIME_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                }
                            },
                            (false, ScanDensity::Dense) => {
                                if self.params.hash_log == HASH_LOG {
                                    self.start_matching_fast::<false, true, HASH_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                } else {
                                    self.start_matching_fast::<false, true, RUNTIME_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                }
                            },
                            (true, ScanDensity::Dense) => {
                                if self.params.hash_log == HASH_LOG {
                                    self.start_matching_fast::<true, true, HASH_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                } else {
                                    self.start_matching_fast::<true, true, RUNTIME_LOG, false, $ldm>(
                                        literals, seqs,
                                    );
                                }
                            },
                        }
                    };
                }
                if self.ldm.is_some() {
                    scan_fast!(true);
                } else {
                    scan_fast!(false);
                }
                // This block's parse density picks the next block's
                // covered-fill policy. Structured shapes never fire
                // (json sits at 2.6-3.0 literal bytes per sequence; text
                // and skewed only ever fire literal-run tails below the
                // sequence floor), so their bytes stay identical.
                self.covered_fill = if seqs.len() >= COVERED_GATE_MIN_SEQS
                    && literals.len() >= COVERED_GATE_AVG_LL * seqs.len()
                {
                    CoveredFill::Strided
                } else {
                    CoveredFill::Dense
                };
                // Same-block dispatch of the next scan's instantiation:
                // match-dense, literal-light parses over a narrow literal
                // alphabet (the fed-back table's coverage) run dense.
                // Frame starts stay plain (no parse yet; DEFAULT_LIT_LENS
                // reads as a 256-wide alphabet).
                self.scan_density = if seqs.len() >= DENSE_GATE_MIN_SEQS
                    && literals.len() < DENSE_GATE_MAX_AVG_LL * seqs.len()
                    && covered_lit_symbols(&self.lit_lens) < DENSE_GATE_SYMS_MAX
                {
                    ScanDensity::Dense
                } else {
                    ScanDensity::Plain
                };
            },
            Strategy::Dfast(_) => {
                // The LDM candidate set is generated ahead of the scan
                // (the chain row's order); the consuming axis is
                // compile-time below, folding to the stock scan on the
                // empty set exactly like the fast arm.
                self.ldm_alphabet_gate();
                self.ldm_generate();
                // Key on the tables' actual lengths (what the scan body
                // derives): the two full rows cover every input the level
                // clamp leaves at full tables; smaller inputs (clamped
                // windows, shrunken logs) take the runtime-log
                // instantiation. Dictionary frames take the `MM4` runtime-log
                // body whatever their table sizes — dict frames are not a
                // hot path, and keeping them off the const-log arms leaves
                // every no-dict instantiation untouched.
                // Small-input policy (libzstd's small-src rows run the
                // dfast short table at minMatch 4, i.e. a 4-byte hash): a
                // <= 128 KiB wide-alphabet frame takes the MM4 runtime-log body —
                // the dict rows' instantiation, so no new codegen — while
                // structured frames keep the 5-byte width (their 4-byte
                // candidates are net-negative).
                macro_rules! scan_dfast {
                    ($ldm:literal) => {
                        if self.dict_row || (self.params.small_src && self.win_small_wide_fast()) {
                            if self.ramp.is_armed() {
                                self.start_matching_dfast::<
                                                            true,
                                                            true,
                                                            RUNTIME_LOG,
                                                            RUNTIME_LOG,
                                                            $ldm,
                                                        >(literals, seqs);
                            } else {
                                self.start_matching_dfast::<
                                                            false,
                                                            true,
                                                            RUNTIME_LOG,
                                                            RUNTIME_LOG,
                                                            $ldm,
                                                        >(literals, seqs);
                            }
                        } else {
                            let long_log = self.second.trailing_zeros();
                            let small_log = (self.tables.len() - self.second).trailing_zeros();
                            match (self.ramp.is_armed(), long_log, small_log) {
                                (false, 17, 16) => {
                                    self.start_matching_dfast::<false, false, 17, 16, $ldm>(
                                        literals, seqs,
                                    );
                                },
                                (true, 17, 16) => {
                                    self.start_matching_dfast::<true, false, 17, 16, $ldm>(
                                        literals, seqs,
                                    );
                                },
                                (false, 18, 18) => {
                                    self.start_matching_dfast::<false, false, 18, 18, $ldm>(
                                        literals, seqs,
                                    );
                                },
                                (true, 18, 18) => {
                                    self.start_matching_dfast::<true, false, 18, 18, $ldm>(
                                        literals, seqs,
                                    );
                                },
                                (armed, ..) => {
                                    if armed {
                                        self.start_matching_dfast::<
                                                                    true,
                                                                    false,
                                                                    RUNTIME_LOG,
                                                                    RUNTIME_LOG,
                                                                    $ldm,
                                                                >(literals, seqs);
                                    } else {
                                        self.start_matching_dfast::<
                                                                    false,
                                                                    false,
                                                                    RUNTIME_LOG,
                                                                    RUNTIME_LOG,
                                                                    $ldm,
                                                                >(literals, seqs);
                                    }
                                },
                            }
                        }
                    };
                }
                if self.ldm.is_some() {
                    scan_dfast!(true);
                } else {
                    scan_dfast!(false);
                }
            },
            Strategy::Chain(_) => {
                self.ldm_alphabet_gate();
                self.ldm_generate();
                if self.ldm.is_some() {
                    self.start_matching_chain::<true>(literals, seqs);
                } else {
                    self.start_matching_chain::<false>(literals, seqs);
                }
            },
            Strategy::Row(row_log) => {
                // The ramp gate folds out of the per-candidate loop on
                // disarmed frames (the fast loop's RAMPED discipline; the
                // env-gated ramp never arms on default paths).
                let ramped = self.ramp.is_armed();
                if ramped {
                    match row_log {
                        4 => self.start_matching_row::<4, true>(literals, seqs),
                        5 => self.start_matching_row::<5, true>(literals, seqs),
                        _ => self.start_matching_row::<6, true>(literals, seqs),
                    }
                } else {
                    match row_log {
                        4 => self.start_matching_row::<4, false>(literals, seqs),
                        5 => self.start_matching_row::<5, false>(literals, seqs),
                        _ => self.start_matching_row::<6, false>(literals, seqs),
                    }
                }
            },
            Strategy::Opt(knobs) => {
                self.ldm_alphabet_gate();
                self.ldm_generate();
                let won = self.start_matching_opt(knobs, literals, seqs);
                self.ldm_note_block(won);
            },
            Strategy::BtLazy(knobs) => {
                // The reset's deferred clear lands at the first searching
                // block (see `dubt_stale`). Heads only: the ring needs no
                // clear once the heads are zeroed — every ring read is
                // reached through a link (a fill-time chain link, a head, a
                // descent child) whose value names a position this frame
                // already filled, and the fill rewrites both ring slots of
                // every position it covers, so stale ring slots sit at
                // positions no this-frame link can ever name (see
                // `dubt.rs`'s provenance note).
                if self.dubt_stale {
                    self.dubt_table.fill(0);
                    self.dubt_stale = false;
                }
                let step = self.bt_lazy_step();
                self.ldm_alphabet_gate();
                self.ldm_generate();
                let won = self.start_matching_btlazy(knobs, literals, seqs, step);
                self.ldm_note_block(won);
            },
        }
        self.absorb_probe_stats(literals, seqs);
    }

    // Outlined on purpose: with the body inlined into `compress_fastest`
    // the block loop's layout shifted and text.fastest lost 7% wall at
    // +0.4% instructions (pure placement). One cold call per block.
    #[inline(never)]
    fn skip_if_incompressible(&mut self) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let block = &win[(self.block_start - self.win_base) as usize..];
        let total = block.len();
        if total < GATE_MIN_BLOCK {
            return false;
        }
        if self.probe.is_empty() {
            self.probe.resize(1 << GATE_PROBE_LOG, 0);
        }
        let probe = &mut self.probe;
        // Strided repeat probe: a hit means a colliding 47-bit content hash
        // exists somewhere in the history sampled so far — near-certainly
        // the same 8 bytes — so the block may be matchable and must not be
        // gated. The odd stride avoids period locking against structured
        // inputs. Any match region of a stride's length or more holds a
        // sample, so only short sparse matches escape detection — those are
        // worth under a percent of the block, the price the threshold below
        // already accepted. A hit exits immediately: any single repeat
        // keeps the whole block matchable.
        //
        // The same pass accumulates a sampled byte bitmap: distinct < 208
        // (the literals gate's pre-screen bar) rules the entropy bar out
        // outright, keeping low-alphabet corpora out of the exact histogram
        // below.
        let stride = (total >> 11) | 1;
        let mut bitmap = [0u64; 4];
        let mut i = 0usize;
        // Micro-screen checkpoints: a block whose sampled distinct count is
        // implausibly low for a max-entropy source (uniform bytes expect 57
        // distinct at 64 samples, 221 at 512) sits far below the entropy
        // bar — text-like alphabets (~70 symbols) and 16-letter skewed exit
        // at the first or second checkpoint instead of paying the rest of
        // the probe pass. Rejecting is always the conservative side, so the
        // screens cannot mis-gate.
        let (mut screen1, mut screen2) = (64 * stride, 512 * stride);
        while i + HASH_READ <= total {
            let h = read8(block, i).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
            let slot = (h >> (64 - GATE_PROBE_LOG)) as usize;
            let tag = h as u32;
            if probe[slot] == tag {
                self.gate_hold = false;
                return false;
            }
            probe[slot] = tag;
            let b = h as u8 as usize;
            bitmap[b >> 6] |= 1 << (b & 63);
            if i == screen1 {
                screen1 = usize::MAX;
                if bitmap.iter().map(|w| w.count_ones()).sum::<u32>() < 32 {
                    self.gate_hold = false;
                    return false;
                }
            } else if i == screen2 {
                screen2 = usize::MAX;
                if bitmap.iter().map(|w| w.count_ones()).sum::<u32>() < 96 {
                    self.gate_hold = false;
                    return false;
                }
            }
            i += stride;
        }
        let distinct: u32 = bitmap.iter().map(|w| w.count_ones()).sum();
        if distinct < 208 {
            self.gate_hold = false;
            return false;
        }
        // The sticky hold skips the exact pass on steady-state
        // incompressible runs (see the field's risk model); the first block
        // of every run always pays it.
        if !self.gate_hold {
            // Exact byte histogram (four lanes break the store-forward chain):
            // the entropy decision must not ride on a sample's noise band —
            // a block wrongly kept below the bar forces a full scan (plus the
            // table catch-up fill), hundreds of times the histogram cost.
            let mut lanes = [[0u32; 256]; 4];
            let (chunks, remainder) = block.as_chunks::<4>();
            for chunk in chunks {
                lanes[0][chunk[0] as usize] += 1;
                lanes[1][chunk[1] as usize] += 1;
                lanes[2][chunk[2] as usize] += 1;
                lanes[3][chunk[3] as usize] += 1;
            }
            for &b in remainder {
                lanes[0][b as usize] += 1;
            }
            let total_f = total as f64;
            let mut entropy_bits = 0.0f64;
            let mut distinct = 0u32;
            for (s, &c0) in lanes[0].iter().enumerate() {
                let c = c0 + lanes[1][s] + lanes[2][s] + lanes[3][s];
                if c > 0 {
                    distinct += 1;
                    entropy_bits -=
                        c as f64 * crate::fse::fse_encoder::approx_log2(c as f64 / total_f);
                }
            }
            // Miller-Madow bias correction, as in the literals entropy gate.
            let bits_per_byte =
                entropy_bits / total_f + (distinct as f64 - 1.0) * 0.7213_4752_0559_1157 / total_f;
            // Well past the literals gate's reject floor: huffman on literals
            // this dense saves under 0.4% before the table description, and the
            // probe above already ruled out matches worth more.
            if bits_per_byte < 7.97 {
                return false;
            }
            self.gate_hold = true;
        }
        // Advance the cursors only. The opt strategies' tree fills the
        // skipped range lazily from `next_update`; the table strategies
        // open the gap at the block start, and the next scan's
        // `catch_up_insertions` restores the block as match history before
        // any probe reads the tables — so a later duplicate of a gated
        // block still matches.
        if self.gap_start == u64::MAX {
            self.gap_start = self.block_start;
        }
        // Donation mode: a gated block's cost model is its own bytes as
        // literals (the frame emits it raw). Runs before the cursor
        // advance: the block borrow must not straddle the LDM fill call.
        if let Some(stats) = &mut self.probe_stats {
            stats.absorb_literals(block);
        }
        self.ldm_fill_block(LdmFill::Skipped);
        self.pos = self.block_end;
        self.anchor = self.block_end;
        true
    }

    fn skip_matching(&mut self) {
        // Only called for RLE blocks so far: every 5-byte window in a
        // uniform run hashes to the same slot, so indexing each byte just
        // rewrites one table entry. The first position covers that slot;
        // future probes into the run resolve through it or the repcode
        // chain. The opt strategies skip even that: their tree fills
        // lazily from `next_update`, which stays at the block start and
        // covers the run on the next block's fill.
        let idx = (self.block_start - self.win_base) as usize;
        let win = window_slice(&self.win, self.ext.as_ref());
        if idx + HASH_READ <= win.len() {
            match self.params.strategy {
                Strategy::Opt(_) | Strategy::BtLazy(_) => {},
                Strategy::Chain(_) => {
                    // The head hash uses params.hash_log; the pattern's
                    // payload is the chain-table log (they differ on rows
                    // whose head table outsizes the chain table).
                    let h = hash_at_width(
                        win,
                        idx,
                        self.params.hash_log,
                        ChainHashWidth::of(self.dict_row),
                    );
                    // SAFETY: h is masked to hash_log bits, the absolute block
                    // start to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let head = *self.tables.get_unchecked(h);
                        let chain_mask = self.tables.len() - self.second - 1;
                        *self.tables.get_unchecked_mut(
                            self.second + (self.block_start as usize & chain_mask),
                        ) = head;
                        *self.tables.get_unchecked_mut(h) =
                            pack_head(self.block_start, self.head_origin);
                    }
                },
                Strategy::Dfast(small_log) => {
                    let hl = hash8_at_log(win, idx, self.params.hash_log);
                    let hs = hash_at_width(win, idx, small_log, ChainHashWidth::of(self.dict_row));
                    // SAFETY: both hashes masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(self.block_start);
                        *self.tables.get_unchecked_mut(hl) = entry;
                        *self.tables.get_unchecked_mut(self.second + hs) = entry;
                    }
                },
                Strategy::Fast => {
                    insert_at(
                        win,
                        &mut self.tables[..self.second],
                        idx,
                        self.block_start,
                        self.params.hash_log,
                    );
                },
                Strategy::Row(row_log) => {
                    // Same single-insert policy as the chain arm: a uniform
                    // run hashes every position to one row, and the newest
                    // insert covers it.
                    let h = row_hash_at(
                        win,
                        idx,
                        self.params.hash_log,
                        row_log,
                        ChainHashWidth::of(self.dict_row),
                    );
                    let table = self.tables.as_mut_ptr();
                    let heads = self.row_heads.as_mut_ptr();
                    match row_log {
                        4 => row_insert::<4>(table, heads, h, self.block_start),
                        5 => row_insert::<5>(table, heads, h, self.block_start),
                        _ => row_insert::<6>(table, heads, h, self.block_start),
                    }
                },
            }
        }
        self.ldm_fill_block(LdmFill::Skipped);
        self.pos = self.block_end;
        self.anchor = self.block_end;
    }
}

impl MatchGeneratorDriver {
    /// LDM-index a block the scan will not parse (incompressibility-gated,
    /// RLE-skipped, or parsed by the DUBT head): the bytes stay matchable
    /// far-distance history for later blocks, and the rolling hash stays
    /// fed to the block end. The mid-size population skips `Skipped`
    /// blocks entirely: such a frame cannot latch through them (no scan
    /// runs, so quiet never counts — random paid half its speed in fills
    /// for entries nothing can match), and max-entropy or uniform windows
    /// carry no far class anyway. The full-window population keeps the
    /// fill unconditionally (its bytes are frozen).
    fn ldm_fill_block(&mut self, why: LdmFill) {
        self.ldm_alphabet_gate();
        // Skipped blocks stop filling except for the full-window population
        // (the chain row, and the armed fast/dfast rows whose widened
        // window is that same frozen population), whose bytes are frozen.
        // The opt rows have no frozen
        // population, so they exempt Skipped blocks at every window: the
        // latch cannot fire through them either way (no scan runs), and
        // max-entropy or uniform windows carry no far class.
        if why == LdmFill::Skipped
            && !(matches!(
                self.params.strategy,
                Strategy::Chain(_) | Strategy::Fast | Strategy::Dfast(_)
            ) && self.params.window >= LDM_FULL_WINDOW)
        {
            return;
        }
        if let Some(ldm) = &mut self.ldm {
            let win = window_slice(&self.win, self.ext.as_ref());
            ldm.fill(win, self.win_base, self.block_start, self.block_end);
        }
    }

    /// The mid-size population's alphabet gate ([`LDM_SYMS_MIN`]): run
    /// once per frame at the first block that reaches LDM, before any of
    /// its own indexing — a poor alphabet disarms outright, bounding the
    /// split-pass tax at the head span already filled. Every population
    /// gates at every window (2026-09-24, with the chain row's gap-parse
    /// consumer): the full-window exemption's blind spot was the unpledged
    /// stream, whose unknown size leaves the row window unclamped at W26 —
    /// a low-alphabet stream then armed LDM with no latch floor (quiet
    /// never counts inside the first window), and the wholesale consumer
    /// emits its structural candidates where the injection model merely
    /// priced them (text.balanced stream +436 B). Binary heads pass with
    /// margin (dll 209-231 sampled distinct); dll100's bytes are
    /// unaffected.
    fn ldm_alphabet_gate(&mut self) {
        if self.ldm_checked || self.ldm.is_none() {
            return;
        }
        self.ldm_checked = true;
        let win = window_slice(&self.win, self.ext.as_ref());
        let start = (self.block_start - self.win_base) as usize;
        let end = (self.block_end - self.win_base) as usize;
        if sampled_distinct(win, start, end) < LDM_SYMS_MIN {
            // Park, don't drop: the pooled state re-arms the same window
            // next frame, and the re-allocation churned megabytes on
            // low-alphabet shapes (the zeros regression's residue).
            self.ldm_parked = self.ldm.take();
            self.ldm_seqs.clear();
        }
    }

    /// Post-block latch bookkeeping, shared by the consumers (the chain
    /// scan's `ldm_won`, the optimal parser's out-length signal): a block
    /// whose LDM candidate won resets the quiet counter; a winless block
    /// past the far-history horizon counts quiet, and [`LDM_QUIET`]
    /// consecutive quiet blocks latch the generator off. Quiet counts only
    /// where a beyond-reach twin could exist at all — the frame's first
    /// `window` bytes have no content far enough back.
    fn ldm_note_block(&mut self, won: bool) {
        if self.ldm.is_none() {
            return;
        }
        if won {
            self.ldm_quiet = 0;
        } else if self.block_end - self.win_base > self.params.window as u64 {
            self.ldm_quiet += 1;
            if self.ldm_quiet >= LDM_QUIET {
                self.ldm_dead = true;
            }
        }
    }

    /// Generate this block's long-distance candidates into
    /// [`Self::ldm_seqs`] ahead of the consuming scan (chain or optimal
    /// parser).
    #[inline(never)]
    fn ldm_generate(&mut self) {
        self.ldm_seqs.clear();
        if self.ldm_dead {
            self.ldm_canary += 1;
            if self.ldm_canary < LDM_CANARY {
                return;
            }
            self.ldm_canary = 0;
        }
        if let Some(ldm) = &mut self.ldm {
            let win = window_slice(&self.win, self.ext.as_ref());
            let reach = self.params.chain_reach.unwrap_or(self.params.window) as u64;
            ldm.generate(
                &mut self.ldm_seqs,
                win,
                self.win_base,
                self.pos,
                self.block_end,
            );
            // Only offsets beyond the consumer's search domain survive:
            // nearer candidates are the dense tables' own class (the
            // newest-wins heads already serve the modal repeat distance),
            // and probing them measured as pure interference — a far read
            // per split plus a miss-step clamp on tiled data.
            self.ldm_seqs.retain(|s| s.offset as u64 > reach);
            if self.ldm_dead && !self.ldm_seqs.is_empty() {
                self.ldm_dead = false;
                self.ldm_quiet = 0;
            }
        }
    }

    /// Dense catch-up fill for the table strategies: the positions an
    /// incompressibility-gated block skipped ([`Matcher::skip_if_incompressible`])
    /// become searchable again before the next scan reads the tables. Gated
    /// blocks are near-certainly matchless, so this only ever runs on a
    /// gate→scan transition — an all-random input never pays it, and mixed
    /// input pays it once per gated streak. The fill is dense and
    /// oldest-to-newest exactly like a scan's miss-path inserts, so the
    /// tables come back scan-equivalent up to the fill convention's
    /// one-position tail margin (see the bound note inside); positions
    /// older than the match window cannot resolve and are skipped (the
    /// same clamp as the opt tree's fill).
    #[inline(never)]
    fn catch_up_insertions(&mut self) {
        if self.gap_start == u64::MAX {
            return;
        }
        let block_start = self.block_start;
        let win_base = self.win_base;
        let from = self
            .gap_start
            .max(win_base)
            .max(block_start.saturating_sub(self.params.window as u64));
        // Each insert hashes HASH_READ bytes at `idx`, but the window only
        // guarantees coverage up to the gated block's end: a block of fewer
        // than HASH_READ bytes right after a gated one (bulk/MT windows
        // borrow the caller's slice) would overread. A scan of the gated
        // block itself inserts through `block_start - HASH_READ` inclusive
        // (its loop stops with `block_end - pos >= HASH_READ` yet still
        // stores at that pos); this exclusive bound stops one position
        // earlier — the same tail margin as every other fill path
        // (`last = len - HASH_READ; while idx < last`), so the tables come
        // back scan-equivalent up to that shared conservatism and never
        // past the coverage the window guarantees.
        let to = ((block_start - win_base) as usize).saturating_sub(HASH_READ);
        let win = window_slice(&self.win, self.ext.as_ref());
        let mut idx = (from - win_base) as usize;
        match self.params.strategy {
            Strategy::Fast => {
                let log = self.params.hash_log;
                while idx < to {
                    insert_at(
                        win,
                        &mut self.tables[..self.second],
                        idx,
                        win_base + idx as u64,
                        log,
                    );
                    idx += 1;
                }
            },
            Strategy::Dfast(small_log) => {
                let long_log = self.params.hash_log;
                let width = ChainHashWidth::of(self.dict_row);
                let (long, small) = self.tables.split_at_mut(self.second);
                while idx < to {
                    // SAFETY: both hashes are masked to their tables' sizes
                    // (same pair as the dfast prefill).
                    unsafe {
                        let entry = pack_pos(win_base + idx as u64);
                        *long.get_unchecked_mut(hash8_at_log(win, idx, long_log)) = entry;
                        *small.get_unchecked_mut(hash_at_width(win, idx, small_log, width)) = entry;
                    }
                    idx += 1;
                }
            },
            Strategy::Chain(_) => {
                let hash_log = self.params.hash_log;
                let width = ChainHashWidth::of(self.dict_row);
                let (table, chain) = self.tables.split_at_mut(self.second);
                let chain_mask = chain.len() - 1;
                let origin = self.head_origin;
                while idx < to {
                    let abs = win_base + idx as u64;
                    // SAFETY: the hash masks to hash_log bits, the absolute
                    // position to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let h = hash_at_width(win, idx, hash_log, width);
                        let head = *table.get_unchecked(h);
                        *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                        *table.get_unchecked_mut(h) = pack_head(abs, origin);
                    }
                    idx += 1;
                }
            },
            Strategy::Row(row_log) => {
                let hash_log = self.params.hash_log;
                let width = ChainHashWidth::of(self.dict_row);
                let table = self.tables.as_mut_ptr();
                let heads = self.row_heads.as_mut_ptr();
                while idx < to {
                    let h = row_hash_at(win, idx, hash_log, row_log, width);
                    match row_log {
                        4 => row_insert_fill::<4>(table, heads, h, win_base + idx as u64),
                        5 => row_insert_fill::<5>(table, heads, h, win_base + idx as u64),
                        _ => row_insert_fill::<6>(table, heads, h, win_base + idx as u64),
                    }
                    idx += 1;
                }
            },
            // The tree strategies fill their tree lazily from `next_update`.
            Strategy::Opt(_) | Strategy::BtLazy(_) => {},
        }
        self.gap_start = u64::MAX;
    }
}

mod gates;
mod hash;
mod params;
mod parse_btlazy;
mod parse_chain;
mod parse_dfast;
mod parse_fast;
mod parse_opt;
mod parse_row;
mod price;
mod tables;
#[cfg(test)]
mod tests;

pub(super) use gates::RampGate;
use gates::*;
pub(in crate::encoding) use hash::extend_match;
use hash::*;
use params::{
    BT_DENSE_LIMIT, BtStepPhase, HEAD_HASH_LOG, HEAD_KNOBS, HEAD_LIMIT, HEAD_MIN_TOTAL,
    HEAD_SYMS_MIN, HeadPhase, LDM_CANARY, LDM_FULL_WINDOW, LDM_MIDSIZE_WINDOW, LDM_QUIET,
    LEVEL_PARAMS, LdmFill, LevelParams, SmallDictRow, Strategy, ldm_min_window, params_for,
    sampled_distinct, small_dict_row,
};
pub(crate) use params::{LDM_SYMS_MIN, LdmArming};
#[cfg(feature = "std")]
pub(crate) use params::{far_repeat_dominant, ldm_head_parses};

/// Whether a frame at `level`/`shape` should run the pre-header far-class
/// screen (encoding::far_screen): a fast/dfast row with LDM whose stock
/// window sits below the far domain and whose declared length lands in
/// the mid-size band [LDM_MIDSIZE_WINDOW, LDM_FULL_WINDOW) — at the full
/// window the declared length arms on its own geometry, below the band
/// the row never arms, so neither side runs a screen (and no screen may
/// run at the full window: a far-class file whose repeats start past the
/// sample would be falsely rejected there, where geometry must keep
/// arming it).
pub(crate) fn fast_row_screen_pending(level: Level, shape: InputShape) -> bool {
    let params = params_for(level, shape);
    params.ldm
        && matches!(params.strategy, Strategy::Fast | Strategy::Dfast(_))
        && params.window < LDM_FULL_WINDOW
        && matches!(
            shape.len,
            Some(n) if (LDM_MIDSIZE_WINDOW as u64..LDM_FULL_WINDOW as u64).contains(&n)
        )
}
use parse_row::{row_hash_at, row_insert, row_insert_fill};
use price::*;
use tables::*;
pub(in crate::encoding) use tables::{pack_pos, push_seq_packed};
