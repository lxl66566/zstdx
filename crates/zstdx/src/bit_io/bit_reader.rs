/// Wraps a slice and enables reading arbitrary amounts of bits
/// from that slice.
pub struct BitReader<'s> {
    idx: usize, // index counts bits already read
    source: &'s [u8],
}

impl<'s> BitReader<'s> {
    pub fn new(source: &'s [u8]) -> BitReader<'s> {
        BitReader { idx: 0, source }
    }

    pub fn bits_left(&self) -> usize {
        self.source.len() * 8 - self.idx
    }

    pub fn bits_read(&self) -> usize {
        self.idx
    }

    pub fn return_bits(&mut self, n: usize) {
        assert!(n <= self.idx, "Cant return this many bits");
        self.idx -= n;
    }

    pub fn get_bits(&mut self, n: usize) -> Result<u64, GetBitsError> {
        if n > 64 {
            return Err(GetBitsError::TooManyBits {
                num_requested_bits: n,
                limit: 64,
            });
        }
        if self.bits_left() < n {
            return Err(GetBitsError::NotEnoughRemainingBits {
                requested: n,
                remaining: self.bits_left(),
            });
        }

        let old_idx = self.idx;

        let bits_left_in_current_byte = 8 - (self.idx % 8);
        let bits_not_needed_in_current_byte = 8 - bits_left_in_current_byte;

        // collect bits from the currently pointed to byte
        let mut value = u64::from(self.source[self.idx / 8] >> bits_not_needed_in_current_byte);

        if bits_left_in_current_byte >= n {
            // no need for fancy stuff

            // just mask all but the needed n bit
            value &= (1 << n) - 1;
            self.idx += n;
        } else {
            self.idx += bits_left_in_current_byte;

            // n spans over multiple bytes
            let full_bytes_needed = (n - bits_left_in_current_byte) / 8;
            let bits_in_last_byte_needed = n - bits_left_in_current_byte - full_bytes_needed * 8;

            assert_eq!(
                bits_left_in_current_byte + full_bytes_needed * 8 + bits_in_last_byte_needed,
                n
            );

            let mut bit_shift = bits_left_in_current_byte; //this many bits are already set in value

            assert!(self.idx.is_multiple_of(8));

            // collect full bytes
            for _ in 0..full_bytes_needed {
                value |= u64::from(self.source[self.idx / 8]) << bit_shift;
                self.idx += 8;
                bit_shift += 8;
            }

            assert_eq!(n - bit_shift, bits_in_last_byte_needed);

            if bits_in_last_byte_needed > 0 {
                let val_las_byte =
                    u64::from(self.source[self.idx / 8]) & ((1 << bits_in_last_byte_needed) - 1);
                value |= val_las_byte << bit_shift;
                self.idx += bits_in_last_byte_needed;
            }
        }

        assert_eq!(self.idx, old_idx + n);

        Ok(value)
    }

    /// Read `n` bits, tolerating up to `slack` missing bits past the end of
    /// the source by serving them as zeros: the index moves past the end by
    /// the served count, and a `return_bits` of exactly that count restores
    /// it, so a short-form read-then-unread pair stays within `bits_read`'s
    /// source bounds. This mirrors libzstd's zero-padded NCount read window,
    /// where the final symbol's short form legally looks one bit past the
    /// last byte. Errors when more than `slack` bits would be missing.
    pub fn get_bits_zero_padded(&mut self, n: usize, slack: usize) -> Result<u64, GetBitsError> {
        let total = self.source.len() * 8;
        if self.idx >= total {
            return Err(GetBitsError::NotEnoughRemainingBits {
                requested: n,
                remaining: 0,
            });
        }
        let remaining = total - self.idx;
        if remaining >= n {
            return self.get_bits(n);
        }
        if n - remaining > slack {
            return Err(GetBitsError::NotEnoughRemainingBits {
                requested: n,
                remaining,
            });
        }
        let value = self.get_bits(remaining)?;
        self.idx += n - remaining;
        Ok(value)
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum GetBitsError {
    TooManyBits {
        num_requested_bits: usize,
        limit: u8,
    },
    NotEnoughRemainingBits {
        requested: usize,
        remaining: usize,
    },
}

// core::error::Error since 1.81; std re-exports the same trait, so one
// unconditional impl serves both builds (the derive treats GetBitsError as
// a source, which requires the Error trait even under no_std).
impl core::error::Error for GetBitsError {}

impl core::fmt::Display for GetBitsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GetBitsError::TooManyBits {
                num_requested_bits,
                limit,
            } => {
                write!(
                    f,
                    "Cant serve this request. The reader is limited to {limit} bits, requested \
                     {num_requested_bits} bits"
                )
            },
            GetBitsError::NotEnoughRemainingBits {
                requested,
                remaining,
            } => {
                write!(
                    f,
                    "Can\'t read {requested} bits, only have {remaining} bits left"
                )
            },
        }
    }
}
