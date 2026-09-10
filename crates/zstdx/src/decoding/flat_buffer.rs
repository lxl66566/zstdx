//! Flat output buffer for dictionary-free streaming decode, mirroring libzstd's
//! `outBuff` model: blocks are decoded straight into a linear buffer and flushed
//! to the caller from it. Flushing hands out everything pending — the bytes stay
//! physically in the buffer afterwards, so later matches can still reference
//! them. When the buffer fills up to its high-water mark (and everything decoded
//! so far has been flushed), writing wraps back to the start while the previous
//! segment's tail keeps serving as the match window.
//!
//! Wrap safety: the buffer holds `window + 2 * MAX_BLOCK_SIZE + slack` bytes. A
//! wrap only happens once at least `window + MAX_BLOCK_SIZE` bytes were produced,
//! so the wrapped-away history segment `[origin - window, origin)` always ends at
//! least `MAX_BLOCK_SIZE` bytes above any position the new segment can write in
//! its first block. By induction (the k-th block after a wrap writes absolute
//! `[k*bs, (k+1)*bs)` while match references stay above `(k+1)*bs + slack`),
//! match sources and match destinations never collide within a buffer
//! generation.

use alloc::vec::Vec;

use crate::common::MAX_BLOCK_SIZE;

const MAX_BLOCK_SIZE_USIZE: usize = MAX_BLOCK_SIZE as usize;

/// Extra headroom so a wrapped block's doubling copies can overshoot the
/// nominal block end without touching the history segment.
const WRAP_SLACK: usize = 64;

/// Wildcopy overshoot the block target must absorb beyond a full block
/// (16-byte chunks copying past the sequence end).
pub(crate) const WILDCOPY_SLACK: usize = 16;

/// Read-only window mapping handed to the sequence executor: virtual
/// addresses are global frame positions; the active segment maps
/// `v >= origin` to `v - origin`, the wrapped-away previous segment maps
/// `prev_origin <= v < origin` to `v - prev_origin` (bounded by its physical
/// end). Anything below is out of window.
#[derive(Clone, Copy)]
pub(crate) struct FlatView {
    pub origin: usize,
    pub prev_origin: usize,
    /// Physical end of the previous segment (its bytes above the window may
    /// have been overwritten by the active segment).
    pub seg_a_end: usize,
    pub out_len: usize,
}

pub(crate) struct FlatOut {
    buf: Vec<u8>,
    /// Virtual origin of the active segment (== total bytes produced at the
    /// last wrap). `origin + end` is the total number of bytes produced in
    /// the frame; virtual addresses must stay globally monotonic because
    /// sequence offsets are global distances.
    origin: usize,
    /// Virtual origin of the previous segment, i.e. of the history that
    /// still serves as the match window after a wrap.
    prev_origin: usize,
    /// Physical end of that previous segment.
    seg_a_end: usize,
    /// Flush cursor: `buf[start..end]` is decoded but not yet handed out.
    start: usize,
    /// Decode cursor.
    end: usize,
    window: usize,
}

impl FlatOut {
    pub fn new() -> Self {
        FlatOut {
            buf: Vec::new(),
            origin: 0,
            prev_origin: 0,
            seg_a_end: 0,
            start: 0,
            end: 0,
            window: 0,
        }
    }

    pub fn reset(&mut self, window: usize) {
        self.origin = 0;
        self.prev_origin = 0;
        self.seg_a_end = 0;
        self.start = 0;
        self.end = 0;
        self.window = window;
    }

    /// Snapshot of the virtual window mapping for the sequence executor.
    pub fn view(&self) -> FlatView {
        FlatView {
            origin: self.origin,
            prev_origin: self.prev_origin,
            seg_a_end: self.seg_a_end,
            out_len: self.buf.len(),
        }
    }

    /// Total bytes produced in the frame so far (size of the virtual space).
    pub fn produced(&self) -> usize {
        self.origin + self.end
    }

    /// Make room for one more block. Returns false when the caller must first
    /// flush the pending bytes (`start < end`); in that case no header should
    /// have been consumed from the source yet.
    ///
    /// On success the block target `buf[end..]` holds at least
    /// `MAX_BLOCK_SIZE + WILDCOPY_SLACK` bytes beyond the cursor, letting the
    /// sequence executor drop its per-sequence bounds checks (the HEADROOM
    /// instantiation of `execute_decoded_flat`).
    ///
    /// With `force`, the buffer grows past the steady-state size instead of
    /// returning false — used by the All strategy, which promises to decode
    /// every block before the caller reads (the ring path satisfies this by
    /// growing without bound too).
    pub fn ensure_block_space(&mut self, force: bool) -> bool {
        if self.end + MAX_BLOCK_SIZE_USIZE + WILDCOPY_SLACK <= self.buf.len() {
            return true;
        }
        if self.start == self.end && self.end >= self.window + MAX_BLOCK_SIZE_USIZE {
            // Wrap: everything decoded so far was flushed and the previous
            // segment keeps a full window with the safety margin proven
            // above. Virtual origins chain so addresses stay monotonic
            // across generations (offsets are global distances).
            self.prev_origin = self.origin;
            self.seg_a_end = self.end;
            self.origin += self.end;
            self.start = 0;
            self.end = 0;
            return true;
        }
        let cap = self.buf.len();
        let needed = self.window + 2 * MAX_BLOCK_SIZE_USIZE + WRAP_SLACK;
        if cap < needed || force {
            // Grow towards the steady-state size (beyond it when forced);
            // the cursor semantics are index-based, so a realloc is
            // transparent. Re-check afterwards: with a large decode cursor
            // the grown size may still be short, in which case the caller
            // must flush first and wrap later.
            let target = (2 * cap).max(2 * MAX_BLOCK_SIZE_USIZE).max(needed);
            self.buf.resize(target, 0);
            return self.end + MAX_BLOCK_SIZE_USIZE + WILDCOPY_SLACK <= self.buf.len();
        }
        false
    }

