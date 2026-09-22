//! Formatted-dictionary emission: the `ZDICT_finalizeDictionary` port.
//!
//! Every training sample's first block is parsed against the trained
//! content as a raw-content dictionary (libzstd's stats level), the
//! literals and the ll/ml/of code streams feed four histograms, and the
//! normalized tables serialize in front of the content in libzstd's
//! layout — `[magic][dictID][huffman][of][ml][ll][reps][content]` — so
//! any zstd decoder (and this crate's) accepts the result.
//!
//! Divergences from C, all measured irrelevant to the output:
//! the "most common first offsets" analysis is dead code upstream
//! (`repStartValue` is what gets written, so it is not collected), and
//! blocks that would encode raw still contribute their parse (C skips
//! them; on real training corpora the case does not occur).

use alloc::vec::Vec;

use crate::{
    InputShape, Level,
    bit_io::BitWriter,
    decoding::dictionary::MAGIC_NUM,
    encoding::{
        Matcher,
        dictionary::{EncDictionary, reset_with_dictionary},
        frame_compressor::new_owned_state,
    },
    fse::fse_encoder::{FseBuildScratch, build_table_from_probabilities, normalize_count},
    huff0::huff0_encoder::{HuffScratch, HuffmanTable, write_table_desc},
};

/// The only rep triple a finalized dictionary writes: C's `repStartValue`.
pub(crate) const REP_START: [u32; 3] = [1, 4, 8];
/// Stats-parse level. C parses at `ZSTD_CLEVEL_DEFAULT` (3); our row
/// there is a denser dfast than libzstd's (more matches, fewer literals)
/// and its histograms seed measurably worse tables than every sparser or
/// deeper row — level 4 (greedy) sits on the measured quality plateau
/// (fixture: 3 seeds x referee levels, fixed content; see docs).
pub(crate) const STATS_LEVEL: i32 = 4;
/// Format maxima for the code histograms.
const MAX_LL_CODE: usize = 35;
const MAX_ML_CODE: usize = 52;
const OFFCODE_MAX: usize = 30;
/// Target accuracy logs, C's LLFSELog/MLFSELog/OffFSELog.
const LL_LOG: u8 = 6;
const ML_LOG: u8 = 6;
const OF_LOG: u8 = 8;
/// C's offcode reach: codes must cover matches into the dictionary plus
/// one window of frame data.
const OFFCODE_MARGIN: usize = 128 * 1024;
/// A dictionary's content must at least span the repcodes it writes.
const MIN_CONTENT: usize = REP_START[2] as usize;

/// Build a formatted dictionary of at most `capacity` bytes: `content`
/// (e.g. from the in-tree trainer) seeded with entropy tables collected
/// over `samples`. Returns `None` when the header plus the minimum content
/// cannot fit.
pub fn finalize_dictionary(content: &[u8], samples: &[&[u8]], capacity: usize) -> Option<Vec<u8>> {
    finalize_dictionary_ex(content, samples, capacity, STATS_LEVEL)
}

/// The configurable-stats form: the parse level selects the histogram
/// source (C pins `ZSTD_CLEVEL_DEFAULT`; the engine's row at that level
/// differs from libzstd's, so the level is tunable here).
pub fn finalize_dictionary_ex(
    content: &[u8],
    samples: &[&[u8]],
    capacity: usize,
    stats_level: i32,
) -> Option<Vec<u8>> {
    let mut fse = FseBuildScratch::default();
    let mut huff = HuffScratch::default();
    let header = serialize_header_ex(content, samples, capacity, stats_level, &mut fse, &mut huff)?;

    // C's shrink: the header takes its bytes out of the content budget,
    // keeping the content's front (the trainer spends the full budget, the
    // finalize step clips the tail).
    let content_keep = capacity.saturating_sub(header.len()).min(content.len());
    let content = &content[..content_keep];
    if header.len() + content.len().max(MIN_CONTENT) > capacity {
        return None;
    }
    // Content shorter than the largest repcode is zero-padded in front, so
    // every rep offset resolves inside the dictionary.
    let padding = MIN_CONTENT.saturating_sub(content.len());

    let mut dict = header;
    dict.reserve(padding + content.len());
    dict.resize(dict.len() + padding, 0);
    dict.extend_from_slice(content);
    Some(dict)
}

