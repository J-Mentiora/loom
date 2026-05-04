// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-core/modules/core_api_facade/interface_tests.rs` instead.
// Interface tests for `CoreApiFacade`. Verifies single-entry-point
// property, BC-CORE-08 config wiring, design.md §3.5 startup health
// check delegation.

use super::core_api_facade::{CoreApiFacade, CoreConfig};
use loom_core::error::LoomError;
use loom_core::startup_manager::RecoveryReport;
use std::path::PathBuf;
use std::sync::Arc;

fn cfg() -> CoreConfig {
    CoreConfig {
        data_root: PathBuf::from("/tmp/loom-test"),
        log_path: PathBuf::from("/tmp/loom-test/loom.log"),
        otel_enabled: false,
        default_seed: 42,
        checkpoint_every_n: 100,
    }
}

#[test]
fn new_signature_takes_core_config_returns_arc_facade() {
    fn _ck(c: CoreConfig) -> Result<Arc<CoreApiFacade>, LoomError> {
        CoreApiFacade::new(c)
    }
    let _ = _ck;
    let _ = cfg();
}

#[test]
fn config_carries_bc_core_08_required_fields() {
    let c = cfg();
    let _: PathBuf = c.data_root;
    let _: PathBuf = c.log_path;
    let _: bool = c.otel_enabled;
    let _: u64 = c.default_seed;
    let _: u64 = c.checkpoint_every_n;
}

// === Single entry point: facade exposes accessors for all 9 modules ===

#[test]
fn facade_struct_holds_all_nine_internal_modules() {
    fn _ck(f: &CoreApiFacade) {
        let _ = &f.session_manager;
        let _ = &f.content_store;
        let _ = &f.manifest_writer;
        let _ = &f.vault;
        let _ = &f.budget_enforcer;
        let _ = &f.determinism;
        let _ = &f.replay_engine;
        let _ = &f.startup_manager;
        let _ = &f.observability;
    }
    let _ = _ck;
}

// === Startup health check delegates to StartupManager ===

#[test]
fn startup_health_check_returns_recovery_report() {
    fn _ck(f: &CoreApiFacade) -> Result<RecoveryReport, LoomError> {
        f.startup_health_check()
    }
    let _ = _ck;
}

// === Accessors ===

#[test]
fn determinism_accessor_returns_arc_clone_of_internal_harness() {
    fn _ck(f: &CoreApiFacade) -> Arc<loom_core::determinism_harness::DeterminismHarness> {
        f.determinism()
    }
    let _ = _ck;
}

#[test]
fn session_manager_accessor_returns_arc_local_session_manager() {
    fn _ck(f: &CoreApiFacade)
        -> Arc<loom_core::session_manager::LocalSessionManager>
    {
        f.session_manager()
    }
    let _ = _ck;
}

#[test]
fn vault_accessor_returns_arc_dyn_vault() {
    fn _ck(f: &CoreApiFacade) -> Arc<dyn loom_core::vault::Vault> {
        f.vault()
    }
    let _ = _ck;
}

#[test]
fn replay_engine_accessor_returns_arc_dyn_replay_engine() {
    fn _ck(f: &CoreApiFacade) -> Arc<dyn loom_core::replay_engine::ReplayEngine> {
        f.replay_engine()
    }
    let _ = _ck;
}

#[test]
fn budget_enforcer_accessor_returns_arc_dyn_budget_enforcer() {
    fn _ck(f: &CoreApiFacade)
        -> Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>
    {
        f.budget_enforcer()
    }
    let _ = _ck;
}

#[test]
fn content_store_accessor_returns_arc_dyn_content_store() {
    fn _ck(f: &CoreApiFacade)
        -> Arc<dyn loom_core::content_store::ContentStore>
    {
        f.content_store()
    }
    let _ = _ck;
}

#[test]
fn manifest_writer_accessor_returns_arc_dyn_manifest_writer() {
    fn _ck(f: &CoreApiFacade)
        -> Arc<dyn loom_core::manifest_writer::ManifestWriter>
    {
        f.manifest_writer()
    }
    let _ = _ck;
}

#[test]
fn observability_accessor_returns_arc_observability() {
    fn _ck(f: &CoreApiFacade) -> Arc<loom_core::observability::Observability> {
        f.observability()
    }
    let _ = _ck;
}
