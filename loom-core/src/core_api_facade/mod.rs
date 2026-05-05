//! `core_api_facade` — see `systems/loom-core/modules/core_api_facade/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod core_api_facade;
pub use core_api_facade::*;

mod impl_local;
pub use impl_local::*;

mod impl_replay;

#[cfg(test)]
mod interface_tests;
