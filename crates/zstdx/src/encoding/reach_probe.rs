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
//! points that see the frame's head before the first block: bulk
//! single- and multithreaded, and pledged streaming. Unpledged streaming
//! and the multithreaded stream core keep the stock reach (their blocks
//! flow before any head is assembled); dictionary frames probe nothing
//! (their history needs the reach it has).

use alloc::{boxed::Box, vec::Vec};
use core::cell::RefCell;

use super::{Matcher, match_generator::MatchGeneratorDriver, util};
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
    if head.len() < PROBE_SPAN || !eligible(level, shape) {
        return ReachChoice::Keep;
    }
    let head = &head[..PROBE_SPAN];
    let keep = parse_cost(head, level, shape, ReachChoice::Keep);
    let shrink = parse_cost(head, level, shape, ReachChoice::Shrink);
    if shrink * 100.0 <= keep * (100.0 - SHRINK_MARGIN_PCT) {
        ReachChoice::Shrink
    } else {
        ReachChoice::Keep
    }
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
/// window, uniform/RLE skip, head and LDM riding the row's own machinery),
/// so the two parses differ in nothing but the reach.
fn parse_cost(head: &[u8], level: Level, shape: InputShape, choice: ReachChoice) -> f64 {
    #[cfg(feature = "std")]
    let mut driver = PROBE_DRIVER
        .with(|p| p.borrow_mut().take())
        .unwrap_or_else(|| Box::new(MatchGeneratorDriver::new_direct()));
    #[cfg(not(feature = "std"))]
    let mut driver = MatchGeneratorDriver::new_direct();

    driver.set_input_shape(shape);
    driver.set_reach_choice(choice);
    driver.reset(level);
    let block = driver.block_size();
    let max_window = driver.window_size();

    let mut literals = Vec::new();
    let mut seqs = Vec::new();
    let mut lit_hist = [0u64; 256];
    let mut seq_bits = 0u64;
    let mut pos = 0usize;
    while pos < head.len() {
        let end = (pos + block).min(head.len());
        let hist = (pos as u64).saturating_sub(max_window) as usize;
        driver.adopt_window(&head[hist..end], hist as u64);
        driver.set_block(pos as u64, end as u64);
        if util::is_uniform(&head[pos..end]) {
            // The RLE path: the block contributes no literals or sequences.
            driver.skip_matching();
        } else {
            driver.start_matching_codes(&mut literals, &mut seqs);
            if seqs.is_empty() {
                // A zero-sequence block's literals are the block itself.
                for &b in &head[pos..end] {
                    lit_hist[b as usize] += 1;
                }
            } else {
                for &b in &literals {
                    lit_hist[b as usize] += 1;
                }
                for w in &seqs {
                    seq_bits += SEQ_CODE_BITS + w.add_nb as u64;
                }
                literals.clear();
                seqs.clear();
            }
        }
        pos = end;
    }

    #[cfg(feature = "std")]
    PROBE_DRIVER.with(|p| *p.borrow_mut() = Some(driver));
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
            bits -= p * p.log2();
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
