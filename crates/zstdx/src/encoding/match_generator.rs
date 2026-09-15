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
//! former u64 slots. The opt strategies keep their own u64 tables with an
//! `(epoch << 48) | position` tag; a reset bumps that epoch instead of
//! clearing them, while the u32 tables need no clearing at all.

use alloc::vec::Vec;

use super::{
    Matcher, SeqWord, Sequence,
    btlazy::LazyScratch,
    ldm::{LdmSeq, LdmState},
    opt::{OptKnobs, OptScratch, OptState},
    reach_probe::{KEEP_REACH, ReachChoice, SHRINK_REACH},
    seq_codes::{decode_packed, pack_seq},
};
// Shared with the decoder so both sides agree on offset-history semantics.
use crate::decoding::sequence_execution::do_offset_history;
use crate::{InputShape, Level};

/// Shortest match worth encoding; matches the format's MINMATCH range.
pub(super) const MIN_MATCH: usize = 4;
/// The hash reads a full u64, so insertable/scannable positions need this
/// many window bytes ahead to stay in bounds.
pub(super) const HASH_READ: usize = 8;
/// Hash table size as a power of two.
const HASH_LOG: u32 = 15;
/// 5-byte window multiply prime (libzstd `prime5bytes`); only the low 64
/// bits of the product feed the slot index.
const HASH_PRIME: u64 = 0xc2b2_ae3d_27d4_eb4f;
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
/// Backward bytes that must agree (beyond the 8-byte anchor) before a strip
/// position becomes the job-start seed offset: long enough that word-level
/// repeats (~10-15 agreeing bytes on natural text) cannot qualify, short
/// enough that one cache line of checking settles it.
const SEED_AGREE: usize = 48;
/// Spread anchors behind the 56-byte agreement window where a seed candidate
/// must still agree (one u64 compare each). The window alone cannot tell a
/// strip-long period from a long local repeat: a duplicated block of ≥56
/// bytes near the strip tail agrees with itself at every offset inside the
/// block, and the nearest-first scan prefers it (measured: a 194-byte
/// duplicated source statement at offset 2244 beat a 441226-byte tile period
/// and burned the seed budget, collapsing that job's ratio). A genuine
/// period or duplicated block agrees at every distance it spans, so spread
/// confirms separate the classes; anchors that underflow the candidate's own
/// history are skipped (short strips).
const SEED_CONFIRMS: [usize; 3] = [64, 640, 6144];
/// Seed matches to emit before retiring the seed: three literal offsets
/// both clear the repcode gate and rotate `rep` until `rep[0]` holds the
/// seed offset, so the regular repcode probes take over from there.
const SEED_MATCHES: u8 = 3;
/// Marker for the const-generic dfast logs meaning "read the runtime
/// value" (real logs are never zero); only clamped-window shapes
/// (inputs small enough to shrink the row's tables) take that path.
const RUNTIME_LOG: u32 = 0;

/// Probe attempts an unused seed survives: a seed whose offset stops
/// matching (broken period, or a repeated block that ended) must not pay a
/// dead compare for the rest of the job.
const SEED_BUDGET: u32 = 8192;
/// Strip length below which the seed scan stays scalar: the AVX-512 block
/// walk's lowest block reads up to byte 63 + 71, and `last + 8 >= 136`
/// keeps every such load inside data. Shorter strips are cheap anyway.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
const SEED_SCAN_MIN: usize = 128;
/// History kept for matching; also the window size declared in the frame header.
const MAX_WINDOW: usize = 0xc0000;

/// Deep-offset ramp at multithreaded job starts (decode-parallelism
/// experiment; see `encoding::mt`): while armed, a match whose source lies
/// below `start` (a cross-boundary read into the previous job's output)
/// must reach at least `depth` bytes below `start`. Sources at or after
/// `start` stay legal at any offset — they are in-job reads the executing
/// piece produces itself. This DEPTH semantics (not an offset floor) is
/// what a parallel stage B needs: a piece's completion interval is bounded
/// by its shallowest crossing read, wherever that read sits in the piece.
/// All-zero disables the gate; the hot-path check folds to two
/// comparisons.
#[derive(Clone, Copy)]
pub(crate) struct RampGate {
    /// Absolute job start; sources below it are cross-boundary reads.
    start: u64,
    /// Armed marker (the ramp region end `start + depth`; 0 = off).
    end: u64,
    /// Minimum cross-boundary depth.
    depth: u64,
}

impl RampGate {
    pub(crate) const OFF: Self = Self {
        start: 0,
        end: 0,
        depth: 0,
    };

    /// Whether the gate is armed; scan-loop instantiation choice keys off
    /// this (disarmed loops compile the checks out entirely).
    #[inline(always)]
    fn is_armed(&self) -> bool {
        self.end != 0
    }

    /// Whether the match `(pos_abs, cand_abs)` violates the ramp: a source
    /// inside the `depth` bytes just below the job start. `pos_abs` is
    /// unused — the constraint is position-independent (the depth of a
    /// crossing read is `start - cand`, not its offset).
    #[inline(always)]
    fn blocks(&self, pos_abs: u64, cand_abs: u64) -> bool {
        let _ = pos_abs;
        self.end != 0 && cand_abs < self.start && cand_abs + self.depth > self.start
    }

    /// Backward-extension floor for a match's source, in window indices.
    /// Extension moves source and match back in lockstep, so the source
    /// walks contiguously: one at or after the job start must stop AT the
    /// boundary — letting it pass would drag it through the whole shallow
    /// band `start - depth .. start`, and an extension that dies inside
    /// the band would emit an illegal shallow crossing read. A source
    /// already below the band (it passed `blocks`) only gets deeper under
    /// extension, so it extends down to the window floor.
    #[inline(always)]
    fn ext_floor(&self, cand_idx: usize, win_base: u64) -> usize {
        if self.end != 0 && win_base + cand_idx as u64 >= self.start {
            self.start.saturating_sub(win_base) as usize
        } else {
            0
        }
    }
}

/// Const-generic [`RampGate`] checks: the fast scan loops instantiate with
/// `RAMPED = false` on disarmed frames (all bulk-ST paths; MT jobs without
/// the env ramp), so the gate's three live u64s and its per-site branches
/// fold away entirely instead of riding the loop as never-taken state.
#[inline(always)]
fn ramp_blocks<const RAMPED: bool>(ramp: RampGate, pos_abs: u64, cand_abs: u64) -> bool {
    RAMPED && ramp.blocks(pos_abs, cand_abs)
}

#[inline(always)]
fn ramp_ext_floor<const RAMPED: bool>(ramp: RampGate, cand_idx: usize, win_base: u64) -> usize {
    if RAMPED {
        ramp.ext_floor(cand_idx, win_base)
    } else {
        0
    }
}

/// Log of the incompressibility gate's probe table (see
/// [`Matcher::skip_if_incompressible`]). 32K tag slots keep the overwrite
/// rate of a block's ~2000 samples negligible.
const GATE_PROBE_LOG: u32 = 15;
/// Blocks below this pay little enough in the matcher that the gate's
/// sample pass is not worth its own cost.
const GATE_MIN_BLOCK: usize = 16384;

// The opt strategies' never-valid table entry; their positions live in
// the low 48 bits with the epoch in the high 16 (see
// [`MatchGeneratorDriver::reset`]).
const EMPTY: u64 = 0;

/// Encode an absolute position as a fast/dfast/chain table entry. The +1
/// bias keeps a zeroed slot dead (no position maps to zero), so fresh and
/// unwritten slots never resolve; position `2^32 - 1` collides with the
/// sentinel — one dead insert per 4 GiB cycle, pure noise.
#[inline(always)]
pub(super) fn pack_pos(abs: u64) -> u32 {
    (abs as u32).wrapping_add(1)
}

/// Decode a fast/dfast/chain entry against the scanning position `pos`:
/// the entry's high bits were truncated, so rebuild them from `pos` and
/// unwrap one 4 GiB cycle when the value landed above it. Live entries are
/// strictly older than `pos`, so a reconstruction at or above it wraps —
/// and a wrap that underflows unmasks a stale entry from an earlier frame
/// (whose positions share no domain with `pos`); those die like empty
/// slots. The caller's window-range check and the byte compare behind it
/// decide the rest.
///
/// The scan loops now resolve entries as distances (see [`chain_search`]
/// and the fast/dfast `resolve` closures), so only the entry-roundtrip
/// tests exercise this exact form.
#[cfg(test)]
#[inline(always)]
fn unpack_pos(entry: u32, pos: u64) -> Option<u64> {
    if entry == 0 {
        return None;
    }
    let cand = (pos & !(u32::MAX as u64)) + entry as u64 - 1;
    let cand = if cand >= pos {
        cand.checked_sub(1 << 32)?
    } else {
        cand
    };
    Some(cand)
}

/// Which search loop the matcher runs. The `chain` buffer is the second
/// table for the two-table strategies and empty for [`Strategy::Fast`].
#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    /// One single-probe table keyed by a 5-byte hash (level Fastest,
    /// libzstd's `fast`).
    Fast,
    /// Two single-probe tables: an 8-byte long hash in `table` and a
    /// 5-byte short hash in `chain` (level Fast, libzstd's `dfast`).
    /// The payload is the short-table log.
    Dfast(u32),
    /// Head table plus chain links walked up to the level's search depth
    /// with lazy deferral (levels above Fast, libzstd's `lazy` family).
    /// The payload is the chain-table log.
    Chain(u32),
    /// Optimal-price parser over a binary match tree (levels Opt/Ultra,
    /// libzstd's btopt/btultra). The `chain` buffer holds the tree ring.
    Opt(OptKnobs),
    /// Lazy2 selection over the optimal parser's binary tree (rows 13-15,
    /// libzstd's `btlazy2`): the tree of [`Strategy::Opt`] without the DP.
    /// The `chain` buffer holds the tree ring.
    BtLazy(OptKnobs),
}

/// Per-level search parameters. One row per numeric level (0-22), modeled
/// on libzstd's `clevels.h` large-source table and adapted to this crate's
/// strategy family: libzstd's greedy/lazy/lazy2 map onto [`Strategy::Chain`]
/// with `lazy_depth` 0/1/2 and `min_match` from the row's search length,
/// btlazy2 onto [`Strategy::BtLazy`], and btopt/btultra(2) onto
/// [`Strategy::Opt`] knobs. Deviations from the libzstd rows are documented
/// per row.
#[derive(Clone, Copy, PartialEq)]
struct LevelParams {
    hash_log: u32,
    /// Match window; also the window declared in the frame header.
    window: usize,
    strategy: Strategy,
    /// Chain-family searches per position (libzstd's `1 << searchLog`).
    search_depth: u32,
    /// 0 = greedy, 1 = lazy, 2 = lazy2 (libzstd's deferred-position count).
    lazy_depth: u32,
    /// Shortest hash-chain match worth emitting; libzstd's searchLength.
    /// Repcode matches stay legal from MIN_MATCH up.
    min_match: u32,
    /// Parse the frame's cold-start head through the btlazy2 driver before
    /// this row's chain takes over (see [`HEAD_LIMIT`]).
    dubt_head: bool,
    /// Run the gear-hash long-distance matcher as an extra candidate
    /// source for the chain scan (see [`super::ldm`]).
    ldm: bool,
    /// Chain search-domain override (the walk's reach), for rows whose
    /// far distance classes ride LDM candidates instead of the window:
    /// the window (frame header, buffer, LDM reach) stays the row's
    /// `window`; `None` keeps search domain == window.
    chain_reach: Option<usize>,
}

const fn fast(hash_log: u32, window: usize) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::Fast,
        search_depth: 1,
        lazy_depth: 0,
        min_match: MIN_MATCH as u32,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
    }
}

const fn dfast(hash_log: u32, small_log: u32, window: usize) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::Dfast(small_log),
        search_depth: 0,
        lazy_depth: 0,
        min_match: MIN_MATCH as u32,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
    }
}

const fn chain(
    hash_log: u32,
    chain_log: u32,
    window: usize,
    search_depth: u32,
    lazy_depth: u32,
) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::Chain(chain_log),
        search_depth,
        lazy_depth,
        min_match: 5,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
    }
}

/// `chain` with the cold-start DUBT head enabled (row 9).
const fn with_head(mut p: LevelParams) -> LevelParams {
    p.dubt_head = true;
    p
}

/// `chain` with the gear-hash long-distance matcher as an extra candidate
/// source (see [`super::ldm`]).
const fn with_ldm(mut p: LevelParams) -> LevelParams {
    p.ldm = true;
    p
}

/// `chain` with the search domain (walk reach) narrowed below the window:
/// LDM candidates carry the classes beyond it.
const fn with_chain_reach(mut p: LevelParams, reach: usize) -> LevelParams {
    p.chain_reach = Some(reach);
    p
}

const fn opt(hash_log: u32, window: usize, knobs: OptKnobs) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::Opt(knobs),
        search_depth: 0,
        lazy_depth: 0,
        min_match: knobs.min_match,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
    }
}

const fn btlazy(hash_log: u32, window: usize, knobs: OptKnobs) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::BtLazy(knobs),
        search_depth: 0,
        lazy_depth: 2,
        min_match: knobs.mls,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
    }
}

/// btlazy2 row knobs: `min_match` stays 4 (the lazy family's accept bar and
/// repcode compare width — libzstd's loop), while `mls` carries the row's
/// searchLength (5: hash5 keys the tree). `sufficient_len` is the rows'
/// targetLength. The DUBT finder keys off `search_log`/`bt_log` alone —
/// its fill is O(1) by construction, so the opt tree's fill-budget knobs
/// carry defaults that the strategy never reads.
const fn bt_knobs(search_log: u32, bt_log: u32) -> OptKnobs {
    OptKnobs {
        search_log,
        insert_log: search_log,
        sufficient_len: 32,
        min_match: 4,
        mls: 5,
        bt_log,
        hash3_log: 0,
        ultra: false,
    }
}

/// Cold-start DUBT head (row 9): the span of frame-start blocks parsed
/// through the btlazy2 driver before the chain takes over. The chain's
/// newest-first selection is the balanced tier's json edge but accepts
/// nearer-shorter candidates while the tables are still filling (text's
/// cold-start tile deficit); the tree's oldest-first order closes exactly
/// that window, and after it the chain parses at parity. The span covers
/// the measured deficit (the corpus's first tile, 768 KiB) plus the tile
/// behind it: matches into the freshly covered region are tree-selected
/// too, worth more than the head's own span again on the tiled corpus.
const HEAD_LIMIT: u64 = 9 * crate::common::MAX_BLOCK_SIZE as u64;
/// Frames shorter than this keep the pure chain parse: a head would cover
/// the whole input (small payloads are all cold start, and json-class
/// smalls favor the chain's selection).
const HEAD_MIN_TOTAL: u64 = 4 << 20;
/// Distinct bytes required in the first parsed block for the head to run:
/// on small-symbol alphabets the tree's batch sort collapses at any search
/// depth (skewed), while the chain's selection there has no deficit.
const HEAD_SYMS_MIN: u32 = 48;
/// Head-table log (the heads table the btlazy bridge sizes from
/// `dubt_table`); the ring and search knobs ride [`HEAD_KNOBS`].
const HEAD_HASH_LOG: u32 = 20;
/// Head knobs: S4 search (libzstd's L13 depth — shallower trees accept
/// nearer-shorter candidates again, S1-S3 measured at -1.3 to -4.4% vs
/// zstd-9) over a 1 MiB ring — head candidates never sit further back
/// than the head span itself.
const HEAD_KNOBS: OptKnobs = bt_knobs(4, 20);

/// Quiet blocks before the LDM generation latch shuts off: a winless
/// megabyte is a shape whose far class either does not exist or never
/// wins — the split pass (gear + xxh over every 64-byte window, ~9 cyc/B
/// measured) is pure tax from there on. Any win resets the latch; the
/// false-kill risk (a far duplicate isolated by winless content on both
/// sides) is bounded by the canary revival below.
const LDM_QUIET: u8 = 16;

/// Blocks between canary generations while latched off: one block per
/// megabyte re-runs the split pass, and a canary whose beyond-reach
/// candidates exist revives the matcher — far-match density can ramp up
/// anywhere in the file, and the quiet stretch that latched cannot prove
/// the class absent beyond what it scanned.
const LDM_CANARY: u8 = 8;

/// Smallest source-downsized window that arms LDM under
/// [`LdmArming::Frame`]: one step under the row's W26, so a 32 MiB source
/// (clamped to W25) still arms — the dll32-class far classes measured
/// −19.7% size there — while the quiet latch bounds the discovery cost the
/// same-size far-less shapes pay.
const LDM_MIDSIZE_WINDOW: usize = 1 << 25;

/// The [`LdmArming::Job`] bar: the row's full window, meaning the source
/// clamp left it intact.
const LDM_FULL_WINDOW: usize = 1 << 26;

/// Sampled distinct-byte count the mid-size population's first parsed
/// block must show to stay armed: low alphabets (skewed's 16 symbols,
/// text's ~100 ASCII) produce structural 64-byte repeats whose far twins
/// never survive the price of their offset — candidates exist and keep
/// the latch alive, but no sequence ever emits, so the split pass runs at
/// near-always-on duty (skewed measured −35% solo). Binary content (dll,
/// 256) clears it by a wide margin. The full-window population skips the
/// check (its bytes are frozen).
const LDM_SYMS_MIN: u32 = 128;

/// Why a block the scan will not parse reaches the LDM indexer.
#[derive(Clone, Copy, PartialEq)]
enum LdmFill {
    /// Parsed by the cold-start DUBT head: real content, and the far class
    /// sources the frame start through it.
    Head,
    /// Incompressibility-gated or RLE-skipped: max-entropy or uniform
    /// bytes whose 64-byte windows hold no twin worth a far offset.
    Skipped,
}

/// Strided distinct-byte count over `win[start..end]`, the incompressibility
/// gate's sampling idiom: ~2048 samples read a 128-symbol alphabet to ~100,
/// a 256-symbol one to ~247 — both margins around [`LDM_SYMS_MIN`] are wide.
fn sampled_distinct(win: &[u8], start: usize, end: usize) -> u32 {
    let stride = ((end - start) >> 11) | 1;
    let mut bitmap = [0u64; 4];
    let mut i = start;
    while i < end {
        let b = win[i] as usize;
        bitmap[b >> 6] |= 1 << (b & 63);
        i += stride;
    }
    bitmap.iter().map(|w| w.count_ones()).sum()
}

