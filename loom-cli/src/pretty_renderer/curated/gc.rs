use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct Gc;

impl CuratedRenderer for Gc {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let mut parts = vec![ansi::paint("gc", ansi::GREEN, cfg.stdout_color_enabled)];
        let mut consumed = HashSet::new();
        if let Some(n) = value.get("deleted_count").and_then(|v| v.as_u64()) {
            parts.push(format!("deleted={}", n));
            consumed.insert("deleted_count".to_string());
        }
        if let Some(b) = value.get("freed_bytes").and_then(|v| v.as_u64()) {
            parts.push(format!("freed_bytes={}", b));
            consumed.insert("freed_bytes".to_string());
        }
        if let Some(s) = value.get("status").and_then(|v| v.as_str()) {
            parts.push(format!("status={}", s));
            consumed.insert("status".to_string());
        }
        Ok(RenderedReceipt {
            text: parts.join(" "),
            consumed_keys: consumed,
        })
    }
    // No quiet_id (D-19): gc is silent under --quiet.
}
