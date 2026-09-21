//! Per-level search parameters and the level ladder: one [`LevelParams`]
//! row per numeric level, plus the LDM/dubt-head arming policy the rows
//! feed into. Split out of [`super`] so the driver's table machinery does
//! not carry the strategy tables.

use super::{HASH_LOG, MAX_WINDOW, MIN_MATCH};
#[cfg(feature = "std")]
use super::{HASH_READ, read8};
use crate::{InputShape, Level, encoding::opt::OptKnobs};

/// Which search loop the matcher runs. The `chain` buffer is the second
/// table for the two-table strategies and empty for [`Strategy::Fast`].
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Strategy {
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
    /// Tagged row matcher (libzstd's `ZSTD_RowFindBestMatch`): the head
    /// table is an array of rows of `1 << payload` u32 entries, each with
    /// a parallel tag byte per slot (the tag row's byte 0 doubles as the
    /// insertion head, cycling backwards). Candidates live in one cache
    /// line per row instead of a chain-link chase, so the search iterates
    /// the row's tag matches newest-first with no dependent loads between
    /// them. Rows 5-12.
    Row(u32),
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
pub(super) struct LevelParams {
    pub(super) hash_log: u32,
    /// Match window; also the window declared in the frame header.
    pub(super) window: usize,
    pub(super) strategy: Strategy,
    /// Chain-family searches per position (libzstd's `1 << searchLog`).
    pub(super) search_depth: u32,
    /// 0 = greedy, 1 = lazy, 2 = lazy2 (libzstd's deferred-position count).
    pub(super) lazy_depth: u32,
    /// Shortest hash-chain match worth emitting; libzstd's searchLength.
    /// Repcode matches stay legal from MIN_MATCH up.
    pub(super) min_match: u32,
    /// Parse the frame's cold-start head through the btlazy2 driver before
    /// this row's chain takes over (see [`HEAD_LIMIT`]).
    pub(super) dubt_head: bool,
    /// Run the gear-hash long-distance matcher as an extra candidate
    /// source for the chain scan (see [`super::ldm`]).
    pub(super) ldm: bool,
    /// Chain search-domain override (the walk's reach), for rows whose
    /// far distance classes ride LDM candidates instead of the window:
    /// the window (frame header, buffer, LDM reach) stays the row's
    /// `window`; `None` keeps search domain == window.
    pub(super) chain_reach: Option<usize>,
    /// Known-declared source at or below libzstd's small-input cParams
    /// boundary (srcSize <= 128 KiB): the small-input policy band, where
    /// row switch shortens accepted matches and slows the miss ramp
    /// (see the parse loops). Never set for unknown-length streams.
    pub(super) small_src: bool,
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
        small_src: false,
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
        small_src: false,
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
        small_src: false,
    }
}