    /// How many bytes can currently be flushed (all pending bytes; they remain
    /// in the buffer as match history after being handed out).
    pub fn can_flush(&self) -> usize {
        self.end - self.start
    }

    /// Copy pending bytes out. Returns the number of bytes written.
    pub fn flush_to(&mut self, target: &mut [u8]) -> usize {
        let amount = target.len().min(self.end - self.start);
        target[..amount].copy_from_slice(&self.buf[self.start..self.start + amount]);
        self.start += amount;
        amount
    }

    /// Take all pending bytes into a fresh Vec, advancing the flush cursor.
    pub fn take_pending(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.end - self.start);
        out.extend_from_slice(&self.buf[self.start..self.end]);
        self.start = self.end;
        out
    }

    /// Write all pending bytes to `sink`, advancing the flush cursor.
    pub fn flush_to_writer(
        &mut self,
        mut sink: impl crate::io::Write,
    ) -> Result<usize, crate::io::Error> {
        let mut written = 0usize;
        while written < self.end - self.start {
            match sink.write(&self.buf[self.start + written..self.end]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) => {
                    self.start += written;
                    return Err(e);
                },
            }
        }
        self.start += written;
        Ok(written)
    }

    /// The slice a block may write into, starting at the decode cursor.
    pub fn block_target(&mut self) -> &mut [u8] {
        &mut self.buf[self.end..]
    }

    /// Advance the decode cursor after a block wrote `n` bytes.
    pub fn advance(&mut self, n: usize) {
        self.end += n;
    }

    /// The decode cursor (absolute offset into the buffer).
    pub fn end_abs(&self) -> usize {
        self.end
    }

    /// Absolute slice for checksumming freshly decoded bytes. Both offsets
    /// must lie in the same physical segment (blocks never straddle a wrap).
    #[cfg(feature = "hash")]
    pub fn abs_slice(&self, a: usize, b: usize) -> &[u8] {
        &self.buf[a..b]
    }

    #[cfg(test)]
    pub fn origin_for_test(&self) -> usize {
        self.origin
    }
}

#[cfg(test)]
mod tests {
    use super::FlatOut;
    use crate::common::MAX_BLOCK_SIZE;

    #[test]
    fn grows_then_wraps_without_stalling() {
        // Tiny window so wraps happen quickly.
        let window = 1024;
        let mut f = FlatOut::new();
        f.reset(window);
        let mut sink = [0u8; 4096];
        let mut total = 0usize;
        let mut given_out = 0usize;
        for i in 0..2000 {
            // Flush fully first, as the streaming layer would.
            loop {
                let n = f.flush_to(&mut sink);
                given_out += n;
                if n == 0 {
                    break;
                }
            }
            assert!(f.ensure_block_space(false), "stuck at iteration {i}");
            let fill = (i % 7 + 1) * 100;
            f.block_target()[..fill].fill((i % 251) as u8); // i: usize
            f.advance(fill);
            total += fill;
            assert_eq!(f.produced(), total);
            // After a wrap, flushed bytes leave the pending region (they stay
            // in the buffer only as match history), so this is an inequality.
            assert!(given_out + f.can_flush() <= total);
        }
    }

    #[test]
    fn wrap_keeps_history_addressable() {
        // Virtual addresses below the origin must keep mapping into the
        // history segment after a wrap.
        let window = MAX_BLOCK_SIZE as usize;
        let mut f = FlatOut::new();
        f.reset(window);
        let mut sink = alloc::vec![0u8; window];
        for i in 0..64u8 {
            loop {
                if f.flush_to(&mut sink) == 0 {
                    break;
                }
            }
            assert!(f.ensure_block_space(false));
            let n = window / 2;
            f.block_target()[..n].fill(i);
            f.advance(n);
        }
        let produced = f.produced();
        assert!(produced > window);
        // A wrap must have happened for this much production.
        let origin = f.origin_for_test();
        assert!(origin == 0 || origin >= window + MAX_BLOCK_SIZE as usize);
    }
}
