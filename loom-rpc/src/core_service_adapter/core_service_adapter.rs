// CoreServiceAdapter — routes `session.*` + `vault.*` methods to
// loom-core via the locked `CoreApiFacade` (see design.md §7).
//
// # Contract semantics
// - **Single facade handle (loom-core single entry point).** This
//   adapter holds `Arc<loom_core::CoreApiFacade>` and reaches the
//   nine internal modules through accessor methods only — never
//   bypassing the facade. Locked names: `SessionManager`, `Vault`,
//   `ContentStore`, `ManifestWriter`, `ReplayEngine`,
//   `BudgetEnforcer`, `DeterminismHarness`, `StartupManager`,
//   `Observability`.
// - **No action.* routing here.** This adapter is
//   structurally incompatible with `Receipt` payloads — `action.*`
//   methods route through `HostServiceAdapter` instead. Misrouting
//   would not type-check.
// - **Vault response shape.** `vault_grant` returns
//   `GrantInfo` with `grant_id` only. The raw secret never enters
//   loom-rpc; `Vault::substitute` (loom-core) is the sole call site
//   for raw bytes per the wire-spec's secret-handling rules.
// - **Errors propagate via `LoomError` → `JsonRpcError`.** This
//   adapter never constructs envelopes itself; it returns
//   `Result<T, LoomError>` and `RpcHandlers` calls
//   `ErrorTranslator::from_loom_error`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Stub: in the full crate, `CoreApiFacade` is
// `Arc<loom_core::CoreApiFacade>` — the locked single entry point.
// We declare the dep abstractly here so the interface tests do not
// require the full loom-core crate. The concrete `Arc` type binds
// in v5.4 implementation.
//
// module_kind: cross-system-bridge

/// Marker trait satisfied by `loom_core::CoreApiFacade` once the
/// crate dep is wired in v5.4. Lets sibling modules write
/// `Arc<dyn CoreFacadeBridge>` for testability without leaking the
/// loom-core types.
pub trait CoreFacadeBridge: Send + Sync {
    /// Export a closed session to the requested format. Stores output in CAS.
    /// Returns `ExportInfo` with `artifact_ref = SHA256` of export bytes.
    fn export_session_to_cas(
        &self,
        session_id: &str,
        format: &str,
    ) -> Result<ExportInfo, AdapterError>;

    /// Fetch export artifact bytes from CAS by SHA-256 reference.
    fn get_export_bytes(&self, artifact_ref: &str) -> Result<Vec<u8>, AdapterError>;

    /// Disk-based session listing. Returns (session_id, status_str, created_at_ms).
    /// Used by loom-rpc's CoreServiceAdapter to build SessionInfo responses.
    fn list_sessions_info(&self) -> Result<Vec<(String, String, u64)>, AdapterError>;

    /// Replay a session; returns the new replay session_id string.
    fn replay_session_to_id(&self, session_id: &str) -> Result<String, AdapterError>;

    /// Diff two sessions; returns the DiffReport as a JSON value.
    fn diff_sessions_json(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
    ) -> Result<serde_json::Value, AdapterError>;

    /// Inspect a session up to `at_action`; returns manifest summary as JSON.
    fn inspect_session_json(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<serde_json::Value, AdapterError>;

    /// Validate a session; returns `(passed, reasons)`.
    fn validate_session_result(
        &self,
        session_id: &str,
    ) -> Result<(bool, Vec<String>), AdapterError>;

    /// Import a Playwright trace.zip from raw bytes. Creates a non-replayable
    /// session and returns its id + action count.
    fn import_playwright_from_bytes(
        &self,
        trace_bytes: &[u8],
    ) -> Result<PlaywrightImportInfo, AdapterError>;

    /// Create a new session. Returns `(session_id, created_at_ms)`.
    /// `capture_policy` carries the operator's `--capture-policy` choice
    /// (`"minimal" | "default" | "full"`); `None` means "use server default".
    /// `budget` carries the optional per-session BudgetLimits as a
    /// serde_json::Value (the daemon's CoreBridge deserialises it into
    /// loom_core::budget_enforcer::BudgetLimits).
    #[allow(clippy::too_many_arguments)]
    fn create_session_raw(
        &self,
        profile: &str,
        network_mode: &str,
        capture_policy: Option<&str>,
        seed: Option<u64>,
        budget: Option<serde_json::Value>,
        no_blocklist: bool,
    ) -> Result<(String, u64), AdapterError>;

    /// Close an active session.
    fn close_session_raw(&self, session_id: &str) -> Result<(), AdapterError>;

    /// Abort an active session with a reason string.
    fn abort_session_raw(&self, session_id: &str, reason: &str) -> Result<(), AdapterError>;

    /// Issue a grant (`vault.grant`). Returns `GrantInfo`
    /// (`grant_id` only — never the secret).
    fn vault_grant(&self, params: GrantParams) -> Result<GrantInfo, AdapterError>;

    /// Revoke a grant (`vault.revoke`). `reason` is a free-form string.
    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), AdapterError>;

    /// List alive grants (`vault.list_grants`). Empty result is valid
    /// (the wire contract admits the "possibly empty" case).
    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError>;

    /// Add an OAuth-only credential (`vault.add`). Allowlisted providers
    /// return a typed `VaultAddInfo` receipt with `status="oauth_required"`;
    /// non-allowlisted reject with the canonical vault-rejection envelope.
    fn vault_add(&self, params: VaultAddParams) -> Result<VaultAddInfo, AdapterError>;

    /// Run garbage collection on the content store (`gc.run`). `ttl_days`
    /// is the maximum age (in days) of unreferenced blobs to retain;
    /// blobs older than `ttl_days` whose referenced-by set is empty are
    /// removed. `None` means use the daemon-default TTL.
    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        store_max_bytes: Option<u64>,
    ) -> Result<GcRunReport, AdapterError>;
}

