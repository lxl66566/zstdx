//! libzstd's post-parse block splitter (`ZSTD_deriveBlockSplits`): one
//! 128 KiB block's staged sequences are recursively bisected, and a split
//! is committed when the two halves' entropy-coded size estimates (real
//! table builds through the same selection the emission runs) beat the
//! whole range's. Each committed range is then encoded as its own block,
//! so an entropy shift inside a block no longer taxes the whole block's
//! tables. Gated to the opt rows with a window of at least 2^17 — the
//! port of `ZSTD_resolveBlockSplitterMode`'s auto rule (strategy >= btopt
//! && windowLog >= 17).
//!
//! Deviation from C: libzstd reconciles the parser's and the decoder's
//! repcode histories per partition (`ZSTD_seqStore_resolveOffCodes`) so a
//! partition may fall back to a raw or RLE block mid-split. A splitter
//! partition here always carries hundreds of sequences whose encoded form
//! cannot lose to the raw bytes, so that path is unreachable in practice;
//! should one ever lose anyway, the whole block restarts unsplit from the
//! block-start entropy state — correct by construction (no partition was
//! emitted, so the decoder's repcode history never diverged).

use alloc::vec::Vec;

use super::compressed::{
    BlockOutcome, BlockScratch, BlockTables, DictEntropy, FseTableMode, PrevTable, StagedBlock,
    encode_staged_block, entropy_log2, histogram_literals, repeat_bit_cost, select_from_counts,
};
use crate::{
    bit_io::BitWriter,
    blocks::block::BlockType,
    encoding::{
        Matcher, SeqWord,
        block_header::BlockHeader,
        seq_codes::{SEQ_CODE_SPACE, decode_packed},
    },
    fse::fse_encoder::{FSETable, FseBuildScratch},
    huff0::huff0_encoder,
};

/// A range shorter than this is never considered for splitting (libzstd's
/// `MIN_SEQUENCES_BLOCK_SPLITTING`); it also bounds every partition to at
/// least half of it in sequences.
pub(crate) const MIN_SEQUENCES_BLOCK_SPLITTING: usize = 300;
/// Cap on committed splits per block (libzstd's
/// `ZSTD_MAX_NB_BLOCK_SPLITS`); unreachable at the 128 KiB block size.
const MAX_NB_BLOCK_SPLITS: usize = 196;

/// Pooled block-splitter driver scratch.
#[derive(Default)]
pub(crate) struct SplitScratch {
    /// Committed split points (sorted sequence indices), reused per block.
    partitions: Vec<u32>,
    /// Literal byte prefix per sequence index (`lit_prefix[i]` = bytes of
    /// literals before sequence `i`).
    lit_prefix: Vec<u32>,
    /// Match byte prefix per sequence index.
    match_prefix: Vec<u32>,
    /// FSE table-description measuring buffer.
    fse_desc: Vec<u8>,
}

/// What the split driver did with the block.
pub(crate) enum SplitOutcome {
    /// Partitions were emitted: every partition's block header is already
    /// final in `output` and each partition passed its own size guard, so
    /// the caller skips the single-block header patch and checks and only
    /// adopts the returned tables.
    Emitted(BlockTables),
    /// No split paid (or the block staged too few sequences): the outcome
    /// and output are exactly the stock single-block path's.
    Single(BlockOutcome),
}

