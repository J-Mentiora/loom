//! Canonical `LoomError` + `LoomErrorCode` shared across every crate.
//!
//! Stable enum. Adding a code is SemVer-minor; removing one is major.
//!
//! ## Wire encoding (single source of truth)
//! `Serialize` emits [`LoomErrorCode::as_wire`] verbatim; `Deserialize` routes
//! through the tolerant [`LoomErrorCode::from_wire`]. `from_wire` accepts BOTH
//! the canonical kebab-case spellings AND the daemon translator's snake_case
//! spellings (`loom-rpc::error_translator`), and maps any unrecognized code to
//! [`LoomErrorCode::Internal`] instead of failing. This is what fixes the
//! historic `"malformed error response"` bug: a long-lived MCP client could not
//! deserialize the daemon's snake_case codes into this kebab-case enum, so every
//! multi-word error collapsed to opaque `io`. Tolerant decode also makes
//! old-client ↔ new-daemon version skew safe (unknown → `internal`, never a hard
//! parse error).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Stable error code enum exposed across all process boundaries
/// (RPC, WIT, MCP, SDK). Serde is implemented manually (see module docs) — do
/// NOT add `#[serde(...)]` attributes here; edit `as_wire`/`from_wire` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Backend refused the op (e.g. user cancelled the OS auth prompt
    /// or the collection is locked). Maps from `KeychainErrorKind::Denied`.
    /// Per A-W5.2 (v0.9.4 W5 inline error-translation table).
    VaultPermissionDenied,
    /// Backend is not available (not initialised, IPC failure, D-Bus
    /// owner changed). Maps from `KeychainErrorKind::Unavailable`.
    VaultBackendUnavailable,
    /// Backend op exceeded its per-op time budget (see
    /// `loom-core::vault::BlockingKeychain`). Maps from
    /// `KeychainErrorKind::TimedOut`.
    VaultBackendTimeout,
    /// Backend would have triggered a UI prompt but `allow_prompt = false`
    /// (default in non-TTY). Maps from `KeychainErrorKind::NonInteractivePrompt`.
    VaultNonInteractivePrompt,
    /// Catch-all for backend errors that don't map to a more specific
    /// kind. `LoomError.context` carries `{ internal_hash: "<sha256-hex>" }`
    /// so support can correlate to the daemon's `tracing::error!` log
    /// (per A-W6.3) without the third-party message ever reaching the
    /// audit-chain manifest. Maps from `KeychainErrorKind::Internal`.
    VaultInternal,
    /// Label failed the canonical `^[A-Za-z0-9:_-]{1,64}$` validation
    /// at the CLI boundary (D37) or the manifest-writer defense-in-depth
    /// gate (A-W8.5). The latter forces a refactor if a future code path
    /// bypasses CLI validation rather than silently accepting an unsafe
    /// label into the audit chain.
    VaultInvalidLabel,

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
    /// Source session is non-replayable by design (recorded with
    /// `--no-determinism`: real clock + unseeded RNG, so its receipts can
    /// never be replay-equal). Distinct from request-shape errors
    /// (`SchemaViolation`/`InvalidArgument`) — the caller's fix is to
    /// re-record with determinism on, not to change the request.
    NotReplayable,

    // ---- LLM cache ----
    /// Recorded-mode cache miss — no stored response for key.
    LlmCacheMiss,

    // ---- Shim / transport ----
    ShimFailure,
    ShimTimeout,
    ShimBreakerOpen,

    // ---- RPC / IO ----
    RpcInvalidRequest,
    RpcAuthFailed,
    RpcSchemaViolation,
    /// Per-request server-side deadline expired before the dispatcher
    /// produced a response. Configurable via `LOOM_REQUEST_TIMEOUT_MS`
    /// (default 30000). Distinct from `BudgetExceeded` (which is per-
    /// session wall-clock) and `ShimTimeout` (which is the host-shim
    /// CBOR round-trip deadline).
    RequestTimeout,
    /// In-flight request was cancelled by a sibling `request.cancel`
    /// call on the same connection. The dispatcher may continue running
    /// briefly in the background (bounded by shim-level timeouts), but
    /// the client has already received this typed envelope so it can
    /// move on.
    RequestCancelled,
    /// Per-connection rate limit exceeded for the requested method.
    /// Currently fires only on `daemon.health` (where the
    /// `{deep:true}` form fans out to N shims per call — see #58).
    /// Token-bucket configured via `LOOM_DAEMON_HEALTH_RATE_RPS`
    /// (sustained, default 10) and `LOOM_DAEMON_HEALTH_RATE_BURST`
    /// (capacity, default 30). The client should back off and retry;
    /// the bucket refills at the configured RPS.
    TooManyRequests,
    /// `session.create` rejected because the daemon is at its concurrent-
    /// session cap (`LOOM_MAX_CONCURRENT_SESSIONS`, default 16). Capacity,
    /// NOT a fault: the daemon is busy-but-healthy, so back off and retry
    /// after a slot frees (reconnecting cannot free one), or reclaim slots
    /// with `loom session reap`. `LoomError.context` carries
    /// `{active, cap, hint}` so callers can distinguish "back off" from
    /// "daemon is broken" without string-matching the message. Previously
    /// this rejection collapsed to the opaque daemon catch-all
    /// `internal_error` at the AdapterError boundary.
    /// Wire string: `"session_cap_exceeded"`.
    SessionCapExceeded,
    /// Transient transport/connection fault on the MCP↔daemon socket — a
    /// broken pipe, a connection closed/EOF with no response, or a daemon
    /// idle-drop of a long-lived connection. **Retryable** via reconnect
    /// (`RetryDisposition::Reconnect`): a dropped persistent connection must
    /// never fail an action that a fresh connection would satisfy. Classified
    /// client-side in `loom-mcp::error_mapper`; distinct from `Io` (a genuine,
    /// non-transient I/O failure) and from `TooManyRequests` (capacity — back
    /// off, do NOT reconnect). Never used for a malformed/undecodable error
    /// envelope (that means the request already executed — see `from_wire`).
    TransportDropped,
    Io,

    // ---- Safety profile ----
    ///.1 (now superseded): evaluate expression matched the
    /// safe-profile denylist. **DEPRECATED for the safe-profile evaluate
    /// path** — production now emits `LoomErrorCode::ProfileRestricted`
    /// (wire kind `"profile_restricted"`) from the daemon-layer gate at
    /// `loom-daemon::WasmBridge::dispatch_action_blocking`. The old verb-side
    /// emit site was removed with the retired `loom-surfaces` crate; the
    /// daemon-layer gate is now the sole safe-profile evaluate path.
    ///
    /// **STILL USED at the JSON-RPC schema-validation boundary**
    /// (`loom-rpc::request_router`, `ConnectionHandler` wire-shape errors,
    /// etc.) — those are unrelated to safe profile and remain valid emit
    /// sites. Do not blanket-deprecate this variant; the safe-profile
    /// emission path is the only deprecated use.
    SchemaViolation,
    /// download target outside session-scoped downloads directory.
    /// **DEPRECATED in favor of the `Browser.setDownloadBehavior(allowAndName)`
    /// model plus visibility handler** . Chromium now
    /// confines downloads to the session dir; rejection events route through
    /// `LoomErrorCode::ProfileRestricted` if the policy gains explicit-reject
    /// semantics later. Schedule for removal.
    SafeProfileDownloadBlocked,
    /// action rejected because the active session profile
    /// (e.g. `"safe"`) forbids it. Wire string: `"profile_restricted"`.
    /// `LoomError.context` carries `{matched_pattern, profile, violation}`
    /// so callers distinguish evaluate-denylist hits (violation =
    /// `safe_profile_evaluate_denylist_match`) from download blocks
    /// (violation = `download_path_outside_session_dir`).
    /// Substring matching against `EVALUATE_DENYLIST` is broader than the
    /// operator's regex spec (e.g. matches `console.log(window.location)` —
    /// read access — not just write). For safe profile this is intentional
    /// defense-in-depth. See `crate::safety::EVALUATE_DENYLIST`.
    ProfileRestricted,

    // ---- Browser / launch ----
    /// chromium binary could not be located by any of the
    /// resolver's search paths (env override, pinned `~/.config/loom/chromium/...`,
    /// PATH lookup for `chromium`/`chromium-browser`/`chrome`/`google-chrome`,
    /// macOS `/Applications/...`). Wire string: `"browser-not-found"`.
    /// `LoomError.message` carries the actionable install command.
    BrowserNotFound,

    // ---- Daemon protocol / validation layer (merged from loom-rpc, error-code-consolidation) ----
    // These 1:1 mirror the former `loom_rpc::error_translator::LoomErrorCode`
    // daemon-distinct variants. Their wire strings are frozen by BC-RPC-03
    // (mirrored snake_case in python-sdk/loom/types.py + typescript-sdk/src/types.ts);
    // the `wire_parity_with_bc_rpc_03` test pins them. Kept distinct from the
    // client-side `Rpc*` / `Internal` / `Unsupported` classifications so the daemon
    // wire stays byte-identical.
    /// JSON-RPC handshake requires auth. Daemon wire `protocol_auth_required`.
    ProtocolAuthRequired,
    /// Malformed JSON-RPC frame/shape. Daemon wire `protocol_malformed`.
    ProtocolMalformed,
    /// Unknown JSON-RPC method. Daemon wire `method_not_found`.
    MethodNotFound,
    /// Surface/shim unavailable to serve the action. Daemon wire `surface_unavailable`.
    SurfaceUnavailable,
    /// Session exists but is closed. Daemon wire `session_closed`.
    SessionClosed,
    /// Vault grant id not found. Daemon wire `vault_grant_not_found`.
    VaultGrantNotFound,
    /// Vault credential type not supported. Daemon wire `vault_credential_type_unsupported`.
    VaultCredentialTypeUnsupported,
    /// session.create: unknown profile. Daemon wire `unknown_profile`.
    UnknownProfile,
    /// session.create: invalid network mode. Daemon wire `invalid_network_mode`.
    InvalidNetworkMode,
    /// session.create: invalid budget key. Daemon wire `invalid_budget_key`.
    InvalidBudgetKey,
    /// session.create: invalid capture policy. Daemon wire `invalid_capture_policy`.
    InvalidCapturePolicy,
    /// Daemon-internal error. Daemon wire `internal_error` — distinct from the
    /// client-side `Internal` catch-all (wire `internal`) to preserve BC-RPC-03.
    InternalError,

    // ---- Catch-alls ----
    InvalidArgument,
    Unsupported,
    Internal,
}

