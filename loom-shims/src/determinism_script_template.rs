//! Pure template substitution for `assets/determinism_init.js`.
//!
//! No Tokio, no CDP, no I/O. Testable in isolation. Called from
//! `DeterminismInjector::inject` once per target, rendering the per-session
//! seed + epoch_ms into the otherwise-static JS template.

/// Substitute the seed tokens (`__LOOM_SEED_LO__`, `__LOOM_SEED_HI__`) with
/// concrete numeric values. `seed` is split into two `u32` halves so it
/// survives JS's 53-bit integer ceiling without float-coercion. The seed_lo/hi
/// are emitted as hex literals (`0x...`) so V8 parses them directly into 32-bit
/// ints.
///
/// `epoch_ms` is retained in the signature because the caller threads it on to
/// CDP `Emulation.setVirtualTimePolicy { initialVirtualTime }` (the clock origin
/// now lives in virtual time, not in the injected JS). The legacy
/// `__LOOM_EPOCH_MS__` token is substituted too if present, so an older asset
/// still renders correctly; the current template does not use it.
pub fn render_determinism_script(template: &str, seed: u64, epoch_ms: u64) -> String {
    let seed_lo = (seed & 0xFFFF_FFFF) as u32;
    let seed_hi = ((seed >> 32) & 0xFFFF_FFFF) as u32;
    template
        .replace("__LOOM_SEED_LO__", &format!("0x{seed_lo:08x}"))
        .replace("__LOOM_SEED_HI__", &format!("0x{seed_hi:08x}"))
        .replace("__LOOM_EPOCH_MS__", &epoch_ms.to_string())
}

/// Validate that `source` is the *raw determinism template*: contains the
/// canonical determinism markers AND the seed substitution tokens. Used at boot
/// to refuse-to-start with a corrupted template asset. Returns false on
/// already-rendered scripts (tokens are gone post-substitution); rendered
/// scripts are not supposed to be re-validated.
///
/// Markers reflect what the script still installs after clock determinism moved
/// to CDP virtual time (faithful-entrance-animations): the seeded RNG
/// (`Math.random`) plus the two seed tokens. `Date.now` / `requestAnimationFrame`
/// are deliberately NO LONGER required — the script no longer overrides them.
pub fn is_determinism_template(source: &str) -> bool {
    source.contains("Math.random")
        && source.contains("__LOOM_SEED_LO__")
        && source.contains("__LOOM_SEED_HI__")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = include_str!("../assets/determinism_init.js");

    #[test]
    fn template_asset_validates() {
        assert!(is_determinism_template(TEMPLATE));
    }

    #[test]
    fn render_substitutes_seed_lo_and_hi() {
        let out = render_determinism_script(TEMPLATE, 42, 1000);
        // 42 = 0x000000000000002a → lo=0x0000002a, hi=0x00000000
        assert!(out.contains("0x0000002a"));
        assert!(out.contains("0x00000000"));
        assert!(!out.contains("__LOOM_SEED_LO__"));
        assert!(!out.contains("__LOOM_SEED_HI__"));
    }

    #[test]
    fn render_substitutes_high_seed_correctly() {
        // seed = 0x_dead_beef_cafe_0042 → lo=0xcafe0042, hi=0xdeadbeef
        let out = render_determinism_script(TEMPLATE, 0xdead_beef_cafe_0042, 0);
        assert!(out.contains("0xcafe0042"));
        assert!(out.contains("0xdeadbeef"));
    }

    #[test]
    fn render_is_a_noop_for_epoch_in_current_template() {
        // Clock origin now lives in CDP virtual time, not the JS asset, so the
        // current template has no `__LOOM_EPOCH_MS__` token. render() still
        // accepts epoch_ms (the caller threads it on to setVirtualTimePolicy)
        // and the substitution is a harmless no-op here.
        let out = render_determinism_script(TEMPLATE, 0, 1_700_000_000_000);
        assert!(!out.contains("__LOOM_EPOCH_MS__"));
        // The legacy substitution still works if a token were present:
        let legacy = render_determinism_script("origin=__LOOM_EPOCH_MS__", 0, 1_700_000_000_000);
        assert_eq!(legacy, "origin=1700000000000");
    }

    #[test]
    fn different_seeds_produce_different_output() {
        let a = render_determinism_script(TEMPLATE, 0, 1000);
        let b = render_determinism_script(TEMPLATE, 42, 1000);
        assert_ne!(a, b);
    }

    #[test]
    fn rendered_output_fails_template_validator() {
        // Rendered scripts have no tokens, so `is_determinism_template`
        // returns false. This is intentional — rendered scripts are not
        // supposed to be re-validated.
        let rendered = render_determinism_script(TEMPLATE, 42, 1000);
        assert!(!is_determinism_template(&rendered));
    }

    #[test]
    fn validator_rejects_missing_tokens() {
        // Has the RNG marker but no seed tokens.
        let bad = "Math.random = _sfc32;";
        assert!(!is_determinism_template(bad));
    }

    #[test]
    fn validator_rejects_missing_markers() {
        // Has the seed tokens but no RNG marker.
        let bad = "__LOOM_SEED_LO__ __LOOM_SEED_HI__";
        assert!(!is_determinism_template(bad));
    }

    #[test]
    fn seed_zero_round_trips() {
        let out = render_determinism_script(TEMPLATE, 0, 0);
        assert!(out.contains("0x00000000"));
    }
}
