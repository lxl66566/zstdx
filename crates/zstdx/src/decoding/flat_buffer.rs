//! Flat output buffer for streaming decode, mirroring libzstd's `outBuff`
//! generation model: blocks are decoded straight into a linear buffer and
//! handed out from it; handed-out bytes stay physically in place as match
//! history. A generation wraps when it is fully handed out and cannot fit
//! one more block (`end + blockMax > cap`, the cap being libzstd's
//! `ZSTD_decodingBufferSize_internal`); writing restarts at the buffer
//! start, the virtual origin advances, and the wrapped-away previous
//! generation keeps serving as the match window until the next wrap
//! overwrites it — exactly libzstd's ring, where
//! `ZSTD_checkContinuity` rebases `virtualStart` to the previous
//! contiguous run. The per-sequence acceptance rule is thereby libzstd's
//! `offset > oLitEnd - virtualStart` corruption check: an offset is legal
//! iff it stays within the previous generation's length plus the position
//! in the current one. A declared Frame_Content_Size caps the buffer
//! (`min(fcs, window + 2*blockMax + 64)`) and disables wrapping entirely
//! while it covers the frame (`outBuffSize < frameContentSize`).
//!
//! Dictionary frames decode through this buffer too: the raw dict content
//! is prefilled as generation 0's physical prefix, so the never-wrapped
//! (first) generation reaches `dictLen + produced` — libzstd's
//! `virtualStart = dst - dictSize` — and the first wrap drops the dict
//! from the window exactly when libzstd's ring discards it as extDict.
//! The wrap threshold ignores the prefill (libzstd's ring never holds the
//! dict), so generation lengths match the C byte for byte.
//!
//! One-shot decoding (strategy `All` with nothing handed out mid-frame)
//! never satisfies the wrap precondition and grows instead, keeping the
//! whole frame as history like `ZSTD_decompress`.
//!
//! Wrap safety: after a wrap the active generation writes `[0, cursor)`
//! (overwriting the dict prefill, then the previous generation from the
//! bottom), and `ensure_block_space` guarantees `blockMax + WILDCOPY_SLACK`
//! bytes of target room, so no write leaves the allocation. Reads below
//! the cursor are either the previous generation's still-intact top
//! `[cursor, genEnd)` — always above the write cursor, per the reach rule
//! the executor enforces — or rejected as corruption.

use alloc::vec::Vec;

use crate::common::{max_block_output, streaming_buffer_cap};

/// Wildcopy overshoot the block target must absorb beyond a full block
/// (16-byte chunks copying past the sequence end).
pub(crate) const WILDCOPY_SLACK: usize = 16;

/// Read-only window mapping handed to the sequence executor: virtual
/// addresses are global frame positions (the dict prefill occupies
/// `[0, dictLen)`). The active generation maps `v >= origin` to the
/// physical offset `v - origin`, the wrapped-away previous generation maps
/// `prev_origin <= v < origin` to `prev_phys_base + (v - prev_origin)`
/// (bounded by its physical end). Anything below `prev_origin` is out of
/// window.
#[derive(Clone, Copy)]
pub(crate) struct FlatView {
    pub origin: usize,
    pub prev_origin: usize,
    /// Physical offset of the previous generation's first byte (0 except
    /// right after the dict-prefilled first generation, whose output sits
    /// above the prefill at physical `dictLen`).
    pub prev_phys_base: usize,
    /// Run length of the previous generation (`origin - prev_origin`); its
    /// bytes sit at `[prev_phys_base, prev_phys_base + seg_a_end)` and its
    /// virtual span is `[prev_origin, origin)`. Its low bytes may already
    /// be overwritten by the active generation's writes.
    pub seg_a_end: usize,
    pub out_len: usize,
}

impl FlatView {
    /// One-shot mapping: the whole decode target is one contiguous
    /// generation and the full history is addressable.
    pub fn one_shot(out_len: usize) -> Self {
        FlatView {
            origin: 0,
            prev_origin: 0,
            prev_phys_base: 0,
            seg_a_end: out_len,
            out_len,
        }
    }
}

