// ConfigResolver — CLI > env > file > compiled defaults.
//
// # Contract semantics
// - **Strict precedence.**
//   `argv flag` overrides `LOOM_*` env var; env overrides
//   `~/.config/loom/config.toml`; file overrides compiled defaults.
// - **Schema validation.** Validates the resolved `CliConfig` against the
//   same JSON Schema the daemon uses (loaded via `SchemaCache`); fails
//   at startup with `CliError::Usage` on invalid keys
//   (`#[serde(deny_unknown_fields)]`).
// - **No reload.** Each CLI invocation re-resolves config from
//   scratch — there is no in-flight reload path.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::cli_config::output_mode::OutputMode;
use crate::schema_cache::SchemaCache;
use crate::CliError;

/// Allowed values for `default_profile`.
const ALLOWED_PROFILES: &[&str] = &["safe", "standard", "full"];

/// The fully-resolved CLI config consumed by every handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    /// Resolved Unix-socket path (default: platform-specific).
    pub socket_path: PathBuf,
    /// Schemas root (default: `~/.config/loom/schemas/v1/`).
    pub schemas_dir: PathBuf,
    /// Auth artefact dir (default: `~/.config/loom/auth/`).
    pub auth_dir: PathBuf,
    /// Surfaces root (`.cwasm` cache + `.wasm` source).
    pub surfaces_dir: PathBuf,
    /// Chromium install root.
    pub chromium_dir: PathBuf,
    /// Pretty-renderer toggle (CLI flag mirror). Retained for back-compat
    /// and for the `LOOM_PRETTY` env var; the canonical resolved value
    /// lives in `output_mode`.
    pub pretty: bool,
    /// Resolved output mode per D-7 precedence (quiet > json > pretty >
    /// auto-detect). Replaces the old "JSON unless pretty=true" toggle.
    /// Default = `Json` (matches the canonical default before TTY
    /// auto-detection runs in `cli_main`).
    #[serde(default, skip_serializing_if = "is_default_output_mode")]
    pub output_mode: OutputMode,
    /// Per-stream color enablement per D-20. Resolved at startup from
    /// `--color`/`--no-color` flag, env vars (CLICOLOR_FORCE / NO_COLOR /
    /// CLICOLOR / TERM), and IsTerminal verdict for each stream
    /// independently.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub stdout_color_enabled: bool,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub stderr_color_enabled: bool,
    /// Request timeout.
    pub request_timeout: Duration,
    /// Default profile for `loom session create` when `--profile` is absent.
    /// Resolved from config file `default_profile`, env `LOOM_DEFAULT_PROFILE`,
    /// or CLI. Valid values: "safe" | "standard" | "full". None means RPC server picks default.
    pub default_profile: Option<String>,
}

/// Inputs for the precedence resolver.
#[derive(Debug, Clone, Default)]
pub struct ResolveInputs {
    /// `--key value` overrides parsed from argv (highest precedence).
    pub cli_overrides: Vec<(String, String)>,
    /// `LOOM_*` env-var snapshot.
    pub env_vars: Vec<(String, String)>,
    /// Optional explicit config-file path. `None` -> default location.
    pub config_path: Option<PathBuf>,
}

/// Private struct for TOML file parsing. Mirrors CliConfig fields as Option.
/// `deny_unknown_fields` ensures startup validation rejects unknown keys.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    socket_path: Option<PathBuf>,
    schemas_dir: Option<PathBuf>,
    auth_dir: Option<PathBuf>,
    surfaces_dir: Option<PathBuf>,
    chromium_dir: Option<PathBuf>,
    pretty: Option<bool>,
    /// Deprecated: tolerated for back-compat (FileConfig denies unknown fields) but
    /// no longer applied — connect timeout was an unwired no-op and was removed.
    #[allow(dead_code)]
    connect_timeout_secs: Option<u64>,
    request_timeout_secs: Option<u64>,
    default_profile: Option<String>,
}

/// Run the full resolve pipeline. Order:
///
/// 1. Compiled defaults.
/// 2. `~/.config/loom/config.toml` (if present).
/// 3. `LOOM_*` env vars.
/// 4. CLI flags.
///
/// Then validates against the JSON Schema in `SchemaCache`.
pub fn resolve(
    inputs: ResolveInputs,
    schemas: Option<&SchemaCache>,
) -> Result<CliConfig, CliError> {
    let mut cfg = compiled_defaults();

    // Step 2: apply file config.
    let config_path = inputs.config_path.unwrap_or_else(default_config_path);
    if config_path.exists() {
        let text = std::fs::read_to_string(&config_path).map_err(|e| {
            CliError::Usage(format!("cannot read {}: {}", config_path.display(), e))
        })?;
        let file: FileConfig = toml::from_str(&text)
            .map_err(|e| CliError::Usage(format!("config.toml parse error: {e}")))?;
        if let Some(v) = file.socket_path {
            cfg.socket_path = v;
        }
        if let Some(v) = file.schemas_dir {
            cfg.schemas_dir = v;
        }
        if let Some(v) = file.auth_dir {
            cfg.auth_dir = v;
        }
        if let Some(v) = file.surfaces_dir {
            cfg.surfaces_dir = v;
        }
        if let Some(v) = file.chromium_dir {
            cfg.chromium_dir = v;
        }
        if let Some(v) = file.pretty {
            cfg.pretty = v;
        }
        // connect_timeout_secs is accepted for back-compat but intentionally ignored.
        if let Some(s) = file.request_timeout_secs {
            cfg.request_timeout = Duration::from_secs(s);
        }
        if let Some(p) = file.default_profile {
            cfg.default_profile = Some(p);
        }
    }

    // Step 3: apply LOOM_* env vars.
    apply_overrides(&mut cfg, &inputs.env_vars, true)?;

    // Step 4: apply CLI overrides.
    apply_overrides(&mut cfg, &inputs.cli_overrides, false)?;

    // Step 5: validate default_profile if set.
    if let Some(ref p) = cfg.default_profile {
        if !ALLOWED_PROFILES.contains(&p.as_str()) {
            return Err(CliError::Usage(format!(
                "config: `default_profile` value {:?} is invalid; allowed values: {}",
                p,
                ALLOWED_PROFILES.join(", ")
            )));
        }
    }

    // Step 6: schema validation (if schemas provided).
    if let Some(schemas) = schemas {
        validate_against_schema(&cfg, schemas)?;
    }

    Ok(cfg)
}

