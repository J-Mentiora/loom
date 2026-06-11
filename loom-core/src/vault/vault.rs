// Vault — credential mediation. Stays in `loom-core`.
//
// # Contract semantics
// - **Vault isolation.** Raw secret bytes
//   appear in EXACTLY ONE call site: `Vault::substitute`. The bytes
//   live in a `Zeroizing<Vec<u8>>` local and are parked in the outbound
//   `NetRequest::authorization` slot — a `Redacted<Zeroizing<String>>`
//   that zeroizes on drop and prints `[REDACTED]` from Debug/Serialize
//   (threat model G1/TB4). They never appear in the returned `NetResp`,
//   never in logs, never in the manifest, never cross the WASM boundary.
// - **Substitution mutates in-place.** `substitute(&mut NetRequest)` per
//   the per-task instructions: the substitution writes into `req`
//   directly so secret bytes never sit in a return value.
// - **OAuth-only at v1.** `grant()` rejects non-OAuth
//   credential types with `VaultCredentialTypeUnsupported`.
// - **4-check sequence in substitute:** alive → origin
//   match → scopes superset → ttl. Order is fixed.
// - **Audit entries.** Every grant/consume/revoke/expire
//   appends a typed audit entry to the same manifest hash chain via
//   `ManifestWriter::append_audit`.
// - **Platform leaf in external crate.** Keychain access lives in
//   `loom-keychain` (feature-gated). `Vault` calls a small trait
//   `KeychainAccess` whose impl is selected by cargo feature.

use crate::vault::blocking_keychain::BlockingKeychain;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use zeroize::Zeroizing;

// Re-export the canonical KeychainAccess trait from the platform-leaf
// crate. v0.9.4 unified the previous duplicate definitions (one here, one
// in loom-keychain) — `loom_core::vault::KeychainAccess` now resolves to
// the loom-keychain trait so backends, the daemon, and vault tests all
// reference the same surface.
//
// TODO(v0.10): delete this re-export and migrate the remaining loom-core
// callers (test files + a few internal vault helpers) to import from
// `loom_keychain` directly. The re-export exists to keep the W1 unification
// PR (this PR) surgically minimal; council ship-review Min-1 flagged the
// dual-import-path drift risk as something to clean up next major.
pub use loom_keychain::KeychainAccess;

/// Opaque grant token visible to WASM. ULID-shaped string; carries no
/// secret material.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantId(pub String);

/// Network request struct. WIT-derived. The shape shown
/// here is the wit-bindgen output; all numeric fields are integers.
/// `authorization` is host-side only (no WIT counterpart): the guest can
/// never read the vault-substituted token back out of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetRequest {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    /// Vault-substituted `Authorization` value, set ONLY by
    /// `Vault::substitute`. Same wrapper stack as cookie values:
    /// `Redacted` scrubs Debug/Display output, the inner `Zeroizing`
    /// wipes the heap buffer on drop, and `#[serde(skip)]` keeps the
    /// token out of every serialization entirely (threat model G1/TB4 —
    /// a plain `String` in `headers` survived the whole HTTP round-trip
    /// un-wiped and leaked via any `{:?}`/Serialize of the request).
    /// loom-host's `do_http_request` is the single wire-out consumer.
    #[serde(skip)]
    pub authorization: Option<loom_shared::Redacted<Zeroizing<String>>>,
    pub body: Vec<u8>,
    pub origin: String,
    pub scopes: Vec<String>,
}

/// Network response. WIT-derived. **MUST NEVER carry secret bytes.**
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetResp {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Supported credential types. v0.9.4 allowed only `OAuth`; v0.9.5 adds
/// `Cookie` for the vault-mediated web-cookie-injection feature. `ApiKey`,
/// `Saml`, `Basic` are reserved enum slots — `grant()` still rejects them
/// with `VaultCredentialTypeUnsupported` until a future release adds their
/// gating logic. (D3 simplification: rather than a separate `VaultPolicy`
/// struct, the `match` block in `LocalVault::grant()` IS the per-
/// CredentialType policy table — same behavior, less ceremony.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    OAuth,
    ApiKey,
    Saml,
    Basic,
    Cookie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantOpts {
    pub credential_type: CredentialType,
    pub label: String,
    pub origin: String,
    pub scopes: Vec<String>,
    pub ttl_ms: u64,
    pub threat_model_acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeReason {
    pub reason: String,
}

/// Options for `Vault::add_credential`. Session-less:
/// `loom vault add` has no `--session` flag; the receipt does not
/// participate in audit chains until the real OAuth device flow lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddCredentialOpts {
    pub provider: String,
    pub label: Option<String>,
    pub yes: bool,
}

/// Typed receipt returned by `Vault::add_credential` on the accept branch.
/// `status` is `"oauth_required"` until the real device flow lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddCredentialReceipt {
    pub provider: String,
    pub label: String,
    pub status: String,
}

