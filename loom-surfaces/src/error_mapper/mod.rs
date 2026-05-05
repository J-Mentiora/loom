//! `error_mapper` — re-exports the implementation submodule.
pub mod error_mapper;
pub use error_mapper::*;

#[cfg(test)]
mod interface_tests;
