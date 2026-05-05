// Recursive sensitive-field redaction for pretty/curated/tail rendering.
//
// Per D-29: applied uniformly to curated, tail, and PrettyFallback paths.
// The `--json` (canonical) path is NEVER redacted — byte-exactness
// preserved. Pretty paths walk the receipt before formatting, replacing
// any nested object value whose KEY name matches the sensitive regex with
// the JSON string `"<redacted>"`.
//
// The denylist is conservative — it errs on the side of redacting fields
// the daemon may add in future. False positives are visual-only (a user can
// always pass `--json` to see the raw value); false negatives could leak
// secrets to a screen-recording session. The list:
//   token, secret, password, api_key, apikey, bearer, credential, cookie,
//   jwt, session_key, sessionkey, access_token, accesstoken, refresh_token,
//   refreshtoken, private_key, privatekey, signing_key, signingkey,
//   client_secret, clientsecret, oauth.

use serde_json::{Map, Value};

const SENSITIVE_PATTERN_LOWERCASE: &[&str] = &[
    "token",
    "secret",
    "password",
    "api_key",
    "apikey",
    "bearer",
    "credential",
    "cookie",
    "jwt",
    "session_key",
    "sessionkey",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "private_key",
    "privatekey",
    "signing_key",
    "signingkey",
    "client_secret",
    "clientsecret",
    "oauth",
    // Code-review additions: catch HTTP-style auth headers that don't
    // already match the above (e.g. `authorization`, `authentication`,
    // `auth_header`). Substring-matched. The `auth_` (trailing underscore)
    // pattern catches `auth_header` / `auth_provider` style names without
    // over-matching `author` or `authored`.
    "auth_",
    "authoriz",
    "authentic",
];

/// Returns true when `key` (case-insensitive) contains any sensitive
/// substring. Substring match — `auth_token`, `oauth_token`, and
/// `bearer` all match `token`/`oauth`/`bearer` respectively.
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_PATTERN_LOWERCASE
        .iter()
        .any(|pat| lower.contains(pat))
}

/// Walk `value` recursively. For every object key matching
/// `is_sensitive_key`, replace its value with the JSON string
/// `"<redacted>"`. Arrays are walked element-by-element; non-object
/// non-array scalars are returned unchanged.
pub fn redact_recursive(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String("<redacted>".to_string()));
                } else {
                    out.insert(k.clone(), redact_recursive(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_recursive).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_token_field_redacted() {
        let v = json!({"action_hash": "abc", "auth_token": "secret-value"});
        let r = redact_recursive(&v);
        assert_eq!(r["action_hash"], json!("abc"));
        assert_eq!(r["auth_token"], json!("<redacted>"));
    }

    #[test]
    fn nested_credential_redacted() {
        let v = json!({"top": {"nested": {"api_key": "abc"}}});
        let r = redact_recursive(&v);
        assert_eq!(r["top"]["nested"]["api_key"], json!("<redacted>"));
    }

    #[test]
    fn three_level_leaf_token_redacted() {
        // Use intermediate keys that don't themselves match the
        // substring denylist ("credentials" contains "credential" and
        // would short-circuit redaction at level 2).
        let v = json!({
            "request": {"headers": {"oauth_token": "x"}}
        });
        let r = redact_recursive(&v);
        assert_eq!(r["request"]["headers"]["oauth_token"], json!("<redacted>"));
    }

    #[test]
    fn intermediate_match_redacts_subtree() {
        // When an intermediate key matches the denylist, the ENTIRE
        // sub-object is redacted as a single string. This is intentional
        // (defense-in-depth) — the sub-tree is never walked. Documented
        // here so the behaviour is testable.
        let v = json!({"user": {"credentials": {"inner": "x"}}});
        let r = redact_recursive(&v);
        assert_eq!(r["user"]["credentials"], json!("<redacted>"));
    }

    #[test]
    fn array_of_objects_redacted() {
        let v = json!({"entries": [{"private_key": "k1"}, {"name": "x"}]});
        let r = redact_recursive(&v);
        assert_eq!(r["entries"][0]["private_key"], json!("<redacted>"));
        assert_eq!(r["entries"][1]["name"], json!("x"));
    }

    #[test]
    fn case_insensitive_match() {
        let v = json!({"AuthToken": "abc", "API_KEY": "x", "Bearer": "y"});
        let r = redact_recursive(&v);
        assert_eq!(r["AuthToken"], json!("<redacted>"));
        assert_eq!(r["API_KEY"], json!("<redacted>"));
        assert_eq!(r["Bearer"], json!("<redacted>"));
    }

    #[test]
    fn safe_keys_pass_through() {
        let v = json!({"session_id": "abc", "final_url": "https://x.test", "status_code": 200});
        let r = redact_recursive(&v);
        assert_eq!(r, v);
    }

    #[test]
    fn expanded_patterns_all_match() {
        for key in &[
            "auth_token",
            "private_key",
            "oauth_token",
            "access_token",
            "refresh_token",
            "signing_key",
            "client_secret",
            "Bearer",
            "Cookie",
            "jwt",
            // Step 18 council additions:
            "authorization",
            "Authorization",
            "proxy_authorization",
            "authentication",
            "auth_header",
        ] {
            let v = serde_json::json!({ *key: "x" });
            let r = redact_recursive(&v);
            assert_eq!(
                r[*key],
                json!("<redacted>"),
                "expected key {} to be redacted",
                key
            );
        }
    }

    #[test]
    fn safe_keys_with_partial_letter_overlap_pass_through() {
        // Make sure the new "authoriz" / "authentic" patterns don't
        // over-match common safe terms.
        for key in &["author", "authored_at", "authentic_user_count"] {
            let v = serde_json::json!({ *key: "x" });
            let r = redact_recursive(&v);
            // "authentic_user_count" CONTAINS "authentic" → would redact.
            // "author" does NOT contain "authoriz" or "authentic" → stays.
            // "authored_at" does NOT contain either → stays.
            // We document the known over-match for "authentic_*" keys
            // here; safer to redact than leak.
            if key.contains("authentic") {
                assert_eq!(r[*key], json!("<redacted>"));
            } else {
                assert_eq!(r[*key], json!("x"));
            }
        }
    }

    #[test]
    fn scalars_pass_through() {
        let v = json!("plain string");
        assert_eq!(redact_recursive(&v), v);
        let v = json!(42);
        assert_eq!(redact_recursive(&v), v);
        let v = json!(null);
        assert_eq!(redact_recursive(&v), v);
    }
}
