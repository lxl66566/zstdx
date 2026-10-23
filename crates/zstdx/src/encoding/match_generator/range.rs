//! Wrap-proof spans between two same-domain cursors.
//!
//! The matcher's window arithmetic pairs absolute cursors (scan position,
//! block/segment end, insert bound, split) whose ordering is established
//! far from the subtraction that consumes it — a clamped bound can
//! legitimately sit below the cursor it is subtracted from (the R68
//! emit/rep-tail class: debug panics on the overflow, release wraps huge
//! and walks off the window). [`IdxRange`] makes the pairing explicit:
//! construction from an already-validated pair (debug-asserted ordered)
//! or with the `IdxRange::new(lo, hi.max(lo))` clamp turns the wrap into
//! an empty span instead. Construction is two u64 moves, so a per-match
//! or per-block use is free; the per-byte scan loops keep their naked
//! arithmetic under their loop-bar invariants (each bar holds
//! `cursor <= end - margin` for every subtraction the body performs).

use super::HASH_READ;

/// The half-open span `[start, end)` between two same-domain cursors.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdxRange {
    start: u64,
    end: u64,
}

impl IdxRange {
    /// The span `[start, end)`; the pair must already be ordered (the
    /// debug assert fires at the producer, not three frames later in a
    /// wrapped index).
    #[inline(always)]
    pub(crate) fn new(start: u64, end: u64) -> Self {
        debug_assert!(
            start <= end,
            "IdxRange constructed inverted: {start} > {end}"
        );
        Self { start, end }
    }

    /// Span length.
    #[inline(always)]
    pub(crate) fn len(&self) -> u64 {
        self.end - self.start
    }

    /// Last absolute position whose [`HASH_READ`]-byte hash read ends at
    /// or before the span end, floored at the span start: the insert bound
    /// every fill path shares. A span shorter than the read has no
    /// insertable position, and the scans of such spans stop before any
    /// emit consults the bound, so the floor is never a live value there.
    #[inline(always)]
    pub(crate) fn insert_max(&self) -> u64 {
        self.end.saturating_sub(HASH_READ as u64).max(self.start)
    }
}
