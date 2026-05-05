//! `receipt_builder` — see `systems/loom-surfaces/modules/receipt_builder/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod receipt_builder;
pub use receipt_builder::*;

#[cfg(test)]
mod interface_tests;
