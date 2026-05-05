//! `llm_cache` — re-exports the implementation submodule.
pub mod llm_cache;
pub use llm_cache::*;

#[cfg(test)]
mod interface_tests;
