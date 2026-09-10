#[test]
fn test_bitreader_reversed() {
    use crate::bit_io::BitReaderReversed;

    let encoded: [u8; 16] = [
        0xc1, 0x41, 0x08, 0x00, 0x00, 0xec, 0xc8, 0x96, 0x42, 0x79, 0xd4, 0xbc, 0xf7, 0x2c, 0xd5,
        0x48,
    ];
    // just the u128 in encoded
    let num_rev: u128 = 0x48_d5_2c_f7_bc_d4_79_42_96_c8_ec_00_00_08_41_c1;

    let mut br = BitReaderReversed::new(&encoded[..]);
    let mut accumulator = 0;
    let mut bits_read = 0;
    let mut x = 0;

    loop {
        x += 3;
        // semi random access pattern
        let mut num_bits = x % 16;
        if bits_read > 128 - num_bits {
            num_bits = 128 - bits_read;
        }

        let bits = br.get_bits(num_bits);
        bits_read += num_bits;
        accumulator |= u128::from(bits) << (128 - bits_read);
        if bits_read >= 128 {
            break;
        }
    }

    assert_eq!(accumulator, num_rev, "Bitreader failed somewhere");
}

#[test]
fn test_bitreader_normal() {
    use crate::bit_io::BitReader;

    let encoded: [u8; 16] = [
        0xc1, 0x41, 0x08, 0x00, 0x00, 0xec, 0xc8, 0x96, 0x42, 0x79, 0xd4, 0xbc, 0xf7, 0x2c, 0xd5,
        0x48,
    ];
    // just the u128 in encoded
    let num: u128 = 0x48_d5_2c_f7_bc_d4_79_42_96_c8_ec_00_00_08_41_c1;

    let mut br = BitReader::new(&encoded[..]);
    let mut accumulator = 0;
    let mut bits_read = 0;
    let mut x = 0;

    loop {
        x += 3;
        // semi random access pattern
        let mut num_bits = x % 16;
        if bits_read > 128 - num_bits {
            num_bits = 128 - bits_read;
        }

        let bits = br.get_bits(num_bits).unwrap();
        accumulator |= u128::from(bits) << bits_read;
        bits_read += num_bits;
        if bits_read >= 128 {
            break;
        }
    }

    assert_eq!(accumulator, num, "Bitreader failed somewhere");
}