/// The serialized dictionary header: magic, content-hash id, entropy
/// tables, repcodes.
fn serialize_header(
    content: &[u8],
    samples: &[&[u8]],
    capacity: usize,
    fse: &mut FseBuildScratch,
    huff: &mut HuffScratch,
) -> Option<Vec<u8>> {
    serialize_header_ex(content, samples, capacity, STATS_LEVEL, fse, huff)
}

fn serialize_header_ex(
    content: &[u8],
    samples: &[&[u8]],
    capacity: usize,
    stats_level: i32,
    fse: &mut FseBuildScratch,
    huff: &mut HuffScratch,
) -> Option<Vec<u8>> {
    // The id is computed over the untruncated content, like C (the header
    // is built before the content budget is settled).
    let id = dict_id(content);
    let stats = collect_stats_ex(content, samples, stats_level);

    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(&MAGIC_NUM);
    out.extend_from_slice(&id.to_le_bytes());

    let table = build_huffman(&stats, huff);
    write_table_desc(&table, fse, huff);
    out.extend_from_slice(&huff.desc);

    let of_max = offcode_max(content.len());
    write_ncode(&mut out, &stats.of[..=of_max], OF_LOG);
    write_ncode(&mut out, &stats.ml[..=MAX_ML_CODE], ML_LOG);
    write_ncode(&mut out, &stats.ll[..=MAX_LL_CODE], LL_LOG);

    for &rep in &REP_START {
        out.extend_from_slice(&rep.to_le_bytes());
    }
    // The header must leave room for the minimum content.
    (out.len() + MIN_CONTENT <= capacity).then_some(out)
}

/// Offset codes stop at the dictionary's own reach: content plus one
/// window (C's `offcodeMax`), capped by the format.
fn offcode_max(content_len: usize) -> usize {
    ((content_len + OFFCODE_MARGIN).ilog2() as usize).min(OFFCODE_MAX)
}

/// The dictionary id: a content-derived nonzero value in the range libzstd
/// reserves for unnamed dictionaries (`XXH64 % (2^31 - 2^15) + 2^15`).
#[cfg(feature = "hash")]
fn dict_id(content: &[u8]) -> u32 {
    let mut hasher = crate::xxh64::Xxh64::new(0);
    hasher.write(content);
    let hash = hasher.finish();
    ((hash % ((1u64 << 31) - 32768)) + 32768) as u32
}

/// Deterministic fallback when the xxhash feature is off: any stable
/// nonzero value serves (the id only has to match across invocations).
#[cfg(not(feature = "hash"))]
fn dict_id(content: &[u8]) -> u32 {
    let mut acc = 0xcbf2_9ce4_8422_2325u64;
    for &b in content {
        acc = (acc ^ b as u64).wrapping_mul(0x1000_0000_01b3);
    }
    ((acc % ((1u64 << 31) - 32768)) + 32768) as u32
}

/// Code histograms over the stats parses. Every symbol starts at one
/// count (C's "any character must be described"), so a table built from
/// an empty or skewed corpus still describes the whole alphabet.
struct EntropyStats {
    lit: [u32; 256],
    ll: [u32; MAX_LL_CODE + 1],
    ml: [u32; MAX_ML_CODE + 1],
    of: Vec<u32>,
}

impl EntropyStats {
    fn new(of_max: usize) -> Self {
        Self {
            lit: [1; 256],
            ll: [1; MAX_LL_CODE + 1],
            ml: [1; MAX_ML_CODE + 1],
            of: alloc::vec![1; of_max + 1],
        }
    }
}

/// Parse every sample's first block against the content as raw match
/// history and count the emitted literals and code triples.
fn collect_stats(content: &[u8], samples: &[&[u8]]) -> EntropyStats {
    collect_stats_ex(content, samples, STATS_LEVEL)
}

