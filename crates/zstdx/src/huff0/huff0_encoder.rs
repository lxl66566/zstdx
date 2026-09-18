use alloc::{boxed::Box, vec::Vec};

use crate::{bit_io::BitWriter, fse::fse_encoder};

pub(crate) struct HuffmanEncoder<'output, 'table, V: AsMut<Vec<u8>>> {
    table: &'table HuffmanTable,
    writer: &'output mut BitWriter<V>,
}

impl<V: AsMut<Vec<u8>>> HuffmanEncoder<'_, '_, V> {
    pub fn new<'o, 't>(
        table: &'t HuffmanTable,
        writer: &'o mut BitWriter<V>,
    ) -> HuffmanEncoder<'o, 't, V> {
        HuffmanEncoder { table, writer }
    }

    /// Encodes the data using the provided table
    /// Writes
    /// * Table description
    /// * Encoded data
    /// * Padding bits to fill up last byte
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn encode(&mut self, data: &[u8], with_table: bool) {
        self.encode_with(
            data,
            with_table,
            &mut fse_encoder::FseBuildScratch::default(),
            &mut HuffScratch::default(),
        );
    }

    /// [`Self::encode`] with pooled build scratch: repeated small-block
    /// encodes recycle the package-merge and FSE weight-table buffers
    /// instead of allocating per call.
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn encode_with(
        &mut self,
        data: &[u8],
        with_table: bool,
        fse: &mut fse_encoder::FseBuildScratch,
        huff: &mut HuffScratch,
    ) {
        if with_table {
            self.write_table_with(fse, huff);
        }
        Self::encode_stream(self.table, self.writer, data);
    }

    /// Encodes the data using the provided table in 4 concatenated streams
    /// Writes
    /// * Table description
    /// * Jumptable
    /// * Encoded data in 4 streams, each padded to fill the last byte
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn encode4x(&mut self, data: &[u8], with_table: bool) {
        self.encode4x_with(
            data,
            with_table,
            &mut fse_encoder::FseBuildScratch::default(),
            &mut HuffScratch::default(),
        );
    }

    /// [`Self::encode4x`] with pooled build scratch (see
    /// [`Self::encode_with`]).
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn encode4x_with(
        &mut self,
        data: &[u8],
        with_table: bool,
        fse: &mut fse_encoder::FseBuildScratch,
        huff: &mut HuffScratch,
    ) {
        if with_table {
            self.write_table_with(fse, huff);
        }
        self.encode4x_only(data);
    }

    /// The four-stream form without a table description: the caller appended
    /// the description (byte-aligned) beforehand or chose the treeless form.
    pub(crate) fn encode4x_only(&mut self, data: &[u8]) {
        assert!(data.len() >= 4);

        // Split data in 4 equally sized parts (the last one might be a bit smaller than the rest)
        let split_size = data.len().div_ceil(4);
        let src1 = &data[..split_size];
        let src2 = &data[split_size..split_size * 2];
        let src3 = &data[split_size * 2..split_size * 3];
        let src4 = &data[split_size * 3..];

        // Reserve space for the jump table, will be changed later
        let size_idx = self.writer.index();
        self.writer.write_bits(0u16, 16);
        self.writer.write_bits(0u16, 16);
        self.writer.write_bits(0u16, 16);

        // Write the 4 streams, noting the sizes of the encoded streams
        let index_before = self.writer.index();
        Self::encode_stream(self.table, self.writer, src1);
        let size1 = (self.writer.index() - index_before) / 8;

        let index_before = self.writer.index();
        Self::encode_stream(self.table, self.writer, src2);
        let size2 = (self.writer.index() - index_before) / 8;

        let index_before = self.writer.index();
        Self::encode_stream(self.table, self.writer, src3);
        let size3 = (self.writer.index() - index_before) / 8;

        Self::encode_stream(self.table, self.writer, src4);

        // Sanity check, if this doesn't hold we produce a broken stream
        assert!(u16::try_from(size1).is_ok());
        assert!(u16::try_from(size2).is_ok());
        assert!(u16::try_from(size3).is_ok());

        // Update the jumptable with the real sizes
        self.writer.change_bits(size_idx, size1 as u16, 16);
        self.writer.change_bits(size_idx + 16, size2 as u16, 16);
        self.writer.change_bits(size_idx + 32, size3 as u16, 16);
    }

    /// The one-stream form without a table description: the caller appended
    /// the description (byte-aligned) beforehand or chose the treeless form.
    pub(crate) fn encode_stream_only(&mut self, data: &[u8]) {
        Self::encode_stream(self.table, self.writer, data);
    }

    /// Encode one stream and pad it to fill the last byte
    fn encode_stream<VV: AsMut<Vec<u8>>>(
        table: &HuffmanTable,
        writer: &mut BitWriter<VV>,
        data: &[u8],
    ) {
        // The batched writer performs the same bit accumulation as one
        // write_bits call per symbol (data reversed, since the format reads
        // the stream back to front), so the output is bit-identical.
        writer.write_packed_codes_rev(&table.packed, &table.aligned, table.uniform_nb, data);

        let bits_to_fill = writer.misaligned();
        if bits_to_fill == 0 {
            writer.write_bits(1u32, 8);
        } else {
            writer.write_bits(1u32, bits_to_fill);
        }
    }

