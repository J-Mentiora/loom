//! End-to-end tests for the macOS Security Framework backend.
//!
//! These tests touch the **real** user login keychain — they use clearly
//! identified labels under the `loom-test` service so collisions with
//! production loom data are impossible and operators can clean up
//! manually via `security find-generic-password -s loom-test` if a test
//! aborts.
//!
//! Gated with `#[ignore]` to keep the default `cargo test` hermetic.
//! Run on macOS only:
//!
//!     cargo test -p loom-keychain --test macos_keychain_e2e -- --ignored
//!
//! CI runs them on the `macos-latest` cell in the workspace's existing
//! `--include-ignored` test step.

#![cfg(target_os = "macos")]

use loom_keychain::{KeychainAccess, KeychainErrorKind, MacOsKeychain};
use zeroize::Zeroizing;

const TEST_SERVICE: &str = "loom-test";

fn fresh_label() -> String {
    let suffix: u32 = rand::random();
    format!("loom-test-{:08x}", suffix)
}

fn fresh_backend() -> MacOsKeychain {
    MacOsKeychain::new(TEST_SERVICE, false).expect("MacOsKeychain::new")
}

#[test]
#[ignore]
fn set_get_round_trip() {
    let kc = fresh_backend();
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
    let kc = fresh_backend();
    let label = fresh_label();
    let first = Zeroizing::new(b"first-value".to_vec());
    let second = Zeroizing::new(b"second-value".to_vec());

    kc.set_secret(&label, first).expect("first set");
    kc.set_secret(&label, second.clone())
        .expect("second set (replace)");

    let fetched = kc.get_secret(&label).expect("get after replace");
    assert_eq!(&fetched[..], &second[..]);

    kc.delete_secret(&label).expect("cleanup delete");
}

#[test]
#[ignore]
fn get_not_found_returns_typed_error() {
    let kc = fresh_backend();
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
    let kc = fresh_backend();
    let missing = fresh_label();
    kc.delete_secret(&missing)
        .expect("delete of missing must be idempotent");
}

#[test]
#[ignore]
fn canary_byte_preservation() {
    // G1 invariant — raw bytes survive round-trip unmodified (no charset
    // mangling, no zero-byte truncation, no encoding). This test catches
    // any future regression that adds an unintended decode step.
    let kc = fresh_backend();
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
fn list_labels_returns_unavailable_v094_limitation() {
    // Documents the v0.9.4 macOS limitation: list_labels returns
    // Unavailable per module-level doc; flip this test when the
    // fast-follow-up wires the lower-level enumeration API.
    let kc = fresh_backend();
    let err = kc
        .list_labels()
        .expect_err("v0.9.4 macOS list_labels returns Unavailable");
    assert!(
        matches!(err.kind(), KeychainErrorKind::Unavailable),
        "expected Unavailable, got {:?}",
        err.kind()
    );
}