pub(crate) struct FlatOut {
    buf: Vec<u8>,
    /// Virtual origin of the active generation (== total bytes produced at
    /// the last wrap, dict prefill included). `origin + end - dict_total`
    /// is the frame's produced count; virtual addresses must stay globally
    /// monotonic because sequence offsets are global distances.
    origin: usize,
    /// Virtual origin of the previous generation, i.e. of the history that
    /// still serves as the match window after a wrap.
    prev_origin: usize,
    /// Physical offset of that previous generation (`dict_total` for the
    /// first wrap, 0 afterwards).
    prev_phys_base: usize,
    /// Run length of that previous generation (`origin - prev_origin`,
    /// the dict prefill excluded); its bytes sit at
    /// `[prev_phys_base, prev_phys_base + seg_a_end)`.
    seg_a_end: usize,
    /// Flush cursor: `buf[start..end]` is decoded but not yet handed out.
    start: usize,
    /// Decode cursor.
    end: usize,
    /// Streaming buffer cap (libzstd's outBuff size) and the frame's block
    /// maximum.
    cap: usize,
    block_max: usize,
    /// libzstd never wraps when the buffer already covers a declared
    /// Frame_Content_Size (`outBuffSize < frameContentSize` guard); such a
    /// frame decodes as one generation with the whole output as history.
    wrap_enabled: bool,
    /// Raw dict content prefilled at `buf[0..dict_prefill]`; part of the
    /// first generation's window. Cleared at the first wrap (libzstd's
    /// ring discards the dict extDict there); `dict_total` keeps the
    /// produced-count accounting for the whole frame.
    dict_prefill: usize,
    dict_total: usize,
}

impl FlatOut {
    pub fn new() -> Self {
        FlatOut {
            buf: Vec::new(),
            origin: 0,
            prev_origin: 0,
            prev_phys_base: 0,
            seg_a_end: 0,
            start: 0,
            end: 0,
            cap: 0,
            block_max: 0,
            wrap_enabled: true,
            dict_prefill: 0,
            dict_total: 0,
        }
    }

    pub fn reset(&mut self, window: usize, fcs: Option<u64>) {
        self.reset_with_dict(window, fcs, &[]);
    }

    /// Reset for a frame whose history starts with `dict`'s raw content
    /// (libzstd's `ZSTD_refDictContent`; empty for dictionary-free frames).
    pub fn reset_with_dict(&mut self, window: usize, fcs: Option<u64>, dict: &[u8]) {
        self.set_generation_chain(0, 0, 0, 0);
        self.cap = streaming_buffer_cap(window, fcs);
        self.block_max = max_block_output(window);
        self.wrap_enabled = fcs.is_none_or(|f| f > self.cap as u64);
        self.dict_total = dict.len();
        self.dict_prefill = dict.len();
        self.start = dict.len();
        self.end = dict.len();
        if self.buf.len() < dict.len() {
            self.buf.resize(dict.len(), 0);
        }
        self.buf[..dict.len()].copy_from_slice(dict);
        #[cfg(debug_assertions)]
        self.assert_invariants();
    }

    /// The generation chain's single write point: the active generation's
    /// virtual origin together with the retained previous generation
    /// (`prev_origin`, `prev_phys_base`, run length). The chain identity
    /// `origin == prev_origin + seg_a_end` is what `copy_wrapped_match`'s
    /// fast path folds into one pointer add, so it is checked at every
    /// transition instead of living in comments.
    #[inline]
    fn set_generation_chain(
        &mut self,
        origin: usize,
        prev_origin: usize,
        prev_phys_base: usize,
        seg_a_end: usize,
    ) {
        debug_assert_eq!(
            origin,
            prev_origin.wrapping_add(seg_a_end),
            "generation chain identity"
        );
        self.origin = origin;
        self.prev_origin = prev_origin;
        self.prev_phys_base = prev_phys_base;
        self.seg_a_end = seg_a_end;
    }

    /// Debug check of the cursor and generation invariants, run at the
    /// mutation entries (per block or per flush, never per byte): the
    /// chain identity, ordered cursors inside the buffer, the prefill
    /// never handed out, and a non-negative produced count.
    #[cfg(debug_assertions)]
    fn assert_invariants(&self) {
        debug_assert_eq!(
            self.origin,
            self.prev_origin.wrapping_add(self.seg_a_end),
            "generation chain identity"
        );
        debug_assert!(self.start <= self.end && self.end <= self.buf.len());
        debug_assert!(self.start >= self.dict_prefill || self.start == 0);
        debug_assert!(self.origin + self.end >= self.dict_total);
    }

