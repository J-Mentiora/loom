use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct VaultListLabels;

impl CuratedRenderer for VaultListLabels {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let labels = value.get("labels").and_then(|v| v.as_array());
        let count = value
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| labels.map(|l| l.len() as u64).unwrap_or(0));

        let body = if count == 0 {
            "No stored credentials. Use `loom vault add --label <name> --from-stdin` to add one.".to_string()
        } else {
            let mut lines = Vec::with_capacity(count as usize + 1);
            lines.push(format!(
                "{count} stored credential{plural}:",
                plural = if count == 1 { "" } else { "s" }
            ));
            if let Some(arr) = labels {
                let mut names: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                names.sort_unstable();
                for n in names {
                    lines.push(format!("  • {n}"));
                }
            }
            lines.join("\n")
        };
        let text = ansi::paint(&body, ansi::DIM, cfg.stdout_color_enabled);

        let mut consumed = HashSet::new();
        consumed.insert("labels".to_string());
        consumed.insert("count".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
}
