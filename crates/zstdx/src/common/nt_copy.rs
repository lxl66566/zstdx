//! The streaming-feed copy kernel: a non-temporal-store copy for spans
//! that land in multi-megabyte job buffers.
//!
//! The multithreaded stream core's read pump pulls its input in 1 MiB
//! steps straight into the accumulate buffer's spare capacity. Those
//! pulls sit under glibc's own non-temporal threshold, so a plain memcpy
//! keeps them temporal: every 100 MB of feed moves a source read plus a
//! destination read-for-ownership plus the writeback, about 300 MB of
//! traffic, beside the encode workers' own. The destination is consumed
//! later, by other cores, from DRAM either way, so the NT-store kernel
//! deletes the RFO half. The kernel is reached through the opt-in source
//! reader (`stream::read::NtSource`), not by specializing the `Read`
//! trait (the falsified internal-specialization class): an opaque
//! `Read`'s memcpy stays the caller's, an explicit wrapper's is ours.

/// Smallest span the kernel serves. Below it the copy stays temporal: the
/// destination's cache residency is what the consumer reads next (the
/// single-threaded core's 16 KiB staging pulls never reach this bar), and
/// an NT store only pays when the span streams to DRAM.
pub(crate) const NT_MIN: usize = 256 * 1024;

type Kernel = unsafe fn(*const u8, *mut u8, usize);

/// Copy `src` into `dst` (equal lengths), through the NT-store kernel for
/// streaming-scale spans and the plain temporal copy below [`NT_MIN`].
pub(crate) fn copy(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len(), dst.len());
    if src.len() < NT_MIN {
        dst.copy_from_slice(src);
        return;
    }
    // SAFETY: both slices are initialized for `len` bytes and disjoint.
    unsafe { kernel()(src.as_ptr(), dst.as_mut_ptr(), src.len()) };
}

/// Plain temporal copy: the fallback on non-x86_64 targets, no-std builds
/// and whatever tier the runtime clamp forces.
unsafe fn temporal(src: *const u8, dst: *mut u8, len: usize) {
    // SAFETY: the caller's contract — initialized, disjoint `len` spans.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, len) };
}

#[cfg(not(all(feature = "std", target_arch = "x86_64")))]
fn kernel() -> Kernel {
    temporal
}

/// The kernel choice is a pure function of the CPU and the env clamp:
/// resolved once per process, then one indirect call (the
/// `count_from_long` dispatch's shape).
#[cfg(all(feature = "std", target_arch = "x86_64"))]
#[cold]
fn kernel() -> Kernel {
    static KERNEL: std::sync::OnceLock<Kernel> = std::sync::OnceLock::new();
    *KERNEL.get_or_init(|| {
        use crate::common::simd::{self, SimdTier};
        if simd::allows(SimdTier::Avx512) && std::is_x86_feature_detected!("avx512f") {
            nt_avx512
        } else if simd::allows(SimdTier::Avx2) && std::is_x86_feature_detected!("avx2") {
            nt_avx2
        } else {
            temporal
        }
    })
}

/// AVX-512 NT-store kernel: scalar head and tail around the 64-aligned
/// body. `sfence` orders the weakly-ordered WC stores before the caller
/// hands the span to the threads that read it.
#[cfg(all(feature = "std", target_arch = "x86_64"))]
#[target_feature(enable = "avx512f")]
unsafe fn nt_avx512(src: *const u8, dst: *mut u8, len: usize) {
    let head = dst.align_offset(64);
    if head == usize::MAX || head >= len {
        // No 64-aligned body reachable: NT stores demand alignment, so a
        // permanently unaligned destination keeps the plain copy.
        // SAFETY: the caller's contract — initialized, disjoint spans.
        return unsafe { temporal(src, dst, len) };
    }
    let mut i = 0;
    while i < head {
        // SAFETY: `i` stays below `head` < `len`, which bounds both spans.
        unsafe {
            *dst.add(i) = *src.add(i);
        }
        i += 1;
    }
    while i + 64 <= len {
        // SAFETY: `src` and `dst` are valid for `len` bytes; `_mm512_`
        // intrinsics read/write exactly the 64 bytes at `i`, both below
        // `len` by the loop bound.
        unsafe {
            let v = core::arch::x86_64::_mm512_loadu_si512(src.add(i).cast());
            core::arch::x86_64::_mm512_stream_si512(dst.add(i).cast(), v);
        }
        i += 64;
    }
    while i < len {
        // SAFETY: `i` < `len` bounds both spans.
        unsafe {
            *dst.add(i) = *src.add(i);
        }
        i += 1;
    }
    // `_mm_sfence` is a safe intrinsic: it fences, accesses nothing.
    core::arch::x86_64::_mm_sfence();
}

