//! `loom-shared` — cross-crate primitives.
//!
//! - `error_format` — canonical `LoomError` + `LoomErrorCode` enum
//!   (binding-constraints §5).
//! - `llm_types` — LLM passthrough types shared across loom-core/rpc/mcp .
//! - `logging` — `tracing_subscriber` setup with secret-redaction layer.
//! - `auth_token` — `HelloToken` shared by daemon + CLI + MCP.

pub mod action_aliases;
pub mod auth_token;
pub mod binary_resolver;
pub mod chromium_resolver;
pub mod error_format;
pub mod llm_types;
pub mod logging;
pub mod navigate_outcome;
pub mod shim_protocol;
pub mod types;

// Convenience re-exports so downstream crates can `use loom_shared::*;`
pub use error_format::{LoomError, LoomErrorCode};
pub use types::{EpochMs, Seed};
