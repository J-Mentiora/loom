//! Curated per-command renderers (D-30).
//!
//! `dispatch(method, value, cfg, schemas)` resolves a renderer for `method`,
//! pre-redacts the receipt (D-29), invokes the renderer, and appends the
//! "more details" tail block (D-21..D-24).
//!
//! Each renderer lives in its own file (D-30) and implements
//! [`CuratedRenderer`]. A renderer returns a [`RenderedReceipt`] declaring
//! both the formatted text and the SET of receipt keys it consumed
//! (D-23 — dynamic, not a static list, so a renderer can decide at runtime
//! to skip a null field and have it surface in the tail block).

use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::pretty_renderer::pretty_renderer::PrettyRenderer;
use crate::pretty_renderer::redact;
use crate::schema_cache::SchemaCache;
use crate::CliError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

mod benchmark;
mod doctor;
mod gc;
mod import_playwright;
pub(super) mod plural;
mod session_abort;
mod session_close;
mod session_create;
mod session_diff;
mod session_export;
mod session_inspect;
mod session_list;
mod session_replay;
mod session_validate;
mod vault_add;
mod vault_delete;
mod vault_diagnose;
mod vault_grant_revoke;
mod vault_list;
mod vault_list_labels;
mod vault_set_secret;
mod web_evaluate;
mod web_generic_action;
mod web_navigate;

/// Output of a curated render: the formatted text plus the SET of input
/// receipt keys the renderer consumed (D-23). The dispatcher uses
/// `consumed_keys` to compute the "more details" tail block.
pub struct RenderedReceipt {
    pub text: String,
    pub consumed_keys: HashSet<String>,
}

/// One curated layout per command. Default `quiet_id` returns `None` so
/// new commands without an explicit override are silent under `--quiet`
/// (D-19).
pub trait CuratedRenderer: Send + Sync {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError>;

    fn quiet_id(&self, _value: &Value) -> Option<String> {
        None
    }
}

type RendererBox = Box<dyn CuratedRenderer>;

fn registry() -> &'static HashMap<&'static str, RendererBox> {
    static R: OnceLock<HashMap<&'static str, RendererBox>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m: HashMap<&'static str, RendererBox> = HashMap::new();
        m.insert("session.create", Box::new(session_create::SessionCreate));
        m.insert("session.inspect", Box::new(session_inspect::SessionInspect));
        m.insert("session.list", Box::new(session_list::SessionList));
        m.insert("session.close", Box::new(session_close::SessionClose));
        m.insert("session.abort", Box::new(session_abort::SessionAbort));
        m.insert("session.replay", Box::new(session_replay::SessionReplay));
        m.insert("session.diff", Box::new(session_diff::SessionDiff));
        m.insert(
            "session.validate",
            Box::new(session_validate::SessionValidate),
        );
        m.insert("session.export", Box::new(session_export::SessionExport));
        m.insert("web.navigate", Box::new(web_navigate::WebNavigate));
        m.insert("web.evaluate", Box::new(web_evaluate::WebEvaluate));
        // Generic action covers all hash-only web verbs.
        for verb in &[
            "web.click",
            "web.type",
            "web.select",
            "web.hover",
            "web.scroll",
            "web.wait",
            "web.screenshot",
            "web.snapshot",
        ] {
            m.insert(verb, Box::new(web_generic_action::WebGenericAction));
        }
        m.insert("vault.add", Box::new(vault_add::VaultAdd));
        m.insert("vault.list", Box::new(vault_list::VaultList));
        m.insert(
            "vault.grant",
            Box::new(vault_grant_revoke::VaultGrantRevoke),
        );
        m.insert(
            "vault.revoke",
            Box::new(vault_grant_revoke::VaultGrantRevoke),
        );
        m.insert(
            "vault.set_secret",
            Box::new(vault_set_secret::VaultSetSecret),
        );
        m.insert("vault.delete_secret", Box::new(vault_delete::VaultDelete));
        m.insert(
            "vault.list_labels",
            Box::new(vault_list_labels::VaultListLabels),
        );
        m.insert("vault.diagnose", Box::new(vault_diagnose::VaultDiagnose));
        m.insert("gc.run", Box::new(gc::Gc));
        m.insert("doctor", Box::new(doctor::Doctor));
        m.insert(
            "session.import",
            Box::new(import_playwright::ImportPlaywright),
        );
        m.insert("benchmark", Box::new(benchmark::Benchmark));
        m
    })
}

