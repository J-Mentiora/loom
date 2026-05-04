// URL scheme allowlist for web.navigate (AC-URLSEC-01 through AC-URLSEC-06).
//
// Checks the URL scheme before the RPC call, preventing file://, javascript:,
// data:, and chrome: URLs from reaching the Chromium shim.

use crate::CliError;

/// Allowlisted URL schemes. Case-insensitive (callers lowercase before lookup).
const ALLOWED_SCHEMES: &[&str] = &["http", "https", "about"];

/// Display form of the allowlist in error messages.
/// Uses "about:blank" (URL-level) as the canonical user-facing form of the
/// "about" scheme, which is the most common safe about: URL.
const ALLOWLIST_DISPLAY: &str = "[http, https, about:blank]";

/// Extract the URL scheme — everything before the first ':'.
/// Returns `None` if there is no ':' or the scheme is empty.
fn extract_scheme(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    if scheme.is_empty() { None } else { Some(scheme) }
}

/// Check that `url`'s scheme is in the allowlist.
///
/// Returns `Ok(())` if the scheme is allowed.
/// Returns `Err(CliError::Receipt(...))` if the scheme is absent or not in the
/// allowlist. The error receipt JSON has the shape:
///
/// ```json
/// {
///   "status": "error",
///   "error": {
///     "kind": "unsafe_url_scheme",
///     "detail": "<scheme> is not in allowlist [http, https, about:blank]"
///   }
/// }
/// ```
///
/// Does NOT print — the caller (dispatch) owns stdout output.
pub fn check_url_scheme(url: &str) -> Result<(), CliError> {
    let detail = match extract_scheme(url) {
        None => format!("(no scheme) is not in allowlist {ALLOWLIST_DISPLAY}"),
        Some(raw) => {
            let scheme = raw.to_lowercase();
            if ALLOWED_SCHEMES.contains(&scheme.as_str()) {
                return Ok(());
            }
            format!("{scheme} is not in allowlist {ALLOWLIST_DISPLAY}")
        }
    };

    Err(CliError::Receipt(serde_json::json!({
        "status": "error",
        "error": {
            "kind": "unsafe_url_scheme",
            "detail": detail
        }
    })))
}
