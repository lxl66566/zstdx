//! Overshoot stores into a byte buffer's reserved tail.
//!
//! The block writers reserve the whole store budget up front and emit
//! unaligned words that may overrun the published length; the final length
//! is published once the semantic end is known. The covering reserve is
//! established once per block from the worst-case bound, so the per-store
//! helpers only assert it in debug builds and add nothing in release.

use alloc::vec::Vec;

/// Stores `val` unaligned at byte offset `end`.
///
/// # Safety
/// The buffer's capacity must cover `end + 8`. `end` addresses bytes that
/// are reserved for this store; a later [`publish_len`] or store must
/// overwrite or cut off the overrun.
#[inline(always)]
pub(crate) unsafe fn store_u64(buf: &mut Vec<u8>, end: usize, val: u64) {
    debug_assert!(end + 8 <= buf.capacity());
    // SAFETY: the capacity contract covers the unaligned 8-byte store.
    unsafe {
        buf.as_mut_ptr()
            .add(end)
            .cast::<u64>()
            .write_unaligned(val.to_le());
    }
}

/// Publishes the semantic end after overrun stores or batched writes.
///
/// # Safety
/// `end` must not exceed the capacity, and every byte below `end` must be
/// initialized by a store or an append.
#[inline(always)]
pub(crate) unsafe fn publish_len(buf: &mut Vec<u8>, end: usize) {
    debug_assert!(end <= buf.capacity());
    // SAFETY: the contract above is the caller's.
    unsafe { buf.set_len(end) }
}
