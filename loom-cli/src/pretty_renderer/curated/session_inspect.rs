use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

/// `loom session inspect` — handler pre-extracts `manifest_summary`, so the
/// receipt this renderer sees IS the manifest summary object. We don't
/// know its shape (depends on session); render a header line and let the
/// tail-block printer surface every field.
pub struct SessionInspect;

impl CuratedRenderer for SessionInspect {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let _ = value;
        let head = ansi::paint(
            "manifest summary",
            ansi::CYAN,
            cfg.stdout_color_enabled,
        );
        Ok(RenderedReceipt {
            text: head,
            consumed_keys: HashSet::new(),
        })
    }
    // No quiet_id: a manifest_summary projection has no top-level id (D-19).
}
