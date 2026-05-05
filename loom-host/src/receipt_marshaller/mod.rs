//! `receipt_marshaller` — re-exports the implementation submodule.
pub mod receipt_marshaller;
pub use receipt_marshaller::*;

#[cfg(test)]
mod interface_tests;
