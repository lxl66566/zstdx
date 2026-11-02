//! Utilities for decoding Huff0 encoded huffman data.

use alloc::{vec, vec::Vec};

use crate::{
    bit_io::BitReaderReversed,
    decoding::errors::HuffmanTableError,
    fse::{FSEDecoder, FSETable},
};

/// The Zstandard specification limits the maximum length of a code to 11 bits.
pub(crate) const MAX_MAX_NUM_BITS: u8 = 11;

pub struct HuffmanDecoder<'table> {
    table: &'table HuffmanTable,
    /// State is used to index into the table.
    pub state: u64,
}

impl<'t> HuffmanDecoder<'t> {
    /// Create a new decoder with the provided table
    pub fn new(table: &'t HuffmanTable) -> HuffmanDecoder<'t> {
        HuffmanDecoder { table, state: 0 }
    }

    /// Decode the symbol the internal state (cursor) is pointed at and return the
    /// decoded literal.
    pub fn decode_symbol(&mut self) -> u8 {
        self.table.decode[self.state as usize].symbol
    }

    /// Initialize internal state and prepare to decode data. Then, `decode_symbol` can be called
    /// to read the byte the internal cursor is pointing at, and `next_state` can be called to
    /// advance the cursor until the max number of bits has been read.
    pub fn init_state(&mut self, br: &mut BitReaderReversed<'_>) -> u8 {
        let num_bits = self.table.max_num_bits;
        let new_bits = br.get_bits(num_bits);
        self.state = new_bits;
        num_bits
    }

    /// Advance the internal cursor to the next symbol. After this, you can call `decode_symbol`
    /// to read from the new position.
    pub fn next_state(&mut self, br: &mut BitReaderReversed<'_>) -> u8 {
        // self.state stores a small section, or a window of the bit stream. The table can be
        // indexed via this state, telling you how many bits identify the current symbol.
        let num_bits = self.table.decode[self.state as usize].num_bits;
        // New bits are read from the stream
        let new_bits = br.get_bits(num_bits);
        // Shift and mask out the bits that identify the current symbol
        self.state <<= num_bits;
        self.state &= self.table.decode.len() as u64 - 1;
        // The new bits are appended at the end of the current state.
        self.state |= new_bits;
        num_bits
    }

    /// Decode the symbol for the current state and advance to the next state in one step.
    ///
    /// Equivalent to `decode_symbol` followed by `next_state`, but loads the table entry
    /// only once per symbol. The packed table stores the symbol in the high 8 bits and
    /// the code length in the low 6 bits of a u16.
    pub fn decode_and_advance(&mut self, br: &mut BitReaderReversed<'_>) -> u8 {
        let entry = self.table.packed[self.state as usize];
        let num_bits = (entry & 0x3f) as u8;
        let new_bits = br.get_bits(num_bits);
        self.state <<= num_bits;
        self.state &= self.table.packed.len() as u64 - 1;
        self.state |= new_bits;
        (entry >> 8) as u8
    }
}

/// A Huffman decoding table contains a list of Huffman prefix codes and their associated values
pub struct HuffmanTable {
    decode: Vec<Entry>,
    /// Same table as `decode`, but each entry packed into a u16:
    /// high 8 bits = symbol, low 6 bits = code length. Enables a single load per symbol.
    packed: Vec<u16>,
    /// The weight of a symbol is the number of occurences in a table.
    /// This value is used in constructing a binary tree referred to as
    /// a Huffman tree. Once this tree is constructed, it can be used to build
    /// the lookup table
    weights: Vec<u8>,
    /// The maximum size in bits a prefix code in the encoded data can be.
    /// This value is used so that the decoder knows how many bits
    /// to read from the bitstream before checking the table. This
    /// value must be 11 or lower.
    pub max_num_bits: u8,
    bits: Vec<u8>,
    bit_ranks: Vec<u32>,
    rank_indexes: Vec<usize>,
    /// Double-symbol (X2) decoding table: 2^11 u32 entries, mirroring libzstd's
    /// HUF_DEltX2 packed layout: bits 0..16 = one or two output symbols
    /// (little-endian, first symbol in the low byte), bits 16..22 = total bits
    /// consumed for the entry, bit 24 = number of symbols (1 or 2).
    /// Empty unless `build_x2_table` ran after the regular table was built.
    x2: Vec<u32>,
    /// In some cases, the list of weights is compressed using FSE compression.
    fse_table: FSETable,
}

impl HuffmanTable {
    /// Create a new, empty table.
    pub fn new() -> HuffmanTable {
        HuffmanTable {
            decode: Vec::new(),
            packed: Vec::new(),

            weights: Vec::with_capacity(256),
            max_num_bits: 0,
            bits: Vec::with_capacity(256),
            bit_ranks: Vec::with_capacity(11),
            rank_indexes: Vec::with_capacity(11),
            x2: Vec::new(),
            // The weight alphabet is 0..=11, not the u8 range: libzstd sizes
            // HUF_readStats' FSE workspace at
            // FSE_DECOMPRESS_WKSP_SIZE_U32(6, HUF_TABLELOG_MAX - 1), so a
            // weights NCount whose max symbol exceeds 11 fails its workspace
            // check (tableLog_tooLarge) before any weight is decoded — even
            // for a declared symbol the bitstream never emits. Capping here
            // rejects that class at the table read; every libzstd-accepted
            // weights header passes that check, so it declares at most
            // symbol 11 and nothing valid regresses.
            fse_table: FSETable::new(11),
        }
    }

