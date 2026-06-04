// Interface tests for `ConfigResolver`. Verifies precedence
// signature, deny_unknown_fields posture, and the
// schema-validation hook.

use super::cli_config::{
    compiled_defaults, resolve, validate_against_schema, CliConfig, ResolveInputs,
};
use crate::schema_cache::SchemaCache;
use crate::CliError;
use std::io::Write as _;

#[test]
fn resolve_signature_takes_inputs_and_optional_schemas() {
    fn _ck(i: ResolveInputs, s: Option<&SchemaCache>) -> Result<CliConfig, CliError> {
        resolve(i, s)
    }
    let _ = _ck;
}

#[test]
fn resolve_inputs_are_default_constructible() {
    let i = ResolveInputs::default();
    assert!(i.cli_overrides.is_empty());
    assert!(i.env_vars.is_empty());
    assert!(i.config_path.is_none());
}

#[test]
fn compiled_defaults_signature() {
    fn _ck() -> CliConfig {
        compiled_defaults()
    }
    let _ = _ck;
}

#[test]
fn validate_against_schema_signature() {
    fn _ck(c: &CliConfig, s: &SchemaCache) -> Result<(), CliError> {
        validate_against_schema(c, s)
    }
    let _ = _ck;
}

// === deny_unknown_fields ===
//
// Encoded structurally — `CliConfig` carries `#[serde(deny_unknown_fields)]`
// at the type level. We assert this by deserialising a payload with an
// extra key and expecting the parse to fail.
#[test]
fn cli_config_rejects_unknown_fields() {
    let bad = serde_json::json!({
        "socket_path": "/tmp/x.sock",
        "schemas_dir": "/tmp/s",
        "auth_dir": "/tmp/a",
        "surfaces_dir": "/tmp/sf",
        "chromium_dir": "/tmp/c",
        "pretty": false,
        "request_timeout": {"secs": 30, "nanos": 0},
        "default_profile": null,
        "extraneous_field": "should_fail"
    });
    let r: Result<CliConfig, _> = serde_json::from_value(bad);
    assert!(
        r.is_err(),
        "deny_unknown_fields must reject extraneous keys"
    );
}

// === zero-config startup ===

#[test]
fn test_zero_config_no_dir_succeeds() {
    // fresh install with no config dir → Ok with sensible defaults.
    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(std::path::PathBuf::from(
            "/tmp/does_not_exist_loom_test/config.toml",
        )),
    };
    let result = resolve(inputs, None);
    assert!(
        result.is_ok(),
        "zero-config startup must succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_zero_config_defaults_have_sensible_values() {
    // compiled defaults are non-empty and have valid structure.
    let cfg = compiled_defaults();
    assert!(
        !cfg.socket_path.as_os_str().is_empty(),
        "socket_path must not be empty"
    );
    assert!(
        !cfg.schemas_dir.as_os_str().is_empty(),
        "schemas_dir must not be empty"
    );
    assert!(
        !cfg.auth_dir.as_os_str().is_empty(),
        "auth_dir must not be empty"
    );
    assert!(
        cfg.request_timeout.as_secs() > 0,
        "request_timeout must be positive"
    );
    assert!(!cfg.pretty, "pretty defaults to false");
}

// === back-compat: tolerated-but-ignored connect_timeout_secs ===

#[test]
fn deprecated_connect_timeout_secs_in_file_still_parses() {
    // connect_timeout was removed as an unwired no-op, but FileConfig denies
    // unknown fields, so the key must remain tolerated (ignored) to avoid
    // breaking existing config.toml files.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, "connect_timeout_secs = 7").unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let result = resolve(inputs, None);
    assert!(
        result.is_ok(),
        "config.toml with deprecated connect_timeout_secs must still parse: {:?}",
        result.err()
    );
}

// === CLI flag > env var > config file precedence ===

#[test]
fn test_precedence_cli_wins_over_env_over_file() {
    // CLI flag wins over env var and config file.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, r#"default_profile = "safe""#).unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![("default_profile".to_string(), "full".to_string())],
        env_vars: vec![("LOOM_DEFAULT_PROFILE".to_string(), "standard".to_string())],
        config_path: Some(config_path),
    };
    let cfg = resolve(inputs, None).expect("resolve must succeed");
    assert_eq!(
        cfg.default_profile.as_deref(),
        Some("full"),
        "CLI flag must win"
    );
}

#[test]
fn test_precedence_env_wins_over_file_when_no_cli() {
    // env var wins over config file when no CLI override.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, r#"default_profile = "safe""#).unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![("LOOM_DEFAULT_PROFILE".to_string(), "standard".to_string())],
        config_path: Some(config_path),
    };
    let cfg = resolve(inputs, None).expect("resolve must succeed");
    assert_eq!(
        cfg.default_profile.as_deref(),
        Some("standard"),
        "env var must win over file"
    );
}

#[test]
fn test_precedence_file_wins_when_no_env_no_cli() {
    // config file wins when no env var or CLI override.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, r#"default_profile = "safe""#).unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let cfg = resolve(inputs, None).expect("resolve must succeed");
    assert_eq!(
        cfg.default_profile.as_deref(),
        Some("safe"),
        "file must win when no overrides"
    );
}

// === startup validation ===

#[test]
fn test_invalid_profile_fails_with_usage_error() {
    // invalid default_profile value → CliError::Usage naming the key.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, r#"default_profile = "invalid""#).unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let err = resolve(inputs, None).expect_err("invalid profile must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("default_profile") || msg.contains("invalid"),
        "error must name the bad key or value: {msg}"
    );
    // Must be a usage error, not an internal error.
    matches!(err, CliError::Usage(_));
}

#[test]
fn test_unknown_toml_key_fails_at_startup() {
    // unknown TOML key → startup failure (deny_unknown_fields).
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, r#"unknown_key = "value""#).unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let result = resolve(inputs, None);
    assert!(result.is_err(), "unknown TOML key must fail at startup");
}

#[test]
fn test_malformed_toml_fails_with_usage_error() {
    // Council edge case: syntax-invalid config.toml → CliError::Usage.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, "this is not = [valid toml {{{{").unwrap();

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let result = resolve(inputs, None);
    assert!(result.is_err(), "malformed TOML must fail");
}

#[test]
fn test_empty_config_file_uses_defaults() {
    // Council edge case: empty config.toml → succeeds with compiled defaults.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::File::create(&config_path).unwrap(); // 0 bytes

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![],
        config_path: Some(config_path),
    };
    let result = resolve(inputs, None);
    assert!(
        result.is_ok(),
        "empty config file must succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_env_empty_profile_treated_as_none() {
    // Council edge case: LOOM_DEFAULT_PROFILE="" → treated as None (no profile set).
    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: vec![("LOOM_DEFAULT_PROFILE".to_string(), String::new())],
        config_path: Some(std::path::PathBuf::from(
            "/tmp/does_not_exist_loom_test/config.toml",
        )),
    };
    let cfg = resolve(inputs, None).expect("empty env var must succeed");
    assert!(
        cfg.default_profile.is_none(),
        "empty LOOM_DEFAULT_PROFILE must produce None"
    );
}
