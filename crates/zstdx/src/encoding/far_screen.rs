//! Pre-header far-class screen for the fast rows' mid-size LDM band.
//!
//! The R19 fast/dfast LDM arming keys on declared geometry alone: the
//! frame header serializes before any block parses, so widening the row's
//! stock window (768 KiB-2 MiB) to the far domain could only key on the
//! declared length, which cannot separate a 32 MiB far-class source from
//! a same-size random one (the window descriptor byte would move on
//! both). The bulk path and a pledged stream hold their head bytes
//! before that decision point, so they run this screen there: a
//! content-aligned sampling pass over the first [`SCREEN_SPAN`] bytes
//! that accepts only wide-alphabet heads with dense 64-byte twins.
//!
//! Sampling mirrors the LDM split pass's own trick: a per-byte gear hash
//! (bit `n` of the state depends on the last `n` bytes, so the whole
//! state is a function of exactly the last 64 bytes) whose trigger
//! condition — top 6 bits all zero — fires at content-relative positions,
//! so two copies of one fragment trigger identically whatever their
//! absolute offsets. A position-grid sample would miss every misaligned
//! twin. Fingerprint equality is 64-bit, so a random head produces
//! collision-level twin counts (measured: exactly 0 twins on the 32 MiB
//! corpus random at every sampled span).

use alloc::vec;

use super::match_generator::{
    LDM_FULL_WINDOW, LDM_MIDSIZE_WINDOW, LDM_SYMS_MIN, LdmArming, fast_row_far_class,
};

/// Head sample the screen reads. dll32's head measures 0.6% / 1.3% /
/// 22.6% twin density at 1 / 2 / 4 MiB — the 2-4 MiB distance class
/// dominates (13.0K of 13.6K twins at 4 MiB), so the span must reach
/// past it for the verdict to see more than the near-field tail.
pub(crate) const SCREEN_SPAN: usize = 4 << 20;

/// Twins (duplicate window fingerprints) an accepted head must show:
/// dll32's 4 MiB head carries ~13.6K (a 50x margin; the early exit
/// accepts mid-scan), random exactly 0. Sources an order of magnitude
/// sparser than dll32's head still clear the bar; sparser ones stay
/// stock (a missed arming, not a broken one).
const TWIN_ACCEPT: usize = 256;

/// Cheap rejection: a head whose first [`REJECT_SPAN`] bytes show fewer
/// than [`REJECT_TWINS`] twins is sparse — the rejected class pays the
/// span scan, not the whole sample (a random 32 MiB head at the fastest
/// row measured −19% wall before it). dll32's first MiB carries ~100
/// twins (the near-field class is dense from byte zero); the miss-class
/// is a far-class source whose head opens with a novel run this long —
/// it stays stock (a missed arming, never a broken one).
const REJECT_SPAN: usize = 1 << 20;
const REJECT_TWINS: usize = 8;

/// The screen's verdict on a head sample.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FarHead {
    /// Wide alphabet plus dense content-aligned 64-byte twins: the source
    /// carries the far class, and the mid-size band may arm.
    FarRepeats,
    /// Narrow alphabet (the runtime alphabet gate's own bar — structural
    /// low-alphabet repeats never survive their offset price) or
    /// collision-level twin density.
    Sparse,
}

/// Resolve a frame's LDM arming context from its head sample: callers
/// gate on `match_generator::fast_row_screen_pending` (the fast rows'
/// mid-size band), so a `FarRepeats` head arms the band via
/// [`LdmArming::FrameScreened`].
pub(crate) fn frame_arming(head: &[u8]) -> LdmArming {
    if screen(head) == FarHead::FarRepeats {
        LdmArming::FrameScreened
    } else {
        LdmArming::Frame
    }
}

/// The bulk-mt planner's fast-row capture window (the R21 port of the
/// R19/R20 far class to multithreaded frames): the far window a captured
/// frame's jobs widen to, when the frame's own evidence arms it — the
/// full band (declared length >= LDM_FULL_WINDOW) on geometry exactly as
/// the frame-continuous entry, the mid-size band on this screen's verdict
/// over `span` (the caller's head sample, clamped to [`SCREEN_SPAN`]).
/// `None` for every other class: the chain rows derive their capture
/// window from their own row parameters, and a rejected or below-band
/// fast row keeps the stock per-job parse. Deterministic in the input and
/// level alone, so the grid and the frame bytes stay worker-independent.
pub(crate) fn mt_capture_window(
    level: crate::Level,
    shape: crate::InputShape,
    span: &[u8],
) -> Option<u64> {
    let len = shape.len?;
    if !fast_row_far_class(level, shape) || len < LDM_MIDSIZE_WINDOW as u64 {
        return None;
    }
    if len >= LDM_FULL_WINDOW as u64 {
        return Some(LDM_FULL_WINDOW as u64);
    }
    let span = &span[..span.len().min(SCREEN_SPAN)];
    (screen(span) == FarHead::FarRepeats).then_some(LDM_FULL_WINDOW as u64)
}

