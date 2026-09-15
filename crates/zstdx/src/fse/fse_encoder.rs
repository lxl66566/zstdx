use alloc::vec::Vec;

use crate::{bit_io::BitWriter, encoding::seq_codes::SEQ_CODE_SPACE};

pub(crate) struct FSEEncoder<'output, V: AsMut<Vec<u8>>> {
    pub(super) table: FSETable,
    writer: &'output mut BitWriter<V>,
}

impl<V: AsMut<Vec<u8>>> FSEEncoder<'_, V> {
    pub fn new(table: FSETable, writer: &mut BitWriter<V>) -> FSEEncoder<'_, V> {
        FSEEncoder { table, writer }
    }

    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn into_table(self) -> FSETable {
        self.table
    }

    /// Consume the encoder, returning its table's transition buffer to a
    /// build pool (see [`FseBuildScratch`]).
    pub(crate) fn finish(self, scratch: &mut FseBuildScratch) {
        self.table.recycle(scratch);
    }

    /// Encodes the data using the provided table
    /// Writes
    /// * Table description
    /// * Encoded data
    /// * Last state index
    /// * Padding bits to fill up last byte
    #[cfg(any(test, feature = "fuzz_exports"))]
    pub fn encode(&mut self, data: &[u8]) {
        self.write_table();

        let mut state = self.table.start_index(data[data.len() - 1]);
        for x in data[0..data.len() - 1].iter().rev().copied() {
            let next = self.table.next_state(x, state);
            self.writer
                .write_bits(next.diff as u64, next.num_bits as usize);
            state = next.index;
        }
        self.writer
            .write_bits(state as u64, self.acc_log() as usize);

        let bits_to_fill = self.writer.misaligned();
        if bits_to_fill == 0 {
            self.writer.write_bits(1u32, 8);
        } else {
            self.writer.write_bits(1u32, bits_to_fill);
        }
    }

    /// Encodes the data using the provided table but with two interleaved streams
    /// Writes
    /// * Table description
    /// * Encoded data with two interleaved states
    /// * Both Last state indexes
    /// * Padding bits to fill up last byte
    pub fn encode_interleaved(&mut self, data: &[u8]) {
        self.write_table();

        let mut state_1 = self.table.start_index(data[data.len() - 1]);
        let mut state_2 = self.table.start_index(data[data.len() - 2]);

        // The first two symbols are represented by the start states
        // Then encode the state transitions for two symbols at a time
        let mut idx = data.len() - 4;
        loop {
            {
                let state = state_1;
                let x = data[idx + 1];
                let next = self.table.next_state(x, state);
                self.writer
                    .write_bits(next.diff as u64, next.num_bits as usize);
                state_1 = next.index;
            }
            {
                let state = state_2;
                let x = data[idx];
                let next = self.table.next_state(x, state);
                self.writer
                    .write_bits(next.diff as u64, next.num_bits as usize);
                state_2 = next.index;
            }

            if idx < 2 {
                break;
            }
            idx -= 2;
        }

        // Determine if we have an even or odd number of symbols to encode
        // If odd we need to encode the last states transition and encode the final states in the
        // flipped order
        if idx == 1 {
            let state = state_1;
            let x = data[0];
            let next = self.table.next_state(x, state);
            self.writer
                .write_bits(next.diff as u64, next.num_bits as usize);
            state_1 = next.index;

            self.writer
                .write_bits(state_2 as u64, self.acc_log() as usize);
            self.writer
                .write_bits(state_1 as u64, self.acc_log() as usize);
        } else {
            self.writer
                .write_bits(state_1 as u64, self.acc_log() as usize);
            self.writer
                .write_bits(state_2 as u64, self.acc_log() as usize);
        }

        let bits_to_fill = self.writer.misaligned();
        if bits_to_fill == 0 {
            self.writer.write_bits(1u32, 8);
        } else {
            self.writer.write_bits(1u32, bits_to_fill);
        }
    }

    fn write_table(&mut self) {
        self.table.write_table(self.writer);
    }

    pub(super) fn acc_log(&self) -> u8 {
        self.table.acc_log()
    }
}

/// log2 for entropy and table-cost estimates. `f64::log2` needs std; the
/// no-std fallback is a linear-mantissa approximation (error < 0.086),
/// which every consumer tolerates: the estimates only steer selections
/// between options whose costs differ by percent-level margins.
#[inline(always)]
pub(crate) fn approx_log2(x: f64) -> f64 {
    #[cfg(feature = "std")]
    {
        x.log2()
    }
    #[cfg(not(feature = "std"))]
    {
        let bits = x.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i32 - 1023;
        let frac = (bits & ((1u64 << 52) - 1)) as f64 / (1u64 << 52) as f64;
        exp as f64 + frac
    }
}

