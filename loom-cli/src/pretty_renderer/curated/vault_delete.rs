use super::{consumed, quiet_id_label, CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;

pub struct VaultDelete;

impl CuratedRenderer for VaultDelete {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let label = value.get("label").and_then(|v| v.as_str()).unwrap_or("?");
        let cascade = value
            .get("cascade_revoked_grants")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let body = if cascade > 0 {
            format!(
                "Deleted credential '{label}' (cascade-revoked {cascade} active grant{plural}).",
                plural = if cascade == 1 { "" } else { "s" }
            )
        } else {
            format!("Deleted credential '{label}'.")
        };
        let colour = if cascade > 0 {
            ansi::YELLOW
        } else {
            ansi::GREEN
        };
        let text = ansi::paint(&body, colour, cfg.stdout_color_enabled);

        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed(&["label", "cascade_revoked_grants"]),
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        quiet_id_label(value)
    }
}
