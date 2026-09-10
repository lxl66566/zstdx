//! Use [BitWriter] to write an arbitrary amount of bits into a buffer.
use alloc::vec::Vec;

/// An interface for writing an arbitrary number of bits into a buffer. Write new bits into the
/// buffer with `write_bits`, and obtain the output using `dump`.
#[derive(Debug)]
pub(crate) struct BitWriter<V: AsMut<Vec<u8>>> {
    /// The buffer that's filled with bits
    output: V,
    /// holds a partially filled byte which gets put in outpu when it's fill with a write_bits call
    partial: u64,
    bits_in_partial: usize,
    /// The index pointing to the next unoccupied bit. Effectively just
    /// the number of bits that have been written into the buffer so far.
    bit_idx: usize,
}

impl BitWriter<Vec<u8>> {
    /// Initialize a new writer.
    pub fn new() -> Self {
        Self {
            output: Vec::new(),
            partial: 0,
            bits_in_partial: 0,
            bit_idx: 0,
        }
    }
}

impl<V: AsMut<Vec<u8>>> BitWriter<V> {
    /// Initialize a new writer.
    pub fn from(mut output: V) -> BitWriter<V> {
        BitWriter {
            bit_idx: output.as_mut().len() * 8,
            output,
            partial: 0,
            bits_in_partial: 0,
        }
    }

    /// Get the current index. Can be used to reset to this index or to later change the bits at
    /// this index
    pub fn index(&self) -> usize {
        self.bit_idx + self.bits_in_partial
    }

    /// Reset to an index. Currently only supports resetting to a byte aligned index
    pub fn reset_to(&mut self, index: usize) {
        assert!(index.is_multiple_of(8));
        self.partial = 0;
        self.bits_in_partial = 0;
        self.bit_idx = index;
        self.output.as_mut().resize(index / 8, 0);
    }

    /// Change the bits at the index. `bits` contains the ǹum_bits` new bits that should be written
    /// Instead of the current content. `bits` *MUST* only contain zeroes in the upper bits outside
    /// of the `0..num_bits` range.
    pub fn change_bits(&mut self, idx: usize, bits: impl Into<u64>, num_bits: usize) {
        self.change_bits_64(idx, bits.into(), num_bits);
    }

    /// Monomorphized version of `change_bits`
    pub fn change_bits_64(&mut self, mut idx: usize, mut bits: u64, mut num_bits: usize) {
        self.flush();
        assert!(idx + num_bits < self.index());
        assert!(self.index() - (idx + num_bits) > self.bits_in_partial);

        // We might be changing bits unaligned to byte borders.
        // This means the lower bits of the first byte we are touching must stay the same
        if !idx.is_multiple_of(8) {
            // How many (upper) bits will change in the first byte?
            let bits_in_first_byte = 8 - (idx % 8);
            // We don't support only changing a few bits in the middle of a byte
            assert!(bits_in_first_byte <= num_bits);
            // Zero out the upper bits that will be changed while keeping the lower bits intact
            self.output.as_mut()[idx / 8] &= 0xffu8 >> bits_in_first_byte;
            // Shift the bits up and put them in the now zeroed out bits
            let new_bits = (bits << (8 - bits_in_first_byte)) as u8;
            self.output.as_mut()[idx / 8] |= new_bits;
            // Update the state. Note that we are now definitely working byte aligned
            num_bits -= bits_in_first_byte;
            bits >>= bits_in_first_byte;
            idx += bits_in_first_byte;
        }

        assert!(idx.is_multiple_of(8));
        // We are now byte aligned, change idx to byte resolution
        let mut idx = idx / 8;

        // Update full bytes by just shifting and extracting bytes from the bits
        while num_bits >= 8 {
            self.output.as_mut()[idx] = bits as u8;
            num_bits -= 8;
            bits >>= 8;
            idx += 1;
        }

        // Deal with leftover bits that wont fill a full byte, keeping the upper bits of the
        // original byte intact
        if num_bits > 0 {
            self.output.as_mut()[idx] &= 0xffu8 << num_bits;
            self.output.as_mut()[idx] |= bits as u8;
        }
    }

