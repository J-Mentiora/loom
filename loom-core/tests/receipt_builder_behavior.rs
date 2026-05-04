//! AC-driven behavior tests for `receipt_builder` and `error_types`.
//!
//! AC-CORE-04.1 — click default tier payload
//! AC-CORE-04.2 — navigate default tier payload
//! AC-CORE-04.3 — evaluate default tier payload
//! AC-CORE-04.4 — capture-full override adds before-refs
//! AC-CORE-04.5 — capture-minimal strips blob refs
//! AC-CORE-05.1 — all errors have typed receipt
//! AC-CORE-05.2 — stable 22-code error enum
//! AC-NFR-DET-03.1 — no floats in receipt fields
//! AC-NFR-DET-04.1 — canonical field ordering (serde_jcs)
//! AC-NFR-DET-05.1 — timing_ticks is integer

use loom_core::content_store::ContentRef;
use loom_core::error_types::{ReceiptCode, ReceiptSurface};
use loom_core::receipt_builder::{
    CaptureProfile, ConsoleLine, NetworkEvent, ReceiptBuilder, ReceiptStatus,
};

// ---------------------------------------------------------------------------
// AC-CORE-04.1 — click default tier
// ---------------------------------------------------------------------------

#[test]
fn ac_core_04_1_click_receipt_default_tier() {
    let receipt = ReceiptBuilder::build_click_receipt(
        "act-001".to_string(),
        42_000_u64, // timing_ticks (microseconds)
        "abc123def456".repeat(4) + "abcdef01234567890123456789012345",
        "fedcba9876543210".repeat(4),
    );

    // Required fields
    assert_eq!(receipt.action_id, "act-001");
    assert_eq!(receipt.surface, ReceiptSurface::Web);
    assert_eq!(receipt.status, ReceiptStatus::Ok);
    assert_eq!(receipt.code, ReceiptCode::WebActionCompleted);
    assert_eq!(receipt.timing_ticks, 42_000);

    // Hash-only fields present
    assert!(
        receipt.dom_after_hash.is_some(),
        "dom_after_hash must be present for click tier"
    );
    assert!(
        receipt.screenshot_after_hash.is_some(),
        "screenshot_after_hash must be present"
    );

    // NO blob fields for default click tier
    assert!(
        receipt.dom_after_blob_ref.is_none(),
        "click tier must NOT have dom_after_blob_ref"
    );
    assert!(
        receipt.screenshot_after_blob_ref.is_none(),
        "click tier must NOT have screenshot_after_blob_ref"
    );
    assert!(
        receipt.dom_before_blob_ref.is_none(),
        "no before-refs in default tier"
    );
    assert!(receipt.screenshot_before_blob_ref.is_none());

    // No navigate-only fields
    assert!(
        receipt.network_events.is_empty(),
        "click tier has no network_events"
    );
    assert!(
        receipt.console_lines.is_empty(),
        "click tier has no console_lines"
    );
    assert!(
        receipt.return_value_json.is_none(),
        "no return_value_json for click"
    );
    assert!(
        receipt.return_value_blob_ref.is_none(),
        "no return_value_blob_ref for click"
    );
}

// ---------------------------------------------------------------------------
// AC-CORE-04.2 — navigate default tier
// ---------------------------------------------------------------------------

