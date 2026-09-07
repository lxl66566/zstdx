use core::convert::TryInto;

/// Zstandard encodes some types of data in a way that the data must be read
/// back to front to decode it properly. `BitReaderReversed` provides a
/// convenient interface to do that.
pub struct BitReaderReversed<'s> {
    /// The index of the last read byte in the source.
    index: usize,

    /// How many bits have been consumed from `bit_container`.
    bits_consumed: u8,

    /// How many bits have been consumed past the end of the input. Will be zero until all the input
    /// has been read.
    extra_bits: usize,

    /// The source data to read from.
    source: &'s [u8],

    /// The reader doesn't read directly from the source, it reads bits from here, and the container
    /// is "refilled" as it's emptied.
    bit_container: u64,
}

impl<'s> BitReaderReversed<'s> {
    /// How many bits are left to read by the reader.
    pub fn bits_remaining(&self) -> isize {
        self.index as isize * 8 + (64 - self.bits_consumed as isize) - self.extra_bits as isize
    }

    pub fn new(source: &'s [u8]) -> BitReaderReversed<'s> {
        BitReaderReversed {
            index: source.len(),
            bits_consumed: 64,
            source,
            bit_container: 0,
            extra_bits: 0,
        }
    }

    /// Construct a reader from an explicit window state.
    ///
    /// pub(crate) contract (callers must uphold):
    /// `index + 8 <= source.len()`, and the top `bits_consumed` bits of the
    /// little-endian u64 at `source[index..]` are the bits the caller already consumed.
    pub(crate) fn from_parts(
        source: &'s [u8],
        index: usize,
        bits_consumed: u8,
        extra_bits: usize,
    ) -> BitReaderReversed<'s> {
        debug_assert!(index + 8 <= source.len());
        let bit_container = u64::from_le_bytes((&source[index..][..8]).try_into().unwrap());
        BitReaderReversed {
            index,
            bits_consumed,
            source,
            bit_container,
            extra_bits,
        }
    }

    /// We refill the container in full bytes, shifting the still unread portion to the left, and filling the lower bits with new data
    #[cold]
    fn refill(&mut self) {
        let bytes_consumed = self.bits_consumed as usize / 8;
        if bytes_consumed == 0 {
            return;
        }

        if self.index >= bytes_consumed {
            // We can safely move the window contained in `bit_container` down by `bytes_consumed`
            // If the reader wasn't byte aligned, the byte that was partially read is now in the highest order bits in the `bit_container`
            self.index -= bytes_consumed;
            // Some bits of the `bits_container` might have been consumed already because we read the window byte aligned
            self.bits_consumed &= 7;
            self.bit_container =
                u64::from_le_bytes((&self.source[self.index..][..8]).try_into().unwrap());
        } else if self.index > 0 {
            // Read the last portion of source into the `bit_container`
            if self.source.len() >= 8 {
                self.bit_container = u64::from_le_bytes((&self.source[..8]).try_into().unwrap());
            } else {
                let mut value = [0; 8];
                value[..self.source.len()].copy_from_slice(self.source);
                self.bit_container = u64::from_le_bytes(value);
            }

            self.bits_consumed -= 8 * self.index as u8;
            self.index = 0;

            self.bit_container <<= self.bits_consumed;
            self.extra_bits += self.bits_consumed as usize;
            self.bits_consumed = 0;
        } else if self.bits_consumed < 64 {
            // Shift out already used bits and fill up with zeroes
            self.bit_container <<= self.bits_consumed;
            self.extra_bits += self.bits_consumed as usize;
            self.bits_consumed = 0;
        } else {
            // All useful bits have already been read and more than 64 bits have been consumed, all we now do is return zeroes
            self.extra_bits += self.bits_consumed as usize;
            self.bits_consumed = 0;
            self.bit_container = 0;
        }

        // Assert that at least `56 = 64 - 8` bits are available to read.
        debug_assert!(self.bits_consumed < 8);
    }

    /// Read `n` number of bits from the source. Will read at most 56 bits.
    /// If there are no more bits to be read from the source zero bits will be returned instead.
    #[inline(always)]
    pub fn get_bits(&mut self, n: u8) -> u64 {
        if self.bits_consumed + n > 64 {
            self.refill();
        }

        let value = self.peek_bits(n);
        self.consume(n);
        value
    }

    /// Get the next `n` bits from the source without consuming them.
    /// Caller is responsible for making sure that `n` many bits have been refilled.
    #[inline(always)]
    pub fn peek_bits(&mut self, n: u8) -> u64 {
        if n == 0 {
            return 0;
        }

        let mask = (1u64 << n) - 1u64;
        let shift_by = 64 - self.bits_consumed - n;
        (self.bit_container >> shift_by) & mask
    }

    /// Like `peek_bits` but refills the container first, so the peek is always
    /// backed by fresh stream bits (padding zeroes past the end of the stream).
    #[cfg(any(test, feature = "fuzz_exports"))]
    #[inline(always)]
    pub fn peek_bits_refilled(&mut self, n: u8) -> u64 {
        if self.bits_consumed + n > 64 {
            self.refill();
        }
        self.peek_bits(n)
    }

    /// Consume `n` bits from the source.
    #[inline(always)]
    pub fn consume(&mut self, n: u8) {
        self.bits_consumed += n;
        debug_assert!(self.bits_consumed <= 64);
    }
}

#[cfg(test)]
mod test {

    #[test]
    fn it_works() {
        let data = [0b10101010, 0b01010101];
        let mut br = super::BitReaderReversed::new(&data);
        assert_eq!(br.get_bits(1), 0);
        assert_eq!(br.get_bits(1), 1);
        assert_eq!(br.get_bits(1), 0);
        assert_eq!(br.get_bits(4), 0b1010);
        assert_eq!(br.get_bits(4), 0b1101);
        assert_eq!(br.get_bits(4), 0b0101);
        // Last 0 from source, three zeroes filled in
        assert_eq!(br.get_bits(4), 0b0000);
        // All zeroes filled in
        assert_eq!(br.get_bits(4), 0b0000);
        assert_eq!(br.bits_remaining(), -7);
    }
}
