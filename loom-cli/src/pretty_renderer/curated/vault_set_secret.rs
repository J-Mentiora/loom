use super::{consumed, quiet_id_label, CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;

pub struct VaultSetSecret;

impl CuratedRenderer for VaultSetSecret {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let label = value.get("label").and_then(|v| v.as_str()).unwrap_or("?");
        let replaced = value
            .get("replaced")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // D33e: plain-language success message with a next-step pointer.
        let lead = if replaced {
            format!("Replaced credential in your keychain (label: {label}).")
        } else {
            format!("Saved credential to your keychain (label: {label}).")
        };
        let tail = "Run `loom vault list-labels` to see all stored credentials.";
        let body = format!("{lead}\n{tail}");
        let text = ansi::paint(&body, ansi::GREEN, cfg.stdout_color_enabled);

        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed(&["label", "replaced", "size_bucket"]),
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        quiet_id_label(value)
    }
}