    #[cfg(any(test, feature = "fuzz_exports"))]
    pub(super) fn weights(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.table.nsym as usize);
        self.table.write_weights_into(&mut out);
        out
    }

    #[cfg(any(test, feature = "fuzz_exports"))]
    fn write_table_with(&mut self, fse: &mut fse_encoder::FseBuildScratch, huff: &mut HuffScratch) {
        write_table_desc(self.table, fse, huff);
        self.writer.append_bytes(&huff.desc);
    }
}

/// Serialize the table's weight description into `scratch.desc` (cleared
/// first), libzstd-exact (`HUF_writeCTable_wksp` + `HUF_compressWeights`):
/// FSE-compressed weights when they beat the raw 4-bit form under libzstd's
/// gate, raw 4-bit weights otherwise. The last symbol's weight is never
/// written (the decoder derives it from the Kraft sum).
pub(crate) fn write_table_desc(
    table: &HuffmanTable,
    fse: &mut fse_encoder::FseBuildScratch,
    scratch: &mut HuffScratch,
) {
    table.write_weights_into(&mut scratch.wire_weights);
    let weights = &scratch.wire_weights[..scratch.wire_weights.len() - 1]; // dont encode last weight
    let desc = &mut scratch.desc;
    desc.clear();
    let n = weights.len();
    if n >= 4 {
        // libzstd's HUF_compressWeights pre-checks: single-symbol and
        // all-distinct weight streams compress worse than the raw form.
        let mut counts = [0u32; MAX_CODE_LENGTH + 1];
        let mut max_w = 0usize;
        let mut max_count = 0u32;
        for &w in weights {
            let w = w as usize;
            counts[w] += 1;
            max_w = max_w.max(w);
            max_count = max_count.max(counts[w]);
        }
        let mut norm = [0i32; MAX_CODE_LENGTH + 1];
        let normalized = max_count != n as u32 && max_count > 1 && {
            let table_log = fse_encoder::optimal_table_log(6, n, max_w);
            fse_encoder::normalize_count(&mut norm, table_log, &counts, n, max_w, false)
        };
        if normalized {
            let table_log = fse_encoder::optimal_table_log(6, n, max_w);
            let fse_table =
                fse_encoder::build_table_from_probabilities_into(&norm[..=max_w], table_log, fse);
            if write_fse_weights(fse_table, weights, &mut scratch.desc_fse, fse) {
                let fse_len = scratch.desc_fse.len();
                // libzstd's FSE-vs-raw gate in HUF_writeCTable_wksp. The raw
                // form's header cannot count past 128 weights, so wider
                // alphabets keep the FSE form unconditionally.
                if fse_len > 1 && (fse_len < n / 2 || n > 128) {
                    desc.push(fse_len as u8);
                    desc.extend_from_slice(&scratch.desc_fse);
                    return;
                }
            }
        } else if n > 128 {
            // Only reachable with a single distinct weight over more than
            // 128 symbols (a uniform code over a full 256-symbol alphabet;
            // the literals entropy gate keeps compressible blocks away from
            // it, direct table dumps do not): the raw form cannot address
            // the header and a one-symbol FSE table has only zero-bit
            // entries, which no stream can terminate. Park one state on an
            // unused weight value so the dominant symbol keeps >= 1-bit
            // entries.
            let phantom = if max_w == MAX_CODE_LENGTH {
                MAX_CODE_LENGTH - 1
            } else {
                max_w + 1
            };
            let mut norm = [0i32; MAX_CODE_LENGTH + 1];
            norm[max_w] = 31;
            norm[phantom] = 1;
            let fse_table = fse_encoder::build_table_from_probabilities_into(
                &norm[..=phantom.max(max_w)],
                5,
                fse,
            );
            if write_fse_weights(fse_table, weights, &mut scratch.desc_fse, fse) {
                let fse_len = scratch.desc_fse.len();
                if fse_len < 128 {
                    desc.push(fse_len as u8);
                    desc.extend_from_slice(&scratch.desc_fse);
                    return;
                }
            }
        }
    }
    debug_assert!(n <= 128);
    // Raw 4-bit weights; the header counts the stored (non-last) weights.
    desc.push(n as u8 + 127);
    let (pairs, remainder) = weights.as_chunks::<2>();
    for pair in pairs {
        desc.push((pair[0] << 4) | pair[1]);
    }
    if !remainder.is_empty() {
        desc.push(remainder[0] << 4);
    }
}

/// FSE-encode `weights` with `table` into `out` (cleared first). Returns
/// false when the stream cannot be built (normalization corner cases).
fn write_fse_weights(
    table: fse_encoder::FSETable,
    weights: &[u8],
    out: &mut Vec<u8>,
    fse: &mut fse_encoder::FseBuildScratch,
) -> bool {
    out.clear();
    {
        let mut bits = BitWriter::from(&mut *out);
        let mut encoder = fse_encoder::FSEEncoder::new(table, &mut bits);
        encoder.encode_interleaved(weights);
        encoder.finish(fse);
        // Materialize the stream's trailing byte: the writer keeps
        // unflushed bits in its accumulator, and this buffer would be
        // dropped right here.
        bits.flush();
    }
    true
}

