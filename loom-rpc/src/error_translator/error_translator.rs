// ErrorTranslator — single conversion point from `LoomError` to JSON-RPC
// error envelope. Mirrors `LoomErrorCode` 1:1.
//
// # Contract semantics
// - **Single point of translation.** `From<LoomError> for
//   JsonRpcError` is the ONLY allowed `LoomError → wire` impl in this
//   crate. Clippy lint bans `serde_json::to_string` and free-form
//   `JsonRpcError::custom` calls outside this module.
// - **Stable enum.** `code` field is the `LoomErrorCode`
//   variant name in `snake_case`. `message` is at most 280 chars
//   (truncated with ellipsis if longer). `data` carries the variant's
//   structured fields verbatim — no free-form prose.
// - **Per-task panic catch.** `catch_panic_into_envelope` is invoked
//   from `ConnectionHandler`'s panic hook to convert any caught
//   `Box<dyn Any + Send>` into `LoomErrorCode::InternalError`.
// - **Schema violations.** `SchemaValidator` invokes
//   `from_schema_violation` directly to construct the
//   `code = "schema_violation"` envelope with `{field, expected, actual}`
//   structured data.

use serde::{Deserialize, Serialize};

// `LoomError` and `LoomErrorCode` come from loom-core. We reference
// them by path so loom-rpc does not re-export loom-core types — error
// propagation stays per-system, and loom-rpc owns its translation
// surface.

// The daemon wire error code IS the canonical `loom_shared::LoomErrorCode`
// (error-code-consolidation): the former 27-variant duplicate enum was merged
// into loom-shared, which now owns every daemon-distinct variant (ProtocolAuthRequired,
// ProtocolMalformed, MethodNotFound, SurfaceUnavailable, SessionClosed, VaultGrantNotFound,
// VaultCredentialTypeUnsupported, UnknownProfile, Invalid{NetworkMode,BudgetKey,CapturePolicy},
// InternalError) alongside the shared ones. The on-wire snake_case spelling is produced by
// `loom_shared::LoomErrorCode`'s manual `Serialize`/`as_wire` (no `rename_all` needed) and is
// pinned to BC-RPC-03 by `wire_parity_with_bc_rpc_03`. Re-exported here so existing
// `error_translator::LoomErrorCode` paths keep resolving.
pub use loom_shared::error_format::{LoomError, LoomErrorCode};

/// Structured field detail for a `schema_violation` envelope.
/// Wire-equal to the `data` block in `{error: {code: "schema_violation",
/// data: {field, expected, actual}}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaViolationDetail {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// JSON-RPC error envelope as it appears on the wire.
/// `{"error": {"code": "...", "message": "...", "data": ...}}`.
/// (code == variant name, message ≤ 280 chars, data ==
/// variant structured fields.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: LoomErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Maximum message length on the wire.
pub const MAX_MESSAGE_LEN: usize = 280;

/// The translator. Stateless; operates as a function namespace.
pub struct ErrorTranslator;

/// Borrow-shaped reference to `loom_core::error::LoomError`.
/// Concrete type lives in loom-core; we declare a transparent newtype
/// to keep `ErrorTranslator` testable without a dep cycle.
pub struct LoomErrorRef<'a>(pub &'a dyn LoomErrorLike);

/// Minimal trait surfacing the variant + structured data we need to
/// build the JSON-RPC envelope. `code()` returns the canonical
/// `loom_shared::LoomErrorCode` (re-exported above); there is no
/// generated `errors.json` schema — the enum is the single source of truth.
pub trait LoomErrorLike: Send + Sync {
    fn code(&self) -> LoomErrorCode;
    fn message(&self) -> String;
    fn data(&self) -> Option<serde_json::Value>;
}

/// The canonical `loom_shared::LoomError` IS error-like: `context` is its
/// structured wire data. Lets adapter methods that return the full
/// `LoomError` (e.g. `create_session`, whose `session_cap_exceeded`
/// rejection carries `{active, cap, hint}`) route through the single
/// `ErrorTranslator::from_loom_error` conversion point instead of
/// collapsing to a bare code.
impl LoomErrorLike for LoomError {
    fn code(&self) -> LoomErrorCode {
        self.code
    }
    fn message(&self) -> String {
        self.message.clone()
    }
    fn data(&self) -> Option<serde_json::Value> {
        self.context.clone()
    }
}
