//! `log_forwarder` — re-exports the implementation submodule.
pub mod log_forwarder;
pub use log_forwarder::*;

#[cfg(test)]
mod interface_tests;
