use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// Renders the wire ValidationResult ({session_id, passed, reasons}) as
/// human PASS/FAIL prose for `--pretty`/TTY output. The `session validate`
/// handler routes through `emit_to_stdout`, so `--json` gets the canonical
/// JSON object and this renderer owns the human path.
pub struct SessionValidate;

impl CuratedRenderer for SessionValidate {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let passed = value
            .get("passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let head = if passed {
            ansi::paint("PASS", ansi::GREEN, cfg.stdout_color_enabled)
        } else {
            ansi::paint("FAIL", ansi::RED, cfg.stdout_color_enabled)
        };
        let mut text = head;
        if !passed {
            if let Some(reasons) = value.get("reasons").and_then(|v| v.as_array()) {
                for r in reasons {
                    if let Some(s) = r.as_str() {
                        text.push('\n');
                        text.push_str("  - ");
                        text.push_str(&ansi::paint(s, ansi::DIM, cfg.stdout_color_enabled));
                    }
                }
            }
        }
        let mut consumed = HashSet::new();
        consumed.insert("passed".to_string());
        consumed.insert("reasons".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
