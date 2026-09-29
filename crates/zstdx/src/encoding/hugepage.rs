//! Hugepage-backed buffers for the pooled matcher's multi-MiB tables and
//! window.
//!
//! A pooled matcher re-sizes its tables at level transitions, and after a
//! big alloc/free cycle the allocator hands the next multi-MiB table
//! physically scattered pages; the frozen layout then taxes every access
//! for the table's whole lifetime (dTLB misses; the 2026-09-29 dispersity
//! root-cause in docs/src/dev/pitfalls/workflow.md). Buffers at or above
//! [`HUGE_MIN_BYTES`] are therefore placed in private anonymous mappings
//! advised `MADV_HUGEPAGE`, so the hugepage backing does not depend on the
//! system THP setting. Everything smaller — and any non-Linux target or
//! failed mmap/madvise — stays on the plain `Vec` path.

use alloc::{vec, vec::Vec};
use core::{
    fmt,
    ops::{Deref, DerefMut},
    ptr::{self, NonNull},
    slice,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Minimum byte size that takes the hugepage path: below it a table is
/// L2/L3-resident and page-dispersity recovery does not pay.
pub(crate) const HUGE_MIN_BYTES: usize = 8 << 20;

/// Mapping granularity (the x86-64 PMD size): sizes round up to it and
/// the placement hint advances on it, so an aligned mapping is fully
/// hugepage-eligible. Configurations with larger PMDs get partial backing.
const HUGE_ALIGN: usize = 2 << 20;

/// Address the aligned-mapping cursor starts at: a 2 MiB-aligned point in
/// the low canonical range, far below the PIE/heap (~0x55…) and the
/// top-down mmap region (~0x7f…) nothing else maps into. The cursor only
/// advances (freed mappings are not reused) — exhausting it would take
/// millions of multi-MiB mappings against a 128 TiB space.
#[cfg(target_os = "linux")]
const HINT_BASE: usize = 0x0000_2000_0000_0000;

/// Next aligned mapping address (see [`HINT_BASE`]); `fetch_add` hands
/// concurrent threads disjoint slots.
#[cfg(target_os = "linux")]
static HINT: AtomicUsize = AtomicUsize::new(HINT_BASE);

/// Element kinds [`HugeBuf`] may hold: the matcher's buffers are integer
/// arrays whose all-zero bit pattern is the initial state (the empty
/// sentinels included — `tables::EMPTY` is zero), so a fresh mapping needs
/// no fill pass: anonymous memory starts zero-filled, exactly like
/// `alloc::vec![0; n]`.
pub(crate) trait BufElem: Copy {
    /// The zero element.
    const ZERO: Self;
}

impl BufElem for u8 {
    const ZERO: Self = 0;
}
impl BufElem for u16 {
    const ZERO: Self = 0;
}
impl BufElem for u32 {
    const ZERO: Self = 0;
}
impl BufElem for u64 {
    const ZERO: Self = 0;
}

/// A zero-initialized table/window buffer: a hugepage-backed mapping for
/// large sizes, a plain `Vec` otherwise. Dereferences to the element
/// slice, so call sites read like the `Vec` they replaced; assignments
/// (`resize_tables`' family swaps) move the backing wholesale, and `Drop`
/// releases it.
pub(crate) struct HugeBuf<T: BufElem> {
    backing: Backing<T>,
}

enum Backing<T> {
    /// The plain path: sizes below [`HUGE_MIN_BYTES`], non-Linux targets,
    /// and any mapping failure.
    Vec(Vec<T>),
    /// A private anonymous mapping, `MADV_HUGEPAGE`-advised. `ptr` is the
    /// mapping base (the elements start at it), `cap` its element
    /// capacity, so release needs exactly these two.
    Map {
        ptr: NonNull<T>,
        len: usize,
        cap: usize,
    },
}

impl<T: BufElem> HugeBuf<T> {
    /// An empty mapping-backed [`Backing`] with capacity for `min_elems`
    /// elements, or `None` below the threshold or on any mapping failure.
    fn try_map(min_elems: usize) -> Option<Backing<T>> {
        let bytes = min_elems.checked_mul(size_of::<T>())?;
        if bytes < HUGE_MIN_BYTES {
            return None;
        }
        // The size rounds to a hugepage multiple, so an aligned placement
        // makes every 2 MiB chunk of the mapping THP-eligible.
        let map_bytes = bytes.next_multiple_of(HUGE_ALIGN);
        let (ptr, map_bytes) = raw::map(map_bytes)?;
        Some(Backing::Map {
            ptr,
            len: 0,
            cap: map_bytes / size_of::<T>(),
        })
    }

    /// An empty buffer holding nothing (`Vec::new`).
    pub(crate) fn new() -> Self {
        Self {
            backing: Backing::Vec(Vec::new()),
        }
    }

    /// A zero-filled buffer of `len` elements: `alloc::vec![0; len]` on
    /// the plain path; the mapping starts zero-filled with no write pass.
    pub(crate) fn zeroed(len: usize) -> Self {
        match Self::try_map(len) {
            Some(backing) => {
                let mut buf = Self { backing };
                // SAFETY: the mapping covers `len` elements and is
                // zero-filled by the kernel, so exposing them needs no
                // write pass.
                unsafe { buf.set_len(len) };
                buf
            },
            None => Self {
                backing: Backing::Vec(vec![T::ZERO; len]),
            },
        }
    }

    /// An empty buffer with capacity for `cap` elements
    /// (`Vec::with_capacity`; the capacity is uninitialized storage).
    pub(crate) fn with_capacity(cap: usize) -> Self {
        match Self::try_map(cap) {
            Some(backing) => Self { backing },
            None => Self {
                backing: Backing::Vec(Vec::with_capacity(cap)),
            },
        }
    }

    /// Element capacity without reallocation (`Vec::capacity` on the plain
    /// path; the mapping's element capacity).
    pub(crate) fn capacity(&self) -> usize {
        match &self.backing {
            Backing::Vec(v) => v.capacity(),
            Backing::Map { cap, .. } => *cap,
        }
    }

    /// Ensure capacity for `additional` elements beyond the current length
    /// (`Vec::reserve`): a backing crossing the hugepage threshold switches
    /// to a mapping (copying the live prefix), a mapping is re-created
    /// larger, and a failed growth falls back to `Vec`.
    pub(crate) fn reserve(&mut self, additional: usize) {
        let len = self.len();
        let need = len + additional;
        if need <= self.capacity() {
            return;
        }
        match self.backing {
            Backing::Vec(_) => {
                if let Some(backing) = Self::try_map(need) {
                    let mut next = Self { backing };
                    next.extend_from_slice(&self[..]);
                    // Assigning drops the old Vec through `Drop`.
                    *self = next;
                } else if let Backing::Vec(v) = &mut self.backing {
                    v.reserve(additional);
                }
            },
            Backing::Map { ptr, cap, .. } => {
                if let Some(backing) = Self::try_map(need) {
                    let mut next = Self { backing };
                    next.extend_from_slice(&self[..]);
                    // Drops the old mapping through `Drop`.
                    *self = next;
                } else {
                    // Plain-path fallback: move the elements out, then
                    // release the mapping once, here (assigning the enum
                    // would not run `HugeBuf`'s `Drop`).
                    let mut v = Vec::with_capacity(need);
                    v.extend_from_slice(&self[..]);
                    raw::unmap(ptr, cap * size_of::<T>());
                    self.backing = Backing::Vec(v);
                }
            },
        }
    }

    /// Keep the first `n` elements (`Vec::truncate`; the backing stays).
    pub(crate) fn truncate(&mut self, n: usize) {
        match &mut self.backing {
            Backing::Vec(v) => v.truncate(n),
            Backing::Map { len, .. } => *len = (*len).min(n),
        }
    }

    /// Drop all elements, keeping the backing (`Vec::clear`).
    pub(crate) fn clear(&mut self) {
        match &mut self.backing {
            Backing::Vec(v) => v.clear(),
            Backing::Map { len, .. } => *len = 0,
        }
    }

    /// Resize to `n` elements, zero-filling any growth (`Vec::resize(n, 0)`
    /// semantics: a regrown tail is written, never stale).
    pub(crate) fn resize_zeroed(&mut self, n: usize) {
        let len = self.len();
        if n <= len {
            self.truncate(n);
            return;
        }
        if n > self.capacity() {
            // Growth past the backing reallocates, like `Vec::resize`.
            let mut next = Self::zeroed(n);
            next[..len].copy_from_slice(&self[..]);
            *self = next;
            return;
        }
        // SAFETY: `n <= capacity`; the tail is zeroed below before any
        // read can expose it.
        unsafe { self.set_len(n) };
        self[len..].fill(T::ZERO);
    }

    /// Append the elements of `src` (`Vec::extend_from_slice`).
    pub(crate) fn extend_from_slice(&mut self, src: &[T]) {
        self.reserve(src.len());
        match &mut self.backing {
            Backing::Vec(v) => v.extend_from_slice(src),
            Backing::Map { ptr, len, .. } => {
                // SAFETY: `reserve` guaranteed room for `src.len()` more
                // elements at `ptr + len`; `src` cannot alias `self`
                // (the borrow checker enforces the same rule as Vec's).
                unsafe {
                    ptr::copy_nonoverlapping(src.as_ptr(), ptr.as_ptr().add(*len), src.len());
                }
                *len += src.len();
            },
        }
    }

    /// Set the element count.
    ///
    /// # Safety
    /// Same contract as `Vec::set_len`: `n` must be within capacity, and
    /// the elements beyond the old length must be initialized before any
    /// read (the staging-tail pattern).
    pub(crate) unsafe fn set_len(&mut self, n: usize) {
        match &mut self.backing {
            Backing::Vec(v) => unsafe { v.set_len(n) },
            Backing::Map { len, cap, .. } => {
                debug_assert!(n <= *cap);
                *len = n;
            },
        }
    }
}

impl<T: BufElem> Deref for HugeBuf<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match &self.backing {
            Backing::Vec(v) => v,
            // SAFETY: `ptr` names `len` initialized elements inside the
            // owned mapping (the invariant of every constructor/mutator).
            Backing::Map { ptr, len, .. } => unsafe { slice::from_raw_parts(ptr.as_ptr(), *len) },
        }
    }
}

