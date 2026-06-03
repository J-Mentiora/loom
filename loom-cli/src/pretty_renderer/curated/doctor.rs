use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct Doctor;

impl CuratedRenderer for Doctor {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let mut consumed = HashSet::new();
        let mut lines = Vec::new();

        if let Some(status) = value.get("status").and_then(|v| v.as_str()) {
            let (glyph, code) = match status {
                "ok" | "healthy" | "pass" => ("✓", ansi::GREEN),
                "warn" => ("!", ansi::YELLOW),
                _ => ("✗", ansi::RED),
            };
            lines.push(format!(
                "{} status: {}",
                ansi::paint(glyph, code, cfg.stdout_color_enabled),
                status
            ));
            consumed.insert("status".to_string());
        }

        if let Some(checks) = value.get("checks").and_then(|v| v.as_array()) {
            for c in checks {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let outcome = c
                    .get("status")
                    .and_then(|v| v.as_str())
                    .or_else(|| c.get("outcome").and_then(|v| v.as_str()))
                    .unwrap_or("?");
                let (glyph, code) = match outcome {
                    "ok" | "pass" | "passed" | "healthy" => ("OK", ansi::GREEN),
                    "warn" | "warning" => ("WARN", ansi::YELLOW),
                    _ => ("FAIL", ansi::RED),
                };
                lines.push(format!(
                    "  {} {}",
                    ansi::paint(glyph, code, cfg.stdout_color_enabled),
                    name
                ));
                // Surface the failure detail (e.g. the quarantine remediation
                // command) under a failing check — passing checks carry no
                // detail. Without this the remediation only reaches `--json`
                // output, never the default human view.
                if !matches!(outcome, "ok" | "pass" | "passed" | "healthy" | "warn" | "warning") {
                    if let Some(detail) = c.get("detail").and_then(|v| v.as_str()) {
                        if !detail.is_empty() {
                            lines.push(format!("      {}", detail));
                        }
                    }
                }
            }
            consumed.insert("checks".to_string());
        }

        Ok(RenderedReceipt {
            text: lines.join("\n"),
            consumed_keys: consumed,
        })
    }
    // No quiet_id (D-19): doctor is silent under --quiet.
}