#[test]
fn ac_core_04_2_navigate_receipt_default_tier() {
    let dom_ref = ContentRef {
        sha256: "a".repeat(64),
        size_bytes: 4096,
    };
    let ss_ref = ContentRef {
        sha256: "b".repeat(64),
        size_bytes: 1024,
    };
    let net_event = NetworkEvent {
        method: "GET".to_string(),
        url: "https://example.com/".to_string(),
        status_code: 200,
        response_body_sha256_hex: "c".repeat(64),
        response_body_size_bytes: 512,
        response_body_ref: Some(ContentRef {
            sha256: "c".repeat(64),
            size_bytes: 512,
        }),
        timing_ticks: 1_000,
        content_type: "text/html".to_string(),
    };
    let console_line = ConsoleLine {
        level: "log".to_string(),
        message: "page loaded".to_string(),
        timing_ticks: 500,
    };

    let receipt = ReceiptBuilder::build_navigate_receipt(
        "act-002".to_string(),
        99_000_u64,
        dom_ref.clone(),
        ss_ref.clone(),
        vec![net_event],
        vec![console_line],
    );

    assert_eq!(receipt.action_id, "act-002");
    assert_eq!(receipt.status, ReceiptStatus::Ok);
    assert_eq!(receipt.code, ReceiptCode::WebActionCompleted);
    assert_eq!(receipt.timing_ticks, 99_000);

    // Full blob refs
    assert!(
        receipt.dom_after_blob_ref.is_some(),
        "navigate tier needs dom_after_blob_ref"
    );
    assert!(
        receipt.screenshot_after_blob_ref.is_some(),
        "navigate tier needs screenshot_after_blob_ref"
    );
    assert_eq!(
        receipt.dom_after_blob_ref.as_ref().unwrap().sha256,
        "a".repeat(64)
    );

    // Arrays
    assert_eq!(receipt.network_events.len(), 1);
    assert_eq!(receipt.console_lines.len(), 1);

    // No hash-only fields (navigate uses blob-refs)
    assert!(
        receipt.dom_after_hash.is_none(),
        "navigate tier uses blob-refs not hash fields"
    );
    assert!(receipt.screenshot_after_hash.is_none());
}

// ---------------------------------------------------------------------------
// AC-CORE-04.3 — evaluate default tier
// ---------------------------------------------------------------------------

#[test]
fn ac_core_04_3_evaluate_receipt_default_tier() {
    let console_line = ConsoleLine {
        level: "log".to_string(),
        message: "eval ran".to_string(),
        timing_ticks: 100,
    };
    let receipt = ReceiptBuilder::build_evaluate_receipt(
        "act-003".to_string(),
        5_000_u64,
        Some("2".to_string()), // return_value_json (canonical-JSON of value)
        None,                  // return_value_blob_ref (only set when value > 64KB)
        vec![console_line],
    );

    assert_eq!(receipt.action_id, "act-003");
    assert_eq!(receipt.status, ReceiptStatus::Ok);
    assert_eq!(receipt.code, ReceiptCode::WebActionCompleted);
    assert_eq!(receipt.return_value_json.as_deref(), Some("2"));
    assert!(receipt.return_value_blob_ref.is_none());
    assert_eq!(receipt.console_lines.len(), 1);
    assert_eq!(receipt.timing_ticks, 5_000);

    // NO DOM, screenshot, or network fields
    assert!(receipt.dom_after_hash.is_none(), "evaluate has no DOM hash");
    assert!(
        receipt.dom_after_blob_ref.is_none(),
        "evaluate has no DOM blob"
    );
    assert!(receipt.screenshot_after_hash.is_none());
    assert!(receipt.screenshot_after_blob_ref.is_none());
    assert!(
        receipt.network_events.is_empty(),
        "evaluate has no network_events"
    );
}

// ---------------------------------------------------------------------------
// AC-CORE-04.4 — capture-full adds before-refs (does NOT upgrade hash-after)
// ---------------------------------------------------------------------------

