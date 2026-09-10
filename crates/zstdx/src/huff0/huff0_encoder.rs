use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::{
    bit_io::BitWriter,
    fse::fse_encoder::{self, FSEEncoder},
};

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
    pub fn encode(&mut self, data: &[u8], with_table: bool) {
        if with_table {
            self.write_table();
        }
        Self::encode_stream(self.table, self.writer, data);
    }

    /// Encodes the data using the provided table in 4 concatenated streams
    /// Writes
    /// * Table description
    /// * Jumptable
    /// * Encoded data in 4 streams, each padded to fill the last byte
    pub fn encode4x(&mut self, data: &[u8], with_table: bool) {
        assert!(data.len() >= 4);

        // Split data in 4 equally sized parts (the last one might be a bit smaller than the rest)
        let split_size = data.len().div_ceil(4);
        let src1 = &data[..split_size];
        let src2 = &data[split_size..split_size * 2];
        let src3 = &data[split_size * 2..split_size * 3];
        let src4 = &data[split_size * 3..];

        // Write table description
        if with_table {
            self.write_table();
        }

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

    /// Encode one stream and pad it to fill the last byte
    fn encode_stream<VV: AsMut<Vec<u8>>>(
        table: &HuffmanTable,
        writer: &mut BitWriter<VV>,
        data: &[u8],
    ) {
        // The batched writer performs the same bit accumulation as one
        // write_bits call per symbol (data reversed, since the format reads
        // the stream back to front), so the output is bit-identical.
        writer.write_packed_codes_rev(&table.packed, table.uniform_nb, data);

        let bits_to_fill = writer.misaligned();
        if bits_to_fill == 0 {
            writer.write_bits(1u32, 8);
        } else {
            writer.write_bits(1u32, bits_to_fill);
        }
    }

    pub(super) fn weights(&self) -> Vec<u8> {
        let max = self.table.codes.iter().map(|(_, nb)| nb).max().unwrap();

        self.table
            .codes
            .iter()
            .copied()
            .map(|(_, nb)| {
                if nb == 0 {
                    0
                } else {
                    max - nb + 1
                }
            })
            .collect::<Vec<u8>>()
    }

    fn write_table(&mut self) {
        // TODO strategy for determining this?
        let weights = self.weights();
        let weights = &weights[..weights.len() - 1]; // dont encode last weight
        if weights.len() > 16 {
            let size_idx = self.writer.index();
            self.writer.write_bits(0u8, 8);
            let idx_before = self.writer.index();
            let mut encoder = FSEEncoder::new(
                fse_encoder::build_table_from_data(weights.iter().copied(), 6, true),
                self.writer,
            );
            encoder.encode_interleaved(weights);
            let encoded_len = (self.writer.index() - idx_before) / 8;
            assert!(encoded_len < 128);
            self.writer.change_bits(size_idx, encoded_len as u8, 8);
        } else {
            self.writer.write_bits(weights.len() as u8 + 127, 8);
            let (pairs, remainder) = weights.as_chunks::<2>();
            for pair in pairs {
                let weight1 = pair[0];
                let weight2 = pair[1];
                assert!(weight1 < 16);
                assert!(weight2 < 16);
                self.writer.write_bits(weight2, 4);
                self.writer.write_bits(weight1, 4);
            }
            if !remainder.is_empty() {
                let weight = remainder[0];
                assert!(weight < 16);
                self.writer.write_bits(weight << 4, 8);
            }
        }
    }
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

    pub fn build_from_weights(weights: &[usize]) -> Self {
        struct SortEntry {
            symbol: u8,
            weight: usize,
        }
        let mut sorted = Vec::with_capacity(weights.len());

        // TODO this doesn't need to be a temporary Vec, it could be done in a [_; 264]
        // only non-zero weights are interesting here
        for (symbol, weight) in weights.iter().copied().enumerate() {
            if weight > 0 {
                sorted.push(SortEntry {
                    symbol: symbol as u8,
                    weight,
                });
            }
        }
        // We process symbols ordered by weight and then ordered by symbol
        sorted.sort_by(|left, right| match left.weight.cmp(&right.weight) {
            Ordering::Equal => left.symbol.cmp(&right.symbol),
            other => other,
        });

        // Prepare huffman table with placeholders
        let mut table = HuffmanTable {
            codes: Vec::with_capacity(weights.len()),
            packed: [0; 256],
            uniform_nb: 0,
        };
        for _ in 0..weights.len() {
            table.codes.push((0, 0));
        }

        // Determine the number of bits needed for codes with the lowest weight
        let weight_sum = sorted.iter().map(|e| 1 << (e.weight - 1)).sum::<usize>();
        assert!(weight_sum.is_power_of_two(), "This is an internal error");
        let max_num_bits = highest_bit_set(weight_sum) - 1; // this is a log_2 of a clean power of two

        // Starting at the symbols with the lowest weight we update the placeholders in the table
        let mut current_code = 0;
        let mut current_weight = 0;
        let mut current_num_bits = 0;
        let mut uniform_nb = 0u8;
        let mut seen_first = false;
        let mut all_same = true;
        for entry in &sorted {
            // If the entry isn't the same weight as the last one we need to change a few things
            if current_weight != entry.weight {
                // The code shifts by the difference of the weights to allow for enough unique
                // values
                current_code >>= entry.weight - current_weight;
                // Encoding a symbol of this weight will take less bits than the previous weight
                current_num_bits = max_num_bits - entry.weight + 1;
                // Run the next update when the weight changes again
                current_weight = entry.weight;
            }
            if seen_first && current_num_bits != uniform_nb as usize {
                all_same = false;
            }
            uniform_nb = current_num_bits as u8;
            seen_first = true;
            table.codes[entry.symbol as usize] = (current_code as u32, current_num_bits as u8);
            debug_assert!(current_num_bits <= 11 && current_code <= 0xfff);
            table.packed[entry.symbol as usize] = ((current_code << 4) | current_num_bits) as u16;
            current_code += 1;
        }
        if all_same && sorted.len() >= 2 {
            table.uniform_nb = uniform_nb;
        }

        table
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

/// Maximum Huffman code length the literals section can carry.
const MAX_CODE_LENGTH: usize = 11;

/// Boundary package-merge (Larmore-Hirschberg): optimal length-limited code
/// lengths. Zero-count symbols get length 0; the returned lengths for used
/// symbols are Kraft-exact (sum of 2^-len == 1) and never exceed `max_len`.
/// Requires `max_len >= log2(symbol count)` and at least two used symbols.
fn package_merge_lengths(counts: &[usize], max_len: usize) -> Vec<usize> {
    #[derive(Clone, Copy)]
    enum Node {
        Leaf(u16),
        Pkg(u32, u32),
    }
    #[derive(Clone, Copy)]
    struct Ent {
        weight: u64,
        node: u32,
    }

    let mut lengths = alloc::vec![0usize; counts.len()];
    let mut leaves: Vec<Ent> = Vec::new();
    let mut arena: Vec<Node> = Vec::new();
    for (sym, &count) in counts.iter().enumerate() {
        if count > 0 {
            arena.push(Node::Leaf(sym as u16));
            leaves.push(Ent {
                weight: count as u64,
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
    // level, keeping the cheapest `take` items.
    let mut lists: Vec<Vec<Ent>> = Vec::with_capacity(max_len);
    let mut first = leaves.clone();
    first.truncate(take);
    lists.push(first);
    for _ in 1..max_len {
        let prev = lists.last().unwrap();
        let mut packages: Vec<Ent> = Vec::with_capacity(n - 1);
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
        packages.sort_by_key(|e| (e.weight, e.node));
        // Intermediate levels can hold fewer than `take` items; only the
        // top level is guaranteed full (L >= log2 n).
        let mut merged: Vec<Ent> = Vec::with_capacity(take);
        let mut li = 0;
        let mut pi = 0;
        while merged.len() < take && (li < leaves.len() || pi < packages.len()) {
            let pick_leaf = pi >= packages.len()
                || (li < leaves.len()
                    && (leaves[li].weight, leaves[li].node)
                        <= (packages[pi].weight, packages[pi].node));
            if pick_leaf {
                merged.push(leaves[li]);
                li += 1;
            } else {
                merged.push(packages[pi]);
                pi += 1;
            }
        }
        lists.push(merged);
    }

    // Walk the solution back down: every leaf encountered at level k adds one
    // length unit; packages expand into their children one level below.
    let mut active: Vec<u32> = lists.last().unwrap().iter().map(|e| e.node).collect();
    for _ in (0..max_len).rev() {
        let mut next = Vec::with_capacity(active.len());
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
    debug_assert_eq!(
        lengths
            .iter()
            .map(|&l| if l == 0 {
                0
            } else {
                1usize << (max_len - l)
            })
            .sum::<usize>(),
        1 << max_len,
        "package-merge lengths must be Kraft-exact"
    );
    lengths
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