/// Recycled scratch for entropy-table builds, pooled in the compressor
/// state: transition buffers only need capacity, because every live
/// symbol's row is fully overwritten by the build walk and dead rows are
/// never read (wire codes stay below the histogram's `max_symbol`). A
/// buffer zeroes its tail at most once per size, not once per table.
#[derive(Default)]
pub(crate) struct FseBuildScratch {
    /// State-owner map, refilled per build.
    owner: Vec<u8>,
    /// Retired transition buffers (their `len` is the initialized prefix).
    spare: Vec<Vec<u32>>,
}

/// Pool depth: the three sequence tables plus the Huffman weight table can
/// be in flight per block.
const FSE_SPARE_CAP: usize = 4;

impl FseBuildScratch {
    /// Take a buffer whose first `len` entries are initialized.
    fn take_transitions(&mut self, len: usize) -> Vec<u32> {
        let mut v = match self.spare.iter().position(|v| v.capacity() >= len) {
            Some(i) => self.spare.swap_remove(i),
            None => Vec::with_capacity(len),
        };
        if v.len() < len {
            v.resize(len, 0);
        } else {
            v.truncate(len);
        }
        v
    }

    /// Keep a retired buffer: at capacity, the pool swaps out its smallest.
    fn recycle(&mut self, v: Vec<u32>) {
        if self.spare.len() < FSE_SPARE_CAP {
            self.spare.push(v);
        } else if self.spare.iter().all(|s| s.capacity() >= v.capacity()) {
            // Every pooled buffer is at least as large: drop the retiree.
        } else {
            let smallest = self
                .spare
                .iter_mut()
                .min_by_key(|s| s.capacity())
                .expect("pool is non-empty at capacity");
            *smallest = v;
        }
    }
}

#[derive(Debug, Clone)]
pub struct FSETable {
    /// Normalized probability per symbol: positive weight, -1 for the
    /// low-probability wire form, 0 for absent symbols. `write_table`
    /// serializes this array in symbol order.
    probs: [i32; 256],
    /// Encoding start state per symbol: the index of its lowest-baseline
    /// state (mirrors the state order the old per-symbol lists kept).
    start: [u16; 256],
    /// Sum of all probabilities (2^acc_log).
    pub(crate) table_size: usize,
    /// Flat encoder transition table indexed by `symbol * table_size + state`,
    /// packing the transition as
    /// `(next_index << 16) | (num_bits << 12) | emitted_diff`, where
    /// `emitted_diff` is the value this state emits (`state - run_baseline`,
    /// precomputed per position at build time).
    /// Replaces a linear scan per encoded symbol.
    pub(super) transitions: Vec<u32>,
}

impl FSETable {
    /// O(1) encoder transition: packed `(next_index << 16) | (num_bits << 12)
    /// | emitted_diff` for encoding `symbol` while in state `idx`.
    #[inline(always)]
    pub(crate) fn transition(&self, symbol: u8, idx: usize) -> u32 {
        self.transitions[symbol as usize * self.table_size + idx]
    }

    /// Flat transition rows for hot encoder loops, as `(rows, row_shift)`:
    /// entry `code << row_shift | state` holds the same packed transition as
    /// [`transition`]. `table_size` is always a power of two, so the row
    /// stride is a shift instead of a multiply.
    pub(crate) fn transitions_flat(&self) -> (&[u32], u32) {
        debug_assert!(self.table_size.is_power_of_two());
        (&self.transitions[..], self.table_size.trailing_zeros())
    }

    /// Index of the state encoding a block's last `symbol` starts from.
    #[inline(always)]
    pub(crate) fn start_index(&self, symbol: u8) -> usize {
        self.start[symbol as usize] as usize
    }

    pub fn acc_log(&self) -> u8 {
        self.table_size.ilog2() as u8
    }

    /// Per-occurrence bit cost of `symbol` for repeat-table selection:
    /// log2(table_size / prob). `None` when the symbol has no state, which
    /// disqualifies the table from being repeated for a histogram that uses
    /// it. The -1 low-probability wire form behaves like a single state.
    pub(crate) fn symbol_bit_cost(&self, symbol: u8) -> Option<f64> {
        let p = self.probs[symbol as usize];
        if p == 0 {
            return None;
        }
        Some(approx_log2(
            self.table_size as f64 / p.unsigned_abs() as f64,
        ))
    }