/// A [`Strategy::Row`] row: `search_depth` is the per-search candidate
/// budget (libzstd's `1 << min(searchLog, rowLog)` attempts).
const fn row(
    hash_log: u32,
    row_log: u32,
    window: usize,
    search_depth: u32,
    lazy_depth: u32,
) -> LevelParams {
    LevelParams {
        hash_log,
        window,
        strategy: Strategy::Row(row_log),
        search_depth,
        lazy_depth,
        min_match: 5,
        dubt_head: false,
        ldm: false,
        chain_reach: None,
        small_src: false,
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
        small_src: false,
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
        small_src: false,
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
pub(super) const HEAD_LIMIT: u64 = 9 * crate::common::MAX_BLOCK_SIZE as u64;
/// Frames shorter than this keep the pure chain parse: a head would cover
/// the whole input (small payloads are all cold start, and json-class
/// smalls favor the chain's selection).
pub(super) const HEAD_MIN_TOTAL: u64 = 4 << 20;
/// Distinct bytes required in the first parsed block for the head to run:
/// on small-symbol alphabets the tree's batch sort collapses at any search
/// depth (skewed), while the chain's selection there has no deficit.
pub(super) const HEAD_SYMS_MIN: u32 = 48;
/// Head-table log (the heads table the btlazy bridge sizes from
/// `dubt_table`); the ring and search knobs ride [`HEAD_KNOBS`].
pub(super) const HEAD_HASH_LOG: u32 = 20;
/// Head knobs: S4 search (libzstd's L13 depth — shallower trees accept
/// nearer-shorter candidates again, S1-S3 measured at -1.3 to -4.4% vs
/// zstd-9) over a 1 MiB ring — head candidates never sit further back
/// than the head span itself.
pub(super) const HEAD_KNOBS: OptKnobs = bt_knobs(4, 20);

/// Quiet blocks before the LDM generation latch shuts off: a winless
/// megabyte is a shape whose far class either does not exist or never
/// wins — the split pass (gear + xxh over every 64-byte window, ~9 cyc/B
/// measured) is pure tax from there on. Any win resets the latch; the
/// false-kill risk (a far duplicate isolated by winless content on both
/// sides) is bounded by the canary revival below.
pub(super) const LDM_QUIET: u8 = 16;

/// Blocks between canary generations while latched off: one block per
/// megabyte re-runs the split pass, and a canary whose beyond-reach
/// candidates exist revives the matcher — far-match density can ramp up
/// anywhere in the file, and the quiet stretch that latched cannot prove
/// the class absent beyond what it scanned.
pub(super) const LDM_CANARY: u8 = 8;

/// Smallest source-downsized window that arms LDM under
/// [`LdmArming::Frame`]: one step under the row's W26, so a 32 MiB source
/// (clamped to W25) still arms — the dll32-class far classes measured
/// −19.7% size there — while the quiet latch bounds the discovery cost the
/// same-size far-less shapes pay.
pub(super) const LDM_MIDSIZE_WINDOW: usize = 1 << 25;

/// The [`LdmArming::Job`] bar: the row's full window, meaning the source
/// clamp left it intact.
pub(super) const LDM_FULL_WINDOW: usize = 1 << 26;

/// Sampled distinct-byte count the mid-size population's first parsed
/// block must show to stay armed: low alphabets (skewed's 16 symbols,
/// text's ~100 ASCII) produce structural 64-byte repeats whose far twins
/// never survive the price of their offset — candidates exist and keep
/// the latch alive, but no sequence ever emits, so the split pass runs at
/// near-always-on duty (skewed measured −35% solo). Binary content (dll,
/// 256) clears it by a wide margin. The full-window population skips the
/// check (its bytes are frozen).
pub(super) const LDM_SYMS_MIN: u32 = 128;

/// Why a block the scan will not parse reaches the LDM indexer.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum LdmFill {
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
pub(super) fn sampled_distinct(win: &[u8], start: usize, end: usize) -> u32 {
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

/// The bulk-mt prefix-LDM engagement screen over the frame's first block:
/// a wide alphabet (the mid-size population's own gate, [`LDM_SYMS_MIN`])
/// plus one strided 8-byte repeat — the same evidence class
/// `skip_if_incompressible`'s probe collects, reduced to its hit
/// predicate (a collision anywhere keeps the whole block matchable
/// there). Binary heads hit within the first few thousand samples;
/// max-entropy heads never hit, so the strip fill's unconditional split
/// pass is not bought for frames whose blocks go raw.
#[cfg(feature = "std")]
pub(crate) fn ldm_head_parses(head: &[u8]) -> bool {
    if sampled_distinct(head, 0, head.len()) < LDM_SYMS_MIN {
        return false;
    }
    let stride = (head.len() >> 11) | 1;
    // Slot count one below the block gate's probe: an engagement screen,
    // not a byte-affecting decision — the 47-bit content hash keeps
    // false hits at ~1e-7 per screen.
    let mut probe = [0u32; 1 << 12];
    let mut i = 0usize;
    while i + HASH_READ <= head.len() {
        let h = read8(head, i).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
        let slot = (h >> 52) as usize;
        let tag = h as u32;
        if probe[slot] == tag {
            return true;
        }
        probe[slot] = tag;
        i += stride;
    }
    false
}

/// Far-repeat veto for the streaming strip cap (see `encoder_mt`'s
/// `FarClass`): whether far-lag hash collisions dominate the span's
/// strided samples — content whose matches come from beyond the row's
/// search domain (tiling or templating at periods past the reach). The
/// slot always keeps its latest sample, so a uniform span self-collides
/// at the stride only (near, never counted) and an incidental
/// long-distance 8-gram whose slot a nearer sample reclaims stays
/// unseen; leakage from near-periodic shapes (an 8-gram whose nearer twin
/// the stride missed) measures a few percent of samples, so the dominance
/// bar sits well above it while any paying far class covers a
/// double-digit fraction.
#[cfg(feature = "std")]
pub(crate) fn far_repeat_dominant(win: &[u8], reach: usize) -> bool {
    const SLOTS: usize = 1 << 17;
    const STRIDE: usize = 4 * 1024;
    // Far-dominant: hits reach one twelfth of the samples (~8%).
    const DOMINANCE_DEN: u32 = 12;
    let far = (reach / STRIDE).max(1);
    let mut probe = alloc::vec![(0u32, 0u32); SLOTS];
    let mut hits = 0u32;
    let mut samples = 0u32;
    let mut i = 0usize;
    while i + HASH_READ <= win.len() {
        let h = read8(win, i).wrapping_mul(0xcf1b_bcdc_b7a5_6463);
        let slot = ((h >> 45) as usize) & (SLOTS - 1);
        let tag = h as u32;
        let (prev_tag, prev_idx) = probe[slot];
        if prev_tag == tag && samples.saturating_sub(prev_idx) >= far as u32 {
            hits += 1;
        }
        probe[slot] = (tag, samples);
        i += STRIDE;
        samples += 1;
    }
    hits * DOMINANCE_DEN >= samples
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
    /// Constructed by the std-gated mt paths only.
    #[cfg(feature = "std")]
    Job,
    /// A multithreaded job whose strip reaches back through the frame's
    /// own prefix (the mt paths' mid-size LDM capture): the strip content
    /// is exactly what the frame-continuous path would have indexed by the
    /// same position, so the mid-size bar arms and the far class survives
    /// the job split — the per-job fill tax the `Job` bar guards against is
    /// paid once by the shared build instead of per job. Also the donated
    /// reach-probe span's arming when the frame's Keep verdict would
    /// engage the capture (the donated span must parse as the kept frame's
    /// job zero would — see `mt::donation_arming` and the stream core's
    /// `post_donation`). Constructed by the std-gated mt paths only.
    #[cfg(feature = "std")]
    JobPrefix,
    /// A streaming job on a far-dead frame (both far-class screens
    /// rejected it, see `encoder_mt`'s `FarClass`): every job's strip
    /// caps at the row's chain reach, so no beyond-reach history exists
    /// to source a far candidate — LDM never arms. Constructed by the
    /// std-gated stream mt path only.
    #[cfg(feature = "std")]
    FarDead,
    /// The reach probe's parses: the keep parse's span sits below the
    /// row's chain reach, so no candidate can survive the beyond-reach
    /// filter there (the shrink parse never arms at all — a shrunk parse
    /// abandons LDM, see the size gate). Skipping the split pass is
    /// byte-exact either way: it changes no sequence the probe could cost.
    ProbeKeep,
}

/// The size gate's arming bar: the smallest (post source-clamp) window
/// that arms LDM under `arming`, or `None` to never arm.
pub(super) const fn ldm_min_window(arming: LdmArming) -> Option<usize> {
    match arming {
        LdmArming::Frame => Some(LDM_MIDSIZE_WINDOW),
        #[cfg(feature = "std")]
        LdmArming::Job => Some(LDM_FULL_WINDOW),
        #[cfg(feature = "std")]
        LdmArming::JobPrefix => Some(LDM_MIDSIZE_WINDOW),
        #[cfg(feature = "std")]
        LdmArming::FarDead => None,
        LdmArming::ProbeKeep => None,
    }
}

/// Lifecycle of the cold-start DUBT head (row 9, see [`HEAD_LIMIT`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum HeadPhase {
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

/// Span of the frame head the btlazy2 rows parse at libzstd's exact
/// probe step ([`LazyStep::Dense`]): text.best's measured deficit sat
/// entirely inside the first 448 KiB (4 blocks covers it with margin),
/// and beyond it the steady ramp's skipping is the measured win on
/// json/skewed-class shapes.
pub(super) const BT_DENSE_LIMIT: u64 = 4 * crate::common::MAX_BLOCK_SIZE as u64;

/// Lifecycle of the btlazy2 rows' cold-head probe step (the Best tier's
/// counterpart of [`HeadPhase`], which stays row-9-specific): the frame's
/// first searching block inside [`BT_DENSE_LIMIT`] evaluates the
/// [`HEAD_SYMS_MIN`] alphabet gate — json-class heads (39 symbols) and
/// low alphabets keep the steady ramp, so their bytes stay identical —
/// then the head blocks parse dense until the limit. Strip- or
/// dictionary-warm starts never arm (a prefilled job is not a cold
/// head), matching the row-9 head's arming rule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum BtStepPhase {
    /// Waiting for the frame's first searching block.
    Armed,
    /// Gate passed; head blocks parse at libzstd's probe step.
    Dense,
    /// Gate failed, scope exhausted, or a warm start: steady ramp.
    Off,
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
pub(super) const LEVEL_PARAMS: [LevelParams; 23] = [
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
    // Lazy band 5-12: the tagged row matcher (libzstd's own storage for
    // these rows). 5-8 (libzstd's L5-8: H19/H19/H20/H20, S3/S3/S4/S4,
    // greedy/lazy/lazy/lazy2) run 8/8/15/15 attempts at libzstd parity
    // — halving l7/l8 to 8 measured -15% json time for +1.2pp text and
    // +1.1pp dll32 size, a trade the tier's ratio identity refuses.
    // 10-12 (libzstd's L10-12: H22/H22/H23, S5/S6/S6, lazy2, TL16/16/32)
    // take rowLog BOUNDED(4, searchLog, 6) = 5/6/6 at 31/63/63 attempts
    // — the old chain rows ran depth 16/32/32, half libzstd's
    // searchLog by the old Balanced calibration. The 2026-09-21 two-axis
    // calibration kept parity: halving to 16/31/31 buys json x1.65->1.36
    // and text x0.52->0.48 but costs size on every column (+0.4-0.6%
    // json, +0.5-0.8% text, +0.1-0.15% dll32, +0.2-0.3% dll100);
    // rowLog 5 on 11/12 and L12 at H22 both lose size and collapse the
    // rungs onto their neighbors. The chain's link chase is latency-bound
    // (~66 cyc/step over a 4-6 MiB two-table working set); a row keeps its
    // 1<<rowLog
    // newest same-bucket candidates inside rowLog 5-6's 2-4 cache lines,
    // trading unbounded chain depth for scan-locality. Selection
    // semantics (literal-aware lazy walk, store gate, miss ramp) stay the
    // chain's.
    row(19, 4, 1 << 21, 8, 0),
    row(19, 4, 1 << 21, 8, 1),
    row(20, 4, 1 << 21, 16, 1),
    row(20, 4, 1 << 21, 16, 2),
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
    row(22, 5, 1 << 22, 32, 2),
    row(22, 6, 1 << 22, 64, 2),
    row(23, 6, 1 << 22, 64, 2),
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

/// Upper bound of the small-input policy band: libzstd's small-src
/// cParams table boundary (srcSize <= 128 KiB).
pub(crate) const SMALL_SRC_MAX: u64 = 128 * 1024;

/// [`params_for_level`] adjusted to what the caller declared about the
/// input: a forced window log overrides the row's window first, then a
/// known length downsizes (libzstd's override-then-adjust order, so a
/// known smaller length still clamps a forced window). Raw-block frames
/// keep the row untouched (their declared window never affects bytes, and
/// staying shape-independent keeps outputs stable).
pub(super) fn params_for(level: Level, shape: InputShape) -> LevelParams {
    let mut p = params_for_level(level);
    if let Some(wl) = shape.window_log {
        p.window = 1usize << wl.clamp(10, 27);
    }
    p.small_src = matches!(shape.len, Some(n) if n <= SMALL_SRC_MAX);
    // Small-input policy for the balanced tier (libzstd's <= 16 KiB table
    // swaps its level 9-10 lazy2 rows to btlazy2, searchLog 5): the
    // chain's newest-first selection accepts nearer-shorter candidates
    // where the tiny window leaves the tree's full coverage cheap, and
    // the DUBT parse measured strictly smaller on every class (text 16
    // KiB -282 B vs the chain, json -107, skewed +9).
    if p.small_src && matches!(level.as_i32(), 9 | 10) {
        let mut p = adjust_params(btlazy(17, 1 << 17, bt_knobs(5, 16)), shape.len);
        // The replacement row's constructor defaults the flag to
        // false; the frame IS small — the downstream small-src gates
        // (the btlazy tree's hash width screen) read it from the
        // adopted row, not from this branch.
        p.small_src = true;
        return p;
    }
    // Small-input policy (libzstd's small-src rows run searchLength 4 at
    // every lazy-family level): the row matcher's default 5 prices
    // literals against long-run entropy; a <= 128 KiB block accepts the
    // shorter matches its tiny window makes common.
    if p.small_src && matches!(p.strategy, Strategy::Row(_)) {
        p.min_match = 4;
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
        // The row layout is size-invariant: rel_row + entries lands below
        // 2^hash_log for any row_log (the hash width shrinks with it), so
        // only a degenerate row_log at or above hash_log needs the clamp.
        Strategy::Row(rl) => Strategy::Row(rl.min(wlog)),
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