#[derive(Clone)]
pub struct HuffmanTable {
    /// Alphabet prefix length: symbols at or beyond it are dead by
    /// construction (`nb` reads as 0).
    nsym: u16,
    /// Per-symbol codes packed as `(code << 4) | num_bits` so the encoding
    /// loop loads one u16 instead of an 8-byte tuple (the table stays fully
    /// L1-resident). Weight redistribution caps codes at 9 bits, so the
    /// packing cannot overflow.
    packed: [u16; 256],
    /// Left-aligned form for the dual-accumulator stream loop: the code in
    /// the top `nb` bits of a u64, `nb` in the low nibble. One load feeds
    /// the container shift, the OR and the bit counter. Pooled behind the
    /// build scratch: dead symbols are never read, so the box is recycled
    /// without clearing.
    aligned: Box<[u64; 256]>,
    /// The common code length when every symbol shares one length (flat
    /// alphabets such as 9..16 symbols), zero otherwise. Fixed-length codes
    /// let the stream encoder pack two symbols per byte without the
    /// variable-length accumulation chain.
    uniform_nb: u8,
}

impl HuffmanTable {
    // Only the round-trip helpers (huff0::round_trip*) need raw-count tables.
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn build_from_data(data: &[u8]) -> Self {
        let mut counts = [0; 256];
        let mut max = 0;
        for x in data {
            counts[*x as usize] += 1;
            max = max.max(*x);
        }

        Self::build_from_counts(&counts[..=max as usize])
    }

    /// Build from the format's per-symbol code lengths (0 = symbol
    /// absent), e.g. a dictionary table's
    /// (`huff0_decoder::HuffmanTable::code_lengths`).
    pub fn build_from_code_lengths(lengths: &[u8]) -> Self {
        let max_len = lengths.iter().copied().max().unwrap_or(1) as usize;
        let weights: Vec<usize> = lengths
            .iter()
            .map(|&len| {
                if len == 0 {
                    0
                } else {
                    max_len - len as usize + 1
                }
            })
            .collect();
        Self::build_from_weights(&weights)
    }

    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn build_from_counts(counts: &[usize]) -> Self {
        assert!(counts.len() <= 256);
        let mut scratch = HuffScratch::default();
        Self::build_from_counts_into(counts, &mut scratch)
    }

    /// The pooled [`Self::build_from_counts`]: the tree-walk scratch and
    /// the aligned-form box are recycled through `scratch`.
    pub(crate) fn build_from_counts_into(counts: &[usize], scratch: &mut HuffScratch) -> Self {
        assert!(counts.len() <= 256);
        build_lengths_into(counts, MAX_CODE_LENGTH as u8, scratch);
        let n = counts.len();
        let max_len = scratch.lengths[..n].iter().copied().max().unwrap_or(1) as usize;
        let weights = &mut scratch.weights;
        for (w, &len) in weights[..n].iter_mut().zip(scratch.lengths[..n].iter()) {
            *w = if len == 0 {
                0
            } else {
                (max_len - len as usize + 1) as u8
            };
        }
        let aligned = scratch.take_aligned();
        build_from_weights_slice(&scratch.weights[..n], aligned)
    }

    /// Return the aligned-form buffer to a build pool (see
    /// [`HuffScratch`]); the table must not be used afterwards.
    pub(crate) fn recycle_aligned(self, scratch: &mut HuffScratch) {
        scratch.recycle_aligned(self.aligned);
    }

    pub fn build_from_weights(weights: &[usize]) -> Self {
        debug_assert!(weights.iter().all(|&w| w <= MAX_CODE_LENGTH));
        let mut narrow = [0u8; 256];
        for (dst, &w) in narrow.iter_mut().zip(weights.iter()) {
            *dst = w as u8;
        }
        build_from_weights_slice(&narrow[..weights.len()], Box::new([0; 256]))
    }

    /// Per-symbol code lengths (0 = symbol not covered by the table).
    /// Feeds the matcher's literal-cost pricing feedback (see
    /// `Matcher::note_literal_costs`).
    pub(crate) fn code_lengths(&self) -> [u8; 256] {
        let mut lens = [0u8; 256];
        for (i, &p) in self.packed.iter().enumerate() {
            lens[i] = (p & 0xf) as u8;
        }
        lens
    }

    /// libzstd's HUF_estimateCompressedSize: the exact cost of coding
    /// `counts` under this table, rounded up to bytes. `counts` may be
    /// shorter or longer than the table's alphabet (entries past it count
    /// zero bits either way).
    pub(crate) fn estimate_compressed_size(&self, counts: &[usize]) -> usize {
        counts
            .iter()
            .enumerate()
            .map(|(s, &c)| c * (self.packed[s] & 0xf) as usize)
            .sum::<usize>()
            .div_ceil(8)
    }

    /// Per-symbol wire weights into a recycled buffer (the format's
    /// weight form; see [`write_table_desc`]).
    pub(crate) fn write_weights_into(&self, out: &mut Vec<u8>) {
        out.clear();
        let max = (0..self.nsym as usize)
            .map(|s| (self.packed[s] & 0xf) as u8)
            .max()
            .unwrap();
        out.extend((0..self.nsym as usize).map(|s| {
            let nb = (self.packed[s] & 0xf) as u8;
            if nb == 0 {
                0
            } else {
                max - nb + 1
            }
        }));
    }

    pub fn can_encode(&self, other: &Self) -> Option<usize> {
        if other.nsym > self.nsym {
            return None;
        }
        let mut sum = 0;
        for s in 0..other.nsym as usize {
            let (other_nb, self_nb) = ((other.packed[s] & 0xf) as u8, (self.packed[s] & 0xf) as u8);
            if other_nb != 0 && self_nb == 0 {
                return None;
            }
            sum += other_nb.abs_diff(self_nb) as usize;
        }
        Some(sum)
    }
}

