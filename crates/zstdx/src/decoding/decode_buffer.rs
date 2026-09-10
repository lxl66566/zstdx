use alloc::vec::Vec;

use super::ringbuffer::RingBuffer;
#[cfg(feature = "hash")]
use crate::xxh64::Xxh64;
use crate::{
    decoding::errors::DecodeBufferError,
    io::{Error, Read, Write},
};

pub struct DecodeBuffer {
    buffer: RingBuffer,
    pub dict_content: Vec<u8>,

    pub window_size: usize,
    total_output_counter: u64,
    /// Only track the xxhash when the frame actually carries a checksum;
    /// hashing every drained byte otherwise is pure overhead.
    #[cfg(feature = "hash")]
    hash_enabled: bool,
    #[cfg(feature = "hash")]
    pub(crate) hash: Xxh64,
}

impl Read for DecodeBuffer {
    fn read(&mut self, target: &mut [u8]) -> Result<usize, Error> {
        let max_amount = self.can_drain_to_window_size().unwrap_or(0);
        let amount = max_amount.min(target.len());

        let mut written = 0;
        self.drain_to(amount, |buf| {
            target[written..][..buf.len()].copy_from_slice(buf);
            written += buf.len();
            (buf.len(), Ok(()))
        })?;
        Ok(amount)
    }
}

impl DecodeBuffer {
    pub fn new(window_size: usize) -> DecodeBuffer {
        DecodeBuffer {
            buffer: RingBuffer::new(),
            dict_content: Vec::new(),
            window_size,
            total_output_counter: 0,
            #[cfg(feature = "hash")]
            hash_enabled: false,
            #[cfg(feature = "hash")]
            hash: Xxh64::new(0),
        }
    }

    pub fn reset(&mut self, window_size: usize) {
        self.window_size = window_size;
        self.buffer.clear();
        self.buffer.reserve(self.window_size);
        self.dict_content.clear();
        self.total_output_counter = 0;
        #[cfg(feature = "hash")]
        {
            self.hash_enabled = false;
            self.hash = Xxh64::new(0);
        }
    }

    /// Enable incremental checksum computation over all drained bytes.
    #[cfg(feature = "hash")]
    pub fn set_checksum_enabled(&mut self, on: bool) {
        self.hash_enabled = on;
    }

