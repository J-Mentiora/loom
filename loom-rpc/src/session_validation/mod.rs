//! `session_validation` — typed business-rule validation for
//! `session.create`. See `interfaces.rs` for documentation.
pub mod session_validation;
pub use session_validation::*;

#[cfg(test)]
mod interface_tests;
