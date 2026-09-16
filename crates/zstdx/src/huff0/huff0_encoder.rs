use alloc::vec::Vec;

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
        let mut out = Vec::with_capacity(self.table.codes.len());
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
    /// Index is the symbol, values are the bitstring in the lower bits of the u32 and the amount
    /// of bits in the u8
    codes: Vec<(u32, u8)>,
    /// Same codes packed as `(code << 4) | num_bits` so the encoding loop
    /// loads one u16 instead of an 8-byte tuple (the table stays fully
    /// L1-resident). Weight redistribution caps codes at 9 bits, so the
    /// packing cannot overflow.
    packed: [u16; 256],
    /// Left-aligned form for the dual-accumulator stream loop: the code in
    /// the top `nb` bits of a u64, `nb` in the low nibble. One load feeds
    /// the container shift, the OR and the bit counter.
    aligned: [u64; 256],
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
        // Optimal length-limited code lengths from the actual magnitudes;
        // the rank-only assignment this replaces lost noticeably on skewed
        // literal distributions (json digits/quotes).
        let lengths = package_merge_lengths(counts, MAX_CODE_LENGTH);
        let max_len = lengths.iter().copied().max().unwrap_or(1);
        let weights: Vec<usize> = lengths
            .iter()
            .map(|&len| {
                if len == 0 {
                    0
                } else {
                    max_len - len + 1
                }
            })
            .collect();
        Self::build_from_weights(&weights)
    }

    /// The pooled [`Self::build_from_counts`]: the package-merge lists and
    /// the codes buffer are recycled through `scratch`.
    pub(crate) fn build_from_counts_into(counts: &[usize], scratch: &mut HuffScratch) -> Self {
        assert!(counts.len() <= 256);
        package_merge_lengths_into(counts, MAX_CODE_LENGTH, scratch);
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
        let mut codes = scratch.take_codes(n);
        build_from_weights_slice(&scratch.weights[..n], &mut codes)
    }

    /// Return the codes buffer to a build pool (see [`HuffScratch`]);
    /// the table must not be used afterwards.
    pub(crate) fn recycle_codes(self, scratch: &mut HuffScratch) {
        scratch.recycle_codes(self.codes);
    }

    pub fn build_from_weights(weights: &[usize]) -> Self {
        debug_assert!(weights.iter().all(|&w| w <= MAX_CODE_LENGTH));
        let mut narrow = [0u8; 256];
        for (dst, &w) in narrow.iter_mut().zip(weights.iter()) {
            *dst = w as u8;
        }
        let mut codes = alloc::vec![(0, 0); weights.len()];
        build_from_weights_slice(&narrow[..weights.len()], &mut codes)
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

    /// Per-symbol wire weights into a recycled buffer (the format's
    /// weight form; see [`write_table_desc`]).
    pub(crate) fn write_weights_into(&self, out: &mut Vec<u8>) {
        out.clear();
        let max = self.codes.iter().map(|(_, nb)| *nb).max().unwrap();
        out.extend(self.codes.iter().copied().map(|(_, nb)| {
            if nb == 0 {
                0
            } else {
                max - nb + 1
            }
        }));
    }

    pub fn can_encode(&self, other: &Self) -> Option<usize> {
        if other.codes.len() > self.codes.len() {
            return None;
        }
        let mut sum = 0;
        for ((_, other_num_bits), (_, self_num_bits)) in other.codes.iter().zip(self.codes.iter()) {
            if *other_num_bits != 0 && *self_num_bits == 0 {
                return None;
            }
            sum += other_num_bits.abs_diff(*self_num_bits) as usize;
        }
        Some(sum)
    }
}

