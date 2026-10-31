//! Gear-hash checkpoint group shared by the LDM matcher and the far-class
//! screen: the four re-associated rolling states over `w[0..4]` from chain
//! state `h`. Each caller carries its own `GEAR_TAB` (the screen's is
//! const-generated, LDM's is libzstd's literal table), so it rides in as a
//! parameter.

/// The four gear checkpoints over the bytes `w[0..4]` from chain state
/// `h`: `(2h+g0, 4h+2g0+g1, 8h+4g0+2g1+g2, 16h+8g0+4g1+2g2+g3)` — exactly
/// the naive serial states, re-associated so the state-to-state dependency
/// is one add per group.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub(super) fn gear4(tab: &[u64; 256], w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    gear4_asm(tab, w, h)
}

/// The portable twin of the x86-64 [`gear4`].
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
pub(super) fn gear4(tab: &[u64; 256], w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    gear4_portable(tab, w, h)
}

/// The portable body: semantically identical to the hand-written x86-64
/// form, but left to LLVM on x86-64 it compiles back into the one-byte
/// serial chain.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
#[inline(always)]
fn gear4_portable(tab: &[u64; 256], w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    debug_assert!(w.len() >= 4);
    let g0 = tab[w[0] as usize];
    let g1 = tab[w[1] as usize];
    let g2 = tab[w[2] as usize];
    let g3 = tab[w[3] as usize];
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

/// x86-64 `gear4`: forces the re-associated schedule (LEA per checkpoint,
/// `shl`+add for the x16 group step, loads hoisted off the chain).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn gear4_asm(tab: &[u64; 256], w: &[u8], h: u64) -> (u64, u64, u64, u64) {
    debug_assert!(w.len() >= 4);
    let (mut t0, mut t1, mut t2, mut t3, mut t4) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (c1, c2, c3, c4);
    // SAFETY: baseline x86-64 instructions only; reads exactly w[0..4] and
    // the 2 KiB `tab`, writes nothing.
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
            tab = in(reg) tab.as_ptr(),
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
    let _ = (t0, t1, t2, t3, t4);
    (c1, c2, c3, c4)
}
