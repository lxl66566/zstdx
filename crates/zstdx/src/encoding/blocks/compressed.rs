use alloc::vec::Vec;

use crate::{
    bit_io::BitWriter,
    encoding::Matcher,
    fse::fse_encoder::{approx_log2, build_normalized_table, rle_table, FSETable},
    huff0::huff0_encoder,
};

/// What one sequence table's block mode means for the caller's remembered
/// copy: a freshly written table to adopt, no change (the decoder's table
/// state is untouched — zero-sequence and raw-fallback blocks), or
/// invalidation (the block overwrote the decoder's table with a predefined
/// or RLE one, so an older custom table can no longer be repeated).
#[derive(Default)]
pub(crate) enum PrevTable {
    New(FSETable),
    #[default]
    Keep,
    Clear,
}

/// Entropy-table outcomes of encoding one compressed block.
///
/// `compress_block` never touches caller-owned state, so a raw-block
/// fallback just drops these instead of restoring snapshots.
#[derive(Default)]
pub(crate) struct BlockTables {
    pub(crate) huff: Option<huff0_encoder::HuffmanTable>,
    pub(crate) ll: PrevTable,
    pub(crate) ml: PrevTable,
    pub(crate) of: PrevTable,
}

/// Outcome of encoding one compressed block. `Encoded` carries the entropy
/// tables the caller should remember for later blocks; `Raw` means nothing
/// was written and the caller should emit the raw block it would have
/// fallen back to anyway (the encoder proved the block cannot shrink).
pub(crate) enum BlockOutcome {
    Encoded(BlockTables),
    Raw,
}

/// Reusable per-block scratch buffers, pooled in the compressor state: the
/// literals and the matcher-emitted packed code streams (codes, merged
/// add-bits payloads, payload widths) used to be fresh Vecs, paying an
/// allocate-and-double chain per block.
#[derive(Default)]
pub(crate) struct BlockScratch {
    literals: Vec<u8>,
    seqs: Vec<crate::encoding::SeqWord>,
    /// Sticky heuristic: the previous block's literals cleared the exact
    /// entropy bound, so the strided gate below is skipped until a block
    /// proves otherwise. Pure cost hint; every outcome stays reachable.
    literals_gate_hold: bool,
}