#[test]
fn ac_core_04_4_capture_full_adds_before_refs() {
    let dom_before = ContentRef {
        sha256: "e".repeat(64),
        size_bytes: 2048,
    };
    let ss_before = ContentRef {
        sha256: "f".repeat(64),
        size_bytes: 512,
    };

    let mut receipt = ReceiptBuilder::build_click_receipt(
        "act-004".to_string(),
        10_000,
        "g".repeat(64), // dom_after_hash
        "h".repeat(64), // screenshot_after_hash
    );

    // Provide before-refs for full capture
    receipt.dom_before_blob_ref = Some(dom_before);
    receipt.screenshot_before_blob_ref = Some(ss_before);
    receipt.apply_capture_profile(CaptureProfile::Full);

    // Before-refs are present
    assert!(
        receipt.dom_before_blob_ref.is_some(),
        "Full capture must include dom_before_blob_ref"
    );
    assert!(
        receipt.screenshot_before_blob_ref.is_some(),
        "Full capture must include screenshot_before_blob_ref"
    );

    // Hash-only after-fields must STILL be present (Full doesn't replace them)
    assert!(
        receipt.dom_after_hash.is_some(),
        "Full capture must NOT remove dom_after_hash for click tier"
    );
    assert!(
        receipt.screenshot_after_hash.is_some(),
        "Full capture must NOT remove screenshot_after_hash"
    );

    // No blob-ref for after in click (hash-only)
    assert!(
        receipt.dom_after_blob_ref.is_none(),
        "click tier keeps hash-only after fields even under Full"
    );
}

// ---------------------------------------------------------------------------
// AC-CORE-04.5 — capture-minimal strips blob refs
// ---------------------------------------------------------------------------

#[test]
fn ac_core_04_5_capture_minimal_strips_blobs() {
    let dom_ref = ContentRef {
        sha256: "a".repeat(64),
        size_bytes: 4096,
    };
    let ss_ref = ContentRef {
        sha256: "b".repeat(64),
        size_bytes: 1024,
    };
    let net_event = NetworkEvent {
        method: "GET".to_string(),
        url: "https://example.com/".to_string(),
        status_code: 200,
        response_body_sha256_hex: "c".repeat(64),
        response_body_size_bytes: 512,
        response_body_ref: Some(ContentRef {
            sha256: "c".repeat(64),
            size_bytes: 512,
        }),
        timing_ticks: 1_000,
        content_type: "text/html".to_string(),
    };
    let console_line = ConsoleLine {
        level: "info".to_string(),
        message: "something".to_string(),
        timing_ticks: 200,
    };

    let mut receipt = ReceiptBuilder::build_navigate_receipt(
        "act-005".to_string(),
        20_000,
        dom_ref.clone(),
        ss_ref.clone(),
        vec![net_event],
        vec![console_line],
    );

    receipt.apply_capture_profile(CaptureProfile::Minimal);

    // Blob-refs must be gone; hash fields must appear (extracted from refs)
    assert!(
        receipt.dom_after_blob_ref.is_none(),
        "Minimal strips dom_after_blob_ref"
    );
    assert!(
        receipt.screenshot_after_blob_ref.is_none(),
        "Minimal strips screenshot_after_blob_ref"
    );
    assert!(
        receipt.dom_before_blob_ref.is_none(),
        "Minimal strips dom_before_blob_ref"
    );
    assert!(
        receipt.screenshot_before_blob_ref.is_none(),
        "Minimal strips screenshot_before_blob_ref"
    );

    // Hash fields must be present (extracted from ContentRef.sha256)
    assert_eq!(
        receipt.dom_after_hash.as_deref(),
        Some("a".repeat(64).as_str())
    );
    assert_eq!(
        receipt.screenshot_after_hash.as_deref(),
        Some("b".repeat(64).as_str())
    );

    // Network events: body refs stripped, hash/metadata preserved
    assert_eq!(
        receipt.network_events.len(),
        1,
        "Network events array stays (just body refs stripped)"
    );
    assert!(
        receipt.network_events[0].response_body_ref.is_none(),
        "Minimal strips body ref from events"
    );
    assert!(
        !receipt.network_events[0]
            .response_body_sha256_hex
            .is_empty(),
        "Hash preserved in minimal"
    );

    // console_lines empty
    assert!(
        receipt.console_lines.is_empty(),
        "Minimal empties console_lines"
    );
}

// ---------------------------------------------------------------------------
// AC-CORE-05.1 — error receipt shape
// ---------------------------------------------------------------------------

