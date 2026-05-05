//! `frame_handler` — re-exports the implementation submodule.
pub mod frame_handler;
pub use frame_handler::*;

#[cfg(test)]
mod interface_tests;