fn collect_stats_ex(content: &[u8], samples: &[&[u8]], stats_level: i32) -> EntropyStats {
    let level = Level::from_zstd(stats_level);
    // C sizes the stats parse's cParams once from the average sample size
    // (`ZDICT_analyzeEntropy`'s `ZSTD_getParams(level, average, dict)`);
    // a per-sample shape would re-clamp the tables on every file.
    let average: u64 = if samples.is_empty() {
        0
    } else {
        samples.iter().map(|s| s.len() as u64).sum::<u64>() / samples.len() as u64
    };
    let dict = EncDictionary::raw_content(content);
    let mut stats = EntropyStats::new(offcode_max(content.len()));
    let mut state = new_owned_state();
    let mut literals = Vec::new();
    let mut seqs = Vec::new();
    for &sample in samples {
        let shape = InputShape::default().with_len(average + content.len() as u64);
        reset_with_dictionary(&mut state, &dict, level, shape);
        // One block per sample, like C's per-file compressBegin (block
        // bound below the level's window).
        let take = sample.len().min(state.matcher.block_size());
        let tail = state.matcher.block_tail();
        tail[..take].copy_from_slice(&sample[..take]);
        state.matcher.commit_block(take);
        literals.clear();
        seqs.clear();
        state.matcher.start_matching_codes(&mut literals, &mut seqs);
        for &b in &literals {
            stats.lit[b as usize] += 1;
        }
        for &word in &seqs {
            let codes = word.codes;
            stats.ll[(codes & 0xff) as usize] += 1;
            stats.ml[((codes >> 8) & 0xff) as usize] += 1;
            stats.of[((codes >> 16) & 0xff) as usize] += 1;
        }
    }
    stats
}

/// The literals table from the collected counts, with C's flat-literal
/// rescue for incompressible alphabets (a full 256-symbol flat table
/// cannot be serialized).
fn build_huffman(stats: &EntropyStats, huff: &mut HuffScratch) -> HuffmanTable {
    let mut counts = [0usize; 256];
    for (dst, &c) in counts.iter_mut().zip(stats.lit.iter()) {
        *dst = c as usize;
    }
    let table = HuffmanTable::build_from_counts_into(&counts, huff);
    let flat = table
        .code_lengths()
        .into_iter()
        .filter(|&len| len != 0)
        .all(|len| len == 8);
    if !flat {
        return table;
    }
    // C's ZDICT_flatLit: a mostly-flat but compressible stand-in.
    counts.fill(2);
    counts[0] = 4;
    counts[253] = 1;
    counts[254] = 1;
    HuffmanTable::build_from_counts_into(&counts, huff)
}

/// Normalize `counts` and append the FSE table description (the same
/// NCount form a block's sequence section carries).
fn write_ncode(out: &mut Vec<u8>, counts: &[u32], log: u8) {
    let max_symbol = counts.len() - 1;
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let mut norm = alloc::vec![0i32; counts.len()];
    if !normalize_count(&mut norm, log, counts, total, max_symbol, true) {
        // The all-ones init keeps every symbol live, so the fallback is
        // unreachable in practice; keep a defined output regardless.
        norm.fill(1);
    }
    let table = build_table_from_probabilities(&norm, log);
    let mut writer = BitWriter::from(out);
    table.write_table(&mut writer);
    writer.flush();
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::decoding::Dictionary;

    fn boilerplate_samples(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|i| {
                let mut sample = Vec::new();
                for _ in 0..3 {
                    sample.extend_from_slice(
                        b"[Unit]\nDescription=finalize boilerplate\nAfter=network.target\n",
                    );
                    sample.extend((0..24usize).map(|j| (i * 131 + j * 17) as u8));
                }
                sample
            })
            .collect()
    }

    #[test]
    fn finalize_is_deterministic_and_parsable() {
        let samples = boilerplate_samples(16);
        let refs: Vec<&[u8]> = samples.iter().map(|s| &s[..]).collect();
        let mut content = Vec::new();
        super::super::create_raw_dict_from_samples(&refs, &mut content, 2048);
        let train: Vec<&[u8]> = refs.clone();
        let a = finalize_dictionary(&content, &train, 2048).unwrap();
        let b = finalize_dictionary(&content, &train, 2048).unwrap();
        assert_eq!(a, b);
        assert!(a.len() <= 2048);

        let dict = Dictionary::decode_dict(&a).unwrap();
        assert_ne!(dict.id, 0);
        assert_eq!(dict.offset_hist, REP_START);
        assert!(!dict.dict_content.is_empty());
    }

    #[test]
    fn too_small_capacity_returns_none() {
        let content = vec![b'x'; 64];
        assert!(finalize_dictionary(&content, &[b"sample"], 16).is_none());
    }
}
