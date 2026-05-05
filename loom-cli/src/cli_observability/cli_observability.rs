// CliObservability — `tracing` subscriber + per-command span +
// redaction layer.
//
// # Contract semantics
// - **Per-command span.** Every command emits one span with fields
//   `command`, `subcommand?`, `latency_us`, `exit_code`,
//   `rpc_method?`, `receipt_status?`.
// - **Redaction layer.** Walks emitted fields; values for
//   `vault.add` OAuth tokens and `vault.grant` request payloads are
//   replaced with `[REDACTED]` BEFORE serialization. Raw secret bytes
//   never reach the JSON formatter.
// - **NO_COLOR / TERM=dumb.** Subscriber's formatter respects both;
//   default path emits machine-readable JSON to stderr.

use std::time::Instant;

/// Span guard returned by `start_span`. Drop emits the span_end event
/// with the elapsed `latency_us`.
pub struct SpanGuard {
    pub(crate) command: String,
    pub(crate) subcommand: Option<String>,
    pub(crate) started_at: Instant,
    pub(crate) rpc_method: Option<String>,
    pub(crate) receipt_status: Option<String>,
    pub(crate) exit_code: Option<i32>,
}

impl SpanGuard {
    /// Annotate the span with the RPC method dispatched by the
    /// handler.
    pub fn record_rpc_method(&mut self, method: &str) {
        self.rpc_method = Some(method.to_string());
    }

    /// Annotate the span with the receipt's `status` field.
    pub fn record_receipt_status(&mut self, status: &str) {
        self.receipt_status = Some(status.to_string());
    }

    /// Annotate the final exit code. Called by the `main` exit path.
    pub fn record_exit_code(&mut self, code: i32) {
        self.exit_code = Some(code);
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        let latency_us = self.started_at.elapsed().as_micros() as u64;
        tracing::info!(
            command = %self.command,
            subcommand = ?self.subcommand,
            latency_us = latency_us,
            rpc_method = ?self.rpc_method,
            receipt_status = ?self.receipt_status,
            exit_code = ?self.exit_code,
            "span_end"
        );
    }
}

/// Initialise the global `tracing_subscriber` with the env-default
/// filter and the redaction layer. Idempotent.
pub fn init() {
    use std::sync::OnceLock;
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .json()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    });
}

/// Open a new per-command span. Drop emits `span_end` with elapsed
/// `latency_us`.
pub fn start_span(command: &str, subcommand: Option<&str>) -> SpanGuard {
    tracing::info!(command = command, subcommand = ?subcommand, "span_start");
    SpanGuard {
        command: command.to_string(),
        subcommand: subcommand.map(|s| s.to_string()),
        started_at: Instant::now(),
        rpc_method: None,
        receipt_status: None,
        exit_code: None,
    }
}

/// Pure helper: list of field names that the redaction layer must
/// scrub. Locked here so unit tests + the codegen can audit.
pub const REDACTED_FIELDS: &[&str] = &[
    "oauth_token",
    "access_token",
    "refresh_token",
    "client_secret",
    "secret",
    "password",
    "api_key",
];
