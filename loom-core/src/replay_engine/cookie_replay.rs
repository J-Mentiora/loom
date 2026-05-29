//! v0.9.6 web-cookie-injection replay value substitution + typed errors.
//!
//! # Contract semantics
//! - **Tuple identity (D13).** Replay value lookup is keyed by
//!   `(name, domain, path)` per RFC 6265 §5.3 cookie identity. The
//!   same tuple keying is used by the ReceiptMarshaller's D13 sort
//!   in `loom-host` so canonical bytes remain byte-identical when the
//!   substituted values are correct.
//! - **Hard fail on missing value.** A cookie tuple present in the
//!   recorded receipt but missing from `replay_cookie_values` triggers
//!   `ReplayError::MissingCookieValue { action_id, name, domain, path }`
//!   — replay does NOT silently substitute an empty value (that would
//!   change canonical bytes and break replay byte-identity guarantees).
//! - **No-op on receipts without cookies.** Receipts whose cookie
//!   fields are all None pass through unchanged.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// v0.9.6 typed replay errors for cookie-bearing actions.
///
/// Separate from the generic `LoomError` chain so callers can
/// pattern-match the specific cookie-replay failure modes (a
/// `ReplayError::MissingCookieValue` is a *value-supply* problem the
/// operator can fix by extending `replay_cookie_values`; a generic
/// `Internal` is a bug in replay machinery).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReplayError {
    /// A cookie identified by `(name, domain, path)` was present in the
    /// recorded receipt but the replay input did not supply a value for
    /// the same tuple. Caller should extend `replay_cookie_values` or
    /// re-record.
    #[error(
        "missing replay cookie value for action {action_id}: (name={name:?}, domain={domain:?}, path={path:?})"
    )]
    MissingCookieValue {
        action_id: u64,
        name: String,
        domain: String,
        path: String,
    },
    /// The replay input shape was malformed (e.g. a non-array
    /// `get_cookies_result` payload, an unparseable JSON cookies field).
    #[error("invalid cookie replay payload at action {action_id}: {reason}")]
    InvalidPayload { action_id: u64, reason: String },
}

/// Replay-input value map. Keyed by `(name, domain, path)` per D13
/// (the same tuple used for the ReceiptMarshaller's canonical-bytes
/// sort). Default for unsupplied cookies is to error rather than
/// silently substitute — see `MissingCookieValue`.
pub type ReplayCookieValues = BTreeMap<(String, String, String), String>;

/// Substitute cookie values in a recorded receipt's `get_cookies_result`
/// JSON payload by replaying from `values`. For each cookie in the
/// payload, look up its `(name, domain, path)` tuple in `values` and
/// substitute the value; missing → `MissingCookieValue`.
///
/// `set_cookies_result` is left untouched: SetCookieResult carries only
/// `(name, success, error_code)` — no value field. Same for
/// `clear_cookies_result` and `delete_cookies_result`.
///
/// Returns the modified JSON string ready to drop back into
/// `ReceiptBuilder.get_cookies_result`.
pub fn substitute_cookie_values(
    action_id: u64,
    raw: &str,
    values: &ReplayCookieValues,
) -> Result<String, ReplayError> {
    let mut v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| ReplayError::InvalidPayload {
            action_id,
            reason: format!("parse: {e}"),
        })?;
    let Some(arr) = v.as_array_mut() else {
        return Err(ReplayError::InvalidPayload {
            action_id,
            reason: "get_cookies_result must be a JSON array".to_string(),
        });
    };
    for item in arr.iter_mut() {
        let Some(obj) = item.as_object_mut() else {
            return Err(ReplayError::InvalidPayload {
                action_id,
                reason: "cookie entry must be a JSON object".to_string(),
            });
        };
        let name = obj
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let domain = obj
            .get("domain")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let path = obj
            .get("path")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let key = (name.clone(), domain.clone(), path.clone());
        let new_value = values
            .get(&key)
            .cloned()
            .ok_or(ReplayError::MissingCookieValue {
                action_id,
                name,
                domain,
                path,
            })?;
        obj.insert("value".to_string(), serde_json::Value::String(new_value));
    }
    serde_json::to_string(&v).map_err(|e| ReplayError::InvalidPayload {
        action_id,
        reason: format!("re-serialise: {e}"),
    })
}