/// Where a driver's LDM history domain ends, deciding the size gate's bar
/// (see [`ldm_min_window`]). Set before `reset`; pooled drivers re-derive
/// their arming on every reset.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum LdmArming {
    /// Frame-continuous history (bulk and single-threaded streaming): the
    /// mid-size bar arms, and the quiet latch bounds the split-pass tax on
    /// shapes whose far class never shows.
    #[default]
    Frame,
    /// A multithreaded job: the table restarts per job, so only strip- and
    /// own-span-sourced candidates exist — the strip fill is an
    /// unconditional per-job split pass that far-less shapes pay in full,
    /// so the bar stays at the unclamped row window and mid-size frames
    /// keep the pure chain parse in MT (deterministic: the bar depends on
    /// the frame's shape alone, never the worker assignment).
    Job,
    /// The reach probe's parses: the keep parse's span sits below the
    /// row's chain reach, so no candidate can survive the beyond-reach
    /// filter there (the shrink parse never arms at all — a shrunk parse
    /// abandons LDM, see the size gate). Skipping the split pass is
    /// byte-exact either way: it changes no sequence the probe could cost.
    ProbeKeep,
}

/// The size gate's arming bar: the smallest (post source-clamp) window
/// that arms LDM under `arming`, or `None` to never arm.
const fn ldm_min_window(arming: LdmArming) -> Option<usize> {
    match arming {
        LdmArming::Frame => Some(LDM_MIDSIZE_WINDOW),
        LdmArming::Job => Some(LDM_FULL_WINDOW),
        LdmArming::ProbeKeep => None,
    }
}

/// Lifecycle of the cold-start DUBT head (row 9, see [`HEAD_LIMIT`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum HeadPhase {
    /// Not running this frame: the row opts out, the input is too small,
    /// the alphabet gate failed, or a strip/dictionary prefilled the start.
    Off,
    /// Armed at the frame start; the first parsed block evaluates the
    /// alphabet gate and arms the head tables.
    Armed,
    /// Gate passed; head blocks parse through the btlazy2 driver.
    Running,
    /// The head parsed its span and handed off to the chain.
    Done,
}

const fn knobs(
    search_log: u32,
    sufficient_len: u32,
    min_match: u32,
    bt_log: u32,
    ultra: bool,
) -> OptKnobs {
    OptKnobs {
        search_log,
        insert_log: search_log,
        sufficient_len,
        min_match,
        mls: min_match,
        bt_log,
        hash3_log: if min_match == 3 {
            17
        } else {
            0
        },
        ultra,
    }
}

/// The per-level parameter ladder, indexed by numeric level. Level 0 (raw
/// blocks) never matches; its row only keeps the state reusable.
///
/// libzstd rows for reference (W, C, H, S, L, TL, strategy):
/// 1: 19,13,14,fast · 2: 20,15,16,fast · 3: 21,16,17,dfast · 4: 21,18,18,dfast ·
/// 5: 21,18,19,greedy · 6: 21,18,19,lazy · 7: 21,19,20,lazy · 8-12: lazy2 ·
/// 13-15: btlazy2 · 16-17: btopt · 18: btultra · 19-22: btultra2.
const LEVEL_PARAMS: [LevelParams; 23] = [
    // 0: raw blocks.
    fast(HASH_LOG, MAX_WINDOW),
    // 1: libzstd L1 keeps a W19/H14 table; our H15/768 KiB row is the
    // tuned Fastest tier (beyond-W21 expansion measured a loss).
    fast(HASH_LOG, MAX_WINDOW),
    fast(16, 1 << 20),
    // 3: the tuned Fast tier; same-window A/B sits within 0.03% of
    // libzstd's L3 dfast (H17 long / C16 short).
    dfast(17, 16, 1 << 21),
    dfast(18, 18, 1 << 21),
    // Chain rows 5-12: search depths are half libzstd's 1 << searchLog
    // (8/8/16/16/16/32/64/64 there) — the old Balanced tier calibrated
    // depth 8 as its zstd-9 row (S4), our per-probe walk being the dearer.
    chain(19, 18, 1 << 21, 4, 0),
    chain(19, 18, 1 << 21, 4, 1),
    chain(20, 19, 1 << 21, 8, 1),
    chain(20, 19, 1 << 21, 8, 2),
    // 9: the Balanced tier's row. libzstd's L9 is W22; the chain stays
    // C20, aliasing beyond 1 MiB like libzstd's cLog-below-wLog chains.
    // The cold-start head parses the first HEAD_LIMIT bytes through the
    // DUBT tree: the chain's newest-first selection accepts nearer-shorter
    // candidates while no cross-tile history exists yet (the text
    // cold-start deficit), which the tree's oldest-first order closes.
    // LDM adds the gear-hash candidate source for the window's far
    // distance classes (see [`super::ldm`]).
    with_ldm(with_head(with_chain_reach(
        chain(21, 20, 1 << 26, 8, 2),
        1 << 22,
    ))),
    chain(22, 21, 1 << 22, 16, 2),
    chain(22, 21, 1 << 22, 32, 2),
    chain(23, 22, 1 << 22, 32, 2),
    // 13: the Best tier: btlazy2 — the DUBT tree (O(1) fill, search-time
    // batch sort) under lazy2 selection, libzstd's L13-15 rows (S4/5/6,
    // searchLength 5, TL 32). The frame window rides LDM's W26 far reach
    // with the tree's search domain at the stock W22.
    with_ldm(with_chain_reach(
        btlazy(22, 1 << 26, bt_knobs(4, 22)),
        1 << 22,
    )),
    with_ldm(with_chain_reach(
        btlazy(23, 1 << 26, bt_knobs(5, 22)),
        1 << 22,
    )),
    with_ldm(with_chain_reach(
        btlazy(23, 1 << 26, bt_knobs(6, 23)),
        1 << 22,
    )),
    // 16: libzstd's btopt rows begin. The opt family keeps a wide frame
    // window (W26, the LDM far reach — C's --long shape) with the tree's
    // search domain at the stock row window via `chain_reach`; LDM carries
    // the distance classes beyond the domain.
    with_ldm(with_chain_reach(
        opt(22, 1 << 26, knobs(5, 48, 4, 22, false)),
        1 << 22,
    )),
    // 17: the Opt tier; libzstd's L17 row with the ring capped one below
    // its C23 (shorter candidate walks: json −18% time for +0.16% dll).
    with_ldm(with_chain_reach(
        opt(22, 1 << 26, knobs(5, 64, 4, 22, false)),
        1 << 23,
    )),
    // 18: btultra: fractional-bit prices, mls 3 (hash3 at H17).
    with_ldm(with_chain_reach(
        opt(22, 1 << 26, knobs(6, 64, 3, 23, true)),
        1 << 23,
    )),
    // 19: the Ultra tier. libzstd's C24 ring is byte-identical to C23
    // here (proven), so the ring stays 23.
    with_ldm(with_chain_reach(
        opt(22, 1 << 26, knobs(7, 256, 3, 23, true)),
        1 << 23,
    )),
    // 20-22: libzstd widens to W25-27 with C25-27/H23-25. Our u64-tagged
    // opt tables would cost 2-8x libzstd's u32 memory there, so the ring
    // caps at 24 and the hash stays at 22 (H23 measured 0.06-0.13% sparser
    // on json — pure dilution); windows widen to W24-26, the reach
    // dll-class data needs. Beyond-window distance classes are LDM's job.
    opt(22, 1 << 24, knobs(7, 256, 3, 23, true)),
    opt(22, 1 << 25, knobs(7, 512, 3, 24, true)),
    opt(22, 1 << 26, knobs(8, 999, 3, 24, true)),
];

fn params_for_level(level: Level) -> LevelParams {
    LEVEL_PARAMS[level.as_i32().clamp(0, 22) as usize]
}

/// [`params_for_level`] adjusted to what the caller declared about the
/// input: a forced window log overrides the row's window first, then a
/// known length downsizes (libzstd's override-then-adjust order, so a
/// known smaller length still clamps a forced window). Raw-block frames
/// keep the row untouched (their declared window never affects bytes, and
/// staying shape-independent keeps outputs stable).
fn params_for(level: Level, shape: InputShape) -> LevelParams {
    let mut p = params_for_level(level);
    if let Some(wl) = shape.window_log {
        p.window = 1usize << wl.clamp(10, 27);
    }
    if level == Level::Uncompressed {
        p
    } else {
        adjust_params(p, shape.len)
    }
}

/// Downsize a row for a known source length — the window/table half of
/// libzstd's `ZSTD_adjustCParams_internal`: the window clamps to the
/// source's log (so small inputs never pay large-input reach or memory),
/// the hash tables to windowLog+1, the chain/ring cycle to windowLog.
/// `None` (unknown size) keeps the row untouched, like libzstd's
/// ZSTD_CONTENTSIZE_UNKNOWN.
fn adjust_params(mut p: LevelParams, src: Option<u64>) -> LevelParams {
    let Some(n) = src.filter(|&n| n > 0) else {
        return p;
    };
    // srcLog = ceil(log2(max(n, 2^HASHLOG_MIN))), floored at the frame
    // format's minimum window log.
    let t = n.max(1u64 << 6);
    let src_log = (64 - (t - 1).leading_zeros()).max(10);
    // Non-power-of-two windows (the Fastest row's 768 KiB) round up.
    let wlog = (64 - (p.window as u64 - 1).leading_zeros()).min(src_log);
    p.window = 1usize << wlog;
    p.chain_reach = p.chain_reach.map(|r| r.min(p.window));
    p.hash_log = p.hash_log.min(wlog + 1);
    p.strategy = match p.strategy {
        Strategy::Dfast(small) => Strategy::Dfast(small.min(wlog + 1)),
        Strategy::Chain(c) => Strategy::Chain(c.min(wlog)),
        Strategy::Opt(mut knobs) => {
            knobs.bt_log = knobs.bt_log.min(wlog);
            knobs.hash3_log = knobs.hash3_log.min(wlog);
            Strategy::Opt(knobs)
        },
        Strategy::BtLazy(mut knobs) => {
            knobs.bt_log = knobs.bt_log.min(wlog);
            knobs.hash3_log = knobs.hash3_log.min(wlog);
            Strategy::BtLazy(knobs)
        },
        Strategy::Fast => Strategy::Fast,
    };
    p
}

/// Hash a window u64 whose low 5 bytes are the hashed prefix (the full u64
/// load feeds the multiplier directly: bits above the fifth byte only add
/// input entropy) into a table of `log` bits. Five bytes skip the frequent
/// 4-byte boilerplate fragments so probes land on structural repeats
/// instead of recent junk.
#[inline(always)]
fn hash5_log(v: u64, log: u32) -> usize {
    // No low mask: the >> (64 - log) already leaves exactly `log` bits, and
    // a runtime `log` would make LLVM rebuild the mask per call.
    (v & 0x00ff_ffff_ffff).wrapping_mul(HASH_PRIME) as usize >> (64 - log)
}

/// Hash the 5 bytes at `idx` into a table of `log` bits. Caller guarantees
/// `idx + 5 <= win.len()` (the scanning and emit loops bound-check once per
/// loop, not per position).
#[inline(always)]
fn hash_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: see the contract above; the hash itself is [`hash5_log`].
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & 0x00ff_ffff_ffff;
        v.wrapping_mul(HASH_PRIME) as usize >> (64 - log)
    }
}

/// Hash the 8 bytes at `idx` into the dfast long table (libzstd's
/// `prime8bytes` multiply). Same read contract as [`hash_at_log`].
#[inline(always)]
fn hash8_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at_log.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned();
        v.wrapping_mul(0xcf1b_bcdc_b7a5_6463) as usize >> (64 - log)
    }
}

/// Read 4 window bytes at `idx`. Caller guarantees `idx + 4 <= win.len()`.
#[inline(always)]
fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
}

/// Read 8 window bytes at `idx`. Caller guarantees `idx + 8 <= win.len()`.
#[inline(always)]
fn read8(win: &[u8], idx: usize) -> u64 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u64>().read_unaligned() }
}

/// Longest common prefix of `win[i..]` and `win[j..]` in u64 chunks. `i` is
/// the current scan position and `j` a candidate strictly before it, so
/// bounding by `i` also bounds `j`.
#[inline(always)]
pub(super) fn extend_match(win: &[u8], i: usize, j: usize) -> usize {
    let limit = win.len() - i;
    let base = win.as_ptr();
    let mut len = 0;
    // SAFETY: i and j are valid indices and i + len + 8 <= win.len() bounds
    // the reads on both sides (j <= i).
    unsafe {
        while len + 8 <= limit {
            let a = base.add(i + len).cast::<u64>().read_unaligned();
            let b = base.add(j + len).cast::<u64>().read_unaligned();
            if a == b {
                len += 8;
            } else {
                return len + ((a ^ b).trailing_zeros() >> 3) as usize;
            }
        }
    }
    while len < limit && win[i + len] == win[j + len] {
        len += 1;
    }
    len
}

/// Offset price for the chain strategy's lazy gain comparisons (libzstd's
/// `ZSTD_highbit32(offBase)` with offBase = offset+1): the offset-code
/// exponent in ~bits. Repcode incumbents price 0.
#[inline(always)]
fn price_of(idx: usize, cand: usize) -> i32 {
    ((idx - cand + 1) as u32).ilog2() as i32
}

/// Assumed literal code lengths before the block encoder's first
/// measurement arrives: the historical flat constant of the static gate
/// (~4 bits per literal).
const DEFAULT_LIT_LENS: [u8; 256] = [4; 256];

/// Whether a non-rep match clears its offset's price: literals cost ~4 bits
/// per byte and the offset its highbit, plus a constant for the sequence
/// code overhead (libzstd's raw approximation from its lazy gain checks).
/// Without this gate the densely pre-indexed multithread job strips flood
/// the stream with five-byte matches a megabyte back — high-entropy shapes
/// lost ratio and speed alike to the emission storm.
/// The +7 margin is libzstd's depth-2 replacement constant; used here at
/// the fast/dfast seed probes and as the constant term of the chain
/// strategy's literal-cost-aware gate ([`pays_for_offset_lit`]).
#[inline(always)]
fn pays_for_offset(ml: usize, idx: usize, cand: usize, rep_hit: bool) -> bool {
    rep_hit || ml * 4 >= (idx - cand).ilog2() as usize + 7
}

/// The chain strategy's store gate: store the match only if the literals it
/// displaces cost more than its offset's price (highbit plus the +7 margin,
/// libzstd's depth-2 replacement constant). Literal bytes price at the code
/// lengths of the previous block's Huffman table — the *marginal* cost,
/// which separates the shapes the flat +7 margin traded against each other
/// (json 6.21/6.55, text 368.1/366.3 at +4/+7; the whole swing is 5-byte
/// matches in the 8-32 KiB offset band): text's displaced bytes are nearly
/// absent from its hyper-skewed residual literal stream (~0.2 bits/B on
/// average — the average itself is no discriminator) and price at the cap,
/// while json's are common in its residual stream and price cheap. For
/// matches longer than eight bytes the first eight price the run (the
/// decision band is ml ≤ 7 anyway: at the 1 MiB window an 8-byte match
/// clears even the flat-4 constant). With `lit_lens == DEFAULT_LIT_LENS`
/// this reduces to the static +7 gate bit-for-bit.
#[inline(always)]
fn pays_for_offset_lit(
    win: &[u8],
    idx: usize,
    ml: usize,
    cand: usize,
    rep_hit: bool,
    lit_lens: &[u8; 256],
) -> bool {
    if rep_hit {
        return true;
    }
    let price = (idx - cand).ilog2() as usize + 7;
    let k = ml.min(6);
    let mut cost = 0usize;
    for &b in &win[idx..idx + k] {
        cost += lit_lens[b as usize] as usize;
    }
    cost * ml >= price * k
}

/// The lazy walk's displaced-literal value of a match: the bits the match
/// saves by covering `len` bytes instead of emitting them as literals,
/// estimated from the first four bytes (every walk candidate is at least
/// MIN_MATCH long) at the code lengths fed back by the block encoder (see
/// [`MatchGeneratorDriver::lit_lens`]), clamped to 6 bits per byte — the
/// walk is a *local* selection between adjacent positions, and uncapped
/// 11-bit prices let cap-priced bytes dominate a whole walk's decisions
/// (measured: no clamp / 8 / 6 / 5 on balanced bulk-st sizes — json
/// 4726875/4701990/4697420/4725425, text 91243/91232/91200/91140; the
/// json swing dwarfs the text one at every step). With
/// `lit_lens == DEFAULT_LIT_LENS` this is exactly `len * 4`, so the walk's
/// gain comparisons reduce bit-for-bit to the flat-scale versions.
#[inline(always)]
fn lit_value(win: &[u8], idx: usize, len: usize, lit_lens: &[u8; 256]) -> i32 {
    debug_assert!(len >= MIN_MATCH);
    let s = (lit_lens[win[idx] as usize] as i32).min(6)
        + (lit_lens[win[idx + 1] as usize] as i32).min(6)
        + (lit_lens[win[idx + 2] as usize] as i32).min(6)
        + (lit_lens[win[idx + 3] as usize] as i32).min(6);
    s * len as i32 / 4
}