    /// Fold already-delivered bytes into the checksum; used by the flat
    /// output path, which bypasses the drain that normally hashes.
    #[cfg(feature = "hash")]
    pub fn hash_bytes(&mut self, bytes: &[u8]) {
        if self.hash_enabled {
            self.hash.write(bytes);
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Ensure space for `amount` more bytes; used to reserve a whole block
    /// up front so per-sequence appends can skip capacity checks.
    pub fn reserve(&mut self, amount: usize) {
        self.buffer.reserve(amount);
    }

    pub fn extend_and_fill(&mut self, fill_with: u8, fill_length: usize) {
        self.buffer.extend_and_fill(fill_with, fill_length);
    }

    pub fn extend_from_reader<R: Read>(
        &mut self,
        read: R,
        fill_length: usize,
    ) -> Result<(), Error> {
        self.buffer.extend_from_reader(read, fill_length)
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend(data);
        self.total_output_counter += data.len() as u64;
    }

    /// Like `push` but for buffers whose capacity was reserved up front for
    /// the whole block (see `execute_sequences`); skips the free-space check.
    #[inline]
    pub fn push_pre_reserved(&mut self, data: &[u8]) {
        debug_assert!(self.buffer.free() >= data.len());
        // SAFETY: the block-level reserve covers this append.
        unsafe { self.buffer.extend_pre_reserved(data) };
        self.total_output_counter += data.len() as u64;
    }

    pub fn repeat(&mut self, offset: usize, match_length: usize) -> Result<(), DecodeBufferError> {
        self.buffer.reserve(match_length);
        self.repeat_pre_reserved(offset, match_length)
    }

    /// Like `repeat` but for buffers whose capacity was reserved up front for
    /// the whole block (see `execute_sequences`); skips the free-space check.
    #[inline]
    pub fn repeat_pre_reserved(
        &mut self,
        offset: usize,
        match_length: usize,
    ) -> Result<(), DecodeBufferError> {
        if offset > self.buffer.len() {
            self.repeat_from_dict(offset, match_length)
        } else {
            let buf_len = self.buffer.len();
            let start_idx = buf_len - offset;
            let end_idx = start_idx + match_length;

            debug_assert!(self.buffer.free() >= match_length);
            if end_idx > buf_len {
                // We need to copy in chunks.
                self.repeat_in_chunks(offset, match_length, start_idx);
            } else {
                // can just copy parts of the existing buffer
                // SAFETY: Requirements checked:
                // 1. start_idx + match_length must be <= self.buffer.len() We know that:
                //      1. start_idx = self.buffer.len() - offset
                //      2. end_idx = start_idx + match_length
                //      3. end_idx <= self.buffer.len()
                //      Thus follows: start_idx + match_length <= self.buffer.len()
                //
                // 2. explicitly reserved enough memory for the whole match_length
                unsafe {
                    self.buffer
                        .extend_from_within_unchecked(start_idx, match_length);
                };
            }

            self.total_output_counter += match_length as u64;
            Ok(())
        }
    }

    fn repeat_in_chunks(&mut self, offset: usize, match_length: usize, start_idx: usize) {
        // Copy in exponentially growing chunks anchored at `start_idx`. After `copied` bytes
        // have been appended, `offset + copied` bytes are readable from `start_idx`, and each
        // chunk reproduces the periodic pattern exactly because dst[i] = src[i] with
        // dst start = start_idx + offset + copied >= src end. This needs log2(match_length /
        // offset) large memcpys instead of match_length / offset single-offset chunks, which
        // matters a lot for small offsets (e.g. offset=1 used to copy byte by byte).
        let mut copied = 0;
        while copied < match_length {
            // SAFETY: Requirements checked:
            // 1. start_idx + chunk must be <= self.buffer.len() chunk <= offset + copied, and
            //    buffer.len() == start_idx + offset + copied at this point (start of the first
            //    iteration has copied == 0 and buffer.len() == start_idx + offset, and both sides
            //    grow by the same amount each iteration). Thus start_idx + chunk <= buffer.len().
            //
            // 2. explicitly reserved enough memory for the whole match_length
            let chunk = usize::min(offset + copied, match_length - copied);
            unsafe { self.buffer.extend_from_within_unchecked(start_idx, chunk) };
            copied += chunk;
        }
    }

    #[cold]
    fn repeat_from_dict(
        &mut self,
        offset: usize,
        match_length: usize,
    ) -> Result<(), DecodeBufferError> {
        if self.total_output_counter <= self.window_size as u64 {
            // at least part of that repeat is from the dictionary content
            let bytes_from_dict = offset - self.buffer.len();

            if bytes_from_dict > self.dict_content.len() {
                return Err(DecodeBufferError::NotEnoughBytesInDictionary {
                    got: self.dict_content.len(),
                    need: bytes_from_dict,
                });
            }

            if bytes_from_dict < match_length {
                let dict_slice = &self.dict_content[self.dict_content.len() - bytes_from_dict..];
                self.buffer.extend(dict_slice);

                self.total_output_counter += bytes_from_dict as u64;
                return self.repeat(self.buffer.len(), match_length - bytes_from_dict);
            }
            let low = self.dict_content.len() - bytes_from_dict;
            let high = low + match_length;
            let dict_slice = &self.dict_content[low..high];
            self.buffer.extend(dict_slice);
            Ok(())
        } else {
            Err(DecodeBufferError::OffsetTooBig {
                offset,
                buf_len: self.buffer.len(),
            })
        }
    }

    /// Check if and how many bytes can currently be drawn from the buffer
    pub fn can_drain_to_window_size(&self) -> Option<usize> {
        if self.buffer.len() > self.window_size {
            Some(self.buffer.len() - self.window_size)
        } else {
            None
        }
    }

    // How many bytes can be drained if the window_size does not have to be maintained
    pub fn can_drain(&self) -> usize {
        self.buffer.len()
    }

    /// Drain as much as possible while retaining enough so that decoding si still possible with the
    /// required window_size At best call only if can_drain_to_window_size reports a 'high'
    /// number of bytes to reduce allocations
    pub fn drain_to_window_size(&mut self) -> Option<Vec<u8>> {
        // TODO investigate if it is possible to return the std::vec::Drain iterator directly
        // without collecting here
        match self.can_drain_to_window_size() {
            None => None,
            Some(can_drain) => {
                let mut vec = Vec::with_capacity(can_drain);
                self.drain_to(can_drain, |buf| {
                    vec.extend_from_slice(buf);
                    (buf.len(), Ok(()))
                })
                .ok()?;
                Some(vec)
            },
        }
    }

    pub fn drain_to_window_size_writer(&mut self, mut sink: impl Write) -> Result<usize, Error> {
        match self.can_drain_to_window_size() {
            None => Ok(0),
            Some(can_drain) => self.drain_to(can_drain, |buf| write_all_bytes(&mut sink, buf)),
        }
    }

    /// drain the buffer completely
    pub fn drain(&mut self) -> Vec<u8> {
        let (slice1, slice2) = self.buffer.as_slices();
        #[cfg(feature = "hash")]
        if self.hash_enabled {
            self.hash.write(slice1);
            self.hash.write(slice2);
        }

        let mut vec = Vec::with_capacity(slice1.len() + slice2.len());
        vec.extend_from_slice(slice1);
        vec.extend_from_slice(slice2);
        self.buffer.clear();
        vec
    }

    pub fn drain_to_writer(&mut self, mut sink: impl Write) -> Result<usize, Error> {
        let write_limit = self.buffer.len();
        self.drain_to(write_limit, |buf| write_all_bytes(&mut sink, buf))
    }

    pub fn read_all(&mut self, target: &mut [u8]) -> Result<usize, Error> {
        let amount = self.buffer.len().min(target.len());

        let mut written = 0;
        self.drain_to(amount, |buf| {
            target[written..][..buf.len()].copy_from_slice(buf);
            written += buf.len();
            (buf.len(), Ok(()))
        })?;
        Ok(amount)
    }

    /// Semantics of write_bytes:
    /// Should dump as many of the provided bytes as possible to whatever sink until no bytes are
    /// left or an error is encountered Return how many bytes have actually been dumped to the
    /// sink.
    fn drain_to(
        &mut self,
        amount: usize,
        mut write_bytes: impl FnMut(&[u8]) -> (usize, Result<(), Error>),
    ) -> Result<usize, Error> {
        struct DrainGuard<'a> {
            buffer: &'a mut RingBuffer,
            amount: usize,
        }

        impl Drop for DrainGuard<'_> {
            fn drop(&mut self) {
                if self.amount != 0 {
                    self.buffer.drop_first_n(self.amount);
                }
            }
        }

        if amount == 0 {
            return Ok(0);
        }

        let mut drain_guard = DrainGuard {
            buffer: &mut self.buffer,
            amount: 0,
        };

        let (slice1, slice2) = drain_guard.buffer.as_slices();
        let n1 = slice1.len().min(amount);
        let n2 = slice2.len().min(amount - n1);

        if n1 != 0 {
            let (written1, res1) = write_bytes(&slice1[..n1]);
            #[cfg(feature = "hash")]
            if self.hash_enabled {
                self.hash.write(&slice1[..written1]);
            }
            drain_guard.amount += written1;

            // Apparently this is what clippy thinks is the best way of expressing this
            res1?;

            // Only if the first call to write_bytes was not a partial write we can continue with
            // slice2 Partial writes SHOULD never happen without res1 being an error,
            // but lets just protect against it anyways.
            if written1 == n1 && n2 != 0 {
                let (written2, res2) = write_bytes(&slice2[..n2]);
                #[cfg(feature = "hash")]
                if self.hash_enabled {
                    self.hash.write(&slice2[..written2]);
                }
                drain_guard.amount += written2;

                // Apparently this is what clippy thinks is the best way of expressing this
                res2?;
            }
        }

        let amount_written = drain_guard.amount;
        // Make sure we don't accidentally drop `DrainGuard` earlier.
        drop(drain_guard);

        Ok(amount_written)
    }
}

