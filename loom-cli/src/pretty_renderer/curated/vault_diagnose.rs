use super::{CuratedRenderer, RenderedReceipt};
use crate::cli_config::CliConfig;
use crate::pretty_renderer::ansi;
use crate::CliError;
use serde_json::Value;
use std::collections::HashSet;

pub struct VaultDiagnose;

impl CuratedRenderer for VaultDiagnose {
    fn render(&self, value: &Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError> {
        let backend = value
            .get("backend")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let service_id = value
            .get("service_id")
            .and_then(|v| v.as_str())
            .unwrap_or("loom");
        let label_count = value
            .get("label_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let init_ok = value
            .get("init_status")
            .and_then(|v| v.as_str())
            .map(|s| s == "ok")
            .unwrap_or_else(|| {
                value
                    .get("init_status")
                    .and_then(|v| v.get("ok"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
            });

        let mut lines = Vec::with_capacity(8);
        lines.push(format!("Keychain backend: {backend}"));
        lines.push(format!("Service ID:       {service_id}"));
        if init_ok {
            lines.push(ansi::paint(
                "Init status:      ok",
                ansi::GREEN,
                cfg.stdout_color_enabled,
            ));
        } else {
            let reason = value
                .get("init_status")
                .and_then(|v| v.get("error"))
                .and_then(|v| v.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    value
                        .get("init_status")
                        .and_then(|v| v.get("reason"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)")
                });
            lines.push(ansi::paint(
                &format!("Init status:      ERROR — {reason}"),
                ansi::RED,
                cfg.stdout_color_enabled,
            ));
        }
        lines.push(format!("Stored credentials: {label_count}"));

        if let Some(last) = value.get("last_keychain_error") {
            if !last.is_null() {
                let kind = last.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                let when = last.get("when_ts").and_then(|v| v.as_str()).unwrap_or("?");
                let hash = last
                    .get("internal_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(none)");
                lines.push(ansi::paint(
                    &format!("Last error:       {kind} @ {when}"),
                    ansi::YELLOW,
                    cfg.stdout_color_enabled,
                ));
                lines.push(ansi::paint(
                    &format!("  internal_hash:  {hash}  (paste into daemon log for diagnostic)"),
                    ansi::DIM,
                    cfg.stdout_color_enabled,
                ));
            }
        }

        let text = lines.join("\n");

        let mut consumed = HashSet::new();
        for k in [
            "backend",
            "service_id",
            "init_status",
            "label_count",
            "last_keychain_error",
        ] {
            consumed.insert(k.to_string());
        }
        Ok(RenderedReceipt {
            text,
            consumed_keys: consumed,
        })
    }
}