/// A block of [`crate::common::BlockType::Compressed`]
pub(crate) fn compress_block<M: Matcher>(
    matcher: &mut M,
    last_huff_table: Option<&huff0_encoder::HuffmanTable>,
    default_tables: (&FSETable, &FSETable, &FSETable),
    previous_tables: (&Option<FSETable>, &Option<FSETable>, &Option<FSETable>),
    output: &mut Vec<u8>,
    scratch: &mut BlockScratch,
) -> BlockOutcome {
    let mut tables = BlockTables::default();
    // Typical block shape: a few KB of literals and a few thousand sequences;
    // the pooled buffers keep that capacity after the first blocks.
    let BlockScratch {
        literals: literals_vec,
        seqs,
        literals_gate_hold,
    } = scratch;
    literals_vec.clear();
    seqs.clear();
    matcher.start_matching_codes(literals_vec, seqs);

    // A zero-sequence block stages no literals (the matcher skips the
    // whole-block copy); its literals are the block itself, read straight
    // from the window. Same bytes, so every entropy decision below — and
    // thus the output — matches the staged path.
    let zero_seq = seqs.is_empty();
    let literals: &[u8] = if zero_seq {
        matcher.get_last_space()
    } else {
        &literals_vec[..]
    };

    // Early raw exit: a zero-sequence block whose literals fail the strided
    // entropy sample encodes to raw literals plus an empty sequence section,
    // which is larger than the block itself — the caller's raw fallback is
    // the certain outcome. Signalling it here skips the encode-and-discard
    // writes (the block bytes are then copied and hashed exactly once, in
    // the raw writer).
    if zero_seq && literals.len() > 1024 {
        if sampled_gate_rejects(literals, literals_gate_hold) {
            return BlockOutcome::Raw;
        }
    }

    // literals section

    // A zero-sequence block's literals are the block itself, whose uniform
    // check already ran before the RLE block decision — never uniform here.
    let mut writer = BitWriter::from(output);
    if !literals.is_empty() && !zero_seq && crate::encoding::util::is_uniform(literals) {
        rle_literals(literals, &mut writer);
    } else if literals.len() > 1024 {
        if let Some(table) =
            compress_literals(literals, last_huff_table, &mut writer, literals_gate_hold)
        {
            tables.huff = Some(table);
        }
    } else {
        raw_literals(literals, &mut writer);
    }

    // sequences section

    if seqs.is_empty() {
        writer.write_bits(0u8, 8);
    } else {
        encode_seqnum(seqs.len(), &mut writer);

        // Choose the tables with libzstd's fast-strategy heuristics: RLE
        // when one code covers everything, predefined when the block is too
        // small (or too skewed) to pay for a table description, otherwise a
        // normalized custom table. The matcher already emitted the packed
        // codes and pre-merged add-bit payloads, so the per-code metadata is
        // computed exactly once per sequence and every consumer (table
        // selection, table description, bitstream encoder) reads the streams.
        let (ll_mode, ml_mode, of_mode) = choose_tables_fast(seqs, default_tables, previous_tables);

        writer.write_bits(encode_fse_table_modes(&ll_mode, &ml_mode, &of_mode), 8);

        encode_table(&ll_mode, &mut writer);
        encode_table(&of_mode, &mut writer);
        encode_table(&ml_mode, &mut writer);

        encode_sequences(
            seqs.len(),
            seqs,
            &mut writer,
            ll_mode.as_ref(),
            ml_mode.as_ref(),
            of_mode.as_ref(),
        );

        for (slot, mode) in [
            (&mut tables.ll, ll_mode),
            (&mut tables.ml, ml_mode),
            (&mut tables.of, of_mode),
        ] {
            *slot = match mode {
                FseTableMode::Encoded(table) => PrevTable::New(table),
                FseTableMode::Repeat(_) => PrevTable::Keep,
                // The decoder's table for this stream is now the predefined
                // or RLE one; an older custom table can never be repeated.
                FseTableMode::Predefined(_) | FseTableMode::Rle { .. } => PrevTable::Clear,
            };
        }
    }
    writer.flush();
    BlockOutcome::Encoded(tables)
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum FseTableMode<'a> {
    Predefined(&'a FSETable),
    Encoded(FSETable),
    /// Single-code RLE mode: `code` is the wire byte, `table` the degenerate
    /// one-state table the encoder runs on.
    Rle {
        code: u8,
        table: FSETable,
    },
    /// Repeat mode: the previous block's table verbatim, no description.
    Repeat(&'a FSETable),
}

impl FseTableMode<'_> {
    pub fn as_ref(&self) -> &FSETable {
        match self {
            Self::Predefined(t) => t,
            Self::Encoded(t) => t,
            Self::Rle { table, .. } => table,
            Self::Repeat(t) => t,
        }
    }
}

/// One pass over the packed codes fills all three histograms; each table's
/// mode is then decided from its own counts (three selection passes used to
/// scan the code stream separately).
fn choose_tables_fast<'a>(
    seqs: &[crate::encoding::SeqWord],
    default_tables: (&'a FSETable, &'a FSETable, &'a FSETable),
    previous_tables: (
        &'a Option<FSETable>,
        &'a Option<FSETable>,
        &'a Option<FSETable>,
    ),
) -> (FseTableMode<'a>, FseTableMode<'a>, FseTableMode<'a>) {
    let nb_seq = seqs.len();
    let mut ll_counts = [0u32; 256];
    let mut ml_counts = [0u32; 256];
    let mut of_counts = [0u32; 256];
    for &word in seqs {
        let packed = word.codes;
        ll_counts[(packed & 0xFF) as usize] += 1;
        ml_counts[((packed >> 8) & 0xFF) as usize] += 1;
        of_counts[(packed >> 16) as usize] += 1;
    }
    let first = seqs[0].codes;
    let last = seqs[nb_seq - 1].codes;
    (
        select_from_counts(
            &mut ll_counts,
            nb_seq,
            first as u8,
            last as u8,
            default_tables.0,
            previous_tables.0.as_ref(),
            6,
            9,
        ),
        select_from_counts(
            &mut ml_counts,
            nb_seq,
            (first >> 8) as u8,
            (last >> 8) as u8,
            default_tables.1,
            previous_tables.1.as_ref(),
            6,
            9,
        ),
        select_from_counts(
            &mut of_counts,
            nb_seq,
            (first >> 16) as u8,
            (last >> 16) as u8,
            default_tables.2,
            previous_tables.2.as_ref(),
            5,
            8,
        ),
    )
}

/// Per-table mode selection from a filled histogram: RLE when a single code
/// covers all sequences, predefined below the dynamic-table break-even (or
/// when the predefined table cannot cover the codes), repeat when the
/// previous block's table codes this histogram no worse than a fresh table's
/// description plus entropy bound, custom normalized table otherwise. Port
/// of libzstd's ZSTD_selectEncodingType: the repeat comparison follows the
/// cost-based path the lazy+ strategies use (the fast-strategy shortcut only
/// engages with dictionary-provided tables, which this encoder never has).
fn select_from_counts<'a>(
    counts: &mut [u32; 256],
    nb_seq: usize,
    first_code: u8,
    last_code: u8,
    default_table: &'a FSETable,
    previous: Option<&'a FSETable>,
    default_norm_log: u32,
    max_log: u8,
) -> FseTableMode<'a> {
    let mut max_symbol = 0usize;
    let mut most_frequent = 0u32;
    for (i, &c) in counts.iter().enumerate() {
        if c > 0 {
            max_symbol = i;
            if c > most_frequent {
                most_frequent = c;
            }
        }
    }
    if most_frequent as usize == nb_seq {
        // With two or fewer sequences the predefined table's description is
        // cheaper than even the one RLE byte.
        if nb_seq <= 2 {
            return FseTableMode::Predefined(default_table);
        }
        return FseTableMode::Rle {
            code: first_code,
            table: rle_table(first_code),
        };
    }
    // The predefined table must cover every code that occurs.
    let default_covers = max_symbol < 31 || default_norm_log == 6;
    if default_covers {
        let dynamic_min = ((1u32 << default_norm_log) * 9) >> 3;
        if (nb_seq as u32) < dynamic_min
            || most_frequent < (nb_seq as u32) >> (default_norm_log - 1)
        {
            return FseTableMode::Predefined(default_table);
        }
    }
    if let Some(prev) = previous {
        if let Some(repeat_bits) = repeat_bit_cost(prev, counts, max_symbol) {
            // The bound understates a fresh table's bitstream and the
            // description term is under two percent of the comparison, so
            // the estimate leans toward rebuilding: ratio-safe.
            let fresh_bits =
                entropy_bound_bits(counts, nb_seq, max_symbol) + description_bits(max_symbol);
            if repeat_bits <= fresh_bits {
                return FseTableMode::Repeat(prev);
            }
        }
    }
    match build_normalized_table(counts, nb_seq, max_symbol, max_log, last_code) {
        Some(table) => FseTableMode::Encoded(table),
        // Normalization corner case: fall back to the predefined table.
        None => FseTableMode::Predefined(default_table),
    }
}

