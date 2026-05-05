//! `manifest_writer` — see crate root.
pub mod manifest_writer;
pub use manifest_writer::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
