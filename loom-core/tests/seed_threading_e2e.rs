//! J.7a — End-to-end seed-threading test (Rust-only).
//!
//! Verifies that `LocalSessionManager::create({ seed: Some(N) })`
//! produces a `Session` whose `seed` field carries `N` and whose
//! per-session metadata is what the host then threads onto the shim
//! wire as `ShimRequest::PageNavigate.seed`.
//!
//! `J.7b` (real V8 evaluate against the rendered template) is a
//! `#[ignore]` integration test in `loom-host/tests/v8_seed_determinism.rs`.
//!
//! Covers the seed-threading half of the RNG-determinism contract.

use loom_core::budget_enforcer::{BudgetEnforcer, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter};
use loom_core::observability::Observability;
use loom_core::session_manager::{LocalSessionManager, SessionCreateOpts};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use loom_shared::types::Seed;
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, loom_keychain::KeychainError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
    fn set_secret(&self, _label: &str, _secret: Zeroizing<Vec<u8>>) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(loom_keychain::KeychainErrorKind::Unavailable, "test stub"))
    }
    fn delete_secret(&self, _label: &str) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(loom_keychain::KeychainErrorKind::Unavailable, "test stub"))
    }
    fn list_labels(&self) -> Result<Vec<String>, loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(loom_keychain::KeychainErrorKind::Unavailable, "test stub"))
    }
}

fn make_sm(tmp: &str, default_seed: u64) -> Arc<LocalSessionManager> {
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
    let dh = Arc::new(DeterminismHarness::new(default_seed, mw.clone()));
    LocalSessionManager::new(
        cs,
        mw,
        v,
        be,
        dh,
        obs,
        default_seed,
        std::path::PathBuf::from("/tmp/loom-test/sessions"),
    )
}

fn opts(seed: Option<u64>) -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "test".into(),
        seed,
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        profile: "safe".to_string(),
    }
}

#[test]
fn explicit_seed_threads_through_to_session() {
    // `--seed 42` reaches `Session.seed` byte-equal.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let id = sm.create(opts(Some(42))).unwrap();
    let session = sm.get(id).unwrap();
    assert_eq!(
        session.seed,
        Seed(42),
        "Session.seed must equal the explicit opts.seed=Some(42)"
    );
}

#[test]
fn missing_seed_falls_back_to_default_seed() {
    // The Option<u64> → Seed collapse happens exactly once, at create().
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let id = sm.create(opts(None)).unwrap();
    let session = sm.get(id).unwrap();
    assert_eq!(
        session.seed,
        Seed(99),
        "Session.seed must equal default_seed when opts.seed is None"
    );
}

#[test]
fn two_sessions_with_same_seed_are_byte_identical_on_seed() {
    // Prerequisite: seed identity is preserved across
    // independent session creates with the same explicit seed.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let id_a = sm.create(opts(Some(42))).unwrap();
    let id_b = sm.create(opts(Some(42))).unwrap();
    let a = sm.get(id_a).unwrap();
    let b = sm.get(id_b).unwrap();
    assert_eq!(
        a.seed, b.seed,
        "two sessions with seed=42 must carry the same Seed"
    );
    assert_eq!(a.seed, Seed(42));
}

#[test]
fn different_seeds_produce_different_session_seed() {
    // Prerequisite: explicit seeds are NOT collapsed to the
    // default. seed=0 ≠ seed=42 ≠ default_seed.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let id_zero = sm.create(opts(Some(0))).unwrap();
    let id_42 = sm.create(opts(Some(42))).unwrap();
    let s_zero = sm.get(id_zero).unwrap();
    let s_42 = sm.get(id_42).unwrap();
    assert_eq!(
        s_zero.seed,
        Seed(0),
        "Seed(0) is a real value, NOT a sentinel meaning 'use default'"
    );
    assert_eq!(s_42.seed, Seed(42));
    assert_ne!(s_zero.seed, s_42.seed);
}

#[test]
fn seed_zero_is_not_a_sentinel_for_default() {
    // Architectural invariant: `Seed(0)` is a real value distinct from
    // "use default seed". An `Option<Seed>` carries that distinction —
    // collapsing to a sentinel would silently break determinism for
    // callers who explicitly want zero.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let id = sm.create(opts(Some(0))).unwrap();
    let session = sm.get(id).unwrap();
    assert_eq!(
        session.seed,
        Seed(0),
        "explicit Seed(0) MUST NOT collapse to default_seed=99"
    );
    assert_ne!(session.seed, Seed(99));
}
