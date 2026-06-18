// LocalVault implementation — vault-core feature.
//
// Implements `Vault` for `LocalVault` declared in `interfaces.rs`.
//
// Invariants enforced here:
//   - raw secret bytes appear ONLY in substitute(), parked in
//     req.authorization (Redacted<Zeroizing<String>>): zeroized on drop,
//     [REDACTED] in Debug/Display, skipped by Serialize (G1/TB4).
//   - OAuth-only at v1.
//   - 4-check sequence in substitute(): revoked → origin → scopes → ttl.
//   - every vault event appends a typed audit entry via ManifestWriter::append_audit.
//   - audit payload material is deterministic per session (NFR-DET-01):
//     seeded grant-id sequence + session-relative ts_tick — never now_ms()
//     or OS entropy, which would poison the chain-hashed canonical bytes.
//
// Large-file split (behavior-preserving module reorganization):
//   - `helpers`     — free functions (keychain-error translation, the
//                     wall-clock + per-session deterministic RNG), the
//                     `SecretFailureSlot` discriminator, and the private
//                     `LocalVault` helper `impl` (determinism context +
//                     audit-append helpers).
//   - `vault_impl`  — the `impl Vault for LocalVault` trait surface.
//   - `tests`       — the `#[cfg(test)]` unit suite.
//
// Audit-payload DTOs + JCS serialization live one level up in
// `vault/audit_payloads.rs` (constructed only by this impl).

mod helpers;
mod vault_impl;

#[cfg(test)]
mod tests;

// Re-exports so the `tests` submodule's `use super::*;` resolves the two
// symbols it pulls from the original module scope (`LoomError` for an
// explicit `Result<(), LoomError>` annotation; `now_ms` for the
// active-grant fixture) — keeps the test import style unchanged.
#[cfg(test)]
use crate::error::LoomError;
#[cfg(test)]
use helpers::now_ms;