    /// Return the transition buffer to a build pool (see
    /// [`FseBuildScratch`]); the table must not be used afterwards.
    pub(crate) fn recycle(self, scratch: &mut FseBuildScratch) {
        scratch.recycle(self.transitions);
    }

    pub(crate) fn write_table<V: AsMut<Vec<u8>>>(&self, writer: &mut BitWriter<V>) {
        writer.write_bits(self.acc_log() - 5, 4);
        let mut probability_counter = 0usize;
        let probability_sum = 1 << self.acc_log();

        let mut prob_idx = 0;
        while probability_counter < probability_sum {
            let max_remaining_value = probability_sum - probability_counter + 1;
            let bits_to_write = max_remaining_value.ilog2() + 1;
            let low_threshold = ((1 << bits_to_write) - 1) - (max_remaining_value);
            let mask = (1 << (bits_to_write - 1)) - 1;

            let prob = self.probs[prob_idx];
            prob_idx += 1;
            let value = (prob + 1) as u32;
            if value < low_threshold as u32 {
                writer.write_bits(value, bits_to_write as usize - 1);
            } else if value > mask {
                writer.write_bits(value + low_threshold as u32, bits_to_write as usize);
            } else {
                writer.write_bits(value, bits_to_write as usize);
            }

            if prob == -1 {
                probability_counter += 1;
            } else if prob > 0 {
                probability_counter += prob as usize;
            } else {
                let mut zeros = 0u8;
                // Trailing zero-probability symbols can run to the end of the
                // table; the outer loop stops on the probability sum anyway.
                while prob_idx < self.probs.len() && self.probs[prob_idx] == 0 {
                    zeros += 1;
                    prob_idx += 1;
                    if zeros == 3 {
                        writer.write_bits(3u8, 2);
                        zeros = 0;
                    }
                }
                writer.write_bits(zeros, 2);
            }
        }
        writer.write_bits(0u8, writer.misaligned());
    }
}

/// Reconstruct the transition an entry encodes. The 12-bit field packing
/// (see [`FSETable::transitions`]) limits tables to acc_log <= 12, which
/// `build_table_from_probabilities` asserts and every builder enforces.
#[derive(Debug, Clone)]
pub(crate) struct State {
    /// How many bits the range of this state needs to be encoded as
    pub(crate) num_bits: u8,
    /// The value this transition emits: the precomputed `state - baseline`
    pub(crate) diff: usize,
    /// Index of this state in the decoding table
    pub(crate) index: usize,
}

impl FSETable {
    pub(crate) fn next_state(&self, symbol: u8, idx: usize) -> State {
        let e = self.transition(symbol, idx);
        let num_bits = ((e >> 12) & 0xf) as u8;
        let diff = (e & 0xfff) as usize;
        State {
            num_bits,
            diff,
            index: (e >> 16) as usize,
        }
    }
}

pub(crate) fn build_table_from_data_into(
    data: impl Iterator<Item = u8>,
    max_log: u8,
    avoid_0_numbit: bool,
    scratch: &mut FseBuildScratch,
) -> FSETable {
    let mut counts = [0; 256];
    let mut max_symbol = 0;
    for x in data {
        counts[x as usize] += 1;
    }
    for (idx, count) in counts.iter().copied().enumerate() {
        if count > 0 {
            max_symbol = idx;
        }
    }
    build_table_from_counts(&counts[..=max_symbol], max_log, avoid_0_numbit, scratch)
}

pub fn build_table_from_data(
    data: impl Iterator<Item = u8>,
    max_log: u8,
    avoid_0_numbit: bool,
) -> FSETable {
    build_table_from_data_into(
        data,
        max_log,
        avoid_0_numbit,
        &mut FseBuildScratch::default(),
    )
}

/// libzstd's FSE_minTableLog: the smallest table size that can safely
/// represent `src_size` symbols over `max_symbol` distinct values.
fn min_table_log(src_size: usize, max_symbol: usize) -> u8 {
    let min_bits_src = usize::BITS - src_size.leading_zeros() + 1; // highbit + 1
    let min_bits_symbols = (usize::BITS - 1 - max_symbol.leading_zeros()) + 2;
    min_bits_src.min(min_bits_symbols) as u8
}

