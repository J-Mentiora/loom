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

// `CoreApiFacade` is `Arc<loom_core::CoreApiFacade>` — the single
// entry point into loom-core. We declare the dep abstractly here so
// interface tests don't have to pull the full loom-core crate in.

/// Marker trait satisfied by `loom_core::CoreApiFacade`. Lets sibling
/// modules write `Arc<dyn CoreFacadeBridge>` for testability without
/// leaking loom-core types.
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
    ///
    /// Errors carry the full canonical `LoomError` (code + human message +
    /// context), NOT a bare `AdapterError` code: replay refusals are typed
    /// in loom-core (`not_replayable`, `session_aborted`, `manifest_corrupt`,
    /// `replay_missing_blob`, `session_not_found`) and their compiled-in
    /// refusal reason must survive to the wire `message` instead of
    /// degrading to a translator default.
    fn replay_session_to_id(&self, session_id: &str) -> Result<String, LoomError>;

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

    /// Validate a session; returns the full wire `ValidationResult`
    /// (integrity verdict + the `replayable` verdict — PASS ≠ replayable,
    /// see `ValidationResult::replayable`).
    fn validate_session_result(&self, session_id: &str) -> Result<ValidationResult, AdapterError>;

    /// `session.reap` — quarantine corrupt-WAL orphan sessions that are stuck in
    /// the on-disk active set (and thus consuming concurrency slots) without a
    /// daemon restart. `dry_run` previews candidates without moving anything.
    fn session_reap(&self, dry_run: bool) -> Result<ReapReport, AdapterError>;

    /// Import a Playwright trace.zip from raw bytes. Creates a non-replayable
    /// session and returns its id + action count.
    fn import_playwright_from_bytes(
        &self,
        trace_bytes: &[u8],
    ) -> Result<PlaywrightImportInfo, AdapterError>;

    /// Create a new session. Returns `(session_id, created_at_ms)`.
    /// `network_mode` is always `"live"` here — `session_validation`
    /// rejects everything else upstream — and implementations ignore it:
    /// page traffic is always fetched live (no page-network
    /// record/replay engine exists; response bodies are never captured).
    /// `capture_policy` carries the operator's `--capture-policy` choice
    /// (`"minimal" | "default" | "full"`); `None` means "use server default".
    /// `budget` carries the optional per-session BudgetLimits as a
    /// serde_json::Value (the daemon's CoreBridge deserialises it into
    /// loom_core::budget_enforcer::BudgetLimits).
    ///
    /// Errors as the full `LoomError`, not a bare `AdapterError` code: the
    /// daemon's `session_cap_exceeded` rejection carries `{active, cap,
    /// hint}` in `context`, which must survive to the JSON-RPC envelope
    /// (`data`). Other bridge methods keep the bare-code shape until they
    /// need structured detail too.
    #[allow(clippy::too_many_arguments)]
    fn create_session_raw(
        &self,
        profile: &str,
        network_mode: &str,
        capture_policy: Option<&str>,
        seed: Option<u64>,
        budget: Option<serde_json::Value>,
        no_blocklist: bool,
        no_determinism: bool,
        clock_anchor: Option<u64>,
    ) -> Result<(String, u64), LoomError>;

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

    // ── v0.9.4 W6 direct-credential RPCs ────────────────────────────

    /// Persist a credential under `label`. `params.overwrite=false`
    /// (the default) rejects with the appropriate AdapterError if the
    /// label already exists; `overwrite=true` upserts.
    fn vault_set_secret(
        &self,
        params: VaultSetSecretParams,
    ) -> Result<VaultSetSecretInfo, AdapterError>;

    /// Delete a credential. Honors the D29 cascade contract:
    /// `params.force=false` + active grants → reject; `force=true` →
    /// cascade-revoke grants by label then delete.
    fn vault_delete_secret(
        &self,
        params: VaultDeleteSecretParams,
    ) -> Result<VaultDeleteSecretInfo, AdapterError>;

    /// Enumerate labels stored under the daemon's service_id.
    fn vault_list_labels(
        &self,
        params: VaultListLabelsParams,
    ) -> Result<VaultListLabelsInfo, AdapterError>;

    /// Snapshot of the keychain backend's health + last-error state.
    /// Stable JSON schema per A-W6.4.
    fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError>;

    /// v0.9.6 web-cookie-injection support: return the operator's
    /// current session context so the CLI can bind a
    /// `--credential-type cookie` grant to it. Picks the most recently
    /// created Active session; errors with a typed code when zero
    /// active sessions exist (operator must `loom session create`
    /// first) or when ambiguity demands explicit `--session` (multiple
    /// active sessions — the most-recent heuristic is fine but the
    /// caller can override).
    fn vault_get_session_context(&self) -> Result<VaultGetSessionContextInfo, AdapterError>;

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