/// Core of [`HuffmanTable::build_from_weights`] over u8 weights and a
/// caller-owned aligned-form buffer (pooled by the `scratch` variants).
fn build_from_weights_slice(weights: &[u8], aligned: Box<[u64; 256]>) -> HuffmanTable {
    debug_assert!(weights.iter().all(|&w| w <= MAX_CODE_LENGTH as u8));
    let mut bucket_counts = [0u16; MAX_CODE_LENGTH + 1];
    for &weight in weights {
        bucket_counts[weight as usize] += 1;
    }
    let mut bucket_start = [0u16; MAX_CODE_LENGTH + 1];
    let mut total = 0u16;
    // Uniform-code detection rides along: every live symbol shares one
    // code length exactly when only one weight bucket is non-empty.
    let mut uniform_w = 0u8;
    let mut uniform = true;
    for weight in 1..=MAX_CODE_LENGTH {
        bucket_start[weight] = total;
        let count = bucket_counts[weight];
        if count != 0 {
            if uniform_w == 0 {
                uniform_w = weight as u8;
            } else {
                uniform = false;
            }
        }
        total += count;
    }

    let mut sorted = [0u8; 256];
    let mut cursor = bucket_start;
    for (symbol, &weight) in weights.iter().enumerate() {
        if weight != 0 {
            sorted[cursor[weight as usize] as usize] = symbol as u8;
            cursor[weight as usize] += 1;
        }
    }

    // Prepare huffman table with placeholders
    let mut table = HuffmanTable {
        nsym: weights.len() as u16,
        packed: [0; 256],
        aligned,
        uniform_nb: 0,
    };

    // Determine the number of bits needed for codes with the lowest weight
    let weight_sum = (1..=MAX_CODE_LENGTH)
        .map(|weight| (bucket_counts[weight] as usize) << (weight - 1))
        .sum::<usize>();
    assert!(weight_sum.is_power_of_two(), "This is an internal error");
    let max_num_bits = highest_bit_set(weight_sum) - 1; // this is a log_2 of a clean power of two

    // Starting at the symbols with the lowest weight we update the placeholders in the table
    let mut current_code = 0;
    let mut current_weight = 0;
    for weight in 1..=MAX_CODE_LENGTH {
        let start = bucket_start[weight] as usize;
        let end = start + bucket_counts[weight] as usize;
        if start == end {
            continue;
        }
        // The code shifts by the difference of the weights to allow for enough unique values
        current_code >>= weight - current_weight;
        // Encoding a symbol of this weight will take less bits than the previous weight
        let current_num_bits = max_num_bits - weight + 1;
        // Run the next update when the weight changes again
        current_weight = weight;
        for &symbol in &sorted[start..end] {
            debug_assert!(current_num_bits <= 11 && current_code <= 0xfff);
            table.packed[symbol as usize] = ((current_code << 4) | current_num_bits) as u16;
            table.aligned[symbol as usize] =
                ((current_code as u64) << (64 - current_num_bits)) | current_num_bits as u64;
            current_code += 1;
        }
    }
    if uniform && total as usize >= 2 {
        table.uniform_nb = (max_num_bits - uniform_w as usize + 1) as u8;
    }

    table
}

/// Assert that the provided value is greater than zero, and returns index of the first set bit
fn highest_bit_set(x: usize) -> usize {
    assert!(x > 0);
    usize::BITS as usize - x.leading_zeros() as usize
}

impl HuffmanTable {
    /// `(code, num_bits)` of `symbol` (test-facing form of the packed
    /// entry).
    #[cfg(test)]
    fn code_of(&self, symbol: usize) -> (u32, u8) {
        let p = self.packed[symbol];
        ((p >> 4) as u32, (p & 0xf) as u8)
    }
}

#[test]
fn huffman() {
    let table = HuffmanTable::build_from_weights(&[2, 2, 2, 1, 1]);
    assert_eq!(table.code_of(0), (1, 2));
    assert_eq!(table.code_of(1), (2, 2));
    assert_eq!(table.code_of(2), (3, 2));
    assert_eq!(table.code_of(3), (0, 3));
    assert_eq!(table.code_of(4), (1, 3));

    let table = HuffmanTable::build_from_weights(&[4, 3, 2, 0, 1, 1]);
    assert_eq!(table.code_of(0), (1, 1));
    assert_eq!(table.code_of(1), (1, 2));
    assert_eq!(table.code_of(2), (1, 3));
    assert_eq!(table.code_of(3), (0, 0));
    assert_eq!(table.code_of(4), (0, 4));
    assert_eq!(table.code_of(5), (1, 4));
}

/// Pooled scratch for the per-block Huffman build (held in the compressor
/// state): the tree-walk node table, the rank-bucket sort positions, the
/// per-symbol length/weight scratch, the wire weight stream, and retired
/// aligned-form boxes.
pub(crate) struct HuffScratch {
    /// The two-queue builder's node table (see [`build_lengths_into`]):
    /// index 0 is the count barrier, 1..=256 the sorted leaves, 257.. the
    /// created internal nodes (≤ one per leaf).
    nodes: Box<[NodeElt; NODES_CAP]>,
    /// Rank-bucket bases and cursors for the count sort (see [`huf_sort`]).
    rank_pos: Box<[(u16, u16); RANK_TABLE]>,
    /// Code lengths per symbol (0 = symbol absent), valid for the build's
    /// alphabet prefix.
    lengths: [u8; 256],
    /// Weights per symbol, same layout.
    weights: [u8; 256],
    /// The literals-section weight stream (one weight per symbol, last
    /// dropped on the wire).
    wire_weights: Vec<u8>,
    /// Serialized weight description (the bytes that follow the literals
    /// header for a fresh table; see [`write_table_desc`]).
    pub(crate) desc: Vec<u8>,
    /// FSE-compressed weight region while probing the description form.
    desc_fse: Vec<u8>,
    /// Retired aligned-form boxes (dead symbols are never read, so a box
    /// recycles without clearing; every byte stays initialized).
    aligned: Vec<Box<[u64; 256]>>,
}