/// Parse a JSON `(name, domain, path)` tuple-key map from the replay
/// input file format. Supports two encodings:
///   1. JSON object with `"name|domain|path"` string keys (most ergonomic
///      for hand-authored fixtures).
///   2. JSON array of `[name, domain, path, value]` tuples (also
///      accepted for tools that emit array shape).
pub fn parse_replay_cookie_values(json_text: &str) -> Result<ReplayCookieValues, ReplayError> {
    let v: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| ReplayError::InvalidPayload {
            action_id: 0,
            reason: format!("replay_cookie_values parse: {e}"),
        })?;
    let mut out: ReplayCookieValues = BTreeMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let parts: Vec<&str> = k.splitn(3, '|').collect();
            if parts.len() != 3 {
                return Err(ReplayError::InvalidPayload {
                    action_id: 0,
                    reason: format!("replay_cookie_values key not 'name|domain|path': {k:?}"),
                });
            }
            let value = val
                .as_str()
                .ok_or_else(|| ReplayError::InvalidPayload {
                    action_id: 0,
                    reason: format!("replay_cookie_values value at {k:?} not a string"),
                })?
                .to_string();
            out.insert(
                (
                    parts[0].to_string(),
                    parts[1].to_string(),
                    parts[2].to_string(),
                ),
                value,
            );
        }
    } else if let Some(arr) = v.as_array() {
        for tup in arr {
            let tarr = tup.as_array().ok_or_else(|| ReplayError::InvalidPayload {
                action_id: 0,
                reason: "tuple form must be [name, domain, path, value]".to_string(),
            })?;
            if tarr.len() != 4 {
                return Err(ReplayError::InvalidPayload {
                    action_id: 0,
                    reason: "tuple form must have exactly 4 elements".to_string(),
                });
            }
            let s = |i: usize| {
                tarr[i]
                    .as_str()
                    .ok_or_else(|| ReplayError::InvalidPayload {
                        action_id: 0,
                        reason: format!("tuple element {i} not a string"),
                    })
                    .map(String::from)
            };
            out.insert((s(0)?, s(1)?, s(2)?), s(3)?);
        }
    } else {
        return Err(ReplayError::InvalidPayload {
            action_id: 0,
            reason: "replay_cookie_values must be JSON object or array".to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_map(entries: &[(&str, &str, &str, &str)]) -> ReplayCookieValues {
        entries
            .iter()
            .map(|(n, d, p, v)| ((n.to_string(), d.to_string(), p.to_string()), v.to_string()))
            .collect()
    }

    #[test]
    fn substitute_swaps_value_for_matching_tuple_key() {
        let recorded = r#"[{"name":"sid","domain":"example.com","path":"/","value":"OLD"}]"#;
        let values = val_map(&[("sid", "example.com", "/", "NEW")]);
        let out = substitute_cookie_values(7, recorded, &values).expect("ok");
        assert!(out.contains("\"value\":\"NEW\""));
        assert!(!out.contains("OLD"));
    }

    #[test]
    fn substitute_missing_tuple_returns_typed_error_with_full_identity() {
        let recorded = r#"[{"name":"sid","domain":"example.com","path":"/api","value":"OLD"}]"#;
        let values = val_map(&[("sid", "example.com", "/", "NEW")]); // path mismatch
        let err = substitute_cookie_values(42, recorded, &values).expect_err("must error");
        match err {
            ReplayError::MissingCookieValue {
                action_id,
                name,
                domain,
                path,
            } => {
                assert_eq!(action_id, 42);
                assert_eq!(name, "sid");
                assert_eq!(domain, "example.com");
                assert_eq!(path, "/api");
            }
            other => panic!("expected MissingCookieValue, got: {other:?}"),
        }
    }

    #[test]
    fn substitute_handles_multiple_cookies_with_distinct_tuples() {
        let recorded = r#"[
            {"name":"sid","domain":"example.com","path":"/","value":"OLD1"},
            {"name":"sid","domain":"api.example.com","path":"/","value":"OLD2"}
        ]"#;
        let values = val_map(&[
            ("sid", "example.com", "/", "NEW1"),
            ("sid", "api.example.com", "/", "NEW2"),
        ]);
        let out = substitute_cookie_values(1, recorded, &values).expect("ok");
        assert!(out.contains("NEW1"));
        assert!(out.contains("NEW2"));
        assert!(!out.contains("OLD1"));
        assert!(!out.contains("OLD2"));
    }

    #[test]
    fn substitute_uses_empty_string_for_default_path_and_domain_when_unset() {
        // RFC 6265 cookies with no domain/path use empty defaults in
        // the tuple key (matches D13 unwrap_or_default).
        let recorded = r#"[{"name":"sid","value":"OLD"}]"#;
        let values = val_map(&[("sid", "", "", "NEW")]);
        let out = substitute_cookie_values(1, recorded, &values).expect("ok");
        assert!(out.contains("NEW"));
    }

    #[test]
    fn parse_pipe_keyed_map_round_trips() {
        let json = r#"{"sid|example.com|/": "v1", "uid|api.example.com|/api": "v2"}"#;
        let map = parse_replay_cookie_values(json).expect("ok");
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&("sid".into(), "example.com".into(), "/".into())),
            Some(&"v1".to_string())
        );
        assert_eq!(
            map.get(&("uid".into(), "api.example.com".into(), "/api".into())),
            Some(&"v2".to_string())
        );
    }

    #[test]
    fn parse_tuple_array_form_also_round_trips() {
        let json = r#"[["sid","example.com","/","v1"],["uid","x.com","/api","v2"]]"#;
        let map = parse_replay_cookie_values(json).expect("ok");
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&("sid".into(), "example.com".into(), "/".into())),
            Some(&"v1".to_string())
        );
    }

    #[test]
    fn parse_malformed_key_returns_typed_invalid_payload() {
        let json = r#"{"sid_only_no_pipes": "v"}"#;
        let err = parse_replay_cookie_values(json).expect_err("must error");
        match err {
            ReplayError::InvalidPayload { reason, .. } => {
                assert!(reason.contains("name|domain|path"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_non_object_non_array_returns_typed_invalid_payload() {
        let err = parse_replay_cookie_values("\"just a string\"").expect_err("must error");
        match err {
            ReplayError::InvalidPayload { reason, .. } => {
                assert!(reason.contains("object or array"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn substitute_no_cookies_in_payload_returns_empty_array() {
        let recorded = "[]";
        let values = val_map(&[]);
        let out = substitute_cookie_values(1, recorded, &values).expect("ok");
        assert_eq!(out, "[]");
    }
}