    /// The per-symbol code-length weights the table was built from
    /// (0 = symbol absent). Feeds encoder-side table reconstruction
    /// (`huff0_encoder::HuffmanTable::build_from_weights`) when seeding
    /// from a dictionary.
    pub fn weights(&self) -> &[u8] {
        &self.weights
    }

    /// Per-symbol code lengths including the format's inferred last symbol
    /// (the wire omits it; `weights` above therefore lacks it). The full
    /// symbol range, zeros for absent symbols — feed to
    /// `huff0_encoder::HuffmanTable::build_from_code_lengths`.
    pub fn code_lengths(&self) -> &[u8] {
        &self.bits
    }

    /// Completely empty the table then repopulate as a replica
    /// of `other`. `bit_ranks` and `rank_indexes` stay empty: both are
    /// fill scratch rebuilt at the top of every table fill from `bits`
    /// (`build_table_from_weights`), never read between fills.
    pub fn reinit_from(&mut self, other: &Self) {
        self.reset();
        self.decode.extend_from_slice(&other.decode);
        self.packed.extend_from_slice(&other.packed);
        self.weights.extend_from_slice(&other.weights);
        self.max_num_bits = other.max_num_bits;
        self.bits.extend_from_slice(&other.bits);
        self.x2.extend_from_slice(&other.x2);
        self.fse_table.reinit_from(&other.fse_table);
    }

    /// Completely empty the table of all data.
    pub fn reset(&mut self) {
        self.decode.clear();
        self.packed.clear();
        self.weights.clear();
        self.max_num_bits = 0;
        self.bits.clear();
        self.bit_ranks.clear();
        self.rank_indexes.clear();
        self.x2.clear();
        self.fse_table.reset();
    }

    /// Read from `source` and decode the input, populating the huffman decoding table.
    ///
    /// Returns the number of bytes read.
    pub fn build_decoder(&mut self, source: &[u8]) -> Result<u32, HuffmanTableError> {
        self.decode.clear();
        self.packed.clear();

        let bytes_used = self.read_weights(source)?;
        self.build_table_from_weights()?;
        Ok(bytes_used)
    }

    /// The decode table packed as u16 entries: `(symbol << 8) | num_bits`.
    /// Allows a single load per decoded symbol.
    pub(crate) fn packed_table(&self) -> &[u16] {
        &self.packed
    }

