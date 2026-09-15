//! Decoder-side sequence recording for differential parse analysis
//! (`seq_dump` dev feature): every sequence the decoder emits is appended to
//! a thread-local log, so a reference frame (e.g. libzstd output) can be
//! turned into (ll, ml, of-wire) triples for comparison against our encoder's
//! parse. Compiled out entirely unless the feature is on.

use alloc::vec::Vec;
use std::cell::RefCell;

/// One decoded sequence, wire values (`of` as stored in the frame).
#[derive(Clone, Copy, Debug)]
pub struct DumpedSeq {
    pub ll: u32,
    pub ml: u32,
    pub of: u32,
}

// Ground-truth match execution on the flat path: (match_pos, ml, offset)
// per executed sequence. Cross-checks the wire-dump resolution in the
// analysis tools.
std::thread_local! {
    static EXEC: RefCell<Vec<(u64, u32, u64)>> = const { RefCell::new(Vec::new()) };
}

std::thread_local! {
    static LOG: RefCell<Vec<DumpedSeq>> = const { RefCell::new(Vec::new()) };
}

/// Record hook called from `decode_step`; one call per decoded sequence.
pub(crate) fn record(ll: u32, ml: u32, of: u32) {
    LOG.with_borrow_mut(|log| log.push(DumpedSeq { ll, ml, of }));
}

/// Record hook called once per executed match on the flat decode path.
#[cfg(feature = "seq_dump")]
pub(crate) fn record_exec(match_pos: u64, ml: u32, offset: u64) {
    EXEC.with_borrow_mut(|e| e.push((match_pos, ml, offset)));
}

/// Take the recorded sequences, clearing the log.
pub fn take() -> Vec<DumpedSeq> {
    LOG.with_borrow_mut(std::mem::take)
}

/// Take the recorded executed matches, clearing the log.
pub fn take_exec() -> Vec<(u64, u32, u64)> {
    EXEC.with_borrow_mut(std::mem::take)
}
