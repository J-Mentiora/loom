//! `vault` — see `systems/loom-core/modules/vault/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod vault;
pub use vault::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