/// Compress the matcher's last committed block, splitting it at entropy
/// shifts when the estimates pay (see the module docs). The contract is
/// [`super::compressed::compress_block`]'s plus one: `output` must end
/// with the block's three reserved header bytes (the caller reserves them
/// for the single-block path; the first partition claims them).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compress_split_block<M: Matcher>(
    matcher: &mut M,
    last_block: bool,
    output: &mut Vec<u8>,
    last_huff_table: Option<&huff0_encoder::HuffmanTable>,
    default_tables: (&FSETable, &FSETable, &FSETable),
    previous_tables: (&Option<FSETable>, &Option<FSETable>, &Option<FSETable>),
    dict_entropy: DictEntropy,
    scratch: &mut BlockScratch,
) -> SplitOutcome {
    let block_size = matcher.get_last_space().len();
    let BlockScratch {
        literals: lits,
        seqs,
        literals_gate_hold,
        fse,
        huff,
        split,
    } = scratch;
    lits.clear();
    seqs.clear();
    matcher.start_matching_codes(lits, seqs);
    let nb_seq = seqs.len();
    if nb_seq < MIN_SEQUENCES_BLOCK_SPLITTING {
        return SplitOutcome::Single(single_block(
            matcher,
            lits,
            seqs,
            last_huff_table,
            default_tables,
            previous_tables,
            dict_entropy,
            output,
            literals_gate_hold,
            fse,
            huff,
        ));
    }
    build_prefixes(seqs, &mut split.lit_prefix, &mut split.match_prefix);
    split.partitions.clear();
    {
        let mut est = Estimator {
            literals: lits,
            lit_prefix: &split.lit_prefix,
            seqs,
            last_huff: last_huff_table,
            prev: previous_tables,
            defaults: default_tables,
            dict: dict_entropy,
            fse,
            huff,
            fse_desc: &mut split.fse_desc,
        };
        let whole = est.estimate_scanned(0, nb_seq);
        derive_splits(&mut est, &mut split.partitions, 0, nb_seq, &whole);
    }
    if split.partitions.is_empty() {
        return SplitOutcome::Single(single_block(
            matcher,
            lits,
            seqs,
            last_huff_table,
            default_tables,
            previous_tables,
            dict_entropy,
            output,
            literals_gate_hold,
            fse,
            huff,
        ));
    }
    emit_partitions(
        matcher,
        block_size,
        last_block,
        lits,
        seqs,
        &split.lit_prefix,
        &split.match_prefix,
        &split.partitions,
        last_huff_table,
        default_tables,
        previous_tables,
        dict_entropy,
        output,
        literals_gate_hold,
        fse,
        huff,
    )
}

/// Encode the whole staged block exactly like the stock single-block path
/// (the caller patches the reserved header and adopts the outcome).
#[allow(clippy::too_many_arguments)]
fn single_block<'l, M: Matcher>(
    matcher: &mut M,
    lits: &'l Vec<u8>,
    seqs: &'l Vec<SeqWord>,
    last_huff_table: Option<&huff0_encoder::HuffmanTable>,
    default_tables: (&FSETable, &FSETable, &FSETable),
    previous_tables: (&Option<FSETable>, &Option<FSETable>, &Option<FSETable>),
    dict_entropy: DictEntropy,
    output: &mut Vec<u8>,
    literals_gate_hold: &mut bool,
    fse: &mut FseBuildScratch,
    huff: &mut huff0_encoder::HuffScratch,
) -> BlockOutcome {
    let staged = if seqs.is_empty() {
        StagedBlock::ZeroSeq
    } else {
        StagedBlock::Seqs {
            literals: lits,
            seqs,
        }
    };
    encode_staged_block(
        matcher,
        staged,
        last_huff_table,
        default_tables,
        (
            previous_tables.0.as_ref(),
            previous_tables.1.as_ref(),
            previous_tables.2.as_ref(),
        ),
        dict_entropy,
        output,
        literals_gate_hold,
        fse,
        huff,
    )
}

/// Fill the literal and match byte prefix arrays over the staged sequences.
fn build_prefixes(seqs: &[SeqWord], lit_prefix: &mut Vec<u32>, match_prefix: &mut Vec<u32>) {
    lit_prefix.clear();
    match_prefix.clear();
    lit_prefix.reserve(seqs.len() + 1);
    match_prefix.reserve(seqs.len() + 1);
    let mut lit = 0u32;
    let mut matched = 0u32;
    lit_prefix.push(0);
    match_prefix.push(0);
    for &w in seqs {
        let (ll, ml, _) = decode_packed(w.codes, w.add);
        lit += ll;
        matched += ml;
        lit_prefix.push(lit);
        match_prefix.push(matched);
    }
}

