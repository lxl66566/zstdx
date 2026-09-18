//! Window hashing and word loads shared by every search loop: the
//! multiply-shift hashes, their width variants, and the unaligned reads
//! plus [`extend_match`].

/// Marker for the const-generic dfast logs meaning "read the runtime
/// value" (real logs are never zero); only clamped-window shapes
/// (inputs small enough to shrink the row's tables) take that path.
pub(super) const RUNTIME_LOG: u32 = 0;

/// 5-byte window multiply prime (libzstd `prime5bytes`); only the low 64
/// bits of the product feed the slot index.
pub(super) const HASH_PRIME: u64 = 0xc2b2_ae3d_27d4_eb4f;

/// Hash a window u64 whose low 5 bytes are the hashed prefix (the full u64
/// load feeds the multiplier directly: bits above the fifth byte only add
/// input entropy) into a table of `log` bits. Five bytes skip the frequent
/// 4-byte boilerplate fragments so probes land on structural repeats
/// instead of recent junk.
#[inline(always)]
pub(super) fn hash5_log(v: u64, log: u32) -> usize {
    // No low mask: the >> (64 - log) already leaves exactly `log` bits, and
    // a runtime `log` would make LLVM rebuild the mask per call.
    (v & 0x00ff_ffff_ffff).wrapping_mul(HASH_PRIME) as usize >> (64 - log)
}

/// Input width of the chain-table hash: 5 bytes on no-dict frames (the
/// calibration that beat libzstd's width there), 4 on dictionary frames —
/// libzstd's lazy-family rows hash `minMatch` bytes and every chain row
/// its small-input tables select at levels 5-12 carries searchLength 4.
/// A small payload parses mostly against dictionary content, where the
/// 4-byte candidate classes (near-duplicate lines diverging at byte 5)
/// convert; the no-dict width stays put (byte-identical no-dict output).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChainHashWidth {
    Five,
    Four,
}

impl ChainHashWidth {
    #[inline(always)]
    pub(super) fn of(dict_chain: bool) -> Self {
        if dict_chain {
            ChainHashWidth::Four
        } else {
            ChainHashWidth::Five
        }
    }

    #[inline(always)]
    pub(super) fn mask(self) -> u64 {
        match self {
            ChainHashWidth::Five => 0x00ff_ffff_ffff,
            ChainHashWidth::Four => 0xffff_ffff,
        }
    }
}

#[inline(always)]
pub(super) fn hash_at_width(win: &[u8], idx: usize, log: u32, width: ChainHashWidth) -> usize {
    // SAFETY: see the contract above.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & width.mask();
        v.wrapping_mul(HASH_PRIME) as usize >> (64 - log)
    }
}

/// Hash the 5 bytes at `idx` into a table of `log` bits. Caller guarantees
/// `idx + 5 <= win.len()` (the scanning and emit loops bound-check once per
/// loop, not per position).
#[inline(always)]
pub(super) fn hash_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    hash_at_width(win, idx, log, ChainHashWidth::Five)
}

/// Hash the 8 bytes at `idx` into the dfast long table (libzstd's
/// `prime8bytes` multiply). Same read contract as [`hash_at_log`].
#[inline(always)]
pub(super) fn hash8_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at_log.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned();
        v.wrapping_mul(0xcf1b_bcdc_b7a5_6463) as usize >> (64 - log)
    }
}

/// Read 4 window bytes at `idx`. Caller guarantees `idx + 4 <= win.len()`.
#[inline(always)]
pub(super) fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
}

/// Read 8 window bytes at `idx`. Caller guarantees `idx + 8 <= win.len()`.
#[inline(always)]
pub(super) fn read8(win: &[u8], idx: usize) -> u64 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u64>().read_unaligned() }
}

/// Longest common prefix of `win[i..]` and `win[j..]` in u64 chunks. `i` is
/// the current scan position and `j` a candidate strictly before it, so
/// bounding by `i` also bounds `j`.
#[inline(always)]
pub(in crate::encoding) fn extend_match(win: &[u8], i: usize, j: usize) -> usize {
    let limit = win.len() - i;
    let base = win.as_ptr();
    let mut len = 0;
    // SAFETY: i and j are valid indices and i + len + 8 <= win.len() bounds
    // the reads on both sides (j <= i).
    unsafe {
        while len + 8 <= limit {
            let a = base.add(i + len).cast::<u64>().read_unaligned();
            let b = base.add(j + len).cast::<u64>().read_unaligned();
            if a == b {
                len += 8;
            } else {
                return len + ((a ^ b).trailing_zeros() >> 3) as usize;
            }
        }
    }
    while len < limit && win[i + len] == win[j + len] {
        len += 1;
    }
    len
}
