//! `loom-core` — surface-agnostic session lifecycle, content store,
//! manifest writer, vault, replay engine, budget enforcer, and the
//! `CoreApiFacade` that loom-host calls back into.
//!
//! API-exposed module: `core_api_facade`. All other modules expose
//! `pub` types only because cross-module references inside the crate
//! traverse `crate::<module>::Type`; downstream crates should depend on
//! the trait/types re-exported via `core_api_facade` plus the canonical
//! ID/error types in `manifest_writer` + `error`.

// Some fields exist for forward-compatibility and are not read yet.
#![allow(dead_code)]

// Self-import so module bodies can reference `loom_core::*` paths.
extern crate self as loom_core;

// `loom_core::error::LoomError` is the universal error type referenced
// by every other crate. The canonical definition lives in
// `loom-shared`; we re-export here so the path `loom_core::error::*`
// keeps working.
pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations ----
pub mod budget_enforcer;
pub mod content_store;
pub mod core_api_facade;
pub mod determinism_harness;
pub mod error_types;
pub mod exporters;
pub mod importers;
pub mod llm_cache;
pub mod manifest_writer;
pub mod observability;
pub mod profile_registry;
pub mod receipt_builder;
pub mod replay_engine;
pub mod session_manager;
pub mod session_scope;
pub mod startup_manager;
pub mod vault;

// ---- Benchmarks ----
pub mod benchmarks;

// ---- Mocks ----
#[cfg(any(test, feature = "mock"))]
pub mod mocks;

// ---- API-exposed re-exports (cross-crate ergonomics) ----
pub use core_api_facade::*;
