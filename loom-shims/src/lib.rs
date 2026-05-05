//! `loom-shims` — out-of-process surface drivers (Chromium for v1).
//! Reachable from `loom-host::shim_manager` over a private IPC.
// Suppress WASM-compat boilerplate and stub struct fields in test builds.
// Stub fields will be read once the real implementations fill in the methods.
#![allow(unused_extern_crates, unused_imports, dead_code)]

pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations (10 + pure helpers) ----
pub mod action_executor;
pub mod cdp_connection;
pub mod determinism_injector;
pub mod determinism_script_template;
pub mod dispatcher;
pub mod ipc_endpoint;
pub mod log_forwarder;
pub mod network_interceptor;
pub mod process_monitor;
pub mod supervisor;
pub mod target_manager;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;
