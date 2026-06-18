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
use loom_core::manifest_writer::{LocalManifestWriter, ManifestEntry, ManifestWriter};
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
    LocalSessionManager::new(
        cs,
        mw,
        v,
        be,
        obs,
        default_seed,
        PathBuf::from(format!("{tmp}/sessions")),
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
        no_determinism: false,
        record_screencast: false,
        profile: "safe".to_string(),
    }
}

#[test]
fn seed_is_recorded_in_manifest_header_for_replay() {
    // D3 (settle-capture): a `--seed 42` session must RECORD seed=42 in the
    // manifest Header so replay can reconstruct it. Before this fix the seed
    // was never recorded, so replay silently created the session with
    // Seed(default) and the in-Chromium Math.random/Date.now diverged.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root, 99);
    let id = sm.create(opts(Some(42))).unwrap();

    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(&id.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).expect("manifest.wal must exist");
    let header_line = contents
        .lines()
        .next()
        .expect("Header is the first WAL line");
    let header: ManifestEntry = serde_json::from_str(header_line).expect("Header must parse");
    match header {
        ManifestEntry::Header { seed, .. } => {
            assert_eq!(
                seed,
                Some(42),
                "the manifest Header must record the session's determinism seed"
            );
        }
        other => panic!("first manifest entry must be a Header, got {other:?}"),
    }
}

#[test]
fn no_determinism_is_recorded_in_manifest_header() {
    // settle-capture (4b): a `--no-determinism` session must RECORD
    // determinism_enabled=false in the Header so replay can REFUSE it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root, 99);
    let mut o = opts(Some(7));
    o.no_determinism = true;
    let id = sm.create(o).unwrap();

    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(&id.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).expect("manifest.wal must exist");
    let header: ManifestEntry =
        serde_json::from_str(contents.lines().next().unwrap()).expect("Header parses");
    match header {
        ManifestEntry::Header {
            determinism_enabled,
            ..
        } => assert_eq!(
            determinism_enabled,
            Some(false),
            "a --no-determinism session must record determinism_enabled=false"
        ),
        other => panic!("first entry must be a Header, got {other:?}"),
    }
}

#[test]
fn clock_anchor_pins_header_started_at_ms() {
    // cross-run determinism (Cluster A): a `--clock-anchor M` session maps to
    // `started_at_ms_override: Some(M)`, which must pin the manifest Header's
    // `started_at_ms` to exactly M (instead of wall-clock now_ms()). The same
    // override drives `epoch_ms` → CDP initialVirtualTime, so equal Header
    // started_at_ms across two fresh runs ⇒ equal injected browser clock.
    const ANCHOR: u64 = 1_700_000_000_000;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root, 99);
    let mut o = opts(Some(42));
    o.started_at_ms_override = Some(ANCHOR);
    let id = sm.create(o).unwrap();

    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(&id.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).expect("manifest.wal must exist");
    let header: ManifestEntry =
        serde_json::from_str(contents.lines().next().unwrap()).expect("Header parses");
    match header {
        ManifestEntry::Header { started_at_ms, .. } => assert_eq!(
            started_at_ms, ANCHOR,
            "a --clock-anchor session must pin Header started_at_ms to the anchor epoch"
        ),
        other => panic!("first entry must be a Header, got {other:?}"),
    }
}

#[test]
fn without_clock_anchor_started_at_ms_is_wall_clock_not_the_anchor() {
    // Converse: with no anchor, the Header records wall-clock now_ms(), NOT the
    // anchor value — proving the anchor is load-bearing (matches the e2e §21
    // negative control: a no-anchor session diffs nonzero against an anchored one).
    const ANCHOR: u64 = 1_700_000_000_000;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root, 99);
    let id = sm.create(opts(Some(42))).unwrap(); // started_at_ms_override: None
    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(&id.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).unwrap();
    let header: ManifestEntry = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    match header {
        ManifestEntry::Header { started_at_ms, .. } => assert_ne!(
            started_at_ms, ANCHOR,
            "a session WITHOUT --clock-anchor must use wall-clock, not the anchor"
        ),
        other => panic!("first entry must be a Header, got {other:?}"),
    }
}

