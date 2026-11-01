//! Encoding agnostic ways to read and write binary data

mod bit_reader;
mod bit_reader_reverse;
mod bit_writer;

pub(crate) use bit_reader::*;
pub(crate) use bit_reader_reverse::*;
pub(crate) use bit_writer::*;

/// Little-endian unaligned word load. The backward bitstreams (Huffman
/// streams, FSE sequences) and the checksums consume from the word's low
/// byte first, so the byte order is part of the format, not the target's;
/// on little-endian targets this is the bare load.
// SAFETY: callers bound the 8-byte read.
#[inline(always)]
pub(crate) unsafe fn load_le64(p: *const u8) -> u64 {
    // SAFETY: the caller's bound covers this unaligned read.
    let v = unsafe { p.cast::<u64>().read_unaligned() };
    #[cfg(target_endian = "big")]
    let v = v.swap_bytes();
    v
}

/// Little-endian 2-byte store: the packed symbol pairs (and other
/// multi-symbol words) lay their first symbol at the low address.
// SAFETY: callers bound the 2-byte write.
#[inline(always)]
pub(crate) unsafe fn store_le16(p: *mut u8, v: u16) {
    #[cfg(target_endian = "big")]
    let v = v.swap_bytes();
    // SAFETY: the caller's bound covers this unaligned write.
    unsafe { p.cast::<u16>().write_unaligned(v) }
}

/// Little-endian unaligned 4-byte load. The matcher hashes are defined
/// over the byte sequence the way libzstd's `MEM_readLE32` defines them,
/// so the byte order is part of the value, not the target's.
// SAFETY: callers bound the 4-byte read.
#[inline(always)]
pub(crate) unsafe fn load_le32(p: *const u8) -> u32 {
    // SAFETY: the caller's bound covers this unaligned read.
    let v = unsafe { p.cast::<u32>().read_unaligned() };
    #[cfg(target_endian = "big")]
    let v = v.swap_bytes();
    v
}