/// Chain-walk search from the hash head `entry` at window index `idx`,
/// returning the longest match's (length, candidate window index) —
/// `(0, usize::MAX)` when nothing matched. Every candidate is beat-checked,
/// so stale chain links (see the sparse fill in rep1_chain) only cost probes,
/// never correctness.
///
/// The head entry is passed in (not loaded) because the caller's insert
/// block links its chain slot to the same value: the position is hashed and
/// the head read exactly once per scan step (nothing writes the table
/// between the probe and the insert). `#[inline(always)]` with the same
/// budget rationale as [`TableEmit::rep1_chain`]: left to its own judgment
/// the inliner outlines the body (two call sites in one scan loop), and the
/// calling convention — a closure environment re-loaded per call plus a
/// dozen stack round-trips — measured at ~66% of json.balanced cycles.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn chain_search(
    win: &[u8],
    chain: *const u32,
    idx: usize,
    entry: u32,
    win_base: u64,
    block_end: u64,
    search_depth: usize,
    chain_mask: usize,
    max_window: u64,
    ramp: RampGate,
) -> (usize, usize) {
    let pos_abs = win_base + idx as u64;
    // Oldest usable candidate age: within the level window and inside the
    // live window buffer, folded into one distance compare (below) — the
    // same resolve form as the fast loop's.
    let reach = (pos_abs - win_base).min(max_window);
    let mut best_len = 0usize;
    let mut best_cand = usize::MAX;
    let mut tried = 0usize;
    let mut dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
    // `dist - 1 < reach` admits exactly dist ∈ [1, reach]: dist 0 is a stale
    // slot holding this very position (pack_pos is injective per 4-GiB
    // cycle, but old-cycle slots alias anything), whose candidate would
    // byte-compare against itself; larger dist wraps huge for the empty
    // sentinel, stale 4-GiB-cycle entries and at-or-newer-than-pos
    // reconstructions. The walk's monotone (links target strictly older
    // positions) keeps an out-of-window dist the exact break the old
    // candidate-floor check was. The subtraction is wrapping for the same
    // reason: dist 0 must wrap to u64::MAX to be rejected, not panic in
    // debug builds.
    while tried < search_depth && dist.wrapping_sub(1) < reach {
        let cand_abs = pos_abs - dist;
        let cand = (cand_abs - win_base) as usize;
        // Beat-check (libzstd's "potentially better" read): the 4 bytes
        // ending at best_len+1 decide whether the candidate can strictly
        // improve, so most hash collisions reject on one load instead of a
        // full extend. With no best yet the probe sits at 0, the plain
        // first-4 compare. The probe stays inside the block: still looping
        // means best_len is short of the block end (the break below fires
        // otherwise), so [probe, probe+4) ends at most at the block end.
        let probe = best_len.saturating_sub(3);
        if !ramp.blocks(pos_abs, cand_abs) && read4(win, cand + probe) == read4(win, idx + probe) {
            let ml = extend_match(win, idx, cand);
            if ml > best_len {
                best_len = ml;
                best_cand = cand;
                // Cannot be improved on within this block.
                if pos_abs + ml as u64 >= block_end {
                    break;
                }
            }
        }
        tried += 1;
        // SAFETY: masked to the chain table size.
        let entry = unsafe { *chain.add(cand_abs as usize & chain_mask) };
        dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
    }
    (best_len, best_cand)
}

/// Store `abs` as the newest position for its hash into a table of `log`
/// bits. Caller guarantees `idx` has at least 5 bytes of window behind it.
#[inline(always)]
fn insert_at(win: &[u8], table: &mut [u32], idx: usize, abs: u64, log: u32) {
    // SAFETY: hash_at_log masks to log bits and the table holds
    // 1 << log slots, so the index cannot leave it.
    unsafe {
        *table.get_unchecked_mut(hash_at_log(win, idx, log)) = pack_pos(abs);
    }
}

/// Push one sequence's literals and packed code/add-bits streams, and
/// update the repeated-offset history. Shared by the fast, chain and opt
/// emit paths; returns the literal length and the window-relative match end.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) fn push_seq_packed(
    win: &[u8],
    win_base: u64,
    anchor: u64,
    start: usize,
    match_len: usize,
    of_value: u32,
    rep: &mut [u32; 3],
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) -> (u32, usize) {
    let anchor_idx = (anchor - win_base) as usize;
    let ll = (start - anchor_idx) as u32;
    // Most sequences carry only a handful of literals; an out-of-line memcpy
    // per match costs more than the copy itself. Half the sequences on
    // structured data are repcode chains with no literals at all — their
    // copy is skipped outright.
    let lits = &win[anchor_idx..start];
    if ll == 0 {
        do_offset_history(of_value, 0, rep);
        seqs.push(pack_seq(0, match_len as u32, of_value));
        return (0, start + match_len);
    }
    // SAFETY: both sides are within the window / buffer; unaligned because
    // byte-granular. The 8-byte read may overlap the match that follows the
    // literals, so it stays in bounds whenever 8 bytes from the anchor fit
    // the window; the write may spill up to 7 bytes past the new length, but
    // reserve(8) covers them and later pushes overwrite them.
    unsafe {
        if lits.len() <= 8 && anchor_idx + 8 <= win.len() {
            literals.reserve(8);
            let dst = literals.as_mut_ptr().add(literals.len());
            let v = lits.as_ptr().cast::<u64>().read_unaligned();
            dst.cast::<u64>().write_unaligned(v);
            literals.set_len(literals.len() + lits.len());
        } else {
            literals.extend_from_slice(lits);
        }
    }
    do_offset_history(of_value, ll, rep);
    seqs.push(pack_seq(ll, match_len as u32, of_value));
    (ll, start + match_len)
}

/// Index a match's covered range (the fast strategy's fill policy).
/// Short matches keep every position (they carry the alignment coverage on
/// structured data). Long matches only index two anchors (zstd fast's fill
/// policy): one just inside the start, one just before the end — the scan
/// loop already indexes the positions it probes, so interior coverage only
/// needs seed points for the phases the scan skips over, and hashing a
/// 4-byte grid across long matches dominated encoder time. Both anchors
/// need HASH_READ bytes of window ahead; a match reaching the insert bound
/// simply leaves them out. Outlined from [`TableEmit::emit`] (see there).
#[inline(never)]
fn insert_covered(
    win: &[u8],
    table: &mut [u32],
    win_base: u64,
    start: usize,
    match_len: usize,
    insert_max: u64,
    log: u32,
) {
    if match_len <= 16 {
        let end = (win_base + (start + match_len) as u64).min(insert_max);
        let mut p = win_base + start as u64;
        // The clamped end can fall below p — tiny windows saturate
        // insert_max to win_base, and rep-chain tails emit to MIN_MATCH of
        // the block end, not HASH_READ. Bail on the empty range: the
        // wrapped `end - p` below would peel-insert one position whose
        // hash reads past the window (fuzz-found heap overread).
        if p >= end {
            return;
        }
        // Peel the odd tail before the loop: `while p < end` alone unrolls
        // mod 2 behind a per-entry parity guard that mispredicts on every
        // other insert (measured on json.fastest).
        if (end - p) & 1 == 1 {
            insert_at(win, table, (p - win_base) as usize, p, log);
            p += 1;
        }
        while p < end {
            insert_at(win, table, (p - win_base) as usize, p, log);
            insert_at(win, table, (p + 1 - win_base) as usize, p + 1, log);
            p += 2;
        }
    } else {
        let base = win_base + start as u64;
        let hi = base + match_len as u64 - 2;
        if hi <= insert_max {
            let lo = base + 2;
            insert_at(win, table, (lo - win_base) as usize, lo, log);
            if hi > lo {
                insert_at(win, table, (hi - win_base) as usize, hi, log);
            }
        }
    }
}

/// Shared mutable state of the single-table strategies (fast and chain):
/// the head hash table, the output streams and the per-block constants,
/// bundled so the emit helpers stay inside the register argument budget
/// (their former twelve-to-fourteen-argument free signatures moved several
/// arguments through the stack on every call).
struct TableEmit<'a> {
    table: &'a mut [u32],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    win_base: u64,
    /// Last absolute position whose hash reads stay inside the window.
    insert_max: u64,
    /// Log of `table` (the fast strategy's hash log).
    hash_log: u32,
}

impl TableEmit<'_> {
    /// Emit the sequence for a match covering `match_len` bytes at window
    /// index `start`, update the repeated-offset history, index the covered
    /// range and return the new cursor (which is also the new anchor). The
    /// sequence is pushed straight into the packed code/add-bits streams the
    /// block encoder consumes — the raw (ll, ml, of) triple never gets its
    /// own buffer. The coverage indexing is outlined ([`insert_covered`]):
    /// with it inline, the whole helper grew past the inliner's budget, and
    /// every scan-loop call site paid a full spill/reload of the emit
    /// context (literals/seqs/rep) around the call.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        win: &[u8],
        anchor: u64,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        insert_covered(
            win,
            self.table,
            self.win_base,
            start,
            match_len,
            self.insert_max,
            self.hash_log,
        );
        self.win_base + match_end as u64
    }

    /// [`TableEmit::emit`] for the chain strategies: the covered range is
    /// indexed with complete hash head plus chain links (a coarse grid for
    /// long matches, so huge runs cannot dominate the hash work), keeping
    /// later chain walks connected to same-hash predecessors.
    /// `inline(always)`: at a plain `#[inline]` LLVM leaves the chain
    /// scan's one emit call outlined, and every sequence pays the fat
    /// calling convention (ten-plus args, Vec fields through `self`) —
    /// the same disease the outlined search closure had.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn emit_chain(
        &mut self,
        win: &[u8],
        chain: &mut [u32],
        hash_log: u32,
        anchor: u64,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        let chain_mask = chain.len() - 1;
        let step = (if match_len <= 64 {
            1
        } else {
            4
        }) as u64;
        let end_abs = (self.win_base + match_end as u64).min(self.insert_max);
        let mut p = self.win_base + start as u64;
        while p < end_abs {
            let i = (p - self.win_base) as usize;
            let h = hash_at_log(win, i, hash_log);
            // SAFETY: h is masked to hash_log bits, p to the chain table
            // size. The chain slot key is the ABSOLUTE position — the walk
            // side resolves candidates absolutely: a window-relative index
            // diverges from it once win_base stops being a multiple of the
            // chain size (window compaction in streaming, per-block adopted
            // windows in bulk, job-relative windows in MT), scrambling every
            // walk past its first hop.
            unsafe {
                let head = *self.table.get_unchecked(h);
                *chain.get_unchecked_mut(p as usize & chain_mask) = head;
                *self.table.get_unchecked_mut(h) = pack_pos(p);
            }
            p += step;
        }
        self.win_base + match_end as u64
    }

    /// Probe continuations at the second repeated offset immediately after a
    /// match (zstd fast's rep_offset2 loop). Alternating-period data chains
    /// rep0/rep1 matches back to back with zero literals; emitting with
    /// of_value 1 at ll == 0 swaps rep0/rep1, so the loop alternates distances
    /// on its own. Returns the cursor after the last chained match.
    /// `inline(always)` for the same budget reason as [`DfastEmit::rep_chain`].
    #[inline(always)]
    fn rep1_chain<const RAMPED: bool>(
        &mut self,
        win: &[u8],
        pos: u64,
        block_end: u64,
        rep: &mut [u32; 3],
        ramp: RampGate,
    ) -> u64 {
        let mut pos = pos;
        while block_end - pos >= MIN_MATCH as u64 {
            let Some(cand_abs) = pos.checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            if RAMPED && ramp.blocks(pos, cand_abs) {
                break;
            }
            let pidx = (pos - self.win_base) as usize;
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pidx) {
                break;
            }
            let ml = extend_match(win, pidx, cand);
            debug_assert!(ml >= MIN_MATCH);
            pos = self.emit(win, pos, pidx, ml, 1, rep);
        }
        pos
    }
}

/// Shared mutable state of the dfast strategy: the two tables, the output
/// streams and the per-block constants, bundled so the emit helpers stay
/// inside the register argument budget (their former fourteen-argument
/// signatures moved several arguments through the stack on every call).
struct DfastEmit<'a> {
    long: &'a mut [u32],
    small: &'a mut [u32],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    win_base: u64,
    /// Last window index whose hash reads stay inside the window.
    insert_max_idx: usize,
    long_log: u32,
    small_log: u32,
}

impl DfastEmit<'_> {
    /// Insert `idx` into both tables. Caller guarantees `idx + HASH_READ`
    /// bytes of window.
    #[inline(always)]
    fn insert_both(&mut self, win: &[u8], idx: usize) {
        // SAFETY: both hashes are masked to their tables' sizes.
        unsafe {
            let entry = pack_pos(self.win_base + idx as u64);
            *self
                .long
                .get_unchecked_mut(hash8_at_log(win, idx, self.long_log)) = entry;
            *self
                .small
                .get_unchecked_mut(hash_at_log(win, idx, self.small_log)) = entry;
        }
    }

    /// Emit one sequence (literals from `anchor_idx`, match at `start`) and
    /// seed libzstd's complementary anchors instead of indexing every
    /// covered position: both tables take the position two past the scan
    /// position the match was found at (`curr_idx + 2`), the long table the
    /// match end minus two, the small table the match end minus one.
    /// Anchors past `insert_max_idx` are dropped; the scan tail never
    /// probes them. Returns the new anchor as a window index.
    ///
    /// `inline(always)` for the same budget reason as [`DfastEmit::rep_chain`]:
    /// at a plain `#[inline]` LLVM leaves the steady-phase sites outlined,
    /// and the eight-argument call (three stack-passed args, rep through
    /// memory, Vec fields via `self`) taxed every sequence with ~30% of
    /// the emit body (dll32 callgrind: 170 Ir/seq outlined, 26% of the
    /// whole fast encode; inlining: json.fast Ir −8.2%, dll100 wall +7%).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        win: &[u8],
        anchor_idx: usize,
        curr_idx: usize,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> usize {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            self.win_base + anchor_idx as u64,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        if curr_idx + 2 <= self.insert_max_idx {
            self.insert_both(win, curr_idx + 2);
        }
        if match_end >= 2 && match_end - 2 <= self.insert_max_idx {
            // SAFETY: masked to the long table size.
            unsafe {
                *self
                    .long
                    .get_unchecked_mut(hash8_at_log(win, match_end - 2, self.long_log)) =
                    pack_pos(self.win_base + (match_end - 2) as u64);
            }
        }
        if match_end >= 1 && match_end - 1 <= self.insert_max_idx {
            // SAFETY: masked to the small table size.
            unsafe {
                *self
                    .small
                    .get_unchecked_mut(hash_at_log(win, match_end - 1, self.small_log)) =
                    pack_pos(self.win_base + (match_end - 1) as u64);
            }
        }
        match_end
    }

    /// [`rep1_chain`] for the dfast strategy: each repcode position goes
    /// into both tables (no complementary anchors — libzstd's offset_2 loop
    /// only re-seeds the position it consumes). Returns the cursor as a
    /// window index, which is also the new anchor.
    /// `inline(always)`: the phase-split loop instantiates its call sites
    /// twice, which halves the inliner's budget per copy — left to its own
    /// `#[inline]` judgment the helper flips to an outlined call in the
    /// steady phase and every dense-match emit pays the argument setup.
    #[inline(always)]
    fn rep_chain<const RAMPED: bool>(
        &mut self,
        win: &[u8],
        pos_idx: usize,
        ilimit_idx: usize,
        rep: &mut [u32; 3],
        ramp: RampGate,
    ) -> usize {
        let mut pos = pos_idx;
        while pos <= ilimit_idx {
            let Some(cand_abs) = (self.win_base + pos as u64).checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            if RAMPED && ramp.blocks(self.win_base + pos as u64, cand_abs) {
                break;
            }
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pos) {
                break;
            }
            let ml = extend_match(win, pos, cand);
            debug_assert!(ml >= MIN_MATCH);
            self.insert_both(win, pos);
            let (_ll, match_end) = push_seq_packed(
                win,
                self.win_base,
                self.win_base + pos as u64,
                pos,
                ml,
                1,
                rep,
                self.literals,
                self.seqs,
            );
            pos = match_end;
        }
        pos
    }
}

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
    /// Head hash table for the fast/dfast/chain strategies; u32 entries
    /// (see [`pack_pos`]).
    table: Vec<u32>,
    /// Second search table for the two-table strategies (the dfast short
    /// hash or the chain links; see [`Strategy`]), u32 entries like
    /// `table`; empty for the fast strategy.
    chain: Vec<u32>,
    /// Head hash table for the opt strategies, epoch-tagged u64 entries.
    opt_table: Vec<u64>,
    /// The opt strategies' binary-tree ring: two u64 link slots per ring
    /// position; empty outside opt.
    bt: Vec<u64>,
    /// Single-probe 3-byte table for the opt strategies with `min_match == 3`
    /// (libzstd's hashTable3); empty otherwise.
    hash3: Vec<u64>,
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
    epoch: u64,
    miss_count: usize,
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
    /// Gear-hash long-distance matcher state for chain rows with
    /// [`LevelParams::ldm`] (see [`super::ldm`]); `None` elsewhere.
    ldm: Option<LdmState>,
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
    /// Armed deep-offset ramp for the current job ([`RampGate`]); OFF on
    /// every single-job path.
    ramp: RampGate,
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

    /// Whether the shape-adaptive reach probe (see [`super::reach_probe`])
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
            table: alloc::vec![0u32; 1usize << HASH_LOG],
            chain: Vec::new(),
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
            epoch: 1,
            miss_count: 0,
            params: LEVEL_PARAMS[1],
            shape: InputShape::default(),
            reach_choice: ReachChoice::Keep,
            ldm_arming: LdmArming::Frame,
            dubt_head: HeadPhase::Off,
            ldm: None,
            ldm_seqs: Vec::new(),
            ldm_quiet: 0,
            ldm_dead: false,
            ldm_canary: 0,
            ldm_checked: false,
            strip_parse: false,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size,
            ramp: RampGate::OFF,
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
            table: alloc::vec![0u32; 1usize << HASH_LOG],
            chain: Vec::new(),
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
            epoch: 1,
            miss_count: 0,
            params: LEVEL_PARAMS[1],
            shape: InputShape::default(),
            reach_choice: ReachChoice::Keep,
            ldm_arming: LdmArming::Frame,
            dubt_head: HeadPhase::Off,
            ldm: None,
            ldm_seqs: Vec::new(),
            ldm_quiet: 0,
            ldm_dead: false,
            ldm_canary: 0,
            ldm_checked: false,
            strip_parse: false,
            rep: [1, 4, 8],
            rep_pending: 0,
            lit_lens: DEFAULT_LIT_LENS,
            seed_offset: 0,
            seed_hits: 0,
            seed_budget: 0,
            slice_size: 0,
            ramp: RampGate::OFF,
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
        if params != self.params {
            // Exactly one table family is live per strategy; switching
            // families drops the other's buffers.
            match params.strategy {
                Strategy::Fast => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = Vec::new();
                },
                Strategy::Dfast(small_log) => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = alloc::vec![0u32; 1usize << small_log];
                },
                Strategy::Chain(chain_log) => {
                    self.table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.chain = alloc::vec![0u32; 1usize << chain_log];
                },
                Strategy::Opt(knobs) => {
                    self.opt_table = alloc::vec![EMPTY; 1usize << params.hash_log];
                    // The tree ring: two link slots per ring position.
                    self.bt = alloc::vec![EMPTY; 2usize << knobs.bt_log];
                    self.hash3 = if knobs.hash3_log > 0 {
                        alloc::vec![EMPTY; 1usize << knobs.hash3_log]
                    } else {
                        Vec::new()
                    };
                    self.table = Vec::new();
                    self.chain = Vec::new();
                },
                Strategy::BtLazy(knobs) => {
                    self.dubt_table = alloc::vec![0u32; 1usize << params.hash_log];
                    self.dubt_bt = alloc::vec![0u32; 2usize << knobs.bt_log];
                    self.opt_table = Vec::new();
                    self.bt = Vec::new();
                    self.hash3 = Vec::new();
                    self.table = Vec::new();
                    self.chain = Vec::new();
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
            self.params = params;
        }
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
                Strategy::Chain(_) | Strategy::Opt(_) | Strategy::BtLazy(_)
            )
            && ldm_min_window(self.ldm_arming).is_some_and(|bar| params.window >= bar);
        let ldm_sized = self
            .ldm
            .as_ref()
            .is_some_and(|l| l.window() == params.window as u64);
        self.ldm = match (ldm_wanted, ldm_sized) {
            (true, true) => self.ldm.take(),
            (true, false) => Some(LdmState::new(
                (params.window as u64).ilog2(),
                params.window as u64,
            )),
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
    /// dropped. The prefill indexes the surviving content exactly like a
    /// multithreaded job strip (grid fill, seed detection included), so
    /// matches into the dictionary cost nothing extra at scan time.
    pub fn load_dictionary(&mut self, content: &[u8], rep: [u32; 3]) {
        debug_assert!(self.ext.is_none() && self.win.is_empty() && self.pos == 0);
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
        // The owned window is filled above; prefill over the caller's slice
        // avoids the self-borrow (identical bytes).
        self.prefill_window(content, 0);
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
    /// (see [`RampGate`]); `depth == 0` disarms it. Job zero needs no ramp:
    /// no output exists below the frame start, so cross-boundary reads are
    /// impossible there.
    pub fn arm_ramp(&mut self, job_start: u64, depth: u64) {
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
    /// and candidates arise only from the cleared head table or from link
    /// values written this job — stale link slots are unreachable. The
    /// dfast `chain` buffer is a second probed head table and is cleared.
    /// The opt tables need no clear (their entries carry the epoch, bumped
    /// per job).
    pub fn prefill_window(&mut self, data: &[u8], base: u64) {
        // The head applies only to a genuinely cold start: a non-empty
        // strip (mt jobs with history, dictionary content) is warm, while
        // the empty strip is mt job zero — a frame start, so the head
        // arms exactly as at reset (pooled states arrive here directly).
        self.dubt_head = if data.is_empty() && self.head_eligible() {
            HeadPhase::Armed
        } else {
            HeadPhase::Off
        };
        clear_table(&mut self.table);
        // The DUBT finder's entries carry no epoch tag, so a job restarts
        // its tree from scratch regardless of strip length: cleared heads
        // make every descent start at a strip chain node, and the strip
        // fill rewrites each position's ring slots. bt slots below the
        // strip are never read — their owners can no longer resolve as
        // candidates.
        if matches!(self.params.strategy, Strategy::BtLazy(_)) {
            self.dubt_table.fill(0);
        }
        if !matches!(self.params.strategy, Strategy::Chain(_)) {
            self.chain.fill(0);
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
        if let Some(ldm) = &mut self.ldm {
            ldm.restart(base);
            self.ldm_quiet = 0;
            self.ldm_dead = false;
            self.ldm_canary = 0;
            let gated = matches!(self.params.strategy, Strategy::Opt(_) | Strategy::BtLazy(_))
                && sampled_distinct(data, 0, data.len()) < LDM_SYMS_MIN;
            if data.len() >= super::ldm::MIN_MATCH_LENGTH && !gated {
                ldm.fill(data, base, base, base + data.len() as u64);
            }
        }
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
                // twin is unreachable (see PREFILL_STRIDE). Only the strip's
                // retention tail is inserted (PREFILL_RETAIN_HORIZONS); the
                // seed scan below still sees the whole strip, so period-long
                // repeats keep their reach.
                let table = &mut self.table[..];
                let log = self.params.hash_log;
                let retain = table.len() * PREFILL_STRIDE * PREFILL_RETAIN_HORIZONS;
                let mut idx = last.saturating_sub(retain);
                while idx < last {
                    insert_at(data, table, idx, base + idx as u64, log);
                    idx += PREFILL_STRIDE;
                }
                self.acquire_seed(data, last);
            },
            Strategy::Dfast(small_log) => {
                let long_log = self.params.hash_log;
                let long = &mut self.table[..];
                let small = &mut self.chain[..];
                let mut idx = 0;
                while idx < last {
                    // SAFETY: both hashes are masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(base + idx as u64);
                        *long.get_unchecked_mut(hash8_at_log(data, idx, long_log)) = entry;
                        *small.get_unchecked_mut(hash_at_log(data, idx, small_log)) = entry;
                    }
                    idx += PREFILL_STRIDE;
                }
                // The double table is as burial-prone as the fast one for
                // period-long twins: both probes are single-candidate.
                self.acquire_seed(data, last);
            },
            Strategy::Chain(_) => {
                let hash_log = self.params.hash_log;
                let chain_mask = self.chain.len() - 1;
                let table = &mut self.table[..];
                let chain = &mut self.chain[..];
                let mut idx = 0;
                while idx < last {
                    let abs = base + idx as u64;
                    // SAFETY: the hash masks to hash_log bits, the absolute
                    // position to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let h = hash_at_log(data, idx, hash_log);
                        let head = *table.get_unchecked(h);
                        *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                        *table.get_unchecked_mut(h) = pack_pos(abs);
                    }
                    idx += PREFILL_STRIDE;
                }
                // The head table's first hop is as burial-prone as the fast
                // strategy's single probe; seed the walk-independent path.
                self.acquire_seed(data, last);
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
        let a8 = read8(data, last);
        if let Some(u) = seed_scan(data, last, a8) {
            self.seed_offset = (last - u) as u32;
            self.seed_hits = 0;
            self.seed_budget = SEED_BUDGET;
        }
    }
}

/// Zero a job-start table. Large clears stream megabytes per job through
/// the bus; non-temporal stores skip the ownership read and keep the zero
/// lines from evicting the scan's working set, while small tables stay
/// cache-warm for the probes that follow the clear.
fn clear_table(t: &mut [u32]) {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    if t.len() >= NT_CLEAR_MIN && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected; the stores stay inside t
        // and the fence retires them before any read.
        unsafe { clear_table_avx512(t) };
        return;
    }
    t.fill(0);
}

