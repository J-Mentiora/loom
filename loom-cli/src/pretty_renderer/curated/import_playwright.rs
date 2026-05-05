use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct ImportPlaywright;

impl CuratedRenderer for ImportPlaywright {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let text = ansi::paint(
            &format!("imported session={}", id),
            ansi::GREEN,
            cfg.stdout_color_enabled,
        );
        let mut consumed = HashSet::new();
        consumed.insert("session_id".to_string());
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