/// Everything one estimate would scan over a range: the literal histogram
/// and the per-stream sequence-code histograms plus the add-bits total.
/// Adjacent ranges add up entry-wise, so a suffix range's histograms are
/// the difference of its parent's and its prefix's — exact, no rescan.
struct RangeCounts {
    lit: [usize; 256],
    ll: [u32; SEQ_CODE_SPACE],
    ml: [u32; SEQ_CODE_SPACE],
    of: [u32; SEQ_CODE_SPACE],
    add_bits: u32,
}

impl RangeCounts {
    /// Histograms of `[b, c)` from `[a, c)`'s minus `[a, b)`'s.
    fn suffix_after(&self, prefix: &Self) -> Self {
        Self {
            lit: core::array::from_fn(|i| self.lit[i] - prefix.lit[i]),
            ll: core::array::from_fn(|i| self.ll[i] - prefix.ll[i]),
            ml: core::array::from_fn(|i| self.ml[i] - prefix.ml[i]),
            of: core::array::from_fn(|i| self.of[i] - prefix.of[i]),
            add_bits: self.add_bits - prefix.add_bits,
        }
    }
}

/// One derived range: its estimated size plus the histograms the estimate
/// was costed from (the recursion's material for the subtraction above).
struct RangeEstimate {
    size: usize,
    counts: RangeCounts,
}

/// libzstd's `ZSTD_deriveBlockSplitsHelper`: bisect `[start, end)` and
/// commit the midpoint when the halves' estimates beat the whole's,
/// recursing into both halves. Pushes split points in ascending order.
///
/// The whole range's estimate is threaded down from the parent — the
/// recursion would otherwise re-derive the exact range the parent just
/// estimated — and only the left half is scanned: the right half's cost
/// runs on `whole - left` histograms, so every decision sees the numbers a
/// fresh scan would produce.
fn derive_splits(
    est: &mut Estimator<'_>,
    out: &mut Vec<u32>,
    start: usize,
    end: usize,
    whole: &RangeEstimate,
) {
    if end - start < MIN_SEQUENCES_BLOCK_SPLITTING || out.len() >= MAX_NB_BLOCK_SPLITS {
        return;
    }
    let mid = (start + end) / 2;
    let left = est.estimate_scanned(start, mid);
    let right_counts = whole.counts.suffix_after(&left.counts);
    let right = RangeEstimate {
        size: est.cost_range(mid, end, &right_counts),
        counts: right_counts,
    };
    if left.size + right.size < whole.size {
        derive_splits(est, out, start, mid, &left);
        out.push(mid as u32);
        derive_splits(est, out, mid, end, &right);
    }
}

/// The remembered table for one entropy stream while partitions roll:
/// borrowed from the caller until a partition replaces it, owned after.
enum Rolling<'a, T> {
    Borrowed(Option<&'a T>),
    Owned(Option<T>),
}

impl<T> Rolling<'_, T> {
    fn as_option(&self) -> Option<&T> {
        match self {
            Self::Borrowed(o) => *o,
            Self::Owned(o) => o.as_ref(),
        }
    }

    /// Drop the owned table (if any) back to nothing, pooled by `recycle`.
    fn take_owned(&mut self, recycle: impl FnOnce(T)) {
        if let Self::Owned(o) = self {
            if let Some(t) = o.take() {
                recycle(t);
            }
        }
    }
}

