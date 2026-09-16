//! Shape-adaptive chain reach for the Balanced row (row 9): a prefix
//! dual-parse probe that decides the frame's chain search domain from its
//! first bytes.
//!
//! The row's stock reach (W22) serves frames whose repeats ride far
//! distances (the dll class); shapes whose repeats are near-local — json
//! logs, tiled structured data — pay for it in both axes at once: the
//! far-walking chain takes 2x the time and its far matches displace nearer
//! ones whose repcode history the following positions then reuse, a net
//! ratio loss (json at reach W12 measures 10% smaller and 2.3x faster than
//! W22). No static reach serves both classes, so the frame decides for
//! itself: before any block is matched, the first [`PROBE_SPAN`] bytes are
//! parsed twice through the row's own pipeline (DUBT head, LDM, chain)
//! with only the reach differing, and the cheaper parse wins the frame.
//!
//! The probe measures a pre-entropy cost (order-0 literal entropy plus a
//! fixed per-sequence code estimate and the packed add bits — repcode
//! offsets pay ~0 add bits, far matches pay their ~22), which is enough
//! separation: the classes differ by 8% on the probe span, the noise floor
//! (text, skewed) sits at 0%.
//!
//! Engagement is deliberately narrow: only the Balanced row with its stock
//! reach unclamped by the frame's shape, only frames a caller declared at
//! least [`PROBE_MIN_FRAME`] bytes (the probe's two parses are a fixed
//! ~30 ms tax; below that size they outrun their savings), and only entry
//! points that see the frame's head before the first block is matched:
//! bulk single- and multithreaded and the multithreaded stream core — all
//! donating (the keep side runs as the frame's/job zero's own blocks; see
//! `mt::donate_keep_span`), with the stream core staging the head on a pool
//! worker at its gate (a pledge gates at construction, an open-ended
//! stream at [`PROBE_MIN_FRAME`] streamed bytes; a stream that ends or
//! flushes before the gate keeps the stock reach). The single-threaded
//! stream cores keep the stock throwaway probe or none (unpledged blocks
//! flow at block size — staging a head there withholds output from
//! small-pull consumers); dictionary frames probe nothing (their history
//! needs the reach it has).

use alloc::{boxed::Box, vec::Vec};
#[cfg(feature = "std")]
use core::cell::RefCell;

use super::{
    Matcher, SeqWord,
    match_generator::{LdmArming, MatchGeneratorDriver},
    util,
};
use crate::{InputShape, Level};

/// The row's stock chain reach (W22): the probe's keep side.
pub(crate) const KEEP_REACH: usize = 1 << 22;
/// The probe's shrink side: near-local shapes measured optimal at W12.
pub(crate) const SHRINK_REACH: usize = 1 << 12;
/// Head bytes each probe parse covers: past the DUBT head's span (9 blocks)
/// so the chain phase dominates, small enough to stay a minority cost of
/// any frame that probes.
pub(crate) const PROBE_SPAN: usize = 2 * 1024 * 1024;
/// Smallest declared frame that probes; below it the two probe parses
/// would outrun the reach's own savings.
pub(crate) const PROBE_MIN_FRAME: u64 = 8 * 1024 * 1024;
/// Shrink needs this much of an edge (percent of the keep parse's cost);
/// the noise floor measures 0% and json ~8%, so anything in between works.
const SHRINK_MARGIN_PCT: f64 = 2.0;
/// Fixed code cost per sequence (three FSE symbols at ~5 bits each); the
/// add bits ride the packed word.
const SEQ_CODE_BITS: u64 = 15;

/// The frame's reach decision, made once per frame before the first block.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(crate) enum ReachChoice {
    /// The row's stock reach.
    #[default]
    Keep,
    /// The shrunk domain (near-local shapes).
    Shrink,
}

/// Running reach-probe cost accumulated off a real parse (donation mode):
/// the frame's own keep-side blocks absorb their literals and sequences as
/// they run, so the keep measurement is the executed parse itself —
/// feedback, LDM and the incompressibility gate included — instead of a
/// separate re-parse without them.
pub(crate) struct ProbeStats {
    lit_hist: [u64; 256],
    seq_bits: u64,
}

impl Default for ProbeStats {
    fn default() -> Self {
        ProbeStats {
            lit_hist: [0; 256],
            seq_bits: 0,
        }
    }
}