/// Serializable view of an alive grant. Distinct from the `pub(crate) Grant`
/// record (which carries internal `issued_at_ms`/`revoked` bookkeeping) so
/// `Grant` stays fully encapsulated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantSnapshot {
    pub grant_id: String,
    pub session_id: String,
    pub origin: String,
    pub scopes: Vec<String>,
    pub ttl_seconds: u64,
    pub label: String,
}

/// OAuth-only allowlist for `Vault::add_credential` (Q2 v1 scope —
/// expansion beyond GitHub belongs to a follow-up feature).
pub const OAUTH_PROVIDER_ALLOWLIST: &[&str] = &["github"];

/// Three-tier size category for stored credentials (D24). Eliminates
/// the exact-byte side-channel that `byte_count` would expose in the
/// hash-chained audit manifest.
///
/// Single source of truth for the two forms this bucket takes: the
/// audit-payload enum (serialised inside `SecretAuditPayload::Stored`)
/// and the daemon RPC receipt string ([`SizeBucket::as_str`]). The serde
/// output and `as_str` MUST agree — enforced by
/// `loom-core/tests/vault_size_bucket_ssot.rs`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SizeBucket {
    /// ≤ 256 bytes (typical for OAuth bearer tokens, API keys).
    Small,
    /// ≤ 4096 bytes (long tokens, refresh-token bundles).
    Medium,
    /// > 4096 bytes (large session cookies, multi-part credentials).
    Large,
}

impl SizeBucket {
    /// Wire string for the daemon RPC receipt (`VaultSetSecretInfo.size_bucket`).
    /// Matches the serde `rename_all = "snake_case"` output above.
    pub fn as_str(&self) -> &'static str {
        match self {
            SizeBucket::Small => "small",
            SizeBucket::Medium => "medium",
            SizeBucket::Large => "large",
        }
    }
}

/// Map a raw secret byte length into its coarse [`SizeBucket`] (D24). The single
/// place the 256/4096 thresholds are defined — both the core audit payload and the
/// daemon receipt go through this.
pub fn size_bucket(byte_count: usize) -> SizeBucket {
    if byte_count <= 256 {
        SizeBucket::Small
    } else if byte_count <= 4096 {
        SizeBucket::Medium
    } else {
        SizeBucket::Large
    }
}

/// In-memory grant record. The `secret_ref` is a small handle into the
/// process-internal secret store (the actual bytes live in a separate
/// allocation owned by the keychain crate, fetched at substitute time).
pub(crate) struct Grant {
    pub session_id: SessionId,
    pub label: String,
    pub origin: String,
    pub scopes: Vec<String>,
    pub issued_at_ms: u64,
    pub ttl_ms: u64,
    pub revoked: bool,
}

/// Per-session determinism context for vault AUDIT material (NFR-DET-01).
///
/// Audit payloads are JCS-encoded into `AuditEntry::canonical_bytes` (a
/// number array in the WAL line), where `hashable_line()`'s projection
/// cannot reach — so everything embedded there must be reproducible across
/// two independent same-seed runs:
///
/// - `rng` (ChaCha20 from a domain-separated SHA-256 of the session seed)
///   mints grant ids: pure function of (seed, draw index), not wall-clock
///   + OS entropy.
/// - `tick` is the session-relative vault event clock, recorded as the
///   payload's `ts_tick` — mirroring the D9 receipt rule. Wall clock is
///   still used for TTL *enforcement* (`Grant::issued_at_ms`); it just
///   never enters chain-hashed bytes.
pub(crate) struct VaultSessionCtx {
    pub(crate) rng: rand_chacha::ChaCha20Rng,
    pub(crate) tick: u64,
}

