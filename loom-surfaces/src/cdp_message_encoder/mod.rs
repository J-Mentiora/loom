//! `cdp_message_encoder` — see `systems/loom-surfaces/modules/cdp_message_encoder/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cdp_message_encoder;
pub use cdp_message_encoder::*;

#[cfg(test)]
mod interface_tests;
