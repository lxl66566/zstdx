/// Huffman coding is a method of encoding where symbols are assigned a code,
/// and more commonly used symbols get shorter codes, and less commonly
/// used symbols get longer codes. Codes are prefix free, meaning no two codes
/// will start with the same sequence of bits.
mod huff0_decoder;
pub use huff0_decoder::*;
pub mod huff0_encoder;

/// Only needed for testing.
///
/// Encodes the data with a table built from that data
/// Decodes the result again by first decoding the table and then the data
/// Asserts that the decoded data equals the input
#[cfg(any(test, feature = "fuzz_exports"))]
pub fn round_trip(data: &[u8]) {
    use alloc::vec::Vec;

    use crate::bit_io::{BitReaderReversed, BitWriter};

    if data.len() < 2 {
        return;
    }
    if data.iter().all(|x| *x == data[0]) {
        return;
    }
    let mut writer = BitWriter::new();
    let encoder_table = huff0_encoder::HuffmanTable::build_from_data(data);
    let mut encoder = huff0_encoder::HuffmanEncoder::new(&encoder_table, &mut writer);

    encoder.encode(data, true);
    let encoded = writer.dump();
    let mut decoder_table = HuffmanTable::new();
    let table_bytes = decoder_table.build_decoder(&encoded).unwrap();
    let mut decoder = HuffmanDecoder::new(&decoder_table);

    let mut br = BitReaderReversed::new(&encoded[table_bytes as usize..]);
    let mut skipped_bits = 0;
    loop {
        let val = br.get_bits(1);
        skipped_bits += 1;
        if val == 1 || skipped_bits > 8 {
            break;
        }
    }
    // if more than 7 bits are 0, this is not the correct end of the bitstream. Either a bug or
    // corrupted data
    assert!(skipped_bits <= 8, "Corrupted end marker");

    decoder.init_state(&mut br);
    let mut decoded = Vec::new();
    while br.bits_remaining() > -(decoder_table.max_num_bits as isize) {
        decoded.push(decoder.decode_symbol());
        decoder.next_state(&mut br);
    }
    assert_eq!(&decoded, data);
}

/// Same as `round_trip` but decoding through the double-symbol X2 table,
/// one table lookup at a time (1 or 2 symbols per lookup).
#[cfg(any(test, feature = "fuzz_exports"))]
pub fn round_trip_x2(data: &[u8]) {
    use alloc::vec::Vec;

    use crate::bit_io::{BitReaderReversed, BitWriter};

    if data.len() < 2 {
        return;
    }
    if data.iter().all(|x| *x == data[0]) {
        return;
    }
    let mut writer = BitWriter::new();
    let encoder_table = huff0_encoder::HuffmanTable::build_from_data(data);
    let mut encoder = huff0_encoder::HuffmanEncoder::new(&encoder_table, &mut writer);

    encoder.encode(data, true);
    let encoded = writer.dump();
    let mut decoder_table = HuffmanTable::new();
    let table_bytes = decoder_table.build_decoder(&encoded).unwrap();
    decoder_table.build_x2_table();
    let dt = decoder_table.x2_table();
    if dt.is_empty() {
        // the table is not pair-friendly enough for X2 to pay off
        return;
    }

    let mut br = BitReaderReversed::new(&encoded[table_bytes as usize..]);
    let mut skipped_bits = 0;
    loop {
        let val = br.get_bits(1);
        skipped_bits += 1;
        if val == 1 || skipped_bits > 8 {
            break;
        }
    }
    assert!(skipped_bits <= 8, "Corrupted end marker");

    let mut decoded = Vec::new();
    while decoded.len() < data.len() {
        // the very last symbol of the stream may share its lookup with
        // padding bits that look like a second code; decode it through the
        // single-symbol table instead (mirrors libzstd's decodeLastSymbolX2)
        if decoded.len() + 2 > data.len() {
            let mut dec1 = HuffmanDecoder::new(&decoder_table);
            let _ = dec1.init_state(&mut br);
            decoded.push(dec1.decode_symbol());
            dec1.next_state(&mut br);
            break;
        }
        let entry = dt[br.peek_bits_refilled(11) as usize];
        let nb = ((entry >> 16) & 0x3f) as u8;
        let len = (entry >> 24) as usize;
        br.consume(nb);
        decoded.push(entry as u8);
        if len == 2 {
            decoded.push((entry >> 8) as u8);
        }
    }
    assert_eq!(&decoded, data);
}

#[test]
fn roundtrip() {
    use alloc::vec::Vec;
    round_trip(&[1, 1, 1, 1, 2, 3]);
    round_trip(&[1, 1, 1, 1, 2, 3, 5, 45, 12, 90]);

    for size in 2..512 {
        use alloc::vec;
        let data = vec![123; size];
        round_trip(&data);
        round_trip_x2(&data);
        let mut data = Vec::new();
        for x in 0..size {
            data.push(x as u8);
        }
        round_trip(&data);
        round_trip_x2(&data);
    }

    #[cfg(feature = "std")]
    if std::fs::exists("fuzz/artifacts/huff0").unwrap_or(false) {
        for file in std::fs::read_dir("fuzz/artifacts/huff0").unwrap() {
            if file.as_ref().unwrap().file_type().unwrap().is_file() {
                let data = std::fs::read(file.unwrap().path()).unwrap();
                round_trip(&data);
            }
        }
    }
}
