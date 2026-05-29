//! `loom-keychain` — out-of-loom-core keychain access.
//!
//! Declares the `KeychainAccess` trait used by `loom-core::vault` to fetch /
//! persist credential bytes through the OS keychain. The trait is the seam
//! between the (platform-agnostic) vault and the (platform-specific) backend.
//!
//! As of v0.9.4 the trait covers the full lifecycle (`get/set/delete/list_labels`)
//! and three real implementations live here: `StubKeychain` (always-error;
//! kept for backend-init-failure tests), `InMemoryKeychain` (in-process test
//! double), and the platform-conditional `MacOsKeychain` / `LinuxKeychain`
//! (under `cfg(target_os = "...")`).
//!
//! `loom-core::vault` re-exports this crate's trait at
//! `loom_core::vault::KeychainAccess` for back-compat; there is one and only
//! one canonical definition of the trait — this one.

pub mod in_memory;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroizing;

pub use in_memory::InMemoryKeychain;

#[cfg(target_os = "macos")]
pub use macos::MacOsKeychain;

#[cfg(target_os = "linux")]
pub use linux::LinuxKeychain;

/// Error returned by all `KeychainAccess` methods. Carries a typed `kind`
/// (a unit-variant enum suitable for `matches!`) plus a `message` for
/// human display. The `Internal` kind additionally carries an opaque
/// `internal_hash` (SHA-256 hex of the original error string) so support
/// can correlate audits to daemon logs without the message itself ever
/// reaching the audit chain.
#[derive(Debug)]
pub struct KeychainError {
    kind: KeychainErrorKind,
    message: String,
    internal_hash: Option<String>,
}

/// Discriminator for `KeychainError`. Unit-variant enum so callers can
/// `matches!` without binding payloads. The `internal_hash` lives in the
/// parent `KeychainError`, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainErrorKind {
    /// Label has no entry in the backend.
    NotFound,
    /// Backend refused (e.g. user cancelled the OS auth prompt).
    Denied,
    /// Backend is not available (uninitialised, disconnected, IPC failure).
    Unavailable,
    /// Backend op exceeded its per-op time budget (see `BlockingKeychain`).
    TimedOut,
    /// Backend would have triggered a UI prompt (biometric / unlock) but
    /// `LOOM_KEYCHAIN_ALLOW_PROMPT=0` (default in non-TTY) refuses to block.
    NonInteractivePrompt,
    /// Anything else. The original message is hashed (SHA-256 hex) into
    /// `KeychainError::internal_hash` so support can correlate without
    /// the message reaching any persistent manifest.
    Internal,
}

impl KeychainError {
    /// Construct an error with a plain message. Use this for known kinds
    /// (`NotFound`, `Denied`, etc.) — the message is operator-visible and
    /// MUST NOT contain secret bytes.
    pub fn new(kind: KeychainErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            internal_hash: None,
        }
    }

    /// Construct an `Internal` error from an arbitrary message that may
    /// originate from a third-party backend and could theoretically contain
    /// sensitive data. Hashes the original message into `internal_hash`;
    /// the original is dropped immediately and never stored.
    pub fn internal_from_message(original: impl AsRef<str>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(original.as_ref().as_bytes());
        let hash = hex::encode(hasher.finalize());
        Self {
            kind: KeychainErrorKind::Internal,
            message: format!("Internal[hash={hash}]"),
            internal_hash: Some(hash),
        }
    }

    pub fn kind(&self) -> KeychainErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// SHA-256 hex of the original error message for `Internal` errors;
    /// `None` for all other kinds.
    pub fn internal_hash(&self) -> Option<&str> {
        self.internal_hash.as_deref()
    }
}

impl std::fmt::Display for KeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for KeychainError {}

/// External keychain interface. The IMPL is provided by platform-specific
/// backends (`MacOsKeychain`, `LinuxKeychain`) or test doubles
/// (`StubKeychain` — always-error; `InMemoryKeychain` — in-process map).
///
/// All methods are sync. Concurrency: `Send + Sync` so the same trait
/// object can be shared across the daemon's tokio runtime. Long-running ops
/// (anything that talks to the OS keychain) are wrapped at a higher layer
/// by the `BlockingKeychain` adapter in `loom-core::vault`, which runs
/// them on `tokio::task::spawn_blocking` with per-op timeouts.
pub trait KeychainAccess: Send + Sync {
    /// Fetch a secret. Returns `NotFound` if no entry for `label`.
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError>;

