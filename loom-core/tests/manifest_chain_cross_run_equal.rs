//! Cross-run manifest hash-chain determinism (stable-dom-snapshot-hash, Part 2).
//!
//! Two INDEPENDENT same-seed record runs of identical content must produce a
//! byte-equal manifest hash chain, so agentic-test-studio can reuse loom's
//! hash-chain equality as a cross-run determinism signal. Before this change the
//! chain folded in the Header's random `session_id` + wall-clock `started_at_ms`
//! and every entry's wall-clock `emitted_at_ms`, so two runs never matched.
//!
//! The fix hashes a projection that zeroes those top-level ephemeral fields
//! (they STAY in the stored file for forensics) while the inner
//! `receipt_canonical_bytes` timestamps are made deterministic at record time.
//! These tests exercise the projection directly at the manifest-writer level
//! (runnable, no browser): same receipts + different session_id/timestamps ⇒
//! identical `prev_hash` chain; different content ⇒ different chain.

use loom_core::manifest_writer::{LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use std::path::PathBuf;

fn mw(tmp: &str) -> LocalManifestWriter {
    let obs = Observability::new(PathBuf::from(format!("{tmp}/loom.log")), false);
    LocalManifestWriter::new(PathBuf::from(format!("{tmp}/sessions")), obs)
}

fn prev_hashes(tmp: &str, sid: &SessionId) -> Vec<String> {
    let wal = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
    std::fs::read_to_string(&wal)
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| v["prev_hash"].as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect()
}

/// Write a 2-receipt manifest for `sid` with the given (ephemeral) session-start
/// and emitted timestamps but identical receipt CONTENT. `started_override` makes
/// the Header's started_at_ms differ across "runs".
fn write_run(tmp: &str, sid: &SessionId, started_override: u64, emitted_base: u64) {
    let w = mw(tmp);
    w.open_manifest_with_started_at(
        sid.clone(),
        None,
        Some(started_override),
        None,
        Some(42),
        true,
    )
    .unwrap();
    for action_id in 1..=2u64 {
        w.append(
            sid.clone(),
            ManifestEntry::ActionReceipt {
                action_id,
                emitted_at_ms: emitted_base + action_id, // ephemeral wall-clock
                receipt_canonical_bytes: format!("receipt-{action_id}").into_bytes(),
                prev_hash: String::new(),
            },
        )
        .unwrap();
    }
}

#[test]
fn two_same_content_runs_produce_identical_prev_hash_chain() {
    let tmp = "/tmp/loom-xrun-equal";
    let _ = std::fs::remove_dir_all(tmp);
    // Two independent runs: DIFFERENT session_id + started_at_ms + emitted_at_ms,
    // but IDENTICAL receipt content + action sequence (the deterministic part).
    let a = SessionId("01run-aaaaaaaaaaaaaaaaaaaaaa".into());
    let b = SessionId("01run-bbbbbbbbbbbbbbbbbbbbbb".into());
    write_run(tmp, &a, 1_000_000, 9_000_000);
    write_run(tmp, &b, 2_222_222, 5_555_555);

    let chain_a = prev_hashes(tmp, &a);
    let chain_b = prev_hashes(tmp, &b);
    assert_eq!(chain_a.len(), 3, "header + 2 receipts");
    assert_eq!(
        chain_a, chain_b,
        "two same-content runs must yield a byte-equal prev_hash chain despite \
         differing session_id / started_at_ms / emitted_at_ms"
    );
    // And both chains validate intact.
    mw(tmp).validate(a).expect("run A validates");
    mw(tmp).validate(b).expect("run B validates");
}

#[test]
fn different_receipt_content_yields_different_chain_no_false_equality() {
    let tmp = "/tmp/loom-xrun-diff";
    let _ = std::fs::remove_dir_all(tmp);
    let a = SessionId("01diff-aaaaaaaaaaaaaaaaaaaaa".into());
    write_run(tmp, &a, 1_000_000, 9_000_000);

    // Run with DIFFERENT receipt content.
    let b = SessionId("01diff-bbbbbbbbbbbbbbbbbbbbb".into());
    let w = mw(tmp);
    w.open_manifest_with_started_at(b.clone(), None, Some(1_000_000), None, Some(42), true)
        .unwrap();
    for action_id in 1..=2u64 {
        w.append(
            b.clone(),
            ManifestEntry::ActionReceipt {
                action_id,
                emitted_at_ms: 9_000_000 + action_id,
                receipt_canonical_bytes: format!("DIFFERENT-{action_id}").into_bytes(),
                prev_hash: String::new(),
            },
        )
        .unwrap();
    }
    assert_ne!(
        prev_hashes(tmp, &a),
        prev_hashes(tmp, &b),
        "different receipt content must change the chain (no false collision)"
    );
}

#[test]
fn interleaved_same_seed_sessions_do_not_bleed() {
    // Council C2: two concurrent same-seed sessions must each produce a chain
    // that is a pure function of their OWN sequence — no cross-session bleed.
    // Each session has its own per-session WAL, so interleaving appends must not
    // affect either chain.
    let tmp = "/tmp/loom-xrun-interleave";
    let _ = std::fs::remove_dir_all(tmp);
    let a = SessionId("01ilv-aaaaaaaaaaaaaaaaaaaaaa".into());
    let b = SessionId("01ilv-bbbbbbbbbbbbbbbbbbbbbb".into());
    let w = mw(tmp);
    w.open_manifest_with_started_at(a.clone(), None, Some(10), None, Some(42), true)
        .unwrap();
    w.open_manifest_with_started_at(b.clone(), None, Some(99), None, Some(42), true)
        .unwrap();
    for action_id in 1..=3u64 {
        for (sid, emit) in [(&a, 100), (&b, 7000)] {
            w.append(
                sid.clone(),
                ManifestEntry::ActionReceipt {
                    action_id,
                    emitted_at_ms: emit + action_id,
                    receipt_canonical_bytes: format!("step-{action_id}").into_bytes(),
                    prev_hash: String::new(),
                },
            )
            .unwrap();
        }
    }
    assert_eq!(
        prev_hashes(tmp, &a),
        prev_hashes(tmp, &b),
        "interleaved same-seed/same-content sessions must produce identical chains"
    );
    mw(tmp).validate(a).expect("A validates");
    mw(tmp).validate(b).expect("B validates");
}

// === vault path (audit 2026-06-10) ==========================================
//
// Regression: vault audit payloads used to embed wall-clock `ts_tick`, random
// grant ULIDs and (for cookie audits) the per-run `session_id` inside the
// JCS-encoded `canonical_bytes` — values `hashable_line`'s top-level projection
// cannot reach — so any session that touched the vault could never be
// cross-run hash-equal. The fix derives grant ids from the session seed and
// `ts_tick` from a session-relative event counter. The payload-level pin lives
// in `loom-core/src/vault/impl_local.rs` tests; THIS test closes the loop at
// the chain level: two independent same-seed runs of an identical
// grant → substitute(ok) → substitute(denied) → revoke flow must produce a
// byte-equal `prev_hash` chain through the audit entries (NFR-DET-01).

#[test]
fn vault_audit_path_two_same_seed_runs_chain_bit_equal() {
    use loom_core::vault::{
        CredentialType, GrantOpts, KeychainAccess, LocalVault, NetRequest, RevokeReason, Vault,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use zeroize::Zeroizing;

    struct StubKc;
    impl KeychainAccess for StubKc {
        fn get_secret(
            &self,
            label: &str,
        ) -> Result<Zeroizing<Vec<u8>>, loom_keychain::KeychainError> {
            if label == "github.com/oauth_token" {
                Ok(Zeroizing::new(b"secret-token-bytes".to_vec()))
            } else {
                Err(loom_keychain::KeychainError::new(
                    loom_keychain::KeychainErrorKind::NotFound,
                    "label not found",
                ))
            }
        }
        fn set_secret(
            &self,
            _label: &str,
            _secret: Zeroizing<Vec<u8>>,
        ) -> Result<(), loom_keychain::KeychainError> {
            unreachable!("not exercised")
        }
        fn delete_secret(&self, _label: &str) -> Result<(), loom_keychain::KeychainError> {
            unreachable!("not exercised")
        }
        fn list_labels(&self) -> Result<Vec<String>, loom_keychain::KeychainError> {
            unreachable!("not exercised")
        }
    }

    fn net_req(origin: &str) -> NetRequest {
        NetRequest {
            url: format!("https://{origin}/api"),
            method: "GET".to_string(),
            headers: BTreeMap::new(),
            authorization: None,
            body: vec![],
            origin: origin.to_string(),
            scopes: vec!["repo:read".to_string()],
        }
    }

    let tmp = "/tmp/loom-xrun-vault";
    let _ = std::fs::remove_dir_all(tmp);

    // One identical vault flow per "run": different session_id +
    // started_at_ms (the ephemeral, projected fields), same seed (42) and
    // same event sequence (the deterministic part).
    let run = |sid: &SessionId, started_override: u64| {
        let w = Arc::new(mw(tmp));
        w.open_manifest_with_started_at(
            sid.clone(),
            None,
            Some(started_override),
            None,
            Some(42),
            true,
        )
        .unwrap();
        let obs = Observability::new(PathBuf::from(format!("{tmp}/loom.log")), false);
        let vault = LocalVault::new(Arc::new(StubKc), w.clone() as Arc<dyn ManifestWriter>, obs);
        vault.begin_session(sid, 42);
        let gid = vault
            .grant(
                sid.clone(),
                GrantOpts {
                    credential_type: CredentialType::OAuth,
                    label: "github.com/oauth_token".to_string(),
                    origin: "api.github.com".to_string(),
                    scopes: vec!["repo:read".to_string()],
                    ttl_ms: 3_600_000,
                    threat_model_acknowledged: true,
                },
            )
            .expect("grant");
        let mut ok = net_req("api.github.com");
        vault
            .substitute(sid, gid.clone(), &mut ok)
            .expect("substitute (allowed)");
        let mut denied = net_req("api.gitlab.com");
        vault
            .substitute(sid, gid.clone(), &mut denied)
            .expect_err("substitute (origin mismatch) must be rejected");
        vault
            .revoke(
                gid,
                RevokeReason {
                    reason: "user_request".to_string(),
                },
            )
            .expect("revoke");
    };

    let a = SessionId("01vault-aaaaaaaaaaaaaaaaaaaa".into());
    let b = SessionId("01vault-bbbbbbbbbbbbbbbbbbbb".into());
    run(&a, 1_000_000);
    run(&b, 2_222_222);

    let chain_a = prev_hashes(tmp, &a);
    let chain_b = prev_hashes(tmp, &b);
    // Header + GrantIssued + GrantConsumed + GrantRejected + GrantRevoked.
    assert_eq!(
        chain_a.len(),
        5,
        "expected header + 4 vault audit entries, got {} lines",
        chain_a.len()
    );
    assert_eq!(
        chain_a, chain_b,
        "two same-seed vault flows must yield a byte-equal prev_hash chain \
         despite differing session_id / started_at_ms (NFR-DET-01)"
    );
    mw(tmp).validate(a).expect("run A validates");
    mw(tmp).validate(b).expect("run B validates");
}

#[test]
fn legacy_raw_hash_chain_still_validates_after_upgrade() {
    // Backward-compat (ship council C1/D16): a manifest written BEFORE this change
    // links each line with prev_hash = sha256(raw previous line) — no projection.
    // The upgraded validate() must still accept it (accepts projected OR raw hash),
    // so existing recordings are not rejected as corrupt.
    use ring::digest::{digest, SHA256};
    fn raw_sha(s: &str) -> String {
        digest(&SHA256, s.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    let tmp = "/tmp/loom-xrun-legacy";
    let _ = std::fs::remove_dir_all(tmp);
    let sid = "01legacy-aaaaaaaaaaaaaaaaaa";
    let dir = format!("{tmp}/sessions/{sid}");
    std::fs::create_dir_all(&dir).unwrap();

    // Hand-build a legacy chain: raw-line prev_hash, real session_id + timestamps.
    let header = format!(
        r#"{{"determinism_enabled":true,"kind":"header","seed":42,"session_id":"{sid}","started_at_ms":1717000000000}}"#
    );
    let mut lines = vec![header];
    for action_id in 1..=2u64 {
        let prev = raw_sha(lines.last().unwrap());
        lines.push(format!(
            r#"{{"action_id":{action_id},"emitted_at_ms":{ts},"kind":"action_receipt","prev_hash":"{prev}","receipt_canonical_bytes":[{action_id}]}}"#,
            ts = 1717000000000u64 + action_id
        ));
    }
    std::fs::write(format!("{dir}/manifest.wal"), lines.join("\n")).unwrap();

    mw(tmp)
        .validate(SessionId(sid.to_string()))
        .expect("legacy raw-hash chain must still validate after the upgrade");
}
