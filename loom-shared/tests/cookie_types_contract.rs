//! Cookie-types contract tests — assert `cookie_types` is reachable at its new
//! `loom-shared` home and that `validate_cookie_params` enforces its limits.
//! This is the relocation contract for the daemon's cookie-validation path.

use loom_shared::cookie_types::{
    validate_cookie_params, CookieValidationError, NetworkCookieParam,
};
use loom_shared::Redacted;

fn param(name: &str, value: &str) -> NetworkCookieParam {
    NetworkCookieParam {
        name: name.to_string(),
        value: Redacted::new(value.to_string()),
        url: None,
        domain: None,
        path: None,
        secure: None,
        http_only: None,
        same_site: None,
        expires: None,
        priority: None,
        source_scheme: None,
        source_port: None,
        partition_key: None,
    }
}

#[test]
fn valid_cookies_pass() {
    let cookies = vec![param("session", "abc123"), param("theme", "dark")];
    assert_eq!(validate_cookie_params(&cookies), Ok(()));
}

#[test]
fn empty_name_rejected() {
    let cookies = vec![param("", "x")];
    assert_eq!(
        validate_cookie_params(&cookies),
        Err(CookieValidationError::NameEmpty)
    );
}

#[test]
fn value_too_large_rejected() {
    let big = "a".repeat(4097);
    let cookies = vec![param("k", &big)];
    assert_eq!(
        validate_cookie_params(&cookies),
        Err(CookieValidationError::ValueTooLarge { size: 4097 })
    );
}

#[test]
fn too_many_cookies_rejected() {
    let cookies: Vec<NetworkCookieParam> = (0..65).map(|i| param(&format!("c{i}"), "v")).collect();
    assert_eq!(
        validate_cookie_params(&cookies),
        Err(CookieValidationError::TooManyCookies(65))
    );
}