    /// Simply append bytes to the buffer. Only works if the buffer was already byte aligned
    pub fn append_bytes(&mut self, data: &[u8]) {
        assert!(
            self.misaligned() == 0,
            "Don't append bytes when writer is misaligned"
        );
        self.flush();
        self.output.as_mut().extend_from_slice(data);
        self.bit_idx += data.len() * 8;
    }

    /// Flush temporary internal buffers to the output buffer. Only works if this is currently byte
    /// aligned
    pub fn flush(&mut self) {
        assert!(self.bits_in_partial.is_multiple_of(8));
        let full_bytes = self.bits_in_partial / 8;
        self.output
            .as_mut()
            .extend_from_slice(&self.partial.to_le_bytes()[..full_bytes]);
        self.partial >>= full_bytes * 8;
        self.bits_in_partial -= full_bytes * 8;
        self.bit_idx += full_bytes * 8;
    }

    /// Write the lower `num_bits` from `bits` into the writer. `bits` *MUST* only contain zeroes in
    /// the upper bits outside of the `0..num_bits` range.
    pub fn write_bits(&mut self, bits: impl Into<u64>, num_bits: usize) {
        self.write_bits_64(bits.into(), num_bits);
    }

    /// This is the special case where we need to flush the partial buffer to the output.
    /// Marked as cold and in a separate function so the optimizer has more information.
    #[cold]
    fn write_bits_64_cold(&mut self, bits: u64, num_bits: usize) {
        assert!(self.bits_in_partial + num_bits >= 64);
        // Fill the partial buffer so it contains 64 bits
        let bits_free_in_partial = 64 - self.bits_in_partial;
        let part = bits << (64 - bits_free_in_partial);
        let merged = self.partial | part;
        // One unaligned u64 store beats a memcpy call for 8 bytes; the extra
        // bytes past a vector's length get overwritten by later stores.
        let output = self.output.as_mut();
        output.reserve(8);
        let len = output.len();
        // SAFETY: reserve covered the 8 bytes; only the low bytes are
        // semantically part of the output once bit_idx advances.
        unsafe {
            output
                .as_mut_ptr()
                .add(len)
                .cast::<u64>()
                .write_unaligned(merged.to_le());
            output.set_len(len + 8);
        }
        self.bit_idx += 64;
        self.partial = 0;
        self.bits_in_partial = 0;

        let mut num_bits = num_bits - bits_free_in_partial;
        let mut bits = bits >> bits_free_in_partial;

        // While we are at it push full bytes into the output buffer instead of polluting the
        // partial buffer
        while num_bits / 8 > 0 {
            let byte = bits as u8;
            self.output.as_mut().push(byte);
            num_bits -= 8;
            self.bit_idx += 8;
            bits >>= 8;
        }

        // The last few bits belong into the partial buffer
        assert!(num_bits < 8);
        if num_bits > 0 {
            let mask = (1 << num_bits) - 1;
            self.partial = bits & mask;
            self.bits_in_partial = num_bits;
        }
    }

    /// Monomorphized version of `change_bits`
    pub fn write_bits_64(&mut self, bits: u64, num_bits: usize) {
        if num_bits == 0 {
            return;
        }

        if bits > 0 {
            debug_assert!(bits.ilog2() <= num_bits as u32);
        }

        // fill partial byte first
        if num_bits + self.bits_in_partial < 64 {
            let part = bits << self.bits_in_partial;
            let merged = self.partial | part;
            self.partial = merged;
            self.bits_in_partial += num_bits;
        } else {
            // If the partial buffer can't hold the num_bits we need to make space
            self.write_bits_64_cold(bits, num_bits);
        }
    }

