//! Cross-run determinism — clock-anchor threading (Cluster A, Rust-only).
//!
//! `--clock-anchor M` is plumbed (CLI → RPC → daemon) onto
//! `SessionCreateOpts.started_at_ms_override`, which already drives BOTH the
//! injected `epoch_ms` (page `Date.now`/`performance.now`) AND the manifest
//! Header `started_at_ms`, and round-trips through replay. This test pins the
//! load-bearing half: a fixed anchor produces a fixed recorded start time across
//! independent fresh sessions, while the default (no anchor) records real
//! wall-clock. The in-Chromium `dom_snapshot_hash` half is the real-Chromium
//! `tests/e2e/run_e2e.sh` Section 16.

use loom_core::budget_enforcer::{BudgetEnforcer, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestEntry, ManifestWriter};
use loom_core::observability::Observability;
use loom_core::session_manager::{LocalSessionManager, SessionCreateOpts};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

const ANCHOR: u64 = 1_700_000_000_000; // 2023-11-14; far below any real "now".

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, loom_keychain::KeychainError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
    fn set_secret(
        &self,
        _label: &str,
        _secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn delete_secret(&self, _label: &str) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn list_labels(&self) -> Result<Vec<String>, loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
}

fn make_sm(tmp: &str) -> Arc<LocalSessionManager> {
    let obs = Observability::new(PathBuf::from(format!("{tmp}/loom.log")), false);
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        PathBuf::from(format!("{tmp}/store")),
        obs.clone(),
    ));
    let mw: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(
        PathBuf::from(format!("{tmp}/sessions")),
        obs.clone(),
    ));
    let kc: Arc<dyn KeychainAccess> = Arc::new(StubKc);
    let v: Arc<dyn Vault> = Arc::new(LocalVault::new(kc, mw.clone(), obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = Arc::new(LocalBudgetEnforcer::new(obs.clone()));
    let dh = Arc::new(DeterminismHarness::new(0, mw.clone()));
    LocalSessionManager::new(
        cs,
        mw,
        v,
        be,
        dh,
        obs,
        0,
        std::path::PathBuf::from("/tmp/loom-test/sessions"),
    )
}

/// `--clock-anchor M` maps to this opts shape (daemon sets
/// `started_at_ms_override: clock_anchor`).
fn opts_with_anchor(anchor: Option<u64>) -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "test".into(),
        seed: None,
        limits: None,
        replay_of: None,
        started_at_ms_override: anchor,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    }
}

fn header_started_at_ms(root: &str, id: &str) -> u64 {
    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(id)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).expect("manifest.wal must exist");
    let header: ManifestEntry =
        serde_json::from_str(contents.lines().next().unwrap()).expect("Header parses");
    match header {
        ManifestEntry::Header { started_at_ms, .. } => started_at_ms,
        other => panic!("first entry must be a Header, got {other:?}"),
    }
}

#[test]
fn clock_anchor_pins_header_started_at_ms_identically_across_fresh_sessions() {
    // Two INDEPENDENT fresh sessions with the same anchor must record the same
    // start time — the per-run wall-clock leak the feature closes.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root);

    let a = sm.create(opts_with_anchor(Some(ANCHOR))).unwrap();
    let b = sm.create(opts_with_anchor(Some(ANCHOR))).unwrap();

    assert_eq!(header_started_at_ms(root, &a.0), ANCHOR);
    assert_eq!(header_started_at_ms(root, &b.0), ANCHOR);
    assert_eq!(
        header_started_at_ms(root, &a.0),
        header_started_at_ms(root, &b.0),
        "same anchor → identical recorded start time across fresh runs"
    );
}

#[test]
fn without_anchor_started_at_ms_is_real_wall_clock_not_the_anchor() {
    // Negative control: absent the flag, the recorded start time is real
    // wall-clock (well above the fixed 2023 anchor), proving the anchor is
    // load-bearing, not a no-op.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root);

    let s = sm.create(opts_with_anchor(None)).unwrap();
    let started = header_started_at_ms(root, &s.0);
    assert_ne!(
        started, ANCHOR,
        "no anchor must not record the anchor value"
    );
    assert!(
        started > ANCHOR,
        "unanchored start time should be a real recent epoch (> {ANCHOR}), got {started}"
    );
}
