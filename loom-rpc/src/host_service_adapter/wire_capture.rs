// wire_capture — apply capture-policy to the wire `Receipt` per the
// shared `keep_field` decision table in
// `loom_core::receipt_builder::capture_policy`.
//
// Why this file exists: the manifest-side enforcer
// (`ReceiptPayload::apply_capture_profile`) and this wire-side
// enforcer must not diverge. Both consult `keep_field(scope, profile,
// field)`; the exhaustiveness test in `capture_policy` ensures every
// (scope, profile, field) triple has an explicit answer. Identity
// fields (`action_id`, `session_id`, `status`, `timing_ticks`) are
// unconditional and not enumerated.
//
// Brief `--capture-policy minimal` strips
// dom_snapshot_hash, screenshot_after_hash, console_lines, etc.,
// leaving only `url`, `status_code`, `error` (+ identity).

use loom_core::receipt_builder::capture_policy::{keep_field, CaptureField, CaptureScope};
use loom_core::receipt_builder::receipt_builder::CaptureProfile;

use super::host_service_adapter::Receipt;

/// Apply the wire-scope capture-policy decision table to a `Receipt`
/// in place. Sets fields to `None` / `Vec::new()` when `keep_field`
/// returns `false`. Runs at dispatch time, after the daemon has
/// constructed the wire receipt and immediately before returning it
/// to the JSON-RPC layer.
pub fn apply_capture_profile_to_wire(r: &mut Receipt, profile: CaptureProfile) {
    let scope = CaptureScope::Wire;

    // `Default` and `Full` keep everything wire-side today; short-circuit.
    if profile != CaptureProfile::Minimal {
        return;
    }

    if !keep_field(scope, profile, CaptureField::FinalUrl) {
        r.final_url = None;
    }
    if !keep_field(scope, profile, CaptureField::Title) {
        r.title = None;
    }
    if !keep_field(scope, profile, CaptureField::DomSnapshotHash) {
        r.dom_snapshot_hash = None;
    }
    if !keep_field(scope, profile, CaptureField::ScreenshotAfterHash) {
        r.screenshot_after_hash = None;
    }
    if !keep_field(scope, profile, CaptureField::ConsoleCount) {
        r.console_count = None;
    }
    if !keep_field(scope, profile, CaptureField::ConsoleLines) {
        r.console_lines.clear();
    }
    if !keep_field(scope, profile, CaptureField::NetworkCount) {
        r.network_count = None;
    }
    if !keep_field(scope, profile, CaptureField::NetworkSummary) {
        r.network_summary = None;
    }
    if !keep_field(scope, profile, CaptureField::SideEffects) {
        r.side_effects.clear();
    }
    if !keep_field(scope, profile, CaptureField::NetworkEntries) {
        r.network_entries.clear();
        r.network_entries_blob_ref = None;
        r.network_entries_truncated = None;
    }
    if !keep_field(scope, profile, CaptureField::ActionHash) {
        r.action_hash = None;
    }
    if !keep_field(scope, profile, CaptureField::OutcomeHash) {
        r.outcome_hash = None;
    }
    if !keep_field(scope, profile, CaptureField::EmittedAtMs) {
        r.emitted_at_ms = None;
    }
    // `Url`, `StatusCode`, `ErrorDetail` are KEPT under Wire+Minimal —
    // no mutation needed. Identity fields are unconditional.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_service_adapter::host_service_adapter::{Receipt, ReceiptError, ReceiptStatus};
    use loom_core::receipt_builder::receipt_builder::NetworkSummary;
    use loom_shared::navigate_outcome::ShimConsoleLine;

    fn full_receipt() -> Receipt {
        Receipt {
            action_id: 1,
            session_id: "S".into(),
            status: ReceiptStatus::Success,
            timing_ticks: 100,
            side_effects: vec![serde_json::json!({"method": "GET"})],
            error: None,
            action_hash: Some("a".into()),
            outcome_hash: Some("o".into()),
            emitted_at_ms: Some(1234),
            url: Some("https://example.com".into()),
            final_url: Some("https://example.com/".into()),
            title: Some("Example".into()),
            status_code: Some(200),
            dom_snapshot_hash: Some("dom".into()),
            dom_after_hash: None,
            screenshot_after_hash: Some("ss".into()),
            console_count: Some(3),
            network_count: Some(7),
            console_lines: vec![ShimConsoleLine {
                level: "info".into(),
                message: "hello".into(),
            }],
            network_summary: Some(NetworkSummary {
                total_count: 7,
                total_bytes: 1024,
                error_count: 0,
            }),
            network_entries: vec![serde_json::json!({"url": "https://example.com/api"})],
            network_entries_blob_ref: None,
            network_entries_truncated: None,
            settle_until: Some("settled".into()),
            settle_outcome: Some("reached".into()),
            return_value_json: None,
            return_value_blob_ref: None,
            set_cookies_result: None,
            get_cookies_result: None,
            clear_cookies_result: None,
            delete_cookies_result: None,
            scroll_result: None,
        }
    }

    /// Default profile is a no-op. Every field comes through unchanged.
    #[test]
    fn default_profile_no_op() {
        let mut r = full_receipt();
        let before = serde_json::to_value(&r).unwrap();
        apply_capture_profile_to_wire(&mut r, CaptureProfile::Default);
        let after = serde_json::to_value(&r).unwrap();
        assert_eq!(before, after);
    }

    /// Full profile is also a no-op today; would change once
    /// `dom_full_text` lands.
    #[test]
    fn full_profile_no_op_today() {
        let mut r = full_receipt();
        let before = serde_json::to_value(&r).unwrap();
        apply_capture_profile_to_wire(&mut r, CaptureProfile::Full);
        let after = serde_json::to_value(&r).unwrap();
        assert_eq!(before, after);
    }

    /// Minimal profile (brief): only `url`,
    /// `status_code`, `error` survive (+ identity fields).
    #[test]
    fn minimal_strips_per_brief() {
        let mut r = full_receipt();
        r.error = Some(ReceiptError {
            kind: "http_status".into(),
            detail: Some(serde_json::json!({"status_code": 404})),
        });
        apply_capture_profile_to_wire(&mut r, CaptureProfile::Minimal);

        // Identity + brief-listed survivors:
        assert_eq!(r.action_id, 1);
        assert_eq!(r.session_id, "S");
        assert_eq!(r.status, ReceiptStatus::Success);
        assert_eq!(r.timing_ticks, 100);
        assert_eq!(r.url.as_deref(), Some("https://example.com"));
        assert_eq!(r.status_code, Some(200));
        assert!(r.error.is_some());

        // Stripped:
        assert!(r.final_url.is_none(), "final_url must be stripped");
        assert!(r.title.is_none(), "title must be stripped");
        assert!(
            r.dom_snapshot_hash.is_none(),
            "dom_snapshot_hash must be stripped"
        );
        assert!(
            r.screenshot_after_hash.is_none(),
            "screenshot_after_hash must be stripped"
        );
        assert!(r.console_count.is_none(), "console_count must be stripped");
        assert!(r.console_lines.is_empty(), "console_lines must be empty");
        assert!(r.network_count.is_none(), "network_count must be stripped");
        assert!(
            r.network_summary.is_none(),
            "network_summary must be stripped"
        );
        assert!(r.side_effects.is_empty(), "side_effects must be empty");
        assert!(r.action_hash.is_none(), "action_hash must be stripped");
        assert!(r.outcome_hash.is_none(), "outcome_hash must be stripped");
        assert!(r.emitted_at_ms.is_none(), "emitted_at_ms must be stripped");
    }
}
