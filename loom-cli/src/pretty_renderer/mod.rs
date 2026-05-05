//! `pretty_renderer` — see crate root.
pub mod ansi;
pub mod curated;
pub mod pretty_renderer;
pub mod redact;
pub use pretty_renderer::*;

#[cfg(test)]
mod interface_tests;
