//! `command_router` — re-exports the implementation submodule.
pub mod command_router;
pub mod validate_flags;
pub use command_router::*;
pub use validate_flags::validate_flags;

#[cfg(test)]
mod interface_tests;
