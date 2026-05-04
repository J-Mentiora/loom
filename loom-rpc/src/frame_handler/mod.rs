//! `frame_handler` — see `systems/loom-rpc/modules/frame_handler/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod frame_handler;
pub use frame_handler::*;

#[cfg(test)]
mod interface_tests;