/// One random u64 per byte value (libzstd's `ZSTD_ldm_gearTab` idiom).
/// Const-generated with splitmix64: the table's job is randomness, not
/// secrecy, and a generated table keeps 2 KiB of literals out of the
/// source.
static GEAR_TAB: [u64; 256] = gear_table();

const fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

const fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut i = 0usize;
    while i < 256 {
        t[i] = mix64(i as u64 ^ 0x1d8e_4e27_c47d_124f);
        i += 1;
    }
    t
}

/// Window size the trigger and fingerprint read (LDM's
/// `MIN_MATCH_LENGTH`).
const WIN: usize = 64;
/// Trigger mask: the gear state's top 6 bits — one trigger per ~64 bytes
/// on random content (LDM's `HASH_RATE_LOG`).
const TRIGGER_MASK: u64 = u64::MAX << 58;

/// The four gear checkpoints over the bytes `w[0..4]` from chain state
/// `h`: `(2h+g0, 4h+2g0+g1, 8h+4g0+2g1+g2, 16h+8g0+4g1+2g2+g3)` — the
/// naive serial states re-associated so the state-to-state dependency
/// is one add per group. See `ldm`'s own `gear4` for why the x86-64
/// form is hand-written: left to LLVM, the portable body compiles back
/// into the one-byte serial chain.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
#[inline(always)]
fn gear4_portable(w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    debug_assert!(w.len() >= 4);
    let g0 = GEAR_TAB[w[0] as usize];
    let g1 = GEAR_TAB[w[1] as usize];
    let g2 = GEAR_TAB[w[2] as usize];
    let g3 = GEAR_TAB[w[3] as usize];
    let p2 = g1.wrapping_add(g0 << 1);
    let p3 = g2.wrapping_add(p2 << 1);
    let p4 = g3.wrapping_add(p3 << 1);
    (
        g0.wrapping_add(h << 1),
        p2.wrapping_add(h << 2),
        p3.wrapping_add(h << 3),
        p4.wrapping_add(h << 4),
    )
}

/// x86-64 `gear4`: forces the re-associated schedule (LEA per
/// checkpoint, `shl`+add for the x16 group step, loads hoisted off the
/// chain). Semantically identical to the portable body above.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn gear4(w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    let (mut t0, mut t1, mut t2, mut t3, mut t4) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (c1, c2, c3, c4);
    // SAFETY: baseline x86-64 instructions only; reads exactly w[0..4]
    // and the 2 KiB GEAR_TAB, writes nothing.
    unsafe {
        core::arch::asm!(
            "movzbl ({wp}), {t0:e}",
            "movzbl 1({wp}), {t1:e}",
            "movzbl 2({wp}), {t2:e}",
            "movzbl 3({wp}), {t3:e}",
            "mov ({tab}, {t0}, 8), {t0}",
            "mov ({tab}, {t1}, 8), {t1}",
            "mov ({tab}, {t2}, 8), {t2}",
            "mov ({tab}, {t3}, 8), {t3}",
            "lea ({t0}, {h}, 2), {c1}",
            "lea ({t1}, {t0}, 2), {t4}",
            "lea ({t4}, {h}, 4), {c2}",
            "lea ({t2}, {t4}, 2), {t4}",
            "lea ({t4}, {h}, 8), {c3}",
            "lea ({t3}, {t4}, 2), {t4}",
            "mov {h}, {t0}",
            "shlq $4, {t0}",
            "lea ({t4}, {t0}), {c4}",
            wp = in(reg) w.as_ptr(),
            tab = in(reg) GEAR_TAB.as_ptr(),
            h = in(reg) h,
            t0 = out(reg) t0,
            t1 = out(reg) t1,
            t2 = out(reg) t2,
            t3 = out(reg) t3,
            t4 = out(reg) t4,
            // early (not late) outs: the template reads `h` after writing
            // `c1`/`c2`, so a lateout sharing `h`'s register would corrupt
            // `c3`/`c4` (lateouts may alias inputs by contract).
            c1 = out(reg) c1,
            c2 = out(reg) c2,
            c3 = out(reg) c3,
            c4 = out(reg) c4,
            options(nostack, att_syntax)
        );
    }
    let _ = (&mut t0, &mut t1, &mut t2, &mut t3, &mut t4);
    (c1, c2, c3, c4)
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn gear4(w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    gear4_portable(w, h)
}

