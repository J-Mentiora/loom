//! `launchd_plist_writer` — re-exports the implementation submodule.
pub mod launchd_plist_writer;
pub use launchd_plist_writer::*;

#[cfg(test)]
mod interface_tests;