/// Two live tables cover the adopt/replace flow per block.
const HUFF_ALIGNED_CAP: usize = 2;

impl Default for HuffScratch {
    fn default() -> Self {
        Self {
            nodes: Box::new([NodeElt::ZERO; NODES_CAP]),
            rank_pos: Box::new([(0, 0); RANK_TABLE]),
            lengths: [0; 256],
            weights: [0; 256],
            wire_weights: Vec::new(),
            desc: Vec::new(),
            desc_fse: Vec::new(),
            aligned: Vec::new(),
        }
    }
}

impl HuffScratch {
    /// Take an aligned-form box (zero-initialized on first use only; the
    /// build overwrites every entry a later read can touch).
    fn take_aligned(&mut self) -> Box<[u64; 256]> {
        self.aligned.pop().unwrap_or_else(|| Box::new([0; 256]))
    }

    /// Keep a retired aligned-form box beyond the adopt/replace depth.
    fn recycle_aligned(&mut self, v: Box<[u64; 256]>) {
        if self.aligned.len() < HUFF_ALIGNED_CAP {
            self.aligned.push(v);
        }
    }
}

/// Maximum Huffman code length the literals section can carry.
const MAX_CODE_LENGTH: usize = 11;

/// One Huffman tree node (C's `nodeElt`): the same array doubles as the
/// count-sorted leaf list and the created-internal-node arena of the
/// two-queue builder.
#[derive(Clone, Copy)]
struct NodeElt {
    count: u32,
    parent: u16,
    byte: u8,
    nb_bits: u8,
}

impl NodeElt {
    const ZERO: NodeElt = NodeElt {
        count: 0,
        parent: 0,
        byte: 0,
        nb_bits: 0,
    };
}

/// Node-table geometry: barrier at 0, leaves 1..=256, internal nodes from
/// 257 up to one per leaf.
const START_NODE: usize = 257;
const NODES_CAP: usize = START_NODE + 256;

/// Rank-bucket sort geometry (C's `HUF_sort`): counts below the cutoff get
/// an exact bucket each, everything above folds into log2 buckets.
const RANK_TABLE: usize = 192;
const RANK_LOG_BEGIN: u32 = (RANK_TABLE as u32 - 1) - 32 - 1;
const RANK_DISTINCT_CUTOFF: u32 = RANK_LOG_BEGIN + RANK_LOG_BEGIN.ilog2();

/// The bucket a count lands in (C's `HUF_getIndex`).
#[inline]
fn rank_of(count: u32) -> usize {
    if count < RANK_DISTINCT_CUTOFF {
        count as usize
    } else {
        (count.ilog2() + RANK_LOG_BEGIN) as usize
    }
}

/// Fill `scratch.lengths[..counts.len()]` with code lengths: a two-queue
/// Huffman tree (optimal, unlimited depth) built over the count-sorted
/// leaves, then depth-limited to `max_nb_bits` by C's `HUF_setMaxHeight`
/// Kraft repayment. This is libzstd's `HUF_buildCTable_wksp` structure —
/// O(n) work per build where boundary package-merge pays max_len levels
/// of 2(n-1)-wide merges. Zero-count symbols get length 0; the result is
/// Kraft-exact. Requires at least two used symbols.
fn build_lengths_into(counts: &[usize], max_nb_bits: u8, scratch: &mut HuffScratch) {
    let HuffScratch {
        nodes,
        rank_pos,
        lengths,
        ..
    } = scratch;
    lengths[..counts.len()].fill(0);
    debug_assert!(
        counts.iter().all(|&c| c < (1 << 30)),
        "the two-queue barrier and uncreated-node markers live above 2^30"
    );
    let n = huf_sort(counts, &mut nodes[..], &mut rank_pos[..]);
    debug_assert!(n >= 2, "single-symbol alphabets go through the RLE path");
    debug_assert!(
        usize::from(max_nb_bits) >= n.next_power_of_two().ilog2() as usize,
        "the limit cannot address every symbol"
    );
    build_tree(&mut nodes[..], n);
    set_max_height(&mut nodes[1..], n - 1, max_nb_bits);
    for node in &nodes[1..=n] {
        lengths[node.byte as usize] = node.nb_bits;
    }
    debug_assert_eq!(
        lengths[..counts.len()]
            .iter()
            .map(|&l| if l == 0 {
                0u64
            } else {
                1u64 << (max_nb_bits - l)
            })
            .sum::<u64>(),
        1u64 << max_nb_bits,
        "code lengths must be Kraft-exact"
    );
}

