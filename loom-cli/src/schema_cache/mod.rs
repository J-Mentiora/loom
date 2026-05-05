//! `schema_cache` — re-exports the implementation submodule.
pub mod schema_cache;
pub use schema_cache::*;

#[cfg(test)]
mod interface_tests;
