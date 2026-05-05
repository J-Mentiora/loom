//! `core_api_facade` — see crate root.
pub mod core_api_facade;
pub use core_api_facade::*;

mod impl_local;
pub use impl_local::*;

mod impl_replay;

#[cfg(test)]
mod interface_tests;