/// Scatter-sort symbols by count descending into `nodes[1..]` (C's
/// `HUF_sort`): exact-count buckets preserve the ascending-symbol scatter
/// order, log2 buckets get a descending count sort. Returns the table
/// index of the last used symbol.
fn huf_sort(counts: &[usize], nodes: &mut [NodeElt], rank_pos: &mut [(u16, u16)]) -> usize {
    rank_pos.fill((0, 0));
    let mut used = 0usize;
    for &c in counts {
        if c > 0 {
            used += 1;
        }
        rank_pos[rank_of(u32::try_from(c).expect("literal count exceeds u32"))].0 += 1;
    }
    for r in (1..RANK_TABLE).rev() {
        rank_pos[r - 1].0 += rank_pos[r].0;
    }
    for slot in rank_pos.iter_mut() {
        slot.1 = slot.0;
    }
    for (sym, &c) in counts.iter().enumerate() {
        let r = rank_of(u32::try_from(c).expect("literal count exceeds u32")) + 1;
        let pos = rank_pos[r].1 as usize;
        rank_pos[r].1 += 1;
        nodes[pos + 1] = NodeElt {
            count: c as u32,
            parent: 0,
            byte: sym as u8,
            nb_bits: 0,
        };
    }
    for slot in &rank_pos[RANK_DISTINCT_CUTOFF as usize..RANK_TABLE - 1] {
        let (base, curr) = (slot.0 as usize, slot.1 as usize);
        if curr - base > 1 {
            nodes[base + 1..=curr].sort_unstable_by_key(|n| core::cmp::Reverse(n.count));
        }
    }
    used
}

/// Build the unlimited-depth Huffman tree over the sorted leaves (C's
/// `HUF_buildTree`): the two-queue merge takes each internal node's
/// children from the smaller of the remaining leaf tail and the created
/// nodes so far, writing parents and then code lengths in one array. Node
/// 0 acts as the count barrier that keeps the leaf index valid after the
/// tail is exhausted.
fn build_tree(nodes: &mut [NodeElt], last_leaf: usize) {
    // last_leaf leaves need last_leaf-1 internal nodes; the root lands at
    // START_NODE + (last_leaf-1) - 1.
    let node_root = START_NODE - 2 + last_leaf;
    let mut node_nb = START_NODE;
    let mut low_s = last_leaf;
    let mut low_n = START_NODE;
    nodes[node_nb].count = nodes[low_s].count + nodes[low_s - 1].count;
    nodes[low_s].parent = node_nb as u16;
    nodes[low_s - 1].parent = node_nb as u16;
    node_nb += 1;
    low_s -= 2;
    for node in &mut nodes[node_nb..=node_root] {
        node.count = 1 << 30;
    }
    nodes[0].count = u32::MAX;
    while node_nb <= node_root {
        // Counts stay under 2^30 (block and dictionary literal totals), so
        // the barrier and the not-yet-created markers can never be picked.
        let n1 = if nodes[low_s].count < nodes[low_n].count {
            let picked = low_s;
            low_s -= 1;
            picked
        } else {
            let picked = low_n;
            low_n += 1;
            picked
        };
        let n2 = if nodes[low_s].count < nodes[low_n].count {
            let picked = low_s;
            low_s -= 1;
            picked
        } else {
            let picked = low_n;
            low_n += 1;
            picked
        };
        nodes[node_nb].count = nodes[n1].count + nodes[n2].count;
        nodes[n1].parent = node_nb as u16;
        nodes[n2].parent = node_nb as u16;
        node_nb += 1;
    }
    nodes[node_root].nb_bits = 0;
    for n in (START_NODE..node_root).rev() {
        nodes[n].nb_bits = nodes[nodes[n].parent as usize].nb_bits + 1;
    }
    for n in 1..=last_leaf {
        nodes[n].nb_bits = nodes[nodes[n].parent as usize].nb_bits + 1;
    }
}