    /// Snapshot of the virtual window mapping for the sequence executor.
    pub fn view(&self) -> FlatView {
        #[cfg(debug_assertions)]
        self.assert_invariants();
        FlatView {
            origin: self.origin,
            prev_origin: self.prev_origin,
            prev_phys_base: self.prev_phys_base,
            seg_a_end: self.seg_a_end,
            out_len: self.buf.len(),
        }
    }

    /// Total bytes produced in the frame so far (size of the virtual space
    /// above the dict prefill).
    pub fn produced(&self) -> usize {
        self.origin + self.end - self.dict_total
    }

    /// Make room for one more block (checked entry; the room logic is
    /// [`Self::ensure_block_space_inner`]). On a `true` return the block
    /// target `buf[end..]` holds at least `block_max + WILDCOPY_SLACK`
    /// bytes beyond the cursor — the HEADROOM chain's producer promise,
    /// asserted here so a regression fails at the boundary instead of
    /// surfacing as an executor overread.
    #[inline]
    pub fn ensure_block_space(&mut self, force: bool) -> bool {
        let ok = self.ensure_block_space_inner(force);
        #[cfg(debug_assertions)]
        if ok {
            debug_assert!(
                self.buf.len() >= self.end + self.block_max + WILDCOPY_SLACK,
                "true return must guarantee the block target plus wildcopy slack"
            );
            self.assert_invariants();
        }
        ok
    }

    /// Make room for one more block. Returns false when the caller must
    /// first hand out the pending bytes (`start < end`); in that case no
    /// header should have been consumed from the source yet.
    ///
    /// On success the block target `buf[end..]` holds at least
    /// `block_max + WILDCOPY_SLACK` bytes beyond the cursor, letting the
    /// sequence executor drop its per-sequence bounds checks (the HEADROOM
    /// instantiation of `execute_decoded_flat`).
    ///
    /// The wrap fires at libzstd's exact threshold — a fully handed-out
    /// generation plus one block max over the cap — independent of the
    /// buffer's actual size, so acceptance matches the reference even when
    /// the buffer was force-grown earlier.
    ///
    /// With `force`, the buffer grows past the steady-state size instead of
    /// wrapping or returning false — used by the All strategy, which
    /// promises to decode every block before the caller reads (and by
    /// never-wrap pledged frames whose output overruns the pledge; the
    /// frame-tail content-size check rejects those).
    fn ensure_block_space_inner(&mut self, force: bool) -> bool {
        #[cfg(debug_assertions)]
        self.assert_invariants();
        // The dict prefill does not count against the cap (libzstd's ring
        // never holds the dict), so generation lengths match the C.
        if self.start == self.end
            && self.wrap_enabled
            && self.end + self.block_max > self.cap + self.dict_prefill
        {
            // Wrap: everything decoded so far was handed out and the
            // generation cannot fit one more block. Virtual origins chain
            // so addresses stay monotonic; the executor rejects sources
            // below the new `prev_origin`, so the new generation only ever
            // reads its own bytes, the previous generation's intact top,
            // and the guaranteed target room. The retained window is the
            // previous generation — starting at its virtual top edge for
            // the dict-prefilled first generation, whose lower `dict_prefill`
            // bytes (the dict) leave the window here exactly when libzstd's
            // ring discards the dict extDict at the first wrap.
            // The start cursor began at the prefill and only grows, so the
            // run length below cannot underflow.
            debug_assert!(self.end >= self.dict_prefill);
            let origin = self.origin + self.end;
            self.set_generation_chain(
                origin,
                self.origin + self.dict_prefill,
                self.dict_prefill,
                self.end - self.dict_prefill,
            );
            self.dict_prefill = 0;
            self.start = 0;
            self.end = 0;
        }
        if self.end + self.block_max + WILDCOPY_SLACK <= self.buf.len() {
            return true;
        }
        let cap = self.buf.len();
        let needed = self.cap + WILDCOPY_SLACK + self.dict_prefill;
        if cap < needed || force || self.start == self.end {
            // Grow towards the steady-state size (beyond it when forced, or
            // when a wrap cannot fire — a pending-free cursor must always
            // make progress); the cursor semantics are index-based, so a
            // realloc is transparent. Re-check afterwards: with a large
            // decode cursor the grown size may still be short, in which case
            // the caller must flush first and wrap later.
            let target = (2 * cap)
                .max(needed)
                .max(self.dict_prefill + self.block_max + WILDCOPY_SLACK);
            self.buf.resize(target, 0);
            return self.end + self.block_max + WILDCOPY_SLACK <= self.buf.len();
        }
        false
    }