impl ProbeStats {
    pub(crate) fn absorb_literals(&mut self, lits: &[u8]) {
        let hist = byte_hist(lits);
        for (slot, c) in self.lit_hist.iter_mut().zip(hist) {
            *slot += c as u64;
        }
    }

    pub(crate) fn absorb_seqs(&mut self, seqs: &[SeqWord]) {
        for w in seqs {
            self.seq_bits += SEQ_CODE_BITS + w.add_nb as u64;
        }
    }

    /// Pre-entropy cost in bits (order-0 literals plus sequence codes).
    pub(crate) fn cost_bits(&self) -> f64 {
        let total: u64 = self.lit_hist.iter().sum();
        order0_bits(&self.lit_hist, total) + self.seq_bits as f64
    }
}

/// How a probe parse prices literals in its store gate: the throwaway
/// parses run feedback-free (flat lengths, like a frame's first block),
/// while a parse measured against a donated keep side needs the entropy
/// feedback the real pipeline enjoys — otherwise the comparison is biased
/// toward the donated side by the whole feedback gain (json's keep side
/// measured 4.75% from it, enough to flip its verdict).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ProbeFeedback {
    /// Flat 4-bit lengths: the stock probe's model on both sides.
    Flat,
    /// Approximate feedback: after each parsed block, the next block's
    /// gate prices literals at Shannon lengths derived from the block's
    /// own literal histogram (absent symbols at the 11-bit cap) — the
    /// same per-block structure as the encoder's table feedback.
    Approx,
}

/// Four-lane byte histogram (breaking the store-forward chain, the
/// incompressibility gate's exact-pass pattern): the model side reads
/// every literal byte once per parse, which on literal-heavy shapes is
/// comparable to the parse itself.
fn byte_hist(lits: &[u8]) -> [u32; 256] {
    let mut lanes = [[0u32; 256]; 4];
    let (chunks, remainder) = lits.as_chunks::<4>();
    for chunk in chunks {
        lanes[0][chunk[0] as usize] += 1;
        lanes[1][chunk[1] as usize] += 1;
        lanes[2][chunk[2] as usize] += 1;
        lanes[3][chunk[3] as usize] += 1;
    }
    for &b in remainder {
        lanes[0][b as usize] += 1;
    }
    let mut hist = [0u32; 256];
    for i in 0..256 {
        hist[i] = lanes[0][i] + lanes[1][i] + lanes[2][i] + lanes[3][i];
    }
    hist
}

/// `f64::log2` is std-only; no_std builds use an atanh-series form
/// (relative error < 1e-7 — the verdict margins are percent-scale, so the
/// two forms decide identically). std keeps the libm call bit-for-bit.
#[cfg(feature = "std")]
#[inline(always)]
fn f64_log2(x: f64) -> f64 {
    x.log2()
}

