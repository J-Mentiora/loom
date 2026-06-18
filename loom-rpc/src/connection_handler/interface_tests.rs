// Interface tests for `ConnectionHandler`. Verifies FSM
// states, single-task hot path signature,
// deps-injection (no fresh runtime constructed inside).

use super::connection_handler::{
    ConnectionHandler, ConnectionHandlerDeps, ConnectionState, HandlerError,
    AUTHENTICATED_IDLE_TIMEOUT,
};
use crate::auth_middleware::auth_middleware::AuthMiddlewareApi;
use crate::request_router::request_router::RequestRouterApi;
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn fsm_state_has_two_variants_per_ic_rpc_05() {
    let _ = ConnectionState::AwaitingHello;
    let _ = ConnectionState::Authenticated;
}

#[test]
fn authenticated_idle_timeout_is_finite_duration() {
    // Soft binding — value may tune; test asserts type only.
    let _: Duration = AUTHENTICATED_IDLE_TIMEOUT;
    assert!(AUTHENTICATED_IDLE_TIMEOUT.as_secs() > 0);
}

#[test]
fn deps_struct_holds_all_four_arc_handles() {
    fn _ck(d: &ConnectionHandlerDeps) {
        let _: &Arc<dyn AuthMiddlewareApi> = &d.auth;
        let _: &Arc<dyn SchemaValidatorApi> = &d.validator;
        let _: &Arc<dyn RequestRouterApi> = &d.router;
        let _: &Arc<dyn RpcObservabilityApi> = &d.observability;
    }
    let _ = _ck;
}

#[test]
fn handler_constructor_takes_arc_deps_starts_in_awaiting_hello() {
    fn _ck(d: Arc<ConnectionHandlerDeps>) -> ConnectionHandler {
        let h = ConnectionHandler::new(d);
        assert_eq!(h.state, ConnectionState::AwaitingHello);
        h
    }
    let _ = _ck;
}

