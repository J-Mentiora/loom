//! `loom-host` — WASM host (wasmtime component-model) that dispatches
//! actions to surface modules + bridges to out-of-process shims.
//!
//! API-exposed module: `wasm_host`. All other modules expose `pub`
//! types only because cross-module references inside the crate
//! traverse `crate::<module>::Type`.

#![allow(dead_code, unused_imports)]

// Re-export canonical error path for ergonomics.
pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations (13) ----
pub mod compiler;
pub mod error_mapper;
pub mod host_function_registry;
pub mod host_function_table;
pub mod host_observability;
pub mod module_library;
pub mod receipt_marshaller;
pub mod session_executor;
pub mod shim_manager;
pub mod trap_handler;
pub mod wasm_host;
pub mod wasm_runtime;
pub mod wit_type_marshaller;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;

pub use wasm_host::*;

#[cfg(test)]
mod ac_tests;

#[cfg(test)]
mod ac_tests_wasmb;

#[cfg(test)]
mod ac_tests_wasi;

#[cfg(test)]
mod ac_tests_navigate_receipt;
