//! Registry-driven `loom action` help and pre-dispatch validation.
//!
//! Two consumers in `loom-cli`:
//!
//! 1. **`loom action --help`** — `cli_main` injects `render_all_actions_after_help()`
//!    as the Action subcommand's clap `after_help` before parsing, so clap's
//!    built-in help renders the registered-action list.
//!
//! 2. **`loom action <name> --help` / dispatch validation** — `action_commands`
//!    after parsing checks `extra` for `--help`/`-h` and calls
//!    [`render_per_action_help`]; otherwise it calls [`validate_against_registry`]
//!    to reject unknown flags (with did-you-mean) and missing required params
//!    BEFORE the existing schema validation runs.
//!
//! Both render functions accept the static registry; both are pure.

use std::collections::BTreeMap;

use loom_rpc::action_registry::{ActionMeta, ParamType, ACTIONS};

/// Render the full all-actions list, suitable for clap `after_help`.
/// Grouped by surface prefix, alphabetised within group.
pub fn render_all_actions_after_help() -> String {
    let mut by_surface: BTreeMap<&'static str, Vec<&'static ActionMeta>> = BTreeMap::new();
    for a in ACTIONS {
        by_surface.entry(a.surface_prefix()).or_default().push(a);
    }
    let mut out = String::new();
    out.push_str("Registered actions:\n");
    for (surface, actions) in &by_surface {
        out.push_str(&format!("\n  {surface}.*\n"));
        for a in actions {
            out.push_str(&format!("    {:<20} {}\n", a.name, a.summary));
        }
    }
    out.push_str("\nRun `loom action <name> --help` for the detailed signature.\n");
    out
}

/// The CLI flag name to display for a given registry param. The registry
/// stores `session_id` to match the JSON-RPC param name, but the CLI takes
/// `--session` (the parsed flag is folded into `session_id` at dispatch).
fn cli_flag_for(param_name: &str) -> &str {
    match param_name {
        "session_id" => "session",
        other => other,
    }
}

/// Render the detailed help for a single registered action — invoked from
/// `dispatch` when `extra` contains `--help` or `-h` and the action is in
/// the registry.
pub fn render_per_action_help(meta: &ActionMeta) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} — {}\n\n", meta.name, meta.summary));
    out.push_str(meta.description);
    out.push_str("\n\nUSAGE:\n    loom action ");
    out.push_str(meta.name);
    for p in meta.params {
        out.push(' ');
        let flag = cli_flag_for(p.name);
        let placeholder = if p.name == "session_id" {
            "<SESSION>".to_string()
        } else {
            format!("<{}>", p.ty.to_string().to_uppercase())
        };
        if p.required {
            out.push_str(&format!("--{flag} {placeholder}"));
        } else {
            out.push_str(&format!("[--{flag} {placeholder}]"));
        }
    }
    out.push_str("\n\nPARAMETERS:\n");
    for p in meta.params {
        let flag = cli_flag_for(p.name);
        out.push_str(&format!(
            "    --{:<12} {:<10} {}  {}\n",
            flag,
            format!("[{}]", p.ty),
            if p.required { "required" } else { "optional" },
            p.doc,
        ));
    }
    out.push_str("\nRETURNS:\n    ");
    out.push_str(meta.returns);
    out.push_str("\n\nEXAMPLE:\n    ");
    out.push_str(&render_example_shell(meta));
    out.push('\n');
    out
}