    /// Read weights from the provided source.
    ///
    /// The huffman table is represented in the input data as a list of weights.
    /// After the header, weights are read, then a Huffman decoding table
    /// can be constructed using that list of weights.
    ///
    /// Returns the number of bytes read.
    fn read_weights(&mut self, source: &[u8]) -> Result<u32, HuffmanTableError> {
        use HuffmanTableError as err;

        if source.is_empty() {
            return Err(err::SourceIsEmpty);
        }
        let header = source[0];
        let mut bits_read = 8;

        // If the header byte is less than 128, the series of weights is
        // compressed using two interleaved FSE streams that share a
        // distribution table.
        if let 0..=127 = header {
            let fse_stream = &source[1..];
            if header as usize > fse_stream.len() {
                return Err(err::NotEnoughBytesForWeights {
                    got_bytes: fse_stream.len(),
                    expected_bytes: header,
                });
            }
            // fse decompress weights
            let bytes_used_by_fse_header = self.fse_table.build_decoder(fse_stream, 6)?;

            if bytes_used_by_fse_header > header as usize {
                return Err(err::FSETableUsedTooManyBytes {
                    used: bytes_used_by_fse_header,
                    available_bytes: header,
                });
            }

            vprintln!(
                "Building fse table for huffman weights used: {}",
                bytes_used_by_fse_header
            );
            // Huffman headers are compressed using two interleaved
            // FSE bitstreams, where the first state (decoder) handles
            // even symbols, and the second handles odd symbols.
            let mut dec1 = FSEDecoder::new(&self.fse_table);
            let mut dec2 = FSEDecoder::new(&self.fse_table);

            let compressed_start = bytes_used_by_fse_header;
            let compressed_length = header as usize - bytes_used_by_fse_header;

            let compressed_weights = &fse_stream[compressed_start..];
            if compressed_weights.len() < compressed_length {
                return Err(err::NotEnoughBytesToDecompressWeights {
                    have: compressed_weights.len(),
                    need: compressed_length,
                });
            }
            let compressed_weights = &compressed_weights[..compressed_length];
            let mut br = BitReaderReversed::new(compressed_weights);

            bits_read += (bytes_used_by_fse_header + compressed_length) * 8;

            // The bitstream must end (in reading order: start) with padding
            // zeros and a single 1 marker; more than 7 zeros means the
            // stream is corrupt.
            if !br.skip_padding_marker() {
                return Err(err::ExtraPadding { skipped_bits: 9 });
            }

            dec1.init_state(&mut br)?;
            dec2.init_state(&mut br)?;

            self.weights.clear();

            // The two decoders take turns decoding a single symbol and updating their state.
            loop {
                let w = dec1.decode_symbol();
                self.weights.push(w);
                dec1.update_state(&mut br);

                if br.bits_remaining() <= -1 {
                    // collect final states
                    self.weights.push(dec2.decode_symbol());
                    break;
                }

                let w = dec2.decode_symbol();
                self.weights.push(w);
                dec2.update_state(&mut br);

                if br.bits_remaining() <= -1 {
                    // collect final states
                    self.weights.push(dec1.decode_symbol());
                    break;
                }
                // maximum number of weights is 255 because we use u8 symbols and the last
                // weight is inferred from the sum of all others
                if self.weights.len() > 255 {
                    return Err(err::TooManyWeights {
                        got: self.weights.len(),
                    });
                }
            }
            // The two exhaustion breaks above skip the in-loop bound, and the
            // inferred last weight would push a 257th symbol past the u8
            // alphabet (aliasing symbol 256 onto 0 in the table fill).
            if self.weights.len() > 255 {
                return Err(err::TooManyWeights {
                    got: self.weights.len(),
                });
            }
        } else {
            // If the header byte is greater than or equal to 128,
            // weights are directly represented, where each weight is
            // encoded directly as a 4 bit field. The weights will
            // always be encoded with full bytes, meaning if there's
            // an odd number of weights, the last weight will still
            // occupy a full byte.
            // weights are directly encoded
            let weights_raw = &source[1..];
            let num_weights = header - 127;
            self.weights.resize(num_weights as usize, 0);

            let bytes_needed = if num_weights.is_multiple_of(2) {
                num_weights as usize / 2
            } else {
                (num_weights as usize / 2) + 1
            };

            if weights_raw.len() < bytes_needed {
                return Err(err::NotEnoughBytesInSource {
                    got: weights_raw.len(),
                    need: bytes_needed,
                });
            }

            for idx in 0..num_weights {
                if idx % 2 == 0 {
                    self.weights[idx as usize] = weights_raw[idx as usize / 2] >> 4;
                } else {
                    self.weights[idx as usize] = weights_raw[idx as usize / 2] & 0xf;
                }
                bits_read += 4;
            }
        }

        let bytes_read = if bits_read % 8 == 0 {
            bits_read / 8
        } else {
            (bits_read / 8) + 1
        };
        Ok(bytes_read as u32)
    }