#[test]
fn deterministic_session_records_determinism_enabled_true() {
    // The default (deterministic) session records `Some(true)` — explicit, so
    // an operator inspecting the Header sees determinism was on.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let sm = make_sm(root, 99);
    let id = sm.create(opts(Some(7))).unwrap();
    let wal = std::path::Path::new(root)
        .join("sessions")
        .join(&id.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal).unwrap();
    let header: ManifestEntry = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    match header {
        ManifestEntry::Header {
            determinism_enabled,
            ..
        } => assert_eq!(determinism_enabled, Some(true)),
        other => panic!("first entry must be a Header, got {other:?}"),
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

// === Per-session DeterminismHarness (audit: the harness used to be a
//     daemon-wide singleton seeded once with default_seed, so the host
//     RNG ignored --seed and interleaved draws across concurrent
//     sessions) ===

#[test]
fn explicit_seed_seeds_the_sessions_own_harness() {
    // `--seed 42` must reach the session's host-RNG harness — not just
    // the in-Chromium JS template. The Header records the same resolved
    // value, so replay reconstructs an identically-seeded harness.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let with_seed = sm.get(sm.create(opts(Some(42))).unwrap()).unwrap();
    assert_eq!(
        with_seed.determinism.seed(),
        42,
        "Session.determinism must be seeded with the session's resolved seed"
    );
    // Documented default: no explicit --seed → a FRESH harness seeded
    // with the manager's default_seed (still per-session state).
    let defaulted = sm.get(sm.create(opts(None)).unwrap()).unwrap();
    assert_eq!(
        defaulted.determinism.seed(),
        99,
        "sessions without --seed get a harness seeded with default_seed"
    );
    assert!(
        !Arc::ptr_eq(&with_seed.determinism, &defaulted.determinism),
        "each session must own a distinct harness instance (no shared singleton)"
    );
}

#[test]
fn concurrent_sessions_get_independent_reproducible_rng_streams() {
    // RUN 1: two sessions with different seeds draw concurrently from
    // two threads — with a shared singleton harness the interleaving
    // would split one global ChaCha20 stream between them.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let a = sm.get(sm.create(opts(Some(7))).unwrap()).unwrap();
    let b = sm.get(sm.create(opts(Some(1234))).unwrap()).unwrap();
    let (a_th, b_th) = (Arc::clone(&a), Arc::clone(&b));
    let ta = std::thread::spawn(move || {
        (0..256)
            .map(|_| a_th.determinism.rng_next())
            .collect::<Vec<u64>>()
    });
    let tb = std::thread::spawn(move || {
        (0..256)
            .map(|_| b_th.determinism.rng_next())
            .collect::<Vec<u64>>()
    });
    let run1_a = ta.join().unwrap();
    let run1_b = tb.join().unwrap();
    assert_ne!(
        run1_a, run1_b,
        "different seeds must yield different streams"
    );

    // RUN 2: same seeds, fresh manager, plain sequential draws — must
    // reproduce run 1 exactly, proving the concurrent interleaving in
    // run 1 could not perturb either session's stream.
    let tmp2 = tempfile::tempdir().unwrap();
    let sm2 = make_sm(tmp2.path().to_str().unwrap(), 99);
    let a_again = sm2.get(sm2.create(opts(Some(7))).unwrap()).unwrap();
    let b_again = sm2.get(sm2.create(opts(Some(1234))).unwrap()).unwrap();
    let run2_a: Vec<u64> = (0..256).map(|_| a_again.determinism.rng_next()).collect();
    let run2_b: Vec<u64> = (0..256).map(|_| b_again.determinism.rng_next()).collect();
    assert_eq!(
        run1_a, run2_a,
        "seed=7 stream must be reproducible regardless of concurrent sessions"
    );
    assert_eq!(
        run1_b, run2_b,
        "seed=1234 stream must be reproducible regardless of concurrent sessions"
    );
}

#[test]
fn session_virtual_clocks_do_not_bleed_across_sessions() {
    // The singleton harness also shared action_clock_ms, so one
    // session's begin_action() advanced every session's clock.
    let tmp = tempfile::tempdir().unwrap();
    let sm = make_sm(tmp.path().to_str().unwrap(), 99);
    let a = sm.get(sm.create(opts(Some(7))).unwrap()).unwrap();
    let b = sm.get(sm.create(opts(Some(7))).unwrap()).unwrap();
    a.determinism.begin_action(50);
    assert_eq!(a.determinism.clock_now(), 50);
    assert_eq!(
        b.determinism.clock_now(),
        0,
        "advancing one session's virtual clock must not move another session's"
    );
}
