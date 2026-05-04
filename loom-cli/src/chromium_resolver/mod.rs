//! `chromium_resolver` — locate a Chromium binary across the install
//! channels in priority order: env override → pinned (`loom postinstall`)
//! → PATH search → macOS `/Applications/...`. AC-DIST-05.
pub mod chromium_resolver;
pub use chromium_resolver::*;

#[cfg(test)]
mod interface_tests;
