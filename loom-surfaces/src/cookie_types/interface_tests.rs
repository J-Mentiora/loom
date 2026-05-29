use super::cookie_types::*;
use loom_shared::Redacted;
use serde_json::json;

fn cookie_param(name: &str, value: &str) -> NetworkCookieParam {
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
fn cookie_param_serializes_value_as_redacted() {
    let c = cookie_param("sid", "abc123");
    let j = serde_json::to_value(&c).expect("serialize");
    assert_eq!(j["name"], "sid");
    assert_eq!(j["value"], "[REDACTED]");
}

#[test]
fn cookie_param_camelcase_fields() {
    let mut c = cookie_param("sid", "abc123");
    c.http_only = Some(true);
    c.same_site = Some(CookieSameSite::Strict);
    c.source_port = Some(443);
    let j = serde_json::to_value(&c).expect("serialize");
    assert!(j.get("httpOnly").is_some(), "expected httpOnly camelCase");
    assert!(j.get("sameSite").is_some(), "expected sameSite camelCase");
    assert!(
        j.get("sourcePort").is_some(),
        "expected sourcePort camelCase"
    );
}

#[test]
fn cookie_param_optional_fields_skipped_when_none() {
    let c = cookie_param("sid", "abc123");
    let j = serde_json::to_string(&c).expect("serialize");
    assert!(!j.contains("url"));
    assert!(!j.contains("domain"));
    assert!(!j.contains("expires"));
    // name + value always present
    assert!(j.contains("\"name\":\"sid\""));
    assert!(j.contains("\"value\":\"[REDACTED]\""));
}

#[test]
fn cookie_param_deserialize_accepts_raw_value() {
    let j = r#"{"name":"sid","value":"abc123","domain":"127.0.0.1"}"#;
    let c: NetworkCookieParam = serde_json::from_str(j).expect("deserialize");
    assert_eq!(c.name, "sid");
    assert_eq!(c.value.expose(), "abc123");
    assert_eq!(c.domain.as_deref(), Some("127.0.0.1"));
}

#[test]
fn cookie_decode_full() {
    let j = r#"{
        "name":"sid","value":"opaque","domain":"127.0.0.1","path":"/",
        "expires":-1,"size":3,"httpOnly":false,"secure":false,"session":true,
        "sameSite":null,"priority":null,"sourceScheme":null,
        "sourcePort":null,"partitionKey":null,"partitionKeyOpaque":null
    }"#;
    let c: NetworkCookie = serde_json::from_str(j).expect("decode");
    assert_eq!(c.name, "sid");
    assert_eq!(c.size, 3);
    assert!(c.session);
}

#[test]
fn same_site_enum_pascal_case() {
    assert_eq!(
        serde_json::to_string(&CookieSameSite::Strict).unwrap(),
        "\"Strict\""
    );
    assert_eq!(
        serde_json::to_string(&CookieSameSite::Lax).unwrap(),
        "\"Lax\""
    );
    assert_eq!(
        serde_json::to_string(&CookieSameSite::None).unwrap(),
        "\"None\""
    );
}

#[test]
fn priority_enum_pascal_case() {
    assert_eq!(
        serde_json::to_string(&CookiePriority::Low).unwrap(),
        "\"Low\""
    );
    assert_eq!(
        serde_json::to_string(&CookiePriority::High).unwrap(),
        "\"High\""
    );
}

#[test]
fn source_scheme_enum_pascal_case() {
    assert_eq!(
        serde_json::to_string(&CookieSourceScheme::Unset).unwrap(),
        "\"Unset\""
    );
    assert_eq!(
        serde_json::to_string(&CookieSourceScheme::NonSecure).unwrap(),
        "\"NonSecure\""
    );
    assert_eq!(
        serde_json::to_string(&CookieSourceScheme::Secure).unwrap(),
        "\"Secure\""
    );
}

#[test]
fn cookie_source_inline_tagged_serialize() {
    let s = CookieSource::Inline {
        cookies: vec![cookie_param("sid", "abc")],
    };
    let j = serde_json::to_value(&s).expect("serialize");
    assert_eq!(j["source"], "inline");
    assert!(j["cookies"].is_array());
}

#[test]
fn cookie_source_grant_tagged_serialize() {
    let s = CookieSource::Grant {
        grant_id: "01HZX0000000000000000000A0".to_string(),
    };
    let j = serde_json::to_value(&s).expect("serialize");
    assert_eq!(j["source"], "grant");
    assert_eq!(j["grant_id"], "01HZX0000000000000000000A0");
}

