//! `logging` — `tracing_subscriber` setup with optional secret-redaction
//! layer (vault-aware) per BC observability requirements.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize global tracing subscriber.
///
/// `redaction_enabled=true` installs a layer that strips known
/// secret-bearing field names from log records (vault labels, raw
/// secrets, hello-token bytes). Phase 6 implementations replace the
/// stub redactor with the vault-aware variant.
pub fn init_tracing(redaction_enabled: bool) {
    let _ = redaction_enabled; // Phase 6 wires the redaction layer.

    let filter = EnvFilter::try_from_env("LOOM_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .expect("default EnvFilter");

    let fmt_layer = fmt::layer().with_target(true);

    // Idempotent: ignore the error if the global subscriber is already set
    // (tests + main can both call this).
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init();
}

/// Field names that the redaction layer (Phase 6) will strip.
pub const REDACTED_FIELDS: &[&str] = &[
    "secret",
    "vault_secret",
    "hello_token",
    "auth_token",
    "passphrase",
    "api_key",
];
