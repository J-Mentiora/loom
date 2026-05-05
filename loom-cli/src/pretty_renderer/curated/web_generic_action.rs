use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// Hash-only tier (web.click, web.type, web.select, web.hover, web.scroll,
/// web.wait, web.screenshot, web.snapshot). Receipts have action_hash +
/// outcome_hash + emitted_at_ms.
pub struct WebGenericAction;

impl CuratedRenderer for WebGenericAction {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let hash = value
            .get("action_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let text = ansi::paint(
            &format!("action_hash={}", hash),
            ansi::GREEN,
            cfg.stdout_color_enabled,
        );
        let mut consumed = HashSet::new();
        consumed.insert("action_hash".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }

    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("action_hash")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
