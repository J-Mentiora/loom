use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// `loom session list` — table id (26) | status (10) | created_at (20)
/// per D-26. Empty list shows a friendly message per D-21.
pub struct SessionList;

const ID_W: usize = 26;
const STATUS_W: usize = 10;
const CREATED_W: usize = 20;

impl CuratedRenderer for SessionList {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let sessions = sessions_array(value);

        // D-21 empty-state.
        if sessions.is_empty() {
            return Ok(RenderedReceipt {
                text: ansi::paint("No sessions found.", ansi::DIM, cfg.stdout_color_enabled),
                consumed_keys: consumed_keys_for(value),
            });
        }

        let header = format!(
            "{:<id$}  {:<status$}  {:<created$}",
            "session_id",
            "status",
            "created_at",
            id = ID_W,
            status = STATUS_W,
            created = CREATED_W,
        );
        let mut out = ansi::paint(&header, ansi::DIM, cfg.stdout_color_enabled);
        out.push('\n');

        for s in &sessions {
            let id = truncate(field_str(s, "session_id"), ID_W);
            let status = truncate(field_str(s, "status"), STATUS_W);
            let created = truncate(field_str(s, "created_at"), CREATED_W);
            let row = format!(
                "{:<id$}  {:<status$}  {:<created$}",
                id,
                status,
                created,
                id = ID_W,
                status = STATUS_W,
                created = CREATED_W,
            );
            out.push_str(&row);
            out.push('\n');
        }
        // Trim trailing newline so the dispatcher / emit can append exactly one.
        if out.ends_with('\n') {
            out.pop();
        }

        Ok(RenderedReceipt {
            text: out,
            consumed_keys: consumed_keys_for(value),
        })
    }

    fn quiet_id(&self, value: &Value) -> Option<String> {
        let sessions = sessions_array(value);
        if sessions.is_empty() {
            return None;
        }
        let ids: Vec<String> = sessions
            .iter()
            .filter_map(|s| s.get("session_id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        if ids.is_empty() {
            None
        } else {
            Some(ids.join("\n"))
        }
    }
}

/// Daemon's session.list receipt is either an array (legacy) or an object
/// `{"sessions": [...]}` (newer). Tolerate both.
fn sessions_array(value: &Value) -> Vec<&Value> {
    if let Some(arr) = value.as_array() {
        return arr.iter().collect();
    }
    if let Some(arr) = value.get("sessions").and_then(|v| v.as_array()) {
        return arr.iter().collect();
    }
    Vec::new()
}

/// Mark the top-level wrapper key as consumed so the tail block doesn't
/// dump the entire array again. For the legacy array shape, we mark
/// nothing (object_iter returns no keys); the dispatcher's
/// compose_tail() short-circuits when value isn't an object.
fn consumed_keys_for(value: &Value) -> HashSet<String> {
    let mut s = HashSet::new();
    if value.get("sessions").is_some() {
        s.insert("sessions".to_string());
    }
    s
}

fn field_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
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

    #[test]
    fn empty_list_object_form_renders_friendly_message() {
        let v = json!({"sessions": []});
        let r = SessionList.render(&v, &cfg()).unwrap();
        assert_eq!(r.text, "No sessions found.");
    }

    #[test]
    fn empty_list_array_form_renders_friendly_message() {
        let v = json!([]);
        let r = SessionList.render(&v, &cfg()).unwrap();
        assert_eq!(r.text, "No sessions found.");
    }

    #[test]
    fn quiet_id_joins_with_newlines() {
        let v = json!({"sessions": [{"session_id": "01A"}, {"session_id": "01B"}]});
        assert_eq!(SessionList.quiet_id(&v), Some("01A\n01B".to_string()));
    }

    #[test]
    fn quiet_id_empty_list_returns_none() {
        let v = json!({"sessions": []});
        assert_eq!(SessionList.quiet_id(&v), None);
    }

    #[test]
    fn truncate_uses_ellipsis() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefg", 5), "abcd…");
    }
}
