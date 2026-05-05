//! `budget_enforcer` — see `systems/loom-core/modules/budget_enforcer/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod budget_enforcer;
pub use budget_enforcer::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
