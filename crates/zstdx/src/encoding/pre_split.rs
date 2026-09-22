//! libzstd's pre-split block boundary detector (`zstd_preSplit.c`,
//! `ZSTD_splitBlock`): content-shape fingerprints over the next 128 KiB
//! decide where a full block should be cut before parsing. Pure function of
//! (bytes, effort level); an exact port of the C integer arithmetic, so a
//! corpus block splits at the same offset as libzstd would.
//!
//! Engagement mirrors libzstd's `ZSTD_optimalBlockSize`: only full 128 KiB
//! blocks are candidates, and only while the frame's verified savings
//! (input minus output accumulated so far) stays >= 3 bytes — the first
//! block of a frame or job therefore never splits, and incompressible data
//! (negative savings) is never re-chunked. The savings bookkeeping lives
//! with the emitting loops ([`FrameSavings`]). Two deliberate deviations
//! from the C gate, both measured (see docs/src/dev/perf/encoding.md): the
//! opt-family rows are exempt (their post-parse splitter degrades on
//! pre-cut inputs), and a narrow-alphabet screen skips the detector
//! outright (its fixed scan is a throughput tax on exactly the shapes that
//! never trigger it).

/// The minimum block the format's splitter path operates on (libzstd's
/// conservative full-block rule: only 128 KiB blocks are ever split).
pub(crate) const SPLIT_BLOCK: usize = 128 * 1024;
/// The smallest block the borders strategy produces; the by-chunks walk
/// can cut at 8 KiB multiples (libzstd's documented splitter lower limit),
/// so this is only the worst-case header-reservation floor.
pub(crate) const SPLIT_MIN: usize = 32 * 1024;
/// Verified frame savings required before a split is attempted (libzstd's
/// bar against oversplitting incompressible data).
const SAVINGS_MIN: i64 = 3;
/// Distinct-symbol count the screen's probes must show before the
/// detector runs. A throughput-class screen on top of libzstd's own gate:
/// the detector's fixed per-block scan (1-20 us depending on the effort
/// level) is noise against a 200+ us dll-class parse but a 10-35% tax on
/// frames whose blocks encode in microseconds — and exactly those
/// narrow-alphabet shapes never trigger it anyway (measured: json 0/257
/// and skewed 0/256 blocks fire at every level; text fires 29/15/4 per
/// level 2/3/4 and LOSES 105-185 B doing so; at this bar dll100 keeps
/// 514/538 balanced-row triggers while the worst text block probes 85,
/// json 39, skewed 16).
const SPLIT_SYMS_MIN: u32 = 128;
/// The screen's probe length: three contiguous 4 KiB reads (head, middle,
/// tail) — sequential enough for the prefetcher that the whole screen
/// costs ~0.1 us, where the strided-sample variant's cache-line-per-sample
/// walk alone cost text.fastest 18%.
const SCREEN_PROBE: usize = 4 * 1024;

/// The detector's effort level, from libzstd's `splitLevels[]` strategy
/// row: how densely the fingerprints sample, and whether the cheap
/// border-compare runs instead of the chunk walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SplitLevel {
    /// libzstd level 0 (`ZSTD_splitBlock_fromBorders`): two 512-byte
    /// border histograms, a middle histogram, and a three-way cut. No
    /// production row maps here currently (the single-probe rows are
    /// exempt, see `MatchGeneratorDriver::pre_split_level`); the complete
    /// C table stays ported and fixture-pinned for future re-arming.
    #[allow(dead_code)]
    Borders,
    /// libzstd levels 1-4 (`ZSTD_splitBlock_byChunks`): 8 KiB chunk
    /// fingerprints at the given sampling rate; the payload is
    /// `(rate, hash_log)` from C's `records_fs[]`/`hashParams[]` rows.
    Chunks { rate: usize, hash_log: u32 },
}

impl SplitLevel {
    /// The levels in libzstd's by-chunks table order (rate 43/11/5/1,
    /// hashLog 8/9/10/10), indexed by `splitLevel - 1`.
    const CHUNKED: [SplitLevel; 4] = [
        SplitLevel::Chunks {
            rate: 43,
            hash_log: 8,
        },
        SplitLevel::Chunks {
            rate: 11,
            hash_log: 9,
        },
        SplitLevel::Chunks {
            rate: 5,
            hash_log: 10,
        },
        SplitLevel::Chunks {
            rate: 1,
            hash_log: 10,
        },
    ];

