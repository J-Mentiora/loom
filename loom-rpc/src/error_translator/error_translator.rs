// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/error_translator/interfaces.rs` instead.
// ErrorTranslator — single conversion point from `LoomError` to JSON-RPC
// error envelope. Mirrors `LoomErrorCode` 1:1 (BC-RPC-03 / IC-RPC-06).
//
// # Contract semantics
// - **Single point of translation (IC-RPC-06).** `From<LoomError> for
//   JsonRpcError` is the ONLY allowed `LoomError → wire` impl in this
//   crate. Clippy lint bans `serde_json::to_string` and free-form
//   `JsonRpcError::custom` calls outside this module.
// - **Stable enum (BC-RPC-03).** `code` field is the `LoomErrorCode`
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

// Stub: in the full crate, `LoomError` and `LoomErrorCode` come from
// loom-core. We reference them by path so the module list does not
// re-export loom-core types (binding constraint #5: error propagation
// stays per-system; loom-rpc owns its translation surface).
//
// module_kind: type-bridge

/// Stable error code mirroring `loom_core::error::LoomErrorCode` 1:1.
/// Encoded as snake_case strings on the wire (BC-RPC-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoomErrorCode {
    // Protocol layer (loom-rpc owners)
    ProtocolAuthRequired,
    ProtocolMalformed,
    SchemaViolation,
    MethodNotFound,
    // Mirrored from loom-core / loom-host (full set lives in errors.json)
    SessionNotFound,
    SessionAborted,
    SessionClosed,
    BudgetExceeded,
    SurfaceTrap,
    SurfaceUnavailable,
    VaultGrantNotFound,
    VaultGrantRevoked,
    VaultGrantExpired,
    VaultRejection,
    VaultCredentialTypeUnsupported,
    StoreIntegrityFailed,
    InternalError,
    // Session-create typed validation (AC-PROFVAL-01/02/03,
    // parent AC-PROTO-02.1). Carry structured `data: {provided,
    // available}` via the matching `ErrorTranslator::from_*`
    // constructors. Additive variants are SemVer-compatible per
    // BC-RPC-03.
    UnknownProfile,
    InvalidNetworkMode,
    InvalidBudgetKey,
    InvalidCapturePolicy,
    /// AC-SAFEPROF-01: action rejected because the active session profile
    /// (e.g. `"safe"`) forbids it. `data` carries
    /// `{matched_pattern, profile, violation}` so callers distinguish
    /// evaluate-denylist hits from download blocks. Wire string:
    /// `"profile_restricted"`.
    ProfileRestricted,
    /// AC-DIST-05: chromium binary not located by any resolver search
    /// path during session.create. Wire string: `"browser_not_found"`
    /// (snake_case via the enum's `rename_all = "snake_case"`; matches
    /// the canonical loom-shared enum's kebab-case `"browser-not-found"`
    /// modulo separator. Additive variant; SemVer-compatible per
    /// BC-RPC-03.
    BrowserNotFound,
}

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
/// (IC-RPC-06: code == variant name, message ≤ 280 chars, data ==
/// variant structured fields.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: LoomErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Maximum message length (IC-RPC-06).
pub const MAX_MESSAGE_LEN: usize = 280;

/// The translator. Stateless; operates as a function namespace.
pub struct ErrorTranslator;

/// Borrow-shaped reference to `loom_core::error::LoomError`.
/// Concrete type lives in loom-core; we declare a transparent newtype
/// to keep `ErrorTranslator` testable without a dep cycle.
pub struct LoomErrorRef<'a>(pub &'a dyn LoomErrorLike);

/// Minimal trait surfacing the variant + structured data we need to
/// build the JSON-RPC envelope. `loom_core::error::LoomError`
/// implements this via a build-time-generated impl (BC-RPC-02 +
/// BC-RPC-03; the impl is emitted alongside `errors.json` schema).
pub trait LoomErrorLike: Send + Sync {
    fn code(&self) -> LoomErrorCode;
    fn message(&self) -> String;
    fn data(&self) -> Option<serde_json::Value>;
}
