//! `compiler` — re-exports the implementation submodule.
pub mod compiler;
pub use compiler::*;

#[cfg(test)]
mod interface_tests;