/// Non-temporal table clear: aligned 64-byte streaming stores plus one
/// fence. Unaligned head and tail run as plain stores.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512f")]
unsafe fn clear_table_avx512(t: &mut [u32]) {
    unsafe {
        use core::arch::x86_64::*;
        let zero = _mm512_setzero_si512();
        let mut p = t.as_mut_ptr();
        let end = p.add(t.len());
        // SAFETY: p advances only while strictly below end; the alignment head
        // and element tail each touch disjoint in-bounds ranges.
        while (p as usize) & 63 != 0 && p < end {
            *p = 0;
            p = p.add(1);
        }
        let aligned_end = (end as usize - ((end as usize) & 63)) as *mut u32;
        while p < aligned_end {
            _mm512_stream_si512(p.cast(), zero);
            p = p.add(16);
        }
        while p < end {
            *p = 0;
            p = p.add(1);
        }
        _mm_sfence();
    }
}

/// Slot count above which the job-start table clear goes non-temporal
/// (4 MiB of u32 slots): smaller tables profit more from staying cached.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
const NT_CLEAR_MIN: usize = 1 << 20;

/// Nearest `u < last` with the anchor's 8 bytes and [`SEED_AGREE`]
/// agreeing bytes before it (see [`MatchGeneratorDriver::acquire_seed`]).
/// The 8-byte compare subsumes the 4-byte prefilter the scalar walk used
/// to run first: a matching u64's low half is the u32 at the same index.
fn seed_scan(data: &[u8], last: usize, a8: u64) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    if last >= SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected; every load stays inside
        // data (see the bound derivation in the callee).
        return unsafe { seed_scan_avx512(data, last, a8) };
    }
    let mut u = last;
    while u > 0 {
        u -= 1;
        if read8(data, u) == a8 && seed_agrees(data, u, last) && seed_confirms(data, u, last) {
            return Some(u);
        }
    }
    None
}

/// Whether the [`SEED_AGREE`] bytes before `u` equal those before `last`:
/// positions below [`SEED_AGREE`] never qualify (the `u >= k` bound).
fn seed_agrees(data: &[u8], u: usize, last: usize) -> bool {
    let mut k = 1;
    while u >= k && k <= SEED_AGREE && data[u - k] == data[last - k] {
        k += 1;
    }
    k > SEED_AGREE
}

/// Whether the [`SEED_CONFIRMS`] anchors behind `u` still agree with those
/// behind `last` (see the const's doc): the local-repeat filter that keeps
/// the nearest-first seed honest. `u < last`, so `u >= t` bounds both reads.
#[inline(always)]
fn seed_confirms(data: &[u8], u: usize, last: usize) -> bool {
    for t in SEED_CONFIRMS {
        if u >= t && read8(data, u - t) != read8(data, last - t) {
            return false;
        }
    }
    true
}

/// AVX-512 seed scan: 64-candidate blocks from the anchor down, eight
/// overlapping unaligned 64-byte loads per block. Load `j` compares the
/// broadcast anchor against the positions `b + j + 8t`, so the eight 8-bit
/// masks assemble into one occupancy bit per candidate; blocks run top-down
/// and set bits are taken highest first — exactly the scalar walk's
/// nearest-first order, byte-for-byte. The top block starts at `last - 63`
/// (its highest lane is the anchor itself, skipped by the `u < last`
/// check); every load's final byte lands at `b + 7 + 64 <= last + 8 ==
/// data.len()`, which [`SEED_SCAN_MIN`] guarantees for the lowest block
/// too. The positions below the final block's start run scalar.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512f")]
unsafe fn seed_scan_avx512(data: &[u8], last: usize, a8: u64) -> Option<usize> {
    unsafe {
        use core::arch::x86_64::*;
        let pat = _mm512_set1_epi64(a8 as i64);
        let bottom = (last - 63) & 63;
        let mut b = last - 63;
        loop {
            let mut occ = 0u64;
            for j in 0..8 {
                // SAFETY: b + j + 64 <= data.len() for every block (top block:
                // last - 63 + 7 + 64 == last + 8; lower blocks read lower).
                let v = _mm512_loadu_si512(data.as_ptr().add(b + j).cast());
                // Load j's lane t compares the position b + j + 8t; spread its
                // mask bit t to occupancy bit j + 8t (== u - b). The per-bit
                // loop only runs on nonzero masks — pure overhead on
                // non-repeating data.
                let mut m = _mm512_cmpeq_epi64_mask(v, pat) as u64;
                while m != 0 {
                    let t = m.trailing_zeros();
                    m &= m - 1;
                    occ |= 1 << (j + 8 * t as usize);
                }
            }
            while occ != 0 {
                let bit = occ.ilog2();
                occ ^= 1 << bit;
                let u = b + bit as usize;
                if u < last && seed_agrees(data, u, last) && seed_confirms(data, u, last) {
                    return Some(u);
                }
            }
            if b == bottom {
                break;
            }
            b -= 64;
        }
        (0..bottom).rev().find(|&u| {
            read8(data, u) == a8 && seed_agrees(data, u, last) && seed_confirms(data, u, last)
        })
    }
}

impl Matcher for MatchGeneratorDriver {
    fn set_input_shape(&mut self, shape: InputShape) {
        self.shape = shape;
    }

    /// Decide the frame's reach from its first bytes (see
    /// [`super::reach_probe`]). Called before any block of the frame is
    /// matched; no-op for heads below the probe span.
    fn consider_reach_probe(&mut self, head: &[u8], level: Level) {
        let choice = super::reach_probe::probe_reach_choice(head, level, self.shape);
        if choice != self.reach_choice {
            self.reach_choice = choice;
            // No block has been matched yet, so re-deriving the params is
            // the whole state change; the tables' sizes are
            // reach-independent.
            self.apply_level(level);
        }
    }

    /// See the inherent [`MatchGeneratorDriver::load_dictionary`] — the
    /// trait view.
    fn load_dictionary(&mut self, content: &[u8], rep: [u32; 3]) {
        self.load_dictionary(content, rep)
    }