/// Clamp the tree depth to `target` (C's `HUF_setMaxHeight`): every deeper
/// leaf moves up to `target` and the Kraft debt is repaid by demoting the
/// cheapest symbols rank by rank, keeping the code complete. `leaves` is
/// the sorted leaf list (node 0 of the table). Returns the resulting
/// maximum length.
fn set_max_height(leaves: &mut [NodeElt], last_non_null: usize, target: u8) -> u8 {
    /// The empty-rank sentinel for `rank_last`; real positions are >= 0.
    const NO_SYMBOL: i32 = -1;
    let largest_bits = leaves[last_non_null].nb_bits;
    if largest_bits <= target {
        return largest_bits;
    }

    let mut total_cost: i64 = 0;
    let base_cost: u64 = 1 << (largest_bits - target);
    let mut n = last_non_null as i32;

    // Lift every over-deep leaf to `target`, collecting the Kraft debt.
    while leaves[n as usize].nb_bits > target {
        total_cost += (base_cost - (1 << (largest_bits - leaves[n as usize].nb_bits))) as i64;
        leaves[n as usize].nb_bits = target;
        n -= 1;
    }
    // Stop on the deepest rank below the target: the repayment candidates.
    while leaves[n as usize].nb_bits == target {
        n -= 1;
    }
    total_cost >>= largest_bits - target;
    debug_assert!(total_cost > 0);

    // `rank_last[d]` = position of the smallest symbol whose length is
    // `target - d` (a demotion there shortens the rank-d gap); NO_SYMBOL
    // marks an empty rank. Positions are leaf indices; 0 is the most
    // frequent symbol and a valid position.
    let mut rank_last = [NO_SYMBOL; MAX_CODE_LENGTH + 2];
    let mut current_nb_bits = target;
    let mut pos = n;
    while pos >= 0 {
        if leaves[pos as usize].nb_bits < current_nb_bits {
            current_nb_bits = leaves[pos as usize].nb_bits;
            rank_last[(target - current_nb_bits) as usize] = pos;
        }
        pos -= 1;
    }

    while total_cost > 0 {
        // Demote at the shallowest rank whose cheapest symbol costs less
        // than two of the next-shallower rank's (each demotion halves the
        // remaining rank mass).
        let mut n_bits = (total_cost as u32).ilog2() + 1;
        while n_bits > 1 {
            let high_pos = rank_last[n_bits as usize];
            let low_pos = rank_last[n_bits as usize - 1];
            if high_pos == NO_SYMBOL {
                n_bits -= 1;
                continue;
            }
            if low_pos == NO_SYMBOL {
                break;
            }
            let high_total = leaves[high_pos as usize].count;
            let low_total = 2 * leaves[low_pos as usize].count;
            if high_total <= low_total {
                break;
            }
            n_bits -= 1;
        }
        // The next-nonempty-rank skip can only walk upward while a rank is
        // empty; rank 1 always holds a symbol (the smallest leaf).
        while n_bits <= MAX_CODE_LENGTH as u32 && rank_last[n_bits as usize] == NO_SYMBOL {
            n_bits += 1;
        }
        debug_assert!(rank_last[n_bits as usize] != NO_SYMBOL);
        total_cost -= 1 << (n_bits - 1);
        leaves[rank_last[n_bits as usize] as usize].nb_bits += 1;

        if rank_last[n_bits as usize - 1] == NO_SYMBOL {
            rank_last[n_bits as usize - 1] = rank_last[n_bits as usize];
        }
        if rank_last[n_bits as usize] == 0 {
            rank_last[n_bits as usize] = NO_SYMBOL;
        } else {
            rank_last[n_bits as usize] -= 1;
            if leaves[rank_last[n_bits as usize] as usize].nb_bits != target - n_bits as u8 {
                rank_last[n_bits as usize] = NO_SYMBOL;
            }
        }
    }

    // Overshot repayments come back by shortening the most frequent rank.
    while total_cost < 0 {
        if rank_last[1] == NO_SYMBOL {
            while leaves[n as usize].nb_bits == target {
                n -= 1;
            }
            leaves[n as usize + 1].nb_bits -= 1;
            debug_assert!(n >= 0);
            rank_last[1] = n + 1;
            total_cost += 1;
            continue;
        }
        leaves[rank_last[1] as usize + 1].nb_bits -= 1;
        rank_last[1] += 1;
        total_cost += 1;
    }
    target
}

#[cfg(test)]
mod reference {
    //! Boundary package-merge (Larmore-Hirschberg): the optimal
    //! length-limited code, kept as the production builder's optimality
    //! reference (the production path is the two-queue port above).

    use alloc::{vec, vec::Vec};

    fn merge_lengths(counts: &[usize], max_len: usize) -> Vec<usize> {
        #[derive(Clone, Copy)]
        struct Ent {
            weight: u32,
            node: u32,
        }
        #[derive(Clone, Copy)]
        enum Node {
            Leaf(u16),
            Pkg(u32, u32),
        }
        let mut lengths = vec![0usize; counts.len()];
        let mut leaves: Vec<Ent> = Vec::new();
        let mut arena: Vec<Node> = Vec::new();
        for (sym, &count) in counts.iter().enumerate() {
            if count > 0 {
                arena.push(Node::Leaf(sym as u16));
                leaves.push(Ent {
                    weight: count as u32,
                    node: arena.len() as u32 - 1,
                });
            }
        }
        assert!(leaves.len() >= 2);
        leaves.sort_by_key(|e| (e.weight, e.node));
        let n = leaves.len();
        let take = 2 * (n - 1);
        let mut prev: Vec<Ent> = leaves.clone();
        for _level in 1..max_len {
            let mut packages = Vec::with_capacity(n - 1);
            let mut i = 0;
            while i + 1 < prev.len() && packages.len() < n - 1 {
                let (a, b) = (prev[i], prev[i + 1]);
                arena.push(Node::Pkg(a.node, b.node));
                packages.push(Ent {
                    weight: a.weight + b.weight,
                    node: arena.len() as u32 - 1,
                });
                i += 2;
            }
            let mut merged: Vec<Ent> = Vec::with_capacity(take);
            let (mut li, mut pi) = (0, 0);
            while merged.len() < take && (li < n || pi < packages.len()) {
                let pick_leaf = pi >= packages.len()
                    || (li < n
                        && (leaves[li].weight, leaves[li].node)
                            <= (packages[pi].weight, packages[pi].node));
                merged.push(if pick_leaf {
                    li += 1;
                    leaves[li - 1]
                } else {
                    pi += 1;
                    packages[pi - 1]
                });
            }
            prev = merged;
        }
        let mut active: Vec<u32> = prev.iter().map(|e| e.node).collect();
        for _ in (0..max_len).rev() {
            let mut next = Vec::with_capacity(take);
            for id in active {
                match arena[id as usize] {
                    Node::Leaf(sym) => lengths[sym as usize] += 1,
                    Node::Pkg(a, b) => {
                        next.push(a);
                        next.push(b);
                    },
                }
            }
            active = next;
        }
        lengths
    }

    pub fn lengths(counts: &[usize], max_len: usize) -> Vec<usize> {
        merge_lengths(counts, max_len)
    }
}

