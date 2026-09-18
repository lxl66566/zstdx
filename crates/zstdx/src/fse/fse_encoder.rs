use alloc::{boxed::Box, vec::Vec};

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

        let acc_log = self.acc_log() as u32;
        let mut state = self.table.start_index(data[data.len() - 1]);
        for x in data[0..data.len() - 1].iter().rev().copied() {
            let (nb, emit, next) = self.table.step(x, state);
            self.writer.write_bits(emit as u64, nb as usize);
            state = next;
        }
        self.writer
            .write_bits((state & ((1 << acc_log) - 1)) as u64, acc_log as usize);

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

        let acc_log = self.acc_log() as u32;
        let mask = (1u32 << acc_log) - 1;
        let mut state_1 = self.table.start_index(data[data.len() - 1]);
        let mut state_2 = self.table.start_index(data[data.len() - 2]);

        // The first two symbols are represented by the start states
        // Then encode the state transitions for two symbols at a time
        let mut idx = data.len() - 4;
        loop {
            {
                let (nb, emit, next) = self.table.step(data[idx + 1], state_1);
                self.writer.write_bits(emit as u64, nb as usize);
                state_1 = next;
            }
            {
                let (nb, emit, next) = self.table.step(data[idx], state_2);
                self.writer.write_bits(emit as u64, nb as usize);
                state_2 = next;
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
            let (nb, emit, next) = self.table.step(data[0], state_1);
            self.writer.write_bits(emit as u64, nb as usize);
            state_1 = next;

            self.writer
                .write_bits((state_2 & mask) as u64, acc_log as usize);
            self.writer
                .write_bits((state_1 & mask) as u64, acc_log as usize);
        } else {
            self.writer
                .write_bits((state_1 & mask) as u64, acc_log as usize);
            self.writer
                .write_bits((state_2 & mask) as u64, acc_log as usize);
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
/// state: table buffers only need capacity, because the tt entries and the
/// state table are fully overwritten by the build walk (dead symbols get
/// the dead fill, every state slot is written). A buffer zeroes its tail at
/// most once per size, not once per table.
pub(crate) struct FseBuildScratch {
    /// State-owner map. High-water buffer: every slot up to `table_size` is
    /// written by each build (the row-start pass debug-asserts the sum
    /// invariant that guarantees coverage), so no per-build clearing.
    owner: Vec<u8>,
    /// Contiguous symbol-run staging for the no-low-prob spread fast path
    /// (`table_size + 8` bytes; the u64 run writes may overrun the last run
    /// by up to 7 bytes). High-water buffer, same coverage argument.
    spread: Vec<u8>,
    /// Retired table buffers (their `len` is the initialized prefix).
    spare: Vec<Vec<u64>>,
    /// Lane-split sequence-code histograms for `choose_tables_fast` (four
    /// sub-histograms per LL/ML/OF channel). Pooled, and only the
    /// [`SEQ_CODE_SPACE`]-entry prefix of each lane is cleared per use:
    /// wire codes never reach 64, so no increment can touch and no merge
    /// can read the stale tail — the per-block clear covers 3 KB, not the
    /// 12 KB a fresh 256-wide array costs. The lanes stay 256-wide so the
    /// per-sequence increments with unmasked u8 wire codes remain
    /// bounds-check-free.
    seq_lanes: Box<[[u32; 256]; 12]>,
}

/// Pool depth: the three sequence tables plus the Huffman weight table can
/// be in flight per block.
const FSE_SPARE_CAP: usize = 4;

impl Default for FseBuildScratch {
    fn default() -> Self {
        Self {
            owner: Vec::new(),
            spread: Vec::new(),
            spare: Vec::new(),
            seq_lanes: Box::new([[0; 256]; 12]),
        }
    }
}

impl FseBuildScratch {
    /// Borrow the lane histograms with every lane's touched prefix
    /// (the code space, < 64 entries) cleared; see [`Self::seq_lanes`].
    pub(crate) fn take_seq_lanes(&mut self) -> &mut [[u32; 256]; 12] {
        let lanes = self.seq_lanes.as_mut();
        for lane in lanes.iter_mut() {
            lane[..SEQ_CODE_SPACE].fill(0);
        }
        lanes
    }

    /// Take a buffer whose first `len` entries are initialized.
    fn take_tab(&mut self, len: usize) -> Vec<u64> {
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
    fn recycle(&mut self, v: Vec<u64>) {
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
    /// Biased encoding start state per symbol (`table_size + state index`,
    /// the scale `tab`'s state table carries): the symbol's lowest spread
    /// position, mirroring libzstd's `FSE_initCState2` ("the smallest state
    /// value possible" — its decode entry always consumes >= 1 bit).
    start: [u16; 256],
    /// Sum of all probabilities (2^acc_log).
    pub(crate) table_size: usize,
    /// Arithmetic tANS transition tables (libzstd's `FSE_CTable` form) in
    /// one buffer: `[tt: u64 x 256][state_table: u16 x table_size]`.
    /// `tt[code]` packs `delta_nb_bits` (low u32) and `delta_find_state`
    /// (high u32, two's complement); one step is
    /// `nb = (state + delta_nb_bits) >> 16`,
    /// `next = state_table[(state >> nb) + delta_find_state]`, and the
    /// emitted value is the biased state's low `nb` bits. Replaces the
    /// materialized `(max_symbol+1) x table_size` transition matrix: the
    /// build writes O(table_size) entries instead of
    /// O(max_symbol x table_size), and both tables stay L1-resident.
    tab: Vec<u64>,
}

/// u64 length of the tt region of [`FSETable::tab`].
const TT_LEN: usize = 256;
/// u16 index of the state table inside [`FSETable::tab`] (byte 2048).
const ST_U16_OFF: usize = TT_LEN * 4;

impl FSETable {
    /// One tANS encode step: `(nb_bits, emitted value, next state)` for
    /// encoding `code` while in the biased running state `state`. The
    /// emitted value is already masked to `nb_bits`.
    #[inline(always)]
    pub(crate) fn step(&self, code: u8, state: u32) -> (u32, u32, u32) {
        let t = self.tab[code as usize];
        let nb = state.wrapping_add(t as u32) >> 16;
        let emit = state & ((1 << nb) - 1);
        let idx = (state >> nb).wrapping_add((t >> 32) as u32);
        let next = self.state_table()[idx as usize] as u32;
        (nb, emit, next)
    }

    /// Raw table pointers for hot encode loops: the tt base (`u64` entries)
    /// and the state-table base (`u16` entries at the fixed byte offset
    /// 2048), so one base register serves both.
    pub(crate) fn tab_parts(&self) -> (*const u64, *const u16) {
        let tt = self.tab.as_ptr();
        // SAFETY: the state table lives at ST_U16_OFF inside the same
        // allocation, with `table_size` u16 entries.
        let st = unsafe { tt.cast::<u16>().add(ST_U16_OFF) };
        (tt, st)
    }

    fn state_table(&self) -> &[u16] {
        // SAFETY: the state table is `table_size` u16 entries at a fixed
        // offset inside the tab allocation (the buffer's u64 length covers
        // TT_LEN + ceil(table_size / 4)).
        unsafe {
            core::slice::from_raw_parts(
                self.tab.as_ptr().cast::<u16>().add(ST_U16_OFF),
                self.table_size,
            )
        }
    }

    /// Index of the state encoding a block's last `symbol` starts from
    /// (biased: `table_size + index`).
    #[inline(always)]
    pub(crate) fn start_index(&self, symbol: u8) -> u32 {
        self.start[symbol as usize] as u32
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

    /// Return the table buffer to a build pool (see
    /// [`FseBuildScratch`]); the table must not be used afterwards.
    pub(crate) fn recycle(self, scratch: &mut FseBuildScratch) {
        scratch.recycle(self.tab);
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

/// Legacy histogram normalization, only used by the FSE round-trip test
/// helper (the literals weight table and sequence tables use the ported
/// libzstd builders).
#[cfg(any(test, feature = "fuzz_exports"))]
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

#[cfg(any(test, feature = "fuzz_exports"))]
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
    let highbit = usize::BITS - (src_size - 1).leading_zeros() - 1;
    // libzstd computes this in U32 and lets it wrap: an underflowed cap (few
    // source symbols) simply never applies.
    let max_bits_src = highbit.wrapping_sub(2);
    let min_bits = min_table_log(src_size, max_symbol) as u32;
    let mut table_log = max_log as u32;
    if max_bits_src < table_log {
        table_log = max_bits_src;
    }
    if min_bits > table_log {
        table_log = min_bits;
    }
    table_log = table_log.clamp(5, 12);
    table_log as u8
}

/// A degenerate one-state table for the sequence-table RLE wire mode: the
/// table description is a single code byte and every transition costs zero
/// bits, so encoding routes through the normal path unchanged.
pub(crate) fn rle_table(code: u8, scratch: &mut FseBuildScratch) -> FSETable {
    let mut probs = [0i32; 256];
    probs[code as usize] = 1;
    build_table_from_probabilities_into(&probs, 0, scratch)
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

#[cfg(any(test, feature = "fuzz_exports"))]
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
    let mut owner = alloc::vec![0u8; table_size];
    let mut spread = alloc::vec![0u8; table_size + 8];
    let mut tab = alloc::vec![0u64; TT_LEN + table_size.div_ceil(4)];
    build_table_body(probs, acc_log, &mut owner, &mut spread, &mut tab)
}

/// The pooled [`build_table_from_probabilities`]: recycles the table buffer
/// through `scratch` instead of allocating (and zeroing) per table.
pub(crate) fn build_table_from_probabilities_into(
    probs: &[i32],
    acc_log: u8,
    scratch: &mut FseBuildScratch,
) -> FSETable {
    let table_size = 1usize << acc_log;
    let mut tab = scratch.take_tab(TT_LEN + table_size.div_ceil(4));
    // High-water growth only: the build overwrites every slot it reads.
    if scratch.owner.len() < table_size {
        scratch.owner.resize(table_size, 0);
    }
    if scratch.spread.len() < table_size + 8 {
        scratch.spread.resize(table_size + 8, 0);
    }
    build_table_body(
        probs,
        acc_log,
        &mut scratch.owner,
        &mut scratch.spread,
        &mut tab,
    )
}

fn build_table_body(
    probs: &[i32],
    acc_log: u8,
    owner: &mut [u8],
    spread: &mut [u8],
    tab: &mut Vec<u64>,
) -> FSETable {
    debug_assert!(
        (0..=12).contains(&acc_log),
        "acc_log {acc_log} exceeds the supported range"
    );
    debug_assert!(owner.len() >= 1usize << acc_log);
    debug_assert!(spread.len() >= (1usize << acc_log) + 8);
    let table_size = 1usize << acc_log;
    let owner = &mut owner[..table_size];
    let spread = &mut spread[..table_size + 8];
    let mut probs_full = [0i32; 256];
    probs_full[..probs.len()].copy_from_slice(probs);

    // Row starts + low-prob placement in one pass (libzstd's symbol start
    // positions): `start[s]` doubles as the state-table walk's cursor below,
    // then transforms into the biased encode start state. -1 weights take
    // state indices from the top downward.
    let mut start = [0u16; 256];
    let mut total = 0u32;
    let mut high_threshold = table_size as i32 - 1;
    for (s, &prob) in probs.iter().enumerate() {
        if prob != 0 {
            start[s] = total as u16;
        }
        if prob == -1 {
            owner[high_threshold as usize] = s as u8;
            high_threshold -= 1;
            total += 1;
        } else {
            total += prob as u32;
        }
    }
    debug_assert_eq!(total, table_size as u32);

    // Spread the symbols over the table. The fast path (no -1 weights — the
    // small-payload norm for sequence and weight tables) lays the symbol runs
    // down contiguously — eight bytes per store — then scatters with a
    // fixed-step, two-way unrolled loop of constant trip count, exactly
    // libzstd's `FSE_buildCTable_wksp` fast branch. `step` is odd for every
    // table_log >= 4, so the walk covers each position exactly once.
    if high_threshold == table_size as i32 - 1 && table_size > 1 {
        let step = (table_size >> 1) + (table_size >> 3) + 3;
        let base = spread.as_mut_ptr();
        let mut sv = 0u64;
        let mut pos = 0usize;
        for &prob in probs.iter() {
            if prob > 0 {
                // SAFETY: the runs tile [0, table_size) and each run's u64
                // writes cover ceil(n / 8) * 8 bytes, so the farthest write
                // stays under table_size + 8 (the slice's length).
                unsafe {
                    base.add(pos).cast::<u64>().write_unaligned(sv);
                    let mut i = 8;
                    while i < prob as usize {
                        base.add(pos + i).cast::<u64>().write_unaligned(sv);
                        i += 8;
                    }
                }
                pos += prob as usize;
            }
            sv = sv.wrapping_add(0x0101_0101_0101_0101);
        }
        let mask = table_size - 1;
        let mut position = 0usize;
        let mut k = 0usize;
        while k < table_size {
            owner[position] = spread[k];
            owner[(position + step) & mask] = spread[k + 1];
            position = (position + 2 * step) & mask;
            k += 2;
        }
        debug_assert_eq!(position, 0, "the spread walk must return to 0");
    } else {
        // Classic per-occurrence walk: positive symbols spread through the
        // remaining space, skipping the low-probability area at the top.
        let mut idx = 0usize;
        for (symbol, prob) in probs.iter().copied().enumerate() {
            if prob <= 0 {
                continue;
            }
            for _ in 0..prob {
                owner[idx] = symbol as u8;
                idx = next_position(idx, table_size);
                while idx > high_threshold as usize {
                    idx = next_position(idx, table_size);
                }
            }
        }
    }

    // State table: positions ascending, symbol-major rows — `next` for the
    // k-th state of a symbol is its k-th smallest spread position. The walk
    // consumes `start` as its per-symbol cursor (libzstd mutates `cumul`
    // in place the same way).
    // SAFETY: the state table is `table_size` u16 entries at ST_U16_OFF in
    // the tab buffer (whose u64 length covers TT_LEN + ceil(table_size/4)).
    let st = unsafe {
        core::slice::from_raw_parts_mut(tab.as_mut_ptr().cast::<u16>().add(ST_U16_OFF), table_size)
    };
    for (u, &symbol) in owner.iter().enumerate() {
        let s = symbol as usize;
        st[start[s] as usize] = (table_size + u) as u16;
        start[s] += 1;
    }

    // Transform entries and start states (port of libzstd's "Build Symbol
    // Transformation Table"): deltaNbBits makes
    // `(state + deltaNbBits) >> 16` the emitted bit count, deltaFindState
    // lands the state-table index inside the symbol's row. The running
    // total re-derives each row's start (the walk above advanced `start`
    // past it) and transforms it into the biased encode start state — the
    // symbol's lowest spread position, the first entry of its state-table
    // row. The equivalence with the previous materialized run walk (double
    // states first in index order, baselines multiples of their run width)
    // is pinned by `soa_matches_reference`.
    let mut total = 0u32;
    for (s, &prob) in probs.iter().enumerate() {
        let (dnb, dfs) = match prob {
            // Dead-symbol fill (libzstd's), never encoded.
            0 => (
                ((u32::from(acc_log) + 1) << 16).wrapping_sub(table_size as u32),
                0,
            ),
            // Single-state symbols (-1 or weight 1): always acc_log bits.
            1 | -1 => (
                (u32::from(acc_log) << 16).wrapping_sub(table_size as u32),
                total.wrapping_sub(1),
            ),
            p => {
                let prob_log = if (p as u32).is_power_of_two() {
                    (p as u32).ilog2()
                } else {
                    (p as u32).ilog2() + 1
                };
                let nb = u32::from(acc_log) - prob_log;
                (
                    ((nb + 1) << 16).wrapping_sub((p as u32) << (nb + 1)),
                    total.wrapping_sub(p as u32),
                )
            },
        };
        tab[s] = u64::from(dnb) | u64::from(dfs) << 32;
        if prob != 0 {
            start[s] = st[total as usize];
        }
        total += if prob == -1 {
            1
        } else {
            prob as u32
        };
    }

    FSETable {
        probs: probs_full,
        start,
        table_size,
        tab: core::mem::take(tab),
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
            // Lowest-index state of the symbol: the encode start (libzstd's
            // FSE_initCState2), whose decode entry always consumes >= 1 bit.
            starts[symbol] = st.iter().map(|s| s.index).min().unwrap();
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
        for case in 0..600 {
            let acc_log = 5 + (rand() % 4) as u8;
            let table_size = 1usize << acc_log;
            let nsym = 1 + (rand() % 20) as usize;
            let mut probs = vec![0i32; nsym];
            // Every third case bans -1 weights, exercising the contiguous
            // run-buffer spread fast path against the reference's classic
            // per-occurrence walk.
            let allow_negative = case % 3 != 0;
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
                if allow_negative && r % 7 == 0 {
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
                let expect = if probs[s] != 0 {
                    (ref_ts + start) as u16
                } else {
                    0
                };
                assert_eq!(
                    table.start[s], expect,
                    "case {case} start {s} probs {probs:?} acc {acc_log}"
                );
            }
            // The arithmetic step must reproduce the reference run walk
            // bit-for-bit: every state of every live symbol steps to the
            // reference's next index, emits the run offset, and spends the
            // run's bit count. The reference runs tile [0, table_size), so
            // the loop covers each (symbol, state) pair exactly once.
            for (symbol, nb, baseline, index) in &ref_flat {
                for i in *baseline..*baseline + (1usize << *nb) {
                    let (step_nb, emit, next) = table.step(*symbol, (ref_ts + i) as u32);
                    assert_eq!(
                        step_nb as usize, *nb as usize,
                        "case {case} sym {symbol} state {i} nb probs {probs:?}"
                    );
                    assert_eq!(
                        emit as usize,
                        i - *baseline,
                        "case {case} sym {symbol} state {i} emit"
                    );
                    assert_eq!(
                        next as usize,
                        ref_ts + *index,
                        "case {case} sym {symbol} state {i} next"
                    );
                }
            }
        }
    }
}
