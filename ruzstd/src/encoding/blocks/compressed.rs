use alloc::vec::Vec;

use crate::{
    bit_io::BitWriter,
    encoding::{EncodedSequence, Matcher},
    fse::fse_encoder::{build_normalized_table, rle_table, FSETable},
    huff0::huff0_encoder,
};

/// Entropy-table outcomes of encoding one compressed block. `Some(table)`
/// means the block was encoded with a freshly built table the caller should
/// remember for later blocks; `None` keeps whatever the caller had.
///
/// `compress_block` never touches caller-owned state, so a raw-block
/// fallback just drops these instead of restoring snapshots.
#[derive(Default)]
pub(crate) struct BlockTables {
    pub(crate) huff: Option<huff0_encoder::HuffmanTable>,
    pub(crate) ll: Option<FSETable>,
    pub(crate) ml: Option<FSETable>,
    pub(crate) of: Option<FSETable>,
}

/// A block of [`crate::common::BlockType::Compressed`]
pub(crate) fn compress_block<M: Matcher>(
    matcher: &mut M,
    last_huff_table: Option<&huff0_encoder::HuffmanTable>,
    default_tables: (&FSETable, &FSETable, &FSETable),
    output: &mut Vec<u8>,
) -> BlockTables {
    let mut tables = BlockTables::default();
    // Typical block shape: a few KB of literals and a few thousand sequences;
    // starting there skips the early reallocation-doubling chain.
    let mut literals_vec = Vec::with_capacity(4096);
    let mut sequences = Vec::with_capacity(2048);
    matcher.start_matching_into(&mut literals_vec, &mut sequences);

    // literals section

    let mut writer = BitWriter::from(output);
    if !literals_vec.is_empty() && crate::encoding::util::is_uniform(&literals_vec) {
        rle_literals(&literals_vec, &mut writer);
    } else if literals_vec.len() > 1024 {
        if let Some(table) = compress_literals(&literals_vec, last_huff_table, &mut writer) {
            tables.huff = Some(table);
        }
    } else {
        raw_literals(&literals_vec, &mut writer);
    }

    // sequences section

    if sequences.is_empty() {
        writer.write_bits(0u8, 8);
    } else {
        encode_seqnum(sequences.len(), &mut writer);

        // Choose the tables with libzstd's fast-strategy heuristics: RLE
        // when one code covers everything, predefined when the block is too
        // small (or too skewed) to pay for a table description, otherwise a
        // normalized custom table.
        // One pass over the sequences produces the packed codes and the
        // pre-merged add-bit payloads; table selection, table description and
        // the bitstream encoder all consume them, so the per-code metadata is
        // looked up exactly once per sequence.
        let mut packed_codes = Vec::with_capacity(sequences.len());
        let mut add_bits = Vec::with_capacity(sequences.len());
        let mut add_nbs = Vec::with_capacity(sequences.len());
        for seq in &sequences {
            let (ll_code, ll_add, ll_nb) = encode_literal_length(seq.ll);
            let (ml_code, ml_add, ml_nb) = encode_match_len(seq.ml);
            let (of_code, of_add, of_nb) = encode_offset(seq.of);
            packed_codes.push(ll_code as u32 | (ml_code as u32) << 8 | (of_code as u32) << 16);
            add_bits.push(
                ll_add as u64
                    | ((ml_add as u64) << ll_nb)
                    | ((of_add as u64) << (ll_nb + ml_nb)),
            );
            add_nbs.push((ll_nb + ml_nb + of_nb) as u8);
        }
        let ll_mode = choose_table_fast(&packed_codes, 0, default_tables.0, 6, 9);
        let ml_mode = choose_table_fast(&packed_codes, 8, default_tables.1, 6, 9);
        let of_mode = choose_table_fast(&packed_codes, 16, default_tables.2, 5, 8);

        writer.write_bits(encode_fse_table_modes(&ll_mode, &ml_mode, &of_mode), 8);

        encode_table(&ll_mode, &mut writer);
        encode_table(&of_mode, &mut writer);
        encode_table(&ml_mode, &mut writer);

        encode_sequences(
            sequences.len(),
            &packed_codes,
            &add_bits,
            &add_nbs,
            &mut writer,
            ll_mode.as_ref(),
            ml_mode.as_ref(),
            of_mode.as_ref(),
        );

        if let FseTableMode::Encoded(table) = ll_mode {
            tables.ll = Some(table)
        }
        if let FseTableMode::Encoded(table) = ml_mode {
            tables.ml = Some(table)
        }
        if let FseTableMode::Encoded(table) = of_mode {
            tables.of = Some(table)
        }
    }
    writer.flush();
    tables
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum FseTableMode<'a> {
    Predefined(&'a FSETable),
    Encoded(FSETable),
    /// Single-code RLE mode: `code` is the wire byte, `table` the degenerate
    /// one-state table the encoder runs on.
    Rle { code: u8, table: FSETable },
}

impl FseTableMode<'_> {
    pub fn as_ref(&self) -> &FSETable {
        match self {
            Self::Predefined(t) => t,
            Self::Encoded(t) => t,
            Self::Rle { table, .. } => table,
        }
    }
}