/// libzstd's FSE_optimalTableLog: pick the accuracy that pays for itself at
/// this number of symbols (fewer symbols don't justify a big table), still
/// respecting the minimum needed to represent every symbol value.
pub(crate) fn optimal_table_log(max_log: u8, src_size: usize, max_symbol: usize) -> u8 {
    debug_assert!(src_size > 1);
    let max_bits_src = (usize::BITS - (src_size - 1).leading_zeros() - 1) - 2; // highbit - 2
    let min_bits = min_table_log(src_size, max_symbol);
    let mut table_log = max_log as u32;
    if max_bits_src < table_log {
        table_log = max_bits_src;
    }
    if (min_bits as u32) > table_log {
        table_log = min_bits as u32;
    }
    table_log = table_log.clamp(5, 12);
    table_log as u8
}

/// A degenerate one-state table for the sequence-table RLE wire mode: the
/// table description is a single code byte and every transition costs zero
/// bits, so encoding routes through the normal path unchanged.
pub(crate) fn rle_table(code: u8, scratch: &mut FseBuildScratch) -> FSETable {
    let mut transitions = scratch.take_transitions(256);
    // table_size is 1, so `symbol * table_size + state` indexes at `symbol`;
    // only the RLE code's degenerate zero transition is ever read.
    transitions[code as usize] = 0;
    let mut table = FSETable {
        probs: [0; 256],
        start: [0; 256],
        table_size: 1,
        transitions,
    };
    table.probs[code as usize] = 1;
    table.start[code as usize] = 0;
    table
}

/// libzstd's set_compressed table build for sequence code histograms: the
/// table log comes from the full sequence count, the last sequence's code
/// loses one count (its symbol is carried by the initial state), and the
/// remaining counts are normalized with the ported FSE_normalizeCount.
/// Returns None when normalization fails; the caller then falls back to the
/// predefined table. `counts` is sized to the sequence-code space (wire
/// codes never reach 64), which also caps the normalization scratch.
pub(crate) fn build_normalized_table(
    counts: &mut [u32; SEQ_CODE_SPACE],
    nb_seq: usize,
    max_symbol: usize,
    max_log: u8,
    last_code: u8,
    scratch: &mut FseBuildScratch,
) -> Option<FSETable> {
    debug_assert!(nb_seq > 2);
    let table_log = optimal_table_log(max_log, nb_seq, max_symbol);
    let total = if counts[last_code as usize] > 1 {
        counts[last_code as usize] -= 1;
        nb_seq - 1
    } else {
        nb_seq
    };
    let mut norm = [0i32; SEQ_CODE_SPACE];
    if !normalize_count(
        &mut norm,
        table_log,
        counts,
        total,
        max_symbol,
        total >= 2048,
    ) {
        return None;
    }
    Some(build_table_from_probabilities_into(
        &norm[..=max_symbol],
        table_log,
        scratch,
    ))
}

/// Rounding-threshold table from libzstd's FSE_normalizeCount: fractions of a
/// vStep that justify rounding a small probability up.
const RTB_TABLE: [u64; 8] = [0, 473195, 504333, 520860, 550000, 700000, 750000, 830000];

/// Port of libzstd's FSE_normalizeCount: scale `count` (total `total` over
/// symbols 0..=max_symbol) to probabilities summing to exactly
/// `1 << table_log`, using -1 (or 1 when `use_low_prob` is false) as the
/// minimum weight. Returns false when even the M2 fallback cannot find a
/// valid distribution (the caller should then avoid a custom table).
pub(crate) fn normalize_count(
    norm: &mut [i32],
    table_log: u8,
    count: &[u32],
    total: usize,
    max_symbol: usize,
    use_low_prob: bool,
) -> bool {
    debug_assert!(norm.len() > max_symbol);
    let low_prob: i32 = if use_low_prob {
        -1
    } else {
        1
    };
    let scale = 62 - table_log as u32;
    let step = (1u64 << 62) / (total as u64).max(1);
    let v_step = 1u64 << (scale - 20);
    let mut still_to_distribute = 1i32 << table_log;
    let mut largest = 0usize;
    let mut largest_p = 0i32;
    let low_threshold = (total >> table_log) as u32;

    for s in 0..=max_symbol {
        let c = count[s];
        if c == 0 {
            norm[s] = 0;
            continue;
        }
        if c <= low_threshold {
            norm[s] = low_prob;
            still_to_distribute -= 1;
        } else {
            let scaled = c as u64 * step;
            let mut proba = (scaled >> scale) as i32;
            if proba < 8 {
                let rest_to_beat = v_step * RTB_TABLE[proba as usize];
                proba += (scaled - ((proba as u64) << scale) > rest_to_beat) as i32;
            }
            if proba > largest_p {
                largest_p = proba;
                largest = s;
            }
            norm[s] = proba;
            still_to_distribute -= proba;
        }
    }
    if -still_to_distribute >= norm[largest] >> 1 {
        // Corner case: the largest symbol would need more than its fair
        // share added back; redistribute with the secondary method.
        normalize_m2(norm, table_log, count, total, max_symbol, low_prob)
    } else {
        norm[largest] += still_to_distribute;
        true
    }
}

