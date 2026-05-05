//! `content_store` — see crate root.
pub mod content_store;
pub use content_store::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
