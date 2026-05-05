//! `content_store` — re-exports the implementation submodule.
pub mod content_store;
pub use content_store::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
