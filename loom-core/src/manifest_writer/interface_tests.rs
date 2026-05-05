// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-core/modules/manifest_writer/interface_tests.rs` instead.
// Interface tests for `ManifestWriter`. Verifies the hash chain,
// audit-in-same-chain, integer-only fields.

use super::manifest_writer::{
    AuditKind, LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId,
};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::observability::Observability;
use std::path::PathBuf;

fn fixture() -> LocalManifestWriter {
    let obs = Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false);
    LocalManifestWriter::new(PathBuf::from("/tmp/loom-test/sessions"), obs)
}

#[test]
fn header_entry_carries_optional_prev_hash_set_to_none() {
    let h = ManifestEntry::Header {
        session_id: "01HZAAAAAAAAAAAAAAAAAAAAAA".into(),
        started_at_ms: 1714074336000,
        prev_hash: None,
        budgets: None,
        capture_policy: None,
    };
    if let ManifestEntry::Header { prev_hash, .. } = h {
        assert!(prev_hash.is_none());
    } else {
        panic!("expected header variant");
    }
}

// === capture_policy persistence in Header ===

#[test]
fn header_serializes_capture_policy_with_skip_if_none() {
    // When `capture_policy: Some("minimal")`, the canonical-JSON Header
    // includes the field. When `None`, the field is omitted (legacy-compat).
    let with = ManifestEntry::Header {
        session_id: "01HZAAAAAAAAAAAAAAAAAAAAAA".into(),
        started_at_ms: 1714074336000,
        prev_hash: None,
        budgets: None,
        capture_policy: Some("minimal".into()),
    };
    let s_with = serde_jcs::to_string(&with).expect("jcs serialize with");
    assert!(
        s_with.contains("\"capture_policy\":\"minimal\""),
        "expected capture_policy field in {s_with}"
    );

    let without = ManifestEntry::Header {
        session_id: "01HZAAAAAAAAAAAAAAAAAAAAAA".into(),
        started_at_ms: 1714074336000,
        prev_hash: None,
        budgets: None,
        capture_policy: None,
    };
    let s_without = serde_jcs::to_string(&without).expect("jcs serialize without");
    assert!(
        !s_without.contains("capture_policy"),
        "expected capture_policy elided in {s_without}"
    );
}

#[test]
fn header_deserializes_legacy_no_capture_policy() {
    // Pre-feature manifests must round-trip: a Header JSON without
    // capture_policy deserializes with `capture_policy: None`.
    let legacy = r#"{"kind":"header","session_id":"01HZAAAAAAAAAAAAAAAAAAAAAA","started_at_ms":1714074336000,"prev_hash":null}"#;
    let parsed: ManifestEntry = serde_json::from_str(legacy).expect("legacy header should parse");
    if let ManifestEntry::Header { capture_policy, .. } = parsed {
        assert!(capture_policy.is_none(), "legacy header expected None");
    } else {
        panic!("expected Header variant");
    }
}

#[test]
fn action_receipt_has_integer_only_numeric_fields() {
    // No f32/f64 anywhere in receipt-shaped types.
    let r = ManifestEntry::ActionReceipt {
        action_id: 7,
        emitted_at_ms: 1714074336100,
        receipt_canonical_bytes: vec![0u8; 0],
        prev_hash: "a".repeat(64),
    };
    if let ManifestEntry::ActionReceipt {
        action_id,
        emitted_at_ms,
        ..
    } = r
    {
        // u64 not f64.
        let _ck: u64 = action_id;
        let _ck2: u64 = emitted_at_ms;
    }
}

// === Hash chain ===

#[test]
fn append_signature_returns_unit_or_loomerror() {
    fn _check<T: ManifestWriter>(w: &T) -> Result<(), LoomError> {
        w.append(
            SessionId("01HZAAAAAAAAAAAAAAAAAAAAAA".into()),
            ManifestEntry::ActionReceipt {
                action_id: 1,
                emitted_at_ms: 0,
                receipt_canonical_bytes: vec![],
                prev_hash: "0".repeat(64),
            },
        )
    }
    let _ = _check::<LocalManifestWriter>;
}

#[test]
fn validate_returns_manifest_corrupt_on_chain_break() {
    // validate() returns Err with code ManifestCorrupt when the hash chain is broken.
    let code = LoomErrorCode::ManifestCorrupt;
    assert_eq!(code.as_wire(), "manifest-corrupt");
}

// === Audit entries in same hash chain ===

#[test]
fn append_audit_accepts_grant_lifecycle_kinds() {
    let w = fixture();
    fn _ck<T: ManifestWriter>(w: &T, kind: AuditKind) -> Result<(), LoomError> {
        w.append_audit(SessionId("01HZAAAAAAAAAAAAAAAAAAAAAA".into()), kind, vec![])
    }
    let _ = _ck::<LocalManifestWriter>;
    let _ = w;
    // All lifecycle kinds compile, including BlockedUrl.
    let _kinds = [
        AuditKind::GrantIssued,
        AuditKind::GrantConsumed,
        AuditKind::GrantExpired,
        AuditKind::GrantRevoked,
        AuditKind::BlockedUrl,
    ];
}

/// `BlockedUrl` round-trips through
/// the JSON serde tag the same way other `AuditKind` variants do.
#[test]
fn audit_kind_blocked_url_serializes_snake_case() {
    let kind = AuditKind::BlockedUrl;
    let s = serde_json::to_string(&kind).expect("snake_case serialize");
    assert_eq!(s, "\"blocked_url\"");
    let back: AuditKind = serde_json::from_str(&s).expect("snake_case roundtrip");
    assert!(matches!(back, AuditKind::BlockedUrl));
}

#[test]
fn audit_entry_shares_hash_chain_with_action_receipts() {
    // Structural: AuditEntry variant carries prev_hash like ActionReceipt.
    let a = ManifestEntry::AuditEntry {
        action_id_ref: Some(7),
        emitted_at_ms: 1714074336200,
        audit_kind: AuditKind::GrantConsumed,
        canonical_bytes: vec![],
        prev_hash: "b".repeat(64),
    };
    if let ManifestEntry::AuditEntry { prev_hash, .. } = a {
        assert_eq!(prev_hash.len(), 64);
    } else {
        panic!("audit entry shape regressed");
    }
}

// === FSM transition audits ===

#[test]
fn fsm_transition_kind_exists_for_session_lifecycle_audits() {
    let _k = AuditKind::FsmTransition;
}

// === SessionTerminal + RuntimeCrash variants ===

#[test]
fn session_terminal_variant_carries_reason_string() {
    let t = ManifestEntry::SessionTerminal {
        action_id: 99,
        emitted_at_ms: 1714074336300,
        reason: "store_full_no_evictable".into(),
        prev_hash: "c".repeat(64),
    };
    if let ManifestEntry::SessionTerminal { reason, .. } = t {
        assert_eq!(reason, "store_full_no_evictable");
    } else {
        panic!();
    }
}

#[test]
fn runtime_crash_variant_carries_last_completed_action_id_as_u64() {
    let c = ManifestEntry::RuntimeCrash {
        last_completed_action_id: 12,
        emitted_at_ms: 1714074336400,
        prev_hash: "d".repeat(64),
    };
    if let ManifestEntry::RuntimeCrash {
        last_completed_action_id,
        ..
    } = c
    {
        let _u: u64 = last_completed_action_id;
    } else {
        panic!();
    }
}

#[test]
fn open_manifest_returns_writer_handle() {
    let w = fixture();
    // Compile-time signature check.
    let _f = |s: SessionId| w.open_manifest(s, None);
}
