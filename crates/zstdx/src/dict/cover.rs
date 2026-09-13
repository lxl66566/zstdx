//! fastCover-style segment selection for the trainer: a frequency table of
//! 8-byte k-mers over the training data, a per-epoch sliding-window search
//! that scores each window by the summed frequency of its *distinct* k-mers,
//! and full zeroing of a selected segment's k-mers so repeated content
//! cannot buy dictionary budget twice (the libzstd `fastcover.c` scheme).

use std::collections::HashMap;

use super::K;

/// Cap on frequency-table entries; larger collections stride their k-mer
/// samples. Estimates stay monotone in the true count.
const MAX_TABLE_ENTRIES: usize = 1 << 21;

/// 64-bit fingerprint of the k-mer starting at `pos`.
#[inline]
fn kmer_hash_at(body: &[u8], pos: usize) -> u64 {
    let kmer = u64::from_le_bytes(body[pos..pos + K].try_into().unwrap());
    kmer.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29)
}

/// A selected byte range of the training body.
pub(super) struct Segment {
    pub(super) begin: usize,
    /// One past the last k-mer start; the segment spans
    /// `end - begin + K - 1` bytes.
    pub(super) end: usize,
    pub(super) score: usize,
}

#[derive(Clone)]
pub(super) struct KMerTable {
    /// K-mer counts over the training data. Selected segments' k-mers are
    /// zeroed (removed), so later epochs must bring new content.
    counts: HashMap<u64, u32>,
    /// Multiplicity of each k-mer inside the active sliding window; empty
    /// between `select_segment` calls.
    window: HashMap<u64, u32>,
}

impl KMerTable {
    pub(super) fn build(body: &[u8]) -> Self {
        let positions = body.len() + 1 - K;
        let stride = (positions / MAX_TABLE_ENTRIES).max(1);
        let mut counts = HashMap::with_capacity((positions / stride).min(MAX_TABLE_ENTRIES));
        for pos in (0..positions).step_by(stride) {
            *counts.entry(kmer_hash_at(body, pos)).or_insert(0) += 1;
        }
        Self {
            counts,
            window: HashMap::new(),
        }
    }

    /// Best window of `k` bytes inside the k-mer position range
    /// `[begin, end)`, sliding one position at a time. The window score is
    /// the summed frequency of its distinct k-mers; the winner's k-mers are
    /// zeroed in `counts` before returning.
    pub(super) fn select_segment(
        &mut self,
        body: &[u8],
        begin: usize,
        end: usize,
        k: usize,
    ) -> Segment {
        let dmers_in_k = k + 1 - K;
        let mut score = 0usize;
        let mut lo = begin;
        let mut best = Segment {
            begin,
            end: begin,
            score: 0,
        };
        for hi in begin..end {
            let hash = kmer_hash_at(body, hi);
            let count = self.counts.get(&hash).copied().unwrap_or(0);
            let multiplicity = self.window.entry(hash).or_insert(0);
            if *multiplicity == 0 {
                score += count as usize;
            }
            *multiplicity += 1;
            if hi - lo + 1 > dmers_in_k {
                let gone = kmer_hash_at(body, lo);
                if let Some(multiplicity) = self.window.get_mut(&gone) {
                    *multiplicity -= 1;
                    if *multiplicity == 0 {
                        self.window.remove(&gone);
                        score -= self.counts.get(&gone).copied().unwrap_or(0) as usize;
                    }
                }
                lo += 1;
            }
            if score > best.score {
                best = Segment {
                    begin: lo,
                    end: hi + 1,
                    score,
                };
            }
        }
        // Drain the trailing window back to empty for the next epoch.
        for pos in lo..end {
            let hash = kmer_hash_at(body, pos);
            if let Some(multiplicity) = self.window.get_mut(&hash) {
                *multiplicity -= 1;
                if *multiplicity == 0 {
                    self.window.remove(&hash);
                }
            }
        }
        debug_assert!(self.window.is_empty());
        // Zero the winner: later selections must pay with new content.
        for pos in best.begin..best.end {
            self.counts.remove(&kmer_hash_at(body, pos));
        }
        best
    }
}

#[test]
fn prefers_repeated_block_over_noise() {
    use std::vec::Vec;

    // A collection whose only shared content is one verbatim block (itself
    // non-repetitive, so no straddling window can cover all its k-mers).
    // The best segment must come from the repeats, not the per-file noise.
    let mut shared = Vec::new();
    let mut state = 0x5eed_1234u32;
    for _ in 0..488 {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        shared.push((state >> 24) as u8);
    }
    let mut body = Vec::new();
    for i in 0..12u32 {
        body.extend_from_slice(&shared);
        let mut state = i.wrapping_mul(2654435761).wrapping_add(12345);
        body.extend((0..600).map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        }));
    }
    let mut table = KMerTable::build(&body);
    let segment = table.select_segment(&body, 0, body.len() + 1 - K, 256);
    assert!(segment.score > 0);
    let seg = &body[segment.begin..segment.end + K - 1];
    assert!(
        shared.windows(seg.len()).any(|w| w == seg),
        "best segment must come from the shared block"
    );
}