    /// The by-chunks level at libzstd's `splitLevel - 1` (0-3).
    pub(crate) const fn chunked(row: usize) -> Self {
        SplitLevel::CHUNKED[row]
    }

    /// From libzstd's numeric `splitLevel` (0-4), or `None` past it (the
    /// [`Matcher::pre_split_effort`] routing's exempt marker).
    pub(crate) fn from_effort(level: u8) -> Option<SplitLevel> {
        match level {
            0 => Some(SplitLevel::Borders),
            1..=4 => Some(SplitLevel::chunked((level - 1) as usize)),
            _ => None,
        }
    }

    /// From libzstd's numeric `splitLevel` (0-4); tests only (the encoder
    /// derives the level from its own strategy family).
    #[cfg(test)]
    fn from_c(level: u8) -> Self {
        match level {
            0 => SplitLevel::Borders,
            n => SplitLevel::chunked((n - 1) as usize),
        }
    }
}

/// The frame/job-side state driving the split gate: libzstd's
/// `consumedSrcSize - producedCSize` savings (tracked per frame on the
/// single-threaded paths and per job on the multithreaded ones, matching
/// libzstd's per-CCtx counters; `note_block` is called with the block's
/// input size and its full emitted size, 3-byte header included) plus the
/// throughput screen's frame latch.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameSavings {
    savings: i64,
    /// Whether the screen armed the detector for this frame (latched on
    /// the first passing probe).
    armed: bool,
    /// Eligible blocks screened without a pass.
    screened: u8,
}

impl FrameSavings {
    pub(crate) const fn new() -> Self {
        FrameSavings {
            savings: 0,
            armed: false,
            screened: 0,
        }
    }

    /// Decide the next block's input size: `window` holds the bytes at the
    /// cursor (its length is the bytes the caller can serve right now,
    /// capped at [`SPLIT_BLOCK`]). Sub-128 KiB tails, unproven frames and
    /// exempt rows keep whole blocks; proven frames consult the screen
    /// latch and then the detector.
    pub(crate) fn block_size(&mut self, window: &[u8], level: Option<SplitLevel>) -> usize {
        if window.len() < SPLIT_BLOCK || self.savings < SAVINGS_MIN {
            return window.len();
        }
        match level {
            Some(level) if self.arm(window) => split_block(window, level),
            _ => window.len(),
        }
    }

    /// Fold one emitted block into the savings.
    pub(crate) fn note_block(&mut self, input: usize, emitted: usize) {
        self.savings += input as i64 - emitted as i64;
    }

    /// The screen, latched per frame: the first [`SCREEN_BLOCKS`] eligible
    /// blocks are probed, and one wide verdict arms the detector for the
    /// rest of the frame (shapes do not alternate class block to block —
    /// the corpus extremes measure 247 distinct probes on dll-class heads
    /// vs 85/39/16/1 for text/json/skewed/zeros). Amortizes the probe to
    /// a few KiB per frame: a per-block probe measurably taxed the
    /// narrow-alphabet shapes even as a pure load stream.
    fn arm(&mut self, window: &[u8]) -> bool {
        if self.armed {
            return true;
        }
        if self.screened >= SCREEN_BLOCKS {
            return false;
        }
        self.screened += 1;
        self.armed = wide_alphabet(window);
        self.armed
    }
}

/// The screen's latch window: eligible blocks probed before the frame
/// verdict settles (never armed).
const SCREEN_BLOCKS: u8 = 4;

/// The throughput-class screen (see [`SPLIT_SYMS_MIN`]): the best
/// distinct count over three 4 KiB probes.
fn wide_alphabet(window: &[u8]) -> bool {
    let mid = window.len() / 2 - SCREEN_PROBE / 2;
    [0, mid, window.len() - SCREEN_PROBE].iter().any(|&off| {
        let mut bitmap = [0u64; 4];
        for &b in &window[off..off + SCREEN_PROBE] {
            bitmap[b as usize >> 6] |= 1u64 << (b & 63);
        }
        bitmap.iter().map(|w| w.count_ones()).sum::<u32>() >= SPLIT_SYMS_MIN
    })
}