#[test]
fn cookie_source_deserialize_inline_round_trip() {
    let j = json!({"source": "inline", "cookies": []});
    let s: CookieSource = serde_json::from_value(j).expect("deserialize");
    matches!(s, CookieSource::Inline { .. });
}

/// v0.9.7 follow-up — the daemon dispatcher relies on `CookieSource`
/// failing closed when the payload omits the `source` tag. If this
/// test breaks, the dispatcher would silently fall through to its
/// `other => other` arm and reach `build_chromium_args` with an
/// empty cookies array — a soundness regression.
#[test]
fn cookie_source_deserialize_rejects_missing_source_tag() {
    let j = json!({"cookies": []});
    serde_json::from_value::<CookieSource>(j).expect_err("missing tag must reject");
}

/// Same contract for an empty object — protects the
/// `web.set_cookies({})` payload from silently dispatching.
#[test]
fn cookie_source_deserialize_rejects_empty_object() {
    let j = json!({});
    serde_json::from_value::<CookieSource>(j).expect_err("empty object must reject");
}

/// And for an unknown variant tag — defends against typos in
/// frontmatter (`source: "Grant"`, `source: "GRANT"`, etc.)
/// silently no-op-ing instead of rejecting.
#[test]
fn cookie_source_deserialize_rejects_unknown_source_tag() {
    let j = json!({"source": "foo", "cookies": []});
    serde_json::from_value::<CookieSource>(j).expect_err("unknown tag must reject");
}

/// Grant variant requires `grant_id` — a `{"source":"grant"}` with
/// no id must reject rather than reaching `Vault::substitute_cookies`
/// with an empty grant id.
#[test]
fn cookie_source_deserialize_rejects_grant_without_grant_id() {
    let j = json!({"source": "grant"});
    serde_json::from_value::<CookieSource>(j).expect_err("grant w/o id must reject");
}

/// Inline variant requires `cookies` — a `{"source":"inline"}` with
/// no array must reject rather than dispatching an empty cookie set.
#[test]
fn cookie_source_deserialize_rejects_inline_without_cookies() {
    let j = json!({"source": "inline"});
    serde_json::from_value::<CookieSource>(j).expect_err("inline w/o cookies must reject");
}

#[test]
fn validate_empty_array_ok() {
    assert!(validate_cookie_params(&[]).is_ok());
}

#[test]
fn validate_rejects_too_many_cookies() {
    let many: Vec<_> = (0..65)
        .map(|i| cookie_param(&format!("c{i}"), "v"))
        .collect();
    let err = validate_cookie_params(&many).expect_err("should reject");
    assert_eq!(err, CookieValidationError::TooManyCookies(65));
}

#[test]
fn validate_accepts_64_cookies() {
    let exactly: Vec<_> = (0..64)
        .map(|i| cookie_param(&format!("c{i}"), "v"))
        .collect();
    assert!(validate_cookie_params(&exactly).is_ok());
}

#[test]
fn validate_rejects_empty_name() {
    let c = cookie_param("", "v");
    assert_eq!(
        validate_cookie_params(&[c]).unwrap_err(),
        CookieValidationError::NameEmpty
    );
}

#[test]
fn validate_rejects_invalid_name_chars() {
    for bad in &['=', ';', ',', ' ', '\t', '"'] {
        let c = cookie_param(&format!("foo{bad}bar"), "v");
        match validate_cookie_params(&[c]).unwrap_err() {
            CookieValidationError::NameInvalid { ch } => assert_eq!(ch, *bad),
            other => panic!("expected NameInvalid for {bad:?}, got {other:?}"),
        }
    }
}

#[test]
fn validate_rejects_oversized_value() {
    let huge = "x".repeat(4097);
    let c = cookie_param("sid", &huge);
    assert_eq!(
        validate_cookie_params(&[c]).unwrap_err(),
        CookieValidationError::ValueTooLarge { size: 4097 }
    );
}

#[test]
fn validate_accepts_exactly_4096_byte_value() {
    let max = "x".repeat(4096);
    let c = cookie_param("sid", &max);
    assert!(validate_cookie_params(&[c]).is_ok());
}

#[test]
fn validate_accepts_session_cookie_expires_minus_one() {
    let mut c = cookie_param("sid", "v");
    c.expires = Some(-1.0);
    assert!(validate_cookie_params(&[c]).is_ok());
}

#[test]
fn validate_rejects_expires_zero() {
    let mut c = cookie_param("sid", "v");
    c.expires = Some(0.0);
    matches!(
        validate_cookie_params(&[c]).unwrap_err(),
        CookieValidationError::InvalidExpires(_)
    );
}