impl<T: BufElem> DerefMut for HugeBuf<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match &mut self.backing {
            Backing::Vec(v) => v,
            // SAFETY: as `Deref::deref`; no other borrow of the mapping is
            // live while this one is.
            Backing::Map { ptr, len, .. } => unsafe {
                slice::from_raw_parts_mut(ptr.as_ptr(), *len)
            },
        }
    }
}

// SAFETY: the mapping is owned exclusively by this buffer and its pointer
// never escapes it; moving the buffer moves the mapping, so the element
// data has exactly the thread semantics of the equivalent `Vec<T>`.
unsafe impl<T: BufElem + Send> Send for HugeBuf<T> {}
unsafe impl<T: BufElem + Sync> Sync for HugeBuf<T> {}

impl<T: BufElem> Drop for HugeBuf<T> {
    fn drop(&mut self) {
        if let Backing::Map { ptr, cap, .. } = &self.backing {
            raw::unmap(*ptr, *cap * size_of::<T>());
        }
    }
}

impl<T: BufElem> Clone for HugeBuf<T> {
    fn clone(&self) -> Self {
        let mut next = match Self::try_map(self.len()) {
            Some(backing) => Self { backing },
            None => Self {
                backing: Backing::Vec(Vec::with_capacity(self.len())),
            },
        };
        next.extend_from_slice(self);
        next
    }