/// Bits the previous table needs for this histogram: one occurrence costs
/// log2(table_size / prob). `None` when a live symbol has no state in the
/// table, which rules the table out entirely.
fn repeat_bit_cost(prev: &FSETable, counts: &[u32; 256], max_symbol: usize) -> Option<f64> {
    let mut bits = 0.0f64;
    for (s, &c) in counts.iter().enumerate().take(max_symbol + 1) {
        if c > 0 {
            bits += c as f64 * prev.symbol_bit_cost(s as u8)?;
        }
    }
    Some(bits)
}

/// Shannon bound of the histogram in bits — the floor a freshly normalized
/// table approaches but never reaches.
fn entropy_bound_bits(counts: &[u32; 256], nb_seq: usize, max_symbol: usize) -> f64 {
    let mut bits = 0.0f64;
    for &c in &counts[..=max_symbol] {
        if c > 0 {
            bits -= c as f64 * entropy_log2(c as f64 / nb_seq as f64);
        }
    }
    bits
}

/// Fresh-table description size in bits. The NCount wire format spends
/// roughly six bits per live symbol plus a header and tail padding; the
/// estimate only shifts a comparison term that stays below two percent.
fn description_bits(max_symbol: usize) -> f64 {
    (max_symbol as f64 + 1.0) * 6.0 + 8.0
}

fn encode_table(mode: &FseTableMode<'_>, writer: &mut BitWriter<&mut Vec<u8>>) {
    match mode {
        FseTableMode::Predefined(_) | FseTableMode::Repeat(_) => {}
        FseTableMode::Rle { code, .. } => {
            // The RLE table description is a single byte: the code.
            writer.write_bits(*code as u64, 8);
        }
        FseTableMode::Encoded(table) => table.write_table(writer),
    }
}

fn encode_fse_table_modes(
    ll_mode: &FseTableMode<'_>,
    ml_mode: &FseTableMode<'_>,
    of_mode: &FseTableMode<'_>,
) -> u8 {
    fn mode_to_bits(mode: &FseTableMode<'_>) -> u8 {
        match mode {
            FseTableMode::Predefined(_) => 0,
            FseTableMode::Rle { .. } => 1,
            FseTableMode::Encoded(_) => 2,
            FseTableMode::Repeat(_) => 3,
        }
    }
    mode_to_bits(ll_mode) << 6 | mode_to_bits(of_mode) << 4 | mode_to_bits(ml_mode) << 2
}

