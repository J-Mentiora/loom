//! Shared test-support code for loom-cli integration tests.
//!
//! Pulled into individual test crates with `mod common;`. Each `tests/*.rs`
//! file compiles as its own crate, so any given test exercises only a subset of
//! the helpers here — hence the crate-wide `dead_code` allow (it propagates to
//! the submodules below).
#![allow(dead_code)]

pub mod daemon_test_harness;
