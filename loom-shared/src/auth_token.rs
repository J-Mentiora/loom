//! `auth_token` — `HelloToken` for daemon HELLO-token issuance + CLI/MCP consumption.
//!
//! Phase 5.4 stub: minimal type + placeholder file IO. Phase 6 wires
//! atomic-rename, fs perms 0600, and rotation per loom-rpc auth_middleware.

use crate::error_format::{LoomError, LoomErrorCode};
use std::path::Path;

/// Opaque HELLO-token shared between daemon (issuer) and CLI/MCP
/// (consumers). Treat as a credential — never log.
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

/// Read the HELLO-token from `path`, or generate a fresh random token,
/// persist it (mode 0600), and return.
///
/// Phase 5.4: stub — Phase 6 wires `ring::rand::SecureRandom` + atomic
/// write + fs perms + tracing.
pub fn read_or_generate(path: &Path) -> Result<HelloToken, LoomError> {
    let _ = path;
    Err(LoomError::new(
        LoomErrorCode::Internal,
        "HelloToken::read_or_generate: Phase 5.4 stub — Phase 6 implements",
    ))
}