fn encode_sequences(
    nb_seq: usize,
    seqs: &[crate::encoding::SeqWord],
    writer: &mut BitWriter<&mut Vec<u8>>,
    ll_table: &FSETable,
    ml_table: &FSETable,
    of_table: &FSETable,
) {
    // The codes and pre-merged add-bit payloads arrive precomputed; the
    // transitions still need each table's entry for the running states.
    // Rows are flat `code << shift | state` (table_size is a power of two),
    // replacing the runtime-stride multiply and bounds check of the indexed
    // accessor. Codes are u8 and states cycle below table_size by table
    // construction, so the row index cannot leave the flat array.
    let (ll_rows, ll_shift) = ll_table.transitions_flat();
    let (ml_rows, ml_shift) = ml_table.transitions_flat();
    let (of_rows, of_shift) = of_table.transitions_flat();

    let li = nb_seq - 1;
    let packed = seqs[li].codes;
    let ll_code = packed as u8;
    let ml_code = (packed >> 8) as u8;
    let of_code = (packed >> 16) as u8;
    let mut ll_state = ll_table.start_index(ll_code);
    let mut ml_state = ml_table.start_index(ml_code);
    let mut of_state = of_table.start_index(of_code);

    let ml_log = ml_table.table_size.ilog2() as usize;
    let of_log = of_table.table_size.ilog2() as usize;
    let ll_log = ll_table.table_size.ilog2() as usize;

    // The accumulator stays in locals for the whole loop; routing the two
    // per-sequence writes through the writer used to reload and store its
    // fields each time. Flushes leave fewer than eight bits pending, so
    // neither push (≤45 transition bits, ≤51 add bits) can overflow.
    let (mut acc, mut bits, mut pos) = writer.hot_state();
    {
        let out = writer.out();
        hot_push(
            out,
            &mut pos,
            &mut acc,
            &mut bits,
            seqs[li].add,
            seqs[li].add_nb as usize,
        );

        // encode backwards so the decoder reads the first sequence first
        if nb_seq > 1 {
            for i in (0..=nb_seq - 2).rev() {
                let packed = seqs[i].codes;
                let ll_code = packed as u8;
                let ml_code = (packed >> 8) as u8;
                let of_code = (packed >> 16) as u8;

                // SAFETY: see the row-index argument above.
                let e_of =
                    unsafe { *of_rows.get_unchecked(((of_code as usize) << of_shift) | of_state) };
                let e_ml =
                    unsafe { *ml_rows.get_unchecked(((ml_code as usize) << ml_shift) | ml_state) };
                let e_ll =
                    unsafe { *ll_rows.get_unchecked(((ll_code as usize) << ll_shift) | ll_state) };
                debug_assert!(of_state < of_table.table_size);
                debug_assert!(ml_state < ml_table.table_size);
                debug_assert!(ll_state < ll_table.table_size);

                // The three state-transition bit groups (max 12 bits each:
                // nb <= acc_log <= 12) fit a single u64 write; concatenating
                // them keeps the writer's hot path.
                let of_diff = (of_state - (e_of & 0xFFF) as usize) as u64;
                let ml_diff = (ml_state - (e_ml & 0xFFF) as usize) as u64;
                let ll_diff = (ll_state - (e_ll & 0xFFF) as usize) as u64;
                let of_nb = ((e_of >> 12) & 0xF) as usize;
                let ml_nb = ((e_ml >> 12) & 0xF) as usize;
                let ll_nb = ((e_ll >> 12) & 0xF) as usize;
                let trans = of_diff | (ml_diff << of_nb) | (ll_diff << (of_nb + ml_nb));
                of_state = (e_of >> 16) as usize;
                ml_state = (e_ml >> 16) as usize;
                ll_state = (e_ll >> 16) as usize;

                // Transition bits then add bits are adjacent in the stream;
                // one combined push keeps the writer's flush path once per
                // sequence whenever the payload fits the accumulator. The 56
                // cap keeps a post-flush (bits < 8) accumulator from dropping
                // payload; wider pairs fall back to two pushes of the same
                // bits.
                let add = seqs[i].add;
                let add_nb = seqs[i].add_nb as usize;
                let trans_nb = of_nb + ml_nb + ll_nb;
                if trans_nb + add_nb <= 56 {
                    hot_push(
                        out,
                        &mut pos,
                        &mut acc,
                        &mut bits,
                        trans | (add << trans_nb),
                        trans_nb + add_nb,
                    );
                } else {
                    hot_push(out, &mut pos, &mut acc, &mut bits, trans, trans_nb);
                    hot_push(out, &mut pos, &mut acc, &mut bits, add, add_nb);
                }
            }
        }
    }
    writer.set_hot_state(acc, bits, pos);

    writer.write_bits(ml_state as u64, ml_log);
    writer.write_bits(of_state as u64, of_log);
    writer.write_bits(ll_state as u64, ll_log);

    let bits_to_fill = writer.misaligned();
    if bits_to_fill == 0 {
        writer.write_bits(1u32, 8);
    } else {
        writer.write_bits(1u32, bits_to_fill);
    }
}