/// Encode each partition as its own block, rolling the remembered entropy
/// tables across the cut (a later partition repeats the table the previous
/// one wrote — the treeless/repeat savings survive the split). Returns the
/// net outcome against the block-start tables for the caller's adoption.
#[allow(clippy::too_many_arguments)]
fn emit_partitions<M: Matcher>(
    matcher: &mut M,
    block_size: usize,
    last_block: bool,
    lits: &[u8],
    seqs: &[SeqWord],
    lit_prefix: &[u32],
    match_prefix: &[u32],
    partitions: &[u32],
    last_huff_table: Option<&huff0_encoder::HuffmanTable>,
    default_tables: (&FSETable, &FSETable, &FSETable),
    previous_tables: (&Option<FSETable>, &Option<FSETable>, &Option<FSETable>),
    dict_entropy: DictEntropy,
    output: &mut Vec<u8>,
    literals_gate_hold: &mut bool,
    fse: &mut FseBuildScratch,
    huff: &mut huff0_encoder::HuffScratch,
) -> SplitOutcome {
    let nb_seq = seqs.len();
    let num_splits = partitions.len();
    let header_base = output.len() - 3;
    let mut cur_huff: Rolling<'_, huff0_encoder::HuffmanTable> = Rolling::Borrowed(last_huff_table);
    let mut huff_written = false;
    let mut roll_ll = Rolling::Borrowed(previous_tables.0.as_ref());
    let mut roll_ml = Rolling::Borrowed(previous_tables.1.as_ref());
    let mut roll_of = Rolling::Borrowed(previous_tables.2.as_ref());
    let mut dict = dict_entropy;
    let mut lit_base = 0usize;
    let mut src_base = 0usize;
    let mut prev_bound = 0usize;
    for (i, bound) in partitions
        .iter()
        .copied()
        .chain([nb_seq as u32])
        .enumerate()
    {
        let (a, b) = (prev_bound, bound as usize);
        prev_bound = b;
        let is_last_partition = i == num_splits;
        // The final partition takes everything through the block's end,
        // including the literals trailing the last sequence.
        let lit_end = if is_last_partition {
            lits.len()
        } else {
            lit_prefix[b] as usize
        };
        let src_bytes = if is_last_partition {
            block_size - src_base
        } else {
            lit_end - lit_base + (match_prefix[b] - match_prefix[a]) as usize
        };
        // The caller reserved the first partition's header bytes; every
        // later partition appends its own before encoding.
        let header_at = if i == 0 {
            output.len() - 3
        } else {
            output.extend_from_slice(&[0u8; 3]);
            output.len() - 3
        };
        let before = output.len();
        let outcome = encode_staged_block(
            matcher,
            StagedBlock::Seqs {
                literals: &lits[lit_base..lit_end],
                seqs: &seqs[a..b],
            },
            cur_huff.as_option(),
            default_tables,
            (
                roll_ll.as_option(),
                roll_ml.as_option(),
                roll_of.as_option(),
            ),
            dict,
            output,
            literals_gate_hold,
            fse,
            huff,
        );
        // Only the zero-sequence early gate returns Raw; every partition
        // here carries sequences.
        let tables = match outcome {
            BlockOutcome::Encoded(tables) => tables,
            BlockOutcome::Raw => unreachable!("sequenced partition took the zero-sequence gate"),
        };
        let csize = output.len() - before;
        if csize >= src_bytes {
            // Incompressible partition: a raw block here would break the
            // repcode continuity the staged parse assumed (see the module
            // docs), so restart the whole block unsplit from the
            // block-start entropy state. The abandoned fresh and rolled
            // tables return to the pools.
            for mode in [tables.ll, tables.ml, tables.of] {
                if let PrevTable::New(t) = mode {
                    t.recycle(fse);
                }
            }
            if let Some(t) = tables.huff {
                t.recycle_aligned(huff);
            }
            roll_ll.take_owned(|t| t.recycle(fse));
            roll_ml.take_owned(|t| t.recycle(fse));
            roll_of.take_owned(|t| t.recycle(fse));
            cur_huff.take_owned(|t| t.recycle_aligned(huff));
            output.truncate(header_base);
            output.extend_from_slice(&[0u8; 3]);
            return SplitOutcome::Single(encode_staged_block(
                matcher,
                StagedBlock::Seqs {
                    literals: lits,
                    seqs,
                },
                last_huff_table,
                default_tables,
                (
                    previous_tables.0.as_ref(),
                    previous_tables.1.as_ref(),
                    previous_tables.2.as_ref(),
                ),
                dict_entropy,
                output,
                literals_gate_hold,
                fse,
                huff,
            ));
        }
        let mut header = [0u8; 3];
        BlockHeader {
            last_block: last_block && is_last_partition,
            block_type: BlockType::Compressed,
            block_size: csize as u32,
        }
        .serialize_into(&mut header);
        output[header_at..header_at + 3].copy_from_slice(&header);
        // Roll the remembered tables: a stream that wrote a table hands it
        // to the next partition, `Clear` forgets the remembered one, and
        // the dictionary seeding only survives streams that keep theirs.
        match tables.huff {
            Some(t) => {
                cur_huff.take_owned(|old| old.recycle_aligned(huff));
                cur_huff = Rolling::Owned(Some(t));
                huff_written = true;
                dict.huff = false;
            },
            None => {},
        }
        for (rolling, mode, dict_flag) in [
            (&mut roll_ll, tables.ll, &mut dict.ll),
            (&mut roll_ml, tables.ml, &mut dict.ml),
            (&mut roll_of, tables.of, &mut dict.of),
        ] {
            match mode {
                PrevTable::New(t) => {
                    rolling.take_owned(|old| old.recycle(fse));
                    *rolling = Rolling::Owned(Some(t));
                    *dict_flag = false;
                },
                PrevTable::Keep => {},
                PrevTable::Clear => {
                    rolling.take_owned(|old| old.recycle(fse));
                    *rolling = Rolling::Owned(None);
                    *dict_flag = false;
                },
            }
        }
        lit_base = lit_end;
        src_base += src_bytes;
    }
    debug_assert_eq!(src_base, block_size);
    SplitOutcome::Emitted(BlockTables {
        huff: if huff_written {
            match cur_huff {
                Rolling::Owned(Some(t)) => Some(t),
                _ => None,
            }
        } else {
            None
        },
        ll: net_prev(roll_ll),
        ml: net_prev(roll_ml),
        of: net_prev(roll_of),
    })
}