    /// Append the huffman codes for `data` in reverse symbol order (the
    /// format reads the stream back to front). `packed` holds
    /// `(code << 4) | num_bits` per symbol with at most 12 code bits, so
    /// four symbols always fit the container once fewer than eight bits are
    /// pending — the hot loop pays one flush per four symbols instead of a
    /// container-overflow branch per symbol. Produces the same bits as one
    /// `write_bits` call per symbol. `uniform_nb` is the common code length
    /// when the table is flat (zero otherwise); four-bit uniform codes take
    /// a dedicated bulk path that packs two symbols per output byte.
    pub fn write_packed_codes_rev(&mut self, packed: &[u16; 256], uniform_nb: u8, data: &[u8]) {
        let mut data = data;
        if uniform_nb == 4 && data.len() >= 16 && self.bits_in_partial.is_multiple_of(4) {
            data = self.write_uniform4_bulk(packed, data);
        }
        let mut acc = self.partial;
        let mut bits = self.bits_in_partial;
        let output = self.output.as_mut();
        let mut pos = self.bit_idx / 8;
        // The length only catches up with pos at the end, so reserves must be
        // sized from pos (a fixed reserve would be a no-op once capacity
        // reaches len + N and later stores would run past the allocation).
        for group in data.rchunks(4) {
            // Bring the pending bits below eight so the next four codes
            // cannot overflow the container.
            if bits >= 8 {
                let k = bits / 8;
                if pos + 16 > output.capacity() {
                    output.reserve(pos + 16 - output.len());
                }
                // SAFETY: the capacity check covers the 8-byte store; only
                // the low k bytes are semantic, the overshoot is overwritten
                // by the next store or snapped off by the final set_len.
                unsafe {
                    output
                        .as_mut_ptr()
                        .add(pos)
                        .cast::<u64>()
                        .write_unaligned(acc.to_le());
                }
                pos += k;
                acc >>= 8 * k;
                bits -= 8 * k;
            }
            for &sym in group.iter().rev() {
                let t = packed[sym as usize] as u64;
                acc |= (t >> 4) << bits;
                bits += (t & 15) as usize;
            }
        }
        let k = bits / 8;
        if k > 0 {
            if pos + 16 > output.capacity() {
                output.reserve(pos + 16 - output.len());
            }
            // SAFETY: as above.
            unsafe {
                output
                    .as_mut_ptr()
                    .add(pos)
                    .cast::<u64>()
                    .write_unaligned(acc.to_le());
            }
            pos += k;
            acc >>= 8 * k;
            bits -= 8 * k;
        }
        // SAFETY: pos matches the semantic end of the output; shrinking or
        // growing within the reserved capacity keeps the invariant that the
        // length equals bit_idx / 8.
        unsafe { output.set_len(pos) };
        self.partial = acc;
        self.bits_in_partial = bits;
        self.bit_idx = pos * 8;
    }