    /// Once the weights have been read from the data, you can decode the weights
    /// into a table, and use that table to decode the actual compressed data.
    ///
    /// This function populates the rest of the table from the series of weights.
    fn build_table_from_weights(&mut self) -> Result<(), HuffmanTableError> {
        use HuffmanTableError as err;

        // invalidate the double-symbol table: it is only valid for the exact
        // weights this call rebuilds from, and is re-created on demand by
        // `build_x2_table` (treeless blocks reuse whatever was last built)
        self.x2.clear();

        self.bits.clear();
        self.bits.resize(self.weights.len() + 1, 0);

        let mut weight_sum: u32 = 0;
        let mut weight_one_count = 0usize;
        for w in &self.weights {
            if *w > MAX_MAX_NUM_BITS {
                return Err(err::WeightBiggerThanMaxNumBits { got: *w });
            }
            weight_one_count += usize::from(*w == 1);
            weight_sum += if *w > 0 {
                1_u32 << (*w - 1)
            } else {
                0
            };
        }

        if weight_sum == 0 {
            return Err(err::MissingWeights);
        }

        let max_bits = highest_bit_set(weight_sum) as u8;
        // Enforce the format's 11-bit code cap before any table state is
        // published: the fast loops' `64 - tl` shifts and 7-byte reload
        // retreat (literals_section_decoder) and the X2 table's fixed 2^11
        // window are sound only for tl <= 11, and `max_num_bits` must not
        // outlive a failed build over stale (or cleared) arrays.
        if max_bits > MAX_MAX_NUM_BITS {
            return Err(err::MaxBitsTooHigh { got: max_bits });
        }
        let left_over = (1 << max_bits) - weight_sum;

        // left_over must be power of two
        if !left_over.is_power_of_two() {
            return Err(err::LeftoverIsNotAPowerOf2 { got: left_over });
        }

        let last_weight = highest_bit_set(left_over) as u8;

        // libzstd's tree-validity check (HUF_readStats): the maximum-weight
        // class (weight 1, inferred last weight included) must hold an even
        // count of at least two symbols. Complete-but-non-canonical weight
        // sets (e.g. raw [2, 2] with the inferred 3) parse Kraft-exact yet
        // cannot come from a real Huffman tree, so libzstd rejects them.
        // (The odd half is already implied by the leftover power-of-two
        // check; it stays for structural parity with the reference.)
        let rank_one = weight_one_count + usize::from(last_weight == 1);
        if rank_one < 2 || rank_one % 2 == 1 {
            return Err(err::InvalidWeightOneCount { got: rank_one });
        }

        for symbol in 0..self.weights.len() {
            let bits = if self.weights[symbol] > 0 {
                max_bits + 1 - self.weights[symbol]
            } else {
                0
            };
            self.bits[symbol] = bits;
        }

        self.bits[self.weights.len()] = max_bits + 1 - last_weight;
        self.max_num_bits = max_bits;

        self.bit_ranks.clear();
        self.bit_ranks.resize((max_bits + 1) as usize, 0);
        for num_bits in &self.bits {
            self.bit_ranks[(*num_bits) as usize] += 1;
        }

        // fill with dummy symbols
        self.decode.resize(1 << self.max_num_bits, Entry {
            symbol: 0,
            num_bits: 0,
        });

        // starting codes for each rank
        self.rank_indexes.clear();
        self.rank_indexes.resize((max_bits + 1) as usize, 0);

        self.rank_indexes[max_bits as usize] = 0;
        for bits in (1..self.rank_indexes.len() as u8).rev() {
            self.rank_indexes[bits as usize - 1] = self.rank_indexes[bits as usize]
                + self.bit_ranks[bits as usize] as usize * (1 << (max_bits - bits));
        }

        assert!(
            self.rank_indexes[0] == self.decode.len(),
            "rank_idx[0]: {} should be: {}",
            self.rank_indexes[0],
            self.decode.len()
        );

        self.fill_decode_tables();

        Ok(())
    }

    /// Fill `decode` and `packed` from `bits` and `bit_ranks` rank by rank,
    /// the port of libzstd's `HUF_readDTableX1_wksp` fill: a counting sort
    /// groups the symbols by code length, then each rank's spans are written
    /// in symbol order with one constant-length fill per symbol. The layout
    /// is what the `rank_indexes` sums define: rank `bits` owns the region
    /// at `rank_indexes[bits]`, its symbols in ascending order. The previous
    /// scatter form interleaved span lengths 1 and 2^10 between adjacent
    /// symbols, and that interleaving was dll100's largest decode
    /// branch-miss lump.
    fn fill_decode_tables(&mut self) {
        let max_bits = self.max_num_bits as usize;

        // `sorted` buckets the symbols by code length; bucket 0 (absent
        // symbols) is sorted along so the placement pass needs no branch.
        let mut sorted = [0u8; 256];
        let mut rank_begin = [0u16; 12];
        let mut acc = 0usize;
        for (bits, &count) in self.bit_ranks.iter().enumerate() {
            rank_begin[bits] = acc as u16;
            acc += count as usize;
        }
        debug_assert_eq!(acc, self.bits.len());
        let mut rank_next = rank_begin;
        for (symbol, &bits) in self.bits.iter().enumerate() {
            let slot = &mut rank_next[bits as usize];
            sorted[*slot as usize] = symbol as u8;
            *slot += 1;
        }

        self.packed.clear();
        self.packed.resize(self.decode.len(), 0);

        // longest codes first: the regions grow from index 0 in this order,
        // so one running cursor covers the whole table
        let mut idx = 0usize;
        for bits in (1..=max_bits).rev() {
            let len = 1usize << (max_bits - bits);
            let begin = rank_begin[bits] as usize;
            let end = rank_next[bits] as usize;
            for &symbol in &sorted[begin..end] {
                let entry = Entry {
                    symbol,
                    num_bits: bits as u8,
                };
                self.decode[idx..idx + len].fill(entry);
                // packed is the decode loops' one-load view: (symbol << 8) | num_bits
                let packed = ((symbol as u16) << 8) | bits as u16;
                self.packed[idx..idx + len].fill(packed);
                idx += len;
            }
        }
        debug_assert_eq!(idx, self.decode.len());
    }