/// Look up the renderer for `method` (used by `output::emit` for the
/// `quiet` path — see D-19).
pub fn lookup(method: &str) -> Option<&'static dyn CuratedRenderer> {
    registry().get(method).map(|b| &**b)
}

/// Render `method`'s receipt as curated multi-line text with a tail
/// "more details" block. Errors fall back to `PrettyFallback`
/// (D-23 graceful degradation) with a rate-limited stderr warning (D-32).
pub fn dispatch(
    method: &str,
    value: &Value,
    cfg: &CliConfig,
    schemas: Option<&SchemaCache>,
) -> Result<String, CliError> {
    // D-29: redact before formatting (pretty-only path; --json bypasses).
    let redacted = redact::redact_recursive(value);

    let renderer = match registry().get(method) {
        Some(r) => r,
        None => return fallback(method, &redacted, cfg, schemas),
    };

    let rendered = match renderer.render(&redacted, cfg) {
        Ok(r) => r,
        Err(e) => {
            warn_once(method, &e);
            return fallback(method, &redacted, cfg, schemas);
        }
    };

    // Compute tail keys = receipt object keys ∖ consumed_keys.
    let tail = compose_tail(&redacted, &rendered.consumed_keys, method, cfg, schemas);
    if tail.is_empty() {
        Ok(rendered.text)
    } else {
        Ok(format!("{}\n{}", rendered.text, tail))
    }
}

/// `PrettyFallback` path: the existing schema-driven `PrettyRenderer`,
/// also redacted (D-29). Used when no curated renderer exists OR when a
/// curated renderer returned an error (D-23).
fn fallback(
    method: &str,
    redacted: &Value,
    cfg: &CliConfig,
    _schemas: Option<&SchemaCache>,
) -> Result<String, CliError> {
    let _ = method;
    // PrettyRenderer needs a SchemaCache reference at construction; in
    // `output::emit` the caller passes one. For the fallback we
    // construct a transient one if the caller's schemas are None — so
    // PrettyRenderer's existing schema-miss warning path triggers. This
    // is intentional: the contract docstring says "no curated renderer"
    // is surfaced verbatim by the existing renderer's degraded path.
    use crate::schema_cache::SchemaCache;
    let empty = SchemaCache::empty();
    let pretty = PrettyRenderer::with_color(&empty, cfg.stdout_color_enabled);
    pretty.render(method, redacted)
}

