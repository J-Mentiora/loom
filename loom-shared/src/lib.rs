//! `loom-shared` — cross-crate primitives.
//!
//! - `error_format` — canonical `LoomError` + `LoomErrorCode` enum
//!   shared across every crate so error types don't fork.
//! - `llm_types` — LLM passthrough types shared across loom-core/rpc/mcp .
//! - `logging` — `tracing_subscriber` setup with secret-redaction layer.
//! - `auth_token` — `HelloToken` shared by daemon + CLI + MCP.
//! - `redacted` — `Redacted<T>` newtype hiding values in
//!   Debug/Display/Serialize output (added v0.9.5 for cookie values).
//! - `safety` — profile-based evaluate denylist + path-scoping checks
//!   (relocated from the retired `loom-surfaces` crate).
//! - `cookie_types` — CDP cookie wire types + `validate_cookie_params`
//!   (relocated from the retired `loom-surfaces` crate).

pub mod action_aliases;
pub mod auth_token;
pub mod binary_resolver;
pub mod builtin_schemas;
pub mod chromium_resolver;
pub mod cookie_types;
pub mod dom_normalize;
pub mod error_format;
pub mod llm_types;
pub mod logging;
pub mod navigate_outcome;
pub mod redacted;
pub mod safety;
pub mod screenshot_decode;
pub mod shim_protocol;
pub mod types;

// Convenience re-exports so downstream crates can `use loom_shared::*;`
pub use error_format::{LoomError, LoomErrorCode};
pub use redacted::{wipe_byte_buffer_in_place, wipe_string_buffer_in_place, Redacted};
pub use types::{EpochMs, Seed};
