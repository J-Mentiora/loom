//! `cli_config` — re-exports the implementation submodule.
pub mod cli_config;
pub mod color_choice;
pub mod output_mode;
pub use cli_config::*;
pub use color_choice::{resolve_color, ColorChoice};
pub use output_mode::OutputMode;

#[cfg(test)]
mod interface_tests;