impl LoomErrorCode {
    /// Stable kebab-case wire string. Hand-written to keep grep-able.
    pub fn as_wire(&self) -> &'static str {
        match self {
            LoomErrorCode::SessionNotFound => "session_not_found",
            LoomErrorCode::SessionAlreadyClosed => "session_already_closed",
            LoomErrorCode::SessionAborted => "session_aborted",
            LoomErrorCode::SessionKilled => "session_killed",
            LoomErrorCode::SurfaceTrap => "surface_trap",
            LoomErrorCode::VaultRejection => "vault_rejection",
            LoomErrorCode::VaultGrantExpired => "vault_grant_expired",
            LoomErrorCode::VaultGrantRevoked => "vault_grant_revoked",
            LoomErrorCode::VaultUnknownLabel => "vault_unknown_label",
            LoomErrorCode::VaultPermissionDenied => "vault_permission_denied",
            LoomErrorCode::VaultBackendUnavailable => "vault_backend_unavailable",
            LoomErrorCode::VaultBackendTimeout => "vault_backend_timeout",
            LoomErrorCode::VaultNonInteractivePrompt => "vault_non_interactive_prompt",
            LoomErrorCode::VaultInternal => "vault_internal",
            LoomErrorCode::VaultInvalidLabel => "vault_invalid_label",
            LoomErrorCode::BudgetExceeded => "budget_exceeded",
            LoomErrorCode::BudgetRateLimited => "budget_rate_limited",
            LoomErrorCode::StoreIntegrityFailed => "store_integrity_failed",
            LoomErrorCode::StoreNotFound => "store_not_found",
            LoomErrorCode::StoreFullNoEvictable => "store_full_no_evictable",
            LoomErrorCode::ManifestCorrupt => "manifest_corrupt",
            LoomErrorCode::ReplayDivergence => "replay_divergence",
            LoomErrorCode::ReplayMissingBlob => "replay_missing_blob",
            LoomErrorCode::NotReplayable => "not_replayable",
            LoomErrorCode::ShimFailure => "shim_failure",
            LoomErrorCode::ShimTimeout => "shim_timeout",
            LoomErrorCode::ShimBreakerOpen => "shim_breaker_open",
            LoomErrorCode::RpcInvalidRequest => "rpc_invalid_request",
            LoomErrorCode::RpcAuthFailed => "rpc_auth_failed",
            LoomErrorCode::RpcSchemaViolation => "rpc_schema_violation",
            LoomErrorCode::RequestTimeout => "request_timeout",
            LoomErrorCode::RequestCancelled => "request_cancelled",
            LoomErrorCode::TooManyRequests => "too_many_requests",
            LoomErrorCode::SessionCapExceeded => "session_cap_exceeded",
            LoomErrorCode::TransportDropped => "transport_dropped",
            LoomErrorCode::LlmCacheMiss => "llm_cache_miss",
            LoomErrorCode::Io => "io",
            LoomErrorCode::SchemaViolation => "schema_violation",
            LoomErrorCode::SafeProfileDownloadBlocked => "safe_profile_download_blocked",
            LoomErrorCode::ProfileRestricted => "profile_restricted",
            LoomErrorCode::BrowserNotFound => "browser_not_found",
            LoomErrorCode::InvalidArgument => "invalid_argument",
            LoomErrorCode::Unsupported => "unsupported",
            LoomErrorCode::Internal => "internal",
            // ---- Daemon protocol / validation layer (BC-RPC-03 frozen wire) ----
            LoomErrorCode::ProtocolAuthRequired => "protocol_auth_required",
            LoomErrorCode::ProtocolMalformed => "protocol_malformed",
            LoomErrorCode::MethodNotFound => "method_not_found",
            LoomErrorCode::SurfaceUnavailable => "surface_unavailable",
            LoomErrorCode::SessionClosed => "session_closed",
            LoomErrorCode::VaultGrantNotFound => "vault_grant_not_found",
            LoomErrorCode::VaultCredentialTypeUnsupported => "vault_credential_type_unsupported",
            LoomErrorCode::UnknownProfile => "unknown_profile",
            LoomErrorCode::InvalidNetworkMode => "invalid_network_mode",
            LoomErrorCode::InvalidBudgetKey => "invalid_budget_key",
            LoomErrorCode::InvalidCapturePolicy => "invalid_capture_policy",
            LoomErrorCode::InternalError => "internal_error",
        }
    }

    /// Tolerant decode of a wire `code` string into a variant. Accepts the
    /// canonical kebab-case spellings (`as_wire`) AND the daemon translator's
    /// snake_case spellings (`loom-rpc::error_translator::LoomErrorCode`, whose
    /// variant names diverge in places). Any unrecognized code maps to
    /// [`LoomErrorCode::Internal`] — NEVER an error — so a long-lived client can
    /// always type a daemon error (no more `"malformed error response"`) and
    /// version skew degrades gracefully. Single source of truth for decode.
    pub fn from_wire(s: &str) -> LoomErrorCode {
        use LoomErrorCode::*;
        match s {
            "session-not-found" | "session_not_found" => SessionNotFound,
            "session-already-closed" | "session_already_closed" => SessionAlreadyClosed,
            "session_closed" => SessionClosed,
            "session-aborted" | "session_aborted" => SessionAborted,
            "session-killed" | "session_killed" => SessionKilled,
            "surface-trap" | "surface_trap" => SurfaceTrap,
            "vault-rejection" | "vault_rejection" => VaultRejection,
            "vault-grant-expired" | "vault_grant_expired" => VaultGrantExpired,
            "vault-grant-revoked" | "vault_grant_revoked" => VaultGrantRevoked,
            "vault-unknown-label" | "vault_unknown_label" => VaultUnknownLabel,
            "vault_grant_not_found" => VaultGrantNotFound,
            "vault_credential_type_unsupported" => VaultCredentialTypeUnsupported,
            "vault-permission-denied" | "vault_permission_denied" => VaultPermissionDenied,
            "vault-backend-unavailable" | "vault_backend_unavailable" => VaultBackendUnavailable,
            "vault-backend-timeout" | "vault_backend_timeout" => VaultBackendTimeout,
            "vault-non-interactive-prompt" | "vault_non_interactive_prompt" => {
                VaultNonInteractivePrompt
            }
            "vault-internal" | "vault_internal" => VaultInternal,
            "vault-invalid-label" | "vault_invalid_label" => VaultInvalidLabel,
            "budget-exceeded" | "budget_exceeded" => BudgetExceeded,
            "budget-rate-limited" | "budget_rate_limited" => BudgetRateLimited,
            "store-integrity-failed" | "store_integrity_failed" => StoreIntegrityFailed,
            "store-not-found" | "store_not_found" => StoreNotFound,
            "store-full-no-evictable" | "store_full_no_evictable" => StoreFullNoEvictable,
            "manifest-corrupt" | "manifest_corrupt" => ManifestCorrupt,
            "replay-divergence" | "replay_divergence" => ReplayDivergence,
            "replay-missing-blob" | "replay_missing_blob" => ReplayMissingBlob,
            "not-replayable" | "not_replayable" => NotReplayable,
            "shim-failure" | "shim_failure" => ShimFailure,
            "surface_unavailable" => SurfaceUnavailable,
            "shim-timeout" | "shim_timeout" => ShimTimeout,
            "shim-breaker-open" | "shim_breaker_open" => ShimBreakerOpen,
            "rpc-invalid-request" | "rpc_invalid_request" => RpcInvalidRequest,
            "protocol_malformed" => ProtocolMalformed,
            "rpc-auth-failed" | "rpc_auth_failed" => RpcAuthFailed,
            "protocol_auth_required" => ProtocolAuthRequired,
            "rpc-schema-violation" | "rpc_schema_violation" => RpcSchemaViolation,
            "request-timeout" | "request_timeout" => RequestTimeout,
            "request-cancelled" | "request_cancelled" => RequestCancelled,
            "too-many-requests" | "too_many_requests" => TooManyRequests,
            "session-cap-exceeded" | "session_cap_exceeded" => SessionCapExceeded,
            "transport-dropped" | "transport_dropped" => TransportDropped,
            "llm-cache-miss" | "llm_cache_miss" => LlmCacheMiss,
            "io" => Io,
            "schema-violation" | "schema_violation" => SchemaViolation,
            "safe-profile-download-blocked" | "safe_profile_download_blocked" => {
                SafeProfileDownloadBlocked
            }
            "profile-restricted" | "profile_restricted" => ProfileRestricted,
            "browser-not-found" | "browser_not_found" => BrowserNotFound,
            "method_not_found" => MethodNotFound,
            "unknown_profile" => UnknownProfile,
            "invalid_network_mode" => InvalidNetworkMode,
            "invalid_budget_key" => InvalidBudgetKey,
            "invalid_capture_policy" => InvalidCapturePolicy,
            "invalid-argument" | "invalid_argument" => InvalidArgument,
            "unsupported" => Unsupported,
            "internal_error" => InternalError,
            "internal" => Internal,
            _ => Internal,
        }
    }

    /// How a client should react to this code. Lets callers distinguish a
    /// reconnect-and-retry transport fault from a back-off-and-retry capacity
    /// limit from a non-retryable failure — without hard-coding code lists at
    /// every call site.
    pub fn retry_disposition(&self) -> RetryDisposition {
        match self {
            // A dropped connection: reconnect and retry (bounded, see
            // loom-mcp::rpc_client). Reconnecting re-establishes a fresh socket.
            LoomErrorCode::TransportDropped => RetryDisposition::Reconnect,
            // Capacity (rate limit or session-cap): back off and retry later.
            // Reconnecting cannot free a slot, so do NOT reconnect.
            LoomErrorCode::TooManyRequests
            | LoomErrorCode::BudgetRateLimited
            | LoomErrorCode::SessionCapExceeded => RetryDisposition::Backoff,
            _ => RetryDisposition::None,
        }
    }

    /// Convenience: any retry is worthwhile (transport OR capacity).
    pub fn is_retryable(&self) -> bool {
        !matches!(self.retry_disposition(), RetryDisposition::None)
    }
}

