//! Vault ↔ backend boundary adapter — per plan amendment A-W5.1 (FND-0045).
//!
//! Wraps a sync `Arc<dyn KeychainAccess>` and re-exposes the **same** sync
//! `KeychainAccess` trait, but each method runs the underlying call on
//! `tokio::task::spawn_blocking` under a per-method `tokio::time::timeout`.
//! The `LocalVault` holds an `Arc<BlockingKeychain>` so the adapter is
//! the SINGLE owning module for the `spawn_blocking` convention — every
//! other site in the workspace continues to use the pre-existing
//! `block_in_place` + `Handle::block_on` pattern (see
//! `loom-daemon/src/lib.rs#L799` and `loom-rpc/src/host_service_adapter/mod.rs`).
//! Centralising the convention in one type makes the boundary visible
//! and reviewable.
//!
//! Timeouts (per plan §6 W5 + D28):
//! - `get` — 30s (keychain may prompt for unlock on first access)
//! - `set` / `delete` / `list` — 5s (non-prompting ops)
//!
//! Runtime detection: when called from a tokio runtime context, the
//! adapter dispatches via `spawn_blocking` + `timeout`. When called from
//! a plain sync context (e.g. the unit tests in this crate, which use
//! `InMemoryKeychain` directly), it falls back to a direct synchronous
//! call so tests don't have to opt into a runtime they don't need. The
//! timeout guarantee is therefore enforced only in the runtime-present
//! path — which is the only path that needs it (`InMemoryKeychain` never
//! blocks, and production daemons always have a runtime).

use loom_keychain::{KeychainAccess, KeychainError, KeychainErrorKind};
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

/// Per-op timeouts. Defaults match plan §6 W5 / D28.
#[derive(Debug, Clone, Copy)]
pub struct KeychainTimeouts {
    pub get: Duration,
    pub set: Duration,
    pub delete: Duration,
    pub list: Duration,
}

impl Default for KeychainTimeouts {
    fn default() -> Self {
        Self {
            get: Duration::from_secs(30),
            set: Duration::from_secs(5),
            delete: Duration::from_secs(5),
            list: Duration::from_secs(5),
        }
    }
}

/// Adapter wrapping an inner `KeychainAccess` impl with per-method timeouts
/// and `spawn_blocking` isolation when a tokio runtime is present.
pub struct BlockingKeychain {
    inner: Arc<dyn KeychainAccess>,
    timeouts: KeychainTimeouts,
}

impl BlockingKeychain {
    pub fn new(inner: Arc<dyn KeychainAccess>) -> Self {
        Self {
            inner,
            timeouts: KeychainTimeouts::default(),
        }
    }

    pub fn with_timeouts(inner: Arc<dyn KeychainAccess>, timeouts: KeychainTimeouts) -> Self {
        Self { inner, timeouts }
    }

    /// Direct access to the wrapped backend. Used by `vault diagnose` (W6)
    /// for `last_keychain_error` reporting where we want to skip the
    /// timeout/audit wrappers.
    pub fn inner(&self) -> &Arc<dyn KeychainAccess> {
        &self.inner
    }

    pub fn timeouts(&self) -> &KeychainTimeouts {
        &self.timeouts
    }

    /// Run `f` either directly (no runtime) or via
    /// `block_in_place(spawn_blocking + timeout)` when a multi-thread
    /// runtime is current. `block_in_place` releases the worker thread
    /// for the duration of the blocking call so other tasks make
    /// progress.
    fn run_with_timeout<F, T>(
        &self,
        timeout: Duration,
        op: &'static str,
        f: F,
    ) -> Result<T, KeychainError>
    where
        F: FnOnce() -> Result<T, KeychainError> + Send + 'static,
        T: Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // Sync context: direct call. Timeout not enforced (only
            // `InMemoryKeychain`-style backends end up here in practice).
            return f();
        };

        let exec = move || {
            handle.block_on(async move {
                match tokio::time::timeout(timeout, tokio::task::spawn_blocking(f)).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(join_err)) => Err(KeychainError::internal_from_message(format!(
                        "{op}: spawn_blocking join error: {join_err}"
                    ))),
                    Err(_elapsed) => Err(KeychainError::new(
                        KeychainErrorKind::TimedOut,
                        format!("{op} exceeded per-op timeout {timeout:?}"),
                    )),
                }
            })
        };

        // block_in_place requires the multi-thread runtime flavour; if
        // someone uses BlockingKeychain under a current-thread runtime
        // we'd panic. Detect once and degrade to direct dispatch so the
        // single-thread test path stays usable.
        match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(exec),
            _ => exec(),
        }
    }
}

impl KeychainAccess for BlockingKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        let inner = self.inner.clone();
        let label = label.to_owned();
        self.run_with_timeout(self.timeouts.get, "keychain.get_secret", move || {
            inner.get_secret(&label)
        })
    }

    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        let inner = self.inner.clone();
        let label = label.to_owned();
        self.run_with_timeout(self.timeouts.set, "keychain.set_secret", move || {
            inner.set_secret(&label, secret)
        })
    }

    fn delete_secret(&self, label: &str) -> Result<(), KeychainError> {
        let inner = self.inner.clone();
        let label = label.to_owned();
        self.run_with_timeout(self.timeouts.delete, "keychain.delete_secret", move || {
            inner.delete_secret(&label)
        })
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        let inner = self.inner.clone();
        self.run_with_timeout(self.timeouts.list, "keychain.list_labels", move || {
            inner.list_labels()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_keychain::InMemoryKeychain;

    #[test]
    fn passthrough_round_trip_without_runtime() {
        // Sync test (no tokio runtime): adapter falls back to direct call.
        let mem = Arc::new(InMemoryKeychain::new()) as Arc<dyn KeychainAccess>;
        let blocking = BlockingKeychain::new(mem);
        blocking
            .set_secret("a", Zeroizing::new(b"x".to_vec()))
            .expect("set");
        let v = blocking.get_secret("a").expect("get");
        assert_eq!(&v[..], b"x");
        blocking.delete_secret("a").expect("delete");
        let labels = blocking.list_labels().expect("list");
        assert!(labels.is_empty());
    }

    #[test]
    fn default_timeouts_match_plan_d28() {
        let t = KeychainTimeouts::default();
        assert_eq!(t.get, Duration::from_secs(30));
        assert_eq!(t.set, Duration::from_secs(5));
        assert_eq!(t.delete, Duration::from_secs(5));
        assert_eq!(t.list, Duration::from_secs(5));
    }
}
