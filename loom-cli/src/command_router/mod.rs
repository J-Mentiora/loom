//! `command_router` — see crate root.
pub mod command_router;
pub mod validate_flags;
pub use command_router::*;
pub use validate_flags::validate_flags;

#[cfg(test)]
mod interface_tests;