/// Append the low `nb` bits of `v` to a hot accumulator, flushing whole
/// bytes with one unaligned u64 store whenever the pending bits could
/// overflow the container. Produces the same bits as `write_bits`; callers
/// keep `bits` below 64 so the shift cannot drop payload.
#[inline(always)]
fn hot_push(
    out: &mut Vec<u8>,
    pos: &mut usize,
    acc: &mut u64,
    bits: &mut usize,
    v: u64,
    nb: usize,
) {
    if *bits + nb >= 64 {
        let k = *bits / 8;
        if *pos + 16 > out.capacity() {
            out.reserve(*pos + 16 - out.len());
        }
        // SAFETY: the capacity check covers the store; bytes past the
        // semantic end are overwritten by later stores or cut by set_len.
        unsafe {
            out.as_mut_ptr()
                .add(*pos)
                .cast::<u64>()
                .write_unaligned(acc.to_le());
        }
        *pos += k;
        *acc >>= 8 * k;
        *bits -= 8 * k;
    }
    *acc |= v << *bits;
    *bits += nb;
}

fn encode_seqnum(seqnum: usize, writer: &mut BitWriter<impl AsMut<Vec<u8>>>) {
    const UPPER_LIMIT: usize = 0xFFFF + 0x7F00;
    match seqnum {
        1..=127 => writer.write_bits(seqnum as u32, 8),
        128..=0x7FFF => {
            let upper = ((seqnum >> 8) | 0x80) as u8;
            let lower = seqnum as u8;
            writer.write_bits(upper, 8);
            writer.write_bits(lower, 8);
        }
        0x8000..=UPPER_LIMIT => {
            let encode = seqnum - 0x7F00;
            let upper = (encode >> 8) as u8;
            let lower = encode as u8;
            writer.write_bits(255u8, 8);
            writer.write_bits(upper, 8);
            writer.write_bits(lower, 8);
        }
        _ => unreachable!(),
    }
}

fn raw_literals(literals: &[u8], writer: &mut BitWriter<&mut Vec<u8>>) {
    writer.write_bits(0u8, 2);
    writer.write_bits(0b11u8, 2);
    writer.write_bits(literals.len() as u32, 20);
    writer.append_bytes(literals);
}

/// Uniform literals encode as one header plus a single content byte
/// (Literals_Block_Type 1); the size formats mirror the raw literals ones
/// (the smallest form spends a single size-format bit so the 5-bit size
/// fills out the first header byte).
fn rle_literals(literals: &[u8], writer: &mut BitWriter<&mut Vec<u8>>) {
    writer.write_bits(1u8, 2);
    match literals.len() {
        0..=31 => {
            writer.write_bits(0u8, 1);
            writer.write_bits(literals.len() as u32, 5);
        }
        32..=4095 => {
            writer.write_bits(0b01u8, 2);
            writer.write_bits(literals.len() as u32, 12);
        }
        _ => {
            writer.write_bits(0b11u8, 2);
            writer.write_bits(literals.len() as u32, 20);
        }
    }
    writer.write_bits(literals[0], 8);
}

/// log2 for the entropy bound and repeat-table costs; shared approximation
/// (see [`approx_log2`]).
#[inline(always)]
fn entropy_log2(x: f64) -> f64 {
    approx_log2(x)
}

