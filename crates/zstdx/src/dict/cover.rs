//! fastCover-style segment selection for the trainer: a frequency table of
//! 8-byte k-mers over the training data, a per-epoch sliding-window search
//! that scores each window by the summed frequency of its *distinct* k-mers,
//! and full zeroing of a selected segment's k-mers so repeated content
//! cannot buy dictionary budget twice (the libzstd `fastcover.c` scheme).

use std::{collections::HashMap, vec::Vec};

use super::K;

/// libzstd fastCover's frequency-table index width (`f`, default 20).
const LIBZSTD_F: u32 = 20;

/// How a k-mer maps to a frequency-table key.
#[derive(Clone, Copy, Debug, Default)]
pub enum DmerHash {
    /// Full-width fingerprint: distinct k-mers never share a key.
    #[default]
    Exact,
    /// libzstd's `ZSTD_hash8Ptr(p, f)` bucket: the top `f` bits of the
    /// k-mer times `prime8bytes` — distinct k-mers collide, exactly like
    /// the C trainer's fixed-size frequency vector.
    LibzstdBuckets,
}

impl DmerHash {
    #[inline]
    fn key(&self, body: &[u8], pos: usize) -> u64 {
        let kmer = u64::from_le_bytes(body[pos..pos + K].try_into().unwrap());
        match self {
            Self::Exact => kmer.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29),
            Self::LibzstdBuckets => kmer.wrapping_mul(0xcf1b_bcdc_b7a5_6463) >> (64 - LIBZSTD_F),
        }
    }
}

/// Cap on frequency-table entries; larger collections stride their k-mer
/// samples. Estimates stay monotone in the true count.
const MAX_TABLE_ENTRIES: usize = 1 << 21;

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
    /// K-mer counts over the training data, keyed by the configured hash
    /// (exact fingerprints, or libzstd's colliding buckets). Selected
    /// segments' keys are zeroed (removed), so later epochs must bring new
    /// content.
    counts: HashMap<u64, u32>,
    /// Multiplicity of each key inside the active sliding window; empty
    /// between `select_segment` calls.
    window: HashMap<u64, u32>,
    hash: DmerHash,
}

/// Sample-end offsets of the training split, for within-sample counting.
fn sample_ends(lens: &[usize]) -> Vec<usize> {
    let mut ends = Vec::with_capacity(lens.len());
    let mut offset = 0;
    for &len in lens {
        offset += len;
        ends.push(offset);
    }
    ends
}

impl KMerTable {
    /// Count the training split's k-mers. With `cross_sample` off, k-mers
    /// spanning a sample boundary are left uncounted (frequency zero),
    /// matching libzstd's per-sample frequency loop; the epoch layout
    /// still walks the concatenated positions like C's `nbDmers`.
    pub(super) fn build(
        body: &[u8],
        sample_lens: &[usize],
        cross_sample: bool,
        hash: DmerHash,
    ) -> Self {
        let positions = body.len() + 1 - K;
        let stride = (positions / MAX_TABLE_ENTRIES).max(1);
        debug_assert!(sample_lens.iter().sum::<usize>() <= body.len());
        let ends = sample_ends(sample_lens);
        let mut sample = 0usize;
        let mut counts = HashMap::with_capacity((positions / stride).min(MAX_TABLE_ENTRIES));
        for pos in (0..positions).step_by(stride) {
            while sample < ends.len() && pos >= ends[sample] {
                sample += 1;
            }
            let inside = sample < ends.len() && pos + K <= ends[sample];
            if !cross_sample && !inside {
                continue;
            }
            *counts.entry(hash.key(body, pos)).or_insert(0) += 1;
        }
        Self {
            counts,
            window: HashMap::new(),
            hash,
        }
    }

    /// Total counted k-mer occurrences (test observable: the within-sample
    /// filter must drop only boundary-spanning positions).
    pub(super) fn total_counts(&self) -> u64 {
        self.counts.values().map(|&c| c as u64).sum()
    }

    /// Best window of `k` bytes inside the k-mer position range
    /// `[begin, end)`, sliding one position at a time. The window score is
    /// the summed frequency of its distinct keys; the winner's keys are
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
            let hash = self.hash.key(body, hi);
            let count = self.counts.get(&hash).copied().unwrap_or(0);
            let multiplicity = self.window.entry(hash).or_insert(0);
            if *multiplicity == 0 {
                score += count as usize;
            }
            *multiplicity += 1;
            if hi - lo + 1 > dmers_in_k {
                let gone = self.hash.key(body, lo);
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
            let hash = self.hash.key(body, pos);
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
            self.counts.remove(&self.hash.key(body, pos));
        }
        best
    }
}

/// Aperiodic filler bytes (a small xorshift stream).
fn noise(seed: u64, len: usize) -> Vec<u8> {
    use std::vec::Vec;
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
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
    let mut table = KMerTable::build(&body, &[body.len()], true, DmerHash::Exact);
    let segment = table.select_segment(&body, 0, body.len() + 1 - K, 256);
    assert!(segment.score > 0);
    let seg = &body[segment.begin..segment.end + K - 1];
    assert!(
        shared.windows(seg.len()).any(|w| w == seg),
        "best segment must come from the shared block"
    );
}

#[test]
fn cross_sample_kmers_go_uncounted() {
    use std::vec::Vec;

    // A body of alternating distinct blocks: the repeated k-mers are both
    // the interior ones (each block appears twice) and the
    // boundary-spanning ones (the junctions repeat too). The within-sample
    // filter must drop exactly the boundary-spanning positions: strictly
    // fewer counted occurrences, while the interior repeats stay scored.
    let a: Vec<u8> = noise(0x5eed_0001, 600);
    let b: Vec<u8> = noise(0x5eed_0002, 600);
    let mut body = Vec::new();
    for block in [&a, &b, &a, &b] {
        body.extend_from_slice(block);
    }
    let lens = [a.len(), b.len(), a.len(), b.len()];
    for hash in [DmerHash::Exact, DmerHash::LibzstdBuckets] {
        let mut stopped = KMerTable::build(&body, &lens, false, hash);
        let mut crossed = KMerTable::build(&body, &lens, true, hash);
        assert!(stopped.total_counts() < crossed.total_counts());

        let seg = stopped.select_segment(&body, 0, body.len() + 1 - K, 256);
        assert!(
            seg.score > 0,
            "interior repeats (each block twice) must stay scored"
        );
        let best = &body[seg.begin..seg.end + K - 1];
        let inside = a
            .windows(best.len())
            .chain(b.windows(best.len()))
            .any(|w| w == best);
        assert!(inside, "the winner must come from within-sample content");
    }
}
