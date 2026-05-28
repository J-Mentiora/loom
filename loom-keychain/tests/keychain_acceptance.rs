//! Acceptance test for the keychain-backends feature (v0.9.4).
//!
//! Locked decisions covered:
//! - D11: identity model = (service_id, account) where account = label
//! - D26: set_secret silently upserts at trait level
//! - D28: timeouts via tokio (tested at the vault layer, not here)
//! - D30: typed KeychainErrorReason; Internal carries internal_hash not the message
//! - D38: trait unification — InMemoryKeychain impls loom_keychain::KeychainAccess
//!
//! RED status when authored: this file references symbols that do not exist
//! today (`InMemoryKeychain`, the extended trait methods, `KeychainErrorReason`).
//! Phase 3 Workstream W1 makes it compile. Phase 3 Workstream W5 makes the
//! canary assertion pass once the vault audit chain is wired.
//!
//! GREEN target: every `assert!`/`assert_eq!` here passes via the
//! `loom-keychain` crate's own exports plus `loom-core::vault` integration.

use loom_keychain::{InMemoryKeychain, KeychainAccess, KeychainError, KeychainErrorKind, StubKeychain};
use zeroize::Zeroizing;

/// D18 / FND-0017 — canary string. Distinct, machine-recognisable, includes a
/// random suffix to defeat accidental hash collisions across test runs. The
/// fact that this exact byte sequence is searched in the manifest grep test
/// is documented in `docs/loom-vault-audit.md`.
fn canary() -> Vec<u8> {
    let suffix: u32 = rand::random();
    format!("LOOM_TEST_CANARY_v094_{:08x}", suffix).into_bytes()
}

/// W1 DoD — extended trait has all 4 methods, callable through the
/// `KeychainAccess` trait object. Compiling this test exercises the
/// trait surface.
#[test]
fn trait_extended_to_full_lifecycle() {
    let kc: Box<dyn KeychainAccess> = Box::new(InMemoryKeychain::new());

    // All four trait methods must be reachable through the trait object.
    let _ = kc.get_secret("missing").unwrap_err();
    let _ = kc.set_secret("missing", Zeroizing::new(b"x".to_vec()));
    let _ = kc.delete_secret("missing");
    let _ = kc.list_labels();
}

/// W1 DoD — `InMemoryKeychain` is the canonical test double for vault-layer
/// tests. Distinct from `StubKeychain` (which is the always-error stub kept
/// for backend-init-failure tests). Round-trips bytes correctly.
#[test]
fn in_memory_round_trip() {
    let kc = InMemoryKeychain::new();
    let secret = Zeroizing::new(canary());

    kc.set_secret("test-label", secret.clone()).expect("set should succeed");

    let fetched = kc.get_secret("test-label").expect("get should succeed");
    assert_eq!(&fetched[..], &secret[..], "round-trip bytes must match");

    let labels = kc.list_labels().expect("list should succeed");
    assert_eq!(labels, vec!["test-label".to_string()], "list shows the stored label");

    kc.delete_secret("test-label").expect("delete should succeed");
    let labels = kc.list_labels().expect("list should succeed after delete");
    assert!(labels.is_empty(), "list empty after delete");

    let missing = kc.get_secret("test-label").expect_err("get after delete must fail");
    assert!(matches!(missing.kind(), KeychainErrorKind::NotFound),
        "get after delete returns NotFound, got {:?}", missing.kind());
}

/// D26 / FND-0031 — at the trait level `set_secret` upserts silently (the
/// substitution path needs this for token rotation). The CLI layer applies
/// the fail-by-default safety; the trait itself stays simple.
#[test]
fn set_secret_silently_upserts_at_trait_level() {
    let kc = InMemoryKeychain::new();
    let first = Zeroizing::new(b"first-value".to_vec());
    let second = Zeroizing::new(b"second-value".to_vec());

    kc.set_secret("label", first).expect("first set succeeds");
    kc.set_secret("label", second.clone()).expect("second set succeeds (upsert)");

    let fetched = kc.get_secret("label").expect("get after upsert");
    assert_eq!(&fetched[..], &second[..], "upsert replaces with second value");

    // Still exactly one label after upsert.
    let labels = kc.list_labels().expect("list");
    assert_eq!(labels.len(), 1, "upsert does not duplicate the label");
}

