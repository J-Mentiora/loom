//! `benchmark_commands` — `loom benchmark` CLI subcommand.
pub mod benchmark_commands;
pub use benchmark_commands::*;

mod impl_benchmark;

#[cfg(test)]
mod interface_tests;
