//! Runtime SIMD-tier selection for the decode hot loops.
//!
//! The decode paths dispatch on BMI2 (variable shifts compile to single-uop
//! shlx/shrx; every AVX2-capable CPU ships BMI2, so this is the top tier real
//! silicon reaches — see `Tier::Bmi2`). `ZSTDX_DEC_SIMD_TIER` forces a tier so
//! the path a lower-tier CPU would take can be measured and roundtrip-gated on
//! any machine: `scalar` or `bmi2`, unset means detect. An invalid value panics
//! loudly instead of silently measuring the detected tier.

use std::sync::OnceLock;

/// Decode dispatch tier, ordered by capability.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    /// Baseline x86-64: destructive `cl` shifts.
    Scalar,
    /// BMI2 three-operand variable shifts. Haswell (2013) and Zen (2017)
    /// introduced AVX2 and BMI2 in the same generation, and no later silicon
    /// drops BMI2 — there is no real CPU between this tier and `Scalar`, so
    /// no intermediate (e.g. AVX2-only) decode tier is constructible.
    Bmi2,
}

impl Tier {
    fn parse(value: &str) -> Option<Tier> {
        match value.trim() {
            "scalar" => Some(Tier::Scalar),
            "bmi2" => Some(Tier::Bmi2),
            _ => None,
        }
    }
}

/// Whether the BMI2 decode instantiations may run. The decision is cached:
/// the env var is read and the feature detected once per process.
#[inline]
pub(super) fn bmi2() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let Ok(value) = std::env::var("ZSTDX_DEC_SIMD_TIER") else {
            return std::is_x86_feature_detected!("bmi2");
        };
        match Tier::parse(&value) {
            Some(Tier::Scalar) => false,
            Some(Tier::Bmi2) => {
                // Forcing is a bench affordance; keep it a panic, not SIGILL,
                // on machines that cannot execute the tier.
                assert!(
                    std::is_x86_feature_detected!("bmi2"),
                    "ZSTDX_DEC_SIMD_TIER=bmi2 on a CPU without BMI2"
                );
                true
            },
            None => panic!("ZSTDX_DEC_SIMD_TIER: invalid tier {value:?} (scalar|bmi2 or unset)"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Tier;

    #[test]
    fn parses_exact_names() {
        assert_eq!(Tier::parse("scalar"), Some(Tier::Scalar));
        assert_eq!(Tier::parse("bmi2"), Some(Tier::Bmi2));
        assert_eq!(Tier::parse("  bmi2  "), Some(Tier::Bmi2));
        for invalid in ["avx2", "sse2", "", "auto "] {
            assert_eq!(Tier::parse(invalid), None, "{invalid:?}");
        }
    }
}
