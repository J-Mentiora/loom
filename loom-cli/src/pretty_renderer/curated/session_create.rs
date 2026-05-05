use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// `loom session create` — `<GREEN>session=<id></GREEN> <DIM>created</DIM>`.
pub struct SessionCreate;

impl CuratedRenderer for SessionCreate {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::Internal("session.create receipt missing session_id".into()))?;
        let head = format!(
            "{} {}",
            ansi::paint(
                &format!("session={}", id),
                ansi::GREEN,
                cfg.stdout_color_enabled
            ),
            ansi::paint("created", ansi::DIM, cfg.stdout_color_enabled),
        );
        let mut consumed = HashSet::new();
        consumed.insert("session_id".to_string());
        Ok(RenderedReceipt {
            text: head,
            consumed_keys: consumed,
        })
    }

    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