/// Pure helper exposed for tests: returns the compiled defaults.
/// If HOME is unavailable (e.g. containers), falls back to `/tmp/loom`.
pub fn compiled_defaults() -> CliConfig {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let config_base = home.join(".config").join("loom");
    // Match loom_rpc::socket_server::default_socket_path():
    // macOS: ~/Library/Caches/loom/loom.sock  (dirs::cache_dir())
    // Linux: $XDG_RUNTIME_DIR/loom.sock or /tmp/loom.sock
    #[cfg(target_os = "macos")]
    let socket_path = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("loom")
        .join("loom.sock");
    #[cfg(not(target_os = "macos"))]
    let socket_path = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("loom.sock");
    CliConfig {
        socket_path,
        schemas_dir: config_base.join("schemas").join("v1"),
        auth_dir: config_base.join("auth"),
        surfaces_dir: config_base.join("surfaces"),
        chromium_dir: config_base.join("chromium"),
        pretty: false,
        output_mode: OutputMode::Json,
        // Default to "color allowed"; resolver in cli_main will downgrade
        // when the stream isn't a TTY or when env vars say no.
        stdout_color_enabled: true,
        stderr_color_enabled: true,
        request_timeout: Duration::from_secs(30),
        default_profile: None,
    }
}

fn default_true() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_default_output_mode(m: &OutputMode) -> bool {
    *m == OutputMode::Json
}

/// Pure helper: validate a `CliConfig` against the loom-rpc JSON
/// Schema. Returns `CliError::Usage` on schema violations.
pub fn validate_against_schema(config: &CliConfig, schemas: &SchemaCache) -> Result<(), CliError> {
    let schema = match schemas.request_schema("loom-cli-config") {
        Some(s) => s,
        None => return Ok(()),
    };
    let value = serde_json::to_value(config)
        .map_err(|e| CliError::Internal(format!("config serialization failed: {e}")))?;
    let compiled = jsonschema::validator_for(schema)
        .map_err(|e| CliError::Internal(format!("schema compile error: {e}")))?;
    let errors: Vec<String> = compiled
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "config schema validation: {}",
            errors.join("; ")
        )))
    }
}

/// Returns the default config file path: `~/.config/loom/config.toml`.
fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("loom")
        .join("config.toml")
}

/// Apply a list of (key, value) overrides to `cfg`.
/// `is_env`: true for LOOM_* env vars (strips LOOM_ prefix, lowercases);
///            false for CLI overrides (key is already the field name).
/// Empty values are treated as "not set" (no override applied).
fn apply_overrides(
    cfg: &mut CliConfig,
    overrides: &[(String, String)],
    is_env: bool,
) -> Result<(), CliError> {
    for (raw_key, val) in overrides {
        if val.is_empty() {
            continue; // empty string = not set
        }
        let field = if is_env {
            let key = raw_key.to_uppercase();
            if !key.starts_with("LOOM_") {
                continue;
            }
            key["LOOM_".len()..].to_lowercase()
        } else {
            raw_key.to_lowercase()
        };

        match field.as_str() {
            "socket_path" => cfg.socket_path = PathBuf::from(val),
            "schemas_dir" => cfg.schemas_dir = PathBuf::from(val),
            "auth_dir" => cfg.auth_dir = PathBuf::from(val),
            "surfaces_dir" => cfg.surfaces_dir = PathBuf::from(val),
            "chromium_dir" => cfg.chromium_dir = PathBuf::from(val),
            "pretty" => {
                cfg.pretty = val.parse::<bool>().map_err(|_| {
                    CliError::Usage(format!(
                        "invalid value for `pretty`: {:?}; expected true/false",
                        val
                    ))
                })?;
            }
            // connect_timeout / connect_timeout_secs: accepted but ignored (removed
            // no-op) — falls through to the silent `_ => {}` arm below.
            "request_timeout" | "request_timeout_secs" => {
                let secs = val.parse::<u64>().map_err(|_| {
                    CliError::Usage(format!(
                        "invalid value for `request_timeout`: {:?}; expected integer seconds",
                        val
                    ))
                })?;
                cfg.request_timeout = Duration::from_secs(secs);
            }
            "default_profile" => cfg.default_profile = Some(val.clone()),
            _ => {} // unknown keys from env/cli silently ignored (not a TOML file)
        }
    }
    Ok(())
}
