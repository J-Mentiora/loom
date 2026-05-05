//! `loom-cli` — `loom` binary library facade. The actual `fn main()`
//! lives in `src/bin/loom.rs`; this lib exposes the runtime helpers
//! used by integration tests + the bin shim.

pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations (25) ----
pub mod action_commands;
pub mod action_help;
pub mod admin_commands;
pub mod auth_manager;
pub mod benchmark_commands;
pub mod chromium_downloader;
pub mod chromium_pin;
pub mod cli_config;
pub mod cli_main;
pub mod cli_observability;
pub mod command_router;
pub mod doctor_runner;
pub mod error_mapper;
pub mod help_generator;
pub mod import_commands;
pub mod launchd_plist_writer;
pub mod loom_binaries_downloader;
pub mod manpage_step;
pub mod mcp_delegate;
pub mod output_formatter;
pub mod postinstall_runner;
pub mod pretty_renderer;
pub mod rpc_client;
pub mod schema_cache;
pub mod serve_runner;
pub mod session_commands;
pub mod url_allowlist;
pub mod vault_commands;
pub mod version_command;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;

// `crate::CliConfig` and `crate::CliError` are referenced bare across
// the crate per the per-module interfaces.
pub use cli_config::CliConfig;
pub use error_mapper::CliError;
