//! `chromium_downloader` — re-exports the implementation submodule.
pub mod chromium_downloader;
pub use chromium_downloader::*;

#[cfg(test)]
mod interface_tests;
