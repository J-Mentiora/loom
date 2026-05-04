//! `loom-keychain` — feature-gated, out-of-loom-core keychain access.
//!
//! Implements the `KeychainAccess` trait declared by
//! `loom_core::vault::KeychainAccess`. Loom-core never imports
//! platform-specific symbols; this crate is the seam (BC-CORE-02 +
//! SR-CORE-01).
//!
//! Phase 5.4: stub `LoomError`-aware error type + a passphrase-backed
//! placeholder impl that Phase 6 replaces with macOS Security Framework
//! / Linux Secret Service / Windows Credential Manager backends.

use zeroize::Zeroizing;

#[derive(Debug)]
pub struct KeychainError {
    pub kind: KeychainErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub enum KeychainErrorKind {
    NotFound,
    Denied,
    Unavailable,
    Internal,
}

impl std::fmt::Display for KeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for KeychainError {}

/// External keychain interface. The IMPL of this trait is provided by
/// Phase 6 platform backends; loom-core's `Vault` constructs an
/// `Arc<dyn KeychainAccess>` from one of these backends at startup.
pub trait KeychainAccess: Send + Sync {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError>;
}

/// Stub backend used until Phase 6 wires real platform stores. Always
/// returns `NotFound`. Useful in tests + scaffold smoke runs.
pub struct StubKeychain;

impl KeychainAccess for StubKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        Err(KeychainError {
            kind: KeychainErrorKind::NotFound,
            message: format!("StubKeychain has no entry for label='{label}' (Phase 5.4 stub)"),
        })
    }
}