/// Exact literal histogram. Blocks whose alphabet stays within sixteen
/// symbols run an AVX-512 kernel (sixteen per-slot mask compares plus
/// popcounts per 64 bytes, coverage-checked through a 256-entry membership
/// LUT); anything wider falls back to the four-lane scalar pass keyed by
/// position mod 4, whose sub-histograms keep concurrent increments in
/// different cache lines. Returns the highest symbol with a nonzero count.
fn histogram_literals(literals: &[u8], counts: &mut [usize; 256]) -> usize {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if literals.len() >= 64
            && std::is_x86_feature_detected!("avx512bw")
            && std::is_x86_feature_detected!("avx512vbmi")
            && std::is_x86_feature_detected!("popcnt")
        {
            // SAFETY: the features were just detected; every access stays
            // inside `literals` (the loop guards i + 64 <= len).
            if let Some(max) = unsafe { histogram_small_alpha_avx512(literals, counts) } {
                return max;
            }
        }
    }
    let mut c0 = [0usize; 256];
    let mut c1 = [0usize; 256];
    let mut c2 = [0usize; 256];
    let mut c3 = [0usize; 256];
    let mut chunks = literals.chunks_exact(4);
    for chunk in &mut chunks {
        c0[chunk[0] as usize] += 1;
        c1[chunk[1] as usize] += 1;
        c2[chunk[2] as usize] += 1;
        c3[chunk[3] as usize] += 1;
    }
    for &b in chunks.remainder() {
        c0[b as usize] += 1;
    }
    for i in 0..256 {
        counts[i] = c0[i] + c1[i] + c2[i] + c3[i];
    }
    let mut max_symbol = 255;
    while counts[max_symbol] == 0 {
        max_symbol -= 1;
    }
    max_symbol
}

/// AVX-512 histogram for at-most-16-symbol alphabets. The slot set grows
/// from uncovered bytes (cold scalar absorb); a seventeenth distinct symbol
/// aborts with `None` and leaves `counts` untouched for the scalar fallback.
/// Published counts are exact, so the block encode is byte-identical to the
/// scalar path.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512bw,avx512vbmi,popcnt")]
unsafe fn histogram_small_alpha_avx512(
    literals: &[u8],
    counts: &mut [usize; 256],
) -> Option<usize> {
    use core::arch::x86_64::*;

    let mut slots = [0u8; 16];
    let mut nslots = 0usize;
    // 0 marks a byte already covered by a slot.
    let mut lut = [1u8; 256];
    let mut acc = [0u32; 16];

    let mask7f = _mm512_set1_epi8(0x7F);
    let load_lut = |lut: &[u8; 256]| {
        (
            _mm512_loadu_si512(lut.as_ptr().cast()),
            _mm512_loadu_si512(lut.as_ptr().add(64).cast()),
            _mm512_loadu_si512(lut.as_ptr().add(128).cast()),
            _mm512_loadu_si512(lut.as_ptr().add(192).cast()),
        )
    };
    let (mut lut01, mut lut01b, mut lut23, mut lut23b) = load_lut(&lut);

    let mut i = 0usize;
    while i + 64 <= literals.len() {
        // Steady state: sixteen established slots. Fixed-bound slot loop in
        // four-chunk batches amortizes the symbol broadcasts, and full
        // coverage reduces to the popcount sum: every byte matches at most
        // one slot (the slots are distinct), so a sum below 256 means a
        // seventeenth symbol — bail for the scalar fallback.
        if nslots == 16 && i + 256 <= literals.len() {
            while i + 256 <= literals.len() {
                // SAFETY: guarded by the loop condition.
                let a = _mm512_loadu_si512(literals.as_ptr().add(i).cast());
                let b = _mm512_loadu_si512(literals.as_ptr().add(i + 64).cast());
                let c = _mm512_loadu_si512(literals.as_ptr().add(i + 128).cast());
                let d = _mm512_loadu_si512(literals.as_ptr().add(i + 192).cast());
                let mut covered = 0u32;
                for s in 0..16 {
                    let sym = _mm512_set1_epi8(slots[s] as i8);
                    let n = _mm512_cmpeq_epi8_mask(a, sym).count_ones()
                        + _mm512_cmpeq_epi8_mask(b, sym).count_ones()
                        + _mm512_cmpeq_epi8_mask(c, sym).count_ones()
                        + _mm512_cmpeq_epi8_mask(d, sym).count_ones();
                    covered += n;
                    acc[s] += n;
                }
                if covered != 256 {
                    return None;
                }
                i += 256;
            }
            continue;
        }
        // SAFETY: the loop guard bounds this 64-byte load.
        let v = _mm512_loadu_si512(literals.as_ptr().add(i).cast());
        // 256-entry byte LUT: bits 0..6 select within a 128-byte permute
        // pair, bit 7 blends between the pairs (same pattern as the uniform4
        // pack kernel).
        let lo7 = _mm512_and_si512(v, mask7f);
        let lutv = _mm512_mask_blend_epi8(
            _mm512_movepi8_mask(v),
            _mm512_permutex2var_epi8(lut01, lo7, lut01b),
            _mm512_permutex2var_epi8(lut23, lo7, lut23b),
        );
        if _mm512_test_epi8_mask(lutv, lutv) == 0 {
            for s in 0..nslots {
                let m = _mm512_cmpeq_epi8_mask(v, _mm512_set1_epi8(slots[s] as i8));
                acc[s] += m.count_ones();
            }
        } else {
            // Absorb the chunk by hand, growing the slot set from its novel
            // bytes. A fresh byte past sixteen slots means the alphabet is
            // too wide for this kernel.
            for j in 0..64 {
                let b = literals[i + j];
                if lut[b as usize] != 0 {
                    if nslots == 16 {
                        return None;
                    }
                    lut[b as usize] = 0;
                    slots[nslots] = b;
                    nslots += 1;
                }
                let slot = slots[..nslots].iter().position(|&s| s == b).unwrap();
                acc[slot] += 1;
            }
            (lut01, lut01b, lut23, lut23b) = load_lut(&lut);
        }
        i += 64;
    }
    for &b in &literals[i..] {
        if lut[b as usize] != 0 {
            if nslots == 16 {
                return None;
            }
            lut[b as usize] = 0;
            slots[nslots] = b;
            nslots += 1;
        }
        let slot = slots[..nslots].iter().position(|&s| s == b).unwrap();
        acc[slot] += 1;
    }
    let mut max_symbol = 0usize;
    for s in 0..nslots {
        counts[slots[s] as usize] = acc[s] as usize;
        max_symbol = max_symbol.max(slots[s] as usize);
    }
    Some(max_symbol)
}