    /// How many bytes can currently be handed out (all pending bytes; they
    /// remain in the buffer until overwritten by a later generation).
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

    /// Advance the decode cursor after a block wrote `n` bytes. The block
    /// never straddles a wrap (wraps only fire in
    /// [`Self::ensure_block_space_inner`] on a fully handed-out buffer, and
    /// blocks decode into `block_target` within one generation), so `end`
    /// stays a same-generation cursor.
    pub fn advance(&mut self, n: usize) {
        debug_assert!(self.end + n <= self.buf.len());
        self.end += n;
    }

    /// The decode cursor (absolute offset into the buffer).
    pub fn end_abs(&self) -> usize {
        self.end
    }

    /// Absolute slice for checksumming freshly decoded bytes. Both offsets
    /// must lie in the same physical generation (blocks never straddle a
    /// wrap).
    #[cfg(feature = "hash")]
    pub fn abs_slice(&self, a: usize, b: usize) -> &[u8] {
        &self.buf[a..b]
    }

    #[cfg(test)]
    pub fn origin_for_test(&self) -> usize {
        self.origin
    }

    #[cfg(test)]
    pub fn cap_for_test(&self) -> usize {
        self.cap
    }

    #[cfg(test)]
    pub fn prev_segment_for_test(&self) -> (usize, usize, usize) {
        (self.prev_origin, self.prev_phys_base, self.seg_a_end)
    }

    #[cfg(test)]
    pub fn buf_len_for_test(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::FlatOut;
    use crate::common::{MAX_BLOCK_SIZE, max_block_output, streaming_buffer_cap};

    #[test]
    fn grows_then_wraps_without_stalling() {
        // Tiny window so wraps happen quickly (cap = 1024 + 2*1024 + 64).
        let window = 1024;
        let mut f = FlatOut::new();
        f.reset(window, None);
        let mut sink = [0u8; 4096];
        let mut total = 0usize;
        let mut given_out = 0usize;
        for i in 0..2000 {
            // Hand out fully first, as the streaming layer would.
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
            assert!(given_out + f.can_flush() <= total);
        }
    }

    #[test]
    fn wrap_advances_origin_at_libzstd_threshold() {
        // The wrap fires when a fully handed-out generation cannot fit one
        // more block: end + block_max > cap, so every generation is longer
        // than cap - block_max and the first one starts at the buffer
        // bottom (cap - block_max = window + block_max + 64 minimum).
        let window = MAX_BLOCK_SIZE as usize;
        let mut f = FlatOut::new();
        f.reset(window, None);
        let cap = f.cap_for_test();
        let block = max_block_output(window);
        let mut sink = alloc::vec![0u8; 8 * window];
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
            if f.origin_for_test() != 0 {
                // Origin == the produced count at the wrap; the threshold
                // guarantees the generation exceeded cap - block_max, and
                // the current generation holds exactly this fill.
                assert!(f.origin_for_test() + block > cap);
                assert_eq!(f.produced(), f.origin_for_test() + n);
                return;
            }
        }
        panic!("a wrap should have happened");
    }

    #[test]
    fn wrap_fires_independent_of_buffer_size() {
        // A force-grown (oversized) buffer must wrap at the cap threshold,
        // not when the block stops fitting — libzstd's ring is always
        // exactly cap bytes, so generations delimit identically.
        let window = 1024;
        let mut f = FlatOut::new();
        f.reset(window, None);
        let cap = f.cap_for_test();
        // Decode four 1000-byte blocks without ever handing them out
        // (strategy All): force growth makes the buffer oversized.
        for _ in 0..4 {
            assert!(f.ensure_block_space(true));
            f.block_target()[..1000].fill(0x5a);
            f.advance(1000);
        }
        assert!(
            f.buf_len_for_test() > cap + 16,
            "buffer should be oversized beyond the steady-state size"
        );
        assert_eq!(f.origin_for_test(), 0);
        // Hand everything out; the next block request must wrap at the cap
        // threshold (4000 + 1024 > 3136) even though the oversized buffer
        // could still fit blocks until 4000 + 1024 + 16 <= 6304.
        let mut sink = [0u8; 8192];
        while f.flush_to(&mut sink) != 0 {}
        assert!(f.ensure_block_space(false));
        assert_eq!(
            f.origin_for_test(),
            4000,
            "wrap must fire at the cap threshold"
        );
    }

