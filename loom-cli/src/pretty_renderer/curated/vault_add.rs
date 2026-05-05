use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct VaultAdd;

impl CuratedRenderer for VaultAdd {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let id = value
            .get("grant_id")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("vault_id").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let text = ansi::paint(
            &format!("vault grant_id={} added", id),
            ansi::GREEN,
            cfg.stdout_color_enabled,
        );
        let mut consumed = HashSet::new();
        consumed.insert("grant_id".to_string());
        consumed.insert("vault_id".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("grant_id")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("vault_id").and_then(|v| v.as_str()))
            .map(str::to_string)
    }
}