/// Like Write::write_all but returns partial write length even on error
fn write_all_bytes(mut sink: impl Write, buf: &[u8]) -> (usize, Result<(), Error>) {
    let mut written = 0;
    while written < buf.len() {
        match sink.write(&buf[written..]) {
            Ok(0) => return (written, Ok(())),
            Ok(w) => written += w,
            Err(e) => return (written, Err(e)),
        }
    }
    (written, Ok(()))
}

#[cfg(test)]
mod tests {
    use super::DecodeBuffer;
    use crate::io::{Error, ErrorKind, Write};

    extern crate std;
    use alloc::{vec, vec::Vec};

    #[test]
    fn short_writer() {
        struct ShortWriter {
            buf: Vec<u8>,
            write_len: usize,
        }

        impl Write for ShortWriter {
            fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
                if buf.len() > self.write_len {
                    self.buf.extend_from_slice(&buf[..self.write_len]);
                    Ok(self.write_len)
                } else {
                    self.buf.extend_from_slice(buf);
                    Ok(buf.len())
                }
            }

            fn flush(&mut self) -> Result<(), Error> {
                Ok(())
            }
        }

        let mut short_writer = ShortWriter {
            buf: vec![],
            write_len: 10,
        };

        let mut decode_buf = DecodeBuffer::new(100);
        decode_buf.push(b"0123456789");
        decode_buf.repeat(10, 90).unwrap();
        let repeats = 1000;
        for _ in 0..repeats {
            assert_eq!(decode_buf.len(), 100);
            decode_buf.repeat(10, 50).unwrap();
            assert_eq!(decode_buf.len(), 150);
            decode_buf
                .drain_to_window_size_writer(&mut short_writer)
                .unwrap();
            assert_eq!(decode_buf.len(), 100);
        }

