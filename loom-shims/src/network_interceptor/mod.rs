//! `network_interceptor` — re-exports the implementation submodule.
pub mod network_interceptor;
pub use network_interceptor::*;

#[cfg(test)]
mod interface_tests;