/// `ZSTD_splitBlock`: the split offset of a full 128 KiB block (equal to
/// its length when the block should stay whole).
pub(crate) fn split_block(block: &[u8], level: SplitLevel) -> usize {
    debug_assert_eq!(block.len(), SPLIT_BLOCK);
    match level {
        SplitLevel::Borders => split_from_borders(block),
        SplitLevel::Chunks { rate, hash_log } => split_by_chunks(block, rate, hash_log),
    }
}

const THRESHOLD_PENALTY_RATE: u64 = 16;
const THRESHOLD_BASE: u64 = THRESHOLD_PENALTY_RATE - 2;
const THRESHOLD_PENALTY: u32 = 3;
const HASHLOG_MAX: u32 = 10;
const HASHTABLE_SIZE: usize = 1 << HASHLOG_MAX;
const KNUTH: u32 = 0x9e37_79b9;
/// The by-chunks walk's fingerprint unit.
const CHUNK: usize = 8 << 10;

/// A sampled content fingerprint: a small event histogram plus its sample
/// count. The histogram is the table libzstd's `hash2` feeds (2-byte
/// hashes), or the plain byte histogram for the borders strategy.
#[derive(Clone)]
struct Fingerprint {
    events: [u32; HASHTABLE_SIZE],
    nb_events: u64,
}

impl Fingerprint {
    fn zeroed(hash_log: u32) -> Self {
        // Only the hashed prefix is ever read back; zeroing it all keeps
        // the struct reproducible without depending on the previous level.
        let mut fp = Fingerprint {
            events: [0; HASHTABLE_SIZE],
            nb_events: 0,
        };
        fp.clear(hash_log);
        fp
    }

    /// libzstd's `recordFingerprint_generic`: reset the used prefix and
    /// re-add the events.
    fn clear(&mut self, hash_log: u32) {
        self.events[..1usize << hash_log].fill(0);
        self.nb_events = 0;
    }
}

/// `hash2`: for hashLog == 8 the byte itself, else the 2-byte native
/// read scaled by Knuth's constant into the table.
#[inline]
fn hash2(p: &[u8], hash_log: u32) -> usize {
    debug_assert!((8..=HASHLOG_MAX).contains(&hash_log));
    if hash_log == 8 {
        return p[0] as usize;
    }
    let pair = u16::from_ne_bytes([p[0], p[1]]) as u32;
    pair.wrapping_mul(KNUTH) as usize >> (32 - hash_log)
}

/// `addEvents_generic`: sample every `rate` positions. Note the sample
/// count is `limit / rate` — one below the loop's iteration count for
/// `rate > 1` in libzstd; kept exactly, the threshold compare reads it.
fn add_events(fp: &mut Fingerprint, src: &[u8], rate: usize, hash_log: u32) {
    let limit = src.len() - 1;
    let mut n = 0;
    while n < limit {
        fp.events[hash2(&src[n..], hash_log)] += 1;
        n += rate;
    }
    fp.nb_events += (limit / rate) as u64;
}

fn record_fingerprint(fp: &mut Fingerprint, src: &[u8], rate: usize, hash_log: u32) {
    fp.clear(hash_log);
    add_events(fp, src, rate, hash_log);
}

/// `fpDistance`: the L1 distance between the histograms scaled by each
/// other's sample counts.
fn fp_distance(a: &Fingerprint, b: &Fingerprint, hash_log: u32) -> u64 {
    let mut distance = 0u64;
    for i in 0..(1usize << hash_log) {
        let delta =
            a.events[i] as i64 * b.nb_events as i64 - b.events[i] as i64 * a.nb_events as i64;
        distance += delta.unsigned_abs();
    }
    distance
}

/// `compareFingerprints`: whether the two samples are considered too
/// different to share a block.
fn too_different(reference: &Fingerprint, new: &Fingerprint, penalty: u32, hash_log: u32) -> bool {
    debug_assert!(reference.nb_events > 0 && new.nb_events > 0);
    let p50 = reference.nb_events * new.nb_events;
    let deviation = fp_distance(reference, new, hash_log);
    let threshold = p50 * (THRESHOLD_BASE + penalty as u64) / THRESHOLD_PENALTY_RATE;
    deviation >= threshold
}