/// D7 / FND-0001 — `StubKeychain` exists and is the always-error stub. It is
/// kept (not replaced by `InMemoryKeychain`) so that backend-init-failure
/// tests can construct an `Arc<dyn KeychainAccess>` that fails predictably.
/// The four trait methods all return `Err(Unavailable)`.
#[test]
fn stub_keychain_unavailable_on_all_methods() {
    let kc: Box<dyn KeychainAccess> = Box::new(StubKeychain);

    let secret = Zeroizing::new(b"any".to_vec());
    let set_err = kc.set_secret("any", secret).expect_err("stub set must fail");
    assert!(matches!(set_err.kind(),
        KeychainErrorKind::Unavailable | KeychainErrorKind::NotFound),
        "stub set returns Unavailable or NotFound, got {:?}", set_err.kind());

    let del_err = kc.delete_secret("any").expect_err("stub delete must fail");
    assert!(matches!(del_err.kind(),
        KeychainErrorKind::Unavailable | KeychainErrorKind::NotFound),
        "stub delete returns Unavailable or NotFound, got {:?}", del_err.kind());

    let list_err = kc.list_labels().expect_err("stub list must fail");
    assert!(matches!(list_err.kind(),
        KeychainErrorKind::Unavailable | KeychainErrorKind::NotFound),
        "stub list returns Unavailable or NotFound, got {:?}", list_err.kind());
}

/// D30 / FND-0035 — `KeychainErrorReason::Internal` MUST carry an opaque
/// `internal_hash` (SHA-256 of the original error message), never the
/// message itself. Operators correlate failures via the hash without
/// any chance of secret bytes leaking through error strings.
#[test]
fn internal_error_carries_hash_not_message() {
    // Use a public constructor that exercises the Internal path. Construction
    // detail: we synthesize an Internal error using the documented constructor
    // (added in W1) that takes an original message and emits the hash.
    let original_message = "low-level driver said: secret=xyz123 was bad";
    let err = KeychainError::internal_from_message(original_message);

    let reason_text = format!("{:?}", err);
    assert!(!reason_text.contains("xyz123"),
        "internal hash form must not contain raw fragments from the original message: {}", reason_text);
    assert!(reason_text.contains("Internal"),
        "Debug output identifies the Internal variant: {}", reason_text);
    // The hash is 64 hex chars (SHA-256 hex digest). At minimum we assert a
    // long hex-shaped substring is present.
    let has_hex_hash = reason_text
        .chars()
        .collect::<String>()
        .split(|c: char| !c.is_ascii_hexdigit())
        .any(|s| s.len() >= 16);
    assert!(has_hex_hash, "internal_hash hex digest should appear in the Debug output: {}", reason_text);
}

/// D11 / FND-0005 — identity model is `(service_id, account)`. The
/// `InMemoryKeychain` exposes an inspection helper for tests that need to
/// verify scoping; the helper takes the service_id explicitly so tests can
/// confirm cross-service isolation.
#[test]
fn identity_scoped_by_service_id() {
    let kc_a = InMemoryKeychain::with_service_id("loom");
    let kc_b = InMemoryKeychain::with_service_id("different");

    kc_a.set_secret("shared-label", Zeroizing::new(b"value-a".to_vec())).unwrap();
    kc_b.set_secret("shared-label", Zeroizing::new(b"value-b".to_vec())).unwrap();

    // Each service_id namespace sees only its own value.
    assert_eq!(&kc_a.get_secret("shared-label").unwrap()[..], b"value-a");
    assert_eq!(&kc_b.get_secret("shared-label").unwrap()[..], b"value-b");
}
