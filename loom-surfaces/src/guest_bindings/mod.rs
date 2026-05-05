//! `guest_bindings` — re-exports the implementation submodule.
pub mod guest_bindings;
pub use guest_bindings::*;

#[cfg(test)]
mod interface_tests;
