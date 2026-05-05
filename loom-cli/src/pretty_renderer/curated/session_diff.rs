use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// Receipt is the projected `.diff` value: `{ field_diffs: [...], action_count_delta: i64, ... }`
pub struct SessionDiff;

impl CuratedRenderer for SessionDiff {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let field_diffs = value.get("field_diffs").and_then(|v| v.as_array());
        let n = field_diffs.map(|a| a.len() as u64).unwrap_or(0);
        let delta = value
            .get("action_count_delta")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let head = format!(
            "{}, action_count_delta={}",
            super::plural::plural(n, "field diff"),
            delta
        );

        let mut text = head;
        if let Some(arr) = field_diffs {
            for d in arr {
                let key = d.get("key").and_then(|v| v.as_str()).unwrap_or("?");
                let old = d
                    .get("old")
                    .map(json_inline)
                    .unwrap_or_else(|| "null".into());
                let new = d
                    .get("new")
                    .map(json_inline)
                    .unwrap_or_else(|| "null".into());
                text.push('\n');
                text.push_str("  ");
                text.push_str(&ansi::paint(
                    &format!("- {}: {}", key, old),
                    ansi::RED,
                    cfg.stdout_color_enabled,
                ));
                text.push('\n');
                text.push_str("  ");
                text.push_str(&ansi::paint(
                    &format!("+ {}: {}", key, new),
                    ansi::GREEN,
                    cfg.stdout_color_enabled,
                ));
            }
        }

        let mut consumed = HashSet::new();
        consumed.insert("field_diffs".to_string());
        consumed.insert("action_count_delta".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
    // No quiet_id: diff projection has no top-level id (D-19).
}

fn json_inline(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_else(|_| "?".into());
    if s.chars().count() > 80 {
        let mut t: String = s.chars().take(79).collect();
        t.push('…');
        t
    } else {
        s
    }
}