/// `mergeEvents`: fold the newer sample into the running one. libzstd
/// walks the full table; the hashed-out prefix is provably zero above
/// `hash_log`, so the shortened loop is exact.
fn merge_events(acc: &mut Fingerprint, new: &Fingerprint, hash_log: u32) {
    for i in 0..(1usize << hash_log) {
        acc.events[i] += new.events[i];
    }
    acc.nb_events += new.nb_events;
}

/// `ZSTD_splitBlock_byChunks`: fingerprint the first 8 KiB, then compare
/// every following 8 KiB chunk against the merged history; the first
/// mismatch is the split, with the threshold relaxing one penalty step per
/// agreeing chunk.
fn split_by_chunks(block: &[u8], rate: usize, hash_log: u32) -> usize {
    let mut past = Fingerprint::zeroed(hash_log);
    record_fingerprint(&mut past, &block[..CHUNK], rate, hash_log);
    // Reused across chunks like C's `newEvents`: `record_fingerprint`
    // clears the hashed prefix, the rest is provably zero.
    let mut new = Fingerprint::zeroed(hash_log);
    let mut penalty = THRESHOLD_PENALTY;
    let mut pos = CHUNK;
    while pos + CHUNK <= block.len() {
        record_fingerprint(&mut new, &block[pos..pos + CHUNK], rate, hash_log);
        if too_different(&past, &new, penalty, hash_log) {
            return pos;
        }
        merge_events(&mut past, &new, hash_log);
        if penalty > 0 {
            penalty -= 1;
        }
        pos += CHUNK;
    }
    block.len()
}

/// The borders strategy's segment size.
const SEGMENT: usize = 512;

/// `HIST_add`: fold a plain byte histogram (the borders strategy samples
/// individual bytes, not hashes).
fn hist_add(fp: &mut Fingerprint, src: &[u8]) {
    for &b in src {
        fp.events[b as usize] += 1;
    }
}