#[cfg(not(feature = "std"))]
fn f64_log2(x: f64) -> f64 {
    if x <= 0.0 {
        return f64::NAN;
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let m = if biased == 0 {
        // Subnormal: m in [0,1) against the -1022 exponent.
        -1022.0
    } else {
        (biased - 1023) as f64
    };
    let mant = 1.0 + (bits & ((1u64 << 52) - 1)) as f64 / (1u64 << 52) as f64;
    let z = (mant - 1.0) / (mant + 1.0);
    let z2 = z * z;
    let mut ln = z;
    let mut zk = z;
    for k in (3..=15).step_by(2) {
        zk *= z2;
        ln += zk / k as f64;
    }
    m + 2.0 * ln / core::f64::consts::LN_2
}

/// `f64::round` is std-only; the caller's domain is positive (the clamp
/// band [1, 11]), where `+0.5` then truncate equals round exactly.
#[cfg(feature = "std")]
#[inline(always)]
fn f64_round(x: f64) -> f64 {
    x.round()
}

#[cfg(not(feature = "std"))]
#[inline(always)]
fn f64_round(x: f64) -> f64 {
    (x + 0.5) as i64 as f64
}

/// Code lengths for a parsed block's literals: `-log2(count/total)`
/// clamped into the format's [1, 11] band, absent symbols capped the way
/// [`Matcher::note_literal_costs`] maps them.
fn approx_lit_lens(lits: &[u8], out: &mut [u8; 256]) {
    let hist = byte_hist(lits);
    let total = lits.len() as f64;
    for i in 0..256 {
        let c = hist[i];
        out[i] = if c == 0 {
            11
        } else {
            f64_round((-f64_log2(c as f64 / total)).clamp(1.0, 11.0)) as u8
        };
    }
}

/// The probe's verdict from two measured costs: shrink needs the margin
/// (the noise floor measures 0% and json ~8%).
pub(crate) fn decide(keep: f64, shrink: f64) -> ReachChoice {
    if shrink * 100.0 <= keep * (100.0 - SHRINK_MARGIN_PCT) {
        ReachChoice::Shrink
    } else {
        ReachChoice::Keep
    }
}

/// Prefix the donated path measures its feedback gain on (whole blocks;
/// the first two blocks of any parse are near-identical, the gain needs a
/// few feedback rounds to show).
const FEEDBACK_PROBE: usize = 512 * 1024;

/// Landslide keep shortcut: a feedback-free shrink cost this far above the
/// donated (true) keep cost keeps without measuring the feedback gain —
/// only a keep side gaining >~20% from feedback (json, the most
/// feedback-sensitive shape on record, gains 5%) could still bring the
/// flat comparison inside the margin from there.
const KEEP_LANDSLIDE: f64 = 1.25;

/// The donated path's verdict. Its keep cost is the TRUE one (the frame's
/// own parse, entropy feedback included) while the shrink side parses
/// feedback-free — an asymmetry worth 4.75% on json's keep side, more
/// than double the margin: the flat comparison the margin is calibrated
/// on must be reconstructed. A shrink that already wins against the
/// true keep cost is contained in the flat verdict (feedback only ever
/// lowered measured keep costs); a landslide keeps outright; the
/// contested band converts the true keep cost back to the feedback-free
/// scale through the gain measured on a cheap twice-parsed prefix.
/// Float equality is the point in the tie shortcut: it fires on
/// bit-identical model costs (identical parses), not near-equal ones.
#[allow(clippy::float_cmp)]
pub(crate) fn decide_donated(
    keep: f64,
    shrink: f64,
    head: &[u8],
    level: Level,
    shape: InputShape,
) -> ReachChoice {
    let mut driver = probe_driver();
    let choice = decide_donated_with(&mut driver, keep, shrink, head, level, shape);
    return_probe_driver(driver);
    choice
}

/// [`decide_donated`] on a caller-supplied pooled driver (see
/// [`parse_cost_with`]).
pub(crate) fn decide_donated_with(
    probe: &mut MatchGeneratorDriver,
    keep: f64,
    shrink: f64,
    head: &[u8],
    level: Level,
    shape: InputShape,
) -> ReachChoice {
    if shrink == keep {
        // Identical costs mean identical parses under the model — the
        // shape is reach- and feedback-degenerate (skewed, random, zeros
        // all land here bit-for-bit, including the all-uniform 0 == 0), so
        // the flat verdict keeps and the escalation below could not
        // measure anything.
        return ReachChoice::Keep;
    }
    if decide(keep, shrink) == ReachChoice::Shrink {
        // A shrink this far ahead of even the true (feedback) keep cost is
        // contained in the flat verdict: feedback only ever lowered
        // measured keep costs.
        return ReachChoice::Shrink;
    }
    if shrink >= keep * KEEP_LANDSLIDE {
        return ReachChoice::Keep;
    }
    let gain = feedback_gain_with(probe, head, level, shape).clamp(0.0, 0.5);
    let keep_flat = keep / (1.0 - gain);
    decide(keep_flat, shrink)
}

/// The keep-side parse's entropy-feedback gain, estimated on a prefix:
/// the relative model-cost drop between a feedback-free parse and one
/// with approximate per-block literal pricing (the same structure the
/// encoder's table feedback gives the real parse).
fn feedback_gain_with(
    probe: &mut MatchGeneratorDriver,
    head: &[u8],
    level: Level,
    shape: InputShape,
) -> f64 {
    let end = FEEDBACK_PROBE.min(head.len());
    let prefix = &head[..end];
    let flat = parse_cost_with(
        probe,
        prefix,
        level,
        shape,
        ReachChoice::Keep,
        ProbeFeedback::Flat,
    );
    let approx = parse_cost_with(
        probe,
        prefix,
        level,
        shape,
        ReachChoice::Keep,
        ProbeFeedback::Approx,
    );
    if flat <= 0.0 {
        0.0
    } else {
        (flat - approx) / flat
    }
}

/// Whether a frame at `level`/`shape` is probe-eligible: only the Balanced
/// row with its stock reach still in place (a shape-clamped reach means the
/// window itself is small — nothing to trade away) and a declared length
/// that clears the probe's fixed cost.
pub(crate) fn eligible(level: Level, shape: InputShape) -> bool {
    shape.len.is_some_and(|n| n >= PROBE_MIN_FRAME)
        && MatchGeneratorDriver::reach_probe_eligible(level, shape)
}

/// Decide a frame's reach from its first [`PROBE_SPAN`] bytes: parse the
/// head twice through the row's own pipeline with only the reach differing
/// and keep the cheaper parse. `head` shorter than the span, or an
/// ineligible frame, keeps the stock reach.
pub(crate) fn probe_reach_choice(head: &[u8], level: Level, shape: InputShape) -> ReachChoice {
    if !eligible(level, shape) {
        return ReachChoice::Keep;
    }
    probe_staged(head, level, shape)
}

/// [`probe_reach_choice`] on a frame the caller already size-gated: the
/// stream-mt core stages its own head (a pledge clears the size gate at
/// construction, an open-ended stream at [`PROBE_MIN_FRAME`] streamed
/// bytes — see its notes), so only the span check remains. A head below
/// the span keeps the stock reach.
pub(crate) fn probe_staged(head: &[u8], level: Level, shape: InputShape) -> ReachChoice {
    if head.len() < PROBE_SPAN {
        return ReachChoice::Keep;
    }
    let head = &head[..PROBE_SPAN];
    let keep = parse_cost(head, level, shape, ReachChoice::Keep, ProbeFeedback::Flat);
    let shrink = parse_cost(head, level, shape, ReachChoice::Shrink, ProbeFeedback::Flat);
    decide(keep, shrink)
}

#[cfg(feature = "std")]
// Pooled probe driver: the row's tables (hash, chain, LDM) are
// reach-independent, so a steady-state thread reuses them across frames.
std::thread_local! {
    static PROBE_DRIVER: RefCell<Option<Box<MatchGeneratorDriver>>> =
        const { RefCell::new(None) };
}

/// Parse `head` at `choice` and return its pre-entropy cost in bits. The
/// block loop mirrors the bulk slice path exactly (per-block adopted
/// window, uniform/RLE skip and the incompressibility gate, head and LDM
/// riding the row's own machinery), so the two parses differ in nothing but
/// the reach. The donating entry points call this for the shrink side only
/// — their keep cost comes off the frame's own blocks (see `ProbeStats`).
pub(crate) fn parse_cost(
    head: &[u8],
    level: Level,
    shape: InputShape,
    choice: ReachChoice,
    feedback: ProbeFeedback,
) -> f64 {
    let mut driver = probe_driver();
    let cost = parse_cost_with(&mut driver, head, level, shape, choice, feedback);
    return_probe_driver(driver);
    cost
}

/// Take the calling thread's pooled probe driver (building one on a cold
/// thread); [`return_probe_driver`] puts it back. The take/return pair is
/// the thread-local wrapper's front half.
#[cfg(feature = "std")]
fn probe_driver() -> Box<MatchGeneratorDriver> {
    PROBE_DRIVER
        .with(|p| p.borrow_mut().take())
        .unwrap_or_else(|| Box::new(MatchGeneratorDriver::new_direct()))
}

#[cfg(feature = "std")]
fn return_probe_driver(driver: Box<MatchGeneratorDriver>) {
    PROBE_DRIVER.with(|p| *p.borrow_mut() = Some(driver));
}

#[cfg(not(feature = "std"))]
fn probe_driver() -> Box<MatchGeneratorDriver> {
    Box::new(MatchGeneratorDriver::new_direct())
}

#[cfg(not(feature = "std"))]
fn return_probe_driver(_driver: Box<MatchGeneratorDriver>) {}

/// [`parse_cost`] on a caller-supplied pooled driver: a donation running
/// on an ephemeral pool worker cannot warm the thread-local driver, so it
/// carries its own across encoders (see `mt::donate_span`).
pub(crate) fn parse_cost_with(
    driver: &mut MatchGeneratorDriver,
    head: &[u8],
    level: Level,
    shape: InputShape,
    choice: ReachChoice,
    feedback: ProbeFeedback,
) -> f64 {
    driver.set_input_shape(shape);
    driver.set_reach_choice(choice);
    // Neither probe parse runs LDM (see `LdmArming::ProbeKeep`): the keep
    // parse's span cannot surface a candidate past its reach, and the
    // shrink parse's configuration never arms — the executed shrunk parse
    // abandons LDM too, so the measurement stays exact.
    driver.set_ldm_arming(LdmArming::ProbeKeep);
    driver.reset(level);
    driver.clear_parse_tables();
    let block = driver.block_size();
    let max_window = driver.window_size();

    let mut literals = Vec::new();
    let mut seqs = Vec::new();
    let mut lit_hist = [0u64; 256];
    let mut seq_bits = 0u64;
    let mut fb_lens = [0u8; 256];
    let mut pos = 0usize;
    while pos < head.len() {
        let end = (pos + block).min(head.len());
        let hist = (pos as u64).saturating_sub(max_window) as usize;
        driver.adopt_window(&head[hist..end], hist as u64);
        driver.set_block(pos as u64, end as u64);
        if util::is_uniform(&head[pos..end]) {
            // The RLE path: the block contributes no literals or sequences.
            driver.skip_matching();
        } else if driver.skip_if_incompressible() {
            // The executed path's gate, missing here before: a gated block
            // is emitted raw, so the probe re-parsed max-entropy blocks the
            // frame never matches (random paid a full chain walk per gated
            // block, twice, to measure a cost the gate had already decided).
            // Its cost model is the block's own bytes as literals.
            for (i, c) in byte_hist(&head[pos..end]).into_iter().enumerate() {
                lit_hist[i] += c as u64;
            }
        } else {
            driver.start_matching_codes(&mut literals, &mut seqs);
            if seqs.is_empty() {
                // A zero-sequence block's literals are the block itself.
                for &b in &head[pos..end] {
                    lit_hist[b as usize] += 1;
                }
                if feedback == ProbeFeedback::Approx {
                    approx_lit_lens(&head[pos..end], &mut fb_lens);
                    driver.note_literal_costs(&fb_lens);
                }
            } else {
                for (i, c) in byte_hist(&literals).into_iter().enumerate() {
                    lit_hist[i] += c as u64;
                }
                for w in &seqs {
                    seq_bits += SEQ_CODE_BITS + w.add_nb as u64;
                }
                if feedback == ProbeFeedback::Approx {
                    approx_lit_lens(&literals, &mut fb_lens);
                    driver.note_literal_costs(&fb_lens);
                }
                literals.clear();
                seqs.clear();
            }
        }
        pos = end;
    }

    let lit_total: u64 = lit_hist.iter().sum();
    order0_bits(&lit_hist, lit_total) + seq_bits as f64
}

/// Order-0 entropy of the literal bytes in bits.
fn order0_bits(hist: &[u64; 256], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let mut bits = 0.0;
    for &c in hist {
        if c > 0 {
            let p = c as f64 / total as f64;
            bits -= p * f64_log2(p);
        }
    }
    bits * total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text-like near-local vocabulary: the shape the shrunk reach serves.
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

    #[test]
    fn ineligible_shapes_keep() {
        let data = textish(PROBE_SPAN);
        let small = InputShape::default().with_len(PROBE_SPAN as u64);
        assert_eq!(
            probe_reach_choice(&data, Level::from_zstd(9), small),
            ReachChoice::Keep
        );
        let unpledged = InputShape::default();
        assert_eq!(
            probe_reach_choice(&data, Level::from_zstd(9), unpledged),
            ReachChoice::Keep
        );
        let pledged = InputShape::default().with_len(16 * 1024 * 1024);
        // Not the Balanced row.
        assert_eq!(
            probe_reach_choice(&data, Level::from_zstd(10), pledged),
            ReachChoice::Keep
        );
        // A window log that clamps the reach below its stock value.
        let forced = pledged.with_window_log(20);
        assert_eq!(
            probe_reach_choice(&data, Level::from_zstd(9), forced),
            ReachChoice::Keep
        );
    }
}
