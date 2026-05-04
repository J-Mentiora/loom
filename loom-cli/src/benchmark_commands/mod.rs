//! `benchmark_commands` — `loom benchmark` CLI subcommand.
//! AC-PERF-01.1, AC-PERF-02.1, AC-PERF-04.1.
pub mod benchmark_commands;
pub use benchmark_commands::*;

mod impl_benchmark;

#[cfg(test)]
mod interface_tests;
