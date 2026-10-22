//! FSE, short for Finite State Entropy, is an encoding technique
//! that assigns shorter codes to symbols that appear more frequently in data,
//! and longer codes to less frequent symbols.
//!
//! FSE works by mutating a state and using that state to index into a table.
//!
//! Zstandard uses two different kinds of entropy encoding: FSE, and Huffman coding.
//! Huffman is used to compress literals,
//! while FSE is used for all other symbols (literal length code, match length code, offset code).
//!
//! <https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#fse>
//!
//! <https://arxiv.org/pdf/1311.2540>

mod fse_decoder;

pub use fse_decoder::*;

pub mod fse_encoder;

/// Offset added to the 4-bit accuracy-log nibble of an NCount stream: the
/// format's minimum FSE table log (libzstd's FSE_MIN_TABLELOG).
pub(crate) const ACC_LOG_OFFSET: u8 = 5;

/// Serialize one normalized distribution as a wire NCount stream (the fuzz
/// targets' counterpart of the internal `write_ncount`; `probs` must sum to
/// `1 << acc_log`, counting a -1 entry as one state, and acc_log must be in
/// `ACC_LOG_OFFSET..=12`).
#[cfg(feature = "fuzz_exports")]
pub fn serialize_ncount(probs: &[i32], acc_log: u8) -> alloc::vec::Vec<u8> {
    let mut writer = crate::bit_io::BitWriter::new();
    fse_encoder::write_ncount(probs, acc_log, &mut writer);
    writer.dump()
}

#[test]
fn tables_equal() {
    let probs = &[0, 0, -1, 3, 2, 2, (1 << 6) - 8];
    let mut dec_table = FSETable::new(255);
    dec_table.build_from_probabilities(6, probs).unwrap();
    let enc_table = fse_encoder::build_table_from_probabilities(probs, 6);

    check_tables(&dec_table, &enc_table);
}

#[cfg(any(test, feature = "fuzz_exports"))]
fn check_tables(dec_table: &FSETable, enc_table: &fse_encoder::FSETable) {
    let ts = enc_table.table_size;
    for (idx, dec_state) in dec_table.decode.iter().enumerate() {
        // Every decoder state is the target of exactly one encode step from
        // its own symbol; scan the source states and compare the wire
        // parameters (the emitted diff is `state - baseline`).
        let mut found = false;
        for i in 0..ts {
            let (nb, emit, next) = enc_table.step(dec_state.symbol, (ts + i) as u32);
            if next as usize == ts + idx {
                assert_eq!(nb as u8, dec_state.num_bits, "num_bits at state {i}");
                assert_eq!(
                    emit as usize,
                    i - dec_state.base_line as usize,
                    "wire diff at state {i}"
                );
                found = true;
            }
        }
        assert!(found, "no encode step targets decoder state {idx}");
    }
}

#[test]
fn roundtrip() {
    round_trip(&(0..64).collect::<alloc::vec::Vec<_>>());
    let mut data = alloc::vec![];
    data.extend(0..32);
    data.extend(0..32);
    data.extend(0..32);
    data.extend(0..32);
    data.extend(0..32);
    data.extend(20..32);
    data.extend(20..32);
    data.extend(0..32);
    data.extend(20..32);
    data.extend(100..255);
    data.extend(20..32);
    data.extend(20..32);
    round_trip(&data);

    #[cfg(feature = "std")]
    if std::fs::exists("../zstdx-fuzz/artifacts/fse").unwrap_or(false) {
        for file in std::fs::read_dir("../zstdx-fuzz/artifacts/fse").unwrap() {
            if file.as_ref().unwrap().file_type().unwrap().is_file() {
                let data = std::fs::read(file.unwrap().path()).unwrap();
                round_trip(&data);
            }
        }
    }
}

/// Only needed for testing.
///
/// Encodes the data with a table built from that data
/// Decodes the result again by first decoding the table and then the data
/// Asserts that the decoded data equals the input
#[cfg(any(test, feature = "fuzz_exports"))]
pub fn round_trip(data: &[u8]) {
    use fse_encoder::FSEEncoder;

    use crate::bit_io::{BitReaderReversed, BitWriter};

    if data.len() < 2 {
        return;
    }
    if data.iter().all(|x| *x == data[0]) {
        return;
    }
    if data.len() < 64 {
        return;
    }

    let mut writer = BitWriter::new();
    let mut encoder = FSEEncoder::new(
        fse_encoder::build_table_from_data(data.iter().copied(), 22, false),
        &mut writer,
    );
    let mut dec_table = FSETable::new(255);
    encoder.encode(data);
    let acc_log = encoder.acc_log();
    let enc_table = encoder.into_table();
    let encoded = writer.dump();

    let table_bytes = dec_table.build_decoder(&encoded, acc_log).unwrap();
    let encoded = &encoded[table_bytes..];
    let mut decoder = FSEDecoder::new(&dec_table);

    check_tables(&dec_table, &enc_table);

    let mut br = BitReaderReversed::new(encoded);
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
    decoder.init_state(&mut br).unwrap();
    let mut decoded = alloc::vec::Vec::new();

    for x in data {
        let w = decoder.decode_symbol();
        assert_eq!(w, *x);
        decoded.push(w);
        if decoded.len() < data.len() {
            decoder.update_state(&mut br);
        }
    }

    assert_eq!(&decoded, data);

    assert_eq!(br.bits_remaining(), 0);
}

#[cfg(test)]
mod ncount_tests {
    use super::{ACC_LOG_OFFSET, FSETable, fse_encoder::write_ncount};
    use crate::bit_io::BitWriter;

