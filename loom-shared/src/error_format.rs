//! Canonical `LoomError` + `LoomErrorCode` per binding-constraints §5.
//!
//! Stable enum (~25 codes). Adding a code is SemVer-minor; removing one
//! is major. The linter `tools/lint-error-codes.py` walks every
//! `LoomError::` constructor and asserts every variant is in the
//! documented `errors.json` schema.

use serde::{Deserialize, Serialize};

/// Stable error code enum exposed across all process boundaries
/// (RPC, WIT, MCP, SDK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoomErrorCode {
    // ---- Core / lifecycle ----
    SessionNotFound,
    SessionAlreadyClosed,
    SessionAborted,
    SessionKilled,
    SurfaceTrap,

    // ---- Vault ----
    VaultRejection,
    VaultGrantExpired,
    VaultGrantRevoked,
    VaultUnknownLabel,

    // ---- Budget ----
    BudgetExceeded,
    BudgetRateLimited,

    // ---- Content store ----
    StoreIntegrityFailed,
    StoreNotFound,
    StoreFullNoEvictable,

    // ---- Manifest / replay ----
    ManifestCorrupt,
    ReplayDivergence,
    ReplayMissingBlob,

    // ---- LLM cache ----
    /// AC-DET-07.1: Recorded-mode cache miss — no stored response for key.
    LlmCacheMiss,

    // ---- Shim / transport ----
    ShimFailure,
    ShimTimeout,
    ShimBreakerOpen,

    // ---- RPC / IO ----
    RpcInvalidRequest,
    RpcAuthFailed,
    RpcSchemaViolation,
    Io,

    // ---- Safety profile ----
    /// AC-WEB-07.1 (now superseded): evaluate expression matched the
    /// safe-profile denylist. **DEPRECATED for the safe-profile evaluate
    /// path** — production now emits `LoomErrorCode::ProfileRestricted`
    /// (wire kind `"profile_restricted"`) from the daemon-layer gate at
    /// `loom-daemon::WasmBridge::dispatch_action_blocking`. The old emit
    /// site at `loom-surfaces::evaluate_verb::EvaluateVerb::execute` is
    /// dead code (see top-of-file notice).
    ///
    /// **STILL USED at the JSON-RPC schema-validation boundary**
    /// (`loom-rpc::request_router`, `ConnectionHandler` wire-shape errors,
    /// etc.) — those are unrelated to safe profile and remain valid emit
    /// sites. Do not blanket-deprecate this variant; the safe-profile
    /// emission path is the only deprecated use.
    #[serde(rename = "schema_violation")]
    SchemaViolation,
    /// AC-WEB-07.2: download target outside session-scoped downloads directory.
    /// **DEPRECATED in favor of the `Browser.setDownloadBehavior(allowAndName)`
    /// model plus visibility handler** (AC-SAFEPROF-04). Chromium now
    /// confines downloads to the session dir; rejection events route through
    /// `LoomErrorCode::ProfileRestricted` if the policy gains explicit-reject
    /// semantics later. Schedule for removal alongside `EvaluateVerb::execute`
    /// cleanup.
    #[serde(rename = "safe_profile_download_blocked")]
    SafeProfileDownloadBlocked,
    /// AC-SAFEPROF-01..04: action rejected because the active session profile
    /// (e.g. `"safe"`) forbids it. Wire string: `"profile_restricted"`.
    /// `LoomError.context` carries `{matched_pattern, profile, violation}`
    /// so callers distinguish evaluate-denylist hits (violation =
    /// `safe_profile_evaluate_denylist_match`) from download blocks
    /// (violation = `download_path_outside_session_dir`).
    /// Substring matching against `EVALUATE_DENYLIST` is broader than the
    /// operator's regex spec (e.g. matches `console.log(window.location)` —
    /// read access — not just write). For safe profile this is intentional
    /// defense-in-depth. See `loom-surfaces::safety::EVALUATE_DENYLIST`.
    #[serde(rename = "profile_restricted")]
    ProfileRestricted,

    // ---- Catch-alls ----
    InvalidArgument,
    Unsupported,
    Internal,
}

impl LoomErrorCode {
    /// Stable kebab-case wire string. Hand-written to keep grep-able.
    pub fn as_wire(&self) -> &'static str {
        match self {
            LoomErrorCode::SessionNotFound => "session-not-found",
            LoomErrorCode::SessionAlreadyClosed => "session-already-closed",
            LoomErrorCode::SessionAborted => "session-aborted",
            LoomErrorCode::SessionKilled => "session-killed",
            LoomErrorCode::SurfaceTrap => "surface-trap",
            LoomErrorCode::VaultRejection => "vault-rejection",
            LoomErrorCode::VaultGrantExpired => "vault-grant-expired",
            LoomErrorCode::VaultGrantRevoked => "vault-grant-revoked",
            LoomErrorCode::VaultUnknownLabel => "vault-unknown-label",
            LoomErrorCode::BudgetExceeded => "budget-exceeded",
            LoomErrorCode::BudgetRateLimited => "budget-rate-limited",
            LoomErrorCode::StoreIntegrityFailed => "store-integrity-failed",
            LoomErrorCode::StoreNotFound => "store-not-found",
            LoomErrorCode::StoreFullNoEvictable => "store-full-no-evictable",
            LoomErrorCode::ManifestCorrupt => "manifest-corrupt",
            LoomErrorCode::ReplayDivergence => "replay-divergence",
            LoomErrorCode::ReplayMissingBlob => "replay-missing-blob",
            LoomErrorCode::ShimFailure => "shim-failure",
            LoomErrorCode::ShimTimeout => "shim-timeout",
            LoomErrorCode::ShimBreakerOpen => "shim-breaker-open",
            LoomErrorCode::RpcInvalidRequest => "rpc-invalid-request",
            LoomErrorCode::RpcAuthFailed => "rpc-auth-failed",
            LoomErrorCode::RpcSchemaViolation => "rpc-schema-violation",
            LoomErrorCode::LlmCacheMiss => "llm-cache-miss",
            LoomErrorCode::Io => "io",
            LoomErrorCode::SchemaViolation => "schema_violation",
            LoomErrorCode::SafeProfileDownloadBlocked => "safe_profile_download_blocked",
            LoomErrorCode::ProfileRestricted => "profile_restricted",
            LoomErrorCode::InvalidArgument => "invalid-argument",
            LoomErrorCode::Unsupported => "unsupported",
            LoomErrorCode::Internal => "internal",
        }
    }
}

/// Canonical error type. Every public function returns `Result<T, LoomError>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoomError {
    pub code: LoomErrorCode,
    pub message: String,
    /// Optional structured context. Stripped of secrets at the boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl LoomError {
    pub fn new(code: LoomErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: None,
        }
    }

    pub fn with_context(mut self, ctx: serde_json::Value) -> Self {
        self.context = Some(ctx);
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(LoomErrorCode::Internal, message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(LoomErrorCode::InvalidArgument, message)
    }
}

impl std::fmt::Display for LoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_wire(), self.message)
    }
}

impl std::error::Error for LoomError {}

impl From<std::io::Error> for LoomError {
    fn from(e: std::io::Error) -> Self {
        LoomError::new(LoomErrorCode::Io, e.to_string())
    }
}

impl From<LoomErrorCode> for LoomError {
    fn from(c: LoomErrorCode) -> Self {
        LoomError::new(c, c.as_wire())
    }
}
