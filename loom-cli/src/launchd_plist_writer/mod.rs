//! `launchd_plist_writer` — see `systems/loom-cli/modules/LaunchdPlistWriter/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod launchd_plist_writer;
pub use launchd_plist_writer::*;

#[cfg(test)]
mod interface_tests;