/// Strided entropy gate before the exact histogram: one sampled count per
/// `stride` bytes decides incompressibility at a fraction of the full pass.
/// Blocks below the stride cutoff go straight to the exact path (sampling
/// them would be the same pass). The distinct-symbol prescreen skips the
/// estimate for small alphabets (an alphabet wider than 208 symbols is
/// necessary to reach the reject floor, and the prescreen only ever defers
/// to the exact path); the estimate itself carries the Miller-Madow bias
/// correction plus a noise margin, so only literals within ~2% of raw size
/// can encode larger than before. Passing sets `gate_hold` so the next
/// block skips the sample; rejecting leaves it, exactly like the inline
/// path did.
fn sampled_gate_rejects(literals: &[u8], gate_hold: &mut bool) -> bool {
    if !*gate_hold {
        let total = literals.len();
        if total >= 8192 {
            let stride = total >> 10;
            let mut sample = [0u32; 256];
            let mut distinct = 0usize;
            let mut n = 0usize;
            let mut i = 0;
            while i < total {
                let c = &mut sample[literals[i] as usize];
                if *c == 0 {
                    distinct += 1;
                }
                *c += 1;
                n += 1;
                i += stride;
            }
            if distinct > 208 {
                let total_f = n as f64;
                let mut entropy_bits = 0.0f64;
                for &c in &sample {
                    if c > 0 {
                        entropy_bits -= c as f64 * entropy_log2(c as f64 / total_f);
                    }
                }
                let bits_per_byte = entropy_bits / total_f
                    + (distinct as f64 - 1.0) * 0.7213_4752_0559_1157 / total_f;
                if bits_per_byte + 256.0 / total as f64 + 0.08 + 0.12 >= 8.0 {
                    return true;
                }
            }
        }
    }
    // The exact bound below passed for this block's literals: keep skipping
    // the gate until a block fails it (or the frame resets the scratch).
    *gate_hold = true;
    false
}

