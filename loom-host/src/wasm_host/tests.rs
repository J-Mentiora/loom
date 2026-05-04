// TDD tests for AC-NFR-COMPAT-01.1 — platform version gate.

use super::check_platform_version;
use loom_shared::error_format::LoomErrorCode;

// AC-NFR-COMPAT-01.1 — macOS < 14 must return Unsupported("platform_unsupported").
#[test]
fn platform_check_returns_unsupported_on_old_macos() {
    let result = check_platform_version(Some("13.0"));
    assert!(result.is_err(), "expected Err for macOS 13.0 but got Ok");
    let err = result.unwrap_err();
    assert_eq!(err.code, LoomErrorCode::Unsupported);
    assert!(
        err.message.contains("platform_unsupported"),
        "expected message to contain 'platform_unsupported', got: {}",
        err.message
    );
}

// AC-NFR-COMPAT-01.1 — macOS 14.0 must succeed.
#[test]
fn platform_check_ok_on_macos_14_0() {
    let result = check_platform_version(Some("14.0"));
    assert!(result.is_ok(), "expected Ok for macOS 14.0, got: {:?}", result.err());
}

// AC-NFR-COMPAT-01.1 — macOS 14.5 (patch release) must also succeed.
#[test]
fn platform_check_ok_on_macos_14_5() {
    let result = check_platform_version(Some("14.5"));
    assert!(result.is_ok(), "expected Ok for macOS 14.5, got: {:?}", result.err());
}

// AC-NFR-COMPAT-01.1 — macOS 15.x (future) must succeed.
#[test]
fn platform_check_ok_on_macos_15() {
    let result = check_platform_version(Some("15.0"));
    assert!(result.is_ok(), "expected Ok for macOS 15.0, got: {:?}", result.err());
}
