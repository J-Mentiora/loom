//! `chromium_downloader` — see `systems/loom-cli/modules/ChromiumDownloader/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod chromium_downloader;
pub use chromium_downloader::*;

#[cfg(test)]
mod interface_tests;
