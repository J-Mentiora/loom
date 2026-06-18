//! `loom-shims` — out-of-process surface drivers (Chromium for v1).
//! Reachable from `loom-host::shim_manager` over a private IPC.

// Some fields exist for forward-compatibility and are not read yet.
#![allow(unused_extern_crates, unused_imports, dead_code)]

pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations ----
pub mod action_executor;
pub mod cdp_connection;
pub mod determinism_injector;
pub mod determinism_script_template;
pub mod dispatcher;
pub mod ipc_endpoint;
pub mod log_forwarder;
pub mod network_interceptor;
pub mod process_monitor;
pub mod readiness_monitor;
pub mod screencast_recorder;
pub mod supervisor;
pub mod target_manager;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;
