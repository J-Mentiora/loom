use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct Benchmark;

impl CuratedRenderer for Benchmark {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let mut consumed = HashSet::new();
        let mut lines = Vec::new();
        if let Some(s) = value.get("status").and_then(|v| v.as_str()) {
            let code = if s == "pass" || s == "ok" {
                ansi::GREEN
            } else {
                ansi::YELLOW
            };
            lines.push(ansi::paint(
                &format!("status: {}", s),
                code,
                cfg.stdout_color_enabled,
            ));
            consumed.insert("status".to_string());
        }
        if let Some(results) = value.get("results").and_then(|v| v.as_object()) {
            for (k, v) in results {
                let v_str = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                lines.push(format!("  {}: {}", k, v_str));
            }
            consumed.insert("results".to_string());
        }
        Ok(RenderedReceipt {
            text: lines.join("\n"),
            consumed_keys: consumed,
        })
    }
    // No quiet_id (D-19): benchmark is silent under --quiet.
}
