//! `budget_enforcer` — see crate root.
pub mod budget_enforcer;
pub use budget_enforcer::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