/// Concrete Vault implementation.
///
/// `keychain` is wrapped in a `BlockingKeychain` (A-W5.1) so every backend
/// call goes through the per-op timeout + `spawn_blocking` adapter. The
/// adapter falls back to a direct synchronous call when no tokio runtime
/// is present (sync unit tests against `InMemoryKeychain`).
pub struct LocalVault {
    pub(crate) keychain: Arc<BlockingKeychain>,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) obs: Arc<Observability>,
    /// Grants keyed by `(session, grant_id)`. Load-bearing: grant ids are
    /// deterministic per session (NFR-DET-01), so two same-seed sessions
    /// mint IDENTICAL id strings — a flat `GrantId` key would let one
    /// session's insert overwrite (and its lookups resolve to) another
    /// session's grant.
    pub(crate) grants: parking_lot::RwLock<BTreeMap<(SessionId, GrantId), Grant>>,
    /// Per-session determinism contexts, registered by
    /// `Vault::begin_session` at session create and dropped at terminal.
    /// Unregistered sessions (direct library use, unit tests) get a lazy
    /// fallback derived from the session id instead of a seed.
    pub(crate) det: parking_lot::Mutex<BTreeMap<SessionId, VaultSessionCtx>>,
}

impl LocalVault {
    pub fn new(
        keychain: Arc<dyn KeychainAccess>,
        manifest_writer: Arc<dyn ManifestWriter>,
        obs: Arc<Observability>,
    ) -> Self {
        Self {
            keychain: Arc::new(crate::vault::BlockingKeychain::new(keychain)),
            manifest_writer,
            obs,
            grants: parking_lot::RwLock::new(BTreeMap::new()),
            det: parking_lot::Mutex::new(BTreeMap::new()),
        }
    }
}

/// Public trait surface (per `loom-core_contract.md`).
pub trait Vault: Send + Sync {
    /// Issue an opaque grant token. Pre: secret exists for label.
    /// Post: GrantIssued audit entry appended; raw secret never leaves vault.
    /// Errors: VaultCredentialTypeUnsupported (non-OAuth at v1),
    ///         VaultSecretUnavailable, VaultThreatModelMissing.
    fn grant(&self, session: SessionId, opts: GrantOpts) -> Result<GrantId, LoomError>;

    /// Substitute the grant token with the real secret AT the network
    /// boundary, mutating `req` in-place. **The single call site for raw
    /// secret bytes.** Called by `loom-host`'s `net_request` host-fn ONLY,
    /// which passes the dispatching session — grants are session-scoped
    /// (deterministic ids collide across same-seed sessions), so the
    /// lookup key is `(session, grant)` and a grant issued to another
    /// session is reported as not-found.
    ///
    /// Pre: grant issued to `session` ∧ alive ∧ origin match ∧
    /// scopes ⊇ req.scopes ∧ ttl not exceeded.
    /// Post: `req.authorization` set (zeroize-on-drop, Debug/Serialize
    /// redacted); GrantConsumed audit entry appended; the keychain
    /// buffer zeroizes on function exit.
    fn substitute(
        &self,
        session: &SessionId,
        grant: GrantId,
        req: &mut NetRequest,
    ) -> Result<(), LoomError>;

    /// Cookie-specific substitution path (v0.9.5 / D5). Parallel to
    /// `substitute()` but for the web-cookie-injection feature: resolves
    /// a `Cookie`-typed grant, validates session binding, fetches the
    /// keychain blob (JSON `{"schema_version":1, "cookies":[...]}`), and
    /// returns the deserialized `Vec<NetworkCookieParam>` for the daemon
    /// to encode into a CDP `Network.setCookies` envelope. Raw cookie
    /// values appear here in `Zeroizing<Vec<u8>>` and are dropped at
    /// function exit; they never cross MCP/WASM.
    ///
    /// Pre: grant alive ∧ stored session_id matches `session` ∧ ttl OK.
    /// Post: `CookiesSubstituted{grant_id, cookie_names}` audit appended;
    /// cookie *names* in the audit chain (deterministic, replay-equal);
    /// the per-run session id is deliberately NOT in the payload (the
    /// chain is already per-session, and the payload bytes are
    /// chain-hashed — NFR-DET-01); cookie *values* are NEVER in audit /
    /// receipt / logs.
    ///
    /// Returns: a `Vec<u8>` of the raw keychain blob bytes (JSON-encoded
    /// cookie array). The caller (daemon) deserializes into the typed
    /// `NetworkCookieParam` struct living in `loom_shared::cookie_types`;
    /// keeping the byte-level boundary here avoids pulling cookie-type
    /// deserialization into `loom-core`.
    fn substitute_cookies(
        &self,
        grant: GrantId,
        session: SessionId,
    ) -> Result<Zeroizing<Vec<u8>>, LoomError>;

    /// Revoke a grant. Subsequent `substitute` returns VaultGrantRevoked.
    fn revoke(&self, grant: GrantId, reason: RevokeReason) -> Result<(), LoomError>;

