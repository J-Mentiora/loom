//! `dispatcher` — see `systems/loom-shims/modules/dispatcher/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod dispatcher;
pub use dispatcher::*;

#[cfg(test)]
mod interface_tests;
