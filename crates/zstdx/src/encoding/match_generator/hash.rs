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

/// Input width of the chain-table hash and the dfast short-table hash:
/// 5 bytes on no-dict frames (the calibration that beat libzstd's width
/// there), 4 on dictionary frames — libzstd hashes `minMatch` bytes, and
/// its small-input tables select width-4 rows for the lazy family at
/// levels 5-12 and dfast at 4. A small payload parses mostly against
/// dictionary content, where the 4-byte candidate classes (near-duplicate
/// lines diverging at byte 5) convert; the no-dict width stays put
/// (byte-identical no-dict output).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChainHashWidth {
    Five,
    Four,
}

impl ChainHashWidth {
    #[inline(always)]
    pub(super) fn of(dict_row: bool) -> Self {
        if dict_row {
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
///
/// On x86-64 the scan loop is hand-written with a phase-proof entry: a hot
/// loop straddling a 64-byte icache/op-cache line boundary loses ~8% of the
/// enclosing cell to frontend uop delivery on Zen4 (R32, text.fast stream
/// solo; binary-patch confirmation: the same loop fully inside one 64B
/// line, or straddling only a 32B fetch window inside a line, runs at base
/// speed), and no stable lever pins a loop's line phase (rustc drops
/// inline-asm `.p2align` directives). The x86-64 path below emits two
/// identical 27-29-byte loop copies whose heads land 32-33 bytes apart and
/// enters, at runtime, 32 bytes into copy 1 — its head or, when a load
/// base is rbp/r13 and copy 2 slips to +33, the pad byte just before that
/// head — so the entry always sits at line phase <= 31 and the executing
/// surrounding code. Even the never-observed both-bases-disp8 case (copy 2
/// at +34, entry two pad bytes early, head phase <= 33) keeps the whole
/// copy inside one line: 33 + 29 <= 63.
/// loop never straddles a 64B line, whatever address the linker gives the
/// surrounding code.
#[inline(always)]
pub(in crate::encoding) fn extend_match(win: &[u8], i: usize, j: usize) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        extend_match_aligned(win, i, j)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        extend_match_portable(win, i, j)
    }
}

#[inline(always)]
// Dead in x86-64 lib builds (the asm path serves them); the differential
// test keeps it compiled there.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn extend_match_portable(win: &[u8], i: usize, j: usize) -> usize {
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

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn extend_match_aligned(win: &[u8], i: usize, j: usize) -> usize {
    let limit = win.len() - i;
    let mut len = 0usize;
    if limit >= 8 {
        let lim8 = limit - 8;
        let diff: u64;
        // SAFETY: baseline x86-64 instructions only. Reads exactly
        // win[i+len..i+len+8] and win[j+len..j+len+8] while len <= limit-8
        // (i + len + 8 <= win.len(); j <= i bounds the other read); writes no
        // memory and touches no stack.
        unsafe {
            core::arch::asm!(
                // Entry stub: entering at +32 from copy 1 (its head or, if
                // copy 2 slipped to +33, the pad byte just before it) always
                // lands at line phase <= 31, so the executing copy never
                // straddles a 64B line. Branchless select: k = t & 32 is 0
                // or 32 (the line-phase bit); the indirect jump's target is
                // constant per load, so it predicts.
                "leaq 1f(%rip), {t}",
                "movq {t}, {k}",
                "andq $32, {k}",
                "addq {k}, {t}",
                "jmp *{t}",
                // Loop copy 1: 27 bytes with register-independent encodings
                // (cmp/ja guard before the loads, optimistic advance before
                // the compare) + 5 pad bytes; copy 2 starts at +32, or +33
                // when a load base is rbp/r13 (disp8) — the entry then
                // executes the last pad nop first.
                "1:",
                "cmpq {lim}, {len}",
                "ja 4f",
                "movq ({wi},{len},1), {a}",
                "movq ({wj},{len},1), {b}",
                "addq $8, {len}",
                "cmpq {b}, {a}",
                "je 1b",
                "xorq {b}, {a}",
                "jmp 5f",
                ".byte 0x90,0x90,0x90,0x90,0x90",
                // Loop copy 2; its backedge and exit displacements differ from
                // copy 1's, everything else is identical.
                "2:",
                "cmpq {lim}, {len}",
                "ja 4f",
                "movq ({wi},{len},1), {a}",
                "movq ({wj},{len},1), {b}",
                "addq $8, {len}",
                "cmpq {b}, {a}",
                "je 2b",
                "xorq {b}, {a}",
                "jmp 5f",
                "4:", // length-limited exit
                "xorq {a}, {a}",
                "5:",
                wi = in(reg) win.as_ptr().add(i),
                wj = in(reg) win.as_ptr().add(j),
                lim = in(reg) lim8,
                len = inout(reg) len,
                // early (not late) outs: the template reads {wj} after writing
                // {a} (first load then second load), so a lateout sharing {wj}'s
                // register would corrupt the second load's base.
                a = out(reg) diff,
                b = out(reg) _,
                t = out(reg) _,
                k = out(reg) _,
                options(nostack, att_syntax)
            );
        }
        if diff != 0 {
            // The mismatching pair already advanced `len` by 8.
            return len - 8 + (diff.trailing_zeros() >> 3) as usize;
        }
    }
    while len < limit && win[i + len] == win[j + len] {
        len += 1;
    }
    len
}