/// `ZSTD_splitBlock_fromBorders`: compare the block's two 512-byte border
/// histograms; on a mismatch, place the middle histogram and cut at 64 KiB
/// when it is evenly between the borders, else on the side it resembles
/// less. (libzstd's third fingerprint aliases the workspace's first; every
/// read stays inside the 256-entry prefix, so a separate struct is exact.)
fn split_from_borders(block: &[u8]) -> usize {
    let mut begin = Fingerprint::zeroed(8);
    let mut end = Fingerprint::zeroed(8);
    hist_add(&mut begin, &block[..SEGMENT]);
    hist_add(&mut end, &block[block.len() - SEGMENT..]);
    begin.nb_events = SEGMENT as u64;
    end.nb_events = SEGMENT as u64;
    if !too_different(&begin, &end, 0, 8) {
        return block.len();
    }
    let mut middle = Fingerprint::zeroed(8);
    let mid = block.len() / 2 - SEGMENT / 2;
    hist_add(&mut middle, &block[mid..mid + SEGMENT]);
    middle.nb_events = SEGMENT as u64;
    let dist_from_begin = fp_distance(&begin, &middle, 8);
    let dist_from_end = fp_distance(&end, &middle, 8);
    let min_distance = (SEGMENT * SEGMENT / 3) as u64;
    if dist_from_begin.abs_diff(dist_from_end) < min_distance {
        return 64 * 1024;
    }
    if dist_from_begin > dist_from_end {
        32 * 1024
    } else {
        96 * 1024
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec, vec::Vec};

    use super::*;

    /// Fixture blocks and their libzstd 1.5.7 split offsets: the layout is
    /// `n_blocks:u32, n_levels:u32, levels:[u8; n_levels]`, then per block
    /// `SPLIT_BLOCK` bytes, then per (block, level) a `u32` split offset
    /// produced by a C harness over `zstd_preSplit.c` (see
    /// docs/src/dev/perf/encoding.md). Covers the triggering dll shapes,
    /// the non-triggering corpus shapes and uniform/incompressible inputs
    /// across all five levels.
    const FIXTURE: &[u8] = include_bytes!("testdata/presplit_fixture.bin");

    #[test]
    fn c_fixture_parity() {
        let mut r = FIXTURE;
        let u32_at = |r: &mut &[u8]| {
            let (head, rest) = r.split_at(4);
            *r = rest;
            u32::from_le_bytes(head.try_into().unwrap())
        };
        let n_blocks = u32_at(&mut r) as usize;
        let n_levels = u32_at(&mut r) as usize;
        let levels: Vec<u8> = r[..n_levels].to_vec();
        r = &r[n_levels..];
        let blocks: Vec<&[u8]> = (0..n_blocks)
            .map(|_| {
                let (head, rest) = r.split_at(SPLIT_BLOCK);
                r = rest;
                head
            })
            .collect();
        let expected: Vec<u32> = (0..n_blocks * n_levels).map(|_| u32_at(&mut r)).collect();
        assert!(r.is_empty());
        let mut i = 0;
        for block in &blocks {
            for &level in &levels {
                let got = split_block(block, SplitLevel::from_c(level));
                assert_eq!(
                    got,
                    expected[i] as usize,
                    "block {} level {}: {} != C's {}",
                    i / n_levels,
                    level,
                    got,
                    expected[i]
                );
                i += 1;
            }
        }
    }

    /// Whole-file differential against the C harness: set
    /// `ZSTDX_PRESPLIT_HARNESS=/path/to/harness` (a binary reading
    /// `harness <file> <level>` and printing one split per 128 KiB block)
    /// to compare every block of an arbitrary file. Dev-only; not part of
    /// CI.
    #[cfg(feature = "std")]
    #[test]
    fn c_harness_differential() {
        let Ok(harness) = std::env::var("ZSTDX_PRESPLIT_HARNESS") else {
            return;
        };
        let path = std::env::var("ZSTDX_PRESPLIT_FILE").expect("ZSTDX_PRESPLIT_FILE");
        let data = std::fs::read(&path).expect("readable input");
        for level in 0u8..=4 {
            let out = std::process::Command::new(&harness)
                .arg(&path)
                .arg(level.to_string())
                .output()
                .expect("harness runs");
            assert!(out.status.success());
            let splits: Vec<usize> = std::str::from_utf8(&out.stdout)
                .unwrap()
                .lines()
                .map(|l| l.trim().parse().unwrap())
                .collect();
            let blocks: Vec<&[u8]> = data.chunks(SPLIT_BLOCK).collect();
            assert_eq!(blocks.len(), splits.len(), "harness block count");
            for (i, block) in blocks.iter().enumerate() {
                let got = if block.len() < SPLIT_BLOCK {
                    block.len()
                } else {
                    split_block(block, SplitLevel::from_c(level))
                };
                assert_eq!(got, splits[i], "level {level} block {i}");
            }
        }
    }

    /// The gate semantics: tails below 128 KiB and unproven frames keep
    /// whole blocks; savings plus a wide screen unlock the detector.
    #[test]
    fn gate_semantics() {
        let block = vec![7u8; SPLIT_BLOCK];
        let mut savings = FrameSavings::new();
        assert_eq!(
            savings.block_size(&block, Some(SplitLevel::Borders)),
            SPLIT_BLOCK
        );
        // An exempt row never consults the detector.
        assert_eq!(savings.block_size(&block, None), SPLIT_BLOCK);
        let short = vec![7u8; SPLIT_BLOCK - 1];
        assert_eq!(
            savings.block_size(&short, Some(SplitLevel::Borders)),
            SPLIT_BLOCK - 1
        );
        let mut proven = FrameSavings::new();
        proven.note_block(SPLIT_BLOCK, 1024);
        // Uniform content never arms the screen.
        assert_eq!(
            proven.block_size(&block, Some(SplitLevel::Borders)),
            SPLIT_BLOCK
        );
        // A wide, uniform-free window arms on the first probe and splits
        // at the detector's verdict.
        let mut wide = Vec::with_capacity(SPLIT_BLOCK);
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        while wide.len() < SPLIT_BLOCK {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            wide.extend_from_slice(&x.to_le_bytes());
        }
        // Random windows stay whole: the borders detector cannot split
        // them below the savings bar... (random triggers borders 2/256 in
        // the C census, so only assert the arming path's determinism).
        let a = proven.block_size(&wide, Some(SplitLevel::Borders));
        let b = proven.block_size(&wide, Some(SplitLevel::Borders));
        assert_eq!(a, b);
    }
}