#[test]
fn validation_error_serialize_tag() {
    let e = CookieValidationError::NameEmpty;
    let j = serde_json::to_value(&e).expect("serialize");
    assert_eq!(j["code"], "name_empty");
}

// === v0.9.6 follow-up: CookieKeychainBlob daemon-side deserialization
// edge cases (the schema_version 1 invariant + forward-compat shape).
// These mirror the CLI's validate_cookie_blob_json but at the daemon
// side — defense-in-depth for the keychain blob shape that crosses
// the host-fn boundary.

#[test]
fn cookie_keychain_blob_deserialises_minimal_well_formed_shape() {
    let json = r#"{"schema_version":1,"cookies":[]}"#;
    let blob: CookieKeychainBlob = serde_json::from_str(json).expect("ok");
    assert_eq!(blob.schema_version, 1);
    assert!(blob.cookies.is_empty());
}

#[test]
fn cookie_keychain_blob_tolerates_extra_top_level_fields_forward_compat() {
    // Future schema additions (e.g. a `ttl_ms` or `creator` field at
    // the blob level) MUST NOT prevent v0.9.6 daemons from deserialising
    // — serde's default is to ignore unknown fields. Pin this so a
    // future #[serde(deny_unknown_fields)] addition is caught and
    // intentional.
    let json = r#"{"schema_version":1,"cookies":[],"future_field":42,"another":"x"}"#;
    let blob: CookieKeychainBlob = serde_json::from_str(json).expect("ok");
    assert_eq!(blob.schema_version, 1);
}

#[test]
fn cookie_keychain_blob_rejects_missing_schema_version_field() {
    // No default for schema_version — missing → deserialise error.
    let json = r#"{"cookies":[]}"#;
    serde_json::from_str::<CookieKeychainBlob>(json).expect_err("must reject");
}

#[test]
fn cookie_keychain_blob_rejects_missing_cookies_field() {
    let json = r#"{"schema_version":1}"#;
    serde_json::from_str::<CookieKeychainBlob>(json).expect_err("must reject");
}

#[test]
fn cookie_keychain_blob_rejects_cookies_as_object_not_array() {
    let json = r#"{"schema_version":1,"cookies":{"not":"an array"}}"#;
    serde_json::from_str::<CookieKeychainBlob>(json).expect_err("must reject");
}

#[test]
fn cookie_keychain_blob_round_trips_through_serde() {
    let original = CookieKeychainBlob {
        schema_version: 1,
        cookies: vec![NetworkCookieParam {
            name: "sid".to_string(),
            value: loom_shared::Redacted::new("v".to_string()),
            url: None,
            domain: Some("x.com".to_string()),
            path: None,
            secure: None,
            http_only: None,
            same_site: None,
            expires: None,
            priority: None,
            source_scheme: None,
            source_port: None,
            partition_key: None,
        }],
    };
    let json = serde_json::to_string(&original).expect("serialize");
    // Round-trip is asymmetric due to Redacted's serialize-as-[REDACTED]:
    // the deserialised blob will have value = "[REDACTED]". This is
    // the intended security invariant — pin it so future "transparent
    // round-trip" refactors are caught.
    let back: CookieKeychainBlob = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.schema_version, 1);
    assert_eq!(back.cookies.len(), 1);
    assert_eq!(back.cookies[0].name, "sid");
    // Asymmetric round-trip: value field is "[REDACTED]" not "v".
    assert_eq!(back.cookies[0].value.expose(), "[REDACTED]");
}

#[test]
fn cookie_keychain_blob_deserialises_blob_with_per_cookie_extra_fields() {
    // NetworkCookieParam already tolerates unknown fields (serde
    // default), and Redacted<String> deserialises as a String. Pin
    // the forward-compat invariant for the wrapping blob too.
    let json = r#"{"schema_version":1,"cookies":[
        {"name":"sid","value":"v","domain":"x","future_attr":42,"another":"yes"}
    ]}"#;
    let blob: CookieKeychainBlob = serde_json::from_str(json).expect("ok");
    assert_eq!(blob.cookies.len(), 1);
}

#[test]
fn cookie_keychain_blob_with_100_cookies_deserialises() {
    // No size limit on the deserialise path; the validator
    // (validate_cookie_params) enforces the 64-cookie cap at the
    // verb-execute level. Pin that the deserialiser handles a blob
    // that exceeds the cap.
    let mut s = String::from(r#"{"schema_version":1,"cookies":["#);
    for i in 0..100 {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(r#"{{"name":"c{i}","value":"v","domain":"x"}}"#));
    }
    s.push_str("]}");
    let blob: CookieKeychainBlob = serde_json::from_str(&s).expect("ok");
    assert_eq!(blob.cookies.len(), 100);
}