    fn clone_from(&mut self, src: &Self) {
        if self.len() == src.len() {
            // Same-size copy keeps the destination's backing (the snapshot
            // adoption path).
            self.copy_from_slice(src);
        } else {
            *self = src.clone();
        }
    }
}

impl<T: BufElem + PartialEq> PartialEq for HugeBuf<T> {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl<T: BufElem + Eq> Eq for HugeBuf<T> {}

impl<T: BufElem + fmt::Debug> fmt::Debug for HugeBuf<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self[..], f)
    }
}

impl<T: BufElem> Default for HugeBuf<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Kernel mapping primitives. The non-Linux arm never maps: MADV_HUGEPAGE
/// is a Linux mechanism, so the plain `Vec` path serves every buffer there.
mod raw {
    use core::ptr::NonNull;

    /// Map `map_bytes` (a [`super::HUGE_ALIGN`] multiple) of private
    /// anonymous memory, `MADV_HUGEPAGE`-advised, and return the aligned
    /// mapping base. `None` on any failure (a failed advice releases the
    /// mapping: a scattered mapping is what this module exists to avoid).
    #[cfg(target_os = "linux")]
    pub(super) fn map<T>(map_bytes: usize) -> Option<(NonNull<T>, usize)> {
        // SAFETY: the three calls below are plain kernel calls on this
        // mapping's own base/size.
        unsafe {
            let base = aligned_mmap(map_bytes)?;
            let base = NonNull::new(base.cast::<T>())?;
            if libc::madvise(base.as_ptr().cast(), map_bytes, libc::MADV_HUGEPAGE) != 0 {
                libc::munmap(base.as_ptr().cast(), map_bytes);
                return None;
            }
            Some((base, map_bytes))
        }
    }

