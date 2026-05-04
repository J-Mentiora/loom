//! `llm_cache` — see `systems/loom-core/modules/llm_cache/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod llm_cache;
pub use llm_cache::*;

#[cfg(test)]
mod interface_tests;