/// How a client should retry a failed call, derived from its error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDisposition {
    /// Transport fault — reconnect, then retry (bounded). See `TransportDropped`.
    Reconnect,
    /// Capacity/rate limit — back off, then retry on the SAME connection.
    Backoff,
    /// Not retryable — surface to the caller.
    None,
}

impl Serialize for LoomErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for LoomErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(LoomErrorCode::from_wire(&s))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every canonical wire string round-trips kebab → variant → kebab.
    #[test]
    fn as_wire_round_trips_via_from_wire() {
        // Representative spread incl. the intentionally-snake variants.
        let cases = [
            LoomErrorCode::SessionNotFound,
            LoomErrorCode::TooManyRequests,
            LoomErrorCode::SessionCapExceeded,
            LoomErrorCode::TransportDropped,
            LoomErrorCode::SchemaViolation,
            LoomErrorCode::SafeProfileDownloadBlocked,
            LoomErrorCode::ProfileRestricted,
            LoomErrorCode::NotReplayable,
            LoomErrorCode::BrowserNotFound,
            LoomErrorCode::Io,
            LoomErrorCode::Internal,
        ];
        for c in cases {
            assert_eq!(LoomErrorCode::from_wire(c.as_wire()), c, "{}", c.as_wire());
        }
    }

    /// The daemon translator emits snake_case; the canonical decode must accept
    /// it (this is the historic "malformed error response" bug — multi-word
    /// snake codes that the kebab-only derive could not parse).
    #[test]
    fn from_wire_accepts_daemon_snake_case() {
        assert_eq!(
            LoomErrorCode::from_wire("session_not_found"),
            LoomErrorCode::SessionNotFound
        );
        assert_eq!(
            LoomErrorCode::from_wire("too_many_requests"),
            LoomErrorCode::TooManyRequests
        );
        // Daemon-distinct codes now decode to their exact (merged) variants
        // (error-code-consolidation: 1:1 instead of the old conflated mapping).
        assert_eq!(
            LoomErrorCode::from_wire("internal_error"),
            LoomErrorCode::InternalError
        );
        assert_eq!(
            LoomErrorCode::from_wire("protocol_malformed"),
            LoomErrorCode::ProtocolMalformed
        );
        assert_eq!(
            LoomErrorCode::from_wire("session_closed"),
            LoomErrorCode::SessionClosed
        );
        // The client-side catch-all keeps its own wire + legacy kebab tolerance.
        assert_eq!(
            LoomErrorCode::from_wire("internal"),
            LoomErrorCode::Internal
        );
        assert_eq!(
            LoomErrorCode::from_wire("session-not-found"),
            LoomErrorCode::SessionNotFound
        );
    }

    /// Unknown / future codes degrade to `Internal` — never a hard parse error
    /// ("malformed"). Makes version skew safe (council C3).
    #[test]
    fn unknown_code_falls_back_to_internal_not_error() {
        assert_eq!(
            LoomErrorCode::from_wire("some_future_code_v2"),
            LoomErrorCode::Internal
        );
        // Through serde (the path the MCP client actually uses): never errors.
        let decoded: LoomErrorCode =
            serde_json::from_value(serde_json::json!("brand_new")).unwrap();
        assert_eq!(decoded, LoomErrorCode::Internal);
    }

    /// A full daemon-style error envelope with a snake_case code decodes into a
    /// typed `LoomError` (the exact thing that produced "malformed error
    /// response" before the fix).
    #[test]
    fn loom_error_decodes_snake_case_envelope() {
        let envelope = serde_json::json!({
            "code": "session_not_found",
            "message": "no such session",
        });
        let err: LoomError = serde_json::from_value(envelope).unwrap();
        assert_eq!(err.code, LoomErrorCode::SessionNotFound);
        assert_eq!(err.message, "no such session");
    }

    #[test]
    fn retry_disposition_distinguishes_reconnect_from_backoff() {
        assert_eq!(
            LoomErrorCode::TransportDropped.retry_disposition(),
            RetryDisposition::Reconnect
        );
        assert_eq!(
            LoomErrorCode::TooManyRequests.retry_disposition(),
            RetryDisposition::Backoff
        );
        // Cap-hit is capacity, not a fault: back off (reconnecting can't
        // free a slot), and it IS worth retrying.
        assert_eq!(
            LoomErrorCode::SessionCapExceeded.retry_disposition(),
            RetryDisposition::Backoff
        );
        assert!(LoomErrorCode::SessionCapExceeded.is_retryable());
        assert_eq!(
            LoomErrorCode::SessionNotFound.retry_disposition(),
            RetryDisposition::None
        );
        assert!(LoomErrorCode::TransportDropped.is_retryable());
        assert!(!LoomErrorCode::SessionNotFound.is_retryable());
    }

    /// Anti-drift guard (error-code-consolidation D9): the canonical `as_wire`
    /// MUST emit the exact snake_case strings the daemon wire is frozen to by
    /// BC-RPC-03 (mirrored in python-sdk/loom/types.py + typescript-sdk/src/types.ts).
    /// Covers all 14 BC-RPC-03 codes; each MUST round-trip via `from_wire`.
    #[test]
    fn wire_parity_with_bc_rpc_03() {
        // The complete BC-RPC-03 frozen set: (variant, exact daemon snake wire).
        // If you add/rename a variant that the daemon emits, update this table AND
        // the SDK mirrors (python-sdk/loom/types.py, typescript-sdk/src/types.ts).
        let frozen: &[(LoomErrorCode, &str)] = &[
            (
                LoomErrorCode::ProtocolAuthRequired,
                "protocol_auth_required",
            ),
            (LoomErrorCode::ProtocolMalformed, "protocol_malformed"),
            (LoomErrorCode::SchemaViolation, "schema_violation"),
            (LoomErrorCode::MethodNotFound, "method_not_found"),
            (LoomErrorCode::SessionNotFound, "session_not_found"),
            (LoomErrorCode::SessionAborted, "session_aborted"),
            (LoomErrorCode::BudgetExceeded, "budget_exceeded"),
            (LoomErrorCode::SurfaceTrap, "surface_trap"),
            (LoomErrorCode::SurfaceUnavailable, "surface_unavailable"),
            (LoomErrorCode::VaultGrantNotFound, "vault_grant_not_found"),
            (LoomErrorCode::VaultGrantRevoked, "vault_grant_revoked"),
            (
                LoomErrorCode::VaultCredentialTypeUnsupported,
                "vault_credential_type_unsupported",
            ),
            (
                LoomErrorCode::StoreIntegrityFailed,
                "store_integrity_failed",
            ),
            (LoomErrorCode::InternalError, "internal_error"),
        ];
        for (variant, wire) in frozen {
            assert_eq!(
                variant.as_wire(),
                *wire,
                "BC-RPC-03 wire drift: {variant:?} must serialize to {wire:?}"
            );
            // Round-trip: the frozen wire must decode back to the same variant.
            assert_eq!(
                LoomErrorCode::from_wire(wire),
                *variant,
                "BC-RPC-03 decode drift: {wire:?} must decode to {variant:?}"
            );
        }
    }
}
