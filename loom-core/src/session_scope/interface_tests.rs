use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::session_scope::{DrainOutcome, SessionScope};

#[tokio::test]
async fn drain_with_no_tasks_returns_clean_immediately() {
    let scope = SessionScope::new();
    let outcome = scope.drain(Duration::from_secs(2)).await;
    assert_eq!(outcome, DrainOutcome::Clean);
}

#[tokio::test]
async fn drain_with_cooperative_tasks_returns_clean_within_grace() {
    let scope = SessionScope::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let token = scope.child_token();
        let counter = Arc::clone(&counter);
        scope.spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
        });
    }

    let started = std::time::Instant::now();
    let outcome = scope.drain(Duration::from_secs(2)).await;
    let elapsed = started.elapsed();

    assert_eq!(outcome, DrainOutcome::Clean);
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert!(
        elapsed < Duration::from_millis(500),
        "cooperative drain should complete fast, took {elapsed:?}"
    );
}

#[tokio::test]
async fn drain_with_stubborn_tasks_force_aborts_after_grace() {
    let scope = SessionScope::new();

    for _ in 0..2 {
        scope.spawn(async {
            // Ignore cancellation; sleep for ages.
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
    }

    let grace = Duration::from_millis(100);
    let started = std::time::Instant::now();
    let outcome = scope.drain(grace).await;
    let elapsed = started.elapsed();

    assert_eq!(outcome, DrainOutcome::AbortedSurvivors { count: 2 });
    assert!(
        elapsed >= grace,
        "drain should wait at least the grace period, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "drain should not wait longer than grace + tiny join overhead, took {elapsed:?}"
    );
}

#[tokio::test]
async fn spawn_after_drain_is_no_op() {
    let scope = SessionScope::new();
    scope.drain(Duration::from_millis(50)).await;

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    scope.spawn(async move {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    // Give the runtime a chance — the task should NOT have run.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(scope.is_cancelled());
}

#[tokio::test]
async fn child_token_observes_scope_cancellation() {
    let scope = SessionScope::new();
    let token = scope.child_token();
    assert!(!token.is_cancelled());

    let scope_clone = Arc::clone(&scope);
    tokio::spawn(async move {
        scope_clone.drain(Duration::from_millis(50)).await;
    });

    tokio::time::timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("child token should be cancelled when scope is drained");
}

#[tokio::test]
async fn cancel_signals_children_without_draining() {
    let scope = SessionScope::new();
    let token = scope.child_token();
    scope.cancel();
    assert!(scope.is_cancelled());
    assert!(token.is_cancelled());
    // Spawn after cancel is a no-op.
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    scope.spawn(async move {
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}
