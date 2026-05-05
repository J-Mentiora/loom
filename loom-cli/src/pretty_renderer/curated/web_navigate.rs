use super::plural::plural;
use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// `loom action web.navigate` — 5-line layout per AC-TTY-01.
pub struct WebNavigate;

impl CuratedRenderer for WebNavigate {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let mut consumed = HashSet::new();
        let mut lines: Vec<String> = Vec::with_capacity(5);

        // status (colored ok/error/...)
        if let Some(status) = value.get("status").and_then(|v| v.as_str()) {
            let code = match status {
                "ok" | "completed" | "success" => ansi::GREEN,
                "error" | "failed" => ansi::RED,
                _ => ansi::YELLOW,
            };
            lines.push(format!(
                "status: {}",
                ansi::paint(status, code, cfg.stdout_color_enabled)
            ));
            consumed.insert("status".to_string());
        }

        if let Some(url) = value.get("final_url").and_then(|v| v.as_str()) {
            lines.push(format!(
                "final_url: {}",
                ansi::paint(url, ansi::CYAN, cfg.stdout_color_enabled)
            ));
            consumed.insert("final_url".to_string());
        }

        if let Some(hash) = value.get("action_hash").and_then(|v| v.as_str()) {
            lines.push(format!("action_hash: {}", hash));
            consumed.insert("action_hash".to_string());
        }

        if let Some(n) = value.get("console_count").and_then(|v| v.as_u64()) {
            lines.push(format!("console_count: {}", plural(n, "line")));
            consumed.insert("console_count".to_string());
        }

        if let Some(ns) = value.get("network_summary") {
            let total = ns.get("total_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let bytes = ns.get("total_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
            let errors = ns.get("error_count").and_then(|v| v.as_u64()).unwrap_or(0);
            lines.push(format!(
                "network_summary: {}, {} bytes, {}",
                plural(total, "request"),
                bytes,
                plural(errors, "error"),
            ));
            consumed.insert("network_summary".to_string());
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
