//! `cli_config` — see `systems/loom-cli/modules/ConfigResolver/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cli_config;
pub mod color_choice;
pub mod output_mode;
pub use cli_config::*;
pub use color_choice::{resolve_color, ColorChoice};
pub use output_mode::OutputMode;

#[cfg(test)]
mod interface_tests;
