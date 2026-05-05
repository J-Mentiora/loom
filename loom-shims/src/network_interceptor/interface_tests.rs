// Interface tests for `NetworkInterceptor`.
// Verifies decompress before SHA-256 (KILL-CRITERION),
// typed `LoomNetworkEvent` shape (no CDP payload escape),
// integer-only numeric fields.

use super::network_interceptor::{
    compute_response_hash, strip_content_encoding, LoomNetworkEvent, SHA256_HEX_LEN,
};

// === hash is over decompressed bytes (KILL) ===

#[test]
fn compute_response_hash_returns_64_char_hex() {
    let decompressed = b"hello world";
    let hash = compute_response_hash(decompressed);
    assert_eq!(hash.len(), SHA256_HEX_LEN);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn compute_response_hash_known_value_matches_sha256_of_input() {
    // SHA-256("") is e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    let h = compute_response_hash(b"");
    assert_eq!(
        h,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn different_decompressed_bytes_produce_different_hashes() {
    // Crucial replay-parity test: if the implementation ever hashed compressed
    // bytes by mistake, two different decompressions could collide.
    let h1 = compute_response_hash(b"<html>a</html>");
    let h2 = compute_response_hash(b"<html>b</html>");
    assert_ne!(h1, h2);
}

// === strip Content-Encoding from request before hash (KILL) ===

#[test]
fn strip_content_encoding_removes_the_header_case_insensitive() {
    let headers = vec![
        ("Accept".into(), "*/*".into()),
        ("Content-Encoding".into(), "gzip".into()),
        ("User-Agent".into(), "loom".into()),
    ];
    let stripped = strip_content_encoding(headers);
    assert_eq!(stripped.len(), 2);
    assert!(stripped
        .iter()
        .all(|(k, _)| !k.eq_ignore_ascii_case("content-encoding")));
}

#[test]
fn strip_content_encoding_handles_lowercase_header_name() {
    let headers = vec![
        ("content-encoding".into(), "br".into()),
        ("X-Other".into(), "y".into()),
    ];
    let stripped = strip_content_encoding(headers);
    assert_eq!(stripped.len(), 1);
    assert_eq!(stripped[0].0, "X-Other");
}

#[test]
fn strip_content_encoding_preserves_unrelated_headers() {
    let headers = vec![
        ("Authorization".into(), "Bearer abc".into()),
        ("Content-Type".into(), "application/json".into()),
    ];
    let stripped = strip_content_encoding(headers.clone());
    assert_eq!(stripped, headers);
}

// === LoomNetworkEvent is typed (no CDP escape) ===

#[test]
fn loom_network_event_has_typed_fields() {
    let ev = LoomNetworkEvent {
        method: "GET".into(),
        url: "https://example.com/page".into(),
        request_hash: "a".repeat(64),
        response_hash: "b".repeat(64),
        status: 200,
        content_type: "text/html".into(),
        duration_ms: 17,
        response_bytes: 4096,
        error_reason: None,
        error_kind: None,
    };
    assert!(ev.is_complete());
    assert_eq!(ev.status, 200u16);
    assert_eq!(ev.duration_ms, 17u64);
    assert_eq!(ev.response_bytes, 4096u64);
}

#[test]
fn loom_network_event_partial_when_error_reason_set() {
    let ev = LoomNetworkEvent {
        method: "GET".into(),
        url: "https://example.com/page".into(),
        request_hash: "a".repeat(64),
        response_hash: String::new(),
        status: 0,
        content_type: String::new(),
        duration_ms: 0,
        response_bytes: 0,
        error_reason: Some("body evicted".into()),
        error_kind: None,
    };
    assert!(!ev.is_complete());
}

// === Integer-only numeric fields ===

#[test]
fn loom_network_event_numeric_fields_are_integers_not_floats() {
    let ev = LoomNetworkEvent {
        method: "GET".into(),
        url: "x".into(),
        request_hash: "a".repeat(64),
        response_hash: "b".repeat(64),
        status: u16::MAX,
        content_type: "x".into(),
        duration_ms: u64::MAX,
        response_bytes: u64::MAX,
        error_reason: None,
        error_kind: None,
    };
    let _: u16 = ev.status;
    let _: u64 = ev.duration_ms;
    let _: u64 = ev.response_bytes;
}

// === Hash output is lowercase hex ===

#[test]
fn hash_output_is_lowercase() {
    let h = compute_response_hash(b"abc");
    assert_eq!(h, h.to_lowercase());
}

// === SHA256_HEX_LEN constant matches reality ===

#[test]
fn sha256_hex_length_is_64() {
    assert_eq!(SHA256_HEX_LEN, 64);
}

// === navigate-timeout-on-unreachable: classify_chromium_nav_error ===

#[test]
fn classify_chromium_nav_error_dns_failure() {
    use super::network_interceptor::classify_chromium_nav_error;
    assert_eq!(
        classify_chromium_nav_error("net::ERR_NAME_NOT_RESOLVED"),
        "dns_failure"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_NAME_RESOLUTION_FAILED"),
        "dns_failure"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_DNS_TIMED_OUT"),
        "dns_failure"
    );
}

#[test]
fn classify_chromium_nav_error_connect_refused() {
    use super::network_interceptor::classify_chromium_nav_error;
    assert_eq!(
        classify_chromium_nav_error("net::ERR_CONNECTION_REFUSED"),
        "connect_refused"
    );
}

#[test]
fn classify_chromium_nav_error_tls_error() {
    use super::network_interceptor::classify_chromium_nav_error;
    assert_eq!(
        classify_chromium_nav_error("net::ERR_CERT_DATE_INVALID"),
        "tls_error"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_CERT_AUTHORITY_INVALID"),
        "tls_error"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_SSL_PROTOCOL_ERROR"),
        "tls_error"
    );
}

// blocklist-blocked URLs return "blocked" (distinct
// from "network_error") so agent retry logic can differentiate.
#[test]
fn classify_chromium_nav_error_blocked() {
    use super::network_interceptor::classify_chromium_nav_error;
    assert_eq!(
        classify_chromium_nav_error("net::ERR_BLOCKED_BY_CLIENT"),
        "blocked"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_BLOCKED_BY_RESPONSE"),
        "blocked"
    );
}

/// Real DNS-failure URLs continue to return kind='dns_failure',
/// not 'blocked' (regression guard).
#[test]
fn classify_chromium_nav_error_dns_not_blocked() {
    use super::network_interceptor::classify_chromium_nav_error;
    assert_ne!(
        classify_chromium_nav_error("net::ERR_NAME_NOT_RESOLVED"),
        "blocked",
        "DNS failures must NOT be misclassified as 'blocked'"
    );
}

#[test]
fn classify_chromium_nav_error_catchall_network_error() {
    use super::network_interceptor::classify_chromium_nav_error;
    // Unknown / not-yet-classified codes fall back to "network_error"
    // so the receipt still carries a typed kind. The raw text in
    // `reason` lets operators disambiguate.
    assert_eq!(
        classify_chromium_nav_error("net::ERR_TIMED_OUT"),
        "network_error"
    );
    assert_eq!(
        classify_chromium_nav_error("net::ERR_CONNECTION_RESET"),
        "network_error"
    );
    assert_eq!(classify_chromium_nav_error(""), "network_error");
}

// === parse_network_event extracts status / errorText ===

#[test]
fn parse_response_received_extracts_document_status_code() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    let params = Value::Map(vec![
        (Value::Text("requestId".into()), Value::Text("R-1".into())),
        (Value::Text("type".into()), Value::Text("Document".into())),
        (
            Value::Text("response".into()),
            Value::Map(vec![
                (
                    Value::Text("url".into()),
                    Value::Text("http://fake.test/x".into()),
                ),
                (Value::Text("status".into()), Value::Integer(404i64.into())),
                (
                    Value::Text("mimeType".into()),
                    Value::Text("text/html".into()),
                ),
            ]),
        ),
    ]);
    let event = parse_network_event("Network.responseReceived", &params)
        .expect("Document responseReceived should yield an event");
    assert_eq!(event.status, 404u16);
    assert_eq!(event.url, "http://fake.test/x");
    assert_eq!(event.content_type, "text/html");
    assert_eq!(event.error_reason, None);
    assert_eq!(event.error_kind, None);
}

#[test]
fn parse_response_received_filters_subresource_events() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    // Real Chromium emits responseReceived for every subresource
    // (Stylesheet / Script / Image). Those would shadow the document's
    // status if we appended them — drop everything except Document.
    let params = Value::Map(vec![
        (Value::Text("type".into()), Value::Text("Stylesheet".into())),
        (
            Value::Text("response".into()),
            Value::Map(vec![(
                Value::Text("status".into()),
                Value::Integer(200i64.into()),
            )]),
        ),
    ]);
    assert!(parse_network_event("Network.responseReceived", &params).is_none());
}

#[test]
fn parse_loading_failed_extracts_error_text_and_classifies() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    let params = Value::Map(vec![
        (Value::Text("requestId".into()), Value::Text("R-1".into())),
        (Value::Text("type".into()), Value::Text("Document".into())),
        (
            Value::Text("errorText".into()),
            Value::Text("net::ERR_NAME_NOT_RESOLVED".into()),
        ),
        (Value::Text("canceled".into()), Value::Bool(false)),
    ]);
    let event = parse_network_event("Network.loadingFailed", &params)
        .expect("Document loadingFailed should yield an event");
    assert_eq!(
        event.error_reason.as_deref(),
        Some("net::ERR_NAME_NOT_RESOLVED")
    );
    assert_eq!(event.error_kind.as_deref(), Some("dns_failure"));
    assert_eq!(event.status, 0);
}

