// LocalBudgetEnforcer implementation.
// Enforces:
//   - session wall-clock total kill (600s default).
//   - per-action wall-clock pre-check (60s default).
//   - cumulative network bytes kill (50MB default).
//   - DOM node gauge check kill (50k default).
//   - JS heap gauge check kill (512MB default).

use crate::budget_enforcer::budget_enforcer::{
    Action, BudgetEnforcer, BudgetLimits, KillCallback, KillReason, LocalBudgetEnforcer,
    ResourceKind, SessionBudgetEntry, SessionCounters,
};
use crate::error::LoomError;
use crate::manifest_writer::SessionId;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Build a BudgetExceeded LoomError for walltime breaches.
/// `observed_ms` and `limit_ms` are in ms; receipt context uses seconds.
fn walltime_error(observed_ms: u64, limit_ms: u64, budget_name: &'static str) -> LoomError {
    use crate::error::LoomErrorCode;
    LoomError::new(
        LoomErrorCode::BudgetExceeded,
        format!("budget exceeded: {budget_name}"),
    )
    .with_context(json!({
        "budget_name": budget_name,
        "observed": observed_ms / 1000,
        "limit": limit_ms / 1000,
    }))
}

fn network_error(observed_bytes: u64, limit_bytes: u64) -> LoomError {
    use crate::error::LoomErrorCode;
    LoomError::new(
        LoomErrorCode::BudgetExceeded,
        "budget exceeded: network bytes",
    )
    .with_context(json!({
        "observed_bytes": observed_bytes,
        "limit_bytes": limit_bytes,
    }))
}

fn dom_nodes_error(observed_nodes: u64, limit_nodes: u64) -> LoomError {
    use crate::error::LoomErrorCode;
    LoomError::new(LoomErrorCode::BudgetExceeded, "budget exceeded: dom nodes").with_context(
        json!({
            "observed_nodes": observed_nodes,
            "limit_nodes": limit_nodes,
        }),
    )
}

fn js_heap_error(observed_heap_bytes: u64, limit_heap_bytes: u64) -> LoomError {
    use crate::error::LoomErrorCode;
    LoomError::new(LoomErrorCode::BudgetExceeded, "budget exceeded: js heap").with_context(json!({
        "observed_heap_bytes": observed_heap_bytes,
        "limit_heap_bytes": limit_heap_bytes,
    }))
}

/// Fire the kill callback once (idempotent via AtomicBool).
fn fire_kill(entry: &SessionBudgetEntry, id: SessionId, reason: KillReason) {
    if !entry.killed.swap(true, Ordering::AcqRel) {
        (entry.kill)(id, reason);
    }
}

impl BudgetEnforcer for LocalBudgetEnforcer {
    /// Pre-action budget check.
    /// Returns Err immediately if:
    ///  - session total walltime already at/over session_walltime_ms
    ///  - action's estimated walltime exceeds action_walltime_ms
    fn check(&self, session: SessionId, action: &Action) -> Result<(), LoomError> {
        let guard = self.per_session.read();
        let entry = match guard.get(&session) {
            Some(e) => Arc::clone(e),
            None => {
                return Err(LoomError::new(
                    crate::error::LoomErrorCode::SessionNotFound,
                    format!("budget check: session not found: {}", session.0),
                ))
            }
        };
        drop(guard);

        let walltime_total = entry.counters.walltime_ms.load(Ordering::Relaxed);
        if walltime_total >= entry.limits.session_walltime_ms {
            return Err(walltime_error(
                walltime_total,
                entry.limits.session_walltime_ms,
                "session_wall_clock_seconds",
            ));
        }

        if action.estimated_walltime_ms > entry.limits.action_walltime_ms {
            return Err(walltime_error(
                action.estimated_walltime_ms,
                entry.limits.action_walltime_ms,
                "action_wall_clock_seconds",
            ));
        }

        Ok(())
    }

    /// Post-effect accounting.
    /// Walltime and network: cumulative (fetch_add).
    /// DomNodes and JsHeap: gauge / high-water-mark (store then check).
    fn account(&self, session: SessionId, kind: ResourceKind, delta: u64) -> Result<(), LoomError> {
        let guard = self.per_session.read();
        let entry = match guard.get(&session) {
            Some(e) => Arc::clone(e),
            None => {
                return Err(LoomError::new(
                    crate::error::LoomErrorCode::SessionNotFound,
                    format!("budget account: session not found: {}", session.0),
                ))
            }
        };
        drop(guard);

        match kind {
            ResourceKind::Walltime => {
                let total = entry
                    .counters
                    .walltime_ms
                    .fetch_add(delta, Ordering::Relaxed)
                    + delta;
                if total >= entry.limits.session_walltime_ms {
                    fire_kill(
                        &entry,
                        session.clone(),
                        KillReason::BudgetExceeded {
                            kind,
                            observed: total,
                            limit: entry.limits.session_walltime_ms,
                        },
                    );
                    return Err(walltime_error(
                        total,
                        entry.limits.session_walltime_ms,
                        "session_wall_clock_seconds",
                    ));
                }
            }
            ResourceKind::Network => {
                let total = entry
                    .counters
                    .network_bytes
                    .fetch_add(delta, Ordering::Relaxed)
                    + delta;
                if total >= entry.limits.network_bytes {
                    fire_kill(
                        &entry,
                        session.clone(),
                        KillReason::BudgetExceeded {
                            kind,
                            observed: total,
                            limit: entry.limits.network_bytes,
                        },
                    );
                    return Err(network_error(total, entry.limits.network_bytes));
                }
            }
            ResourceKind::DomNodes => {
                // Gauge: delta IS the current absolute DOM node count.
                entry.counters.dom_nodes.store(delta, Ordering::Relaxed);
                if delta >= entry.limits.dom_nodes {
                    fire_kill(
                        &entry,
                        session.clone(),
                        KillReason::BudgetExceeded {
                            kind,
                            observed: delta,
                            limit: entry.limits.dom_nodes,
                        },
                    );
                    return Err(dom_nodes_error(delta, entry.limits.dom_nodes));
                }
            }
            ResourceKind::JsHeap => {
                // Gauge: delta IS the current absolute heap size.
                entry.counters.js_heap_bytes.store(delta, Ordering::Relaxed);
                if delta >= entry.limits.js_heap_bytes {
                    fire_kill(
                        &entry,
                        session.clone(),
                        KillReason::BudgetExceeded {
                            kind,
                            observed: delta,
                            limit: entry.limits.js_heap_bytes,
                        },
                    );
                    return Err(js_heap_error(delta, entry.limits.js_heap_bytes));
                }
            }
        }

        Ok(())
    }

    fn register_session(
        &self,
        id: SessionId,
        counters: Arc<SessionCounters>,
        limits: BudgetLimits,
        kill: KillCallback,
    ) {
        LocalBudgetEnforcer::register_session(self, id, counters, limits, kill);
    }

    fn unregister_session(&self, id: SessionId) {
        LocalBudgetEnforcer::unregister_session(self, id);
    }
}
