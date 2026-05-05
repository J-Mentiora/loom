// receipt_builder — tiered receipt types + capture-profile transformation.
pub mod receipt_builder;
pub use receipt_builder::*;

pub mod capture_policy;
mod impl_capture;

#[cfg(test)]
mod interface_tests;