/// 64-byte window fingerprint: an 8-lane multiply-xor fold with a
/// splitmix-style finalizer (screen-local; LDM's own `split_fp` differs,
/// which costs nothing — the screen only ever compares its own
/// fingerprints).
#[inline(always)]
fn window_fp(win: &[u8]) -> u64 {
    debug_assert_eq!(win.len(), WIN);
    let lane = |k: usize| u64::from_le_bytes(win[k..k + 8].try_into().unwrap());
    let acc = lane(0).wrapping_mul(0xa076_1d64_78bd_642f)
        ^ lane(1).wrapping_mul(0xe703_7ed1_a0b4_28db)
        ^ lane(2).wrapping_mul(0x8ebc_6af0_9c88_c6e3)
        ^ lane(3).wrapping_mul(0x5899_65cc_7587_4f13)
        ^ lane(4).wrapping_mul(0x1d8e_4e27_c47d_124f)
        ^ lane(5).wrapping_mul(0xeb44_acca_b455_d165)
        ^ lane(6).wrapping_mul(0xc685_6960_5a92_1e65)
        ^ lane(7).wrapping_mul(0x7394_4f5b_82d3_8245);
    mix64(acc)
}

/// Strided distinct-byte count, the incompressibility gate's sampling
/// idiom: an eighth of the bytes reads a 128-symbol alphabet to ~110, a
/// 256-symbol one to ~248.
fn strided_distinct(head: &[u8]) -> u32 {
    let mut seen = [false; 256];
    let mut n = 0u32;
    for &b in head.iter().step_by(8) {
        let seen = &mut seen[b as usize];
        if !*seen {
            *seen = true;
            n += 1;
        }
    }
    n
}

/// Insert `fp` into the open-addressed table; `true` when an equal
/// fingerprint already sat there (a twin). Slot 0 doubles as the empty
/// sentinel, so a fingerprint that hashes to 0 itself is skipped (a
/// 2^-64 event, deterministic either way).
#[inline]
fn twin_insert(table: &mut [u64], mask: usize, fp: u64) -> bool {
    if fp == 0 {
        return false;
    }
    let mut slot = (fp as usize) & mask;
    loop {
        let e = table[slot];
        if e == fp {
            return true;
        }
        if e == 0 {
            table[slot] = fp;
            return false;
        }
        slot = (slot + 1) & mask;
    }
}

/// Fingerprint-and-insert the window ending at byte `p` (inclusive);
/// `false` before the first full window fills or when the window is not
/// a twin.
#[inline]
fn twin_at(head: &[u8], table: &mut [u64], mask: usize, p: usize) -> bool {
    p + 1 >= WIN && {
        let fp = window_fp(&head[p + 1 - WIN..=p]);
        twin_insert(table, mask, fp)
    }
}

/// The screen over a head sample (`head.len() <= SCREEN_SPAN`; callers
/// clamp). The alphabet bar runs first (a strided pass, µs-scale) —
/// low-alphabet heads reject without the sampling pass — then the
/// content-aligned twin count: dll-class heads accept mid-span at
/// [`TWIN_ACCEPT`], a twinless opening rejects at [`REJECT_SPAN`], and
/// only a head that showed early signal pays the full scan to its end.
pub(crate) fn screen(head: &[u8]) -> FarHead {
    if head.len() < WIN || strided_distinct(head) < LDM_SYMS_MIN {
        return FarHead::Sparse;
    }
    // One slot per ~48 bytes of head (expected inserts: one per ~64
    // bytes) keeps the load factor under two thirds; 4 MiB heads land at
    // 128 Ki slots — 1 MiB, L2-resident.
    let slots = (head.len() / 48).next_power_of_two();
    let mask = slots - 1;
    let mut table = vec![0u64; slots];
    let mut twins = 0usize;
    let mut h = 0xffff_ffff_0000_0000u64;
    let mut i = 0usize;
    while i + 4 <= head.len() {
        let (c1, c2, c3, c4) = gear4(&head[i..], h);
        h = c4;
        if c1 & TRIGGER_MASK == 0 && twin_at(head, &mut table, mask, i) {
            twins += 1;
        }
        if c2 & TRIGGER_MASK == 0 && twin_at(head, &mut table, mask, i + 1) {
            twins += 1;
        }
        if c3 & TRIGGER_MASK == 0 && twin_at(head, &mut table, mask, i + 2) {
            twins += 1;
        }
        if c4 & TRIGGER_MASK == 0 && twin_at(head, &mut table, mask, i + 3) {
            twins += 1;
        }
        i += 4;
        if twins >= TWIN_ACCEPT {
            return FarHead::FarRepeats;
        }
        if i == REJECT_SPAN && twins < REJECT_TWINS {
            return FarHead::Sparse;
        }
    }
    FarHead::Sparse
}