/// Net per-stream outcome against the block-start table: an owned
/// surviving table is `New`, an owned empty slot is `Clear` (a partition
/// replaced or invalidated the remembered table), still-borrowed is
/// `Keep`.
fn net_prev(rolling: Rolling<'_, FSETable>) -> PrevTable {
    match rolling {
        Rolling::Borrowed(_) => PrevTable::Keep,
        Rolling::Owned(Some(t)) => PrevTable::New(t),
        Rolling::Owned(None) => PrevTable::Clear,
    }
}

/// Size estimation for one candidate range, mirroring libzstd's
/// `ZSTD_buildEntropyStatisticsAndEstimateSubBlockSize`: every estimate
/// runs the real table builds and the same selection the emission would
/// run against the block-start entropy state, then costs the streams
/// under the selected tables (so the numbers track what emission
/// produces, not a lower bound).
struct Estimator<'a> {
    literals: &'a [u8],
    /// See [`SplitScratch::lit_prefix`]; slices the literal range of a
    /// sequence range.
    lit_prefix: &'a [u32],
    seqs: &'a [SeqWord],
    last_huff: Option<&'a huff0_encoder::HuffmanTable>,
    prev: (
        &'a Option<FSETable>,
        &'a Option<FSETable>,
        &'a Option<FSETable>,
    ),
    defaults: (&'a FSETable, &'a FSETable, &'a FSETable),
    dict: DictEntropy,
    fse: &'a mut FseBuildScratch,
    huff: &'a mut huff0_encoder::HuffScratch,
    fse_desc: &'a mut Vec<u8>,
}