/// Wire shape for `session.reap`. `quarantined` holds the corrupt-orphan session
/// IDs that were (or, when `dry_run`, would be) moved aside; `failed` holds
/// `"<id>: <reason>"` for sessions that could not be moved (e.g. cross-device).
///
/// The `idle_evicted` / `zombies_closed` / `orphan_browsers_killed` / `orphan_dirs_removed`
/// fields extend reap to also cover leaked LIVE resources (idle and zombie sessions, orphan
/// Chromium trees). All are `#[serde(default)]` so older clients that don't send/expect them
/// still validate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReapReport {
    pub quarantined: Vec<String>,
    pub skipped_live: u64,
    pub dry_run: bool,
    pub quarantine_dir: Option<String>,
    pub failed: Vec<String>,
    /// Idle Active sessions evicted (or, in dry-run, that would be).
    #[serde(default)]
    pub idle_evicted: Vec<String>,
    /// Active sessions whose Chromium pid was dead, closed (or would be).
    #[serde(default)]
    pub zombies_closed: Vec<String>,
    /// Orphan Chromium trees terminated (or would be), keyed by session id.
    #[serde(default)]
    pub orphan_browsers_killed: Vec<String>,
    /// Orphan user-data-dirs removed.
    #[serde(default)]
    pub orphan_dirs_removed: u64,
}

// === Wire types (WIT-derived; mirrored here for adapter return
// shapes). These will be replaced by the wit-bindgen output
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
    /// `true` when `session.replay` would accept this session. PASS ≠
    /// replayable: a `--no-determinism` recording validates clean yet is
    /// non-replayable by design, and crashed/aborted sources are refused
    /// too. Serde-default `true` so a payload from an older daemon (which
    /// implied PASS ⇒ replayable) keeps its old meaning, and older clients
    /// that don't know the field still validate (additive change).
    #[serde(default = "default_replayable")]
    pub replayable: bool,
    /// Human reason when `replayable == false` — the same compiled-in
    /// explanation `session.replay` refuses with. Skipped when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_replayable_reason: Option<String>,
}