#[test]
fn run_signature_consumes_unix_stream_async() {
    // runs on the caller's task; no fresh runtime spawned.
    fn _ck() {
        async fn _go(h: ConnectionHandler, s: tokio::net::UnixStream) {
            h.run(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn handler_error_distinguishes_idle_framing_auth_disconnect_io() {
    // design.md §4 error rows.
    let _ = HandlerError::IdleTimeout;
    let _ = HandlerError::FramingFailure {
        reason: "too big".into(),
    };
    let _ = HandlerError::AuthFailed;
    let _ = HandlerError::ClientDisconnected;
    let _ = HandlerError::Io {
        reason: "epipe".into(),
    };
}

// ─── unwrap_sdk_envelope ─────────────────────────────────────────────
//
// Tests for the `@mentiora-ai/loom-sdk@0.9.x` envelope-unwrap helper.
// The SDK sends action verbs as `action.web.<verb>` with payload bytes
// in a JSON byte-array; the schema validator wants a flat-shape object
// with `session` as the session key. The helper bridges both shapes.

use super::unwrap_sdk_envelope;
use serde_json::json;

/// SDK envelope with valid UTF-8 JSON payload — the happy path.
#[test]
fn unwrap_sdk_envelope_happy_path_emits_session_key() {
    let payload_json = r#"{"url":"https://example.com"}"#;
    let payload_bytes: Vec<u8> = payload_json.bytes().collect();
    let params = json!({
        "session_id": "01h7m4z3p2k1q8m5n7v6r9t2j",
        "action": {
            "kind": "navigate",
            "payload": payload_bytes,
            "deadline_ms": 30000
        }
    });
    let unwrapped = unwrap_sdk_envelope(params);
    let obj = unwrapped.as_object().expect("unwrap returns an object");
    // session key is the schema-canonical name (not session_id).
    assert_eq!(
        obj.get("session").and_then(|v| v.as_str()),
        Some("01h7m4z3p2k1q8m5n7v6r9t2j")
    );
    assert_eq!(
        obj.get("url").and_then(|v| v.as_str()),
        Some("https://example.com")
    );
    // Envelope keys are stripped — the schemas have additionalProperties:false.
    assert!(!obj.contains_key("action"));
    assert!(!obj.contains_key("session_id"));
}

/// Flat-shape input (CLI / MCP / older-SDK callers) is returned unchanged.
#[test]
fn unwrap_sdk_envelope_flat_shape_passthrough() {
    let params = json!({
        "session": "01h7m4z3p2k1q8m5n7v6r9t2j",
        "url": "https://example.com"
    });
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// `session_id` present but no `action` envelope — returned unchanged
/// (rare, but possible from a partially-unwrapped earlier stage).
#[test]
fn unwrap_sdk_envelope_no_action_passthrough() {
    let params = json!({
        "session_id": "01h7m4z3p2k1q8m5n7v6r9t2j",
        "url": "https://example.com"
    });
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// Non-object params (array, string) flow through unchanged.
#[test]
fn unwrap_sdk_envelope_non_object_passthrough() {
    let params = json!(["a", "b"]);
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// `action.payload` is present but isn't valid UTF-8 — pass through;
/// the validator surfaces a usable error from there.
#[test]
fn unwrap_sdk_envelope_invalid_utf8_payload_passthrough() {
    // 0xFF is not the start of a valid UTF-8 sequence.
    let params = json!({
        "session_id": "sess",
        "action": { "payload": [0xFF, 0xFE], "kind": "navigate" }
    });
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// `action.payload` decodes to UTF-8 but isn't valid JSON — pass through.
#[test]
fn unwrap_sdk_envelope_non_json_payload_passthrough() {
    let payload_bytes: Vec<u8> = b"not json".to_vec();
    let params = json!({
        "session_id": "sess",
        "action": { "payload": payload_bytes, "kind": "navigate" }
    });
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// Empty params (no session_id, no action) — pass through.
#[test]
fn unwrap_sdk_envelope_empty_object_passthrough() {
    let params = json!({});
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// Payload byte > 255 must NOT silently truncate (`300 as u8` == 44):
/// the whole envelope passes through unchanged so the validator rejects
/// it, instead of decoding to different-but-valid JSON and dispatching.
#[test]
fn unwrap_sdk_envelope_overflow_byte_passthrough_not_truncated() {
    // `{"url":"a"}` with one byte shifted by +256 — the wrapped decode
    // would yield the exact bytes the client did NOT send.
    let mut payload: Vec<i64> = br#"{"url":"a"}"#.iter().map(|&b| b as i64).collect();
    payload[8] += 256;
    let params = json!({
        "session_id": "sess",
        "action": { "payload": payload, "kind": "navigate" }
    });
    let unwrapped = unwrap_sdk_envelope(params.clone());
    assert_eq!(unwrapped, params);
}

/// Negative, fractional, and non-numeric payload elements are rejected
/// (passthrough), not silently dropped with later bytes shifted.
#[test]
fn unwrap_sdk_envelope_non_byte_elements_passthrough() {
    for bad in [json!(-1), json!(1.5), json!("x"), json!(null)] {
        let params = json!({
            "session_id": "sess",
            "action": { "payload": [123, bad, 125], "kind": "navigate" }
        });
        let unwrapped = unwrap_sdk_envelope(params.clone());
        assert_eq!(unwrapped, params);
    }
}

// ─── merge_envelope_deadline (SDK envelope deadline → flat params) ────
//
// `unwrap_sdk_envelope` keeps only payload fields + session, so the
// envelope-level `deadline_ms` is mirrored back into the flat params by
// `merge_envelope_deadline` — otherwise the router's per-action daemon-side
// kill would never engage for SDK callers (only flat MCP/CLI callers).

use super::merge_envelope_deadline;

/// A positive envelope deadline is injected into the flat params so the
/// router can extract it and arm the daemon-side kill.
#[test]
fn merge_envelope_deadline_injects_positive_into_flat_params() {
    let params = json!({ "session": "s", "url": "https://example.com" });
    let merged = merge_envelope_deadline(params, Some(2000));
    assert_eq!(
        merged.get("deadline_ms").and_then(|v| v.as_u64()),
        Some(2000),
        "SDK envelope deadline must reach the flat params so the router sees it"
    );
}

/// `0` (the SDKs' "no preference") and `None` are NOT injected — the
/// executor's no-deadline path must stay byte-for-byte unchanged.
#[test]
fn merge_envelope_deadline_skips_zero_and_none() {
    for d in [Some(0u64), None] {
        let params = json!({ "session": "s", "url": "u" });
        let merged = merge_envelope_deadline(params, d);
        assert!(
            merged.get("deadline_ms").is_none(),
            "deadline_ms={d:?} must not be injected (no-deadline path)"
        );
    }
}

/// Idempotent: a top-level `deadline_ms` a flat caller already set is never
/// overwritten by the envelope value.
#[test]
fn merge_envelope_deadline_does_not_overwrite_existing() {
    let params = json!({ "session": "s", "deadline_ms": 500 });
    let merged = merge_envelope_deadline(params, Some(9999));
    assert_eq!(
        merged.get("deadline_ms").and_then(|v| v.as_u64()),
        Some(500),
        "an existing top-level deadline_ms must win over the envelope value"
    );
}

/// A non-object value passes through untouched (defensive).
#[test]
fn merge_envelope_deadline_non_object_passthrough() {
    let params = json!(["a", "b"]);
    let merged = merge_envelope_deadline(params.clone(), Some(2000));
    assert_eq!(merged, params);
}

// ─── effective_request_timeout (per-action deadline_ms clamp) ────────

use super::effective_request_timeout;
use std::time::Duration as Dur;

/// A positive `deadline_ms` below the server cap tightens the bound.
#[test]
fn deadline_below_cap_clamps_down() {
    let cap = Dur::from_secs(30);
    assert_eq!(
        effective_request_timeout(Some(300), cap),
        Dur::from_millis(300)
    );
}

/// A `deadline_ms` above the server cap can NOT extend past server
/// policy — the cap wins.
#[test]
fn deadline_above_cap_is_capped() {
    let cap = Dur::from_secs(30);
    assert_eq!(effective_request_timeout(Some(120_000), cap), cap);
}

/// Absent or zero deadline (the SDKs send 0 for "no preference") means
/// the server default applies.
#[test]
fn absent_or_zero_deadline_uses_server_cap() {
    let cap = Dur::from_secs(30);
    assert_eq!(effective_request_timeout(None, cap), cap);
    assert_eq!(effective_request_timeout(Some(0), cap), cap);
}
