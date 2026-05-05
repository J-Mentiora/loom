//! Integration tests — typed navigate-error receipts.
//!
//! These tests exercise the public `ReceiptMarshaller::assemble_canonical_bytes`
//! pathway with a `ReceiptBuilder` shaped exactly as the host produces after
//! `decode_typed_receipt` processes a `shim-failure` variant carrying
//! structured JSON detail (kind="http_status" / kind="dns_failure"). They are
//! the integration-level fixture for the end-to-end contract: from a
//! navigate failure → typed receipt JSON in the manifest.

use loom_host::receipt_marshaller::{ReceiptBuilder, ReceiptMarshaller, ReceiptStatus};

/// Helper: build a canonical-JSON value from a builder.
fn assemble_value(builder: &ReceiptBuilder) -> serde_json::Value {
    let bytes = ReceiptMarshaller::assemble_canonical_bytes(builder)
        .expect("assemble_canonical_bytes must not fail");
    serde_json::from_slice(&bytes).expect("canonical bytes must be valid JSON")
}

// ─── HTTP 4xx surfaces typed-error receipt ─────────────────────────────────

#[test]
fn http_404_navigate_emits_typed_error_receipt() {
    let builder = ReceiptBuilder {
        action_id: 1,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(r#"{"kind":"http_status","status_code":404}"#.to_string()),
        navigate_status_code: Some(404),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "http_status");
    assert_eq!(val["details"]["status_code"], 404u64);
    let msg = val["message"].as_str().expect("message must be string");
    assert!(msg.contains("404"));
    assert!(msg.len() <= 280, "message ≤ 280 chars");
}

// ─── HTTP 5xx surfaces typed-error receipt ─────────────────────────────────

#[test]
fn http_500_navigate_emits_typed_error_receipt() {
    let builder = ReceiptBuilder {
        action_id: 2,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(r#"{"kind":"http_status","status_code":503}"#.to_string()),
        navigate_status_code: Some(503),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "http_status");
    assert_eq!(val["details"]["status_code"], 503u64);
}

// ─── DNS / network failure surfaces typed-error receipt ────────────────────

#[test]
fn dns_failure_navigate_emits_typed_error_receipt() {
    let builder = ReceiptBuilder {
        action_id: 3,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(
            r#"{"kind":"dns_failure","chromium_error":"net::ERR_NAME_NOT_RESOLVED"}"#.to_string(),
        ),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "dns_failure");
    assert_eq!(
        val["details"]["chromium_error"],
        "net::ERR_NAME_NOT_RESOLVED"
    );
    let msg = val["message"].as_str().expect("message must be string");
    assert!(msg.to_lowercase().contains("dns") || msg.to_lowercase().contains("network"));
}

// ─── 2xx success keeps web_action_completed (no regression) ────────────────

#[test]
fn http_200_navigate_keeps_web_action_completed() {
    let builder = ReceiptBuilder {
        action_id: 4,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 5_000,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("OK".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(1),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "ok");
    assert_eq!(val["code"], "web_action_completed");
}

// ─── Connection refused surfaces typed-error receipt ───────────────────────

#[test]
fn connect_refused_navigate_emits_typed_error_receipt() {
    let builder = ReceiptBuilder {
        action_id: 30,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(
            r#"{"kind":"connect_refused","chromium_error":"net::ERR_CONNECTION_REFUSED","url":"https://localhost:1/"}"#
                .to_string(),
        ),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "connect_refused");
    assert_eq!(
        val["details"]["chromium_error"],
        "net::ERR_CONNECTION_REFUSED"
    );
    let msg = val["message"].as_str().expect("message must be string");
    assert!(
        msg.to_lowercase().contains("connection") || msg.to_lowercase().contains("refused"),
        "friendly message must name the failure mode (got {msg:?})"
    );
    assert!(msg.len() <= 280, "message ≤ 280 chars");
}

// ─── TLS handshake error surfaces typed-error receipt ──────────────────────

#[test]
fn tls_error_navigate_emits_typed_error_receipt() {
    let builder = ReceiptBuilder {
        action_id: 31,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(
            r#"{"kind":"tls_error","chromium_error":"net::ERR_CERT_DATE_INVALID","url":"https://expired.badssl.com/"}"#
                .to_string(),
        ),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "tls_error");
    assert_eq!(
        val["details"]["chromium_error"],
        "net::ERR_CERT_DATE_INVALID"
    );
    let msg = val["message"].as_str().expect("message must be string");
    assert!(
        msg.to_lowercase().contains("tls") || msg.to_lowercase().contains("certificate"),
        "friendly message must name the TLS failure mode (got {msg:?})"
    );
}

// ─── Network error.detail names URL + chromium error code ──────────────────

#[test]
fn network_error_receipt_carries_url_in_details() {
    let url = "https://this-domain-definitely-does-not-exist-xyzqq.example/";
    let builder = ReceiptBuilder {
        action_id: 32,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(format!(
            r#"{{"kind":"dns_failure","chromium_error":"net::ERR_NAME_NOT_RESOLVED","url":"{url}"}}"#
        )),
        // (chromium_error replaces the old `reason` field.)
        ..Default::default()
    };

    let val = assemble_value(&builder);

    assert_eq!(val["details"]["kind"], "dns_failure");
    assert_eq!(
        val["details"]["url"], url,
        "details.url MUST contain the requested URL"
    );
    assert_eq!(
        val["details"]["chromium_error"], "net::ERR_NAME_NOT_RESOLVED",
        "details.chromium_error MUST carry the chromium error code"
    );
}

// ─── Typed-error receipt has stable schema shape ───────────────────────────

#[test]
fn navigate_error_receipt_has_stable_schema_shape() {
    let builder = ReceiptBuilder {
        action_id: 5,
        finished_at_ms: 9_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 9_500,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(r#"{"kind":"http_status","status_code":418}"#.to_string()),
        navigate_status_code: Some(418),
        ..Default::default()
    };

    let val = assemble_value(&builder);

    // Stable fields all present in the typed-error path.
    for key in &[
        "action_id",
        "status",
        "code",
        "details",
        "message",
        "surface",
    ] {
        assert!(
            val.get(key).is_some(),
            "stable key '{key}' must be present in typed-error receipt"
        );
    }
    assert_eq!(val["surface"], "web");
}
