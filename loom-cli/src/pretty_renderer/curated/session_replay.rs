use super::session_close::verb_session;
use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::CliError;
use serde_json::Value;

pub struct SessionReplay;

impl CuratedRenderer for SessionReplay {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        verb_session(value, cfg, "replayed")
    }
    fn quiet_id(&self, value: &Value) -> Option<String> {
        value.get("session_id").and_then(|v| v.as_str()).map(str::to_string)
    }
}
