//! Frame checksum backends: the inline XXH64 accumulator and the slice-path
//! selector (inline / sidecar-thread offload / off), behind the shared
//! [`BlockChecksum`] shape the block writers absorb through.

use alloc::vec::Vec;

/// Frame checksum accumulator. With the `hash` feature it is an XXH64 over
/// the frame content; without it a no-op so the block paths share one shape.
/// The raw-block writers feed it while they copy (`write_appending`), which
/// removes the separate hash read pass over incompressible data.
pub(crate) struct FrameHasher {
    #[cfg(feature = "hash")]
    inner: crate::xxh64::Xxh64,
}

#[cfg(feature = "hash")]
impl FrameHasher {
    pub(crate) fn new() -> Self {
        Self {
            inner: crate::xxh64::Xxh64::new(0),
        }
    }

    #[inline(always)]
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.inner.write(bytes);
    }

    #[inline(always)]
    pub(crate) fn write_appending(&mut self, out: &mut Vec<u8>, bytes: &[u8]) {
        self.inner.write_appending(out, bytes);
    }

    #[inline(always)]
    pub(crate) fn write_appending_from(
        &mut self,
        out: &mut Vec<u8>,
        bytes: &[u8],
        hash_from: usize,
    ) {
        self.inner.write_appending_from(out, bytes, hash_from);
    }

    /// Uniform scan fused with the checksum absorb: RLE blocks come out
    /// fully hashed in the scan's single pass; anything else returns the
    /// resume offset for the outcome paths. Misaligned streams (streaming
    /// reader) fall back to a plain scan that hashes from zero.
    #[inline(always)]
    pub(crate) fn scan_uniform(&mut self, data: &[u8]) -> (bool, usize) {
        if self.inner.mem_is_empty() {
            self.inner.scan_uniform_absorbing(data)
        } else {
            let uniform = super::util::is_uniform(data);
            if uniform {
                self.inner.write(data);
                (true, data.len())
            } else {
                (false, 0)
            }
        }
    }

    #[inline(always)]
    pub(crate) fn finish(&self) -> u32 {
        self.inner.finish() as u32
    }
}

#[cfg(not(feature = "hash"))]
impl FrameHasher {
    pub(crate) fn new() -> Self {
        Self {}
    }

    #[inline(always)]
    pub(crate) fn write(&mut self, _bytes: &[u8]) {}

    #[inline(always)]
    pub(crate) fn write_appending(&mut self, out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(bytes);
    }

    #[inline(always)]
    pub(crate) fn write_appending_from(
        &mut self,
        out: &mut Vec<u8>,
        bytes: &[u8],
        _hash_from: usize,
    ) {
        out.extend_from_slice(bytes);
    }

    #[inline(always)]
    pub(crate) fn scan_uniform(&mut self, data: &[u8]) -> (bool, usize) {
        (super::util::is_uniform(data), 0)
    }

    #[inline(always)]
    pub(crate) fn finish(&self) -> u32 {
        0
    }
}

/// Frame-checksum backend shared by the block paths: check a block for a
/// uniform run while absorbing it, absorb whatever the scan did not cover,
/// copy a raw block out while absorbing from the scan's offset, and produce
/// the frame checksum. The inline [`FrameHasher`] implements all four by
/// itself; the offloaded implementation (std + hash) runs the absorbs on a
/// sidecar thread and its `scan_block` returns the block's full length as
/// the covered offset, which turns the resume paths into no-ops.
pub(crate) trait BlockChecksum {
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize);
    fn hash_tail(&mut self, bytes: &[u8]);
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize);
    fn finish32(&mut self) -> u32;
}

impl BlockChecksum for FrameHasher {
    #[inline(always)]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        self.scan_uniform(data)
    }

    #[inline(always)]
    fn hash_tail(&mut self, bytes: &[u8]) {
        self.write(bytes);
    }

    #[inline(always)]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize) {
        self.write_appending_from(out, bytes, from);
    }

    #[inline(always)]
    fn finish32(&mut self) -> u32 {
        self.finish()
    }
}

/// Slice-path checksum backend: inline below the offload threshold (the
/// sidecar thread's spawn and hand-off cost more than hashing saves on small
/// inputs), offloaded above it, or disabled when the caller opted out of a
/// checksum (the uniform scan still runs for the RLE path).
pub(crate) enum SliceChecksum {
    #[cfg(feature = "hash")]
    Inline(FrameHasher),
    #[cfg(all(feature = "std", feature = "hash"))]
    Offload(super::async_checksum::AsyncChecksum),
    Off,
}

#[cfg(all(feature = "std", feature = "hash"))]
const ASYNC_MIN_INPUT: usize = 256 * 1024;

impl SliceChecksum {
    pub(crate) fn new(src_len: usize, checksum: bool) -> Self {
        #[cfg(not(all(feature = "std", feature = "hash")))]
        let _ = src_len;
        if !checksum {
            return Self::Off;
        }
        #[cfg(all(feature = "std", feature = "hash"))]
        if src_len >= ASYNC_MIN_INPUT
            && let Some(offload) = super::async_checksum::AsyncChecksum::new()
        {
            return Self::Offload(offload);
        }
        #[cfg(feature = "hash")]
        {
            Self::Inline(FrameHasher::new())
        }
        #[cfg(not(feature = "hash"))]
        {
            Self::Off
        }
    }
}

#[cfg(feature = "hash")]
impl BlockChecksum for SliceChecksum {
    #[inline]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        match self {
            Self::Inline(h) => h.scan_block(data),
            // The whole block is posted up front; the resume paths below see
            // a covered offset equal to the length and contribute nothing.
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => {
                h.write(data);
                (super::util::is_uniform(data), data.len())
            },
            Self::Off => (super::util::is_uniform(data), 0),
        }
    }

    #[inline]
    fn hash_tail(&mut self, bytes: &[u8]) {
        match self {
            Self::Inline(h) => h.hash_tail(bytes),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => h.write(bytes),
            Self::Off => {},
        }
    }

    #[inline]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], from: usize) {
        match self {
            Self::Inline(h) => h.raw_out(out, bytes, from),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => {
                out.extend_from_slice(bytes);
                if from < bytes.len() {
                    h.write(&bytes[from..]);
                }
            },
            Self::Off => out.extend_from_slice(bytes),
        }
    }

    #[inline]
    fn finish32(&mut self) -> u32 {
        match self {
            Self::Inline(h) => h.finish32(),
            #[cfg(all(feature = "std", feature = "hash"))]
            Self::Offload(h) => h.finish(),
            Self::Off => 0,
        }
    }
}

/// Without the `hash` feature only the `Off` variant exists: uniform
/// detection still runs (the RLE path needs it).
#[cfg(not(feature = "hash"))]
impl BlockChecksum for SliceChecksum {
    #[inline]
    fn scan_block(&mut self, data: &[u8]) -> (bool, usize) {
        (super::util::is_uniform(data), 0)
    }

    #[inline]
    fn hash_tail(&mut self, _bytes: &[u8]) {}

    #[inline]
    fn raw_out(&mut self, out: &mut Vec<u8>, bytes: &[u8], _from: usize) {
        out.extend_from_slice(bytes);
    }

    #[inline]
    fn finish32(&mut self) -> u32 {
        0
    }
}
