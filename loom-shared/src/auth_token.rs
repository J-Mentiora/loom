//! `auth_token` — opaque `HelloToken` shared between the daemon
//! (issuer) and CLI / MCP (consumers).
//!
//! The daemon generates a fresh random token on each start (see
//! `loom-daemon::generate_hello_token`) and writes it to its
//! per-user data dir; on-disk persistence between daemon restarts is
//! intentionally not provided.

/// Opaque HELLO-token shared between the daemon (issuer) and the
/// CLI / MCP (consumers). Treat as a credential — never log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloToken(String);

impl HelloToken {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Borrow the raw string for transport. Avoid printing.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}
