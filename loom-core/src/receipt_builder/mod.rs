// receipt_builder — tiered receipt types + capture-profile transformation.
// AC-CORE-04.1–04.5, AC-CORE-05.1, AC-NFR-DET-03.1, AC-NFR-DET-04.1, AC-NFR-DET-05.1
pub mod receipt_builder;
pub use receipt_builder::*;

pub mod capture_policy;
mod impl_capture;

#[cfg(test)]
mod interface_tests;
