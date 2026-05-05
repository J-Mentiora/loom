//! `manifest_writer` — re-exports the implementation submodule.
pub mod manifest_writer;
pub use manifest_writer::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
