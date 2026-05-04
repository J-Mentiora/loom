// error_types — stable receipt status/error discriminators (AC-CORE-05.1, AC-CORE-05.2).
pub mod error_types;
pub use error_types::*;

#[cfg(test)]
mod interface_tests;
