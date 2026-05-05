// Vault — credential mediation. Stays in `loom-core`.
//
// # Contract semantics
// - **Vault isolation.** Raw secret bytes
//   appear in EXACTLY ONE call site: `Vault::substitute`. The bytes
//   live in a `Zeroizing<Vec<u8>>` local, are written into the outbound
//   `NetRequest::headers["Authorization"]` slot, and zeroize on drop.
//   They never appear in the returned `NetResp`, never in logs, never
//   in the manifest, never cross the WASM boundary.
// - **Substitution mutates in-place.** `substitute(&mut NetRequest)` per
//   the per-task instructions: the substitution writes to `req.headers`
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

use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Opaque grant token visible to WASM. ULID-shaped string; carries no
/// secret material.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantId(pub String);

/// Network request struct. WIT-derived. The shape shown
/// here is the wit-bindgen output; all numeric fields are integers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetRequest {
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
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

/// Supported credential types. Only `OAuth` is allowed at v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    OAuth,
    ApiKey,
    Saml,
    Basic,
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

/// External keychain interface. The IMPL of this trait lives in the
/// out-of-crate, feature-gated `loom-keychain` crate (no platform
/// symbols inside `loom-core`).
pub trait KeychainAccess: Send + Sync {
    /// Return the raw secret bytes for `label`, wrapped in `Zeroizing`.
    /// The returned buffer drops zeroize when it leaves scope.
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, LoomError>;
}

/// Concrete Vault implementation.
pub struct LocalVault {
    pub(crate) keychain: Arc<dyn KeychainAccess>,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) obs: Arc<Observability>,
    pub(crate) grants: parking_lot::RwLock<BTreeMap<GrantId, Grant>>,
}

impl LocalVault {
    pub fn new(
        keychain: Arc<dyn KeychainAccess>,
        manifest_writer: Arc<dyn ManifestWriter>,
        obs: Arc<Observability>,
    ) -> Self {
        Self {
            keychain,
            manifest_writer,
            obs,
            grants: parking_lot::RwLock::new(BTreeMap::new()),
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
    /// secret bytes.** Called by `loom-host`'s `net_request` host-fn ONLY.
    ///
    /// Pre: grant alive ∧ origin match ∧ scopes ⊇ req.scopes ∧ ttl
    /// not exceeded.
    /// Post: req.headers["Authorization"] mutated; GrantConsumed audit
    /// entry appended; secret bytes zeroized on function exit.
    fn substitute(&self, grant: GrantId, req: &mut NetRequest) -> Result<(), LoomError>;

    /// Revoke a grant. Subsequent `substitute` returns VaultGrantRevoked.
    fn revoke(&self, grant: GrantId, reason: RevokeReason) -> Result<(), LoomError>;

    /// Add an OAuth-only credential. Session-less: the
    /// CLI `loom vault add` has no `--session` flag, and the receipt does
    /// not participate in audit chains until the real OAuth device flow
    /// lands. Allowlisted providers (`OAUTH_PROVIDER_ALLOWLIST`) return a
    /// typed `AddCredentialReceipt` with `status = "oauth_required"`;
    /// non-allowlisted providers reject with
    /// `LoomErrorCode::VaultRejection` and structured context
    /// `{ code: "vault_credential_type_unsupported",
    ///    details.allowed_types: ["oauth2_authorization_code_pkce"] }`.
    fn add_credential(
        &self,
        opts: AddCredentialOpts,
    ) -> Result<AddCredentialReceipt, LoomError>;

    /// List alive grants, optionally filtered by `session`. "Alive" means
    /// `!revoked` AND `now <= issued_at_ms + ttl_ms`. Empty result is a
    /// valid outcome.
    fn list_grants(
        &self,
        session: Option<SessionId>,
    ) -> Result<Vec<GrantSnapshot>, LoomError>;
}

// impl Vault for LocalVault is in impl_local.rs.
