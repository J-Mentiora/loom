//! End-to-end tests for the Linux Secret Service backend.
//!
//! These tests touch the **real** session-bus secret service (gnome-keyring
//! or equivalent). They use clearly identified labels under the `loom-test`
//! service so production loom data is never touched; operators can clean
//! up manually via `secret-tool clear service loom-test` if a test aborts.
//!
//! Gated with `#[ignore]` so the default `cargo test` stays hermetic.
//! Run on Linux only:
//!
//!     cargo test -p loom-keychain --test linux_keychain_e2e -- --ignored
//!
//! Each test self-skips with a `tracing::info!` log line if the secret-service
//! D-Bus name has no owner (no daemon running) — per W3.10 / A-W3.1, this
//! makes the tests safe to run in CI cells where gnome-keyring setup is
//! best-effort (per W7 / A-W7.2 `timeout 60s` + observable skip pattern).

#![cfg(target_os = "linux")]

use loom_keychain::{KeychainAccess, KeychainErrorKind, LinuxKeychain};
use zeroize::Zeroizing;

const TEST_SERVICE: &str = "loom-test";

fn fresh_label() -> String {
    let suffix: u32 = rand::random();
    format!("loom-test-{:08x}", suffix)
}

/// Attempt construction; on `Unavailable` (no secret-service daemon), log
/// and return None so the test self-skips. Per W3.10.
fn try_backend() -> Option<LinuxKeychain> {
    match LinuxKeychain::new(TEST_SERVICE, false) {
        Ok(kc) => Some(kc),
        Err(e) => {
            // CI gate per A-W7.2: if KEYCHAIN_CI_REQUIRE_DAEMON=1 the test
            // MUST fail loudly — a CI setup regression would otherwise mask
            // a real Linux backend regression as a silent skip.
            if std::env::var("KEYCHAIN_CI_REQUIRE_DAEMON").ok().as_deref() == Some("1") {
                panic!(
                    "KEYCHAIN_CI_REQUIRE_DAEMON=1 but LinuxKeychain::new failed: {e}. \
                     Check the gnome-keyring-daemon setup in this CI cell."
                );
            }
            tracing::info!(
                error = %e,
                "secret-service unreachable; skipping linux_keychain_e2e \
                 (configure gnome-keyring or KeePassXC, then re-run with --ignored)"
            );
            None
        }
    }
}

#[test]
#[ignore]
fn set_get_round_trip() {
    let Some(kc) = try_backend() else { return };
    let label = fresh_label();
    let secret = Zeroizing::new(b"round-trip-bytes-123".to_vec());

    kc.set_secret(&label, secret.clone())
        .expect("set should succeed");
    let fetched = kc.get_secret(&label).expect("get should succeed");
    assert_eq!(&fetched[..], &secret[..], "round-trip bytes match");
    kc.delete_secret(&label).expect("cleanup delete");
}

#[test]
#[ignore]
fn set_set_replaces() {
    let Some(kc) = try_backend() else { return };
    let label = fresh_label();
    let first = Zeroizing::new(b"first-value".to_vec());
    let second = Zeroizing::new(b"second-value".to_vec());

    kc.set_secret(&label, first).expect("first set");
    kc.set_secret(&label, second.clone())
        .expect("second set (replace=true)");

    let fetched = kc.get_secret(&label).expect("get after replace");
    assert_eq!(
        &fetched[..],
        &second[..],
        "replace semantics: latest write wins"
    );

    kc.delete_secret(&label).expect("cleanup delete");
}

#[test]
#[ignore]
fn get_not_found_returns_typed_error() {
    let Some(kc) = try_backend() else { return };
    let missing = fresh_label();
    let err = kc.get_secret(&missing).expect_err("get missing must fail");
    assert!(
        matches!(err.kind(), KeychainErrorKind::NotFound),
        "expected NotFound, got {:?}",
        err.kind()
    );
}

#[test]
#[ignore]
fn delete_not_found_is_idempotent() {
    let Some(kc) = try_backend() else { return };
    let missing = fresh_label();
    kc.delete_secret(&missing)
        .expect("delete of missing must be idempotent");
}

#[test]
#[ignore]
fn canary_byte_preservation() {
    // G1 invariant — raw bytes survive round-trip unmodified (no charset
    // mangling, no zero-byte truncation, no encoding). Mirrors the macOS
    // canary test so any future regression on either backend trips its CI.
    let Some(kc) = try_backend() else { return };
    let label = fresh_label();
    let canary: Vec<u8> = (0u8..=255).chain(std::iter::repeat_n(0, 16)).collect();
    let secret = Zeroizing::new(canary.clone());

    kc.set_secret(&label, secret).expect("set canary");
    let fetched = kc.get_secret(&label).expect("get canary");
    assert_eq!(&fetched[..], &canary[..], "byte-perfect round trip");
    kc.delete_secret(&label).expect("cleanup");
}

#[test]
#[ignore]
fn list_labels_returns_only_account_attribute() {
    // W3.8: list_labels must enumerate via attribute-only search; the
    // returned labels are the `account` attribute values. Confirms
    // `account` is what the impl reads (not `label` or the display string)
    // by writing two entries and asserting both `account` values appear.
    let Some(kc) = try_backend() else { return };
    let a = fresh_label();
    let b = fresh_label();
    kc.set_secret(&a, Zeroizing::new(b"aa".to_vec()))
        .expect("set a");
    kc.set_secret(&b, Zeroizing::new(b"bb".to_vec()))
        .expect("set b");

    let labels = kc.list_labels().expect("list_labels");
    assert!(
        labels.contains(&a),
        "expected {a:?} in list, got {labels:?}"
    );
    assert!(
        labels.contains(&b),
        "expected {b:?} in list, got {labels:?}"
    );

    kc.delete_secret(&a).expect("cleanup a");
    kc.delete_secret(&b).expect("cleanup b");
}

#[test]
#[ignore]
fn owner_pinned_at_construction() {
    // A-W3.1: constructor pins the unique owner of `org.freedesktop.secrets`.
    // Sanity check that `pinned_owner()` returns a non-empty unique name
    // (`:1.NN`-shaped) — we don't try to assert the exact value because it's
    // process-instance dependent.
    let Some(kc) = try_backend() else { return };
    let owner = kc.pinned_owner();
    assert!(
        owner.starts_with(':'),
        "pinned owner should be a unique-name (`:1.NN`), got {owner:?}"
    );
}
