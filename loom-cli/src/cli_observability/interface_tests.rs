// Interface tests for `CliObservability`. Verifies the per-command
// span shape and the redaction-field allowlist.

use super::cli_observability::{init, start_span, SpanGuard, REDACTED_FIELDS};

#[test]
fn init_signature() {
    fn _ck() {
        init()
    }
    let _ = _ck;
}

#[test]
fn start_span_signature() {
    fn _ck(c: &str, s: Option<&str>) -> SpanGuard {
        start_span(c, s)
    }
    let _ = _ck;
}

#[test]
fn redacted_fields_includes_oauth_secrets() {
    for expected in [
        "oauth_token",
        "access_token",
        "refresh_token",
        "client_secret",
        "secret",
    ] {
        assert!(
            REDACTED_FIELDS.contains(&expected),
            "REDACTED_FIELDS missing {expected}"
        );
    }
}

#[test]
fn redacted_fields_includes_password_and_api_key() {
    for expected in ["password", "api_key"] {
        assert!(
            REDACTED_FIELDS.contains(&expected),
            "REDACTED_FIELDS missing {expected}"
        );
    }
}

// Span-guard accessors are tested via the `record_*` signatures —
// they must compile against `&mut self`.
#[test]
fn span_guard_record_signatures() {
    fn _ck(g: &mut SpanGuard) {
        g.record_rpc_method("session.create");
        g.record_receipt_status("ok");
        g.record_exit_code(0);
    }
    let _ = _ck;
}
