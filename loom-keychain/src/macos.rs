//! macOS Security Framework backend.
//!
//! **W2 stub** — full implementation lands in W2 (Phase 3 Iter 2).
//! Provides the `MacOsKeychain` type so `select_keychain` can construct it
//! on `cfg(target_os = "macos")`.

use crate::{KeychainAccess, KeychainError, KeychainErrorKind};
use zeroize::Zeroizing;

pub struct MacOsKeychain {
    service_id: &'static str,
    allow_prompt: bool,
}

impl MacOsKeychain {
    pub fn new(service_id: &'static str, allow_prompt: bool) -> Result<Self, KeychainError> {
        Ok(Self {
            service_id,
            allow_prompt,
        })
    }

    pub fn service_id(&self) -> &'static str {
        self.service_id
    }

    pub fn allow_prompt(&self) -> bool {
        self.allow_prompt
    }
}

impl KeychainAccess for MacOsKeychain {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "MacOsKeychain not yet implemented (W2 placeholder; v0.9.4 Phase 3 Iter 2)",
        ))
    }

    fn set_secret(&self, _label: &str, _secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "MacOsKeychain not yet implemented (W2 placeholder; v0.9.4 Phase 3 Iter 2)",
        ))
    }

    fn delete_secret(&self, _label: &str) -> Result<(), KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "MacOsKeychain not yet implemented (W2 placeholder; v0.9.4 Phase 3 Iter 2)",
        ))
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "MacOsKeychain not yet implemented (W2 placeholder; v0.9.4 Phase 3 Iter 2)",
        ))
    }
}