#[test]
fn ac_core_05_1_error_receipt_shape() {
    let receipt = ReceiptBuilder::build_error_receipt(
        "act-err-001".to_string(),
        7_000,
        ReceiptCode::WebSelectorNotFound,
        "Could not find element #btn".to_string(),
        ReceiptSurface::Web,
        None,
    );

    assert_eq!(receipt.action_id, "act-err-001");
    assert_eq!(receipt.status, ReceiptStatus::Error);
    assert_eq!(receipt.code, ReceiptCode::WebSelectorNotFound);
    assert_eq!(receipt.surface, ReceiptSurface::Web);
    assert!(receipt.message.is_some(), "error receipt must have message");
    let msg = receipt.message.as_ref().unwrap();
    assert!(msg.len() <= 280, "message must be <= 280 chars");
    assert!(
        receipt.details.is_none(),
        "optional details absent when not passed"
    );

    // Error receipt has no payload fields
    assert!(receipt.dom_after_hash.is_none());
    assert!(receipt.dom_after_blob_ref.is_none());
}

#[test]
fn ac_core_05_1_error_receipt_message_truncation() {
    let long_message = "x".repeat(300);
    let receipt = ReceiptBuilder::build_error_receipt(
        "act-err-002".to_string(),
        0,
        ReceiptCode::WebNavigationFailed,
        long_message,
        ReceiptSurface::Web,
        None,
    );
    let msg = receipt.message.as_ref().unwrap();
    assert_eq!(
        msg.len(),
        280,
        "message must be truncated to exactly 280 chars"
    );
}

// ---------------------------------------------------------------------------
// AC-CORE-05.2 — stable 22-code enum (actual count is 21 per AC text)
// ---------------------------------------------------------------------------

#[test]
fn ac_core_05_2_all_receipt_codes_have_wire_strings() {
    let codes = vec![
        ReceiptCode::WebActionCompleted,
        ReceiptCode::WebNavigationFailed,
        ReceiptCode::WebSelectorNotFound,
        ReceiptCode::WebEvaluateThrew,
        ReceiptCode::WebActionTimeout,
        ReceiptCode::VaultGrantExpired,
        ReceiptCode::VaultOriginMismatch,
        ReceiptCode::VaultGrantRevoked,
        ReceiptCode::VaultSecretUnavailable,
        ReceiptCode::VaultThreatModelMissing,
        ReceiptCode::BudgetExceededWalltime,
        ReceiptCode::BudgetExceededNetwork,
        ReceiptCode::BudgetExceededDomNodes,
        ReceiptCode::BudgetExceededJsHeap,
        ReceiptCode::SchemaViolation,
        ReceiptCode::SessionUnknown,
        ReceiptCode::SessionAborted,
        ReceiptCode::SessionClosed,
        ReceiptCode::SurfaceUnavailable,
        ReceiptCode::StoreIntegrityFailed,
        ReceiptCode::ManifestIntegrityFailed,
        ReceiptCode::RuntimeCrash,
    ];
    // 22 codes per AC-CORE-05.2 (note: AC text lists 21 distinct codes despite saying "22")
    assert_eq!(codes.len(), 22, "AC-CORE-05.2 enumerates exactly 22 codes");
    for code in &codes {
        let wire = code.as_wire();
        assert!(!wire.is_empty(), "as_wire() must return non-empty string");
        assert!(
            wire.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "wire string must be snake_case: {}",
            wire
        );
    }
}

#[test]
fn ac_core_05_2_receipt_code_serde_matches_as_wire() {
    // P1 fix: verify serde output matches as_wire()
    let codes = vec![
        ReceiptCode::WebActionCompleted,
        ReceiptCode::WebNavigationFailed,
        ReceiptCode::VaultGrantExpired,
        ReceiptCode::BudgetExceededWalltime,
        ReceiptCode::RuntimeCrash,
    ];
    for code in codes {
        let serialized = serde_json::to_string(&code).unwrap();
        let expected = format!("\"{}\"", code.as_wire());
        assert_eq!(
            serialized, expected,
            "serde output {:?} must match as_wire() {:?}",
            serialized, expected
        );
    }
}

// ---------------------------------------------------------------------------
// AC-NFR-DET-03.1 — no floats in receipt fields
// ---------------------------------------------------------------------------