#[test]
fn parse_loading_failed_filters_subresource_events() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    let params = Value::Map(vec![
        (Value::Text("type".into()), Value::Text("Image".into())),
        (
            Value::Text("errorText".into()),
            Value::Text("net::ERR_FAILED".into()),
        ),
    ]);
    assert!(parse_network_event("Network.loadingFailed", &params).is_none());
}

#[test]
fn parse_loading_failed_skips_empty_error_text() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    let params = Value::Map(vec![
        (Value::Text("type".into()), Value::Text("Document".into())),
        (Value::Text("errorText".into()), Value::Text("".into())),
    ]);
    assert!(parse_network_event("Network.loadingFailed", &params).is_none());
}

#[test]
fn parse_network_event_ignores_unrelated_methods() {
    use super::network_interceptor::parse_network_event;
    use ciborium::value::Value;

    let params = Value::Map(vec![(
        Value::Text("type".into()),
        Value::Text("Document".into()),
    )]);
    assert!(parse_network_event("Network.requestWillBeSent", &params).is_none());
    assert!(parse_network_event("Page.loadEventFired", &params).is_none());
}

#[test]
fn loom_network_event_carries_error_kind_when_failed() {
    let ev = LoomNetworkEvent {
        method: String::new(),
        url: "https://x.example/".into(),
        request_hash: String::new(),
        response_hash: String::new(),
        status: 0,
        content_type: String::new(),
        duration_ms: 0,
        response_bytes: 0,
        error_reason: Some("net::ERR_NAME_NOT_RESOLVED".into()),
        error_kind: Some("dns_failure".into()),
    };
    assert!(!ev.is_complete());
    assert_eq!(ev.error_kind.as_deref(), Some("dns_failure"));
}