#[test]
fn build_lengths_matches_optimal_and_is_kraft_exact() {
    let mut shapes: Vec<Vec<usize>> = Vec::new();
    for amount in 2..=256usize {
        shapes.push((0..amount).map(|i| amount - i).collect());
    }
    // Fibonacci counts drive the unlimited tree past the limit.
    let mut fib = alloc::vec![1usize, 1];
    while fib.len() < 40 {
        let next = fib[fib.len() - 1] + fib[fib.len() - 2];
        fib.push(next);
    }
    shapes.push(fib.clone());
    shapes.push(fib.iter().rev().copied().collect());
    // Mixed magnitudes over a full alphabet.
    shapes.push((0..256usize).map(|i| (i * 7919) % 1000 + 1).collect());
    shapes.push(
        (0..256usize)
            .map(|i| {
                if i % 3 == 0 {
                    1 << 20
                } else {
                    i % 17 + 1
                }
            })
            .collect(),
    );
    // Deterministic pseudo-random skews.
    let mut x = 0x12345678u32;
    for amount in [2usize, 3, 8, 40, 90, 200, 256] {
        shapes.push(
            (0..amount)
                .map(|i| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 13) as usize % (1 << (i % 20)) + 1
                })
                .collect(),
        );
    }

    let mut worst = 0f64;
    for counts in &shapes {
        let mut scratch = HuffScratch::default();
        let table = HuffmanTable::build_from_counts_into(counts, &mut scratch);
        let lens = table.code_lengths();
        let max = lens[..counts.len()].iter().copied().max().unwrap();
        assert!(usize::from(max) <= MAX_CODE_LENGTH);
        let kraft: u64 = lens[..counts.len()]
            .iter()
            .map(|&l| {
                if l == 0 {
                    0
                } else {
                    1u64 << (max - l)
                }
            })
            .sum();
        assert_eq!(kraft, 1u64 << max, "counts = {counts:?}");
        let cost: usize = counts
            .iter()
            .zip(lens.iter())
            .map(|(&c, &l)| c * l as usize)
            .sum();
        let ref_lens = reference::lengths(counts, MAX_CODE_LENGTH);
        let ref_cost: usize = counts
            .iter()
            .zip(ref_lens.iter())
            .map(|(&c, &l)| c * l)
            .sum();
        assert!(cost >= ref_cost, "build claims a better-than-optimal code");
        worst = worst.max((cost - ref_cost) as f64 / ref_cost.max(1) as f64);
    }
    // The two-queue + repayment heuristic sits at package-merge's optimum
    // on all but the limit-clamped shapes, where it pays a bounded slack.
    // Fibonacci-tail histograms (the unlimited tree far past the limit)
    // pay ~2%; everything else sits at the optimum.
    assert!(worst <= 0.02, "worst excess over optimal = {worst}");
}

#[test]
fn build_lengths_reference_optimality() {
    // {A:1, B:1, C:2} with limit 2: optimal lengths [2, 2, 1].
    let lengths = reference::lengths(&[1, 1, 2], 2);
    assert_eq!(lengths.as_slice(), &[2, 2, 1]);
    // Fibonacci counts produce consecutive lengths summing to the optimum.
    let counts = [1, 1, 2, 3, 5, 8];
    let lengths = reference::lengths(&counts, 11);
    let cost: usize = counts.iter().zip(&lengths).map(|(c, l)| c * l).sum();
    assert_eq!(
        cost,
        [1, 1, 2, 3, 5, 8]
            .iter()
            .zip([5, 5, 4, 3, 2, 1])
            .map(|(c, l)| c * l)
            .sum()
    );
}

#[test]
fn counts() {
    let counts = &[3, 0, 4, 1, 5];
    let table = HuffmanTable::build_from_counts(counts);

    assert_eq!(table.code_of(1).1, 0);
    // Optimal lengths: strictly larger counts never get longer codes.
    let mut sorted: Vec<(usize, u8)> = counts
        .iter()
        .zip(table.packed.iter())
        .filter(|(c, _)| **c > 0)
        .map(|(c, p)| (*c, (p & 0xf) as u8))
        .collect();
    sorted.sort_by_key(|(c, _)| *c);
    for pair in sorted.windows(2) {
        assert!(pair[1].1 <= pair[0].1, "sorted = {sorted:?}");
    }
    let counts = &[3, 0, 4, 0, 7, 2, 2, 2, 0, 2, 2, 1, 5];
    let table = HuffmanTable::build_from_counts(counts);

    assert_eq!(table.code_of(1).1, 0);
    assert_eq!(table.code_of(3).1, 0);
    assert_eq!(table.code_of(8).1, 0);
    let mut sorted: Vec<(usize, u8)> = counts
        .iter()
        .zip(table.packed.iter())
        .filter(|(c, _)| **c > 0)
        .map(|(c, p)| (*c, (p & 0xf) as u8))
        .collect();
    sorted.sort_by_key(|(c, _)| *c);
    for pair in sorted.windows(2) {
        assert!(pair[1].1 <= pair[0].1, "sorted = {sorted:?}");
    }
}

#[test]
fn from_data() {
    let data = &[0, 2, 4, 4, 0, 3, 2, 2, 0, 2];
    let mut counts = [0usize; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let table = HuffmanTable::build_from_counts(&counts[..=4]);
    let table2 = HuffmanTable::build_from_data(data);

    assert_eq!(table.packed, table2.packed);
    assert_eq!(table.uniform_nb, table2.uniform_nb);
}