    /// Store / replace a secret. Trait-level upsert is silent (substitution
    /// path needs this for token rotation); the `loom vault add` CLI applies
    /// the fail-by-default safety on top.
    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError>;

    /// Idempotent delete: `NotFound` is mapped to `Ok(())` at the backend
    /// level.
    fn delete_secret(&self, label: &str) -> Result<(), KeychainError>;

    /// Enumerate labels stored in this keychain. NEVER reads secret bytes.
    fn list_labels(&self) -> Result<Vec<String>, KeychainError>;
}

/// Always-error stub. Used by backend-init-failure tests and as the
/// `LOOM_KEYCHAIN_BACKEND=stub` opt-in (NOT a silent default per D7).
/// All four methods return `Err`; the kinds are picked so that vault-layer
/// code paths exercise the typical OS-keychain error shapes.
pub struct StubKeychain;

impl KeychainAccess for StubKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::NotFound,
            format!("StubKeychain has no entry for label='{label}' (stub backend; persistence disabled)"),
        ))
    }

    fn set_secret(&self, _label: &str, _secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "StubKeychain does not persist secrets (stub backend)",
        ))
    }

    fn delete_secret(&self, _label: &str) -> Result<(), KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "StubKeychain does not persist secrets (stub backend)",
        ))
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "StubKeychain does not persist secrets (stub backend)",
        ))
    }
}

/// Runtime backend choice resolved from `LOOM_KEYCHAIN_BACKEND` env var.
/// `InMemory` is a daemon-level test-only escape hatch (per A-W4.3) — it
/// gives a hermetic e2e a real persistent backend without touching the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Always-error stub. Explicit `LOOM_KEYCHAIN_BACKEND=stub` only.
    Stub,
    /// In-process map. Explicit `LOOM_KEYCHAIN_BACKEND=in_memory` only;
    /// intended for daemon-level integration tests.
    InMemory,
    /// macOS Security Framework backend (per `target_os = "macos"`).
    MacOs,
    /// Linux Secret Service backend (per `target_os = "linux"`).
    Linux,
}

/// Resolved at daemon startup; passed to `select_keychain`.
#[derive(Debug, Clone)]
pub struct KeychainConfig {
    pub backend: BackendChoice,
    /// `false` in non-TTY/daemon mode (default); `true` in interactive
    /// shells. Backends use this to gate OS prompts that would block the
    /// daemon (biometric / unlock dialogs).
    pub allow_prompt: bool,
    /// Hardcoded `"loom"` per D36 in v0.9.4. Reserved for a future
    /// follow-up that may make this configurable.
    pub service_id: &'static str,
}

impl Default for KeychainConfig {
    fn default() -> Self {
        Self {
            backend: default_platform_backend(),
            allow_prompt: false,
            service_id: "loom",
        }
    }
}

fn default_platform_backend() -> BackendChoice {
    #[cfg(target_os = "macos")]
    {
        BackendChoice::MacOs
    }
    #[cfg(target_os = "linux")]
    {
        BackendChoice::Linux
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Other platforms (Windows etc.) get the stub — they need to opt in
        // explicitly via env var. select_keychain() will hard-fail otherwise.
        BackendChoice::Stub
    }
}

/// Resolve a backend from config. Hard-fails (returns Err) when the
/// requested backend cannot be constructed; the daemon translates this
/// into a non-zero exit per D7 ("no silent stub fallback").
pub fn select_keychain(cfg: &KeychainConfig) -> Result<Arc<dyn KeychainAccess>, KeychainError> {
    match cfg.backend {
        BackendChoice::Stub => Ok(Arc::new(StubKeychain)),
        BackendChoice::InMemory => Ok(Arc::new(InMemoryKeychain::with_service_id(cfg.service_id))),
        #[cfg(target_os = "macos")]
        BackendChoice::MacOs => Ok(Arc::new(macos::MacOsKeychain::new(
            cfg.service_id,
            cfg.allow_prompt,
        )?)),
        #[cfg(not(target_os = "macos"))]
        BackendChoice::MacOs => Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "macOS Security Framework backend is not available on this target",
        )),
        #[cfg(target_os = "linux")]
        BackendChoice::Linux => Ok(Arc::new(linux::LinuxKeychain::new(
            cfg.service_id,
            cfg.allow_prompt,
        )?)),
        #[cfg(not(target_os = "linux"))]
        BackendChoice::Linux => Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "Linux Secret Service backend is not available on this target",
        )),
    }
}
