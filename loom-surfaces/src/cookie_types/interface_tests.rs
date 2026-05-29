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
    assert!(j.get("sourcePort").is_some(), "expected sourcePort camelCase");
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
    assert_eq!(serde_json::to_string(&CookieSameSite::Strict).unwrap(), "\"Strict\"");
    assert_eq!(serde_json::to_string(&CookieSameSite::Lax).unwrap(), "\"Lax\"");
    assert_eq!(serde_json::to_string(&CookieSameSite::None).unwrap(), "\"None\"");
}

#[test]
fn priority_enum_pascal_case() {
    assert_eq!(serde_json::to_string(&CookiePriority::Low).unwrap(), "\"Low\"");
    assert_eq!(serde_json::to_string(&CookiePriority::High).unwrap(), "\"High\"");
}

#[test]
fn source_scheme_enum_pascal_case() {
    assert_eq!(serde_json::to_string(&CookieSourceScheme::Unset).unwrap(), "\"Unset\"");
    assert_eq!(serde_json::to_string(&CookieSourceScheme::NonSecure).unwrap(), "\"NonSecure\"");
    assert_eq!(serde_json::to_string(&CookieSourceScheme::Secure).unwrap(), "\"Secure\"");
}

#[test]
fn cookie_source_inline_tagged_serialize() {
    let s = CookieSource::Inline { cookies: vec![cookie_param("sid", "abc")] };
    let j = serde_json::to_value(&s).expect("serialize");
    assert_eq!(j["source"], "inline");
    assert!(j["cookies"].is_array());
}

#[test]
fn cookie_source_grant_tagged_serialize() {
    let s = CookieSource::Grant { grant_id: "01HZX0000000000000000000A0".to_string() };
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

#[test]
fn validate_empty_array_ok() {
    assert!(validate_cookie_params(&[]).is_ok());
}

#[test]
fn validate_rejects_too_many_cookies() {
    let many: Vec<_> = (0..65).map(|i| cookie_param(&format!("c{i}"), "v")).collect();
    let err = validate_cookie_params(&many).expect_err("should reject");
    assert_eq!(err, CookieValidationError::TooManyCookies(65));
}

#[test]
fn validate_accepts_64_cookies() {
    let exactly: Vec<_> = (0..64).map(|i| cookie_param(&format!("c{i}"), "v")).collect();
    assert!(validate_cookie_params(&exactly).is_ok());
}

#[test]
fn validate_rejects_empty_name() {
    let c = cookie_param("", "v");
    assert_eq!(validate_cookie_params(&[c]).unwrap_err(), CookieValidationError::NameEmpty);
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
