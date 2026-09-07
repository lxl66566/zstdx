use alloc::vec::Vec;

use crate::{
    bit_io::BitWriter,
    encoding::frame_compressor::CompressState,
    encoding::{Matcher, Sequence},
    fse::fse_encoder::{build_table_from_data, FSETable},
    huff0::huff0_encoder,
};

/// A block of [`crate::common::BlockType::Compressed`]
pub fn compress_block<M: Matcher>(state: &mut CompressState<M>, output: &mut Vec<u8>) {
    let mut literals_vec = Vec::new();
    let mut sequences = Vec::new();
    state.matcher.start_matching(|seq| match seq {
        Sequence::Literals { literals } => literals_vec.extend_from_slice(literals),
        Sequence::Triple {
            literals,
            offset,
            match_len,
            } => {
                literals_vec.extend_from_slice(literals);
                sequences.push(crate::blocks::sequence_section::Sequence {
                    ll: literals.len() as u32,
                    ml: match_len as u32,
                    of: offset as u32,
                });
            }
    });

    // literals section

    let mut writer = BitWriter::from(output);
    if literals_vec.len() > 1024 {
        if let Some(table) =
            compress_literals(&literals_vec, state.last_huff_table.as_ref(), &mut writer)
        {
            state.last_huff_table.replace(table);
        }
    } else {
        raw_literals(&literals_vec, &mut writer);
    }

    // sequences section

    if sequences.is_empty() {
        writer.write_bits(0u8, 8);
    } else {
        encode_seqnum(sequences.len(), &mut writer);

        // Choose the tables
        // TODO store previously used tables
        let ll_mode = choose_table(
            state.fse_tables.ll_previous.as_ref(),
            &state.fse_tables.ll_default,
            sequences.iter().map(|seq| encode_literal_length(seq.ll).0),
            9,
        );
        let ml_mode = choose_table(
            state.fse_tables.ml_previous.as_ref(),
            &state.fse_tables.ml_default,
            sequences.iter().map(|seq| encode_match_len(seq.ml).0),
            9,
        );
        let of_mode = choose_table(
            state.fse_tables.of_previous.as_ref(),
            &state.fse_tables.of_default,
            sequences.iter().map(|seq| encode_offset(seq.of).0),
            8,
        );

        writer.write_bits(encode_fse_table_modes(&ll_mode, &ml_mode, &of_mode), 8);

        encode_table(&ll_mode, &mut writer);
        encode_table(&of_mode, &mut writer);
        encode_table(&ml_mode, &mut writer);

        encode_sequences(
            &sequences,
            &mut writer,
            ll_mode.as_ref(),
            ml_mode.as_ref(),
            of_mode.as_ref(),
        );

        if let FseTableMode::Encoded(table) = ll_mode {
            state.fse_tables.ll_previous = Some(table)
        }
        if let FseTableMode::Encoded(table) = ml_mode {
            state.fse_tables.ml_previous = Some(table)
        }
        if let FseTableMode::Encoded(table) = of_mode {
            state.fse_tables.of_previous = Some(table)
        }
    }
    writer.flush();
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum FseTableMode<'a> {
    Predefined(&'a FSETable),
    Encoded(FSETable),
    RepeateLast(&'a FSETable),
}

impl FseTableMode<'_> {
    pub fn as_ref(&self) -> &FSETable {
        match self {
            Self::Predefined(t) => t,
            Self::RepeateLast(t) => t,
            Self::Encoded(t) => t,
        }
    }
}

fn choose_table<'a>(
    previous: Option<&'a FSETable>,
    default_table: &'a FSETable,
    data: impl Iterator<Item = u8>,
    max_log: u8,
) -> FseTableMode<'a> {
    // TODO check if the new table is better than the predefined and previous table
    let use_new_table = true;
    let use_previous_table = false;
    if use_previous_table {
        FseTableMode::RepeateLast(previous.unwrap())
    } else if use_new_table {
        FseTableMode::Encoded(build_table_from_data(data, max_log, true))
    } else {
        FseTableMode::Predefined(default_table)
    }
}

fn encode_table(mode: &FseTableMode<'_>, writer: &mut BitWriter<&mut Vec<u8>>) {
    match mode {
        FseTableMode::Predefined(_) => {}
        FseTableMode::RepeateLast(_) => {}
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
            FseTableMode::Encoded(_) => 2,
            FseTableMode::RepeateLast(_) => 3,
        }
    }
    mode_to_bits(ll_mode) << 6 | mode_to_bits(of_mode) << 4 | mode_to_bits(ml_mode) << 2
}

fn encode_sequences(
    sequences: &[crate::blocks::sequence_section::Sequence],
    writer: &mut BitWriter<&mut Vec<u8>>,
    ll_table: &FSETable,
    ml_table: &FSETable,
    of_table: &FSETable,
) {
    let sequence = sequences[sequences.len() - 1];
    let (ll_code, ll_add_bits, ll_num_bits) = encode_literal_length(sequence.ll);
    let (of_code, of_add_bits, of_num_bits) = encode_offset(sequence.of);
    let (ml_code, ml_add_bits, ml_num_bits) = encode_match_len(sequence.ml);
    let mut ll_state = ll_table.start_state(ll_code).index;
    let mut ml_state = ml_table.start_state(ml_code).index;
    let mut of_state = of_table.start_state(of_code).index;

    write_add_bits(
        writer,
        ll_add_bits,
        ll_num_bits,
        ml_add_bits,
        ml_num_bits,
        of_add_bits,
        of_num_bits,
    );

    let ll_size = ll_table.table_size;
    let ml_size = ml_table.table_size;
    let of_size = of_table.table_size;

    // encode backwards so the decoder reads the first sequence first
    if sequences.len() > 1 {
        for sequence in (0..=sequences.len() - 2).rev() {
            let sequence = sequences[sequence];
            let (ll_code, ll_add_bits, ll_num_bits) = encode_literal_length(sequence.ll);
            let (of_code, of_add_bits, of_num_bits) = encode_offset(sequence.of);
            let (ml_code, ml_add_bits, ml_num_bits) = encode_match_len(sequence.ml);

            // The three state-transition bit groups (max 15 bits each) and the
            // three extra-bit groups (max 16+16+19 bits) each fit a single
            // u64 write; concatenating them keeps the writer's hot path.
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

            write_add_bits(
                writer,
                ll_add_bits,
                ll_num_bits,
                ml_add_bits,
                ml_num_bits,
                of_add_bits,
                of_num_bits,
            );
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

/// Write the per-sequence literal-length, match-length and offset extra bits
/// with one concatenated bit write (max 51 bits). Bit order matches the
/// decoder: ll first, then ml, then of.
fn write_add_bits(
    writer: &mut BitWriter<&mut Vec<u8>>,
    ll_add_bits: u32,
    ll_num_bits: usize,
    ml_add_bits: u32,
    ml_num_bits: usize,
    of_add_bits: u32,
    of_num_bits: usize,
) {
    let add = ll_add_bits as u64
        | ((ml_add_bits as u64) << ll_num_bits)
        | ((of_add_bits as u64) << (ll_num_bits + ml_num_bits));
    writer.write_bits(add, ll_num_bits + ml_num_bits + of_num_bits);
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

fn compress_literals(
    literals: &[u8],
    last_table: Option<&huff0_encoder::HuffmanTable>,
    writer: &mut BitWriter<&mut Vec<u8>>,
) -> Option<huff0_encoder::HuffmanTable> {
    let reset_idx = writer.index();

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
