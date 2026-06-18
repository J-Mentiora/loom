// LocalVault helpers — keychain-error translation, the wall-clock + the
// per-session deterministic audit RNG, the `SecretFailureSlot` discriminator,
// and the private `LocalVault` helper `impl` (determinism context + the three
// best-effort audit-append helpers).
//
// Split out of the original `impl_local.rs` (large-file reorganization). All
// items are `pub(super)`/`pub(crate)` so the `vault_impl` trait surface and
// the test module resolve them unchanged.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::manifest_writer::{AuditKind, SessionId};
use crate::vault::audit_payloads::{
    audit_bytes, secret_audit_bytes, secret_reason, SecretAuditPayload, SecretOp, VaultAuditPayload,
};
use crate::vault::vault::{GrantId, LocalVault, VaultSessionCtx};
use loom_keychain::{KeychainError, KeychainErrorKind};
use rand::RngExt;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use std::time::{SystemTime, UNIX_EPOCH};

/// Translate a backend-layer `KeychainError` into a vault-layer `LoomError`
/// per the A-W5.2 inline error-translation table:
///
/// | `KeychainErrorKind`     | `LoomErrorCode`            |
/// |-------------------------|----------------------------|
/// | `NotFound`              | `VaultUnknownLabel`        |
/// | `Denied`                | `VaultPermissionDenied`    |
/// | `Unavailable`           | `VaultBackendUnavailable`  |
/// | `TimedOut`              | `VaultBackendTimeout`      |
/// | `NonInteractivePrompt`  | `VaultNonInteractivePrompt`|
/// | `Internal`              | `VaultInternal`            |
///
/// `Internal` carries the SHA-256-hashed identifier of the original
/// error message in `LoomError.context.internal_hash` so support can
/// correlate to the daemon's `tracing::error!` log (A-W6.3) — the
/// original message itself is never persisted in any session manifest.
pub(super) fn from_keychain_err(err: KeychainError) -> LoomError {
    let code = match err.kind() {
        KeychainErrorKind::NotFound => LoomErrorCode::VaultUnknownLabel,
        KeychainErrorKind::Denied => LoomErrorCode::VaultPermissionDenied,
        KeychainErrorKind::Unavailable => LoomErrorCode::VaultBackendUnavailable,
        KeychainErrorKind::TimedOut => LoomErrorCode::VaultBackendTimeout,
        KeychainErrorKind::NonInteractivePrompt => LoomErrorCode::VaultNonInteractivePrompt,
        KeychainErrorKind::Internal => LoomErrorCode::VaultInternal,
    };
    let internal_hash = err.internal_hash().map(str::to_owned);
    let mut out = LoomError::new(code, err.to_string());
    if let Some(hash) = internal_hash {
        out = out.with_context(serde_json::json!({ "internal_hash": hash }));
    }
    out
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Domain separator for the per-session vault audit RNG — keeps the stream
/// disjoint from the `DeterminismHarness` guest stream
/// (`ChaCha20Rng::seed_from_u64(seed)`), so grant-id draws never shift the
/// guest's `rng_next` sequence.
const VAULT_RNG_DOMAIN: &[u8] = b"loom/vault-audit-v1";

/// Derive the per-session ChaCha20 audit RNG:
/// `from_seed(sha256(VAULT_RNG_DOMAIN || material))` — `material` is the LE
/// session seed, or the session-id bytes for the unregistered fallback.
pub(super) fn vault_session_rng(material: &[u8]) -> ChaCha20Rng {
    use ring::digest::{digest, SHA256};
    let mut input = Vec::with_capacity(VAULT_RNG_DOMAIN.len() + material.len());
    input.extend_from_slice(VAULT_RNG_DOMAIN);
    input.extend_from_slice(material);
    let d = digest(&SHA256, &input);
    let mut key = [0u8; 32];
    key.copy_from_slice(d.as_ref());
    ChaCha20Rng::from_seed(key)
}

/// Internal discriminator for which success-side audit kind a failure
/// belongs to — used to keep the `append_secret_failure` helper compact
/// without three near-identical method bodies.
#[derive(Debug, Clone, Copy)]
pub(super) enum SecretFailureSlot {
    Store,
    Delete,
    Fetch,
}

impl LocalVault {
    /// Run `f` against the session's determinism context, lazily creating
    /// a fallback context (RNG derived from the session-id bytes, tick 0)
    /// when `begin_session` was never called — direct library use and
    /// unit tests construct `LocalVault` without a session manager.
    fn with_det_ctx<R>(&self, session: &SessionId, f: impl FnOnce(&mut VaultSessionCtx) -> R) -> R {
        let mut det = self.det.lock();
        let ctx = det
            .entry(session.clone())
            .or_insert_with(|| VaultSessionCtx {
                rng: vault_session_rng(session.0.as_bytes()),
                tick: 0,
            });
        f(ctx)
    }

    /// Next session-relative audit tick (0-based, one per audit-emitting
    /// vault event) — what `VaultAuditPayload::ts_tick` records; the vault
    /// analogue of the D9 action_id-derived receipt timestamps.
    pub(super) fn next_ts_tick(&self, session: &SessionId) -> u64 {
        self.with_det_ctx(session, |ctx| {
            let tick = ctx.tick;
            ctx.tick += 1;
            tick
        })
    }

    /// Mint the next ULID-shaped grant id for `session` from the seeded
    /// per-session audit RNG — deterministic across same-seed runs.
    pub(super) fn next_grant_id(&self, session: &SessionId) -> GrantId {
        self.with_det_ctx(session, |ctx| {
            let hi = ctx.rng.random::<u64>();
            let lo = ctx.rng.random::<u64>();
            let bits = (u128::from(hi) << 64) | u128::from(lo);
            GrantId(ulid::Ulid::from(bits).to_string())
        })
    }

    /// Revoke a single `(session, grant)` entry: flip `revoked` and append
    /// the GrantRevoked audit to that session's chain. Used by `revoke`
    /// (which pre-resolves keys) and the `delete_secret --force` cascade.
    pub(super) fn revoke_entry(&self, key: &(SessionId, GrantId)) {
        let (label, origin, scopes) = {
            let mut grants = self.grants.write();
            let Some(g) = grants.get_mut(key) else { return };
            g.revoked = true;
            (g.label.clone(), g.origin.clone(), g.scopes.clone())
        };
        let (session, grant) = key;
        let payload = VaultAuditPayload {
            grant_id: &grant.0,
            origin: &origin,
            credential_label: &label,
            requested_scopes: &scopes,
            result: "revoked",
            triggering_action_id: None,
            ts_tick: self.next_ts_tick(session),
        };
        let _ = self.manifest_writer.append_audit(
            session.clone(),
            AuditKind::GrantRevoked,
            audit_bytes(&payload),
        );
    }

    /// Append a G5b `SecretOpPending` audit for the named op. No-op when
    /// `session` is `None` (sessionless CLI flow). Failures to write the
    /// audit are logged-and-swallowed — same convention as the existing
    /// `grant`/`consume`/`revoke` flows above.
    // All three audit helpers below (`append_secret_op_pending`,
    // `append_secret_audit`, `append_secret_failure`) intentionally swallow
    // `manifest_writer::append_audit` errors after a `tracing::warn!` echo.
    // Per the threat model (G5a post-op + G5b pre-op intent), audit is
    // best-effort observability — NOT a security gate. A full disk or a
    // transiently-broken WAL must not abort the user's vault operation,
    // because the operation either already happened (G5a) or is about to
    // happen anyway (G5b). Operators correlate audit-write failures via
    // the `warn`-level log; the absence of an audit entry is itself
    // observable.
    pub(super) fn append_secret_op_pending(
        &self,
        session: Option<&SessionId>,
        label: &str,
        op: SecretOp,
    ) {
        let Some(session) = session else { return };
        let payload = SecretAuditPayload::OpPending { label, op };
        let bytes = secret_audit_bytes(&payload);
        if let Err(e) =
            self.manifest_writer
                .append_audit(session.clone(), AuditKind::SecretOpPending, bytes)
        {
            tracing::warn!(error = %e, "append SecretOpPending failed");
        }
    }

    /// Append a success-side audit (`SecretStored`/`SecretFetched`/
    /// `SecretDeleted`/`SecretsListed`/`SecretReplaced`). No-op when
    /// `session` is `None`.
    pub(super) fn append_secret_audit(
        &self,
        session: Option<&SessionId>,
        kind: AuditKind,
        payload: &SecretAuditPayload<'_>,
    ) {
        let Some(session) = session else { return };
        let bytes = secret_audit_bytes(payload);
        let kind_for_log = kind.clone();
        if let Err(e) = self
            .manifest_writer
            .append_audit(session.clone(), kind, bytes)
        {
            tracing::warn!(error = %e, kind = ?kind_for_log, "append secret audit failed");
        }
    }

    /// Append a failure-side audit (`Secret*Failed`). The original error
    /// message is hashed into `internal_hash` for support correlation
    /// (A-W6.3) — operators paste the hash into the daemon log to
    /// recover the original message; the message itself never reaches
    /// the persistent manifest.
    pub(super) fn append_secret_failure(
        &self,
        session: Option<&SessionId>,
        kind: AuditKind,
        label: &str,
        err: &KeychainError,
        slot: SecretFailureSlot,
    ) {
        // A-W6.3 + D30: structured tracing::error! echo with ONLY the
        // internal_hash for Internal-kind errors. The original message
        // is intentionally elided from the daemon log — per D30 the
        // hash is the correlation handle; the message itself never
        // travels to a persistent surface (audit chain, daemon log).
        // Support recovers the message by pasting the hash into the
        // original keychain backend's diagnostic channel (per the
        // `docs/loom-vault-audit.md` runbook).
        if matches!(err.kind(), KeychainErrorKind::Internal) {
            tracing::error!(
                internal_hash = err.internal_hash().unwrap_or("<missing>"),
                "vault.{} internal error",
                match slot {
                    SecretFailureSlot::Store => "set_secret",
                    SecretFailureSlot::Delete => "delete_secret",
                    SecretFailureSlot::Fetch => "get_secret",
                }
            );
        }

        let Some(session) = session else { return };
        let reason = secret_reason(err);
        let internal_hash = err.internal_hash();
        let payload = match slot {
            SecretFailureSlot::Store => SecretAuditPayload::StoreFailed {
                label,
                reason,
                internal_hash,
            },
            SecretFailureSlot::Delete => SecretAuditPayload::DeleteFailed {
                label,
                reason,
                internal_hash,
            },
            SecretFailureSlot::Fetch => SecretAuditPayload::FetchFailed {
                label,
                reason,
                internal_hash,
            },
        };
        let bytes = secret_audit_bytes(&payload);
        let kind_for_log = kind.clone();
        if let Err(e) = self
            .manifest_writer
            .append_audit(session.clone(), kind, bytes)
        {
            tracing::warn!(error = %e, kind = ?kind_for_log, "append secret failure audit failed");
        }
    }
}