/// Port of libzstd's FSE_normalizeM2 secondary normalization: assign 1 (or
/// the low-prob weight) to all small symbols, then distribute the rest
/// proportionally on a fixed-point staircase.
fn normalize_m2(
    norm: &mut [i32],
    table_log: u8,
    count: &[u32],
    mut total: usize,
    max_symbol: usize,
    low_prob: i32,
) -> bool {
    const NOT_YET_ASSIGNED: i32 = -2;
    let mut distributed: u32 = 0;

    let low_threshold = (total >> table_log) as u32;
    let mut low_one = ((total * 3) >> (table_log + 1)) as u32;

    for s in 0..=max_symbol {
        if count[s] == 0 {
            norm[s] = 0;
            continue;
        }
        if count[s] <= low_threshold {
            norm[s] = low_prob;
            distributed += 1;
            total -= count[s] as usize;
            continue;
        }
        if count[s] <= low_one {
            norm[s] = 1;
            distributed += 1;
            total -= count[s] as usize;
            continue;
        }
        norm[s] = NOT_YET_ASSIGNED;
    }
    let mut to_distribute = (1u32 << table_log) - distributed;
    if to_distribute == 0 {
        return true;
    }

    if total / to_distribute as usize > low_one as usize {
        // risk of rounding to zero
        low_one = ((total as u64 * 3) / (to_distribute as u64 * 2)) as u32;
        for s in 0..=max_symbol {
            if norm[s] == NOT_YET_ASSIGNED && count[s] <= low_one {
                norm[s] = 1;
                distributed += 1;
                total -= count[s] as usize;
            }
        }
        to_distribute = (1u32 << table_log) - distributed;
    }

    if distributed as usize == max_symbol + 1 {
        // All values are poor; give the remainder to the maximum.
        let mut max_v = 0usize;
        let mut max_c = 0u32;
        for (s, &c) in count.iter().enumerate().take(max_symbol + 1) {
            if c > max_c {
                max_c = c;
                max_v = s;
            }
        }
        norm[max_v] += to_distribute as i32;
        return true;
    }

    if total == 0 {
        // Everything was small; spread the remainder over positive entries.
        let mut s = 0usize;
        while to_distribute > 0 {
            if norm[s] > 0 {
                norm[s] += 1;
                to_distribute -= 1;
            }
            s = (s + 1) % (max_symbol + 1);
        }
        return true;
    }

    let v_step_log = 62 - table_log as u32;
    let mid = (1u64 << (v_step_log - 1)) - 1;
    let r_step = (((1u64 << v_step_log) * to_distribute as u64) + mid) / total as u64;
    let mut tmp_total = mid;
    for s in 0..=max_symbol {
        if norm[s] == NOT_YET_ASSIGNED {
            let end = tmp_total + (count[s] as u64 * r_step);
            let s_start = (tmp_total >> v_step_log) as u32;
            let s_end = (end >> v_step_log) as u32;
            let weight = s_end - s_start;
            if weight < 1 {
                return false;
            }
            norm[s] = weight as i32;
            tmp_total = end;
        }
    }
    true
}

