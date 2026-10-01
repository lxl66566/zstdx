//! Runtime SIMD tier clamp shared by every runtime-dispatched kernel on
//! the encode side: the highest tier a dispatch may select, lowered from
//! hardware by the `ZSTDX_SIMD_FORCE` dev override (`auto|avx512|avx2|
//! scalar`, parsed once per process). Unset — the default, and every
//! library user — clamps nothing: one acquire load per dispatch, the
//! environment is read exactly once.
//!
//! Tier semantics at a dispatch site: the site walks its kernels
//! descending (avx512 → avx2 → scalar) and takes the first whose tier the
//! clamp still admits AND whose features the CPU reports, so forcing a
//! tier the hardware lacks falls through exactly like a machine without
//! it — which is the point: measuring the intermediate tiers on
//! AVX-512 hardware. BMI2-only sites stay outside the clamp (every
//! AVX2-era CPU carries BMI2; it has no vector width to tier).

use std::sync::OnceLock;

/// Dispatch tier, ascending by width. The ordering IS the clamp
/// comparison: a site may select `tier` when [`allows`] returns true.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum SimdTier {
    /// Baseline scalar (autovectorizer output only).
    Scalar,
    /// 256-bit kernels (`avx2`).
    Avx2,
    /// 512-bit kernels (`avx512f`/`avx512bw`/`avx512vbmi` as the site
    /// requires; the clamp does not distinguish feature subsets).
    Avx512,
}

impl SimdTier {
    /// Clamp value for an environment setting. Unknown values abort
    /// loudly: a typo silently benching the default tier is exactly the
    /// failure this override exists to prevent.
    fn from_env(raw: &str) -> Self {
        match raw.trim() {
            "" | "auto" | "avx512" => Self::Avx512,
            "avx2" => Self::Avx2,
            "scalar" => Self::Scalar,
            other => panic!("ZSTDX_SIMD_FORCE={other:?}: expected auto|avx512|avx2|scalar"),
        }
    }
}

static CLAMP: OnceLock<SimdTier> = OnceLock::new();

/// Whether the clamp still admits `tier` — the dispatch-site guard.
#[inline]
pub(crate) fn allows(tier: SimdTier) -> bool {
    *CLAMP
        .get_or_init(|| SimdTier::from_env(&std::env::var("ZSTDX_SIMD_FORCE").unwrap_or_default()))
        >= tier
}

#[cfg(test)]
mod tests {
    use super::SimdTier;

    #[test]
    fn env_mapping() {
        assert_eq!(SimdTier::from_env(""), SimdTier::Avx512);
        assert_eq!(SimdTier::from_env("auto"), SimdTier::Avx512);
        assert_eq!(SimdTier::from_env(" avx2 "), SimdTier::Avx2);
        assert_eq!(SimdTier::from_env("scalar"), SimdTier::Scalar);
        assert!(SimdTier::Scalar < SimdTier::Avx2);
        assert!(SimdTier::Avx2 < SimdTier::Avx512);
    }

    #[test]
    #[should_panic(expected = "ZSTDX_SIMD_FORCE")]
    fn env_rejects_unknown() {
        SimdTier::from_env("sse2");
    }
}