    /// The per-symbol scatter fill this table used before the rank-grouped
    /// port, kept as the equivalence reference for `fill_decode_tables`.
    /// Consumes `rank_indexes`, so it must run against the initial region
    /// starts that `build_table_from_weights` leaves in place.
    #[cfg(test)]
    fn fill_decode_tables_scatter(&mut self) {
        let max_bits = self.max_num_bits;
        for symbol in 0..self.bits.len() {
            let bits_for_symbol = self.bits[symbol];
            if bits_for_symbol != 0 {
                // allocate code for the symbol and set in the table
                // a code ignores all max_bits - bits[symbol] bits, so it gets
                // a range that spans all of those in the decoding table
                let base_idx = self.rank_indexes[bits_for_symbol as usize];
                let len = 1usize << (max_bits - bits_for_symbol);
                self.rank_indexes[bits_for_symbol as usize] += len;
                for idx in 0..len {
                    self.decode[base_idx + idx].symbol = symbol as u8;
                    self.decode[base_idx + idx].num_bits = bits_for_symbol;
                }
            }
        }

        // build the packed view of the same table
        self.packed.clear();
        self.packed.extend(
            self.decode
                .iter()
                .map(|e| ((e.symbol as u16) << 8) | e.num_bits as u16),
        );
    }

    /// Build the double-symbol (X2) decoding table from the current weights.
    ///
    /// Port of libzstd's `HUF_readDTableX2_wksp` / `HUF_fillDTableX2`. The table
    /// has a fixed size of 2^11 entries (HUF_DECODER_FAST_TABLELOG): each
    /// single-symbol code owns the slice of indices sharing its prefix, and
    /// whenever at least one more shortest code fits into the remaining window
    /// bits the entries of that slice are upgraded to decode two symbols per
    /// lookup (level 2), consuming the summed bit count.
    ///
    /// Must be called after `build_table_from_weights`; the regular single-symbol
    /// table is still used by the finish loop and the single-stream path.
    pub(crate) fn build_x2_table(&mut self) {
        /// X2 entries are indexed by the top 11 bits of the bit window.
        const TARGET_LOG: i32 = 11;

        let tl = self.max_num_bits as i32;
        debug_assert!((1..=11).contains(&tl));
        let nb_bits_baseline = tl + 1;

        // rank_stats[w] = number of symbols with huffman weight w (1..=tl);
        // self.bits[] holds per-symbol code lengths incl. the inferred last
        // symbol, weight = nb_bits_baseline - code_length.
        let mut rank_stats = [0u32; 13];
        let mut max_w = 0usize;
        for &b in &self.bits {
            if b == 0 {
                continue;
            }
            let w = (nb_bits_baseline - b as i32) as usize;
            rank_stats[w] += 1;
            max_w = max_w.max(w);
        }

        let min_bits = nb_bits_baseline - max_w as i32; // shortest code length
        let scale_log = nb_bits_baseline - TARGET_LOG; // ≤ 0
        let rescale = TARGET_LOG - tl - 1;

        // Only keep the X2 table when pairing actually pays off: the share of
        // the code space whose codes are short enough for a second code to fit
        // into the 11-bit lookup window must dominate. Below ~80% the many
        // single-symbol entries (2x wider than X1 entries, u16 writes with a
        // garbage second byte, variable output advance) make X2 slower than
        // the plain single-symbol loop despite the halved chain for the rest.
        let mut eligible_slots = 0u32;
        for &b in &self.bits {
            if b == 0 {
                continue;
            }
            let w = nb_bits_baseline - b as i32;
            if TARGET_LOG - (nb_bits_baseline - w) >= min_bits {
                eligible_slots += 1u32 << (w + rescale) as u32;
            }
        }
        if eligible_slots * 5 < 4u32 << TARGET_LOG {
            self.x2.clear();
            return;
        }

        // sort symbols by weight: starts[w]..ends[w] is the weight-w range
        let mut starts = [0u32; 14];
        let mut next = 0u32;
        for w in 1..=max_w {
            starts[w] = next;
            next += rank_stats[w];
        }
        let mut sorted = vec![0u8; next as usize];
        let mut ends = starts;
        for (sym, &b) in self.bits.iter().enumerate() {
            if b == 0 {
                continue;
            }
            let w = (nb_bits_baseline - b as i32) as usize;
            sorted[ends[w] as usize] = sym as u8;
            ends[w] += 1;
        }

        // rank_val0[w] = start of the weight-w span inside the 2^11 table;
        // a weight-w symbol owns 2^(w + rescale) slots there.
        let mut rank_val0 = [0u32; 13];
        let mut acc = 0u32;
        for w in 1..=max_w {
            rank_val0[w] = acc;
            acc += rank_stats[w] << (w as i32 + rescale) as u32;
        }
        debug_assert_eq!(acc, 1 << TARGET_LOG);

        // rank_val[consumed][w] = offset of the weight-w second symbols inside
        // the span of a first symbol that consumed `consumed` bits.
        let mut rank_val = [[0u32; 13]; 12];
        for consumed in min_bits..=(TARGET_LOG - min_bits) {
            for w in 1..=max_w {
                rank_val[consumed as usize][w] = rank_val0[w] >> consumed;
            }
        }

        self.x2.clear();
        self.x2.resize(1 << TARGET_LOG, 0);
        let dt = &mut self.x2;

        let pack =
            |sequence: u32, nb_bits: u32, length: u32| sequence | (nb_bits << 16) | (length << 24);

        for w in 1..=max_w {
            let nb_bits = nb_bits_baseline - w as i32;
            let begin = starts[w] as usize;
            let end = ends[w] as usize;
            if TARGET_LOG - nb_bits >= min_bits {
                // Enough room after the code for at least the shortest second
                // code: upgrade every entry of the span to two-symbol entries.
                let length = 1usize << (TARGET_LOG - nb_bits);
                let min_weight = (nb_bits + scale_log).max(1) as usize;
                let rv = &rank_val[nb_bits as usize];
                let mut start = rank_val0[w] as usize;
                for s in begin..end {
                    let base_seq = sorted[s];
                    // continuations whose remaining bits are too few for any
                    // second code keep decoding a single symbol
                    let skip = rv[min_weight] as usize;
                    dt[start..start + skip].fill(pack(base_seq as u32, nb_bits as u32, 1));
                    for w2 in min_weight..=max_w {
                        let total_bits = nb_bits_baseline - w2 as i32 + nb_bits;
                        let span = 1usize << (TARGET_LOG - total_bits);
                        let mut base = start + rv[w2] as usize;
                        for &second in &sorted[starts[w2] as usize..ends[w2] as usize] {
                            let double = pack(
                                base_seq as u32 | ((second as u32) << 8),
                                total_bits as u32,
                                2,
                            );
                            dt[base..base + span].fill(double);
                            base += span;
                        }
                    }
                    start += length;
                }
            } else {
                // no room for a second symbol: single-symbol entries
                let span = 1usize << (TARGET_LOG - nb_bits);
                let mut start = rank_val0[w] as usize;
                for &sym in &sorted[begin..end] {
                    dt[start..start + span].fill(pack(sym as u32, nb_bits as u32, 1));
                    start += span;
                }
            }
        }
    }

