//! `session_manager` — see crate root.
pub mod session_manager;
pub use session_manager::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
