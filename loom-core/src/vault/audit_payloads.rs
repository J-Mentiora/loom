// Vault audit payloads — JCS-serialized DTOs for vault lifecycle audit entries.
//
// Extracted from `impl_local.rs` (cleanup: large-file split). These types NEVER
// carry raw secret bytes; they are JCS-encoded and embedded in
// `ManifestEntry::AuditEntry::canonical_bytes`, so their on-wire shape is
// hash-chain-load-bearing (NFR-DET-01) — do not reorder fields or rename serde
// tags without accounting for replay-equality. All items are `pub(crate)`:
// constructed and consumed only by the `LocalVault` impl.

use crate::vault::vault::SizeBucket;
use loom_keychain::{KeychainError, KeychainErrorKind};
use serde::{Deserialize, Serialize};

/// Parse cookie names from a raw vault blob without holding raw values
/// beyond this function. The blob shape is
/// `{"schema_version": 1, "cookies": [{"name": "...", ...}, ...]}` per
/// the CLI `vault add --credential-type cookie --from-file` contract.
///
/// On parse failure (blob is malformed JSON or missing the cookies array)
/// returns an empty Vec — the daemon's CDP encode step will surface the
/// parse error to the caller via the typed surface error; the audit gets
/// an empty `cookie_names` array rather than a partial / wrong list.
pub(crate) fn extract_cookie_names(blob: &[u8]) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_slice(blob) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("cookies")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Canonical audit payload for vault lifecycle events.
/// Serialized to JCS bytes.
/// NEVER contains raw secret bytes.
///
/// Every field is deterministic per session (NFR-DET-01): `grant_id` is the
/// seeded per-session sequence; `ts_tick` is the 0-based session-relative
/// vault event tick (`VaultSessionCtx::tick`), NOT wall-clock ms.
#[derive(Serialize)]
pub(crate) struct VaultAuditPayload<'a> {
    pub(crate) credential_label: &'a str,
    pub(crate) grant_id: &'a str,
    pub(crate) origin: &'a str,
    pub(crate) requested_scopes: &'a [String],
    pub(crate) result: &'static str,
    pub(crate) triggering_action_id: Option<u64>,
    pub(crate) ts_tick: u64,
}

pub(crate) fn audit_bytes(payload: &VaultAuditPayload<'_>) -> Vec<u8> {
    // JCS (sorted keys) required for hash-chain integrity.
    serde_jcs::to_string(payload)
        .unwrap_or_else(|_| serde_json::to_string(payload).unwrap_or_default())
        .into_bytes()
}

// ─── v0.9.4 credential-lifecycle audit payloads (W5.3) ────────────────
//
// `SizeBucket` + `size_bucket()` (D24) live in `vault.rs` as the single source
// of truth shared with the daemon RPC receipt — imported above.

/// Wire-stable category of a `KeychainError` for audit-entry payloads
/// (D30 typed-reason requirement). Mirrors `KeychainErrorKind` but is
/// serialised as snake_case strings inside `SecretAuditPayload::*Failed`
/// variants — NOT free-form messages, which could leak third-party
/// error text into the persistent hash chain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretReason {
    NotFound,
    Denied,
    Unavailable,
    TimedOut,
    NonInteractivePrompt,
    Internal,
}

pub(crate) fn secret_reason(err: &KeychainError) -> SecretReason {
    match err.kind() {
        KeychainErrorKind::NotFound => SecretReason::NotFound,
        KeychainErrorKind::Denied => SecretReason::Denied,
        KeychainErrorKind::Unavailable => SecretReason::Unavailable,
        KeychainErrorKind::TimedOut => SecretReason::TimedOut,
        KeychainErrorKind::NonInteractivePrompt => SecretReason::NonInteractivePrompt,
        KeychainErrorKind::Internal => SecretReason::Internal,
    }
}

/// Discriminator for the pre-op `SecretOpPending` audit (D8 G5b).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretOp {
    Set,
    Get,
    Delete,
    List,
}

/// Tagged audit payload for credential lifecycle. JCS-encoded via
/// `secret_audit_bytes` and embedded in `ManifestEntry::AuditEntry::canonical_bytes`.
///
/// **Never carries raw secret bytes.** `Stored` reports a 3-tier
/// `size_bucket` (D24) rather than `byte_count` to eliminate the
/// exact-size side-channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
#[allow(dead_code)] // OwnerChanged is emitted by Linux-backend audit wiring (W5 follow-up)
pub(crate) enum SecretAuditPayload<'a> {
    OpPending {
        label: &'a str,
        op: SecretOp,
    },
    Stored {
        label: &'a str,
        size_bucket: SizeBucket,
        replaced: bool,
    },
    Fetched {
        label: &'a str,
    },
    Deleted {
        label: &'a str,
        cascade_revoked_grants: u32,
    },
    Listed {
        count: u32,
        service_id: &'a str,
    },
    StoreFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    DeleteFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    FetchFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    PromptBlocked {
        label: &'a str,
        op: SecretOp,
    },
    OwnerChanged {
        pinned: &'a str,
        current: &'a str,
    },
}

pub(crate) fn secret_audit_bytes(payload: &SecretAuditPayload<'_>) -> Vec<u8> {
    serde_jcs::to_string(payload)
        .unwrap_or_else(|_| serde_json::to_string(payload).unwrap_or_default())
        .into_bytes()
}
