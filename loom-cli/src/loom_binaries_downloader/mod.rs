//! `loom_binaries_downloader` — download + verify the auxiliary loom binaries
//! (`loom-daemon`, `loom-mcp`, `loom-shim-chromium`) from the matching GitHub
//! Release for the cargo-install path.

#[allow(clippy::module_inception)]
mod loom_binaries_downloader;

pub use loom_binaries_downloader::{
    default_install_dir, ensure, host_target_triple, DownloadOutcome, AUX_BINARY_NAMES,
};

#[cfg(test)]
mod interface_tests;
