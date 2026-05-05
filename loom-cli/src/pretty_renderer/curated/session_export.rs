use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// `session.export` server response is `ExportInfo { artifact_ref }`.
/// Note: the handler subsequently fetches and writes binary bytes via
/// `content.get`; this renderer only handles the announcement receipt.
pub struct SessionExport;

impl CuratedRenderer for SessionExport {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let r = value
            .get("artifact_ref")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let text = ansi::paint(
            &format!("artifact_ref={}", r),
            ansi::CYAN,
            cfg.stdout_color_enabled,
        );
        let mut consumed = HashSet::new();
        consumed.insert("artifact_ref".to_string());
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value
            .get("artifact_ref")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
