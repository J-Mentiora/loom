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
                // FieldDiff serializes as field_path/source_value/replay_value
                // (loom-core replay_engine); source/replay are already stringified
                // JSON, so read them as &str rather than re-encoding.
                let field_path = d.get("field_path").and_then(|v| v.as_str()).unwrap_or("?");
                let old = d
                    .get("source_value")
                    .and_then(|v| v.as_str())
                    .map(truncate_inline)
                    .unwrap_or_else(|| "null".into());
                let new = d
                    .get("replay_value")
                    .and_then(|v| v.as_str())
                    .map(truncate_inline)
                    .unwrap_or_else(|| "null".into());
                text.push('\n');
                text.push_str("  ");
                text.push_str(&ansi::paint(
                    &format!("- {}: {}", field_path, old),
                    ansi::RED,
                    cfg.stdout_color_enabled,
                ));
                text.push('\n');
                text.push_str("  ");
                text.push_str(&ansi::paint(
                    &format!("+ {}: {}", field_path, new),
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

/// Truncate an already-string value to 80 chars with an ellipsis. Values arrive
/// pre-stringified (FieldDiff's source_value/replay_value are `String`), so we
/// render them verbatim rather than re-encoding through serde.
fn truncate_inline(s: &str) -> String {
    if s.chars().count() > 80 {
        let mut t: String = s.chars().take(79).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_config::cli_config::compiled_defaults;
    use serde_json::json;

    fn cfg() -> CliConfig {
        let mut c = compiled_defaults();
        c.stdout_color_enabled = false;
        c.stderr_color_enabled = false;
        c
    }

    /// A populated field diff must render the real FieldDiff fields
    /// (field_path/source_value/replay_value), not the `?`/`null` fallbacks.
    #[test]
    fn populated_field_diff_renders_path_and_values() {
        let v = json!({
            "field_diffs": [{
                "action_id": 1,
                "field_path": "receipt.timing_ticks",
                "source_value": "100",
                "replay_value": "101",
            }],
            "action_count_delta": 1,
        });
        let r = SessionDiff.render(&v, &cfg()).unwrap();
        assert!(
            r.text.contains("- receipt.timing_ticks: 100"),
            "old line should show field_path + source_value, got:\n{}",
            r.text
        );
        assert!(
            r.text.contains("+ receipt.timing_ticks: 101"),
            "new line should show field_path + replay_value, got:\n{}",
            r.text
        );
        assert!(
            !r.text.contains("?"),
            "no `?` placeholder for a populated diff, got:\n{}",
            r.text
        );
    }

    /// source_value/replay_value are already stringified JSON, so they must NOT
    /// be re-encoded (double-quoted) by the renderer.
    #[test]
    fn string_values_are_not_double_quoted() {
        let v = json!({
            "field_diffs": [{
                "action_id": 2,
                "field_path": "receipt.url",
                "source_value": "\"https://a\"",
                "replay_value": "\"https://b\"",
            }],
            "action_count_delta": 0,
        });
        let r = SessionDiff.render(&v, &cfg()).unwrap();
        // The stored value already carries its own quotes; rendering must not add more.
        assert!(
            r.text.contains("- receipt.url: \"https://a\""),
            "value should render verbatim without extra encoding, got:\n{}",
            r.text
        );
        assert!(
            !r.text.contains("\\\""),
            "value must not be escaped/double-encoded, got:\n{}",
            r.text
        );
    }

    /// A malformed entry missing source/replay values renders the `null`
    /// fallback gracefully (well-formed FieldDiff always carries strings, but the
    /// renderer must not panic or emit `?` for the value on partial input).
    #[test]
    fn missing_values_fall_back_to_null() {
        let v = json!({
            "field_diffs": [{"action_id": 9, "field_path": "receipt.x"}],
            "action_count_delta": 0,
        });
        let r = SessionDiff.render(&v, &cfg()).unwrap();
        assert!(r.text.contains("- receipt.x: null"), "got:\n{}", r.text);
        assert!(r.text.contains("+ receipt.x: null"), "got:\n{}", r.text);
    }

    /// Head line is unaffected by the bug and must stay stable.
    #[test]
    fn head_line_reports_count_and_delta() {
        let v = json!({"field_diffs": [], "action_count_delta": 3});
        let r = SessionDiff.render(&v, &cfg()).unwrap();
        assert_eq!(r.text, "0 field diffs, action_count_delta=3");
    }
}