    #[test]
    fn wrap_frees_a_full_block_of_room() {
        // After a wrap the cursor resets and a maximal block plus wildcopy
        // slack must fit immediately (the HEADROOM contract).
        let window = 4096;
        let mut f = FlatOut::new();
        f.reset(window, None);
        let block = max_block_output(window);
        let mut sink = alloc::vec![0u8; 64 * window];
        let mut filled;
        loop {
            loop {
                if f.flush_to(&mut sink) == 0 {
                    break;
                }
            }
            assert!(f.ensure_block_space(false));
            filled = 0;
            while filled + block <= f.block_target().len() {
                f.block_target()[filled..filled + block].fill(0xab);
                filled += block;
                f.advance(block);
            }
            if f.origin_for_test() != 0 {
                break;
            }
        }
        assert!(f.block_target().len() >= block + 16);
        assert!(f.ensure_block_space(false));
    }

    #[test]
    fn pledged_frame_covered_by_cap_never_wraps() {
        // libzstd: outBuff = min(fcs, needed), and no wrap ever happens
        // when it already covers the pledged content — the whole frame is
        // one generation with full history (envelope lifted to one-shot).
        let window = 1024;
        let fcs = 3000u64; // below cap 3136
        let mut f = FlatOut::new();
        f.reset(window, Some(fcs));
        assert_eq!(f.cap_for_test(), fcs as usize);
        let mut sink = [0u8; 8192];
        let mut total = 0usize;
        for i in 0..40 {
            loop {
                if f.flush_to(&mut sink) == 0 {
                    break;
                }
            }
            assert!(f.ensure_block_space(false), "stalled at {total}");
            let fill = 1000.min(fcs as usize - total.min(fcs as usize));
            f.block_target()[..fill].fill((i % 251) as u8);
            f.advance(fill);
            total += fill;
            assert_eq!(f.origin_for_test(), 0, "pledged frame must not wrap");
        }
    }

    #[test]
    fn dict_prefill_extends_then_expires() {
        // Generation 0's window includes the dict prefill; the first wrap
        // drops it and rebases the previous generation's physical base.
        let window = 1024;
        let dict: alloc::vec::Vec<u8> = (0..4096u32).map(|i| i as u8).collect();
        let mut f = FlatOut::new();
        f.reset_with_dict(window, None, &dict);
        assert_eq!(f.produced(), 0, "prefill is not frame output");
        assert_eq!(f.can_flush(), 0, "prefill is never handed out");
        let mut sink = [0u8; 8192];
        let mut total = 0usize;
        for i in 0..16 {
            loop {
                if f.flush_to(&mut sink) == 0 {
                    break;
                }
            }
            assert!(f.ensure_block_space(false));
            if f.origin_for_test() != 0 {
                // The first wrap retains exactly the dict-topped first
                // generation: output-only window starting at the dict top.
                let (prev_origin, prev_phys_base, seg_a_end) = f.prev_segment_for_test();
                assert_eq!(prev_origin, dict.len());
                assert_eq!(prev_phys_base, dict.len());
                assert_eq!(seg_a_end, total);
                return;
            }
            f.block_target()[..1000].fill(i);
            f.advance(1000);
            total += 1000;
            assert_eq!(f.produced(), total);
        }
        panic!("a wrap should have happened");
    }

    #[test]
    fn cap_follows_libzstd_formula() {
        use crate::common::MIN_WINDOW_SIZE;
        // unpledged: max(window, 1KiB) + 2*min(window,128K) + 64
        assert_eq!(streaming_buffer_cap(4096, None), 4096 + 2 * 4096 + 64);
        assert_eq!(
            streaming_buffer_cap(MIN_WINDOW_SIZE as usize / 2, None),
            1024 + 2 * 512 + 64
        );
        // pledged below the needed size: the buffer stops at the pledge
        assert_eq!(streaming_buffer_cap(1 << 20, Some(999)), 999);
        // pledged above it: unchanged
        assert_eq!(
            streaming_buffer_cap(1 << 20, Some(1 << 40)),
            (1 << 20) + 2 * (1 << 17) + 64
        );
    }
}