fn compress_literals(
    literals: &[u8],
    last_table: Option<&huff0_encoder::HuffmanTable>,
    writer: &mut BitWriter<&mut Vec<u8>>,
    gate_hold: &mut bool,
) -> Option<huff0_encoder::HuffmanTable> {
    let reset_idx = writer.index();

    if sampled_gate_rejects(literals, gate_hold) {
        raw_literals(literals, writer);
        return None;
    }

    // One histogram feeds both the entropy-bound reject and the table build:
    // literals used to be scanned twice, once per consumer.
    let mut counts = [0usize; 256];
    let max_symbol = histogram_literals(literals, &mut counts);

    // Cheap reject for near-incompressible literals (libzstd's
    // suspectUncompressible idea): building the tree, describing it and
    // running the four streams costs most of the literals section, so when
    // even the entropy bound cannot beat the raw copy by a margin, emit raw
    // right away instead of encoding and throwing the result away.
    {
        let total = literals.len() as f64;
        let mut entropy_bits = 0.0f64;
        for &c in &counts[..=max_symbol] {
            if c > 0 {
                entropy_bits -= c as f64 * entropy_log2(c as f64 / total);
            }
        }
        if entropy_bits + 256.0 + total * 0.08 >= total * 8.0 {
            raw_literals(literals, writer);
            return None;
        }
    }

    let new_encoder_table = huff0_encoder::HuffmanTable::build_from_counts(&counts[..=max_symbol]);

    let (encoder_table, new_table) = if let Some(_table) = last_table {
        if let Some(diff) = _table.can_encode(&new_encoder_table) {
            // TODO this is a very simple heuristic, maybe we should try to do better
            if diff > 5 {
                (&new_encoder_table, true)
            } else {
                (_table, false)
            }
        } else {
            (&new_encoder_table, true)
        }
    } else {
        (&new_encoder_table, true)
    };

    if new_table {
        writer.write_bits(2u8, 2); // compressed literals type
    } else {
        writer.write_bits(3u8, 2); // treeless compressed literals type
    }

    let (size_format, size_bits) = match literals.len() {
        0..6 => (0b00u8, 10),
        6..1024 => (0b01, 10),
        1024..16384 => (0b10, 14),
        16384..262144 => (0b11, 18),
        _ => unimplemented!("too many literals"),
    };

    writer.write_bits(size_format, 2);
    writer.write_bits(literals.len() as u32, size_bits);
    let size_index = writer.index();
    writer.write_bits(0u32, size_bits);
    let index_before = writer.index();
    let mut encoder = huff0_encoder::HuffmanEncoder::new(encoder_table, writer);
    if size_format == 0 {
        encoder.encode(literals, new_table)
    } else {
        encoder.encode4x(literals, new_table)
    };
    let encoded_len = (writer.index() - index_before) / 8;
    writer.change_bits(size_index, encoded_len as u64, size_bits);
    let total_len = (writer.index() - reset_idx) / 8;

    // If encoded len is bigger than the raw literals we are better off just writing the raw literals here
    if total_len >= literals.len() {
        writer.reset_to(reset_idx);
        raw_literals(literals, writer);
        None
    } else if new_table {
        Some(new_encoder_table)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fse::fse_encoder::default_ll_table;

    fn counts_from(symbols: &[u8]) -> [u32; 256] {
        let mut counts = [0u32; 256];
        for &s in symbols {
            counts[s as usize] += 1;
        }
        counts
    }

    #[test]
    fn repeat_selection_by_cost_and_coverage() {
        let default = default_ll_table();
        // Two near-identical distributions: reusing the table must beat
        // rebuilding it (description cost saved).
        let mut symbols = alloc::vec![];
        let mut state = 0x1234_5678u64;
        for _ in 0..4000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            symbols.push(2 + (state >> 33) as u8 % 7);
        }
        let nb_seq = symbols.len();
        let first = symbols[0];
        let last = symbols[nb_seq - 1];
        let prev_counts = counts_from(&symbols);
        let prev = build_normalized_table(&mut prev_counts.clone(), nb_seq, 8, 9, last).unwrap();

        let mut counts = counts_from(&symbols);
        let mode = select_from_counts(
            &mut counts,
            nb_seq,
            first,
            last,
            &default,
            Some(&prev),
            6,
            9,
        );
        assert!(
            matches!(mode, FseTableMode::Repeat(_)),
            "stable distribution must repeat"
        );

        // A new symbol the previous table has no state for disqualifies
        // repeat entirely, whatever the cost.
        let mut shifted = alloc::vec![40u8; 2];
        shifted.extend_from_slice(&symbols[..nb_seq - 2]);
        let mut counts = counts_from(&shifted);
        let mode = select_from_counts(
            &mut counts,
            nb_seq,
            first,
            last,
            &default,
            Some(&prev),
            6,
            9,
        );
        assert!(
            matches!(mode, FseTableMode::Encoded(_)),
            "uncovered symbol must rebuild"
        );

        // A wildly different distribution makes the old table too costly.
        let mut other = alloc::vec![];
        for i in 0..nb_seq {
            other.push(30 + (i % 5) as u8);
        }
        let mut counts = counts_from(&other);
        let mode = select_from_counts(&mut counts, nb_seq, first, 30, &default, Some(&prev), 6, 9);
        assert!(
            matches!(mode, FseTableMode::Encoded(_)),
            "drifted distribution must rebuild"
        );
    }
}
