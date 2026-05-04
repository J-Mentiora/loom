//! `pretty_renderer` — see `systems/loom-cli/modules/PrettyRenderer/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod pretty_renderer;
pub use pretty_renderer::*;

#[cfg(test)]
mod interface_tests;
