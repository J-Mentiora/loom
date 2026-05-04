//! `loom-surfaces` — surface verbs implemented as WASM guest modules.
//!
//! Compiles to `wasm32-wasi-preview2` for production; compiles to
//! native `lib` for unit tests. v1 ships only the WEB surface;
//! shell/fs/api/native are declared in `wit/loom-surface.wit` but not
//! built until v1.5.
// Suppress WASM-compat boilerplate (extern crate alloc, use alloc::...)
// that's needed in no_std/WASM builds but redundant in native test builds.
#![allow(unused_extern_crates, unused_imports)]

pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations (16) ----
pub mod cdp_message_encoder;
pub mod click_verb;
pub mod error_mapper;
pub mod evaluate_verb;
pub mod guest_bindings;
pub mod host_bindings;
pub mod hover_verb;
pub mod navigate_verb;
pub mod receipt_builder;
pub mod safety;
pub mod screenshot_verb;
pub mod scroll_verb;
pub mod select_verb;
pub mod snapshot_verb;
pub mod type_text_verb;
pub mod wait_verb;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;
