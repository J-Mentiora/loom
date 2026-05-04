//! `profile_registry` — canonical session profile / network-mode / budget
//! key sets. See `interfaces.rs` for documentation.
pub mod profile_registry;
pub use profile_registry::*;

#[cfg(test)]
mod interface_tests;