/// Core of [`HuffmanTable::build_from_weights`] over u8 weights and a
/// caller-owned codes buffer (recycled by the pooled variant).
fn build_from_weights_slice(weights: &[u8], codes: &mut Vec<(u32, u8)>) -> HuffmanTable {
    debug_assert!(weights.iter().all(|&w| w <= MAX_CODE_LENGTH as u8));
    codes.resize(weights.len(), (0, 0));
    codes.fill((0, 0));
    let mut bucket_counts = [0u16; MAX_CODE_LENGTH + 1];
    for &weight in weights {
        bucket_counts[weight as usize] += 1;
    }
    let mut bucket_start = [0u16; MAX_CODE_LENGTH + 1];
    let mut total = 0u16;
    for weight in 1..=MAX_CODE_LENGTH {
        bucket_start[weight] = total;
        total += bucket_counts[weight];
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
        packed: [0; 256],
        aligned: [0; 256],
        uniform_nb: 0,
        codes: core::mem::take(codes),
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
    let mut uniform_nb = 0u8;
    let mut seen_first = false;
    let mut all_same = true;
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
            if seen_first && current_num_bits != uniform_nb as usize {
                all_same = false;
            }
            uniform_nb = current_num_bits as u8;
            seen_first = true;
            table.codes[symbol as usize] = (current_code as u32, current_num_bits as u8);
            debug_assert!(current_num_bits <= 11 && current_code <= 0xfff);
            table.packed[symbol as usize] = ((current_code << 4) | current_num_bits) as u16;
            table.aligned[symbol as usize] =
                ((current_code as u64) << (64 - current_num_bits)) | current_num_bits as u64;
            current_code += 1;
        }
    }
    if all_same && total as usize >= 2 {
        table.uniform_nb = uniform_nb;
    }

    table
}

/// Assert that the provided value is greater than zero, and returns index of the first set bit
fn highest_bit_set(x: usize) -> usize {
    assert!(x > 0);
    usize::BITS as usize - x.leading_zeros() as usize
}

#[test]
fn huffman() {
    let table = HuffmanTable::build_from_weights(&[2, 2, 2, 1, 1]);
    assert_eq!(table.codes[0], (1, 2));
    assert_eq!(table.codes[1], (2, 2));
    assert_eq!(table.codes[2], (3, 2));
    assert_eq!(table.codes[3], (0, 3));
    assert_eq!(table.codes[4], (1, 3));

    let table = HuffmanTable::build_from_weights(&[4, 3, 2, 0, 1, 1]);
    assert_eq!(table.codes[0], (1, 1));
    assert_eq!(table.codes[1], (1, 2));
    assert_eq!(table.codes[2], (1, 3));
    assert_eq!(table.codes[3], (0, 0));
    assert_eq!(table.codes[4], (0, 4));
    assert_eq!(table.codes[5], (1, 4));
}

/// Pooled scratch for the per-block Huffman build (held in the compressor
/// state): the package-merge level lists, the per-symbol length/weight
/// scratch, the wire weight stream, and retired `codes` buffers.
pub(crate) struct HuffScratch {
    leaves: Vec<Ent>,
    arena: Vec<Node>,
    prev: Vec<Ent>,
    packages: Vec<Ent>,
    cur: Vec<Ent>,
    active: Vec<u32>,
    next: Vec<u32>,
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
    /// Retired `codes` buffers.
    codes: Vec<Vec<(u32, u8)>>,
}

/// Two live code buffers cover the adopt/replace flow per block.
const HUFF_CODES_CAP: usize = 2;

impl Default for HuffScratch {
    fn default() -> Self {
        Self {
            leaves: Vec::new(),
            arena: Vec::new(),
            prev: Vec::new(),
            packages: Vec::new(),
            cur: Vec::new(),
            active: Vec::new(),
            next: Vec::new(),
            lengths: [0; 256],
            weights: [0; 256],
            wire_weights: Vec::new(),
            desc: Vec::new(),
            desc_fse: Vec::new(),
            codes: Vec::new(),
        }
    }
}

impl HuffScratch {
    /// Take a codes buffer of `len` initialized entries (the build fills
    /// every slot, so recycling only needs the capacity).
    fn take_codes(&mut self, len: usize) -> Vec<(u32, u8)> {
        let mut v = match self.codes.iter().position(|v| v.len() >= len) {
            Some(i) => self.codes.swap_remove(i),
            None => Vec::with_capacity(len),
        };
        if v.len() < len {
            v.resize(len, (0, 0));
        } else {
            v.truncate(len);
        }
        v
    }

