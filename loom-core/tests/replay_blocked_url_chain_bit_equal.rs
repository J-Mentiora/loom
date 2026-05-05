// Behavior tests for replay-engine extension to AuditEntry::BlockedUrl.
//
// Coverage:
//   - replay produces the same blocked-set and the BlockedUrl audit
//     entries participate in the manifest hash chain (validated
//     end-to-end).
//   - Negative: OTHER AuditEntry variants (Vault grant lifecycle)
//     are intentionally NOT re-emitted — replay drops them today and
//     that behavior is unchanged.
//
// Note on "bit-equal replay": existing receipt-bit-equal behaviour
// covers bit-equal `receipt_canonical_bytes`. The Header bytes differ
// across replay (session_id changes), so prev_hash strings of subsequent
// lines also differ — that is by design and not in scope here. These
// tests assert: same blocked URLs at the same canonical_bytes content,
// chained into the replay's own valid hash chain.

use loom_core::budget_enforcer::{BudgetEnforcer, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{
    AuditKind, LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId,
};
use loom_core::observability::Observability;
use loom_core::replay_engine::{LocalReplayEngine, ReplayEngine, ReplayOpts};
use loom_core::session_manager::LocalSessionManager;
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, LoomError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
}

fn make_engine_and_writer() -> (
    tempfile::TempDir,
    Arc<LocalManifestWriter>,
    LocalReplayEngine,
    PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let obs = Observability::new(tmp.path().join("loom.log"), false);
    let mw = Arc::new(LocalManifestWriter::new(
        tmp.path().join("sessions"),
        obs.clone(),
    ));
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        tmp.path().join("store"),
        obs.clone(),
    ));
    let mw_dyn: Arc<dyn ManifestWriter> = mw.clone();
    let dh = Arc::new(DeterminismHarness::new(7, mw_dyn.clone()));
    let kc: Arc<dyn KeychainAccess> = Arc::new(StubKc);
    let v: Arc<dyn Vault> = Arc::new(LocalVault::new(kc, mw_dyn.clone(), obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = Arc::new(LocalBudgetEnforcer::new(obs.clone()));
    let sessions_root = tmp.path().join("sessions");
    let sm = LocalSessionManager::new(
        cs.clone(),
        mw_dyn.clone(),
        v,
        be,
        dh.clone(),
        Observability::new(PathBuf::from("/dev/null"), false),
        0,
        sessions_root.clone(),
    );
    let engine = LocalReplayEngine::new(
        cs,
        mw_dyn,
        dh,
        Observability::new(tmp.path().join("replay.log"), false),
        sm,
        sessions_root.clone(),
    );
    (tmp, mw, engine, sessions_root)
}

/// Build a source manifest with `Header` + 2 `ActionReceipt` + 1
/// `AuditEntry { kind: BlockedUrl }` (between the receipts) +
/// `SessionTerminal`. The interleaving is what makes this scenario
/// non-trivial: replay must preserve the LINE ORDER and hash chain
/// across mixed variant types.
fn build_session_with_blocked_url(
    mw: &dyn ManifestWriter,
    sessions_root: &std::path::Path,
) -> SessionId {
    let id = SessionId("01TESTBLOCKEDURL00000000AB".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();

    // Receipt 0
    let r0 = serde_jcs::to_string(&serde_json::json!({"action_id": 0, "kind": "noop"}))
        .unwrap()
        .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: r0,
            prev_hash: String::new(),
        },
    )
    .unwrap();

    // BlockedUrl audit entry — the variant under test.
    let audit_bytes = serde_jcs::to_string(&serde_json::json!({
        "matched_pattern": "*.google-analytics.com",
        "reason": "analytics",
        "url": "https://www.google-analytics.com/collect?v=1"
    }))
    .unwrap()
    .into_bytes();
    mw.append_audit(id.clone(), AuditKind::BlockedUrl, audit_bytes)
        .unwrap();

    // Receipt 1
    let r1 = serde_jcs::to_string(&serde_json::json!({"action_id": 1, "kind": "navigate"}))
        .unwrap()
        .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 2_000,
            receipt_canonical_bytes: r1,
            prev_hash: String::new(),
        },
    )
    .unwrap();

    // SessionTerminal closes the WAL.
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 2,
            emitted_at_ms: 3_000,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    id
}

