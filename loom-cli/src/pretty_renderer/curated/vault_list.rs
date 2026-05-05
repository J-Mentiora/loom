use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct VaultList;

impl CuratedRenderer for VaultList {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let arr = entries(value);
        if arr.is_empty() {
            return Ok(RenderedReceipt {
                text: ansi::paint("No vault entries.", ansi::DIM, cfg.stdout_color_enabled),
                consumed_keys: consumed_keys_for(value),
            });
        }
        let mut out = String::new();
        for e in &arr {
            let id = e
                .get("grant_id")
                .and_then(|v| v.as_str())
                .or_else(|| e.get("vault_id").and_then(|v| v.as_str()))
                .unwrap_or("?");
            out.push_str(&ansi::paint(
                &format!("grant_id={}", id),
                ansi::CYAN,
                cfg.stdout_color_enabled,
            ));
            out.push('\n');
        }
        if out.ends_with('\n') {
            out.pop();
        }
        Ok(RenderedReceipt {
            text: out,
            consumed_keys: consumed_keys_for(value),
        })
    }

    fn quiet_id(&self, value: &Value) -> Option<String> {
        let arr = entries(value);
        if arr.is_empty() {
            return None;
        }
        let ids: Vec<String> = arr
            .iter()
            .filter_map(|e| {
                e.get("grant_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| e.get("vault_id").and_then(|v| v.as_str()))
                    .map(str::to_string)
            })
            .collect();
        if ids.is_empty() {
            None
        } else {
            Some(ids.join("\n"))
        }
    }
}

fn entries(value: &Value) -> Vec<&Value> {
    if let Some(a) = value.as_array() {
        return a.iter().collect();
    }
    for k in &["entries", "grants", "items"] {
        if let Some(a) = value.get(*k).and_then(|v| v.as_array()) {
            return a.iter().collect();
        }
    }
    Vec::new()
}

fn consumed_keys_for(value: &Value) -> HashSet<String> {
    let mut s = HashSet::new();
    for k in &["entries", "grants", "items"] {
        if value.get(*k).is_some() {
            s.insert((*k).to_string());
        }
    }
    s
}
