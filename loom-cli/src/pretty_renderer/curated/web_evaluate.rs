use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct WebEvaluate;

impl CuratedRenderer for WebEvaluate {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let mut consumed = HashSet::new();
        let mut lines = Vec::new();

        if let Some(hash) = value.get("action_hash").and_then(|v| v.as_str()) {
            lines.push(format!(
                "action_hash: {}",
                ansi::paint(hash, ansi::GREEN, cfg.stdout_color_enabled)
            ));
            consumed.insert("action_hash".to_string());
        }

        if let Some(rv) = value.get("return_value_json") {
            let s = serde_json::to_string(rv).unwrap_or_default();
            let display = if s.chars().count() > 200 {
                let mut t: String = s.chars().take(199).collect();
                t.push('…');
                format!("{} (use --json for full value)", t)
            } else {
                s
            };
            lines.push(format!("return_value: {}", display));
            consumed.insert("return_value_json".to_string());
        }

        if let Some(blob) = value.get("return_value_blob_ref") {
            let sha = blob.get("sha256").and_then(|v| v.as_str()).unwrap_or("?");
            let size = blob.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            lines.push(format!("blob_ref: sha256={}.. ({} bytes)", &sha[..sha.len().min(12)], size));
            consumed.insert("return_value_blob_ref".to_string());
        }

        Ok(RenderedReceipt {
            text: lines.join("\n"),
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