    /// `write_ncount`'s description must be read back by `read_probabilities`
    /// exactly: the same distribution (up to the last live symbol; trailing
    /// zeros are not serialized), the same accuracy log, a byte count equal to
    /// the written bytes, and the identical decoding table a direct
    /// `build_from_probabilities` produces. Pins the zero-run marker path —
    /// runs past 24 exercise the packed 0xFFFF marker form — and the -1
    /// low-probability wire form against the decoder, across the full
    /// serializable accuracy range.
    #[test]
    fn ncount_roundtrip() {
        let mut seed = 0x9e3779b9_7f4a7c15u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..300u32 {
            let acc_log = ACC_LOG_OFFSET + (rand() % 8) as u8;
            let table_size = 1u64 << acc_log;
            let nsym = 2 + (rand() % 62) as usize;
            let mut probs = alloc::vec![0i32; nsym];

            // Every 5th case plants an interior dead run past 24 symbols so
            // the serializer takes its packed 0xFFFF marker form. The run
            // stays clear of the last symbol.
            let (run_start, run_end) = if case % 5 == 0 {
                let run = 25 + (rand() % 10) as usize;
                let start = 1 + (rand() % ((nsym / 2).max(1) as u64)) as usize;
                let end = (start + run).min(nsym - 1);
                (Some(start), end)
            } else {
                (None, 0)
            };

            // Assign single states (or -1 wire forms) while the state budget
            // lasts; everything else stays dead.
            let mut budget = table_size;
            let mut has_positive = false;
            for (idx, prob) in probs.iter_mut().enumerate() {
                if run_start.is_some_and(|s| idx >= s && idx < run_end) {
                    continue;
                }
                let r = rand();
                if budget <= 1 || r % 4 == 0 {
                    continue; // dead symbol
                }
                budget -= 1;
                if r % 9 == 0 && budget > 1 {
                    *prob = -1;
                } else {
                    *prob = 1;
                    has_positive = true;
                }
            }
            let in_run = |i: usize| run_start.is_some_and(|s| i >= s && i < run_end);
            if !has_positive {
                // The pour below needs a positive sink. Promoting a dead
                // slot costs one of the >= 1 states the assignment loop
                // left over; if every live slot is a -1, promote one of
                // those instead (same single-state cost, no extra budget).
                if let Some(i) = (0..nsym).find(|&i| probs[i] == 0 && !in_run(i)) {
                    budget -= 1;
                    probs[i] = 1;
                } else {
                    let i = (0..nsym).find(|&i| probs[i] == -1).unwrap();
                    probs[i] = 1;
                }
            }

            // Pour the remaining states onto the live positives in random
            // chunks so the sum lands exactly on 1 << acc_log.
            while budget > 0 {
                let idx = (rand() as usize) % nsym;
                if probs[idx] > 0 {
                    let add = 1 + rand() % budget;
                    probs[idx] += add as i32;
                    budget -= add;
                }
            }
            // -1 costs one state; positives cost their weight.
            let sum: i32 = probs
                .iter()
                .map(|&p| {
                    if p == -1 {
                        1
                    } else {
                        p
                    }
                })
                .sum();
            assert_eq!(sum as u64, table_size, "case {case} generator");

            let mut writer = BitWriter::new();
            write_ncount(&probs, acc_log, &mut writer);
            let bytes = writer.dump();

            let mut decoded = FSETable::new(255);
            let used = match decoded.build_decoder(&bytes, 12) {
                Ok(u) => u,
                Err(e) => {
                    panic!("case {case} acc {acc_log} probs {probs:?}: {e:?}");
                },
            };
            assert_eq!(used, bytes.len(), "case {case} consumed bytes");
            assert_eq!(decoded.accuracy_log, acc_log, "case {case} acc_log");

            // Trailing dead symbols are not serialized: compare through the
            // last live one, then pin both paths to the same decoding table.
            let last = probs.iter().rposition(|&p| p != 0).unwrap();
            let mut reference = FSETable::new(255);
            reference
                .build_from_probabilities(acc_log, &probs[..=last])
                .unwrap();
            assert_eq!(
                decoded.decode, reference.decode,
                "case {case} decode table probs {probs:?}"
            );
        }
    }

    /// The final symbol's short form legally ends its read one bit past the
    /// last byte of the description; libzstd's zero-padded read window
    /// accepts it, so this decoder must too. Pins the exact distribution
    /// that first exposed the off-by-one rejection.
    #[test]
    fn ncount_final_short_form_lookahead() {
        // acc_log 10, table 1024: the trailing -1 serializes to a single
        // short-form bit landing on the last bit of the 14th byte.
        let probs: &[i32] = &[-1, 0, 1, -1, 1, 70, 0, 1, 1, 888, 59, -1];
        let mut writer = BitWriter::new();
        write_ncount(probs, 10, &mut writer);
        let bytes = writer.dump();
        assert_eq!(bytes.len(), 14);
        let mut table = FSETable::new(255);
        let used = table.build_decoder(&bytes, 12).unwrap();
        assert_eq!(used, 14);
        assert_eq!(table.accuracy_log, 10);
    }

    /// A description truncated beyond the one-bit lookahead must be
    /// rejected, not zero-padded through.
    #[test]
    fn ncount_truncated() {
        let probs: &[i32] = &[-1, 0, 1, -1, 1, 70, 0, 1, 1, 888, 59, -1];
        let mut writer = BitWriter::new();
        write_ncount(probs, 10, &mut writer);
        let bytes = writer.dump();
        for cut in [11, 12, 13] {
            let mut table = FSETable::new(255);
            assert!(
                table.build_decoder(&bytes[..cut], 12).is_err(),
                "cut {cut} must be rejected"
            );
        }
    }
}