fn build_table_from_counts(
    counts: &[usize],
    max_log: u8,
    legacy_avoid_0_numbit: bool,
    scratch: &mut FseBuildScratch,
) -> FSETable {
    let mut probs = [0; 256];
    let probs = &mut probs[..counts.len()];
    let mut min_count = 0;
    for (idx, count) in counts.iter().copied().enumerate() {
        probs[idx] = count as i32;
        if count > 0 && (count < min_count || min_count == 0) {
            min_count = count;
        }
    }

    // shift all probabilities down so that the lowest are 1
    min_count -= 1;
    let mut max_prob = 0i32;
    for prob in probs.iter_mut() {
        if *prob > 0 {
            *prob -= min_count as i32;
        }
        max_prob = max_prob.max(*prob);
    }

    if max_prob > 0 && max_prob as usize > probs.len() {
        let divisor = max_prob / (probs.len() as i32);
        for prob in probs.iter_mut() {
            if *prob > 0 {
                *prob = (*prob / divisor).max(1);
            }
        }
    }

    // normalize probabilities to a 2^x
    let sum = probs.iter().sum::<i32>();
    assert!(sum > 0);
    let sum = sum as usize;
    let acc_log = (sum.ilog2() as u8 + 1).max(5);
    // The transition packing (and the format itself) caps accuracy at 2^12
    // states; wider inputs clamp here instead of truncating baselines.
    let acc_log = u8::min(acc_log, 12).min(max_log);

    if sum < 1 << acc_log {
        // just raise the maximum probability as much as possible
        // TODO is this optimal?
        let diff = (1 << acc_log) - sum;
        let max = probs.iter_mut().max().unwrap();
        *max += diff as i32;
    } else {
        // decrease the smallest ones to 1 first
        let mut diff = sum - (1 << acc_log);
        while diff > 0 {
            let min = probs.iter_mut().filter(|prob| **prob > 1).min().unwrap();
            let decrease = usize::min(*min as usize - 1, diff);
            diff -= decrease;
            *min -= decrease as i32;
        }
    }
    // A distribution with a single distinct symbol has no second maximum to
    // move weight to; it keeps the full table weight and encodes with zero
    // bits per symbol.
    let has_second = {
        let max = *probs.iter().max().unwrap();
        probs.iter().any(|x| *x != max)
    };
    let max = probs.iter_mut().max().unwrap();
    if legacy_avoid_0_numbit && has_second && *max > 1 << (acc_log - 1) {
        let redistribute = *max - (1 << (acc_log - 1));
        *max -= redistribute;
        let max = *max;

        // find first occurence of the second_max to avoid lifting the last zero
        let second_max = *probs.iter_mut().filter(|x| **x != max).max().unwrap();
        let second_max = probs.iter_mut().find(|x| **x == second_max).unwrap();
        *second_max += redistribute;
        assert!(*second_max <= max);
    }

    build_table_from_probabilities_into(probs, acc_log, scratch)
}

pub(crate) fn build_table_from_probabilities(probs: &[i32], acc_log: u8) -> FSETable {
    let table_size = 1usize << acc_log;
    build_table_body(
        probs,
        acc_log,
        &mut alloc::vec![255u8; table_size],
        &mut alloc::vec![0u32; probs.len() * table_size],
    )
}

/// The pooled [`build_table_from_probabilities`]: recycles the transition
/// buffer through `scratch` instead of allocating (and zeroing) per table.
pub(crate) fn build_table_from_probabilities_into(
    probs: &[i32],
    acc_log: u8,
    scratch: &mut FseBuildScratch,
) -> FSETable {
    let table_size = 1usize << acc_log;
    let mut transitions = scratch.take_transitions(probs.len() * table_size);
    scratch.owner.clear();
    scratch.owner.resize(table_size, 255);
    build_table_body(probs, acc_log, &mut scratch.owner, &mut transitions)
}

