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

/// Stable error code mirroring `loom_core::error::LoomErrorCode` 1:1.
/// Encoded as snake_case strings on the wire.
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
    // Session-create typed validation. Carries structured
    // `data: {provided, available}` via the matching
    // `ErrorTranslator::from_*` constructors. Additive variants are
    // SemVer-compatible.
    UnknownProfile,
    InvalidNetworkMode,
    InvalidBudgetKey,
    InvalidCapturePolicy,
    /// Action rejected because the active session profile
    /// (e.g. `"safe"`) forbids it. `data` carries
    /// `{matched_pattern, profile, violation}` so callers distinguish
    /// evaluate-denylist hits from download blocks. Wire string:
    /// `"profile_restricted"`.
    ProfileRestricted,
    /// Chromium binary not located by any resolver search
    /// path during session.create. Wire string: `"browser_not_found"`
    /// (snake_case via `rename_all = "snake_case"`; matches the
    /// canonical loom-shared enum's kebab-case `"browser-not-found"`
    /// modulo separator). Additive variant; SemVer-compatible.
    BrowserNotFound,
    /// Per-request server-side deadline expired before dispatch returned
    /// a response. Configurable via `LOOM_REQUEST_TIMEOUT_MS` (default
    /// 30000). Distinct from `BudgetExceeded` (per-session wall-clock)
    /// and shim-level timeouts (host-shim CBOR round-trip).
    RequestTimeout,
    /// In-flight request cancelled by a sibling `request.cancel` on
    /// the same connection.
    RequestCancelled,
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
/// build the JSON-RPC envelope. `loom_core::error::LoomError`
/// implements this via a build-time-generated impl (the impl is
/// emitted alongside `errors.json` schema).
pub trait LoomErrorLike: Send + Sync {
    fn code(&self) -> LoomErrorCode;
    fn message(&self) -> String;
    fn data(&self) -> Option<serde_json::Value>;
}