/// Wire shape for `gc.run`: `{removed, kept, freed_bytes}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcRunReport {
    pub blobs_scanned: u64,
    pub blobs_collected: u64,
    pub bytes_freed: u64,
}

// === Wire types (WIT-derived; mirrored here for adapter return
// shapes). In v5.4 these are replaced by the wit-bindgen output
// in `loom-rpc/src/types/`. ===

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub status: String,
    pub created_at_ms: u64,
    /// Free-form reason carried by `session.abort`.
    /// Populated for aborted sessions; `None` for active/closed/crashed.
    /// Skipped during JSON serialisation when absent so existing parsers
    /// that don't know the field still validate.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInspection {
    pub session_id: String,
    pub at_action: Option<u64>,
    pub manifest_summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub a: String,
    pub b: String,
    pub diff: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportInfo {
    pub session_id: String,
    pub format: String,
    pub artifact_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub session_id: String,
    pub passed: bool,
    pub reasons: Vec<String>,
}

/// Result of `import.playwright`. Mirrors the loom-core `PlaywrightImportResult`
/// shape so we don't leak the loom-core type across the wire boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightImportInfo {
    pub session_id: String,
    pub action_count: u64,
}

/// Content artifact returned by `content.get`. Bytes are hex-encoded so
/// they transfer safely over JSON-RPC without binary encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentData {
    pub artifact_ref: String,
    pub data_hex: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantInfo {
    /// The response carries `grant_id` only.
    pub grant_id: String,
    pub origin: String,
    pub scopes: Vec<String>,
    pub ttl_seconds: u64,
    pub label: String,
    // No `secret`, `token`, or `value` fields — schema enforced
    // additionally by `SchemaValidator::validate_response`.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionParams {
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default = "default_network_mode")]
    pub network_mode: String,
    /// Operator's `--capture-policy` choice.
    /// Wire form: `"minimal" | "default" | "full"`. `None` → server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_policy: Option<String>,
    pub seed: Option<u64>,
    pub budget: Option<serde_json::Value>,
    /// Operator's `--no-blocklist` opt-out.
    /// Default `false` (blocklist enforced). Pre-feature CLI clients
    /// omit the field entirely → defaults to `false` → enforcement on.
    #[serde(default)]
    pub no_blocklist: bool,
}

fn default_profile() -> String {
    "safe".to_string()
}
fn default_network_mode() -> String {
    "live".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantParams {
    pub session_id: String,
    pub origin: String,
    pub scopes: Vec<String>,
    pub ttl_seconds: u64,
    pub label: String,
}

/// Wire params for `vault.add` — session-less.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAddParams {
    pub provider: String,
    pub label: Option<String>,
    #[serde(default)]
    pub yes: bool,
}

/// Wire receipt for `vault.add` success branch. Carries no secret bytes.
/// Status is `"oauth_required"` until the real OAuth device flow lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAddInfo {
    pub provider: String,
    pub label: String,
    pub status: String,
}

// Stub LoomError reference — replaced by `loom_core::error::LoomError`
// in v5.4 wiring.
pub type AdapterError = crate::error_translator::error_translator::LoomErrorCode;

/// Trait surface for `CoreServiceAdapter` so `RpcHandlers` can be
/// unit-tested with a fake. Each method maps 1:1 to a method in the
/// loom-rpc contract's `session.*` / `vault.*` block.
pub trait CoreServiceAdapterApi: Send + Sync {
    fn create_session(&self, params: CreateSessionParams) -> Result<SessionInfo, AdapterError>;

    fn inspect_session(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<SessionInspection, AdapterError>;

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, AdapterError>;

    fn close_session(&self, session_id: &str) -> Result<SessionInfo, AdapterError>;

    fn abort_session(&self, session_id: &str, reason: &str) -> Result<SessionInfo, AdapterError>;

    fn replay_session(
        &self,
        session_id: &str,
        speed: Option<f32>,
        network_mode: Option<&str>,
    ) -> Result<SessionInfo, AdapterError>;

    fn diff_sessions(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
        show_dom_diffs: bool,
    ) -> Result<DiffReport, AdapterError>;

    fn export_session(&self, session_id: &str, format: &str) -> Result<ExportInfo, AdapterError>;

    /// Fetch raw export artifact bytes by SHA-256 reference (hex-encoded).
    fn content_get(&self, artifact_ref: &str) -> Result<ContentData, AdapterError>;

    fn validate_session(&self, session_id: &str) -> Result<ValidationResult, AdapterError>;

    /// Import a Playwright trace.zip (raw bytes). Returns the new
    /// non-replayable session id + action count.
    fn import_playwright(&self, trace_bytes: &[u8]) -> Result<PlaywrightImportInfo, AdapterError>;

    fn vault_grant(&self, params: GrantParams) -> Result<GrantInfo, AdapterError>;

    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), AdapterError>;

    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError>;

    fn vault_add(&self, params: VaultAddParams) -> Result<VaultAddInfo, AdapterError>;

    /// Run GC on the content store. Returns scanned/collected/bytes-freed.
    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        store_max_bytes: Option<u64>,
    ) -> Result<GcRunReport, AdapterError>;
}

#[allow(dead_code)]
pub struct CoreServiceAdapter {
    pub(crate) core: Arc<dyn CoreFacadeBridge>,
}

impl CoreServiceAdapter {
    pub fn new(core: Arc<dyn CoreFacadeBridge>) -> Arc<Self> {
        Arc::new(Self { core })
    }
}
