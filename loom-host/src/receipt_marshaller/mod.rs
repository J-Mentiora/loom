//! `receipt_marshaller` — see `systems/loom-host/modules/receipt_marshaller/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod receipt_marshaller;
pub use receipt_marshaller::*;

#[cfg(test)]
mod interface_tests;
