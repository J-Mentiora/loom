//! `content_store` — see `systems/loom-core/modules/content_store/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod content_store;
pub use content_store::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