/// Read the WAL and return each line's `prev_hash` field (or empty for
/// the Header). Used to assert chain bit-equality across replay.
fn read_prev_hashes(sessions_root: &std::path::Path, id: &SessionId) -> Vec<String> {
    let path = sessions_root.join(&id.0).join("manifest.wal");
    let content = std::fs::read_to_string(&path).expect("WAL exists");
    content
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("WAL line is JSON");
            v.get("prev_hash")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// Read the WAL and return the variant tag of each entry (the `kind` field).
fn read_kinds(sessions_root: &std::path::Path, id: &SessionId) -> Vec<String> {
    let path = sessions_root.join(&id.0).join("manifest.wal");
    let content = std::fs::read_to_string(&path).expect("WAL exists");
    content
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).expect("WAL line is JSON");
            v.get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// Extract `(audit_kind, canonical_bytes)` for every `AuditEntry`
/// variant in the WAL. Used to compare blocked-set content across
/// source and replay.
fn extract_audit_entries(
    sessions_root: &std::path::Path,
    id: &SessionId,
) -> Vec<(String, Vec<u8>)> {
    let path = sessions_root.join(&id.0).join("manifest.wal");
    let content = std::fs::read_to_string(&path).expect("WAL exists");
    let mut out = Vec::new();
    for line in content.lines() {
        if let Ok(ManifestEntry::AuditEntry {
            audit_kind,
            canonical_bytes,
            ..
        }) = serde_json::from_str::<ManifestEntry>(line)
        {
            let kind_tag = serde_json::to_string(&audit_kind).unwrap();
            out.push((kind_tag, canonical_bytes));
        }
    }
    out
}

// === blocked-url replay ===

#[test]
fn test_replay_preserves_blocked_url_audit_entry_in_chain() {
    let (_tmp, mw, engine, sessions_root) = make_engine_and_writer();
    let source_id =
        build_session_with_blocked_url(mw.as_ref() as &dyn ManifestWriter, &sessions_root);

    // Sanity: source manifest has Header + Receipt + AuditEntry + Receipt + SessionTerminal.
    let source_kinds = read_kinds(&sessions_root, &source_id);
    assert_eq!(
        source_kinds,
        vec![
            "header",
            "action_receipt",
            "audit_entry",
            "action_receipt",
            "session_terminal"
        ],
        "source manifest layout pre-condition"
    );

    let replay_id = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .expect("replay should succeed");

    // Replay must contain the same line order MINUS the source's
    // SessionTerminal (which the replay engine writes its OWN terminal
    // for, with reason='replay_complete'). The Header is auto-emitted
    // by SessionManager::create with the source's started_at_ms.
    let replay_kinds = read_kinds(&sessions_root, &replay_id);
    assert_eq!(
        replay_kinds,
        vec![
            "header",
            "action_receipt",
            "audit_entry",
            "action_receipt",
            "session_terminal"
        ],
        "replay manifest layout must preserve audit-entry interleaving"
    );

    // Same blocked-set content. Audit entry
    // canonical_bytes are byte-equal between source and replay (the
    // replay engine copies them verbatim).
    let source_audits = extract_audit_entries(&sessions_root, &source_id);
    let replay_audits = extract_audit_entries(&sessions_root, &replay_id);
    assert_eq!(
        source_audits, replay_audits,
        "BlockedUrl audit set (kind + canonical_bytes) must match across replay"
    );
    assert_eq!(
        replay_audits.len(),
        1,
        "exactly one BlockedUrl audit entry in replay"
    );
    assert!(
        String::from_utf8_lossy(&replay_audits[0].1).contains("\"reason\":\"analytics\""),
        "replayed BlockedUrl audit must carry reason='analytics' (JCS canonical)"
    );

    // The replay manifest's hash chain is internally consistent (audit
    // entry's prev_hash chains correctly to the previous WAL line).
    // `validate()` walks the entire chain and would Err on any break.
    mw.validate(replay_id.clone())
        .expect("replay manifest hash chain must validate");

    // The audit entry's prev_hash must be non-empty (it IS in the chain
    // — not appended to a side log). Position 2 = AuditEntry.
    let replay_hashes = read_prev_hashes(&sessions_root, &replay_id);
    assert!(
        !replay_hashes[2].is_empty(),
        "BlockedUrl AuditEntry must carry a non-empty prev_hash (chained, not orphaned)"
    );
    assert_eq!(
        replay_hashes[2].len(),
        64,
        "prev_hash is sha256 hex (64 chars)"
    );

    // Sanity: source hash chain still validates too.
    mw.validate(source_id)
        .expect("source manifest hash chain must validate");
}

/// Negative regression — Vault grant audit entries are NOT re-emitted
/// during replay (out of scope for this test file; existing behavior
/// preserved). This pins the "explicit allowlist" decision so a future
/// fall-through pattern would fail this test.
#[test]
fn test_replay_does_not_re_emit_vault_audit_entry() {
    let (_tmp, mw, engine, sessions_root) = make_engine_and_writer();

    let id = SessionId("01TESTVAULTSCOPE00000000AB".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    let receipt = serde_jcs::to_string(&serde_json::json!({"action_id": 0, "kind": "noop"}))
        .unwrap()
        .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    // A GrantConsumed audit entry — replay should DROP this.
    mw.append_audit(
        id.clone(),
        AuditKind::GrantConsumed,
        serde_jcs::to_string(&serde_json::json!({"grant_id": "g0"}))
            .unwrap()
            .into_bytes(),
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 2_000,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let replay_id = engine
        .replay(id.clone(), ReplayOpts::default())
        .expect("replay should succeed");

    let replay_kinds = read_kinds(&sessions_root, &replay_id);
    // Header + ActionReceipt + SessionTerminal — the GrantConsumed
    // AuditEntry is NOT in the replay manifest. Replay only
    // covers BlockedUrl; Vault audit replay scope is a separate
    // out-of-scope concern.
    assert_eq!(
        replay_kinds,
        vec!["header", "action_receipt", "session_terminal"],
        "replay must not re-emit Vault GrantConsumed audit entries (out of scope)"
    );
}