fn render_example_shell(meta: &ActionMeta) -> String {
    meta.example
        .iter()
        .map(|t| {
            if t.chars()
                .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '$' | '`' | '\\'))
            {
                format!("'{}'", t.replace('\'', "'\\''"))
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// True if `extra` contains a top-level help flag — `--help` or `-h` — i.e.
/// the user wants help, not validation. The trailing_var_arg attribute on
/// `ActionArgs.extra` makes clap collect `--help` raw rather than triggering
/// its built-in help, so we have to spot it ourselves.
pub fn extra_requests_help(extra: &[String]) -> bool {
    extra.iter().any(|s| s == "--help" || s == "-h")
}

/// Reject unknown flags and missing required params for a registered action,
/// validating types where the registry declares them.
///
/// `params_obj` is the JSON object already produced by
/// `action_commands::parse_extra_to_json_typed` — keys are flag names without
/// the `--` prefix, values are coerced JSON values.
///
/// On any violation, returns a single `Err(message)` with all problems
/// concatenated. The caller wraps that in `CliError::Usage` so exit code is 2.
pub fn validate_against_registry(
    meta: &ActionMeta,
    params_obj: &serde_json::Value,
) -> Result<(), String> {
    let obj = params_obj
        .as_object()
        .ok_or_else(|| "internal: extra params should be a JSON object".to_string())?;

    let mut problems: Vec<String> = Vec::new();
    let registry_param_names: Vec<&'static str> = meta.params.iter().map(|p| p.name).collect();
    let global_known: &[&str] = &["config", "pretty", "help"];

    // Reject unknown keys, with did-you-mean hint when exactly one registry
    // param is within Levenshtein-1 of the unknown key.
    for key in obj.keys() {
        let known_in_registry = registry_param_names.iter().any(|n| n == key);
        // `session` is folded in by `action_commands::dispatch` from the parsed
        // `--session` arg — registry tracks it as `session_id` for parity with
        // the JSON-RPC param name. Accept either.
        let is_session_alias = key == "session" || key == "session_id";
        let is_global = global_known.iter().any(|g| g == key);
        if known_in_registry || is_session_alias || is_global {
            continue;
        }
        let suggestion = registry_param_names
            .iter()
            .find(|n| levenshtein_at_most_1(key, n))
            .copied();
        match suggestion {
            Some(c) => problems.push(format!("unknown flag --{key} (did you mean --{c}?)")),
            None => problems.push(format!("unknown flag --{key}")),
        }
    }

    // Reject missing required params. `session_id` is supplied via
    // `--session` separately, so its presence is checked against the
    // accepted aliases.
    for p in meta.params {
        if !p.required {
            continue;
        }
        let present = if p.name == "session_id" {
            obj.contains_key("session_id") || obj.contains_key("session")
        } else {
            obj.contains_key(p.name)
        };
        if !present {
            problems.push(format!("missing required flag --{} ({})", p.name, p.doc));
        }
    }

    // Type-check values supplied for typed params. We do this locally so the
    // user gets fast feedback before any RPC call (and a clearer error than
    // the JSON-Schema validator's wording).
    for p in meta.params {
        let key = if p.name == "session_id" {
            "session"
        } else {
            p.name
        };
        let Some(value) = obj.get(key) else { continue };
        let ty_ok = match p.ty {
            ParamType::String => value.is_string(),
            ParamType::I64 => {
                value.is_i64()
                    || value
                        .as_str()
                        .map(|s| s.parse::<i64>().is_ok())
                        .unwrap_or(false)
            }
            ParamType::U64 => {
                value.is_u64()
                    || value
                        .as_str()
                        .map(|s| s.parse::<u64>().is_ok())
                        .unwrap_or(false)
            }
            ParamType::Object => value.is_object(),
            ParamType::Array => value.is_array(),
        };
        if !ty_ok {
            problems.push(format!(
                "invalid value for --{}: expected {}, got {}",
                p.name,
                p.ty,
                short_describe(value),
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

fn short_describe(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(n) => {
            if n.is_i64() {
                "integer"
            } else if n.is_u64() {
                "unsigned integer"
            } else {
                "number"
            }
        }
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Null => "null",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn levenshtein_at_most_1(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    if (ac.len() as isize - bc.len() as isize).abs() > 1 {
        return false;
    }
    if ac.len() == bc.len() {
        let mismatches = ac.iter().zip(&bc).filter(|(x, y)| x != y).count();
        mismatches == 1
    } else {
        let (short, long) = if ac.len() < bc.len() {
            (&ac, &bc)
        } else {
            (&bc, &ac)
        };
        let mut i = 0usize;
        let mut j = 0usize;
        let mut found_skip = false;
        while i < short.len() && j < long.len() {
            if short[i] == long[j] {
                i += 1;
                j += 1;
            } else if found_skip {
                return false;
            } else {
                j += 1;
                found_skip = true;
            }
        }
        true
    }
}

/// Compile-time exhaustiveness check for `ParamType` — kept as a function
/// rather than a test so adding a new variant fails the production build,
/// not just `cargo test`. (The variant must also be handled by
/// `validate_against_registry`'s match above.)
#[allow(dead_code)]
fn _paramtype_exhaustiveness_proof(t: ParamType) -> &'static str {
    match t {
        ParamType::String => "string",
        ParamType::I64 => "i64",
        ParamType::U64 => "u64",
        ParamType::Object => "object",
        ParamType::Array => "array",
    }
}