    /// Bulk tail of [`write_packed_codes_rev`] for flat four-bit tables:
    /// byte-aligns the pending bits, then packs whole 16-symbol groups as
    /// eight bytes each (two symbols per byte, first-encoded symbol in the
    /// low nibble) straight into the output. Returns the unconsumed prefix
    /// for the generic loop. Caller guarantees `bits_in_partial % 4 == 0`
    /// and `data.len() >= 16`.
    fn write_uniform4_bulk<'a>(&mut self, packed: &[u16; 256], data: &'a [u8]) -> &'a [u8] {
        let mut acc = self.partial;
        let mut bits = self.bits_in_partial;
        let mut n = data.len();
        // Peel trailing symbols until the pending bits hit a byte border so
        // the bulk groups start byte-aligned (at most two four-bit symbols).
        while !bits.is_multiple_of(8) {
            let t = packed[data[n - 1] as usize];
            acc |= ((t >> 4) as u64) << bits;
            bits += 4;
            n -= 1;
        }
        let output = self.output.as_mut();
        let mut pos = self.bit_idx / 8;
        let k = bits / 8;
        if k > 0 {
            if pos + 16 > output.capacity() {
                output.reserve(pos + 16 - output.len());
            }
            // SAFETY: the capacity check covers the 8-byte store; only the
            // low k bytes are semantic, the overshoot is overwritten by the
            // bulk stores or snapped off by the final set_len.
            unsafe {
                output
                    .as_mut_ptr()
                    .add(pos)
                    .cast::<u64>()
                    .write_unaligned(acc.to_le());
            }
            pos += k;
            acc >>= 8 * k;
            bits -= 8 * k;
        }
        debug_assert_eq!(bits, 0);

        let groups = n / 16;
        let bulk_end = n - groups * 16;
        if groups > 0 {
            let total = groups * 8;
            if pos + total + 16 > output.capacity() {
                output.reserve(pos + total + 16 - output.len());
            }
            // SAFETY: the reserve covers every store below; positions stay
            // within pos..pos+total. Four-bit codes cannot exceed the low
            // nibble, so two packed codes exactly form one output byte.
            unsafe {
                let mut p = output.as_mut_ptr().add(pos);
                let lut = |b: u8| (packed[b as usize] >> 4) as u8;
                let mut i = n;
                // AVX-512 packs whole 64-symbol chunks (four groups) with
                // byte-permutes instead of per-symbol LUT loads.
                #[cfg(all(target_arch = "x86_64", feature = "std"))]
                {
                    const CHUNK: usize = 64;
                    let full = groups / 4;
                    let simd_stop = n - full * CHUNK;
                    if full > 0
                        && std::is_x86_feature_detected!("avx512bw")
                        && std::is_x86_feature_detected!("avx512vbmi")
                    {
                        let mut tab = [0u8; 256];
                        for (b, t) in tab.iter_mut().enumerate() {
                            *t = lut(b as u8);
                        }
                        // SAFETY: the feature was just detected; the reserve
                        // covers all stores; i >= simd_stop + 63 keeps the
                        // 64-byte loads inside data.
                        p = uniform4_pack_avx512(data, simd_stop, n, p, &tab);
                        i = simd_stop;
                    }
                }
                while i > bulk_end {
                    // Encoding order runs back to front: data[i-1] is the
                    // next symbol and lands in the low nibble of byte 0.
                    *p = lut(data[i - 1]) | lut(data[i - 2]) << 4;
                    *p.add(1) = lut(data[i - 3]) | lut(data[i - 4]) << 4;
                    *p.add(2) = lut(data[i - 5]) | lut(data[i - 6]) << 4;
                    *p.add(3) = lut(data[i - 7]) | lut(data[i - 8]) << 4;
                    *p.add(4) = lut(data[i - 9]) | lut(data[i - 10]) << 4;
                    *p.add(5) = lut(data[i - 11]) | lut(data[i - 12]) << 4;
                    *p.add(6) = lut(data[i - 13]) | lut(data[i - 14]) << 4;
                    *p.add(7) = lut(data[i - 15]) | lut(data[i - 16]) << 4;
                    p = p.add(8);
                    i -= 16;
                }
            }
            pos += total;
        }
        // SAFETY: pos is the semantic end; see write_packed_codes_rev.
        unsafe { output.set_len(pos) };
        self.partial = acc;
        self.bits_in_partial = bits;
        self.bit_idx = pos * 8;
        &data[..bulk_end]
    }

    /// Returns the populated buffer that you've been writing bits into.
    ///
    /// This function consumes the writer, so it cannot be used after
    /// dumping
    pub fn dump(mut self) -> V {
        assert!(
            self.misaligned() == 0,
            "`dump` was called on a bit writer but an even number of bytes weren't written into \
             the buffer. Was: {}",
            self.index()
        );
        self.flush();
        debug_assert_eq!(self.partial, 0);
        self.output
    }

    /// Returns how many bits are missing for an even byte
    pub fn misaligned(&self) -> usize {
        let idx = self.index();
        if idx.is_multiple_of(8) {
            0
        } else {
            8 - (idx % 8)
        }
    }

    /// Hot-loop bit state as `(partial, bits_in_partial, byte_position)` so
    /// batched writers can keep the accumulator in registers instead of
    /// round-tripping it through the writer's fields on every push. The
    /// output buffer's length equals the byte position at this point.
    pub(crate) fn hot_state(&self) -> (u64, usize, usize) {
        (self.partial, self.bits_in_partial, self.bit_idx / 8)
    }

    /// Direct output access for the batched-writer flush stores.
    pub(crate) fn out(&mut self) -> &mut Vec<u8> {
        self.output.as_mut()
    }

    /// Restore state produced by a batched loop that started from
    /// [`hot_state`]: `pos` is the new semantic end (the caller's stores may
    /// have written past it within reserved capacity), with `bits` pending
    /// bits left in `partial`.
    pub(crate) fn set_hot_state(&mut self, partial: u64, bits: usize, pos: usize) {
        let output = self.output.as_mut();
        debug_assert!(pos <= output.capacity());
        // SAFETY: the batched loop reserved pos + slack and only advanced pos
        // over bytes it wrote, so growing (or snapping off overshoot) within
        // the capacity keeps the length == bit_idx / 8 invariant.
        unsafe { output.set_len(pos) };
        self.partial = partial;
        self.bits_in_partial = bits;
        self.bit_idx = pos * 8;
    }
}