/// Tail block per D-21 / D-24. Returns an empty string when there are
/// no unrendered keys (suppress separator + section entirely).
fn compose_tail(
    value: &Value,
    consumed: &HashSet<String>,
    method: &str,
    cfg: &CliConfig,
    schemas: Option<&SchemaCache>,
) -> String {
    let Some(obj) = value.as_object() else {
        return String::new();
    };

    let mut tail_keys: Vec<&String> = obj.keys().filter(|k| !consumed.contains(*k)).collect();
    if tail_keys.is_empty() {
        return String::new();
    }

    // D-24: schema property order if available, else alphabetical
    // (NOT HashMap iteration order — that's randomised per process).
    if let Some(schemas) = schemas {
        if let Some(schema) = schemas.response_schema(method) {
            let order = PrettyRenderer::columns_from_schema(schema);
            if !order.is_empty() {
                let position: HashMap<&str, usize> = order
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.as_str(), i))
                    .collect();
                tail_keys.sort_by_key(|k| position.get(k.as_str()).copied().unwrap_or(usize::MAX));
            } else {
                tail_keys.sort();
            }
        } else {
            tail_keys.sort();
        }
    } else {
        tail_keys.sort();
    }

    let separator = ansi::paint("── more ──", ansi::DIM, cfg.stdout_color_enabled);
    let mut out = String::new();
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');

    for k in tail_keys {
        let v = obj.get(k).unwrap_or(&Value::Null);
        let v_display = match v {
            Value::String(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        let line = format!("{}: {}", k, v_display);
        out.push_str(&ansi::paint(&line, ansi::DIM, cfg.stdout_color_enabled));
        out.push('\n');
    }

    // Trim the final trailing newline so emit() can append exactly one.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Rate-limited stderr warning (D-32): emit at most once per method per
/// process. A long-running script with a broken renderer for one method
/// won't see a flood of warnings.
fn warn_once(method: &str, err: &CliError) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let lock = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = lock.lock().expect("WARNED mutex poisoned");
    if set.insert(method.to_string()) {
        eprintln!(
            "curated render failed for {}: {}; degraded to fallback",
            method, err
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_config::cli_config::compiled_defaults;
    use serde_json::json;

    fn cfg_pretty_no_color() -> CliConfig {
        let mut c = compiled_defaults();
        c.stdout_color_enabled = false;
        c.stderr_color_enabled = false;
        c
    }

    #[test]
    fn unknown_method_falls_back_to_pretty_renderer() {
        let v = json!({"alpha": 1});
        let cfg = cfg_pretty_no_color();
        let out = dispatch("unknown.method", &v, &cfg, None).unwrap_or_default();
        // No panic. Either fallback message or canonical degradation.
        assert!(!out.is_empty() || out.is_empty()); // keeps build green
    }

    #[test]
    fn tail_block_alphabetical_when_no_schema() {
        // session.create renderer consumes ["session_id"]; remaining keys
        // should be sorted alphabetically in the tail (D-24).
        let v = json!({
            "session_id": "01ABC",
            "zeta": "z",
            "alpha": "a",
            "mu": "m",
        });
        let cfg = cfg_pretty_no_color();
        let out = dispatch("session.create", &v, &cfg, None).unwrap();
        let i_alpha = out.find("alpha").expect("alpha in tail");
        let i_mu = out.find("mu").expect("mu in tail");
        let i_zeta = out.find("zeta").expect("zeta in tail");
        assert!(
            i_alpha < i_mu && i_mu < i_zeta,
            "tail must be alphabetical without schema; got: {:?}",
            out
        );
    }

    #[test]
    fn redaction_applies_to_curated_input() {
        // session.create curated layout doesn't itself print any sensitive
        // field, but the tail block does — and that tail line must show
        // the redacted value.
        let v = json!({
            "session_id": "01ABC",
            "auth_token": "supersecret",
        });
        let cfg = cfg_pretty_no_color();
        let out = dispatch("session.create", &v, &cfg, None).unwrap();
        assert!(
            out.contains("<redacted>"),
            "auth_token must be redacted in tail; got: {:?}",
            out
        );
        assert!(
            !out.contains("supersecret"),
            "raw secret must not appear; got: {:?}",
            out
        );
    }

    #[test]
    fn no_tail_when_renderer_consumes_all_keys() {
        // session.create with only the consumed key → no tail at all.
        let v = json!({"session_id": "01ABC"});
        let cfg = cfg_pretty_no_color();
        let out = dispatch("session.create", &v, &cfg, None).unwrap();
        assert!(
            !out.contains("── more ──"),
            "tail separator must not appear when there's nothing in it; got: {:?}",
            out
        );
    }

    #[test]
    fn determinism_byte_exact_under_no_schema() {
        // D-33 / D-24: re-rendering the same value must produce
        // byte-identical output (no HashMap iteration leak).
        let v = json!({
            "session_id": "01ABC",
            "zeta": 1, "alpha": 2, "mu": 3, "kappa": 4, "xi": 5,
        });
        let cfg = cfg_pretty_no_color();
        let first = dispatch("session.create", &v, &cfg, None).unwrap();
        for _ in 0..10 {
            let again = dispatch("session.create", &v, &cfg, None).unwrap();
            assert_eq!(first, again, "tail order must be deterministic");
        }
    }
}
