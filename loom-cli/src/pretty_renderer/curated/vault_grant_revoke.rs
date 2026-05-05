use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct VaultGrantRevoke;

impl CuratedRenderer for VaultGrantRevoke {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("ok");
        let code = if status == "error" { ansi::RED } else { ansi::GREEN };
        let mut consumed = HashSet::new();
        consumed.insert("status".to_string());
        Ok(RenderedReceipt {
            text: ansi::paint(&format!("status: {}", status), code, cfg.stdout_color_enabled),
            consumed_keys: consumed,
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("grant_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
