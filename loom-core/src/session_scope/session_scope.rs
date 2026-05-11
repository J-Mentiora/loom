//! Structured-concurrency primitive that owns every session-lifetime task.
//!
//! Background: the daemon previously leaked tokio tasks because shim teardown
//! and circuit-breaker eviction used fire-and-forget `tokio::spawn` calls,
//! dropping the `JoinHandle` at the spawn site. After 4-6 sequential client
//! sessions, accumulated never-joined tasks saturated the tokio runtime and
//! new `session.create` requests stalled. `SessionScope` is the
//! replacement: a `CancellationToken` paired with a `JoinSet` that owns
//! every task spawned through it. On `drain`, the token is cancelled, the
//! `JoinSet` is awaited with a grace period, and survivors are force-aborted.
//!
//! ## Locking discipline
//!
//! The `JoinSet` lives behind a `parking_lot::Mutex` (sync) so `spawn` is
//! callable from non-async contexts (matches the existing budget-timer
//! pattern in `Session`). `drain` takes ownership of the `JoinSet` via
//! `mem::take` before awaiting anything, so the mutex is never held across
//! an await point. Concurrent `spawn` callers are short-circuited by the
//! double-checked `is_cancelled()` guard.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Outcome of [`SessionScope::drain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Every task observed the cancellation token and exited within `grace`.
    Clean,
    /// `count` tasks did not honor the cancellation token before `grace`
    /// expired and were force-aborted via `JoinHandle::abort()`. The drain
    /// still returns within `grace + join-of-aborted` so the daemon stays
    /// responsive.
    AbortedSurvivors { count: usize },
}

/// Owns every task spawned for a given session's lifetime and provides a
/// cooperative-then-abort `drain` for the close path.
///
/// Lifetime contract: lives as long as a `Session`. `drain` MUST be called
/// from outside the scope — calling it from inside a task that the scope
/// owns will deadlock waiting for that task to complete.
pub struct SessionScope {
    cancel: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
}

impl SessionScope {
    /// Construct a fresh scope wrapped in `Arc` for shared ownership.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
        })
    }

    /// Child cancellation token. Surface code can `select!` on
    /// `child_token().cancelled()` to short-circuit on cancellation without
    /// being able to cancel the parent scope.
    pub fn child_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    /// Cancel the scope's token immediately. Cooperative children observing
    /// `child_token()` will see the cancellation and can exit. The actual
    /// task drain still requires a separate `drain().await` call — this is
    /// just the cooperative signal.
    ///
    /// Idempotent.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// `true` once the scope has been cancelled (drain initiated or `cancel`
    /// called).
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Spawn a future as a child of this scope.
    ///
    /// Must be called from within a tokio runtime context (same constraint
    /// as bare `tokio::spawn`). If the scope has already been cancelled, the
    /// future is dropped and no task is spawned. Callers that need to know
    /// whether the future took ownership should check `is_cancelled()`
    /// beforehand.
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.cancel.is_cancelled() {
            return;
        }
        let mut tasks = self.tasks.lock();
        // Re-check after acquiring the lock: drain may have begun between
        // our first check and now.
        if self.cancel.is_cancelled() {
            return;
        }
        tasks.spawn(fut);
    }

    /// Cooperative-then-abort drain.
    ///
    /// 1. Cancel the scope's token so children observing it can exit cleanly.
    /// 2. Wait up to `grace` for in-flight children to finish.
    /// 3. Abort any survivors and wait for the JoinSet to fully drain.
    ///
    /// After this returns, no further tasks can be spawned on the scope
    /// (`spawn` becomes a no-op). MUST NOT be called from inside a task
    /// owned by this scope (deadlock).
    pub async fn drain(&self, grace: Duration) -> DrainOutcome {
        // Cancel BEFORE taking the JoinSet so concurrent spawns short-circuit.
        self.cancel.cancel();

        let started = std::time::Instant::now();
        let initial_count;

        // Take ownership of the JoinSet so the rest of the drain doesn't
        // hold a lock across awaits.
        let mut tasks = {
            let mut guard = self.tasks.lock();
            initial_count = guard.len();
            std::mem::take(&mut *guard)
        };

        // Cooperative phase.
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            if tasks.is_empty() {
                tracing::debug!(
                    metric = "loom_daemon_scope_drain",
                    outcome = "clean",
                    initial = initial_count,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                );
                return DrainOutcome::Clean;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, tasks.join_next()).await {
                Ok(Some(_)) => continue,
                Ok(None) => {
                    tracing::debug!(
                        metric = "loom_daemon_scope_drain",
                        outcome = "clean",
                        initial = initial_count,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                    );
                    return DrainOutcome::Clean;
                }
                Err(_) => break,
            }
        }

        // Force-abort phase.
        let survivors = tasks.len();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        tracing::warn!(
            metric = "loom_daemon_scope_drain",
            outcome = "aborted",
            initial = initial_count,
            survivors,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "session scope drained with force-abort survivors"
        );
        DrainOutcome::AbortedSurvivors { count: survivors }
    }
}
