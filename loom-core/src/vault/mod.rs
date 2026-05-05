//! `vault` — re-exports the implementation submodule.
pub mod vault;
pub use vault::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