impl Estimator<'_> {
    /// Scan a range no parent estimated and cost it.
    fn estimate_scanned(&mut self, start: usize, end: usize) -> RangeEstimate {
        let counts = self.scan_counts(start, end);
        let size = self.cost_range(start, end, &counts);
        RangeEstimate { size, counts }
    }

    /// One pass over the range's literals and packed sequences filling the
    /// histograms every cost below consumes.
    fn scan_counts(&self, start: usize, end: usize) -> RangeCounts {
        let lit_a = self.lit_prefix[start] as usize;
        let lit_b = self.lit_prefix[end] as usize;
        let mut counts = RangeCounts {
            lit: [0; 256],
            ll: [0; SEQ_CODE_SPACE],
            ml: [0; SEQ_CODE_SPACE],
            of: [0; SEQ_CODE_SPACE],
            add_bits: 0,
        };
        // An empty literal range keeps the all-zero histogram (the cost's
        // `n == 0` early return); `histogram_literals` requires at least one
        // byte (its tail scan assumes a live symbol).
        if lit_b > lit_a {
            histogram_literals(&self.literals[lit_a..lit_b], &mut counts.lit);
        }
        for &w in &self.seqs[start..end] {
            let packed = w.codes;
            // Wire codes never reach 64; the mask keeps the increments
            // bounds-check-free like the emission's histograms.
            counts.ll[(packed & 0x3f) as usize] += 1;
            counts.ml[((packed >> 8) & 0x3f) as usize] += 1;
            counts.of[((packed >> 16) & 0x3f) as usize] += 1;
            counts.add_bits += w.add_nb as u32;
        }
        counts
    }

    /// Estimated bytes of one range's block (block header included); pure
    /// in the histograms, no O(range) work.
    fn cost_range(&mut self, start: usize, end: usize, counts: &RangeCounts) -> usize {
        let n_lit = self.lit_prefix[end] as usize - self.lit_prefix[start] as usize;
        self.cost_literals(n_lit, &counts.lit)
            + self.cost_sequences(&self.seqs[start..end], counts)
            + 3
    }

    /// Estimated bytes of the literals section: the same raw/RLE/repeat/
    /// fresh-table decision [`compress_literals`](super::compressed) makes,
    /// costed through the exact code lengths instead of encoding. Pure in
    /// the histogram — uniformity is `exactly one live symbol`, which is
    /// what a byte-wise `is_uniform` scan would report.
    fn cost_literals(&mut self, n: usize, counts: &[usize; 256]) -> usize {
        // The raw and RLE forms share the size-format ladder.
        let raw_header = 1 + (n > 31) as usize + (n > 4095) as usize;
        if n == 0 {
            return 1;
        }
        let mut max_symbol = 0usize;
        let mut live = 0usize;
        for (s, &c) in counts.iter().enumerate() {
            if c > 0 {
                max_symbol = s;
                live += 1;
            }
        }
        if live == 1 {
            return raw_header + 1;
        }
        let total = n as f64;
        let mut entropy_bits = 0.0f64;
        for &c in &counts[..=max_symbol] {
            if c > 0 {
                entropy_bits -= (c as f64) * entropy_log2(c as f64 / total);
            }
        }
        // The exact entropy gate: even a fresh table cannot win.
        if entropy_bits + 160.0 + total * 0.02 >= total * 8.0 {
            return raw_header + n;
        }
        let costs = &counts[..=max_symbol];
        let table = huff0_encoder::HuffmanTable::build_from_counts_into(costs, self.huff);
        huff0_encoder::write_table_desc(&table, self.fse, self.huff);
        let desc = self.huff.desc.len();
        let new_cost = table.estimate_compressed_size(costs);
        // The treeless-vs-fresh-table comparison, exactly as emission
        // runs it (the dictionary-seeded table enters through the same
        // slot).
        let stream_cost = match self.last_huff.filter(|t| t.can_encode(&table).is_some()) {
            Some(prev) => {
                let old_cost = prev.estimate_compressed_size(costs);
                if desc + 12 >= n || old_cost <= desc + new_cost {
                    old_cost
                } else {
                    desc + new_cost
                }
            },
            None => desc + new_cost,
        };
        table.recycle_aligned(self.huff);
        let single_stream = n < 256 || (self.dict.huff && n < 1024);
        let header = if n < 1024 {
            2
        } else {
            3
        };
        let jump = if single_stream {
            0
        } else {
            6
        };
        let compressed = stream_cost + header + jump;
        // Emission falls back to raw when the encoded section does not
        // shrink the literals.
        if compressed >= n {
            raw_header + n
        } else {
            compressed
        }
    }

    /// Estimated bytes of the sequences section: per-stream table sizes
    /// and stream costs under the same selection the emission runs, plus
    /// the shared add-bits payload and the section header. The histograms
    /// arrive prescanned ([`RangeCounts`]); `seqs` is read for nothing but
    /// the first and last codes.
    fn cost_sequences(&mut self, seqs: &[SeqWord], counts: &RangeCounts) -> usize {
        let n = seqs.len();
        let first = seqs[0].codes;
        let last = seqs[n - 1].codes;
        let ll = self.stream_size(
            counts.ll,
            n,
            first as u8,
            last as u8,
            self.defaults.0,
            self.prev.0.as_ref(),
            self.dict.ll,
            6,
            9,
        );
        let ml = self.stream_size(
            counts.ml,
            n,
            (first >> 8) as u8,
            (last >> 8) as u8,
            self.defaults.1,
            self.prev.1.as_ref(),
            self.dict.ml,
            6,
            9,
        );
        let of = self.stream_size(
            counts.of,
            n,
            (first >> 16) as u8,
            (last >> 16) as u8,
            self.defaults.2,
            self.prev.2.as_ref(),
            self.dict.of,
            5,
            8,
        );
        ll + ml
            + of
            + (counts.add_bits >> 3) as usize
            + 2
            + (n >= 128) as usize
            + (n >= 0x7f00) as usize
    }

    /// One stream's description bytes plus its symbol cost under the
    /// selected mode; built tables return their transition buffers to the
    /// pool after measuring.
    fn stream_size(
        &mut self,
        counts: [u32; SEQ_CODE_SPACE],
        n: usize,
        first: u8,
        last: u8,
        default: &FSETable,
        prev: Option<&FSETable>,
        dict_seeded: bool,
        default_norm_log: u32,
        max_log: u8,
    ) -> usize {
        // Selection consumes its copy (normalization mutates the last
        // code's count); costs read the original histogram.
        let mut sel = counts;
        let mode = select_from_counts(
            &mut sel,
            n,
            first,
            last,
            default,
            prev,
            dict_seeded,
            default_norm_log,
            max_log,
            self.fse,
        );
        let max_symbol = counts.iter().rposition(|&c| c > 0).unwrap_or(0);
        match mode {
            FseTableMode::Predefined(t) | FseTableMode::Repeat(t) => {
                (repeat_bit_cost(t, &counts, max_symbol).unwrap_or(0.0) / 8.0) as usize
            },
            FseTableMode::Rle { table, .. } => {
                table.recycle(self.fse);
                1
            },
            FseTableMode::Encoded(t) => {
                let bits = repeat_bit_cost(&t, &counts, max_symbol).unwrap_or(0.0);
                let desc = self.fse_desc_len(&t);
                t.recycle(self.fse);
                desc + (bits / 8.0) as usize
            },
        }
    }

    /// Serialized size of one FSE table description.
    fn fse_desc_len(&mut self, table: &FSETable) -> usize {
        let desc = &mut *self.fse_desc;
        desc.clear();
        let mut writer = BitWriter::from(&mut *desc);
        table.write_table(&mut writer);
        writer.flush();
        desc.len()
    }
}