    fn reset(&mut self, level: Level) {
        self.apply_level(level);
        self.ext = None;
        self.win.clear();
        self.win_base = 0;
        self.pos = 0;
        self.block_end = 0;
        self.anchor = 0;
        self.block_start = 0;
        // Stale opt-table entries from previous frames fail the epoch check;
        // the u32 tables need no reset (entries decode against the scanning
        // position and die on the window-range check). The DUBT finder has
        // no tag: its rows clear both tables, since frame positions restart
        // at zero and old absolute positions would alias the new window.
        self.epoch += 1;
        if self.epoch > 0xffff {
            self.opt_table.fill(EMPTY);
            self.bt.fill(EMPTY);
            self.hash3.fill(EMPTY);
            self.epoch = 1;
        }
        if matches!(self.params.strategy, Strategy::BtLazy(_)) {
            self.dubt_table.fill(0);
            self.dubt_bt.fill(0);
        }
        self.miss_count = 0;
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
        self.ldm_seqs.clear();
        self.ldm_quiet = 0;
        self.ldm_dead = false;
        self.ldm_canary = 0;
        self.ldm_checked = false;
        self.strip_parse = false;
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
            self.start_matching_btlazy(HEAD_KNOBS, literals, seqs);
            self.finish_head();
            return;
        }
        if !matches!(self.params.strategy, Strategy::Opt(_) | Strategy::BtLazy(_)) {
            self.catch_up_insertions();
        }
        match self.params.strategy {
            Strategy::Fast => {
                if self.ramp.is_armed() {
                    self.start_matching_fast::<true>(literals, seqs);
                } else {
                    self.start_matching_fast::<false>(literals, seqs);
                }
            },
            Strategy::Dfast(_) => {
                // Key on the tables' actual lengths (what the scan body
                // derives): the two full rows cover every input the level
                // clamp leaves at full tables; smaller inputs (clamped
                // windows, shrunken logs) take the runtime-log
                // instantiation.
                let long_log = self.table.len().trailing_zeros();
                let small_log = self.chain.len().trailing_zeros();
                match (self.ramp.is_armed(), long_log, small_log) {
                    (false, 17, 16) => self.start_matching_dfast::<false, 17, 16>(literals, seqs),
                    (true, 17, 16) => self.start_matching_dfast::<true, 17, 16>(literals, seqs),
                    (false, 18, 18) => self.start_matching_dfast::<false, 18, 18>(literals, seqs),
                    (true, 18, 18) => self.start_matching_dfast::<true, 18, 18>(literals, seqs),
                    (armed, ..) => {
                        if armed {
                            self.start_matching_dfast::<true, RUNTIME_LOG, RUNTIME_LOG>(
                                literals, seqs,
                            )
                        } else {
                            self.start_matching_dfast::<false, RUNTIME_LOG, RUNTIME_LOG>(
                                literals, seqs,
                            )
                        }
                    },
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
            Strategy::Opt(knobs) => {
                self.ldm_alphabet_gate();
                self.ldm_generate();
                let won = self.start_matching_opt(knobs, literals, seqs);
                self.ldm_note_block(won);
            },
            Strategy::BtLazy(knobs) => {
                self.ldm_alphabet_gate();
                self.ldm_generate();
                let won = self.start_matching_btlazy(knobs, literals, seqs);
                self.ldm_note_block(won);
            },
        }
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
                    let h = hash_at_log(win, idx, self.params.hash_log);
                    // SAFETY: h is masked to hash_log bits, the absolute block
                    // start to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let head = *self.table.get_unchecked(h);
                        let chain_mask = self.chain.len() - 1;
                        *self
                            .chain
                            .get_unchecked_mut(self.block_start as usize & chain_mask) = head;
                        *self.table.get_unchecked_mut(h) = pack_pos(self.block_start);
                    }
                },
                Strategy::Dfast(small_log) => {
                    let hl = hash8_at_log(win, idx, self.params.hash_log);
                    let hs = hash_at_log(win, idx, small_log);
                    // SAFETY: both hashes masked to their tables' sizes.
                    unsafe {
                        let entry = pack_pos(self.block_start);
                        *self.table.get_unchecked_mut(hl) = entry;
                        *self.chain.get_unchecked_mut(hs) = entry;
                    }
                },
                Strategy::Fast => {
                    insert_at(
                        win,
                        &mut self.table,
                        idx,
                        self.block_start,
                        self.params.hash_log,
                    );
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
        // Skipped blocks stop filling except for the chain row's full-window
        // population, whose bytes are frozen. The opt rows have no frozen
        // population, so they exempt Skipped blocks at every window: the
        // latch cannot fire through them either way (no scan runs), and
        // max-entropy or uniform windows carry no far class.
        if why == LdmFill::Skipped
            && !(matches!(self.params.strategy, Strategy::Chain(_))
                && self.params.window >= LDM_FULL_WINDOW)
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
    /// split-pass tax at the head span already filled. The chain row's
    /// full-window population is the one exception (its bytes are frozen,
    /// so the gate stays skipped there); the opt rows gate at every window.
    fn ldm_alphabet_gate(&mut self) {
        let chain_frozen = matches!(self.params.strategy, Strategy::Chain(_))
            && self.params.window >= LDM_FULL_WINDOW;
        if self.ldm_checked || self.ldm.is_none() || chain_frozen {
            return;
        }
        self.ldm_checked = true;
        let win = window_slice(&self.win, self.ext.as_ref());
        let start = (self.block_start - self.win_base) as usize;
        let end = (self.block_end - self.win_base) as usize;
        if sampled_distinct(win, start, end) < LDM_SYMS_MIN {
            self.ldm = None;
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
    /// tables end in the state a full scan would have left; positions older
    /// than the match window cannot resolve and are skipped (the same clamp
    /// as the opt tree's fill).
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
        let to = (block_start - win_base) as usize;
        let win = window_slice(&self.win, self.ext.as_ref());
        let mut idx = (from - win_base) as usize;
        match self.params.strategy {
            Strategy::Fast => {
                let log = self.params.hash_log;
                while idx < to {
                    insert_at(win, &mut self.table, idx, win_base + idx as u64, log);
                    idx += 1;
                }
            },
            Strategy::Dfast(small_log) => {
                let long_log = self.params.hash_log;
                let long = &mut self.table[..];
                let small = &mut self.chain[..];
                while idx < to {
                    // SAFETY: both hashes are masked to their tables' sizes
                    // (same pair as the dfast prefill).
                    unsafe {
                        let entry = pack_pos(win_base + idx as u64);
                        *long.get_unchecked_mut(hash8_at_log(win, idx, long_log)) = entry;
                        *small.get_unchecked_mut(hash_at_log(win, idx, small_log)) = entry;
                    }
                    idx += 1;
                }
            },
            Strategy::Chain(_) => {
                let hash_log = self.params.hash_log;
                let chain_mask = self.chain.len() - 1;
                let table = &mut self.table[..];
                let chain = &mut self.chain[..];
                while idx < to {
                    let abs = win_base + idx as u64;
                    // SAFETY: the hash masks to hash_log bits, the absolute
                    // position to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let h = hash_at_log(win, idx, hash_log);
                        let head = *table.get_unchecked(h);
                        *chain.get_unchecked_mut(abs as usize & chain_mask) = head;
                        *table.get_unchecked_mut(h) = pack_pos(abs);
                    }
                    idx += 1;
                }
            },
            // The tree strategies fill their tree lazily from `next_update`.
            Strategy::Opt(_) | Strategy::BtLazy(_) => {},
        }
        self.gap_start = u64::MAX;
    }

    /// The single-probe `fast` strategy loop (level [`Level::Fastest`]).
    fn start_matching_fast<const RAMPED: bool>(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        // Hot state lives in locals for the whole loop: the emit helpers
        // used to take `&mut self`, which forced a reload of every cursor
        // from memory after each match.
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_end = self.block_end;
        // saturating: tiny first blocks never reach an emit, so the bound is
        // never consulted when win.len() < HASH_READ.
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer: routing
        // them through the emit context's slice field kept the pointer
        // stack-resident (a re-load per access; see the dfast loop's note on
        // the same SROA failure).
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = self.table.as_mut_ptr();
        let hash_log = self.params.hash_log;
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            win_base,
            insert_max,
            hash_log,
        };
        let max_window = self.params.window as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut miss_count = self.miss_count;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let hash_read = HASH_READ as u64;

        // Resolve a table entry to a window index. The truncated absolute
        // position is rebuilt as a *distance* from the scanning position:
        // `dist = pos - (entry - 1)` wraps to a huge u64 for both the empty
        // sentinel (dist = pos + 1 > reach) and candidates from the previous
        // 4 GiB cycle, so the window-range check `dist <= reach` (reach =
        // min(pos - win_base, max_window), i.e. `pos - lo`) rejects all
        // three failure modes in one compare. Invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip` then rejects them with one
        // predictable branch instead of a three-way check per probe
        // (libzstd's selectAddr trick).
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // dist = pos - entry + 1 without a debug panic on entry == 0
            // (the empty sentinel wraps to dist = pos + 1 > reach).
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Two-position pipeline (libzstd's ip0/ip1 interleave): the hash and
        // table entry of the next position are prepared before the current
        // one is probed, so the hash multiply and table-load latencies of
        // both positions overlap instead of chaining behind every probe. A
        // pair advances by the miss step once both positions miss; the
        // second position is dropped at the block tail.
        // The miss step can jump past the block end, so the guard must
        // saturate instead of relying on pos < block_end.
        //
        // The body exists in two phases. The seeded phase carries the
        // job-start state (`seed_offset`/`seed_hits`/`seed_budget`,
        // `rep_pending`), which only ever winds down; once it converges the
        // steady phase runs a body with all of it folded out. In one loop
        // those four values stayed live across every iteration (bulk-ST and
        // post-convergence MT scanning carried pure dead weight), and the
        // scan paid for them in spill traffic — a quarter of its cycles sat
        // on stack reloads. `$gated` is a literal, so the phase-only code
        // paths are constant-folded away in the steady instantiation.
        //
        // A steady/tail loop split that folds `pair_len` to the constant 2
        // (full pairs while a pair fits, then a single-position epilogue
        // instantiation) costs 3% fewer instructions but regressed
        // text.fastest wall by 7% — the third macro instantiation displaces
        // the hot blocks; do not retry without a code-layout story.
        let ramp = self.ramp;
        macro_rules! scan_fast {
            ($restart:lifetime, $gated:literal) => {
                let idx0 = (pos - win_base) as usize;
                // One u64 load per position feeds the hash, the 4-byte
                // probe prefilter (its low half) and the repcode compares.
                let v0 = read8(win, idx0);
                let h0 = hash5_log(v0, hash_log);
                // SAFETY: the hash masks to hash_log bits and the table
                // always holds 1 << hash_log slots (see insert_at).
                let prev0 = unsafe { *table_ptr.add(h0) };
                let cur0 = v0 as u32;
                let mut pair_len = 1u64;
                let mut idx1 = idx0;
                let mut h1 = h0;
                let mut prev1 = prev0;
                let mut cur1 = cur0;
                if block_end - pos > hash_read {
                    pair_len = 2;
                    idx1 = idx0 + 1;
                    let v1 = read8(win, idx1);
                    h1 = hash5_log(v1, hash_log);
                    // SAFETY: as above.
                    prev1 = unsafe { *table_ptr.add(h1) };
                    cur1 = v1 as u32;
                }
                // Store after both lookups so each probe sees the pre-store
                // entry (newest-wins).
                // SAFETY: as above.
                unsafe {
                    *table_ptr.add(h0) = pack_pos(pos);
                }
                if pair_len == 2 {
                    // SAFETY: as above.
                    unsafe {
                        *table_ptr.add(h1) = pack_pos(pos + 1);
                    }
                }

                // Probe the first position. Repcode candidate first (mirrors
                // zstd's fast strategy: rep[0] only); a repcode match needs
                // at least one pending literal so of_value 1 stays encodable: with
                // literals pending the probe runs at the current position,
                // otherwise one byte ahead so that byte becomes the literal.
                // The probe bound is window-relative (`pidx >= rep[0]` is
                // `probe >= win_base + rep[0]` with win_base folded out).
                let mut rep1_armed = false;
                {
                    let anchor_idx = (anchor - win_base) as usize;
                    // A gated job start must not probe repcodes (unknown
                    // decoder history); probe 0 sits below the bound, which
                    // folds the underflow guard into the same single
                    // comparison.
                    let pidx = if $gated && rep_pending != 0 {
                        0
                    } else if idx0 == anchor_idx {
                        idx0 + 1
                    } else {
                        idx0
                    };
                    // OR-folded repcode prefilter for the pair: both
                    // positions' compares collapse into one unpredictable
                    // branch (adjacent-position rep hits are correlated, so
                    // the fold predicts no worse than either compare alone).
                    // `(a == 0) | (b == 0)`, not `||`: the short-circuit
                    // form compiles to three unpredictable branches (the
                    // two compares plus the combine — ~19% of json.fastest's
                    // mispredicts); the bitwise OR keeps the one intended
                    // branch, with the `a`-disambiguation on the taken path.
                    // A taken fold whose first position misses arms the
                    // second position's full probe — its compare is then
                    // known good. Probe and emission order are unchanged,
                    // so the output is byte-identical to the two-compare
                    // form. `cur1` reuses the already-loaded second position
                    // (read4(idx1) is its low half); 1 = never hit, covering
                    // both the tail pair (idx1 falls back to idx0) and the
                    // second position sitting below the window bound.
                    if pidx >= rep[0] as usize {
                        let r = rep[0] as usize;
                        let a = read4(win, pidx - r) ^ read4(win, pidx);
                        let b = if pair_len == 2 && idx1 >= r {
                            read4(win, idx1 - r) ^ cur1
                        } else {
                            1
                        };
                        if (a == 0) | (b == 0) {
                            if a == 0 {
                                let mut cand = pidx - r;
                                let mut ml = extend_match(win, pidx, cand);
                                if ml >= MIN_MATCH
                                    && !ramp_blocks::<RAMPED>(ramp, win_base + pidx as u64, win_base + cand as u64)
                                {
                                    let mut start = pidx;
                                    let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                                    // Extend backwards into the pending literals;
                                    // the offset (pidx - cand) stays constant.
                                    while start > anchor_idx + 1
                                        && cand > cfl
                                        && win[cand - 1] == win[start - 1]
                                    {
                                        cand -= 1;
                                        start -= 1;
                                        ml += 1;
                                    }
                                    anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                                    pos = emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp);
                                    // The chain's matches advance the cursor too.
                                    anchor = pos;
                                    miss_count = 0;
                                    continue $restart;
                                }
                            } else {
                                rep1_armed = true;
                            }
                        }
                    }

                    // Seed-offset probe (job starts only): direct byte compare
                    // at the prefill-detected long-repeat offset — the table
                    // cannot serve this candidate (see `seed_offset`). Emits as
                    // a plain literal-offset match, so it is legal under the
                    // repcode gate; each emit rotates `rep` toward holding the
                    // offset, and after SEED_MATCHES the repcode probes ride it
                    // without further seed help.
                    if $gated && seed_offset != 0 {
                    let ci = (pos - seed_offset as u64 - win_base) as usize;
                    if read4(win, ci) == cur0 {
                        let mut ml = extend_match(win, idx0, ci);
                        // Same bar as the hash path: below 6 the sequence
                        // overhead eats the match, and the offset-gain gate
                        // keeps a far seed honest about its worth.
                        if ml >= 6
                            && pays_for_offset(ml, idx0, ci, false)
                            && !ramp_blocks::<RAMPED>(ramp, pos, win_base + ci as u64)
                        {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx0;
                            let mut ci = ci;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, ci, win_base);
                            // Extend backwards into the pending literals;
                            // the offset stays constant.
                            while start > anchor_idx && ci > cfl && win[ci - 1] == win[start - 1] {
                                ci -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - ci + 3) as u32;
                            anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            pos = if rep_pending == 0 {
                                emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp)
                            } else {
                                anchor
                            };
                            anchor = pos;
                            miss_count = 0;
                            continue $restart;
                        }
                    }
                    // A seed that never pays retires on its budget instead
                    // of comparing dead bytes for the whole job.
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                // Both positions' hash candidates are resolved and compared
                // up front so the two data-dependent compare outcomes share
                // ONE unpredictable branch (the rep OR-fold's rationale:
                // adjacent-position outcomes are correlated). The pre-reads
                // are loads only — the table writes happen inside emit, after
                // every probe here — and probe/emission order is untouched,
                // so the output is byte-identical to the two-compare form.
                // An `m` is 0 iff the candidate is not the scanning position
                // itself and its first 4 bytes agree; the tail pair
                // (pair_len 1) aliases the first position's values, so its
                // `m1` can only re-fire the already-tried `m0` probe. The
                // fold consumes only the two register-resident `m`s —
                // folding `rep1_armed` into the same condition kept its
                // spilled byte load on the hot branch input (a measured
                // json-64K regression), so the armed-but-no-hash-hit case
                // enters through its own rarely-taken branch instead.
                let reach = (pos - win_base).min(max_window);
                let mut cand0 = resolve(prev0, pos, reach, idx0);
                let m0 = (cand0 == idx0) as u32 | (read4(win, cand0) ^ cur0);
                let pos1 = win_base + idx1 as u64;
                let reach1 = (pos1 - win_base).min(max_window);
                let mut cand1 = resolve(prev1, pos1, reach1, idx1);
                let m1 = (cand1 == idx1) as u32 | (read4(win, cand1) ^ cur1);
                'probes: {
                if (m0 == 0) | (m1 == 0) {
                    if m0 == 0 {
                        let mut ml = extend_match(win, idx0, cand0);
                        // A hash match already spans 5 bytes; below 6 the
                        // sequence overhead roughly equals the literals
                        // it covers, and rejecting it lets the scan try
                        // the next position where a longer match may
                        // start.
                        if ml >= 6 && !ramp_blocks::<RAMPED>(ramp, pos, win_base + cand0 as u64) {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx0;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, cand0, win_base);
                            // Extend backwards into the pending literals;
                            // the offset (idx0 - cand) stays constant.
                            while start > anchor_idx
                                && cand0 > cfl
                                && win[cand0 - 1] == win[start - 1]
                            {
                                cand0 -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - cand0 + 3) as u32;
                            anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                            // A literal offset shifts the decoder's history
                            // one slot down; after the third one a job-start
                            // gate has fully converged and repcode use is
                            // safe again.
                            if $gated {
                                rep_pending = rep_pending.saturating_sub(1);
                                pos = if rep_pending == 0 {
                                    emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp)
                                } else {
                                    anchor
                                };
                            } else {
                                pos =
                                    emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp);
                            }
                            anchor = pos;
                            miss_count = 0;
                            continue $restart;
                        }
                    }
                } else if !rep1_armed {
                    break 'probes;
                }

