//! `schema_cache` — see `systems/loom-cli/modules/SchemaCache/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod schema_cache;
pub use schema_cache::*;

#[cfg(test)]
mod interface_tests;