/// AVX-2 NT-store kernel (256-bit), the same shape for the avx2 tier.
#[cfg(all(feature = "std", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn nt_avx2(src: *const u8, dst: *mut u8, len: usize) {
    let head = dst.align_offset(32);
    if head == usize::MAX || head >= len {
        // SAFETY: the caller's contract — initialized, disjoint spans.
        return unsafe { temporal(src, dst, len) };
    }
    let mut i = 0;
    while i < head {
        // SAFETY: `i` stays below `head` < `len`, which bounds both spans.
        unsafe {
            *dst.add(i) = *src.add(i);
        }
        i += 1;
    }
    while i + 32 <= len {
        // SAFETY: valid for `len` bytes; the 32-byte step stays below it.
        unsafe {
            let v = core::arch::x86_64::_mm256_loadu_si256(src.add(i).cast());
            core::arch::x86_64::_mm256_stream_si256(dst.add(i).cast(), v);
        }
        i += 32;
    }
    while i < len {
        // SAFETY: `i` < `len` bounds both spans.
        unsafe {
            *dst.add(i) = *src.add(i);
        }
        i += 1;
    }
    // `_mm_sfence` is a safe intrinsic: it fences, accesses nothing.
    core::arch::x86_64::_mm_sfence();
}

#[cfg(test)]
mod tests {
    use super::NT_MIN;

    /// The kernel is a copy: every path it can take must reproduce the
    /// source bytes exactly, including the scalar head/tail around the
    /// aligned body and the sub-threshold plain path.
    #[test]
    fn nt_copy_reproduces_bytes() {
        let mut src = alloc::vec![0u8; NT_MIN + 128];
        let mut x = 0x1234_5678_9abc_def0u64;
        for b in src.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        for len in [
            0,
            1,
            31,
            64,
            4096,
            NT_MIN - 1,
            NT_MIN,
            NT_MIN + 1,
            NT_MIN + 63,
        ] {
            let mut dst = alloc::vec![0u8; len + 64];
            // Offset the destination so the scalar head covers a real
            // misalignment before the aligned body.
            for off in [0, 3, 64] {
                let end = off + len;
                super::copy(&src[..len], &mut dst[off..end]);
                assert_eq!(&dst[off..end], &src[..len], "len {len} off {off}");
            }
        }
        // The whole-source copy through the kernel path.
        let mut dst = alloc::vec![0u8; src.len()];
        super::copy(&src, &mut dst);
        assert_eq!(dst, src);
    }

    /// Every compiled kernel must reproduce the source bytes on its own:
    /// the dispatch's OnceLock picks one per process, so the copy test
    /// pins only the tier this host selects — the avx2 and temporal
    /// bodies (a different alignment width) would never run in-process
    /// without this sweep.
    #[cfg(all(feature = "std", target_arch = "x86_64"))]
    #[test]
    fn nt_kernels_reproduce_bytes_individually() {
        let mut src = alloc::vec![0u8; NT_MIN + 130];
        let mut x = 0x1234_5678_9abc_def0u64;
        for b in src.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        let kernels: [super::Kernel; 3] = [super::nt_avx512, super::nt_avx2, super::temporal];
        for len in [
            0,
            1,
            31,
            64,
            4096,
            NT_MIN - 1,
            NT_MIN,
            NT_MIN + 1,
            NT_MIN + 63,
        ] {
            let mut dst = alloc::vec![0u8; len + 64];
            for off in [0, 3, 64] {
                for &kernel in &kernels {
                    dst.fill(0);
                    // SAFETY: equal, initialized, disjoint `len` spans —
                    // the kernel's own contract.
                    unsafe { kernel(src.as_ptr(), dst[off..].as_mut_ptr(), len) };
                    assert_eq!(&dst[off..off + len], &src[..len], "len {len} off {off}");
                }
            }
        }
    }
}