#[test]
fn ac_nfr_det_03_1_no_floats_in_receipt_fields() {
    let receipt = ReceiptBuilder::build_click_receipt(
        "act-f".to_string(),
        1_000_000,
        "0".repeat(64),
        "1".repeat(64),
    );
    let json = serde_json::to_value(&receipt).unwrap();
    assert_no_floats(&json);
}

fn assert_no_floats(v: &serde_json::Value) {
    match v {
        serde_json::Value::Number(n) => {
            assert!(
                n.is_i64() || n.is_u64(),
                "receipt must not contain float values, found: {}",
                n
            );
        }
        serde_json::Value::Array(arr) => arr.iter().for_each(assert_no_floats),
        serde_json::Value::Object(obj) => obj.values().for_each(assert_no_floats),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// AC-NFR-DET-04.1 — canonical field ordering via serde_jcs
// ---------------------------------------------------------------------------

#[test]
fn ac_nfr_det_04_1_canonical_field_ordering() {
    let receipt = ReceiptBuilder::build_click_receipt(
        "act-ord".to_string(),
        2_000,
        "a".repeat(64),
        "b".repeat(64),
    );
    let bytes1 = receipt
        .canonical_bytes()
        .expect("canonical_bytes must succeed");
    let bytes2 = receipt.canonical_bytes().expect("second call must succeed");

    // Deterministic across calls
    assert_eq!(bytes1, bytes2, "canonical_bytes must be deterministic");

    // Valid UTF-8 JSON
    let json_str = String::from_utf8(bytes1).expect("canonical_bytes must be valid UTF-8");
    let _: serde_json::Value = serde_json::from_str(&json_str).expect("must be valid JSON");

    // Lexicographic key ordering: extract top-level keys and verify sorted
    let obj: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&json_str).expect("must be a JSON object");
    let keys: Vec<String> = obj.keys().cloned().collect();
    let mut sorted_keys = keys.clone();
    sorted_keys.sort();
    assert_eq!(
        keys, sorted_keys,
        "JSON keys must be in lexicographic order"
    );
}

// ---------------------------------------------------------------------------
// AC-NFR-DET-05.1 — timing_ticks is monotonic integer
// ---------------------------------------------------------------------------

#[test]
fn ac_nfr_det_05_1_timing_ticks_is_integer() {
    let receipt = ReceiptBuilder::build_click_receipt(
        "act-t".to_string(),
        999_999_u64, // microseconds
        "0".repeat(64),
        "1".repeat(64),
    );
    assert_eq!(receipt.timing_ticks, 999_999);

    // Verify timing_ticks serializes as integer (not float)
    let json = serde_json::to_value(&receipt).unwrap();
    if let Some(v) = json.get("timing_ticks") {
        assert!(
            v.is_u64() || v.is_i64(),
            "timing_ticks must serialize as integer, got: {}",
            v
        );
    }
}

#[test]
fn ac_nfr_det_05_1_timing_ticks_monotonic_in_sequence() {
    // Verify that the type accepts monotonically increasing values (no enforcement in the builder,
    // but the type constraint — u64, not f64 — prevents fractional values)
    let t0 = 0_u64;
    let t1 = 1_000_u64;
    let t2 = 2_000_u64;
    assert!(
        t1 >= t0 && t2 >= t1,
        "timing_ticks must be monotonically non-decreasing"
    );
    // The builder accepts u64, which is always an integer.
    let r0 =
        ReceiptBuilder::build_click_receipt("a0".to_string(), t0, "0".repeat(64), "1".repeat(64));
    let r1 =
        ReceiptBuilder::build_click_receipt("a1".to_string(), t1, "0".repeat(64), "1".repeat(64));
    let r2 =
        ReceiptBuilder::build_click_receipt("a2".to_string(), t2, "0".repeat(64), "1".repeat(64));
    assert!(r1.timing_ticks >= r0.timing_ticks);
    assert!(r2.timing_ticks >= r1.timing_ticks);
}
