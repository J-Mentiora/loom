//! Integration tests for `url-scheme-allowlist` feature.
//!
//! All cases test `check_url_scheme` directly — no RPC server needed.
//! For rejected URLs the check returns before `rpc.call`; for allowed URLs
//! the AC requires the scheme check to pass, not a full navigation.

use loom_cli::error_mapper::{map_exit_code, CliError, EXIT_RECEIPT_ERROR};
use loom_cli::url_allowlist::check_url_scheme;
use serde_json::Value;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn assert_rejected(url: &str, expected_scheme: &str) {
    let result = check_url_scheme(url);
    match result {
        Err(CliError::Receipt(ref v)) => {
            assert_eq!(
                v.get("status").and_then(|s| s.as_str()),
                Some("error"),
                "url={url}: expected status=error, got {:?}",
                v.get("status")
            );
            let err_obj = v
                .get("error")
                .unwrap_or_else(|| panic!("url={url}: receipt missing 'error' key"));
            assert_eq!(
                err_obj.get("kind").and_then(|k| k.as_str()),
                Some("unsafe_url_scheme"),
                "url={url}: expected error.kind=unsafe_url_scheme, got {:?}",
                err_obj.get("kind")
            );
            let detail = err_obj.get("detail").and_then(|d| d.as_str()).unwrap_or("");
            assert!(
                detail.contains(expected_scheme),
                "url={url}: detail should contain scheme '{expected_scheme}', got: {detail}"
            );
            assert!(
                detail.contains("is not in allowlist"),
                "url={url}: detail should contain 'is not in allowlist', got: {detail}"
            );
            // Verify exit code contract
            assert_eq!(
                map_exit_code(&Err(CliError::Receipt(v.clone()))),
                EXIT_RECEIPT_ERROR,
                "url={url}: map_exit_code should return EXIT_RECEIPT_ERROR (1)"
            );
        }
        Ok(()) => panic!("url={url}: expected rejection, got Ok(())"),
        Err(e) => panic!("url={url}: expected CliError::Receipt, got {:?}", e),
    }
}

fn assert_allowed(url: &str) {
    let result = check_url_scheme(url);
    assert!(
        result.is_ok(),
        "url={url}: expected Ok(()), got {:?}",
        result
    );
}

// ── file:// rejected ───────────────────────────────────────────

#[test]
fn test_file_url_rejected() {
    assert_rejected("file:///etc/hosts", "file");
}

// ── javascript: rejected ───────────────────────────────────────

#[test]
fn test_javascript_url_rejected() {
    assert_rejected("javascript:alert(1)", "javascript");
}

// ── data: rejected ─────────────────────────────────────────────

#[test]
fn test_data_url_rejected() {
    assert_rejected("data:text/html,<h1>hi</h1>", "data");
}

// ── chrome: rejected ───────────────────────────────────────────

#[test]
fn test_chrome_url_rejected() {
    assert_rejected("chrome://settings", "chrome");
}

// ── https:// allowed ───────────────────────────────────────────

#[test]
fn test_https_url_allowed() {
    assert_allowed("https://example.com");
}

// ── about:blank allowed ────────────────────────────────────────

#[test]
fn test_about_blank_allowed() {
    assert_allowed("about:blank");
}

// ── Unit coverage for check_url_scheme ─────────────────────────

#[test]
fn test_check_url_scheme_http_allowed() {
    assert_allowed("http://example.com");
}

#[test]
fn test_check_url_scheme_case_insensitive_file_rejected() {
    // Uppercase scheme must still be rejected (RFC 3986 §3.1 scheme is case-insensitive)
    assert_rejected("FILE:///etc/hosts", "file");
}

#[test]
fn test_check_url_scheme_case_insensitive_https_allowed() {
    assert_allowed("HTTPS://example.com");
}

#[test]
fn test_check_url_scheme_no_scheme_rejected() {
    // URL with no ':' at all → no scheme → rejected
    let result = check_url_scheme("example.com/path");
    assert!(
        matches!(result, Err(CliError::Receipt(_))),
        "expected rejection for URL with no scheme, got {:?}",
        result
    );
}

#[test]
fn test_scheme_relative_url_rejected() {
    // scheme-relative URL (//evil.com) has no ':' → no scheme → rejected
    let result = check_url_scheme("//evil.com/path");
    assert!(
        matches!(result, Err(CliError::Receipt(_))),
        "expected rejection for scheme-relative URL, got {:?}",
        result
    );
}

#[test]
fn test_check_url_scheme_ftp_rejected() {
    assert_rejected("ftp://files.example.com", "ftp");
}

#[test]
fn test_check_url_scheme_about_newtab_allowed() {
    // about: scheme broadly allowed (not just about:blank)
    assert_allowed("about:newtab");
}

#[test]
fn test_error_receipt_json_structure() {
    // Verify the full JSON structure matches the expected exact shape
    let result = check_url_scheme("file:///etc/hosts");
    if let Err(CliError::Receipt(v)) = result {
        assert_eq!(v["status"], Value::String("error".to_string()));
        assert_eq!(
            v["error"]["kind"],
            Value::String("unsafe_url_scheme".to_string())
        );
        let detail = v["error"]["detail"].as_str().unwrap_or("");
        assert!(
            detail.contains("file"),
            "detail should contain scheme 'file'"
        );
        assert!(
            detail.contains("[http, https, about:blank]"),
            "detail should contain allowlist"
        );
    } else {
        panic!("expected Err(CliError::Receipt(_)), got {:?}", result);
    }
}
