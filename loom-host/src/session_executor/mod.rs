//! `session_executor` — re-exports the implementation submodule.
pub mod session_executor;
pub use session_executor::*;

#[cfg(test)]
mod interface_tests;