/// Pack 4-bit huffman codes for `data[stop..end]` (a multiple of 64 bytes)
/// into `dst`, walking the data back to front: output byte `j` of each
/// 64-symbol chunk holds `code(data[i-1-2j]) | code(data[i-2-2j]) << 4`,
/// exactly like the scalar tail of [`BitWriter::write_packed_codes_rev`].
/// `tab` is the per-symbol 4-bit code table. Returns the advanced dst.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx512bw,avx512vbmi")]
unsafe fn uniform4_pack_avx512(
    data: &[u8],
    stop: usize,
    end: usize,
    mut dst: *mut u8,
    tab: &[u8; 256],
) -> *mut u8 {
    unsafe {
        use core::arch::x86_64::*;

        let lut01 = _mm512_loadu_si512(tab.as_ptr().cast());
        let lut01b = _mm512_loadu_si512(tab.as_ptr().add(64).cast());
        let lut23 = _mm512_loadu_si512(tab.as_ptr().add(128).cast());
        let lut23b = _mm512_loadu_si512(tab.as_ptr().add(192).cast());
        // Within a 64-symbol chunk (local 0..63, chunk base i): the low nibble of
        // output byte j reads local 63-2j, the high nibble local 62-2j; the upper
        // half of the permute indices is unused (only the low 32 bytes store).
        let mut idx_lo = [0u8; 64];
        let mut idx_hi = [0u8; 64];
        for j in 0..32 {
            idx_lo[j] = (63 - 2 * j) as u8;
            idx_hi[j] = (63 - 2 * j - 1) as u8;
        }
        let idx_lo = _mm512_loadu_si512(idx_lo.as_ptr().cast());
        let idx_hi = _mm512_loadu_si512(idx_hi.as_ptr().cast());
        let mask7f = _mm512_set1_epi8(0x7f);
        let mask_f0 = _mm512_set1_epi8(0xf0u8 as i8);

        let mut i = end;
        while i > stop {
            i -= 64;
            let v = _mm512_loadu_si512(data.as_ptr().add(i).cast());
            // 256-entry byte LUT: bits 0..6 select within a 128-byte permute
            // pair, bit 7 blends between the pairs.
            let lo7 = _mm512_and_si512(v, mask7f);
            let codes = _mm512_mask_blend_epi8(
                _mm512_movepi8_mask(v),
                _mm512_permutex2var_epi8(lut01, lo7, lut01b),
                _mm512_permutex2var_epi8(lut23, lo7, lut23b),
            );
            let lo = _mm512_permutexvar_epi8(idx_lo, codes);
            let hi = _mm512_and_si512(
                _mm512_slli_epi16(_mm512_permutexvar_epi8(idx_hi, codes), 4),
                mask_f0,
            );
            _mm256_storeu_si256(dst.cast(), _mm512_castsi512_si256(_mm512_or_si512(lo, hi)));
            dst = dst.add(32);
        }
        dst
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::BitWriter;

    #[test]
    fn from_existing() {
        // Define an existing vec, write some bits into it
        let mut existing_vec = vec![255_u8];
        let mut bw = BitWriter::from(&mut existing_vec);
        bw.write_bits(0u8, 8);
        bw.flush();
        assert_eq!(vec![255, 0], existing_vec);
    }

    #[test]
    fn change_bits() {
        let mut writer = BitWriter::new();
        writer.write_bits(0u32, 24);
        writer.change_bits(8, 0xffu8, 8);
        assert_eq!(vec![0, 0xff, 0], writer.dump());

        let mut writer = BitWriter::new();
        writer.write_bits(0u32, 24);
        writer.change_bits(6, 0x0fffu16, 12);
        assert_eq!(vec![0b11000000, 0xff, 0b00000011], writer.dump());
    }

    #[test]
    fn single_byte_written_4_4() {
        // Write the first 4 bits as 1s and the last 4 bits as 0s
        // 1010 is used where values should never be read from.
        let mut bw = BitWriter::new();
        bw.write_bits(0b1111u8, 4);
        bw.write_bits(0b0000u8, 4);
        let output = bw.dump();
        assert!(
            output.len() == 1,
            "Single byte written into writer returned a vec that wasn't one byte, vec was {} \
             elements long",
            output.len()
        );
        assert_eq!(
            0b0000_1111, output[0],
            "4 bits and 4 bits written into buffer"
        );
    }

    #[test]
    fn single_byte_written_3_5() {
        // Write the first 3 bits as 1s and the last 5 bits as 0s
        let mut bw = BitWriter::new();
        bw.write_bits(0b111u8, 3);
        bw.write_bits(0b0_0000u8, 5);
        let output = bw.dump();
        assert!(
            output.len() == 1,
            "Single byte written into writer return a vec that wasn't one byte, vec was {} \
             elements long",
            output.len()
        );
        assert_eq!(0b0000_0111, output[0], "3 and 5 bits written into buffer");
    }

    #[test]
    fn single_byte_written_1_7() {
        // Write the first bit as a 1 and the last 7 bits as 0s
        let mut bw = BitWriter::new();
        bw.write_bits(0b1u8, 1);
        bw.write_bits(0u8, 7);
        let output = bw.dump();
        assert!(
            output.len() == 1,
            "Single byte written into writer return a vec that wasn't one byte, vec was {} \
             elements long",
            output.len()
        );
        assert_eq!(0b0000_0001, output[0], "1 and 7 bits written into buffer");
    }

    #[test]
    fn single_byte_written_8() {
        // Write an entire byte
        let mut bw = BitWriter::new();
        bw.write_bits(1u8, 8);
        let output = bw.dump();
        assert!(
            output.len() == 1,
            "Single byte written into writer return a vec that wasn't one byte, vec was {} \
             elements long",
            output.len()
        );
        assert_eq!(1, output[0], "1 and 7 bits written into buffer");
    }

    #[test]
    fn multi_byte_clean_boundary_4_4_4_4() {
        // Writing 4 bits at a time for 2 bytes
        let mut bw = BitWriter::new();
        bw.write_bits(0u8, 4);
        bw.write_bits(0b1111u8, 4);
        bw.write_bits(0b1111u8, 4);
        bw.write_bits(0u8, 4);
        assert_eq!(vec![0b1111_0000, 0b0000_1111], bw.dump());
    }

    #[test]
    fn multi_byte_clean_boundary_16_8() {
        // Writing 16 bits at once
        let mut bw = BitWriter::new();
        bw.write_bits(0x0100u16, 16);
        bw.write_bits(69u8, 8);
        assert_eq!(vec![0, 1, 69], bw.dump());
    }

    #[test]
    fn multi_byte_boundary_crossed_4_12() {
        // Writing 4 1s and then 12 zeros
        let mut bw = BitWriter::new();
        bw.write_bits(0b1111u8, 4);
        bw.write_bits(0b0000_0011_0100_0010u16, 12);
        assert_eq!(vec![0b0010_1111, 0b0011_0100], bw.dump());
    }

    #[test]
    fn multi_byte_boundary_crossed_4_5_7() {
        // Writing 4 1s and then 5 zeros then 7 1s
        let mut bw = BitWriter::new();
        bw.write_bits(0b1111u8, 4);
        bw.write_bits(0b0_0000u8, 5);
        bw.write_bits(0b111_1111u8, 7);
        assert_eq!(vec![0b0000_1111, 0b1111_1110], bw.dump());
    }

    #[test]
    fn multi_byte_boundary_crossed_1_9_6() {
        // Writing 1 1 and then 9 zeros then 6 1s
        let mut bw = BitWriter::new();
        bw.write_bits(0b1u8, 1);
        bw.write_bits(0b0_0000_0000u16, 9);
        bw.write_bits(0b11_1111u8, 6);
        assert_eq!(vec![0b0000_0001, 0b1111_1100], bw.dump());
    }

    #[test]
    #[should_panic(expected = "`dump` was called on a bit writer")]
    fn catches_unaligned_dump() {
        // Write a single bit in then dump it, making sure
        // the correct error is returned
        let mut bw = BitWriter::new();
        bw.write_bits(0u8, 1);
        bw.dump();
    }

    // Relies on a debug_assert inside write_bits_64, so it can only panic on
    // debug builds; gate it to keep `cargo test --release` green.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "bits.ilog2() <= num_bits as u32")]
    fn catches_dirty_upper_bits() {
        let mut bw = BitWriter::new();
        bw.write_bits(10u8, 1);
    }

    #[test]
    fn add_multiple_aligned() {
        let mut bw = BitWriter::new();
        bw.write_bits(0x00_0f_f0_ffu32, 32);
        assert_eq!(vec![0xff, 0xf0, 0x0f, 0x00], bw.dump());
    }

    // #[test]
    // fn catches_more_than_in_buf() {
    //     todo!();
    // }

    #[test]
    fn packed_codes_rev_matches_write_bits() {
        // deterministic skewed symbols, codes up to 9 bits
        let mut packed = [0u16; 256];
        let mut state = 0x0123_4567_89ab_cdefu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for p in &mut packed {
            let nb = 1 + (next() as usize % 9);
            let code = next() as usize & ((1 << nb) - 1);
            *p = ((code << 4) | nb) as u16;
        }
        // Flat table variant: every symbol shares a four-bit code, which is
        // what uniform alphabets (9..16 symbols) produce; the bulk path must
        // reproduce the generic bit accumulation for every entry alignment.
        let mut packed_uniform = [0u16; 256];
        for (sym, p) in packed_uniform.iter_mut().enumerate() {
            let code = (sym * 7 + 3) & 0xf;
            *p = ((code << 4) | 4) as u16;
        }
        for size in [0usize, 1, 2, 3, 4, 5, 8, 31, 1025, 4096, 16384] {
            let mut data = vec![0u8; size];
            for b in &mut data {
                *b = (next() % 256) as u8;
            }
            // Entry states the encoder really produces: fresh writer, and
            // writers with a partial container left by preceding headers.
            for entry_bits in [0usize, 3, 4, 8, 12, 17, 20, 48, 63] {
                for (packed, uniform_nb) in [(packed, 0u8), (packed_uniform, 4u8)] {
                    let mut reference = BitWriter::new();
                    let mut batched = BitWriter::new();
                    if entry_bits > 0 {
                        let entry = next() & ((1u64 << entry_bits.min(63)) - 1);
                        reference.write_bits(entry, entry_bits.min(63));
                        batched.write_bits(entry, entry_bits.min(63));
                    }
                    for symbol in data.iter().rev() {
                        let t = packed[*symbol as usize];
                        reference.write_bits((t >> 4) as u64, (t & 15) as usize);
                    }
                    let fill = reference.misaligned();
                    if fill == 0 {
                        reference.write_bits(1u32, 8);
                    } else {
                        reference.write_bits(1u32, fill);
                    }
                    batched.write_packed_codes_rev(&packed, uniform_nb, &data);
                    let fill = batched.misaligned();
                    if fill == 0 {
                        batched.write_bits(1u32, 8);
                    } else {
                        batched.write_bits(1u32, fill);
                    }
                    assert_eq!(
                        reference.dump(),
                        batched.dump(),
                        "stream mismatch at size {size} entry_bits {entry_bits} uniform \
                         {uniform_nb}"
                    );
                }
            }
        }
    }
}