    /// The double-symbol table, empty when it was not built for the current
    /// table description.
    pub(crate) fn x2_table(&self) -> &[u32] {
        &self.x2
    }
}

impl Default for HuffmanTable {
    fn default() -> Self {
        Self::new()
    }
}

/// A single entry in the table contains the decoded symbol/literal and the
/// size of the prefix code.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Entry {
    /// The byte that the prefix code replaces during encoding.
    symbol: u8,
    /// The number of bits the prefix code occupies.
    num_bits: u8,
}

/// Assert that the provided value is greater than zero, and returns the
/// 32 - the number of leading zeros
fn highest_bit_set(x: u32) -> u32 {
    assert!(x > 0);
    u32::BITS - x.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Raw (4-bit) weight description: header 128 + count, then the nibbles
    /// (high first), the trailing low nibble of an odd count is padding.
    fn raw_table(weights: &[u8]) -> Vec<u8> {
        let mut out = vec![127 + weights.len() as u8];
        for pair in weights.chunks(2) {
            let lo = if pair.len() == 2 {
                pair[1]
            } else {
                0
            };
            out.push((pair[0] << 4) | lo);
        }
        out
    }

    #[test]
    fn rejects_weight_sets_without_rank_one_symbols() {
        // [2, 2] closes Kraft-exact with the inferred weight 3, but an empty
        // maximum-weight class cannot come from a real Huffman tree
        // (libzstd's rankStats[1] < 2 check).
        let mut t = HuffmanTable::new();
        match t.build_decoder(&raw_table(&[2, 2])) {
            Err(HuffmanTableError::InvalidWeightOneCount { got: 0 }) => {},
            other => panic!("expected InvalidWeightOneCount(0), got {other:?}"),
        }
        // [3] has the same shape one weight class up.
        let mut t = HuffmanTable::new();
        match t.build_decoder(&raw_table(&[3])) {
            Err(HuffmanTableError::InvalidWeightOneCount { got: 0 }) => {},
            other => panic!("expected InvalidWeightOneCount(0), got {other:?}"),
        }
    }

    #[test]
    fn accepts_canonical_weight_sets() {
        // [2, 1]: the inferred weight 1 closes an even weight-1 pair.
        let mut t = HuffmanTable::new();
        let src = raw_table(&[2, 1]);
        assert_eq!(t.build_decoder(&src).unwrap(), src.len() as u32);
        assert_eq!(t.max_num_bits, 2);
        // [1, 1, 1, 1]: four 3-bit codes plus the inferred 1-bit code.
        let mut t = HuffmanTable::new();
        let src = raw_table(&[1, 1, 1, 1]);
        assert_eq!(t.build_decoder(&src).unwrap(), src.len() as u32);
        assert_eq!(t.max_num_bits, 3);
        // A single written weight is the two-symbol minimum (header 128).
        let mut t = HuffmanTable::new();
        let src = raw_table(&[1]);
        assert_eq!(t.build_decoder(&src).unwrap(), src.len() as u32);
        assert_eq!(t.max_num_bits, 1);
    }

    /// Deterministic xorshift64 stream for the fill equivalence generator.
    fn rng_next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// A random valid weight list built in the slot domain: the inferred
    /// last symbol's span is reserved first, the remaining 2^max_bits slots
    /// are split into random aligned spans, and the set is rejected unless
    /// it satisfies the rank-one-even tree rule and the 255-symbol cap.
    /// `tail` biases the walk towards weight 1 to 3 for long-tail shapes.
    fn gen_weight_set(state: &mut u64, tail: bool) -> Option<Vec<u8>> {
        let r = |state: &mut u64, n: u64| rng_next(state) % n;
        let max_bits = 1 + r(state, 11);
        let last_weight = 1 + r(state, max_bits);
        let mut budget = (1u32 << max_bits) - (1u32 << (last_weight - 1));
        let mut weights = Vec::new();
        while budget > 0 {
            if weights.len() >= 248 {
                return None;
            }
            // 2^(hbs(budget)-1) <= budget, so any weight up to hbs(budget) fits
            let top = (32 - budget.leading_zeros()) as u64;
            let w = 1 + r(
                state,
                if tail {
                    top.min(3)
                } else {
                    top
                },
            );
            weights.push(w as u8);
            budget -= 1 << (w - 1);
        }
        let ones = weights.iter().fold(0usize, |n, &w| n + usize::from(w == 1))
            + usize::from(last_weight == 1);
        if ones < 2 || !ones.is_multiple_of(2) {
            return None;
        }
        // decorrelate symbol id from weight magnitude
        for i in (1..weights.len()).rev() {
            let j = (rng_next(state) as usize) % (i + 1);
            weights.swap(i, j);
        }
        Some(weights)
    }

    /// The rank-grouped fill and the scatter reference must produce identical
    /// `decode` and `packed` tables for every valid weight distribution: the
    /// table is a pure function of the weights, and the fast fill may never
    /// drift from the scatter semantics the format defines.
    #[test]
    fn rank_grouped_fill_matches_scatter() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut sets: Vec<Vec<u8>> = vec![vec![1], vec![1, 1, 1, 1]];
        for mb in 1..=7u32 {
            sets.push(vec![1; (1 << mb) - 1]);
        }
        let mut trials = 0;
        while trials < 400 {
            let tail = rng_next(&mut state) % 4 < 2;
            if let Some(weights) = gen_weight_set(&mut state, tail) {
                sets.push(weights);
                trials += 1;
            }
        }
        for (n, weights) in sets.iter().enumerate() {
            // raw 4-bit descriptions cap at 128 weights; FSE-encode the wide
            // ones and every sixteenth set to cover both weights-header paths
            let src = if weights.len() <= 128 && (n % 16 != 0 || weights.len() < 4) {
                raw_table(weights)
            } else {
                fse_table(weights)
            };
            let mut ranked = HuffmanTable::new();
            assert_eq!(ranked.build_decoder(&src).unwrap() as usize, src.len());
            let mut scattered = HuffmanTable::new();
            scattered.build_decoder(&src).unwrap();
            scattered.fill_decode_tables_scatter();
            assert_eq!(
                ranked.decode, scattered.decode,
                "decode mismatch for {weights:?}"
            );
            assert_eq!(
                ranked.packed, scattered.packed,
                "packed mismatch for {weights:?}"
            );
        }
    }

    /// FSE-compressed weights description (the same pipeline
    /// `write_table_desc` produces) for hand-chosen weight lists.
    fn fse_table(weights: &[u8]) -> Vec<u8> {
        use crate::{bit_io::BitWriter, fse::fse_encoder};

        let mut counts = [0u32; 12];
        let mut max_w = 0usize;
        for &w in weights {
            counts[w as usize] += 1;
            max_w = max_w.max(w as usize);
        }
        let n = weights.len();
        let table_log = fse_encoder::optimal_table_log(6, n, max_w);
        let mut norm = [0i32; 12];
        assert!(fse_encoder::normalize_count(
            &mut norm, table_log, &counts, n, max_w, false
        ));
        let mut fse = fse_encoder::FseBuildScratch::default();
        let table =
            fse_encoder::build_table_from_probabilities_into(&norm[..=max_w], table_log, &mut fse);
        let mut body = Vec::new();
        {
            let mut bits = BitWriter::from(&mut body);
            let mut encoder = fse_encoder::FSEEncoder::new(table, &mut bits);
            encoder.encode_interleaved(weights);
            encoder.finish(&mut fse);
            bits.flush();
        }
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(body.len() as u8);
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn fse_weights_decode_count_bound() {
        // 251 ones + 2 twos: sum 255, leftover 1, rank-1 count 252 — the
        // maximum legal written-weight count (255 with the inferred one).
        let weights: Vec<u8> = core::iter::repeat_n(1, 251)
            .chain(core::iter::repeat_n(2, 2))
            .collect();
        let src = fse_table(&weights);
        let mut t = HuffmanTable::new();
        assert_eq!(t.build_decoder(&src).unwrap(), src.len() as u32);
        assert_eq!(t.weights(), &weights[..]);
        assert_eq!(t.max_num_bits, 8);

        // One more weight exceeds the u8 alphabet once the last symbol is
        // inferred; the count bound must reject it on every exit path of
        // the interleaved decode loop.
        let weights: Vec<u8> = core::iter::repeat_n(1, 252)
            .chain(core::iter::repeat_n(2, 4))
            .collect();
        let src = fse_table(&weights);
        let mut t = HuffmanTable::new();
        match t.build_decoder(&src) {
            Err(HuffmanTableError::TooManyWeights { got }) => assert_eq!(got, 256),
            other => panic!("expected TooManyWeights(256), got {other:?}"),
        }
    }

    /// Like [`fse_table`] but the NCount additionally declares the weight
    /// symbol `declared` (probability 1) that the encoded weight list never
    /// emits — the shape that probes the alphabet cap independent of the
    /// decoded weight set.
    fn fse_table_with_declared(weights: &[u8], declared: usize) -> Vec<u8> {
        use crate::{bit_io::BitWriter, fse::fse_encoder};

        // 64 slots at accuracy log 6: 40/20/3 for the emitted weights 0/1/2
        // plus 1 for the declared-but-never-emitted symbol.
        let mut norm = [0i32; 14];
        norm[0] = 40;
        norm[1] = 20;
        norm[2] = 3;
        norm[declared] = 1;
        let mut fse = fse_encoder::FseBuildScratch::default();
        let table = fse_encoder::build_table_from_probabilities_into(&norm, 6, &mut fse);
        let mut body = Vec::new();
        {
            let mut bits = BitWriter::from(&mut body);
            let mut encoder = fse_encoder::FSEEncoder::new(table, &mut bits);
            encoder.encode_interleaved(weights);
            encoder.finish(&mut fse);
            bits.flush();
        }
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(body.len() as u8);
        out.extend_from_slice(&body);
        out
    }

    /// The weight-alphabet NCount is capped at symbol 11 (HUF_TABLELOG_MAX-1):
    /// libzstd's FSE workspace sizing in HUF_readStats rejects anything above
    /// at the table read — even when the symbol is never emitted, a shape no
    /// Kraft/tablelog bound on the decoded weights can reject. Verified
    /// against libzstd v1.5.7: declared 11 parses, 12+ returns tableLog_tooLarge.
    #[test]
    fn weight_fse_alphabet_cap() {
        use crate::decoding::errors::FSETableError;

        let weights: Vec<u8> = core::iter::repeat_n(1, 251)
            .chain(core::iter::repeat_n(2, 2))
            .collect();
        // Boundary: symbol 11 declared but never emitted still parses.
        let src = fse_table_with_declared(&weights, 11);
        let mut t = HuffmanTable::new();
        assert_eq!(t.build_decoder(&src).unwrap(), src.len() as u32);
        assert_eq!(t.weights(), &weights[..]);
        // One past the cap: rejected at the NCount read.
        let src = fse_table_with_declared(&weights, 12);
        let mut t = HuffmanTable::new();
        match t.build_decoder(&src) {
            Err(HuffmanTableError::FSETableError(FSETableError::TooManySymbols { got })) => {
                assert_eq!(got, 13);
            },
            other => panic!("expected TooManySymbols(13), got {other:?}"),
        }
    }
}