    /// Bind a session's determinism seed (NFR-DET-01). Called once by
    /// `LocalSessionManager::create` so grant ids and audit ts_ticks derive
    /// from the seed. Default no-op for mock vaults.
    fn begin_session(&self, session: &SessionId, seed: u64) {
        let _ = (session, seed);
    }

    /// Drop the per-session vault determinism context at session
    /// terminal (close/abort). Default no-op.
    fn end_session(&self, session: &SessionId) {
        let _ = session;
    }

    /// Add an OAuth-only credential. Session-less: the
    /// CLI `loom vault add` has no `--session` flag, and the receipt does
    /// not participate in audit chains until the real OAuth device flow
    /// lands. Allowlisted providers (`OAUTH_PROVIDER_ALLOWLIST`) return a
    /// typed `AddCredentialReceipt` with `status = "oauth_required"`;
    /// non-allowlisted providers reject with
    /// `LoomErrorCode::VaultRejection` and structured context
    /// `{ code: "vault_credential_type_unsupported",
    ///    details.allowed_types: ["oauth2_authorization_code_pkce"] }`.
    fn add_credential(&self, opts: AddCredentialOpts) -> Result<AddCredentialReceipt, LoomError>;

    /// List alive grants, optionally filtered by `session`. "Alive" means
    /// `!revoked` AND `now <= issued_at_ms + ttl_ms`. Empty result is a
    /// valid outcome.
    fn list_grants(&self, session: Option<SessionId>) -> Result<Vec<GrantSnapshot>, LoomError>;

    // ─── Credential-management methods (v0.9.4 W5 full) ───────────────
    // Direct mediation between the CLI's `vault.add` / `vault.delete` /
    // `vault.list_labels` RPCs and the backing keychain. These methods
    // bypass the Grant lifecycle (Grant is per-session use-authorisation;
    // these manage the underlying Credential that grants reference).
    //
    // **Audit semantics.** Each call appends a `SecretOpPending` G5b
    // audit before invoking the keychain, then a typed success/failure
    // G5a audit after the call returns — both go on the same hash chain
    // as Grant lifecycle entries. The audit target is `session`; when
    // `None` (sessionless `loom vault add`) no manifest write occurs and
    // operations are observable only via `tracing::info!`/`tracing::error!`.
    // Backend calls dispatch through the `BlockingKeychain` adapter
    // (`spawn_blocking` + per-op `tokio::time::timeout`) per A-W5.1.

    /// Persist a credential under the given label. CLI safety
    /// (fail-by-default on existing label) lives at the CLI layer per
    /// A-W6.1; the trait method itself silently upserts.
    fn set_secret(
        &self,
        session: Option<&SessionId>,
        label: &str,
        secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), LoomError>;

    /// Fetch a credential by label (CLI's `vault.get` — distinct from
    /// the production-substitution path in `substitute()`). Returns the
    /// raw bytes wrapped in `Zeroizing`. v0.9.4 has no `vault show` CLI
    /// (Non-Goal per D13); this trait method exists for completeness
    /// and for the audit-timing perf test.
    fn get_secret_direct(
        &self,
        session: Option<&SessionId>,
        label: &str,
    ) -> Result<Zeroizing<Vec<u8>>, LoomError>;

    /// Delete a credential. Idempotent at the keychain level. When
    /// `force = false` and one or more active Grants reference `label`
    /// the call fails with `VaultRejection { code: "credential_in_use" }`
    /// and the credential is left intact. When `force = true` each
    /// active Grant is revoked with reason `credential_deleted` (one
    /// `GrantRevoked` audit per grant) before the keychain delete. Per
    /// D29.
    fn delete_secret(
        &self,
        session: Option<&SessionId>,
        label: &str,
        force: bool,
    ) -> Result<DeleteSecretOutcome, LoomError>;

    /// Enumerate stored credential labels for this service_id. Per D14:
    /// no pagination, no per-label audit — ONE `SecretsListed` audit
    /// carrying `{count, service_id}` is appended after the keychain
    /// returns.
    fn list_labels(&self, session: Option<&SessionId>) -> Result<Vec<String>, LoomError>;
}

/// Outcome of `Vault::delete_secret`. `cascade_revoked_grants` is 0
/// unless `force = true` triggered a cascade revoke; the CLI uses it to
/// emit the "deleted (cascade-revoked N grants)" plain-language message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteSecretOutcome {
    pub cascade_revoked_grants: u32,
}

// impl Vault for LocalVault is in impl_local.rs.
