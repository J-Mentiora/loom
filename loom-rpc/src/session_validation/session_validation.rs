//! `session_validation` — typed business-rule validation for the
//! `session.create` RPC params, ahead of the
//! `CoreServiceAdapter::create_session` call.
//!
//! # Contract semantics
//! - **Reject unrecognized
//!   profile / network-mode / budget-key values with typed envelopes
//!   produced by `ErrorTranslator::from_*`. Schema-violation envelopes
//!   are NOT used here because the AC text demands `error.kind` = the
//!   typed variant name (e.g. `unknown_profile`), not
//!   `schema_violation`.
//! - **Allowlists.** Sourced from `loom_core::profile_registry` (single
//!   source of truth for profile / network-mode / budget-key sets).
//! - **Order.** Profile first, then network-mode, then budget keys.
//!   First failing field returns immediately. Order is observable via
//!   the AC integration tests.

use crate::core_service_adapter::core_service_adapter::CreateSessionParams;
use crate::error_translator::error_translator::{ErrorTranslator, JsonRpcError};
use loom_core::profile_registry::profile_registry::{
    is_known_budget_key, is_known_network_mode, is_known_profile, KNOWN_BUDGET_KEYS,
    KNOWN_NETWORK_MODES, KNOWN_PROFILES,
};

/// Validate a `session.create` request's typed business rules. Returns
/// `Ok(())` if every value is in its canonical allowlist; otherwise
/// returns a fully-formed `JsonRpcError` envelope built by
/// `ErrorTranslator::from_*`.
pub fn validate_create_session_params(p: &CreateSessionParams) -> Result<(), JsonRpcError> {
    if !is_known_profile(&p.profile) {
        return Err(ErrorTranslator::from_unknown_profile(
            &p.profile,
            KNOWN_PROFILES,
        ));
    }
    if !is_known_network_mode(&p.network_mode) {
        return Err(ErrorTranslator::from_invalid_network_mode(
            &p.network_mode,
            KNOWN_NETWORK_MODES,
        ));
    }
    if let Some(cp) = p.capture_policy.as_deref() {
        // server-side rejection for non-CLI callers (mcp,
        // sdk). CLI uses `clap::ValueEnum` and never sends bogus values.
        const ALLOWED: &[&str] = &["minimal", "default", "full", "fingerprint"];
        if !ALLOWED.contains(&cp) {
            return Err(ErrorTranslator::from_invalid_capture_policy(cp, ALLOWED));
        }
    }
    if let Some(budget) = &p.budget {
        if let Some(obj) = budget.as_object() {
            // the CLI parses `--budget wall_clock=1s`
            // into a `BudgetLimits` struct whose serde field names are
            // the typed-internal names (`session_walltime_ms`,
            // `network_bytes`, etc.). When the JSON arrives here, those
            // typed names are NOT in the user-facing `KNOWN_BUDGET_KEYS`
            // allowlist — so a valid CLI invocation was wrongly rejected
            // as `invalid_budget_key`. Accept either shape: the
            // user-facing keys for direct-RPC clients, OR the typed
            // struct keys for CLI-serialised BudgetLimits.
            const KNOWN_TYPED_BUDGET_FIELDS: &[&str] = &[
                "session_walltime_ms",
                "action_walltime_ms",
                "network_bytes",
                "dom_nodes",
                "js_heap_bytes",
            ];
            for key in obj.keys() {
                if !is_known_budget_key(key) && !KNOWN_TYPED_BUDGET_FIELDS.contains(&key.as_str()) {
                    return Err(ErrorTranslator::from_invalid_budget_key(
                        key,
                        KNOWN_BUDGET_KEYS,
                    ));
                }
            }
        }
        // Non-object budget shapes fall through; the existing schema
        // / typing layer is the right rejection surface for those.
    }
    Ok(())
}
