//! Debug-only poison margins over owned buffers' slack capacity.
//!
//! An overread past a buffer's *logical* length that lands inside its
//! *allocation* (the streaming window's slack, a staged Vec's spare
//! capacity) is invisible to ASAN and usually harmless-looking — the R68
//! row-tail class survived exactly there. Filling the slack past every
//! logical length with a poison pattern and asserting it intact at the
//! next checkpoint turns any such touch into a loud test failure with the
//! checkpoint named.
//!
//! The whole module is declared under `#[cfg(debug_assertions)]`: release
//! builds compile none of it. The `paranoid` profile keeps
//! `debug-assertions` on over release codegen, so it inherits the walls.

/// Margin width poisoned past every logical length: wide enough that any
/// scan family's overread (read4/read8/hash windows) crosses it, small
/// enough to keep per-block fill+check cheap in debug runs.
pub(crate) const MARGIN: usize = 64;

/// The poison pattern.
pub(crate) const PATTERN: u8 = 0xa5;

/// Poison `[len, min(len + MARGIN, buf.len()))` — the slack a logical
/// overread touches first.
pub(crate) fn fill_margin(buf: &mut [u8], len: usize) {
    let span = margin_span(buf, len);
    fill(&mut buf[span]);
}

/// Assert the margin filled by [`fill_margin`] is unchanged. `ctx` names
/// the checkpoint so the panic localizes the offending path.
pub(crate) fn assert_margin(buf: &[u8], len: usize, ctx: &str) {
    let span = margin_span(buf, len);
    assert_intact(&buf[span], ctx);
}

/// Poison an arbitrary span (whole-slice form for layouts the
/// len-keyed margin does not fit, e.g. a wrapped ring head).
pub(crate) fn fill(buf: &mut [u8]) {
    buf.fill(PATTERN);
}

/// Whole-slice companion of [`fill`].
pub(crate) fn assert_intact(buf: &[u8], ctx: &str) {
    if let Some(i) = buf.iter().position(|b| *b != PATTERN) {
        panic!("poison margin disturbed at span byte {i} (checkpoint: {ctx})");
    }
}

fn margin_span(buf: &[u8], len: usize) -> core::ops::Range<usize> {
    let start = len.min(buf.len());
    start..(len + MARGIN).min(buf.len())
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn margin_roundtrip_and_clamp() {
        let mut b = vec![0u8; 32];
        fill_margin(&mut b, 8);
        assert_eq!(&b[8..32], &[PATTERN; 24]);
        assert_margin(&b, 8, "test");
        // Logical length at/past capacity clamps to a (possibly empty) span.
        let full = b.len();
        fill_margin(&mut b, full);
        assert_margin(&b, full, "test");
        fill_margin(&mut b, 64);
        assert_margin(&b, 64, "test");
    }

    #[test]
    #[should_panic(expected = "poison margin disturbed")]
    fn margin_catches_a_touch() {
        let mut b = vec![0u8; 32];
        fill_margin(&mut b, 8);
        b[9] = 0;
        assert_margin(&b, 8, "test");
    }

    #[test]
    fn span_forms_roundtrip() {
        let mut b = vec![0u8; 8];
        fill(&mut b);
        assert_intact(&b, "test");
        b[7] = 0;
        assert!(matches!(
            std::panic::catch_unwind(|| assert_intact(&b, "test")),
            Err(_)
        ));
    }
}
