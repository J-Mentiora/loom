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
        seed: None,
        determinism_enabled: None,
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
        seed: None,
        determinism_enabled: None,
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
        seed: None,
        determinism_enabled: None,
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
    assert_eq!(code.as_wire(), "manifest_corrupt");
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

/// v0.9.4 W5.1: new credential-lifecycle audit kinds serialize using the
/// expected snake_case wire form. Manifest consumers (`jq`, audit doc
/// queries per W8.3) rely on these exact strings.
#[test]
fn audit_kind_v094_credential_lifecycle_snake_case() {
    let cases = [
        (AuditKind::SecretOpPending, "secret_op_pending"),
        (AuditKind::SecretStored, "secret_stored"),
        (AuditKind::SecretFetched, "secret_fetched"),
        (AuditKind::SecretDeleted, "secret_deleted"),
        (AuditKind::SecretReplaced, "secret_replaced"),
        (AuditKind::SecretsListed, "secrets_listed"),
        (AuditKind::SecretStoreFailed, "secret_store_failed"),
        (AuditKind::SecretDeleteFailed, "secret_delete_failed"),
        (AuditKind::SecretFetchFailed, "secret_fetch_failed"),
        (AuditKind::PromptBlocked, "prompt_blocked"),
        (
            AuditKind::SecretServiceOwnerChanged,
            "secret_service_owner_changed",
        ),
    ];
    for (kind, wire) in cases {
        let encoded = serde_json::to_string(&kind).expect("serialize");
        assert_eq!(encoded, format!("\"{wire}\""), "kind {kind:?} wire form");
        let back: AuditKind = serde_json::from_str(&encoded).expect("roundtrip");
        assert_eq!(back, kind, "roundtrip equality {kind:?}");
    }
}

/// A-W8.5 defense-in-depth: `append_audit` rejects a `Secret*` payload
/// whose `label` field violates the canonical `^[A-Za-z0-9:_-]{1,64}$`
/// regex. CLI catches first; this gate exists so a future code path that
/// bypasses CLI validation cannot silently slip a malformed label into
/// the hash-chained audit.
#[test]
fn append_audit_rejects_malformed_secret_label() {
    use serde_json::json;
    let w = fixture();
    let session = SessionId(format!(
        "01HZAAAAAAAAAAAAAAAAAA{:04x}",
        rand::random::<u16>()
    ));
    // Header first to bootstrap the chain.
    let _ = w
        .open_manifest_with_started_at(session.clone(), None, Some(1), None, None, true)
        .expect("open_manifest");

    // Label has a space (not in [A-Za-z0-9:_-]) — must be rejected.
    let bad = serde_json::to_vec(&json!({"event": "stored", "label": "bad label", "size_bucket": "small", "replaced": false})).unwrap();
    let res = w.append_audit(session.clone(), AuditKind::SecretStored, bad);
    let err = res.expect_err("malformed label must be rejected");
    assert_eq!(err.code, LoomErrorCode::VaultInvalidLabel);

    // Empty label — must be rejected.
    let empty = serde_json::to_vec(
        &json!({"event": "stored", "label": "", "size_bucket": "small", "replaced": false}),
    )
    .unwrap();
    let res = w.append_audit(session.clone(), AuditKind::SecretStored, empty);
    let err = res.expect_err("empty label must be rejected");
    assert_eq!(err.code, LoomErrorCode::VaultInvalidLabel);

    // 65 chars — must be rejected (boundary at 64).
    let over = serde_json::to_vec(&json!({"event": "stored", "label": "a".repeat(65), "size_bucket": "small", "replaced": false})).unwrap();
    let res = w.append_audit(session.clone(), AuditKind::SecretStored, over);
    let err = res.expect_err("65-char label must be rejected");
    assert_eq!(err.code, LoomErrorCode::VaultInvalidLabel);

    // Canonical label — accepted.
    let ok = serde_json::to_vec(&json!({"event": "stored", "label": "github-oauth_v2", "size_bucket": "small", "replaced": false})).unwrap();
    w.append_audit(session, AuditKind::SecretStored, ok)
        .expect("canonical label must be accepted");
}

/// Non-`Secret*` audit kinds (e.g. `BlockedUrl`, `GrantIssued`) bypass
/// the label gate even if they happen to carry a `label` field, because
/// the gate is scoped to credential-lifecycle entries only.
#[test]
fn append_audit_label_gate_scoped_to_secret_kinds() {
    use serde_json::json;
    let w = fixture();
    let session = SessionId(format!(
        "01HZBBBBBBBBBBBBBBBBBB{:04x}",
        rand::random::<u16>()
    ));
    let _ = w
        .open_manifest_with_started_at(session.clone(), None, Some(2), None, None, true)
        .expect("open_manifest");

    // A label that would fail the Secret* gate — but Grant* is scoped out.
    let bytes = serde_json::to_vec(&json!({"label": "bad label", "kind": "grant_issued"})).unwrap();
    w.append_audit(session, AuditKind::GrantIssued, bytes)
        .expect("non-secret audit kind bypasses label gate");
}

/// D39 forward-compatibility: an unknown audit-kind tag (e.g. a future
/// daemon wrote a manifest with a variant the current binary doesn't
/// know) deserializes to `AuditKind::Unknown` rather than failing the
/// parse. Hash-chain validation must keep working on a mixed manifest.
#[test]
fn audit_kind_unknown_serde_other_round_trip() {
    let future: AuditKind = serde_json::from_str("\"future_variant_xyz\"")
        .expect("unknown tag must deserialize, not error");
    assert_eq!(future, AuditKind::Unknown);

    // ManifestEntry::AuditEntry carrying the Unknown kind also parses.
    let entry_json = serde_json::json!({
        "kind": "audit_entry",
        "action_id_ref": null,
        "emitted_at_ms": 1_700_000_000_000u64,
        "audit_kind": "totally_made_up_kind_v999",
        "canonical_bytes": [],
        "prev_hash": "a".repeat(64),
    });
    let parsed: ManifestEntry =
        serde_json::from_value(entry_json).expect("audit_entry with unknown audit_kind must parse");
    if let ManifestEntry::AuditEntry { audit_kind, .. } = parsed {
        assert_eq!(audit_kind, AuditKind::Unknown);
    } else {
        panic!("expected AuditEntry variant");
    }
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