    /// Non-Linux targets never map (the plain `Vec` path serves every
    /// buffer).
    #[cfg(not(target_os = "linux"))]
    pub(super) fn map<T>(_map_bytes: usize) -> Option<(NonNull<T>, usize)> {
        None
    }

    /// Release a mapping returned by [`map`](Self::map).
    #[cfg(target_os = "linux")]
    pub(super) fn unmap<T>(base: NonNull<T>, map_bytes: usize) {
        // SAFETY: the base/size pair is the one `map` returned, and the
        // mapping is still owned by the caller.
        unsafe { libc::munmap(base.as_ptr().cast(), map_bytes) };
    }

    /// Non-Linux counterpart of [`unmap`](Self::map): unreachable, no
    /// mapping can exist there.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn unmap<T>(base: NonNull<T>, map_bytes: usize) {
        let _ = (base, map_bytes);
    }

    /// A hugepage-aligned anonymous mapping of `map_bytes`: take the next
    /// cursor slot with `MAP_FIXED_NOREPLACE` (exact placement, so the
    /// 2 MiB-multiple size is fully THP-eligible), retrying on a busy
    /// slot; a persistent miss falls back to the kernel's own placement
    /// (unaligned, only partially eligible, still correct).
    #[cfg(target_os = "linux")]
    fn aligned_mmap(map_bytes: usize) -> Option<*mut core::ffi::c_void> {
        // SAFETY: as `map`; every attempt owns exactly its own mapping.
        unsafe {
            for _ in 0..8 {
                let hint = super::HINT.fetch_add(map_bytes, super::Ordering::Relaxed)
                    as *mut core::ffi::c_void;
                let p = libc::mmap(
                    hint,
                    map_bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                    -1,
                    0,
                );
                if p != libc::MAP_FAILED {
                    return Some(p);
                }
            }
            let p = libc::mmap(
                core::ptr::null_mut(),
                map_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            (p != libc::MAP_FAILED).then_some(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Length/capacity bookkeeping across the plain arm: the clone and
    /// reserve paths once preset the length AND appended the copy, doubling
    /// it (the snapshot adoption segfault).
    #[test]
    fn plain_arm_bookkeeping() {
        let mut buf = HugeBuf::<u32>::zeroed(4);
        assert_eq!(&buf[..], &[0; 4]);
        buf.reserve(8);
        buf.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(buf.len(), 8);
        assert_eq!(&buf[..4], &[0; 4]);
        assert_eq!(&buf[4..], &[1, 2, 3, 4]);
        let c = buf.clone();
        assert_eq!(c.len(), 8);
        assert_eq!(&c[..], &buf[..]);
        let mut d = HugeBuf::<u32>::zeroed(8);
        d[0] = 9;
        d.clone_from(&c);
        assert_eq!(&d[..], &c[..]);
        d.truncate(2);
        assert_eq!(d.len(), 2);
        d.resize_zeroed(6);
        assert_eq!(&d[..], &[0; 6]);
        d.clear();
        assert!(d.is_empty());
    }

    /// The mapping arm (Linux) and its Vec fallback elsewhere: capacity
    /// covers the request, the kernel zero-fill holds, and re-homing
    /// (clone, growth) moves the elements intact.
    #[test]
    fn huge_arm_bookkeeping() {
        let n = HUGE_MIN_BYTES / 4;
        let mut buf = HugeBuf::<u32>::zeroed(n);
        assert!(buf.capacity() >= n);
        assert_eq!(buf.len(), n);
        assert!(buf.iter().all(|&v| v == 0));
        buf[n - 1] = 7;
        assert_eq!(buf[n - 1], 7);
        let c = buf.clone();
        assert_eq!(c.len(), n);
        assert_eq!(&c[..], &buf[..]);
        buf.reserve(n);
        assert_eq!(buf.len(), n);
        assert_eq!(&buf[..], &c[..]);
        buf.resize_zeroed(n / 2);
        assert_eq!(&buf[..], &c[..n / 2]);
    }
}
