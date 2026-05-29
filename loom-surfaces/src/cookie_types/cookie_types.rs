//! `NetworkCookieParam` (set input) + `NetworkCookie` (get output) +
//! enums + `CookieSource` XOR + `CookieValidationError` + the pure
//! `validate_cookie_params` function.
//!
//! Cookie value fields are typed `Redacted<String>` per D4. Note: D12's
//! original `Redacted<Zeroizing<String>>` shape doesn't fit serde because
//! `Zeroizing<T>` from the `zeroize` crate doesn't impl `Deserialize`.
//! `String::zeroize()` from zeroize 1.6+ calls `Vec::clear()` which sets
//! length to 0 but doesn't write zeros to the heap buffer — best-effort
//! heap wipe only. Documented as a caveat in security/vault_threat_model.md;
//! proper heap wipe (via `Zeroizing<Vec<u8>>` at the boundary, or a manual
//! `as_mut_vec().fill(0)` step) is a follow-up.

use loom_shared::Redacted;
use serde::{Deserialize, Serialize};

/// CDP `Network.CookieParam` — the input shape for `Network.setCookies`.
///
/// 13 fields per the May 2026 protocol spec. The `value` field is wrapped
/// in `Redacted<Zeroizing<String>>` so output paths emit `"[REDACTED]"` and
/// the heap buffer is wiped on drop. Domain/path/url defaults flow from the
/// CDP semantics; we don't second-guess.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCookieParam {
    pub name: String,
    pub value: Redacted<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<CookieSameSite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<CookiePriority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_scheme: Option<CookieSourceScheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_port: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<serde_json::Value>,
}

impl std::fmt::Debug for NetworkCookieParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkCookieParam")
            .field("name", &self.name)
            .field("value", &self.value)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// CDP `Network.Cookie` — the output shape for `Network.getCookies`.
///
/// 15 fields per the May 2026 protocol spec. Distinct from `NetworkCookieParam`
/// — no `url`, but has `size`, `session`, `partition_key_opaque`. Council
/// FND-0002 (intake round 1) called this asymmetry out; we model both.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCookie {
    pub name: String,
    pub value: Redacted<String>,
    pub domain: String,
    pub path: String,
    pub expires: f64,
    pub size: i32,
    pub http_only: bool,
    pub secure: bool,
    pub session: bool,
    #[serde(default)]
    pub same_site: Option<CookieSameSite>,
    #[serde(default)]
    pub priority: Option<CookiePriority>,
    #[serde(default)]
    pub source_scheme: Option<CookieSourceScheme>,
    #[serde(default)]
    pub source_port: Option<i32>,
    #[serde(default)]
    pub partition_key: Option<serde_json::Value>,
    #[serde(default)]
    pub partition_key_opaque: Option<bool>,
}

impl std::fmt::Debug for NetworkCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkCookie")
            .field("name", &self.name)
            .field("value", &self.value)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum CookieSameSite {
    Strict,
    Lax,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum CookiePriority {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum CookieSourceScheme {
    Unset,
    NonSecure,
    Secure,
}

/// Type-safe XOR for `set_cookies` input (D9 / council FND-0042).
///
/// Replaces `Option<Vec<NetworkCookieParam>> + Option<GrantId>` runtime XOR
/// with a tagged enum — invalid states aren't representable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CookieSource {
    Inline { cookies: Vec<NetworkCookieParam> },
    Grant { grant_id: String },
}

/// Typed validation errors for the synchronous per-cookie pass (D9 / council
/// FND-0036). Validation runs ONLY on the input path (`set_cookies`);
/// `get/clear/delete` skip it. CDP `Network.setCookies` returns nothing
/// synchronously, so this is the only place we can report per-cookie
/// rejection before the CDP call fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CookieValidationError {
    #[error("cookie name is empty")]
    NameEmpty,
    #[error("cookie name contains invalid character: {ch:?}")]
    NameInvalid { ch: char },
    #[error("cookie value too large: {size} bytes (max 4096)")]
    ValueTooLarge { size: usize },
    #[error("invalid sameSite value: {0:?}")]
    InvalidSameSite(String),
    #[error(
        "invalid expires value: {0} (must be -1 for session cookie or >=1.0 seconds since epoch)"
    )]
    InvalidExpires(f64),
    #[error("too many cookies in set_cookies call: {0} (max 64)")]
    TooManyCookies(usize),
}

/// Validate the input cookie array. Council FND-0036/0044: enforce a 64-cookie
/// cap (DoS guard); per-cookie name/value/expires checks; reject early so
/// the CDP envelope never sees bad input.
///
/// The set is atomic: if any cookie rejects, the whole batch rejects.
pub fn validate_cookie_params(cookies: &[NetworkCookieParam]) -> Result<(), CookieValidationError> {
    if cookies.len() > 64 {
        return Err(CookieValidationError::TooManyCookies(cookies.len()));
    }
    for c in cookies {
        if c.name.is_empty() {
            return Err(CookieValidationError::NameEmpty);
        }
        // RFC 6265 token-char restriction — reject the common offenders.
        for ch in c.name.chars() {
            if matches!(ch, '=' | ';' | ',' | ' ' | '\t' | '"') {
                return Err(CookieValidationError::NameInvalid { ch });
            }
        }
        let value_len = c.value.expose().len();
        if value_len > 4096 {
            return Err(CookieValidationError::ValueTooLarge { size: value_len });
        }
        if let Some(e) = c.expires {
            // -1 = session cookie; any positive value is seconds-since-epoch.
            // Reject the gap (-1, 1.0) which usually indicates a bug (e.g. a
            // millisecond value or pre-1970 timestamp).
            if !(e == -1.0 || e >= 1.0) {
                return Err(CookieValidationError::InvalidExpires(e));
            }
        }
    }
    Ok(())
}

/// Per-cookie result reported on the receipt for `set_cookies` (D9 /
/// PROMPT FND-0023). For v0.9.6 every entry that survives validation has
/// `success: true`; on validation failure we short-circuit before populating
/// the result vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetCookieResult {
    pub name: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Per-receipt aggregate result for `clear_cookies` (v0.9.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearCookiesResult {
    pub cleared_count: u32,
}

/// Per-receipt aggregate result for `delete_cookies` (v0.9.6). `matched`
/// is determined by a `getCookies` peek before and after the CDP call;
/// `true` iff a cookie with the given `(name, domain, path)` triple was
/// present before and is absent after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteCookiesResult {
    pub name: String,
    pub matched: bool,
}

/// Schema for the vault-stored cookie blob produced by `loom vault add
/// --credential-type cookie` and consumed by `Vault::substitute_cookies`
/// → `host::vault_substitute_cookies`. v0.9.6 schema_version = 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieKeychainBlob {
    pub schema_version: u32,
    pub cookies: Vec<NetworkCookieParam>,
}
