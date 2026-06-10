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
//! Runtime detection: when called from a multi-thread tokio runtime, the
//! adapter dispatches via `block_in_place(spawn_blocking + timeout)`.
//! Under a current-thread runtime (default `#[tokio::test]`, embedders)
//! both `block_in_place` and `Handle::block_on` panic inside the async
//! context, so the adapter instead runs the call on a dedicated OS
//! thread bounded by `recv_timeout` — same isolation + timeout
//! semantics, no runtime entanglement. When called from a plain sync
//! context (e.g. the unit tests in this crate, which use
//! `InMemoryKeychain` directly), it falls back to a direct synchronous
//! call so tests don't have to opt into a runtime they don't need. The
//! timeout guarantee is therefore enforced only in the runtime-present
//! paths — which are the only paths that need it (`InMemoryKeychain`
//! never blocks, and production daemons always have a runtime).

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

    /// Run `f` either directly (no runtime), via
    /// `block_in_place(spawn_blocking + timeout)` when a multi-thread
    /// runtime is current (`block_in_place` releases the worker thread
    /// for the duration of the blocking call so other tasks make
    /// progress), or on a dedicated OS thread bounded by `recv_timeout`
    /// when a current-thread runtime is current (where both
    /// `block_in_place` and `Handle::block_on` would panic).
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

        // block_in_place requires the multi-thread runtime flavour; if
        // someone uses BlockingKeychain under a current-thread runtime
        // we'd panic. Detect once and degrade to a dedicated-thread
        // dispatch so the single-thread test path stays usable.
        match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(move || {
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
            }),
            // Current-thread (or any future non-multi-thread) flavour:
            // `block_in_place` AND `Handle::block_on` both panic when
            // invoked from within an async execution context on this
            // flavour (e.g. a default `#[tokio::test]`), so neither is a
            // safe degradation. Instead, dispatch on a dedicated OS thread
            // and bound it with `recv_timeout` — the same blocking
            // isolation + per-op timeout semantics as the multi-thread
            // path, with no runtime entanglement. On timeout the worker is
            // abandoned (it exits when the blocking call returns; its send
            // fails silently), exactly like a timed-out `spawn_blocking`
            // task.
            _ => {
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new()
                    .name(format!("loom-keychain-{op}"))
                    .spawn(move || {
                        let _ = tx.send(f());
                    });
                if let Err(spawn_err) = spawned {
                    return Err(KeychainError::internal_from_message(format!(
                        "{op}: keychain worker thread spawn failed: {spawn_err}"
                    )));
                }
                match rx.recv_timeout(timeout) {
                    Ok(r) => r,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(KeychainError::new(
                        KeychainErrorKind::TimedOut,
                        format!("{op} exceeded per-op timeout {timeout:?}"),
                    )),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        Err(KeychainError::internal_from_message(format!(
                            "{op}: keychain worker thread panicked before replying"
                        )))
                    }
                }
            }
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

    // Regression (audit 2026-06-10): the non-MultiThread arm used to call
    // `Handle::block_on` from within the async context, which PANICS on a
    // current-thread runtime (the default `#[tokio::test]` flavour). The
    // fallback must degrade to a dedicated-thread dispatch instead.
    #[tokio::test]
    async fn current_thread_runtime_round_trip_does_not_panic() {
        let mem = Arc::new(InMemoryKeychain::new()) as Arc<dyn KeychainAccess>;
        let blocking = BlockingKeychain::new(mem);
        blocking
            .set_secret("ct", Zeroizing::new(b"y".to_vec()))
            .expect("set under current-thread runtime");
        let v = blocking
            .get_secret("ct")
            .expect("get under current-thread runtime");
        assert_eq!(&v[..], b"y");
        blocking
            .delete_secret("ct")
            .expect("delete under current-thread runtime");
        let labels = blocking
            .list_labels()
            .expect("list under current-thread runtime");
        assert!(labels.is_empty());
    }

    /// Backend whose `get_secret` blocks long enough to trip the per-op
    /// timeout. Other ops answer immediately.
    struct SlowGetKeychain {
        block_for: Duration,
    }

    impl KeychainAccess for SlowGetKeychain {
        fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
            std::thread::sleep(self.block_for);
            Ok(Zeroizing::new(vec![1u8]))
        }
        fn set_secret(
            &self,
            _label: &str,
            _secret: Zeroizing<Vec<u8>>,
        ) -> Result<(), KeychainError> {
            Ok(())
        }
        fn delete_secret(&self, _label: &str) -> Result<(), KeychainError> {
            Ok(())
        }
        fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
            Ok(vec![])
        }
    }

    // The dedicated-thread fallback must still enforce the per-op timeout
    // (the multi-thread path enforces it via tokio::time::timeout; the
    // current-thread path via mpsc recv_timeout).
    #[tokio::test]
    async fn current_thread_runtime_enforces_per_op_timeout() {
        let slow = Arc::new(SlowGetKeychain {
            block_for: Duration::from_millis(500),
        }) as Arc<dyn KeychainAccess>;
        let timeouts = KeychainTimeouts {
            get: Duration::from_millis(50),
            ..KeychainTimeouts::default()
        };
        let blocking = BlockingKeychain::with_timeouts(slow, timeouts);
        let err = blocking
            .get_secret("slow")
            .expect_err("blocked get must time out, not hang or panic");
        assert_eq!(
            err.kind(),
            loom_keychain::KeychainErrorKind::TimedOut,
            "typed TimedOut error from the dedicated-thread fallback"
        );
    }
}