fn build_table_body(
    probs: &[i32],
    acc_log: u8,
    owner: &mut Vec<u8>,
    transitions: &mut Vec<u32>,
) -> FSETable {
    // Entry packing gives 12 bits each to baseline and target index.
    debug_assert!(
        (1..=12).contains(&acc_log),
        "acc_log {acc_log} exceeds the transition packing"
    );
    let table_size = 1usize << acc_log;
    let mut probs_full = [0i32; 256];
    probs_full[..probs.len()].copy_from_slice(probs);

    let mut start = [0u16; 256];

    // -1 symbols take state indices from the top downward.
    let mut negative_idx = (table_size - 1) as i32;
    for (symbol, prob) in probs.iter().copied().enumerate() {
        if prob == -1 {
            owner[negative_idx as usize] = symbol as u8;
            start[symbol] = negative_idx as u16;
            negative_idx -= 1;
        }
    }

    // Positive symbols spread their states through the remaining space with
    // the classic next_position walk.
    let mut idx = 0usize;
    for (symbol, prob) in probs.iter().copied().enumerate() {
        if prob <= 0 {
            continue;
        }
        for _ in 0..prob {
            owner[idx] = symbol as u8;
            idx = next_position(idx, table_size);
            while idx > negative_idx as usize {
                idx = next_position(idx, table_size);
            }
        }
    }

    // Per-symbol counters for the baseline walk below.
    let mut seen = [0u32; 256];
    let mut baseline = [0usize; 256];
    let mut prev_baseline = [usize::MAX; 256];

    // A -1 symbol has a single state spanning the whole index range: its
    // entry is the top-region slot recorded above with acc_log output bits.
    for (symbol, prob) in probs.iter().copied().enumerate() {
        if prob != -1 {
            continue;
        }
        let index = start[symbol] as usize;
        let entry = (index as u32) << 16 | (acc_log as u32) << 12;
        let base = symbol * table_size;
        // Baseline 0: the emitted value at row position p is p itself.
        for (p, slot) in transitions[base..base + table_size].iter_mut().enumerate() {
            *slot = entry | p as u32;
        }
    }

    // Assign baselines in ascending state-index order (identical to the old
    // index sort + sequential walk): the first `double` states of a symbol
    // emit one extra bit and their baselines wrap mod table_size; the state
    // right after the wrap is the encoding start state.
    for (i, &owner_symbol) in owner.iter().enumerate() {
        let symbol = owner_symbol as usize;
        let prob = probs_full[symbol];
        if prob <= 0 {
            continue;
        }
        let prob = prob as u32;
        let prob_log = if prob.is_power_of_two() {
            prob.ilog2()
        } else {
            prob.ilog2() + 1
        };
        let rounded_up = 1u32 << prob_log;
        let double_states = rounded_up - prob;
        let num_bits = acc_log - prob_log as u8;
        let k = seen[symbol];
        if k == 0 {
            let single_states = prob - double_states;
            baseline[symbol] = (single_states as usize * (1 << num_bits)) % table_size;
        }
        let (nb, width) = if k < double_states {
            (num_bits + 1, 1usize << (num_bits + 1))
        } else {
            (num_bits, 1usize << num_bits)
        };
        let b = baseline[symbol];
        let entry = ((i as u32) << 16) | ((nb as u32) << 12);
        // The run [b, b+width) emits its own offset: a row is indexed by the
        // state itself, so the transition value `state - b` is a build-time
        // constant per position — the hot encoders read it straight from the
        // entry instead of subtracting the run base at runtime.
        for (j, slot) in transitions[symbol * table_size + b..symbol * table_size + b + width]
            .iter_mut()
            .enumerate()
        {
            *slot = entry | j as u32;
        }
        if b < prev_baseline[symbol] {
            start[symbol] = i as u16;
        }
        prev_baseline[symbol] = b;
        baseline[symbol] = if k < double_states {
            (b + width) % table_size
        } else {
            b + width
        };
        seen[symbol] = k + 1;
    }

    FSETable {
        probs: probs_full,
        start,
        table_size,
        transitions: core::mem::take(transitions),
    }
}

/// Calculate the position of the next entry of the table given the current
/// position and size of the table.
fn next_position(mut p: usize, table_size: usize) -> usize {
    p += (table_size >> 1) + (table_size >> 3) + 3;
    p &= table_size - 1;
    p
}

const ML_DIST: &[i32] = &[
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];

const LL_DIST: &[i32] = &[
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];