/// Port of libzstd's ZSTD_selectEncodingType for the fast strategy (no
/// dictionary, so repeat mode never applies): RLE when a single code covers
/// all sequences, predefined below the dynamic-table break-even, custom
/// normalized table otherwise. `shift` selects which byte of the packed code
/// stream belongs to this table.
fn choose_table_fast<'a>(
    packed_codes: &[u32],
    shift: u32,
    default_table: &'a FSETable,
    default_norm_log: u32,
    max_log: u8,
) -> FseTableMode<'a> {
    let nb_seq = packed_codes.len();
    let mut counts = [0u32; 256];
    let mut max_symbol = 0usize;
    for &packed in packed_codes {
        let c = (packed >> shift) as u8;
        counts[c as usize] += 1;
        if c as usize > max_symbol {
            max_symbol = c as usize;
        }
    }
    let mut most_frequent = 0u32;
    for &c in &counts[..=max_symbol] {
        most_frequent = most_frequent.max(c);
    }
    if most_frequent as usize == nb_seq {
        // With two or fewer sequences the predefined table's description is
        // cheaper than even the one RLE byte.
        if nb_seq <= 2 {
            return FseTableMode::Predefined(default_table);
        }
        let code = packed_codes[0] >> shift;
        return FseTableMode::Rle {
            code: code as u8,
            table: rle_table(code as u8),
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
    match build_normalized_table(
        &mut counts,
        nb_seq,
        max_symbol,
        max_log,
        (packed_codes[nb_seq - 1] >> shift) as u8,
    ) {
        Some(table) => FseTableMode::Encoded(table),
        // Normalization corner case: fall back to the predefined table.
        None => FseTableMode::Predefined(default_table),
    }
}

fn encode_table(mode: &FseTableMode<'_>, writer: &mut BitWriter<&mut Vec<u8>>) {
    match mode {
        FseTableMode::Predefined(_) => {}
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
        }
    }
    mode_to_bits(ll_mode) << 6 | mode_to_bits(of_mode) << 4 | mode_to_bits(ml_mode) << 2
}

