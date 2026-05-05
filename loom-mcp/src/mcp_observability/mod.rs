pub mod mcp_observability;
pub use mcp_observability::*;

#[cfg(test)]
mod interface_tests;

use std::sync::Arc;
use std::sync::OnceLock;

static SUBSCRIBER_INIT: OnceLock<()> = OnceLock::new();

impl McpObservability {
    pub fn new(redact_vault: bool) -> Arc<Self> {
        Arc::new(Self { redact_vault })
    }

    pub fn span_request_start(
        &self,
        request_id: impl Into<String>,
        mcp_method: &'static str,
        tool_name: Option<String>,
    ) -> RequestSpan {
        let span = RequestSpan {
            request_id: request_id.into(),
            mcp_method,
            tool_name,
            started_at: std::time::Instant::now(),
        };
        tracing::info!(
            request_id = %span.request_id,
            mcp_method = span.mcp_method,
            tool_name = ?span.tool_name,
            "mcp_request_start"
        );
        span
    }

    pub fn span_request_end(&self, span: RequestSpan, outcome: Outcome, error_code: Option<&str>) {
        let latency_us = duration_to_us(span.started_at.elapsed());
        tracing::info!(
            request_id = %span.request_id,
            mcp_method = span.mcp_method,
            tool_name = ?span.tool_name,
            outcome = ?outcome,
            error_code = ?error_code,
            latency_us = latency_us,
            "mcp_request_end"
        );
    }

    pub fn info(&self, msg: &str, fields: serde_json::Value) {
        tracing::info!(msg = msg, fields = %fields);
    }

    pub fn warn(&self, msg: &str, fields: serde_json::Value) {
        tracing::warn!(msg = msg, fields = %fields);
    }

    pub fn error(&self, msg: &str, fields: serde_json::Value) {
        tracing::error!(msg = msg, fields = %fields);
    }

    pub fn redact_arguments(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        if self.redact_vault && REDACTED_TOOL_NAMES.contains(&tool_name) {
            serde_json::Value::String("[REDACTED]".to_string())
        } else {
            arguments
        }
    }
}

/// Initialise the global tracing subscriber (stderr JSON). Idempotent.
pub fn init_subscriber(redact_vault: bool) -> Arc<McpObservability> {
    SUBSCRIBER_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .json()
            .try_init();
    });
    McpObservability::new(redact_vault)
}
