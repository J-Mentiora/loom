//! `error_mapper` — see `systems/loom-cli/modules/ErrorMapper/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod error_mapper;
pub use error_mapper::*;

#[cfg(test)]
mod interface_tests;
