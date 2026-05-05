//! `wasm_runtime` — re-exports the implementation submodule.
pub mod wasm_runtime;
pub use wasm_runtime::*;

#[cfg(test)]
mod interface_tests;
