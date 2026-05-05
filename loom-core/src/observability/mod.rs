//! `observability` — see `systems/loom-core/modules/observability/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod observability;
pub use observability::*;

#[cfg(test)]
mod interface_tests;
