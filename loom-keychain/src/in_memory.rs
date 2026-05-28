//! `InMemoryKeychain` — a process-internal test double.
//!
//! Distinct from `StubKeychain` (always-error). `InMemoryKeychain` is a
//! correct implementation of the trait backed by a `Mutex<HashMap>`; it
//! round-trips bytes and satisfies the vault layer's contract end-to-end.
//!
//! Used by:
//! - vault-layer unit tests in `loom-core::vault::impl_local`
//! - the hermetic e2e at `loom-cli/tests/keychain_e2e_hermetic.rs`
//!   (selected via `LOOM_KEYCHAIN_BACKEND=in_memory`)
//! - any other test that needs a working keychain without touching the OS

use std::collections::HashMap;
use std::sync::Mutex;
use zeroize::Zeroizing;

use crate::{KeychainAccess, KeychainError, KeychainErrorKind};

/// In-process keychain double. Scoped by `service_id` so tests can exercise
/// cross-service namespace isolation (matches the platform backends'
/// `(service_id, account)` identity model per D11).
pub struct InMemoryKeychain {
    service_id: String,
    store: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl InMemoryKeychain {
    /// Default service_id is `"loom"` (matches the daemon's default).
    pub fn new() -> Self {
        Self::with_service_id("loom")
    }

    pub fn with_service_id(service_id: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            store: Mutex::new(HashMap::new()),
        }
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }
}

impl Default for InMemoryKeychain {
    fn default() -> Self {
        Self::new()
    }
}

impl KeychainAccess for InMemoryKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        let store = self.store.lock().expect("InMemoryKeychain mutex poisoned");
        store.get(label).cloned().ok_or_else(|| {
            KeychainError::new(
                KeychainErrorKind::NotFound,
                format!("InMemoryKeychain has no entry for label='{label}'"),
            )
        })
    }

    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        let mut store = self.store.lock().expect("InMemoryKeychain mutex poisoned");
        store.insert(label.to_string(), secret);
        Ok(())
    }

    fn delete_secret(&self, label: &str) -> Result<(), KeychainError> {
        let mut store = self.store.lock().expect("InMemoryKeychain mutex poisoned");
        store.remove(label);
        Ok(())
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        let store = self.store.lock().expect("InMemoryKeychain mutex poisoned");
        let mut labels: Vec<String> = store.keys().cloned().collect();
        labels.sort();
        Ok(labels)
    }
}
