//! `dispatcher` — re-exports the implementation submodule.
pub mod dispatcher;
pub use dispatcher::*;

#[cfg(test)]
mod interface_tests;
