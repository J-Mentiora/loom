use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct SessionClose;

impl CuratedRenderer for SessionClose {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        verb_session(value, cfg, "closed")
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

pub(super) fn verb_session(
    value: &Value,
    cfg: &CliConfig,
    verb: &str,
) -> Result<RenderedReceipt, CliError> {
    let id = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            CliError::Internal(format!("session.{} receipt missing session_id", verb))
        })?;
    let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("ok");
    let id_paint = ansi::paint(
        &format!("session={}", id),
        if status == "error" {
            ansi::RED
        } else {
            ansi::GREEN
        },
        cfg.stdout_color_enabled,
    );
    let verb_paint = ansi::paint(verb, ansi::DIM, cfg.stdout_color_enabled);
    let mut consumed = HashSet::new();
    consumed.insert("session_id".to_string());
    Ok(RenderedReceipt {
        text: format!("{} {}", id_paint, verb_paint),
        consumed_keys: consumed,
    })
}
