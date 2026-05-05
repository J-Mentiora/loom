//! `cdp_message_encoder` — re-exports the implementation submodule.
pub mod cdp_message_encoder;
pub use cdp_message_encoder::*;

#[cfg(test)]
mod interface_tests;
