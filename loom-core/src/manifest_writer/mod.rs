//! `manifest_writer` — see `systems/loom-core/modules/manifest_writer/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod manifest_writer;
pub use manifest_writer::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