fn encode_sequences(
    nb_seq: usize,
    packed_codes: &[u32],
    add_bits: &[u64],
    add_nbs: &[u8],
    writer: &mut BitWriter<&mut Vec<u8>>,
    ll_table: &FSETable,
    ml_table: &FSETable,
    of_table: &FSETable,
) {
    // The codes and pre-merged add-bit payloads arrive precomputed; the
    // transitions still need each table's entry for the running states.
    let li = nb_seq - 1;
    let packed = packed_codes[li];
    let ll_code = packed as u8;
    let ml_code = (packed >> 8) as u8;
    let of_code = (packed >> 16) as u8;
    let mut ll_state = ll_table.start_index(ll_code);
    let mut ml_state = ml_table.start_index(ml_code);
    let mut of_state = of_table.start_index(of_code);

    writer.write_bits(add_bits[li], add_nbs[li] as usize);

    let ll_size = ll_table.table_size;
    let ml_size = ml_table.table_size;
    let of_size = of_table.table_size;

    // encode backwards so the decoder reads the first sequence first
    if nb_seq > 1 {
        for i in (0..=nb_seq - 2).rev() {
            let packed = packed_codes[i];
            let ll_code = packed as u8;
            let ml_code = (packed >> 8) as u8;
            let of_code = (packed >> 16) as u8;

            // The three state-transition bit groups (max 15 bits each) fit a
            // single u64 write; concatenating them keeps the writer's hot
            // path.
            let e_of = of_table.transition(of_code, of_state);
            let e_ml = ml_table.transition(ml_code, ml_state);
            let e_ll = ll_table.transition(ll_code, ll_state);
            let of_diff = (of_state - (e_of & 0x1FF) as usize) as u64;
            let ml_diff = (ml_state - (e_ml & 0x1FF) as usize) as u64;
            let ll_diff = (ll_state - (e_ll & 0x1FF) as usize) as u64;
            let of_nb = ((e_of >> 9) & 0xF) as usize;
            let ml_nb = ((e_ml >> 9) & 0xF) as usize;
            let ll_nb = ((e_ll >> 9) & 0xF) as usize;
            let trans = of_diff | (ml_diff << of_nb) | (ll_diff << (of_nb + ml_nb));
            writer.write_bits(trans, of_nb + ml_nb + ll_nb);
            of_state = (e_of >> 13) as usize;
            ml_state = (e_ml >> 13) as usize;
            ll_state = (e_ll >> 13) as usize;

            writer.write_bits(add_bits[i], add_nbs[i] as usize);
        }
    }
    writer.write_bits(ml_state as u64, ml_size.ilog2() as usize);
    writer.write_bits(of_state as u64, of_size.ilog2() as usize);
    writer.write_bits(ll_state as u64, ll_size.ilog2() as usize);

    let bits_to_fill = writer.misaligned();
    if bits_to_fill == 0 {
        writer.write_bits(1u32, 8);
    } else {
        writer.write_bits(1u32, bits_to_fill);
    }
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

/// Per-code metadata for literal lengths: (base value, extra bit count).
const fn ll_meta() -> [(u32, u8); 36] {
    let mut t = [(0u32, 0u8); 36];
    let mut code = 0usize;
    while code <= 15 {
        t[code] = (code as u32, 0);
        code += 1;
    }
    let rest = [
        (16u32, 1u8), (18, 1), (20, 1), (22, 1), (24, 2), (28, 2), (32, 3), (40, 3), (48, 4),
        (64, 6), (128, 7), (256, 8), (512, 9), (1024, 10), (2048, 11), (4096, 12), (8192, 13),
        (16384, 14), (32768, 15), (65536, 16),
    ];
    let mut i = 0;
    while i < rest.len() {
        t[16 + i] = rest[i];
        i += 1;
    }
    t
}
const LL_META: [(u32, u8); 36] = ll_meta();

/// Literal-length code for the dense low range; mirrors the ladder below.
const fn ll_code_lut() -> [u8; 64] {
    let mut t = [0u8; 64];
    let mut len = 0usize;
    while len < 64 {
        let mut code = 35;
        while code > 0 {
            if LL_META[code].0 as usize <= len {
                break;
            }
            code -= 1;
        }
        t[len] = code as u8;
        len += 1;
    }
    t
}
const LL_CODE_LUT: [u8; 64] = ll_code_lut();

#[inline]
fn encode_literal_length(len: u32) -> (u8, u32, usize) {
    if len < 64 {
        let code = LL_CODE_LUT[len as usize] as usize;
        let (base, bits) = LL_META[code];
        return (code as u8, len - base, bits as usize);
    }
    match len {
        64..=127 => (25, len - 64, 6),
        128..=255 => (26, len - 128, 7),
        256..=511 => (27, len - 256, 8),
        512..=1023 => (28, len - 512, 9),
        1024..=2047 => (29, len - 1024, 10),
        2048..=4095 => (30, len - 2048, 11),
        4096..=8191 => (31, len - 4096, 12),
        8192..=16383 => (32, len - 8192, 13),
        16384..=32767 => (33, len - 16384, 14),
        32768..=65535 => (34, len - 32768, 15),
        65536..=131071 => (35, len - 65536, 16),
        _ => unreachable!(),
    }
}

/// Per-code metadata for match lengths: (base value, extra bit count).
const fn ml_meta() -> [(u32, u8); 53] {
    let mut t = [(0u32, 0u8); 53];
    let mut code = 0usize;
    // codes 0..=31 encode len = code + 3 directly
    while code < 32 {
        t[code] = (code as u32 + 3, 0);
        code += 1;
    }
    let rest = [
        (35u32, 1u8), (37, 1), (39, 1), (41, 1), (43, 2), (47, 2), (51, 3), (59, 3), (67, 4),
        (83, 4), (99, 5), (131, 7), (259, 8), (515, 9), (1027, 10), (2051, 11), (4099, 12),
        (8195, 13), (16387, 14), (32771, 15), (65539, 16),
    ];
    let mut i = 0;
    while i < rest.len() {
        t[32 + i] = rest[i];
        i += 1;
    }
    t
}
const ML_META: [(u32, u8); 53] = ml_meta();

/// Match-length code for the dense low range (len < 131).
const fn ml_code_lut() -> [u8; 131] {
    let mut t = [0u8; 131];
    let mut len = 0usize;
    while len < 131 {
        let mut code = 52;
        while code > 0 {
            if ML_META[code].0 as usize <= len {
                break;
            }
            code -= 1;
        }
        t[len] = code as u8;
        len += 1;
    }
    t
}
const ML_CODE_LUT: [u8; 131] = ml_code_lut();

#[inline]
fn encode_match_len(len: u32) -> (u8, u32, usize) {
    debug_assert!(len >= 3, "match lengths below 3 cannot be encoded");
    if len < 131 {
        let code = ML_CODE_LUT[len as usize] as usize;
        let (base, bits) = ML_META[code];
        return (code as u8, len - base, bits as usize);
    }
    match len {
        131..=258 => (43, len - 131, 7),
        259..=514 => (44, len - 259, 8),
        515..=1026 => (45, len - 515, 9),
        1027..=2050 => (46, len - 1027, 10),
        2051..=4098 => (47, len - 2051, 11),
        4099..=8194 => (48, len - 4099, 12),
        8195..=16386 => (49, len - 8195, 13),
        16387..=32770 => (50, len - 16387, 14),
        32771..=65538 => (51, len - 32771, 15),
        65539..=131074 => (52, len - 65539, 16),
        _ => unreachable!(),
    }
}

#[inline]
fn encode_offset(len: u32) -> (u8, u32, usize) {
    let log = len.ilog2();
    let lower = len & ((1 << log) - 1);
    (log as u8, lower, log as usize)
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

fn compress_literals(
    literals: &[u8],
    last_table: Option<&huff0_encoder::HuffmanTable>,
    writer: &mut BitWriter<&mut Vec<u8>>,
) -> Option<huff0_encoder::HuffmanTable> {
    let reset_idx = writer.index();

    // Cheap reject for near-incompressible literals (libzstd's
    // suspectUncompressible idea): building the tree, describing it and
    // running the four streams costs most of the literals section, so when
    // even the entropy bound cannot beat the raw copy by a margin, emit raw
    // right away instead of encoding and throwing the result away.
    {
        let mut counts = [0u32; 256];
        for &b in literals {
            counts[b as usize] += 1;
        }
        let total = literals.len() as f64;
        let mut entropy_bits = 0.0f64;
        for &c in &counts {
            if c > 0 {
                entropy_bits -= c as f64 * (c as f64 / total).log2();
            }
        }
        if entropy_bits + 256.0 + total * 0.08 >= total * 8.0 {
            raw_literals(literals, writer);
            return None;
        }
    }

    let new_encoder_table = huff0_encoder::HuffmanTable::build_from_data(literals);

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
