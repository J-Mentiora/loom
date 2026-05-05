//! `trap_handler` — re-exports the implementation submodule.
pub mod trap_handler;
pub use trap_handler::*;

#[cfg(test)]
mod interface_tests;