    /// Return a retired table's codes buffer to the pool.
    fn recycle_codes(&mut self, v: Vec<(u32, u8)>) {
        if self.codes.len() < HUFF_CODES_CAP {
            self.codes.push(v);
        } else if self.codes.iter().all(|s| s.len() >= v.len()) {
            // Every pooled buffer is at least as large: drop the retiree.
        } else {
            let smallest = self
                .codes
                .iter_mut()
                .min_by_key(|s| s.len())
                .expect("pool is non-empty at capacity");
            *smallest = v;
        }
    }
}

/// Maximum Huffman code length the literals section can carry.
const MAX_CODE_LENGTH: usize = 11;

#[derive(Clone, Copy)]
enum Node {
    Leaf(u16),
    Pkg(u32, u32),
}

#[derive(Clone, Copy)]
struct Ent {
    // Block literals are capped far below 2^32, so package weights (sums
    // of leaf counts) cannot overflow either; the narrow field halves
    // sort/merge memory traffic.
    weight: u32,
    node: u32,
}

/// Boundary package-merge (Larmore-Hirschberg): optimal length-limited code
/// lengths. Zero-count symbols get length 0; the returned lengths for used
/// symbols are Kraft-exact (sum of 2^-len == 1) and never exceed `max_len`.
/// Requires `max_len >= log2(symbol count)` and at least two used symbols.
#[cfg(any(test, feature = "fuzz_exports"))]
fn package_merge_lengths(counts: &[usize], max_len: usize) -> Vec<usize> {
    let mut scratch = HuffScratch::default();
    package_merge_lengths_into(counts, max_len, &mut scratch);
    scratch.lengths[..counts.len()]
        .iter()
        .map(|&l| l as usize)
        .collect()
}

/// The pooled [`package_merge_lengths`]: fills `scratch.lengths[..n]`.
fn package_merge_lengths_into(counts: &[usize], max_len: usize, scratch: &mut HuffScratch) {
    let lengths = &mut scratch.lengths;
    lengths[..counts.len()].fill(0);
    let leaves = &mut scratch.leaves;
    let arena = &mut scratch.arena;
    leaves.clear();
    arena.clear();
    for (sym, &count) in counts.iter().enumerate() {
        if count > 0 {
            arena.push(Node::Leaf(sym as u16));
            leaves.push(Ent {
                weight: u32::try_from(count).expect("literal count exceeds u32"),
                node: (arena.len() - 1) as u32,
            });
        }
    }
    let n = leaves.len();
    assert!(n >= 2, "single-symbol alphabets go through the RLE path");
    assert!(max_len >= n.next_power_of_two().ilog2() as usize);
    leaves.sort_by_key(|e| (e.weight, e.node));
    let take = 2 * (n - 1);

    // Level lists: level 0 is the leaves alone; every further level merges
    // the leaves with packages formed from consecutive pairs of the previous
    // level, keeping the cheapest `take` items. Only the previous level is
    // ever read again, so two swapped buffers replace the list-of-lists
    // (which allocated two Vecs per level) — and both now live in the
    // scratch, so no level allocates at all.
    let prev = &mut scratch.prev;
    prev.clear();
    prev.extend_from_slice(&leaves[..take.min(leaves.len())]);
    let packages = &mut scratch.packages;
    let cur = &mut scratch.cur;
    for _ in 1..max_len {
        packages.clear();
        let mut i = 0;
        while i + 1 < prev.len() && packages.len() < n - 1 {
            let id = arena.len() as u32;
            arena.push(Node::Pkg(prev[i].node, prev[i + 1].node));
            packages.push(Ent {
                weight: prev[i].weight + prev[i + 1].weight,
                node: id,
            });
            i += 2;
        }
        // No sort needed: `prev` is (weight, node)-sorted, so disjoint adjacent
        // pair sums are weight-monotone (w[2k+2] >= w[2k] and w[2k+3] >= w[2k+1]
        // imply pkg[k+1] >= pkg[k]); ties break by node id, which strictly
        // increases in push order. The packages are therefore already in the
        // exact order the removed sort produced.
        debug_assert!(packages.is_sorted_by(|a, b| { (a.weight, a.node) <= (b.weight, b.node) }));
        // Intermediate levels can hold fewer than `take` items; only the
        // top level is guaranteed full (L >= log2 n).
        cur.clear();
        let mut li = 0;
        let mut pi = 0;
        while cur.len() < take && (li < leaves.len() || pi < packages.len()) {
            let pick_leaf = pi >= packages.len()
                || (li < leaves.len()
                    && (leaves[li].weight, leaves[li].node)
                        <= (packages[pi].weight, packages[pi].node));
            if pick_leaf {
                cur.push(leaves[li]);
                li += 1;
            } else {
                cur.push(packages[pi]);
                pi += 1;
            }
        }
        core::mem::swap(prev, cur);
    }

    // Walk the solution back down: every leaf encountered at level k adds one
    // length unit; packages expand into their children one level below. The
    // active/next lists alternate inside the scratch.
    let active = &mut scratch.active;
    active.clear();
    active.extend(prev.iter().map(|e| e.node));
    let next = &mut scratch.next;
    for _ in (0..max_len).rev() {
        next.clear();
        for &id in active.iter() {
            match arena[id as usize] {
                Node::Leaf(sym) => lengths[sym as usize] += 1,
                Node::Pkg(a, b) => {
                    next.push(a);
                    next.push(b);
                },
            }
        }
        core::mem::swap(active, next);
    }
    debug_assert_eq!(
        lengths[..counts.len()]
            .iter()
            .map(|&l| if l == 0 {
                0
            } else {
                1usize << (max_len - l as usize)
            })
            .sum::<usize>(),
        1 << max_len,
        "package-merge lengths must be Kraft-exact"
    );
}