        assert_eq!(short_writer.buf.len(), repeats * 50);
        decode_buf.drain_to_writer(&mut short_writer).unwrap();
        assert_eq!(short_writer.buf.len(), repeats * 50 + 100);
    }

    #[test]
    fn wouldblock_writer() {
        struct WouldblockWriter {
            buf: Vec<u8>,
            last_blocked: usize,
            block_every: usize,
        }

        impl Write for WouldblockWriter {
            fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
                if self.last_blocked < self.block_every {
                    self.buf.extend_from_slice(buf);
                    self.last_blocked += 1;
                    Ok(buf.len())
                } else {
                    self.last_blocked = 0;
                    Err(Error::from(ErrorKind::WouldBlock))
                }
            }

            fn flush(&mut self) -> Result<(), Error> {
                Ok(())
            }
        }

        let mut short_writer = WouldblockWriter {
            buf: vec![],
            last_blocked: 0,
            block_every: 5,
        };

        let mut decode_buf = DecodeBuffer::new(100);
        decode_buf.push(b"0123456789");
        decode_buf.repeat(10, 90).unwrap();
        let repeats = 1000;
        for _ in 0..repeats {
            assert_eq!(decode_buf.len(), 100);
            decode_buf.repeat(10, 50).unwrap();
            assert_eq!(decode_buf.len(), 150);
            loop {
                match decode_buf.drain_to_window_size_writer(&mut short_writer) {
                    Ok(written) => {
                        if written == 0 {
                            break;
                        }
                    },
                    Err(e) => {
                        assert!(e.kind() == ErrorKind::WouldBlock, "Unexpected error {e:?}");
                    },
                }
            }
            assert_eq!(decode_buf.len(), 100);
        }

        assert_eq!(short_writer.buf.len(), repeats * 50);
        loop {
            match decode_buf.drain_to_writer(&mut short_writer) {
                Ok(written) => {
                    if written == 0 {
                        break;
                    }
                },
                Err(e) => {
                    assert!(e.kind() == ErrorKind::WouldBlock, "Unexpected error {e:?}");
                },
            }
        }
        assert_eq!(short_writer.buf.len(), repeats * 50 + 100);
    }
}