const OF_DIST: &[i32] = &[
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

pub(crate) fn default_ml_table() -> FSETable {
    build_table_from_probabilities(ML_DIST, 6)
}

pub(crate) fn default_ll_table() -> FSETable {
    build_table_from_probabilities(LL_DIST, 6)
}

pub(crate) fn default_of_table() -> FSETable {
    build_table_from_probabilities(OF_DIST, 5)
}

#[cfg(test)]
mod soa_tests {
    use alloc::vec;

    use super::*;

    /// Flattened reference table: `(symbol, num_bits, baseline, index)` rows
    /// in per-symbol baseline order, per-symbol start indices, table size.
    type RefTable = (Vec<(u8, u8, usize, usize)>, Vec<usize>, usize);

    /// Reference: the pre-SoA construction with per-symbol state lists.
    fn build_reference(probs: &[i32], acc_log: u8) -> RefTable {
        #[derive(Clone)]
        struct RState {
            num_bits: u8,
            baseline: usize,
            last_index: usize,
            index: usize,
        }
        let table_size = 1usize << acc_log;
        let mut sym_states: Vec<Vec<RState>> = vec![Vec::new(); 256];
        let mut negative_idx = (table_size - 1) as i32;
        for (symbol, prob) in probs.iter().copied().enumerate() {
            if prob == -1 {
                sym_states[symbol].push(RState {
                    num_bits: acc_log,
                    baseline: 0,
                    last_index: table_size - 1,
                    index: negative_idx as usize,
                });
                negative_idx -= 1;
            }
        }
        let mut idx = 0usize;
        for (symbol, prob) in probs.iter().copied().enumerate() {
            if prob <= 0 {
                continue;
            }
            for _ in 0..prob {
                sym_states[symbol].push(RState {
                    num_bits: 0,
                    baseline: 0,
                    last_index: 0,
                    index: idx,
                });
                idx = next_position(idx, table_size);
                while idx > negative_idx as usize {
                    idx = next_position(idx, table_size);
                }
            }
        }
        for (symbol, prob) in probs.iter().copied().enumerate() {
            if prob <= 0 {
                continue;
            }
            let prob = prob as u32;
            let st = &mut sym_states[symbol];
            st.sort_by_key(|l| l.index);
            let prob_log = if prob.is_power_of_two() {
                prob.ilog2()
            } else {
                prob.ilog2() + 1
            };
            let rounded_up = 1u32 << prob_log;
            let double_states = rounded_up - prob;
            let single_states = prob - double_states;
            let num_bits = acc_log - prob_log as u8;
            let mut baseline = (single_states as usize * (1 << num_bits)) % table_size;
            for (k, state) in st.iter_mut().enumerate() {
                if (k as u32) < double_states {
                    let nb = num_bits + 1;
                    state.baseline = baseline;
                    state.num_bits = nb;
                    state.last_index = baseline + ((1 << nb) - 1);
                    baseline += 1 << nb;
                    baseline %= table_size;
                } else {
                    state.baseline = baseline;
                    state.num_bits = num_bits;
                    state.last_index = baseline + ((1 << num_bits) - 1);
                    baseline += 1 << num_bits;
                }
            }
            st.sort_by_key(|l| l.baseline);
        }
        // flatten: (symbol, nb, baseline, index) in baseline order per symbol
        let mut flat = Vec::new();
        let mut starts = vec![0usize; 256];
        for (symbol, st) in sym_states.iter().enumerate() {
            if st.is_empty() {
                continue;
            }
            starts[symbol] = st[0].index;
            for s in st {
                flat.push((symbol as u8, s.num_bits, s.baseline, s.index));
            }
        }
        (flat, starts, table_size)
    }

    #[test]
    fn soa_matches_reference() {
        let mut seed = 0x12345678u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..400 {
            let acc_log = 5 + (rand() % 4) as u8;
            let table_size = 1usize << acc_log;
            let nsym = 1 + (rand() % 20) as usize;
            let mut probs = vec![0i32; nsym];
            // -1 weights occupy one table slot each, so positives must sum to
            // table_size minus the number of -1 entries.
            let mut remaining = table_size as i32;
            for p in &mut probs {
                if remaining <= 1 {
                    if remaining == 1 {
                        *p = 1;
                        remaining -= 1;
                    }
                    continue;
                }
                let r = rand();
                if r % 7 == 0 {
                    *p = -1;
                    remaining -= 1;
                } else {
                    let max = remaining - 1;
                    let v = 1 + (r % max as u64) as i32;
                    *p = v;
                    remaining -= v;
                }
            }
            if remaining > 0 {
                probs[0] += remaining;
            }
            let sum: i32 = probs
                .iter()
                .map(|p| {
                    if *p == -1 {
                        1
                    } else {
                        *p
                    }
                })
                .sum();
            if sum != table_size as i32 {
                continue;
            }
            let table = build_table_from_probabilities(&probs, acc_log);
            let (ref_flat, ref_starts, ref_ts) = build_reference(&probs, acc_log);
            assert_eq!(table.table_size, ref_ts, "case {case} ts");
            for (s, &start) in ref_starts.iter().enumerate().take(nsym) {
                assert_eq!(
                    table.start[s], start as u16,
                    "case {case} start {s} probs {probs:?} acc {acc_log}"
                );
            }
            // rebuild flat from new table transitions
            for (symbol, nb, baseline, _index) in &ref_flat {
                let e = table.transitions[*symbol as usize * ref_ts + *baseline];
                assert_eq!(
                    ((e >> 12) & 0xf) as u8,
                    *nb,
                    "case {case} sym {symbol} base {baseline} nb"
                );
                // The run's first position emits a zero diff: the low field
                // holds `position - run_baseline`.
                assert_eq!(
                    e & 0xfff,
                    0,
                    "case {case} sym {symbol} base {baseline} diff"
                );
            }
        }
    }
}