fn default_replayable() -> bool {
    true
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
    /// Page-network mode. `"live"` — the serde default and the ONLY
    /// accepted value — means page traffic is fetched live; loom never
    /// records or replays page-network responses. Anything else (incl.
    /// the formerly-allowlisted-but-inert `"recorded"`/`"mixed"`) is
    /// rejected by `session_validation` with `invalid_network_mode`.
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
    /// Operator's `--no-determinism` opt-out (settle-capture slice 4b).
    /// Default `false` (determinism ON). Pre-feature CLI clients omit the
    /// field → `false` → deterministic capture. `true` injects a pass-through
    /// (real clock + unseeded RNG) and marks the session non-replayable.
    #[serde(default)]
    pub no_determinism: bool,
    /// Operator's `--clock-anchor` value: a fixed Unix epoch (ms) that pins the
    /// injected browser clock for cross-run determinism. Maps to
    /// `SessionCreateOpts.started_at_ms_override` (→ epoch_ms → CDP
    /// initialVirtualTime + Header started_at_ms + replay round-trip).
    /// Default `None` → epoch falls back to wall-clock `now_ms()` (pre-feature
    /// behavior unchanged). No effect under `--no-determinism`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_anchor: Option<u64>,
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
    /// v0.9.6 web-cookie-injection: optional credential-type override.
    /// Default `None` preserves the v0.9.5 OAuth-only contract; set
    /// to `"cookie"` for the `loom vault add --credential-type cookie`
    /// flow to bind a Cookie grant rather than an OAuth grant.
    /// Daemon-side mapping in `vault_grant` translates the string to
    /// `loom_core::vault::CredentialType::{OAuth,Cookie}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_type: Option<String>,
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

// ── v0.9.4 W6: direct credential CRUD wire types ────────────────────

/// Wire params for `vault.set_secret`. The secret travels as hex (per
/// existing `ContentData` convention) so binary payloads round-trip
/// safely over JSON-RPC without escape ambiguity. The CLI hex-encodes
/// raw bytes; the daemon decodes and wraps in `Zeroizing<Vec<u8>>` at
/// the bridge boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSetSecretParams {
    pub label: String,
    /// Lowercase hex (no `0x` prefix).
    pub secret_hex: String,
    /// Mirror of the CLI flag — if `false` and the label already
    /// exists, the daemon returns `AlreadyExists` (exit 5). If `true`,
    /// silently upsert. The trait-level `Vault::set_secret` always
    /// upserts; the safety check lives at this wire boundary.
    #[serde(default)]
    pub overwrite: bool,
    /// Session in which to record G5a/G5b audit entries. `None` means
    /// sessionless (`loom vault add` without `--session`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Wire receipt for `vault.set_secret` success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSetSecretInfo {
    pub label: String,
    /// `true` if an existing entry was overwritten.
    pub replaced: bool,
    /// Three-tier size category (D24): `"small"` | `"medium"` | `"large"`.
    pub size_bucket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDeleteSecretParams {
    pub label: String,
    /// When `true`, cascade-revoke active grants by `label` before
    /// deleting (D29). When `false` and active grants exist, returns
    /// `VaultRejection { code: "credential_in_use" }` and the
    /// credential is left intact.
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDeleteSecretInfo {
    pub label: String,
    /// Count of grants the `--force` cascade revoked. `0` when no
    /// cascade was needed.
    pub cascade_revoked_grants: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListLabelsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListLabelsInfo {
    pub labels: Vec<String>,
    pub count: u32,
}

/// Wire shape for `vault.diagnose` per plan amendment A-W6.4. Stable
/// across patch releases so power users can `jq` against this schema.
///
/// `last_keychain_error` is the most recent backend op error; `None`
/// when no op has failed since daemon startup. The `internal_hash`
/// field on the inner shape is what the operator pastes into their
/// daemon log search to recover the original third-party message
/// (A-W6.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDiagnoseInfo {
    pub backend: String,
    pub init_status: VaultDiagnoseInitStatus,
    pub service_id: String,
    pub label_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_keychain_error: Option<VaultDiagnoseLastError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultDiagnoseInitStatus {
    Ok,
    Error { reason: String },
}

/// v0.9.6 web-cookie-injection — response for `vault.get_session_context`.
/// `session_id` is the operator's current Active session (most recently
/// created). The CLI uses this when issuing `loom vault add
/// --credential-type cookie` so the grant gets bound to the right
/// session per D5 / FND-0008.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGetSessionContextInfo {
    pub session_id: String,
    /// True when the daemon has exactly one Active session — the
    /// resolution is unambiguous. False when multiple Actives exist
    /// and the most-recent heuristic was applied; the CLI may want
    /// to confirm with the operator.
    pub unambiguous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDiagnoseLastError {
    /// Snake_case `KeychainErrorKind` variant string.
    pub kind: String,
    /// RFC-3339 timestamp captured at the moment `vault diagnose` was
    /// invoked — **NOT** the original error's timestamp (council
    /// ship-review M4). v0.9.4 doesn't yet cache the original failure
    /// time; until the v0.9.5 cached-last-error implementation lands,
    /// the field name `diagnosed_at_ts` is the operator's signal that
    /// "this is when you looked, not when it broke."
    pub diagnosed_at_ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_hash: Option<String>,
}

// `AdapterError` is the loom-rpc-local view of `LoomErrorCode`; the
// canonical type lives in `loom_core::error`.
pub type AdapterError = crate::error_translator::error_translator::LoomErrorCode;

// Full canonical error (code + message + structured `context`) for adapter
// methods whose rejections carry wire data — the create-session path
// (`session_cap_exceeded` → `{active, cap, hint}`) and the replay path
// (refusal fidelity — see `replay_session_to_id`).
pub use crate::error_translator::error_translator::LoomError;

/// Trait surface for `CoreServiceAdapter` so `RpcHandlers` can be
/// unit-tested with a fake. Each method maps 1:1 to a method in the
/// loom-rpc contract's `session.*` / `vault.*` block.
pub trait CoreServiceAdapterApi: Send + Sync {
    /// Errors as the full `LoomError` so the cap rejection's structured
    /// `{active, cap, hint}` context reaches the wire (see
    /// `CoreFacadeBridge::create_session_raw`).
    fn create_session(&self, params: CreateSessionParams) -> Result<SessionInfo, LoomError>;

    fn inspect_session(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<SessionInspection, AdapterError>;

    fn list_sessions(&self) -> Result<Vec<SessionInfo>, AdapterError>;

    fn close_session(&self, session_id: &str) -> Result<SessionInfo, AdapterError>;

    fn abort_session(&self, session_id: &str, reason: &str) -> Result<SessionInfo, AdapterError>;

    /// `network_mode` is accepted for wire back-compat (released SDKs
    /// ≤0.10.x sent `"replay"`) but ignored — replay re-executes from
    /// the manifest; there is no page-network mode.
    ///
    /// Errors carry the full `LoomError` (see `CoreFacadeBridge::
    /// replay_session_to_id`) so `RpcHandlers::session_replay` can put the
    /// real refusal reason on the wire.
    fn replay_session(
        &self,
        session_id: &str,
        speed: Option<f32>,
        network_mode: Option<&str>,
    ) -> Result<SessionInfo, LoomError>;

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

    // v0.9.4 W6 direct credential CRUD.
    fn vault_set_secret(
        &self,
        params: VaultSetSecretParams,
    ) -> Result<VaultSetSecretInfo, AdapterError>;

    fn vault_delete_secret(
        &self,
        params: VaultDeleteSecretParams,
    ) -> Result<VaultDeleteSecretInfo, AdapterError>;

    fn vault_list_labels(
        &self,
        params: VaultListLabelsParams,
    ) -> Result<VaultListLabelsInfo, AdapterError>;

    fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError>;

    /// v0.9.6 — see other `vault_get_session_context` doc.
    fn vault_get_session_context(&self) -> Result<VaultGetSessionContextInfo, AdapterError>;

    /// Run GC on the content store. Returns scanned/collected/bytes-freed.
    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        store_max_bytes: Option<u64>,
    ) -> Result<GcRunReport, AdapterError>;

    /// `session.reap` — quarantine stuck corrupt-WAL orphan sessions. `dry_run`
    /// previews without moving anything.
    fn session_reap(&self, dry_run: bool) -> Result<ReapReport, AdapterError>;
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