#[test]
fn package_merge_optimality() {
    // {A:1, B:1, C:2} with limit 2: optimal lengths [2, 2, 1].
    let lengths = package_merge_lengths(&[1, 1, 2], 2);
    assert_eq!(lengths.as_slice(), &[2, 2, 1]);
    // A classic Huffman shape: fibonacci counts produce consecutive lengths.
    let counts = [1, 1, 2, 3, 5, 8];
    let lengths = package_merge_lengths(&counts, 11);
    let cost: usize = counts.iter().zip(&lengths).map(|(c, l)| c * l).sum();
    // hand-computed optimum: count * code_length per symbol
    assert_eq!(
        cost,
        [1, 1, 2, 3, 5, 8]
            .iter()
            .zip([5, 5, 4, 3, 2, 1])
            .map(|(c, l)| c * l)
            .sum::<usize>()
    );
    // Every alphabet size produces Kraft-exact, bounded lengths.
    for amount in 2..=256usize {
        let counts: Vec<usize> = (0..amount).map(|i| amount - i).collect();
        let lengths = package_merge_lengths(&counts, 11);
        assert!(lengths.iter().all(|&l| l <= 11));
        let kraft: usize = lengths
            .iter()
            .map(|&l| {
                if l == 0 {
                    0
                } else {
                    1usize << (11 - l)
                }
            })
            .sum();
        assert_eq!(kraft, 1 << 11);
    }
}

#[test]
fn counts() {
    let counts = &[3, 0, 4, 1, 5];
    let table = HuffmanTable::build_from_counts(counts).codes;

    assert_eq!(table[1].1, 0);
    // Optimal lengths: strictly larger counts never get longer codes.
    let mut sorted: Vec<(usize, u8)> = counts
        .iter()
        .zip(table.iter())
        .filter(|(c, _)| **c > 0)
        .map(|(c, (_, nb))| (*c, *nb))
        .collect();
    sorted.sort_by_key(|(c, _)| *c);
    for pair in sorted.windows(2) {
        assert!(pair[1].1 <= pair[0].1, "sorted = {sorted:?}");
    }
    let counts = &[3, 0, 4, 0, 7, 2, 2, 2, 0, 2, 2, 1, 5];
    let table = HuffmanTable::build_from_counts(counts).codes;

    assert_eq!(table[1].1, 0);
    assert_eq!(table[3].1, 0);
    assert_eq!(table[8].1, 0);
    let mut sorted: Vec<(usize, u8)> = counts
        .iter()
        .zip(table.iter())
        .filter(|(c, _)| **c > 0)
        .map(|(c, (_, nb))| (*c, *nb))
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
    let table = HuffmanTable::build_from_counts(&counts[..=4]).codes;
    let table2 = HuffmanTable::build_from_data(data).codes;

    assert_eq!(table, table2);
}
