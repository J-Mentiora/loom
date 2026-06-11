//! Embedded-first schema provider: the daemon validates builtin methods
//! against the schemas compiled into the binary, so a stale on-disk schema
//! from an earlier install can never shadow them (the v0.11.0 regression:
//! a pre-settle-capture `web.navigate.json` rejected the documented
//! `until`/`timeout_ms` args while the fresher `web.wait_for.json` accepted
//! them).

use loom_rpc::{
    schema_provider::schema_provider::SchemaProvider,
    schema_validator::schema_validator::{SchemaValidator, SchemaValidatorApi, ValidationOutcome},
};
use serde_json::json;

/// The pre-settle-capture navigate schema as found on the repro machine —
/// `properties: {session, url}` only, `additionalProperties: false`.
const STALE_NAVIGATE: &str = r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"url":{"type":"string"}},"required":["session","url"],"additionalProperties":false},"response":{"type":"object"}}"#;

#[test]
fn embedded_provider_validates_settle_args_without_any_disk_state() {
    // Pre-postinstall daemon: no schema dir anywhere. Builtins must still
    // be enforced AND accept their full documented arg sets.
    let provider = SchemaProvider::load_embedded().expect("embedded schemas compile");
    let validator = SchemaValidator::new(provider);
    for (method, params) in [
        (
            "web.navigate",
            json!({"session": "s", "url": "https://example.test/", "until": "settled", "timeout_ms": 5000}),
        ),
        (
            "web.wait_for",
            json!({"session": "s", "until": "settled", "timeout_ms": 3000}),
        ),
    ] {
        assert!(
            matches!(
                validator.validate_request(method, &params),
                ValidationOutcome::Pass
            ),
            "{method} must accept its documented args from the embedded schema"
        );
    }
}

#[test]
fn stale_disk_mirror_cannot_shadow_the_embedded_builtin() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("web.navigate.json"), STALE_NAVIGATE).expect("seed stale");

    let (provider, stale) =
        SchemaProvider::load_embedded_with_overlay(Some(dir.path())).expect("load");
    assert_eq!(stale.len(), 1, "stale mirror must be detected");
    assert_eq!(stale[0].method, "web.navigate");

    // The embedded schema wins: the args the stale file would reject pass.
    let validator = SchemaValidator::new(provider);
    let params = json!({"session": "s", "url": "https://example.test/", "until": "settled", "timeout_ms": 5000});
    assert!(
        matches!(
            validator.validate_request("web.navigate", &params),
            ValidationOutcome::Pass
        ),
        "embedded schema must win over the stale disk mirror"
    );
}

#[test]
fn overlay_extras_load_and_validate_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("custom.echo.json"),
        r#"{"request":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"response":{"type":"object"}}"#,
    )
    .expect("seed extra");

    let (provider, stale) =
        SchemaProvider::load_embedded_with_overlay(Some(dir.path())).expect("load");
    assert!(stale.is_empty());
    let validator = SchemaValidator::new(provider);
    assert!(matches!(
        validator.validate_request("custom.echo", &json!({"text": "hi"})),
        ValidationOutcome::Pass
    ));
    assert!(matches!(
        validator.validate_request("custom.echo", &json!({"bogus": 1})),
        ValidationOutcome::Violation(_)
    ));
}

#[test]
fn embedded_registry_snapshot_is_deterministic_and_complete() {
    let provider = SchemaProvider::load_embedded().expect("embedded schemas compile");
    use loom_rpc::schema_provider::schema_provider::SchemaProviderApi;
    let snapshot = provider.get_registry_snapshot();
    assert_eq!(
        snapshot.methods.len(),
        loom_shared::builtin_schemas::BUILTIN_SCHEMAS.len(),
        "snapshot must advertise every embedded method"
    );
    assert_eq!(
        snapshot.source_wit_sha256.len(),
        64,
        "registry hash must be a real sha256 hex"
    );
    // Machine-independent: a second load yields the identical hash.
    let again = SchemaProvider::load_embedded().expect("reload");
    assert_eq!(
        snapshot.source_wit_sha256,
        again.get_registry_snapshot().source_wit_sha256
    );
}
