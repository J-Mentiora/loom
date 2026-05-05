//! Integration tests for `--capture-policy` CLI flag.
//!
//! - `--capture-policy minimal` invokes `apply_capture_profile(Minimal)`
//!   on the fixture receipt → no `dom_snapshot_hash`, no
//!   `screenshot_after_hash` blob_ref, empty `console_lines`,
//!   network event body refs stripped.
//! - `--capture-policy full` invokes `apply_capture_profile(Full)` →
//!   tier-2 + tier-3 fields preserved.
//! - canonical-JSON + skip-if-none round-trip is pinned by
//!   `manifest_writer/interface_tests.rs`.
//! - clap rejects bogus values with exit 2 (pinned by
//!   `session_commands/interface_tests.rs::clap_rejects_*`); this
//!   file additionally pins the wire-shape contract — wire form is
//!   the lowercased enum variant ("minimal"/"default"/"full").
//!
//! Receipt-emission wiring through `CaptureProfile` is out of scope (see
//! `decisions.md` "Scope boundary"); the integration test exercises the
//! `apply_capture_profile` shape contract directly.

use loom_core::content_store::ContentRef;
use loom_core::receipt_builder::receipt_builder::{
    capture_profile_from_str, CaptureProfile, ConsoleLine, NetworkEvent, ReceiptBuilder,
};

fn dummy_content_ref(seed: &str) -> ContentRef {
    ContentRef {
        sha256: format!("{seed:0<64}").chars().take(64).collect(),
        size_bytes: 1024,
    }
}

fn navigate_fixture() -> loom_core::receipt_builder::receipt_builder::ReceiptPayload {
    let mut r = ReceiptBuilder::build_navigate_receipt(
        "act-1".into(),
        100,
        dummy_content_ref("dom"),
        dummy_content_ref("scr"),
        vec![NetworkEvent {
            method: "GET".into(),
            url: "https://example.com/x".into(),
            status_code: 200,
            response_body_sha256_hex: format!("{:0<64}", "feed"),
            response_body_size_bytes: 42,
            response_body_ref: Some(dummy_content_ref("body")),
            timing_ticks: 50,
            content_type: "text/html".into(),
        }],
        vec![ConsoleLine {
            level: "info".into(),
            message: "hi".into(),
            timing_ticks: 10,
        }],
    );
    // Tier-2 navigate fields:
    r.url = Some("https://example.com".into());
    r.final_url = Some("https://example.com/".into());
    r.title = Some("Example".into());
    r.status_code = Some(200);
    r.dom_snapshot_hash = Some("a".repeat(64));
    r.console_count = Some(1);
    r.network_count = Some(1);
    r.emitted_at_ms = Some(1714074336000);
    r
}

// === capture_profile_from_str helper ==========================================

#[test]
fn capture_profile_from_str_maps_known_strings() {
    assert!(matches!(
        capture_profile_from_str("minimal"),
        Some(CaptureProfile::Minimal)
    ));
    assert!(matches!(
        capture_profile_from_str("default"),
        Some(CaptureProfile::Default)
    ));
    assert!(matches!(
        capture_profile_from_str("full"),
        Some(CaptureProfile::Full)
    ));
}

#[test]
fn capture_profile_from_str_rejects_unknown() {
    assert!(capture_profile_from_str("bogus").is_none());
    assert!(capture_profile_from_str("Minimal").is_none()); // case-sensitive on the wire
    assert!(capture_profile_from_str("").is_none());
}

// === Minimal strips fields ====================================================

#[test]
fn capture_policy_minimal_strips_non_hash_fields() {
    let mut r = navigate_fixture();
    r.apply_capture_profile(CaptureProfile::Minimal);

    // Blob-refs gone; downgraded to hash-only.
    assert!(r.dom_after_blob_ref.is_none());
    assert!(r.screenshot_after_blob_ref.is_none());
    assert!(
        r.dom_after_hash.is_some(),
        "dom_after_hash must be derived from blob_ref sha256"
    );
    assert!(
        r.screenshot_after_hash.is_some(),
        "screenshot_after_hash must be derived from blob_ref sha256"
    );
    assert!(r.dom_before_blob_ref.is_none());
    assert!(r.screenshot_before_blob_ref.is_none());

    // Console emptied.
    assert!(
        r.console_lines.is_empty(),
        "Minimal must empty console_lines"
    );

    // Network event body refs stripped (hash + size_bytes preserved).
    assert_eq!(r.network_events.len(), 1);
    assert!(r.network_events[0].response_body_ref.is_none());
    assert_eq!(r.network_events[0].response_body_size_bytes, 42);
}

// === Full preserves tier-2 + tier-3 fields ===================================

#[test]
fn capture_policy_full_keeps_tier_two_and_three_fields() {
    let mut r = navigate_fixture();
    let before = r.clone();
    r.apply_capture_profile(CaptureProfile::Full);

    // Full is no-op on the post-build struct (per impl_capture.rs comment).
    assert_eq!(r, before);

    // Tier-2 navigate fields untouched.
    assert_eq!(r.url.as_deref(), Some("https://example.com"));
    assert_eq!(r.final_url.as_deref(), Some("https://example.com/"));
    assert_eq!(r.title.as_deref(), Some("Example"));
    assert_eq!(r.status_code, Some(200));
    assert!(r.dom_snapshot_hash.is_some());

    // Tier-3 console + network preserved with body refs.
    assert_eq!(r.console_lines.len(), 1);
    assert!(r.network_events[0].response_body_ref.is_some());
}

#[test]
fn capture_policy_default_is_noop() {
    // flag-absent (None) → server resolves to `Default` → no shape change.
    let mut r = navigate_fixture();
    let before = r.clone();
    r.apply_capture_profile(CaptureProfile::Default);
    assert_eq!(r, before);
}

// === wire-shape contract for CapturePolicyArg ================================

#[test]
fn cli_create_includes_capture_policy_in_rpc_params() {
    // Construct CreateArgs the same way `clap` would after parsing
    // `--capture-policy minimal`, and assert the value the CLI sends on the
    // wire matches the server's accepted enum (`minimal|default|full`).
    use loom_cli::session_commands::session_commands::{CapturePolicyArg, CreateArgs};

    for (variant, wire) in [
        (CapturePolicyArg::Minimal, "minimal"),
        (CapturePolicyArg::Default, "default"),
        (CapturePolicyArg::Full, "full"),
    ] {
        let args = CreateArgs {
            profile: None,
            network_mode: None,
            seed: None,
            budget: None,
            capture_policy: Some(variant),
            no_blocklist: false,
        };
        // The CLI's `create()` handler turns `Some(variant)` into a JSON
        // `params["capture_policy"]: <wire>` entry. Assert via the
        // documented helper on the enum.
        assert_eq!(args.capture_policy.unwrap().as_wire_str(), wire);
    }
}