                // Probe the second position through the entry prepared
                // above. `pos1 == anchor` is impossible (anchor <= pos <
                // pos + 1), so the pending-literal select folds away
                // here. The repcode probe is armed by the folded
                // prefilter above (which also applies the window bound
                // and the job-start gate), so it runs compare-free.
                if rep1_armed {
                    let mut cand = idx1 - rep[0] as usize;
                    let mut ml = extend_match(win, idx1, cand);
                    if ml >= MIN_MATCH
                        && !ramp_blocks::<RAMPED>(ramp, win_base + idx1 as u64, win_base + cand as u64)
                    {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                        while start > anchor_idx + 1
                            && cand > cfl
                            && win[cand - 1] == win[start - 1]
                        {
                            cand -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                        pos = emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp);
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }

                if m1 == 0 {
                    let mut ml = extend_match(win, idx1, cand1);
                    if ml >= 6 && !ramp_blocks::<RAMPED>(ramp, pos1, win_base + cand1 as u64) {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand1, win_base);
                        while start > anchor_idx
                            && cand1 > cfl
                            && win[cand1 - 1] == win[start - 1]
                        {
                            cand1 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - cand1 + 3) as u32;
                        anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                        // A literal offset shifts the decoder's history
                        // one slot down; after the third one a job-start
                        // gate has fully converged and repcode use is
                        // safe again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            pos = if rep_pending == 0 {
                                emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp)
                            } else {
                                anchor
                            };
                        } else {
                            pos = emit.rep1_chain::<RAMPED>(win, anchor, block_end, &mut rep, ramp);
                        }
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }
                }
                }

            // Both positions missed: grow the probe step on long literal
            // runs so incompressible data does not pay a full hash per
            // byte. The step scales the whole pair so the probes-per-byte
            // density matches the single-position loop at every step size.
            // Faster-growing than libzstd's anchor-distance grid: our
            // single-probe table loses its far matches to overwrites when
            // incompressible gaps are probed densely (measured: json at
            // Fastest +9% size), and skipping is what keeps sparse-match
            // corpora fast.
            miss_count += pair_len as usize;
            let step = 1 + (miss_count >> 2).min(255) as u64;
            pos += pair_len * step;
            };
        }
        // Seeded phase: job starts only (bulk-ST skips it entirely); each
        // emit re-checks convergence at the head, and the values only ever
        // wind down.
        'seeded: while (rep_pending != 0 || seed_offset != 0)
            && block_end.saturating_sub(pos) >= hash_read
        {
            scan_fast!('seeded, true);
        }
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            scan_fast!('restart, false);
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        // A zero-sequence block stages nothing: its literals are exactly the
        // block, which the caller reads through get_last_space() instead of
        // paying a whole-block copy into and out of the scratch buffer.
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }

    /// The double-hash strategy loop (level [`Level::Fast`], libzstd's
    /// dfast): one 8-byte long-hash probe and one 5-byte short-hash probe
    /// per position — no chain walk, no lazy deferral. A two-position
    /// pipeline overlaps the hash multiplies and table loads, a short hit
    /// is upgraded by the long probe prepared for the next position, and
    /// matched ranges re-seed both tables through a few anchors (see
    /// [`DfastEmit::emit`]). The miss step grows with the literal run
    /// (one per 256 B, libzstd's `kSearchStrength` grid).
    #[allow(clippy::too_many_lines)]
    fn start_matching_dfast<const RAMPED: bool, const LONG_LOG: u32, const SMALL_LOG: u32>(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        // Known rows instantiate with their logs as constants (the hash
        // shifts fold to immediates and the two shift registers free up);
        // clamped-window shapes pass [`RUNTIME_LOG`] for both.
        let long_log = if LONG_LOG == RUNTIME_LOG {
            self.table.len().trailing_zeros()
        } else {
            LONG_LOG
        };
        let small_log = if SMALL_LOG == RUNTIME_LOG {
            self.chain.len().trailing_zeros()
        } else {
            SMALL_LOG
        };
        let win_base = self.win_base;
        let ramp = self.ramp;
        let block_len = (self.block_end - win_base) as usize;
        // Probing hashes 8 bytes, so the last probeable window index; the
        // insert bound coincides with it (the window ends at the block), so
        // one shared limit serves both.
        let limit_idx = block_len
            .saturating_sub(HASH_READ)
            .min(win.len().saturating_sub(HASH_READ));
        let insert_max_idx = limit_idx;
        let max_window = self.params.window as u64;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let mut anchor_idx = (self.anchor - win_base) as usize;
        let mut ip_idx = (self.pos - win_base) as usize;
        // The scan's own table writes go through raw pointers: routing them
        // through the emit context's slice fields kept the pointers,
        // log shifts and tag stack-resident (a re-load per access, the
        // classic SROA failure at this loop's live-value count).
        // SAFETY: derived here, before the context below takes its
        // borrows; both address the same memory, and the loop and the
        // emit helpers never access a slot concurrently.
        let long_ptr: *mut u32 = self.table.as_mut_ptr();
        let small_ptr: *mut u32 = self.chain.as_mut_ptr();
        let mut emit = DfastEmit {
            long: &mut self.table[..],
            small: &mut self.chain[..],
            literals,
            seqs,
            win_base,
            insert_max_idx,
            long_log,
            small_log,
        };

        // Resolve a table entry to a window index. Same distance trick as
        // the fast loop's `resolve`: `dist = pos - (entry - 1)` wraps huge
        // for the empty sentinel and previous-4-GiB-cycle entries, so
        // `dist <= reach` (reach = min(pos - win_base, max_window)) rejects
        // every failure mode in one compare, and invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip_idx` then rejects them with
        // predictable branches (libzstd's selectAddr trick).
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // Same double-wrapping form as the fast loop's `resolve`.
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Outer loop: one pass per emitted match; re-entering resets the
        // miss step. The inner loop walks single positions until a match
        // or the block tail. Like the fast loop, the body runs in two
        // phases (see `scan_fast`): the seeded phase carries the job-start
        // seed/gate state, the steady phase folds it out — this loop's live
        // set is even larger (two tables, two hash logs), so the dead
        // weight cost more here.
        macro_rules! scan_dfast {
            ($outer:lifetime, $gated:literal) => {
                // The short-match upgrade probes at ip + 1, so a pass needs a
                // full position pair inside the block.
                if ip_idx + 1 > limit_idx {
                    break $outer;
                }
                let mut ip1_idx = ip_idx + 1;
                let mut hl0 = hash8_at_log(win, ip_idx, long_log);
                // SAFETY: hl0 is masked to the long table size.
                let mut entry_l0 = unsafe { *long_ptr.add(hl0) };

                loop {
                let hs0 = hash_at_log(win, ip_idx, small_log);
                // SAFETY: hs0 is masked to the small table size.
                let entry_s0 = unsafe { *small_ptr.add(hs0) };
                let pos_abs = win_base + ip_idx as u64;
                // Oldest usable candidate age: within the level window and
                // inside the live window buffer.
                let reach = (pos_abs - win_base).min(max_window);
                // Insert after both lookups, before probing (newest-wins).
                // SAFETY: both hashes masked to their tables' sizes.
                unsafe {
                    let entry = pack_pos(pos_abs);
                    *long_ptr.add(hl0) = entry;
                    *small_ptr.add(hs0) = entry;
                }

                // Repcode pre-probe one byte ahead: the probed byte stays a
                // pending literal, so of_value 1 encodes rep0 instead of a
                // swap, and no backward extension may consume it. Gated job
                // starts skip the probe (unknown decoder history). The bound
                // is window-relative: `probe >= rep[0]` is
                // `win_base + probe >= win_base + rep[0]` with win_base
                // folded out.
                if !($gated && rep_pending != 0) {
                    let probe = ip_idx + 1;
                    if probe >= rep[0] as usize {
                        let cand = probe - rep[0] as usize;
                        if read4(win, cand) == read4(win, probe)
                            && !ramp_blocks::<RAMPED>(ramp,
                                win_base + probe as u64,
                                win_base + cand as u64,
                            )
                        {
                            let ml = extend_match(win, probe, cand);
                            debug_assert!(ml >= MIN_MATCH);
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, probe, ml, 1, &mut rep);
                            ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                            // The chain's matches advance the anchor too.
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                }

                // Seed-offset probe (job starts only): direct byte compare
                // at the prefill-detected long-repeat offset — neither
                // single-candidate table can serve it (see `seed_offset`).
                // Mirrors the fast loop's twin block.
                if $gated && seed_offset != 0 {
                    let ci = (pos_abs - seed_offset as u64 - win_base) as usize;
                    if read4(win, ci) == read4(win, ip_idx) {
                        let mut ml = extend_match(win, ip_idx, ci);
                        if ml >= 6
                            && pays_for_offset(ml, ip_idx, ci, false)
                            && !ramp_blocks::<RAMPED>(ramp, pos_abs, win_base + ci as u64)
                        {
                            let mut start = ip_idx;
                            let mut c = ci;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, ci, win_base);
                            while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                                c -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - c + 3) as u32;
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                            } else {
                                anchor_idx
                            };
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                let hl1 = hash8_at_log(win, ip1_idx, long_log);

                // Long probe: a full 8-byte match at the long-hash candidate.
                {
                    let cand = resolve(entry_l0, pos_abs, reach, ip_idx);
                    if cand != ip_idx
                        && read8(win, cand) == read8(win, ip_idx)
                        && !ramp_blocks::<RAMPED>(ramp, pos_abs, win_base + cand as u64)
                    {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                        // Backward catch-up into the pending literals; the
                        // offset (start - c) stays constant.
                        while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if ip1_idx < anchor_idx {
                            // The emit moved the anchor past ip1, so the
                            // match covers it: indexing it cannot pollute
                            // later probes.
                            // SAFETY: hl1 is masked to the long table size.
                            unsafe {
                                *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                            }
                        }
                        // A literal offset shifts the decoder's history one
                        // slot down; after the third one a job-start gate
                        // has fully converged and repcode use is safe
                        // again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                            } else {
                                anchor_idx
                            };
                        } else {
                            ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                        }
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue $outer;
                    }
                }

                // SAFETY: hl1 is masked to the long table size.
                let entry_l1 = unsafe { *long_ptr.add(hl1) };

                // Short probe: 4 bytes at the short-hash candidate, upgraded
                // by the long probe prepared for the next position.
                {
                    let cand = resolve(entry_s0, pos_abs, reach, ip_idx);
                    if cand != ip_idx && read4(win, cand) == read4(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let pos1_abs = win_base + ip1_idx as u64;
                        let reach1 = (pos1_abs - win_base).min(max_window);
                        let c1 = resolve(entry_l1, pos1_abs, reach1, ip1_idx);
                        if c1 != ip1_idx && read8(win, c1) == read8(win, ip1_idx) {
                            let l1len = extend_match(win, ip1_idx, c1);
                            if l1len > ml {
                                start = ip1_idx;
                                c = c1;
                                ml = l1len;
                            }
                        }
                        // A ramp-blocked pair falls through to the miss
                        // advance below (never `break`: the inner loop's
                        // advance is what moves past this position).
                        if !ramp_blocks::<RAMPED>(ramp, win_base + start as u64, win_base + c as u64) {
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, c, win_base);
                            while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                                c -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - c + 3) as u32;
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                            if ip1_idx < anchor_idx {
                                // The emit moved the anchor past ip1, so the
                                // match covers it: indexing it cannot pollute
                                // later probes.
                                // SAFETY: hl1 is masked to the long table
                                // size; see the long probe above.
                                unsafe {
                                    *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                                }
                            }
                            if $gated {
                                rep_pending = rep_pending.saturating_sub(1);
                                ip_idx = if rep_pending == 0 {
                                    emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                                } else {
                                    anchor_idx
                                };
                            } else {
                                ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                            }
                            // The chain's matches advance the anchor too.
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                }

                // Miss: advance the pair; the step grows with the literal
                // run (one per 256 B), so compressible data keeps probing
                // every byte while incompressible runs accelerate.
                ip_idx = ip1_idx;
                ip1_idx += 1 + ((ip1_idx - anchor_idx) >> 8);
                hl0 = hl1;
                entry_l0 = entry_l1;
                if ip1_idx > limit_idx {
                    break;
                }
            }
            // The pair left the block: nothing left to probe.
            break $outer;
            };
        }
        // Seeded phase (job starts only; bulk-ST skips it entirely), then
        // the steady phase — same convergence contract as `scan_fast`.
        'seeded: while (rep_pending != 0 || seed_offset != 0) && ip_idx < limit_idx {
            scan_dfast!('seeded, true);
        }
        'outer: loop {
            scan_dfast!('outer, false);
        }
        if !emit.seqs.is_empty() && anchor_idx < block_len {
            emit.literals.extend_from_slice(&win[anchor_idx..block_len]);
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }

    /// The hash-chain strategy loop (levels above [`Level::Fastest`]):
    /// walk same-hash candidates through the chain table up to the level's
    /// search depth, prefer repcode candidates (they encode nearly free),
    /// and defer emission across up to `lazy_depth` further positions when
    /// a longer match may start there — libzstd's lazy family.
    #[allow(clippy::too_many_lines)]
    fn start_matching_chain<const LDM: bool>(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let chain = &mut self.chain[..];
        let chain_mask = chain.len() - 1;
        let win_base = self.win_base;
        let ramp = self.ramp;
        let block_end = self.block_end;
        let hash_log = self.params.hash_log;
        let search_depth = self.params.search_depth as usize;
        let lazy_depth = self.params.lazy_depth;
        let min_match = self.params.min_match as usize;
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer (see the
        // fast loop's note on the same SROA failure); emits go through the
        // context. The chain-link reads share the pointer for the same
        // reason (the walk's slot reads must not re-derive the slice).
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = self.table.as_mut_ptr();
        let chain_ptr: *const u32 = chain.as_ptr();
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            win_base,
            insert_max,
            hash_log,
        };
        let hash_read = HASH_READ as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut miss_count = self.miss_count;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let lit_lens = &self.lit_lens;
        // Long-distance candidates of this block ([`Self::ldm_generate`]):
        // `ldm_i` is the first unconsumed one; covered candidates (their
        // split lies behind the anchor) are dropped as emissions advance.
        let ldm_seqs = &self.ldm_seqs[..];
        let mut ldm_i = 0usize;
        let mut ldm_won = LDM && false;

        // Chain-walk search from the hash head `entry` at window index `idx`,
        // returning the longest match's (length, candidate window index):
        // the module-level [`chain_search`], inlined here and in the lazy
        // walk below. The head entry is computed once by the caller (the
        // insert block links the same value — see there).
        let search = |win: &[u8], chain: *const u32, idx: usize, entry: u32| -> (usize, usize) {
            chain_search(
                win,
                chain,
                idx,
                entry,
                win_base,
                block_end,
                search_depth,
                chain_mask,
                max_window,
                ramp,
            )
        };

        // The body exists in two phases (the fast loop's pattern): the
        // seeded phase carries the job-start state (`seed_offset`/
        // `seed_hits`/`seed_budget`, `rep_pending`), which only ever winds
        // down; once it converges the steady phase runs a body with all of
        // it folded out. One loop kept those four values live across every
        // iteration — pure dead weight over bulk-ST and post-convergence
        // scanning — and the register pressure they added spilled the chain
        // walk's own loop values (win_base/pos_abs/reach/depth/mask
        // reloaded from the stack on every walk step). `$gated` is a
        // literal, so the phase-only paths constant-fold away in the steady
        // instantiation.
        macro_rules! scan_chain {
            ($restart:lifetime, $gated:literal, $ldm:expr) => {
            let idx = (pos - win_base) as usize;
            // Hash and head read once per position: the search, and the
            // insert below (whose chain link wants the same previous head),
            // share them — nothing writes the table in between.
            // SAFETY: hash_at_log masks to hash_log bits and the table holds
            // 1 << hash_log slots.
            let h = hash_at_log(win, idx, hash_log);
            let entry = unsafe { *table_ptr.add(h) };
            // Cross-position pipelining of the hash+head read (the dfast
            // ip0/ip1 pattern): the lazy walk below usually searches pos+1
            // first, and that head read is an L3-class random load that
            // would otherwise serialize in front of the walk. Issued here
            // it completes under the incumbent walk's shadow — issue
            // placement is load-bearing: after the walk's loop the loads
            // only decode behind its poorly-predicted exit branch and the
            // shadow evaporates (measured: dll +4.5% pre-walk vs +0.5%
            // post-walk). When the depth-0 rep probe advances pos the walk
            // starts at pos+2 instead and computes fresh (the None arm).
            // Skipped on the block tail (hashing idx+1 needs HASH_READ+1
            // bytes ahead).
            let mut pre1 = (0usize, 0u32);
            let piped = block_end - pos > hash_read;
            if piped {
                pre1.0 = hash_at_log(win, idx + 1, hash_log);
                // SAFETY: the hash masks to hash_log bits and the table
                // holds 1 << hash_log slots.
                pre1.1 = unsafe { *table_ptr.add(pre1.0) };
            }
            let (mut best_len, mut best_cand) = search(win, chain_ptr, idx, entry);

            // Long-distance candidate ([`super::ldm`]): a split whose far
            // 64-byte-window twin the sparse table retained. Probed before
            // the rep probe (which may advance pos past the split) and
            // after the chain search — a plain length competition; the
            // store gate and the lazy walk price the far offset. The live
            // length re-derivation cannot fall below MIN_MATCH_LENGTH
            // against the same bytes generation verified.
            if $ldm && ldm_i < ldm_seqs.len() {
                while ldm_i < ldm_seqs.len() && ldm_seqs[ldm_i].split < anchor {
                    ldm_i += 1;
                }
                if ldm_i < ldm_seqs.len() && ldm_seqs[ldm_i].split == pos {
                    let seq = ldm_seqs[ldm_i];
                    ldm_i += 1;
                    let cand_abs = pos - seq.offset as u64;
                    if cand_abs >= win_base {
                        let ci = (cand_abs - win_base) as usize;
                        if read4(win, ci) == read4(win, idx) {
                            let ml = extend_match(win, idx, ci);
                            if ml > best_len {
                                best_len = ml;
                                best_cand = ci;
                                ldm_won = true;
                            }
                        }
                    }
                }
            }

            // Repcode probe first when armed: with literals pending it runs
            // at the current position, otherwise one byte ahead so that
            // byte becomes the pending literal and of_value 1 stays
            // encodable (mirrors the fast loop). The offset encodes nearly
            // free, so bias it past the chain match.
            let mut rep_hit = false;
            if !$gated || rep_pending == 0 {
                let probe = if pos == anchor {
                    pos + 1
                } else {
                    pos
                };
                if let Some(cand_abs) = probe.checked_sub(rep[0] as u64)
                    && cand_abs >= win_base
                {
                    let pidx = (probe - win_base) as usize;
                    let cand = (cand_abs - win_base) as usize;
                    if read4(win, cand) == read4(win, pidx) {
                        let ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH
                            && ml + 3 > best_len
                            && !ramp.blocks(probe, cand_abs)
                        {
                            best_len = ml;
                            best_cand = cand;
                            rep_hit = true;
                            pos = probe;
                        }
                    }
                }
            }

            // Seed-offset probe (job starts only): the prefill-detected
            // long-repeat offset as a direct candidate, before the chain's
            // candidates — clumped data buries the period twin too deep in
            // the chain for the depth-limited walk to reach (see
            // `seed_offset`). Mirrors the fast loop's twin block; the pays
            // gate below keeps a far seed honest.
            let mut seed_hit = false;
            if $gated && seed_offset != 0 {
                let ci = (pos - seed_offset as u64 - win_base) as usize;
                if read4(win, ci) == read4(win, idx) {
                    let ml = extend_match(win, idx, ci);
                    if ml >= 6 && ml + 3 > best_len && !ramp.blocks(pos, win_base + ci as u64) {
                        best_len = ml;
                        best_cand = ci;
                        rep_hit = false;
                        seed_hit = true;
                    }
                }
                if !seed_hit {
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }
            }

            // Insert this position behind the probe (newest-wins), linking
            // the chain to the previous head. The chain slot key is the
            // absolute position — what the walk resolves candidates with.
            // The head value is the one read before the search: nothing has
            // written the table since.
            // SAFETY: both indices are masked to their tables' sizes.
            unsafe {
                *chain.get_unchecked_mut(pos as usize & chain_mask) = entry;
                *table_ptr.add(h) = pack_pos(pos);
            }

            // Resolve the pipelined head for the lazy walk below: the
            // insert above wrote table[h], so a pre-read of that very slot
            // must observe the new head (a fresh read at search time
            // would). The rep probe advancing pos leaves `pipe` empty —
            // its first search sits at pos+2 and computes fresh.
            let mut pipe: Option<u32> = None;
            if piped && (pos - win_base) as usize == idx {
                let (ph, mut pe) = pre1;
                if ph == h {
                    pe = pack_pos(pos);
                }
                pipe = Some(pe);
            }

            // Repcode matches stay legal from MIN_MATCH up regardless of the
            // row's min_match (libzstd's rep probe also checks 4 bytes).
            if (best_len < min_match && !(rep_hit && best_len >= MIN_MATCH))
                || !pays_for_offset_lit(win, idx, best_len, best_cand, rep_hit, lit_lens)
            {
                // Grow the probe step on long literal runs (same policy as
                // the fast loop) so incompressible data does not pay a full
                // chain walk per byte. Faster-growing than libzstd's
                // anchor-distance grid: our per-probe chain walk is dearer,
                // and skipping over sparse-match gaps is what keeps the
                // Balanced levels fast on them.
                miss_count += 1;
                let step = 1 + (miss_count >> 2).min(255) as u64;
                if $ldm && ldm_i < ldm_seqs.len() {
                    // Never step over an unconsumed split: its candidate
                    // dies with the position (the scan is its only
                    // consumer).
                    pos = (pos + step).min(ldm_seqs[ldm_i].split);
                } else {
                    pos += step;
                }
                continue $restart;
            }
            miss_count = 0;

            // Lazy evaluation, ported to libzstd lazy's alternating gain
            // walk: each next position competes on offset-aware gain — a
            // match's distance is priced at its highbit (~4 bits per length
            // byte), so a much nearer match of near-equal length displaces a
            // long-range one (the selection error that cost text ~2% ratio:
            // pure-length comparison kept far chain candidates). A repcode
            // probe rides along (rep offsets price ~free). The walk advances
            // while some probe improves; a position that fails the depth-1
            // margins gets one second-chance probe at depth-2 margins before
            // giving up — exactly libzstd's ZSTD_lazy/ZSTD_lazy2 split.
            // Long-enough matches skip straight to emission.
            let mut best_pos = pos;
            let mut best_price = if rep_hit {
                0
            } else {
                price_of((best_pos - win_base) as usize, best_cand)
            };
            // Greedy rows (lazy_depth 0) emit the first acceptable match
            // without probing further positions (libzstd's ZSTD_greedy).
            if lazy_depth > 0 && best_len < 64 {
                // The incumbent's displaced-literal value (see lit_value);
                // recomputed whenever the incumbent changes so both sides
                // of the walk's gain comparisons price in fed-back literal
                // code lengths instead of the flat 4 bits/byte.
                let mut best_v = lit_value(win, (best_pos - win_base) as usize, best_len, lit_lens);
                'lazy: loop {
                    for (attempt, &(rep_mul, rep_m, search_m)) in
                        [(3i32, 1i32, 4i32), (4, 1, 7)].iter().enumerate()
                    {
                        if attempt > 0 && lazy_depth < 2 {
                            // Second-chance margins are the depth-2 strategy.
                            break;
                        }
                        let p2 = pos + 1;
                        if block_end.saturating_sub(p2) < hash_read {
                            break 'lazy;
                        }
                        pos = p2;
                        let idx2 = (p2 - win_base) as usize;
                        // Pipelined hash+head for this position, issued one
                        // search back under that walk's shadow (fresh for
                        // the first step, the refresh below for the rest):
                        // no table writes happen inside the lazy walk, and
                        // every path to the next search steps exactly one
                        // position. None only before the first search and
                        // at the block tail.
                        let entry2 = match pipe.take() {
                            Some(pre) => pre,
                            None => {
                                let hf = hash_at_log(win, idx2, hash_log);
                                // SAFETY: hash_at_log masks to hash_log
                                // bits and the table holds 1 << hash_log
                                // slots.
                                unsafe { *table_ptr.add(hf) }
                            }
                        };
                        // Refresh the pipeline for the next search (this
                        // position +1); the guard equals the next attempt's
                        // own break condition, so a skipped refresh is
                        // never consumed.
                        if block_end.saturating_sub(p2 + 1) >= hash_read {
                            let hn = hash_at_log(win, idx2 + 1, hash_log);
                            // SAFETY: as above.
                            pipe = Some(unsafe { *table_ptr.add(hn) });
                        }
                        // Repcode probe at the stepped position; literals are
                        // pending by construction (the walk advanced past the
                        // anchor), so of_value 1 stays encodable. Skipped when
                        // the incumbent is itself a rep: rep-vs-rep only
                        // accepts a strictly longer ride, which the depth-0
                        // probe and rep1_chain already cover, and on
                        // rep-dense shapes the extra read4+extend per step
                        // visibly taxed scan speed.
                        if (!$gated || rep_pending == 0)
                            && !rep_hit
                            && let Some(cand_abs) = p2.checked_sub(rep[0] as u64)
                            && cand_abs >= win_base
                            && !ramp.blocks(p2, cand_abs)
                        {
                            let ci = (cand_abs - win_base) as usize;
                            if read4(win, ci) == read4(win, idx2) {
                                let ml = extend_match(win, idx2, ci);
                                if ml >= MIN_MATCH {
                                    // Same gain-cap cheap reject as the
                                    // chain probe: lit_value tops out at 6
                                    // bits per byte (the clamp).
                                    if 6 * ml as i32 * rep_mul / 4
                                        > best_v * rep_mul / 4 - best_price + rep_m
                                    {
                                        let v = lit_value(win, idx2, ml, lit_lens);
                                        // rep_mul keeps libzstd's rep discount
                                        // (3/4 of the byte value at depth 1);
                                        // at the default lens v = ml*4 and this
                                        // is the flat ml*rep_mul comparison.
                                        if v * rep_mul / 4
                                            > best_v * rep_mul / 4 - best_price + rep_m
                                        {
                                            best_len = ml;
                                            best_cand = ci;
                                            best_pos = p2;
                                            best_price = 0;
                                            best_v = v;
                                            rep_hit = true;
                                            seed_hit = false;
                                            continue 'lazy;
                                        }
                                    }
                                }
                            }
                        }
                        // Chain search at the stepped position, on the
                        // pipelined hash+head read above.
                        let (len2, cand2) = search(win, chain_ptr, idx2, entry2);
                        let price2 = if len2 >= min_match {
                            price_of((p2 - win_base) as usize, cand2)
                        } else {
                            0
                        };
                        if len2 >= min_match {
                            // Cheap reject: lit_value tops out at 6 bits per
                            // byte (the clamp), so a candidate whose gain
                            // cap cannot beat the incumbent skips the four
                            // gathers entirely.
                            if 6 * len2 as i32 - price2 > best_v - best_price + search_m {
                                let v2 = lit_value(win, idx2, len2, lit_lens);
                                if v2 - price2 > best_v - best_price + search_m {
                                    best_len = len2;
                                    best_cand = cand2;
                                    best_pos = p2;
                                    best_price = price2;
                                    best_v = v2;
                                    rep_hit = false;
                                    seed_hit = false;
                                    continue 'lazy;
                                }
                            }
                        }
                    }
                    break;
                }
            }
            let mut start = (best_pos - win_base) as usize;

            // Backward extension into the pending literals; the offset
            // (start - cand) stays constant. A repcode emission must keep
            // one literal pending: of_value 1 with a zero literal length
            // resolves to a repcode *swap* on the decoder side, not rep0.
            let anchor_idx = (anchor - win_base) as usize;
            let floor = if rep_hit {
                anchor_idx + 1
            } else {
                anchor_idx
            };
            let cfl = ramp.ext_floor(best_cand, win_base);
            let mut cand = best_cand;
            let mut ml = best_len;
            while start > floor && cand > cfl && win[cand - 1] == win[start - 1] {
                cand -= 1;
                start -= 1;
                ml += 1;
            }
            let of_value = if rep_hit {
                1
            } else {
                (start - cand + 3) as u32
            };
            anchor = emit.emit_chain(win, chain, hash_log, anchor, start, ml, of_value, &mut rep);
            if $gated && rep_pending != 0 && of_value > 3 {
                rep_pending -= 1;
            }
            if seed_hit {
                seed_hits += 1;
                if seed_hits >= SEED_MATCHES {
                    seed_offset = 0;
                }
            }
            if $gated && rep_pending != 0 {
                pos = anchor;
            } else {
                pos = emit.rep1_chain::<true>(win, anchor, block_end, &mut rep, ramp);
            }
            anchor = pos;
            };
        }
        // Seeded phase: job starts only (bulk-ST skips it entirely); each
        // emit re-checks convergence at the head, and the values only ever
        // wind down.
        'seeded: while (rep_pending != 0 || seed_offset != 0)
            && block_end.saturating_sub(pos) >= hash_read
        {
            scan_chain!('seeded, true, LDM);
        }
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            scan_chain!('restart, false, LDM);
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        if LDM {
            self.ldm_note_block(ldm_won);
        }
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }

    /// Bridge into the optimal parser (levels Opt/Ultra): hands over
    /// the window, the epoch-tagged tables and the persistent price state,
    /// then stores back the cursors the parser advanced. The tree's search
    /// domain is the `chain_reach` override (the stock row window; the
    /// frame window may sit wider for LDM's far classes).
    fn start_matching_opt(
        &mut self,
        knobs: OptKnobs,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_start = self.block_start;
        let block_end = self.block_end;
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let ldm_seqs: &[LdmSeq] = &self.ldm_seqs[..];
        let mut epoch = self.epoch;
        let mut next_update = self.next_update;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        // The fill-lag clamp in `opt::run_block` runs only on the
        // alphabet-gated population: `ldm_checked` set with `ldm` disabled
        // means LDM was armed and the first block's alphabet check turned
        // it off (a size-gated frame never reaches the check, and an
        // armed frame keeps its far candidates). Job/dictionary strips
        // (`prefill_window`) stay unclamped: their restart parses measurably
        // used the lagged region's candidates.
        let clamp_lag = self.ldm_checked && self.ldm.is_none() && !self.strip_parse;
        let Some(scratch) = self.opt_scratch.as_mut() else {
            unreachable!("opt scratch allocated by apply_level")
        };
        // Disjoint field borrows: the window (win/ext) against the tables.
        let ldm_won = if knobs.ultra {
            super::opt::run_block::<true>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                ldm_seqs,
                &mut epoch,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
                clamp_lag,
            )
        } else {
            super::opt::run_block::<false>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                ldm_seqs,
                &mut epoch,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
                clamp_lag,
            )
        };
        self.epoch = epoch;
        self.next_update = next_update;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.pos = block_end;
        self.anchor = block_end;
        ldm_won
    }

    /// Whether the cold-start DUBT head applies to this frame: the row
    /// opts in and the input is large enough that the head stays a
    /// minority share of the parse (a declared length below
    /// [`HEAD_MIN_TOTAL`] would be parsed entirely by the head).
    fn head_eligible(&self) -> bool {
        self.params.dubt_head
            && matches!(self.params.strategy, Strategy::Chain(_))
            && self.shape.len.map_or(true, |l| l >= HEAD_MIN_TOTAL)
    }

    /// Dispatch guard for the cold-start DUBT head ([`HEAD_LIMIT`]): on
    /// the first head block, evaluate the alphabet gate and arm the head
    /// tables; true while this block parses through the btlazy2 driver.
    fn head_block(&mut self) -> bool {
        if self.dubt_head != HeadPhase::Armed || self.block_start >= HEAD_LIMIT {
            if self.dubt_head == HeadPhase::Running && self.block_start >= HEAD_LIMIT {
                // First dispatch past the span: a gated or RLE-skipped final
                // head block never reaches the parser, so the handoff rides
                // the first block that does (before its chain scan probes).
                self.finish_head();
            }
            return self.dubt_head == HeadPhase::Running;
        }
        let win = window_slice(&self.win, self.ext.as_ref());
        let idx = self.idx_of(self.block_start);
        let mut seen = [0u64; 4];
        for &b in &win[idx..(idx + 8192).min(win.len())] {
            seen[(b >> 6) as usize] |= 1 << (b & 63);
        }
        let syms: u32 = seen.iter().map(|w| w.count_ones()).sum();
        if syms < HEAD_SYMS_MIN {
            self.dubt_head = HeadPhase::Off;
            return false;
        }
        if self.dubt_table.len() == 1 << HEAD_HASH_LOG
            && self.dubt_bt.len() == 2 << HEAD_KNOBS.bt_log
        {
            self.dubt_table.fill(0);
            self.dubt_bt.fill(0);
        } else {
            self.dubt_table = alloc::vec![0u32; 1 << HEAD_HASH_LOG];
            self.dubt_bt = alloc::vec![0u32; 2 << HEAD_KNOBS.bt_log];
        }
        if self.lazy_scratch.is_none() {
            self.lazy_scratch = Some(LazyScratch::new());
        }
        self.dubt_head = HeadPhase::Running;
        true
    }

    /// Head handoff after the block that crosses [`HEAD_LIMIT`]: the chain
    /// takes over from the next block with the whole head region
    /// dense-indexed into its tables (`gap_start` at the frame start
    /// drives [`Self::catch_up_insertions`] before the next scan probes).
    /// The head tables stay allocated for the pooled state's next frame.
    fn finish_head(&mut self) {
        if self.block_end >= HEAD_LIMIT {
            self.dubt_head = HeadPhase::Done;
            self.gap_start = 0;
            self.miss_count = 0;
            self.rep_pending = 0;
        }
    }

    /// Bridge into the btlazy2 parser (rows 13-15): the same tables and
    /// cursors as the opt bridge, no price state. The search domain is the
    /// `chain_reach` override (the stock row window; the frame window may
    /// sit wider for LDM's far classes).
    fn start_matching_btlazy(
        &mut self,
        knobs: OptKnobs,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let ldm_seqs: &[LdmSeq] = &self.ldm_seqs[..];
        let Some(scratch) = self.lazy_scratch.as_mut() else {
            unreachable!("btlazy scratch allocated by apply_level")
        };
        let lit_lens = self.lit_lens;
        // Disjoint field borrows: the window (win/ext) against the tables.
        let ldm_won = super::btlazy::run_block_lazy(
            &knobs,
            win,
            self.win_base,
            self.block_start,
            self.block_end,
            max_window,
            ldm_seqs,
            &mut self.dubt_table,
            &mut self.dubt_bt,
            &mut self.next_update,
            scratch,
            &lit_lens,
            &mut rep,
            &mut rep_pending,
            literals,
            seqs,
        );
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        ldm_won
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{LdmArming, MatchGeneratorDriver, ldm_min_window, pack_pos, unpack_pos};
    use crate::encoding::{Matcher, Sequence};

    #[test]
    fn ldm_arming_bars() {
        // The mid-size bar arms one step under the row window; the job bar
        // requires the clamp to have left the row window intact; the probe's
        // keep parse never arms.
        assert_eq!(ldm_min_window(LdmArming::Frame), Some(1 << 25));
        assert_eq!(ldm_min_window(LdmArming::Job), Some(1 << 26));
        assert_eq!(ldm_min_window(LdmArming::ProbeKeep), None);
    }

    fn block_label(i: usize) -> Vec<u8> {
        // "block N filler text; " without needing format! in no_std tests
        let mut v = Vec::new();
        v.extend_from_slice(b"block ");
        v.push(b'0' + i as u8);
        v.extend_from_slice(b" filler text; ");
        v
    }

    /// [`match_and_reconstruct`] at the default block size, recording every
    /// emitted sequence's offset.
    fn match_and_reconstruct_collecting_offsets(data: &[u8], offsets: &mut Vec<usize>) -> Vec<u8> {
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(crate::Level::Balanced);
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(128 * 1024) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    offsets.push(offset);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        reconstructed
    }

    /// Feed `data` through the matcher one block at a time and reconstruct the
    /// original from the emitted sequences.
    fn match_and_reconstruct(data: &[u8], block_size: usize) -> Vec<u8> {
        let mut driver = MatchGeneratorDriver::new(block_size);
        driver.reset(crate::Level::Fastest);
        // Offset history mirrors the decoder's per-frame scratch.
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(block_size) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    // Matches may overlap their own output (offset < match_len).
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        reconstructed
    }

    #[test]
    fn reconstructs_short_runs() {
        let mut data = Vec::new();
        data.extend([0u8; 16]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([0u8; 8]);
        assert_eq!(match_and_reconstruct(&data, 8), data);
        assert_eq!(match_and_reconstruct(&data, 4), data);
    }

    #[test]
    fn reconstructs_tiny_rep_inputs() {
        // Fuzz-found (2026-09-14): an 8-byte input whose rep probe hits at
        // position 1 gives insert_covered a match start past insert_max
        // (win_base + 0 here); the wrapped `end - p` parity peel then
        // hashed 8 bytes past the input. Guarded empty-range now.
        for data in [
            &[0x0fu8; 6][..],
            &[0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x2a, 0xff][..],
            &[0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x2a, 0x90][..],
        ] {
            assert_eq!(&match_and_reconstruct(data, 8)[..], data);
        }
    }

    #[test]
    fn reconstructs_across_blocks() {
        // Matches must reach into previous blocks through the shared window.
        let mut data = Vec::new();
        for i in 0..10 {
            data.extend_from_slice(&[0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0f]);
            data.extend_from_slice(&block_label(i));
        }
        assert_eq!(match_and_reconstruct(&data, 32), data);
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    fn reconstructs_random() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut data = Vec::with_capacity(300 * 1024);
        while data.len() < 300 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(&state.to_le_bytes());
        }
        assert_eq!(match_and_reconstruct(&data, 64 * 1024), data);
    }

    #[test]
    fn reconstructs_after_compaction() {
        // 3 MiB of data with repeats forces window compaction to run.
        let mut data = Vec::with_capacity(3 << 20);
        for i in 0..(3 << 20) / 64 {
            data.push((i % 251) as u8);
            data.extend(&[7u8; 63]);
        }
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    /// Far repeats beyond the chain reach must ride LDM candidates: two
    /// copies of a random block 5 MiB apart inside filler, compressed at
    /// the balanced row (window W26, chain reach W22). The parse is only
    /// legal if some sequence references the far class.
    #[test]
    fn ldm_row_covers_beyond_chain_reach() {
        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // dll-shaped: 80 distinct 64 KiB blocks, then a run of far copies
        // (5 MiB back, beyond the row's 4 MiB chain reach) spaced 64 KiB
        // apart — the far class recurs throughout, like shared code in
        // concatenated binaries, instead of one isolated duplicate (which
        // the quiet latch could legitimately miss; see LDM_QUIET).
        const UNIT: usize = 64 * 1024;
        let mut data = Vec::with_capacity(6 * 1024 * 1024);
        for _ in 0..80 {
            data.extend((0..UNIT).map(|_| (rand() >> 32) as u8));
        }
        for i in 0..16 {
            let unit: Vec<u8> = data[i * UNIT..(i + 1) * UNIT].to_vec();
            data.extend_from_slice(&unit);
        }
        let mut offsets = Vec::new();
        let reconstructed = match_and_reconstruct_collecting_offsets(&data, &mut offsets);
        assert_eq!(reconstructed, data);
        assert!(
            offsets.iter().any(|&o| o > (1 << 22)),
            "no sequence beyond the chain reach: LDM far class missing"
        );
    }

    /// dll corpus roundtrips through the balanced row's LDM path, when the
    /// generated large-binary corpus is present (gitignored; built by
    /// bench/gen_big.sh): dll100 exercises the full W26 reach, dll32 the
    /// source-clamped window.
    #[test]
    #[cfg(feature = "std")]
    fn ldm_dll_roundtrip() {
        for name in ["bench/big/dll100.raw", "bench/big/dll32.raw"] {
            let Ok(raw) = std::fs::read(name) else {
                continue;
            };
            let c = crate::encoding::compress_slice_opts(&raw, crate::Level::Balanced, false);
            let d = crate::bulk::decompress(&c, raw.len()).expect("decode");
            assert_eq!(d, raw, "{name}");
        }
    }

    /// Far repeats beyond the tree domain must ride LDM candidates on the
    /// opt and btlazy2 rows too (wide W26 frame window, tree domain at the
    /// stock row window): the same shape as the balanced test above, at
    /// Best, Opt and Ultra, with the copies 12 MiB apart — beyond the
    /// rows' 4-8 MiB domains.
    #[test]
    fn ldm_high_rows_cover_beyond_tree_domain() {
        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const UNIT: usize = 64 * 1024;
        let mut data = Vec::with_capacity(14 * 1024 * 1024);
        for _ in 0..200 {
            data.extend((0..UNIT).map(|_| (rand() >> 32) as u8));
        }
        for i in 0..16 {
            let unit: Vec<u8> = data[i * UNIT..(i + 1) * UNIT].to_vec();
            data.extend_from_slice(&unit);
        }
        for level in [crate::Level::Best, crate::Level::Opt, crate::Level::Ultra] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            driver.reset(level);
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = Vec::new();
            let mut far = false;
            for block in data.chunks(128 * 1024) {
                driver.block_tail()[..block.len()].copy_from_slice(block);
                driver.commit_block(block.len());
                driver.start_matching(|seq| match seq {
                    Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                    Sequence::Triple {
                        literals,
                        offset,
                        match_len,
                    } => {
                        reconstructed.extend_from_slice(literals);
                        if offset > (1 << 23) {
                            far = true;
                        }
                        let actual = crate::decoding::sequence_execution::do_offset_history(
                            offset as u32,
                            literals.len() as u32,
                            &mut rep,
                        );
                        let start = reconstructed.len() - actual as usize;
                        for i in 0..match_len {
                            let b = reconstructed[start + i];
                            reconstructed.push(b);
                        }
                    },
                });
            }
            assert_eq!(reconstructed, data, "reconstruct {level:?}");
            assert!(far, "{level:?}: no sequence beyond the tree domain");
        }
    }

    #[test]
    fn opt_levels_reconstruct() {
        let mut data = Vec::with_capacity(700 * 1024);
        let words = [
            &b"the quick brown fox "[..],
            &b"jumps over the lazy dog "[..],
            &b"lorem ipsum dolor sit amet "[..],
            b"\x00\x01\x02\x03 structured noise ",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        while data.len() < 700 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(words[(state as usize) % words.len()]);
        }
        for level in [crate::Level::Best, crate::Level::Opt, crate::Level::Ultra] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            driver.reset(level);
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = Vec::new();
            for block in data.chunks(128 * 1024) {
                driver.block_tail()[..block.len()].copy_from_slice(block);
                driver.commit_block(block.len());
                driver.start_matching(|seq| match seq {
                    Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                    Sequence::Triple {
                        literals,
                        offset,
                        match_len,
                    } => {
                        reconstructed.extend_from_slice(literals);
                        let actual = crate::decoding::sequence_execution::do_offset_history(
                            offset as u32,
                            literals.len() as u32,
                            &mut rep,
                        );
                        let start = reconstructed.len() - actual as usize;
                        for i in 0..match_len {
                            let b = reconstructed[start + i];
                            reconstructed.push(b);
                        }
                    },
                });
            }
            assert_eq!(reconstructed, data, "reconstruct {level:?}");
        }
    }

    #[test]
    fn pos_entry_roundtrip_across_cycles() {
        // A live entry always resolves back to its position, even across a
        // 4 GiB cycle boundary; a value numerically above the scan position
        // with no cycle to unwrap into is dead (stale cross-frame entry).
        for abs in [0u64, 1, 7, 0xffff_fffe, 5_000_000_000, 1 << 40] {
            for delta in [1u64, 2, 0x123, 0xffff_f000] {
                let pos = abs + delta;
                assert_eq!(unpack_pos(pack_pos(abs), pos), Some(abs), "{abs}+{delta}");
            }
        }
        // 2^32 - 1 collides with the empty sentinel: one dead position per
        // 4 GiB cycle, by design.
        assert_eq!(unpack_pos(pack_pos(0xffff_ffff), 1 << 40), None);
        assert_eq!(unpack_pos(pack_pos(1000), 10), None);
        assert_eq!(unpack_pos(0, 1 << 40), None);
    }

    /// The seed scan must return exactly the nearest qualifying position a
    /// naive byte-wise walk would find, on every path: the AVX-512 block
    /// walk (residue classes mod 8, block boundaries, the anchor's trivial
    /// self-match), the scalar tail below the last block, and strips too
    /// short for the block scheme.
    #[test]
    fn seed_scan_matches_naive_walk() {
        let naive = |data: &[u8], last: usize| -> Option<usize> {
            let a8 = super::read8(data, last);
            (0..last).rev().find(|&u| {
                super::read8(data, u) == a8
                    && super::seed_agrees(data, u, last)
                    && super::seed_confirms(data, u, last)
            })
        };
        let check = |data: &[u8]| {
            let last = data.len() - 8;
            let expect = naive(data, last);
            assert_eq!(
                super::seed_scan(data, last, super::read8(data, last)),
                expect,
                "dispatched scan at len {}",
                data.len()
            );
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            if last >= super::SEED_SCAN_MIN && std::is_x86_feature_detected!("avx512f") {
                assert_eq!(
                    // SAFETY: feature detected above; bounds argued at the
                    // definition.
                    unsafe { super::seed_scan_avx512(data, last, super::read8(data, last)) },
                    expect,
                    "avx512 scan at len {}",
                    data.len()
                );
            }
        };

        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // A 56-byte pattern planted as the anchor window `[last-48,
        // last+8)` and, disjoint from it, as the candidate window
        // `[hit-48, hit+8)`; everything else random. The candidate is a
        // confirmed repeat: the u64s behind it at each confirm offset
        // (where it has history) mirror the anchor's. `hit` sweeps the
        // interesting offsets: residue classes mod 8, the block-grid
        // boundaries and the bottom tail (< 64).
        let len = 700;
        let last = len - 8;
        let mut data = alloc::vec![0u8; len];
        for hit in [
            last - 56,
            last - 57,
            last - 63,
            last - 64,
            last - 65,
            last - 72,
            last - 119,
            last - 120,
            last - 128,
            65,
            64,
            63,
            48,
        ] {
            for b in &mut data {
                *b = rng() as u8;
            }
            let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
            data[hit - 48..hit + 8].copy_from_slice(&pat);
            data[last - 48..].copy_from_slice(&pat);
            for t in super::SEED_CONFIRMS {
                if hit >= t {
                    let a = super::read8(&data, last - t);
                    data[hit - t..hit - t + 8].copy_from_slice(&a.to_le_bytes());
                }
            }
            check(&data);
            assert_eq!(
                super::seed_scan(&data, last, super::read8(&data, last)),
                Some(hit),
                "planted hit {hit}"
            );
        }
        // A candidate below the agree bound never qualifies: the planted
        // anchor window alone must yield no seed.
        for b in &mut data {
            *b = rng() as u8;
        }
        let pat: Vec<u8> = (0..56).map(|_| rng() as u8).collect();
        data[last - 48..].copy_from_slice(&pat);
        check(&data);
        assert_eq!(
            super::seed_scan(&data, last, super::read8(&data, last)),
            None
        );
        // A local block repeat passes the 56-byte window but not the spread
        // confirms; only a confirmed candidate may become the seed, so the
        // nearer unconfirmed plant is skipped for the farther confirmed one
        // — and with no confirmed plant at all there is no seed.
        for far in [Some(last - 300), None] {
            for b in &mut data {
                *b = rng() as u8;
            }
            data[last - 48..].copy_from_slice(&pat);
            data[last - 148..last - 92].copy_from_slice(&pat); // near, unconfirmed
            if let Some(hit) = far {
                data[hit - 48..hit + 8].copy_from_slice(&pat);
                for t in super::SEED_CONFIRMS {
                    if hit >= t {
                        let a = super::read8(&data, last - t);
                        data[hit - t..hit - t + 8].copy_from_slice(&a.to_le_bytes());
                    }
                }
            }
            check(&data);
            assert_eq!(
                super::seed_scan(&data, last, super::read8(&data, last)),
                far,
                "confirmed candidate {far:?}"
            );
        }
        // All-same bytes: every position trivially matches, nearest wins and
        // the anchor itself is excluded.
        let flat = alloc::vec![0x5Au8; len];
        check(&flat);
        assert_eq!(
            super::seed_scan(&flat, last, super::read8(&flat, last)),
            Some(last - 1)
        );
        // Exact periods across all residue classes mod 8: the scan must find
        // the period (or a multiple) exactly like the naive walk.
        for period in [200, 201, 202, 203, 204, 205, 206, 207, 256, 257] {
            let unit: Vec<u8> = (0..period).map(|_| rng() as u8).collect();
            let mut tiled = alloc::vec![0u8; 0];
            while tiled.len() < len {
                tiled.extend_from_slice(&unit);
            }
            tiled.truncate(len);
            check(&tiled);
        }
        // No repeat at all, at sizes just around the block-scheme minimum.
        for l in [48, 56, 63, 64, 120, 127, 128, 129, 136, len] {
            let mut data = alloc::vec![0u8; l];
            for b in &mut data {
                *b = rng() as u8;
            }
            // A u64-rng strip of this size has no 8-byte recurrence.
            check(&data);
        }
    }

    /// Stale u32 entries from an earlier frame must die on the domain
    /// check: their values share no position domain with the new frame's
    /// scan, and the wrap reconstruction must reject the ones numerically
    /// above it instead of underflowing into an out-of-bounds window index.
    /// Regression for a pooled-driver double-frame compression.
    #[test]
    fn stale_entries_survive_frame_reset() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // Frame 1: > 1 MiB so the tables end holding positions far above
        // frame 2's scan. Frame 2: small and structured, so its probes hit
        // stale slots from the first position on.
        let mut long = Vec::with_capacity(3 << 20);
        while long.len() < 3 << 20 {
            long.extend_from_slice(&rng().to_le_bytes());
            long.extend_from_slice(b"stable filler segment; ");
        }
        let mut small = Vec::new();
        for i in 0..64 {
            small.extend_from_slice(&block_label(i % 7));
            small.push(rng() as u8);
        }
        for level in [
            crate::Level::Fastest,
            crate::Level::Fast,
            crate::Level::Balanced,
        ] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            for data in [&long, &small] {
                driver.reset(level);
                let mut rep = [1u32, 4, 8];
                let mut reconstructed = Vec::new();
                for block in data.chunks(128 * 1024) {
                    driver.block_tail()[..block.len()].copy_from_slice(block);
                    driver.commit_block(block.len());
                    driver.start_matching(|seq| match seq {
                        Sequence::Literals { literals } => {
                            reconstructed.extend_from_slice(literals);
                        },
                        Sequence::Triple {
                            literals,
                            offset,
                            match_len,
                        } => {
                            reconstructed.extend_from_slice(literals);
                            let actual = crate::decoding::sequence_execution::do_offset_history(
                                offset as u32,
                                literals.len() as u32,
                                &mut rep,
                            );
                            let start = reconstructed.len() - actual as usize;
                            for i in 0..match_len {
                                let b = reconstructed[start + i];
                                reconstructed.push(b);
                            }
                        },
                    });
                }
                assert_eq!(reconstructed, *data, "reconstruct {level:?}");
            }
        }
    }

    #[test]
    fn skip_matching_indexes_for_later_blocks() {
        let mut driver = MatchGeneratorDriver::new(16);
        driver.reset(crate::Level::Fastest);
        let pattern = [3u8, 1, 4, 1, 5, 9, 2, 6];
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        driver.skip_matching();
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        let mut got_triple = false;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                // New-offset wire value: actual offset 8 encodes as 8 + 3.
                assert_eq!(offset, pattern.len() + 3);
                got_triple = true;
            }
        });
        assert!(
            got_triple,
            "second block must match the skipped first block"
        );
    }

    #[test]
    fn gated_block_stays_match_history() {
        // A near-random block big enough for the incompressibility gate must
        // gate; a later duplicate must NOT gate (probe hit) and must match
        // into the gated block — through the catch-up fill for the table
        // strategies, the lazy tree fill for the opt strategies.
        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut block = Vec::with_capacity(20_000);
        while block.len() < 20_000 {
            block.extend_from_slice(&rng().to_le_bytes());
        }
        for level in [
            crate::Level::Fastest,
            crate::Level::Fast,
            crate::Level::Balanced,
            crate::Level::Best,
        ] {
            let mut driver = MatchGeneratorDriver::new(128 * 1024);
            driver.reset(level);
            driver.block_tail()[..block.len()].copy_from_slice(&block);
            driver.commit_block(block.len());
            assert!(
                driver.skip_if_incompressible(),
                "first block must gate at {level:?}"
            );
            driver.block_tail()[..block.len()].copy_from_slice(&block);
            driver.commit_block(block.len());
            assert!(
                !driver.skip_if_incompressible(),
                "duplicate must stay matchable at {level:?}"
            );
            let mut got_triple = false;
            driver.start_matching(|seq| {
                if let Sequence::Triple {
                    offset, match_len, ..
                } = seq
                    && !got_triple
                {
                    // New-offset wire value: the actual offset is the block
                    // length, encoded as len + 3.
                    assert_eq!(offset, block.len() + 3, "first match offset at level");
                    assert!(match_len > 1000, "duplicate must match wholesale");
                    got_triple = true;
                }
            });
            assert!(
                got_triple,
                "duplicate must match the gated block at {level:?}"
            );
        }
    }

    #[test]
    fn emits_repcode_sequences() {
        // Structured repetition at a fixed distance: the first repeat is found
        // by the hash probe (offset becomes rep[0]), later repeats must be
        // emitted as repcode 1 (wire offset value 1).
        let pattern: &[u8] = &[
            0xa5, 0x5a, 0xc3, 0x3c, 0x99, 0x66, 0xf0, 0x0d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x91, 0x82, 0x73, 0x64,
        ];
        let mut data = Vec::new();
        for i in 0..40 {
            data.extend_from_slice(pattern);
            // Constant-length, varying separators keep one pending literal in
            // front of each repeat and hold the period stable.
            data.push(0xf0 ^ i as u8);
        }
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(crate::Level::Fastest);
        driver.block_tail()[..data.len()].copy_from_slice(&data);
        driver.commit_block(data.len());
        let mut repcodes = 0usize;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq
                && offset <= 3
            {
                repcodes += 1;
            }
        });
        assert!(
            repcodes > 0,
            "repeated structure must produce repcode matches"
        );
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    /// A multithreaded job's first parsed block runs with adopted history
    /// below it (`prefill_window` + `adopt_window` + `set_block`) and the
    /// repcode gate armed; Ultra's strip-tail statistics seeding must not
    /// corrupt the emitted stream or the offset history.
    #[test]
    fn ultra_job_boundary_reconstructs() {
        let mut data = Vec::with_capacity(300 * 1024);
        let words = [
            &b"the quick brown fox "[..],
            &b"jumps over the lazy dog "[..],
            &b"lorem ipsum dolor sit amet "[..],
            b"\x00\x01\x02\x03 structured noise ",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        while data.len() < 300 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(words[(state as usize) % words.len()]);
        }
        let start = 192 * 1024;
        let run = || {
            let mut driver = MatchGeneratorDriver::new_direct();
            driver.reset(crate::Level::Ultra);
            driver.gate_repcodes();
            driver.prefill_window(&data[..start], 0);
            driver.adopt_window(&data, 0);
            driver.set_block(start as u64, data.len() as u64);
            let mut rep = [1u32, 4, 8];
            let mut reconstructed = data[..start].to_vec();
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    let from = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[from + i];
                        reconstructed.push(b);
                    }
                },
            });
            reconstructed
        };
        assert_eq!(run(), data);
        assert_eq!(run(), data, "job parse must be deterministic");
    }

    /// The cold-start DUBT head (row 9) must roundtrip a frame that spans
    /// the head, the handoff's dense re-index, and the chain tail — on a
    /// wide alphabet (head runs) and on a small one (the alphabet gate
    /// keeps the chain pure) — and stay deterministic.
    #[test]
    fn dubt_head_roundtrips() {
        let block = 128 * 1024;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // Wide alphabet: text-like words over > HEAD_SYMS_MIN symbols, with
        // a repeated sentence to give the head real matches.
        let mut wide = Vec::with_capacity(5 << 20);
        let sentence = b"the cold-start head parses this sentence over and over; ";
        while wide.len() < 5 << 20 {
            wide.extend_from_slice(sentence);
            for _ in 0..8 {
                wide.push(b' ' + (rand() % 64) as u8);
            }
        }
        // Narrow alphabet: 16 symbols, above HEAD_MIN_TOTAL so only the
        // alphabet gate can keep the chain pure.
        let narrow: Vec<u8> = (0..5 << 20).map(|_| (rand() & 15) as u8).collect();
        for (data, expect_head) in [(&wide[..], true), (&narrow[..], false)] {
            let run = || {
                let mut driver = MatchGeneratorDriver::new(block);
                driver.reset(crate::Level::from_zstd(9));
                let mut rep = [1u32, 4, 8];
                let mut reconstructed = Vec::new();
                for chunk in data.chunks(block) {
                    driver.block_tail()[..chunk.len()].copy_from_slice(chunk);
                    driver.commit_block(chunk.len());
                    driver.start_matching(|seq| match seq {
                        Sequence::Literals { literals } => {
                            reconstructed.extend_from_slice(literals)
                        },
                        Sequence::Triple {
                            literals,
                            offset,
                            match_len,
                        } => {
                            reconstructed.extend_from_slice(literals);
                            let actual = crate::decoding::sequence_execution::do_offset_history(
                                offset as u32,
                                literals.len() as u32,
                                &mut rep,
                            );
                            let from = reconstructed.len() - actual as usize;
                            for i in 0..match_len {
                                let b = reconstructed[from + i];
                                reconstructed.push(b);
                            }
                        },
                    });
                }
                (reconstructed, driver.dubt_head)
            };
            let (first, phase) = run();
            assert_eq!(
                first, *data,
                "roundtrip failed (head expected {expect_head})"
            );
            assert_eq!(
                phase,
                if expect_head {
                    super::HeadPhase::Done
                } else {
                    super::HeadPhase::Off
                },
                "head lifecycle (head expected {expect_head})"
            );
            assert_eq!(run().0, *data, "parse must be deterministic");
        }
    }

    /// A gated (incompressible) block crossing `HEAD_LIMIT` must still
    /// hand the head off to the chain: the handoff rides the first
    /// dispatched block past the span, and the head region is
    /// dense-indexed before the chain probes.
    #[test]
    fn dubt_head_hands_off_across_gated_block() {
        let block = 128 * 1024;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut text = Vec::new();
        let sentence = b"the gated block must not swallow the handoff; ";
        while text.len() < block * 8 {
            text.extend_from_slice(sentence);
            // Wide-alphabet filler so the head's alphabet gate accepts.
            for _ in 0..sentence.len() {
                text.push(b' ' + (rand() % 64) as u8);
            }
        }
        text.truncate(block * 8);
        let mut random_block = Vec::with_capacity(block);
        for _ in 0..block {
            random_block.push((rand() >> 32) as u8);
        }
        let mut data = text.clone();
        data.extend_from_slice(&random_block);
        // The tail repeats the head region: post-handoff chain blocks must
        // find matches reaching back into it.
        data.extend_from_slice(&text);
        data.extend_from_slice(&text);

        let mut driver = MatchGeneratorDriver::new(block);
        driver.reset(crate::Level::from_zstd(9));
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for chunk in data.chunks(block) {
            driver.block_tail()[..chunk.len()].copy_from_slice(chunk);
            driver.commit_block(chunk.len());
            if driver.skip_if_incompressible() {
                assert!(
                    reconstructed.len() >= block * 8 && reconstructed.len() < block * 9 + block,
                    "only the random block may gate"
                );
                reconstructed.extend_from_slice(chunk);
                continue;
            }
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    let from = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[from + i];
                        reconstructed.push(b);
                    }
                },
            });
        }
        assert_eq!(reconstructed, data);
        assert_eq!(driver.dubt_head, super::HeadPhase::Done);
    }
}
