//! TDD spec (Phase 2 Step 11, RED): the size-bucket single source of truth.
//!
//! These tests pin the public `loom_core::vault` API that Item 1 of the
//! vault-dedup plan introduces — the now-`pub` `SizeBucket` enum (the audit-payload
//! form) with a `pub fn as_str()` (the `&'static str` form the daemon receipt uses)
//! and a single `pub fn size_bucket(usize) -> SizeBucket`. They fail to COMPILE
//! until Phase 3 makes those items public, which is the intended RED state. Once
//! green they guard against the two failure modes the refactor exists to prevent:
//! a silently shifted threshold, and the daemon string drifting from the core enum.
//!
//! API shape revised per plan-council finding (skeptic/architect): expose ONE enum
//! + `as_str()` rather than two parallel free fns.
//!
//! Wire-format invariant: the serialized strings MUST stay "small"/"medium"/"large"
//! (the enum feeds the hash-chained audit manifest; changing them breaks integrity).

use loom_core::vault::{size_bucket, SizeBucket};

#[test]
fn as_str_boundaries() {
    // <= 256 -> small
    assert_eq!(size_bucket(0).as_str(), "small");
    assert_eq!(size_bucket(1).as_str(), "small");
    assert_eq!(size_bucket(256).as_str(), "small");
    // 257..=4096 -> medium
    assert_eq!(size_bucket(257).as_str(), "medium");
    assert_eq!(size_bucket(4096).as_str(), "medium");
    // > 4096 -> large
    assert_eq!(size_bucket(4097).as_str(), "large");
    assert_eq!(size_bucket(1_000_000).as_str(), "large");
}

#[test]
fn size_bucket_enum_matches_thresholds() {
    assert_eq!(size_bucket(256), SizeBucket::Small);
    assert_eq!(size_bucket(257), SizeBucket::Medium);
    assert_eq!(size_bucket(4096), SizeBucket::Medium);
    assert_eq!(size_bucket(4097), SizeBucket::Large);
}

#[test]
fn enum_serializes_to_same_wire_strings_as_as_str() {
    // The single source of truth: enum serde output == as_str() for every input.
    for n in [0usize, 256, 257, 4096, 4097, 1_000_000] {
        let via_enum = serde_json::to_value(size_bucket(n)).unwrap();
        assert_eq!(
            via_enum,
            serde_json::Value::String(size_bucket(n).as_str().to_string()),
            "enum serde and as_str() disagree at byte_count={n}",
        );
    }
    // Pin the exact wire literals (audit-manifest contract).
    assert_eq!(
        serde_json::to_value(SizeBucket::Small).unwrap(),
        serde_json::json!("small")
    );
    assert_eq!(
        serde_json::to_value(SizeBucket::Medium).unwrap(),
        serde_json::json!("medium")
    );
    assert_eq!(
        serde_json::to_value(SizeBucket::Large).unwrap(),
        serde_json::json!("large")
    );
}
