# Vault Threat Model

This document defines the security model for the Loom credential vault — an in-process,
grant-mediated credential store that mediates OAuth token access for agent sessions.

## Attacker Classes

### A1 — Compromised WASM Guest
A guest module (web surface, tool) running inside a Wasmtime sandbox that has been
compromised by a malicious website or code injection. The attacker controls the WASM
linear memory and can call host functions exposed via WIT bindings.

**Capabilities:** Call any exposed host function with attacker-controlled arguments.
Cannot access host memory outside the WASM linear memory boundary.

**Goal:** Obtain raw OAuth token bytes to exfiltrate credentials.

### A2 — Malicious Action Script
A user-supplied action script (e.g. a `playwright-import` converted script) that
attempts to escalate privileges beyond its declared scope/origin.

**Capabilities:** Issue vault.grant() requests via the RPC/CLI layer with
attacker-controlled scopes and origins.

**Goal:** Issue grants with elevated scopes or different origins than intended.

### A3 — Log/Manifest Scraper
An attacker with read access to the session manifest, log files, or exported artifacts
(e.g. a compromised CI system or a shared machine with inadequate file permissions).

**Goal:** Find OAuth token material in logs, manifests, or debug output.

### A4 — Process Memory Reader
An attacker with access to the loom daemon process memory (via ptrace, /proc/mem,
or a memory corruption exploit in another crate).

**Goal:** Read heap memory containing token bytes before they are zeroized.

## Security Goals

### G1 — Token Isolation
Raw OAuth token bytes NEVER appear in: manifest entries, log output, RPC responses,
WASM linear memory, or grant receipts. The only location raw bytes exist is the
`substitute()` call frame, briefly held in a `Zeroizing<Vec<u8>>` buffer.

### G2 — Scope Enforcement
A grant issued for `origin=X, scopes=[S]` cannot be used for `origin=Y` or for
`scopes=[S, S2]`. Enforcement happens before DNS resolution / socket connect.

### G3 — TTL Enforcement
Grants are time-limited. Once a grant's TTL expires, `substitute()` returns
`VaultGrantExpired`. In-flight network operations using an expired grant are
cancelled within 250ms (enforced at the loom-host network layer).

### G4 — OAuth-Only at v1
Only `oauth2_authorization_code_pkce` credential type is accepted. API keys,
SAML assertions, and Basic Auth credentials are rejected at `grant()` time,
forcing callers to use the OAuth PKCE flow.

### G5 — Audit Completeness
Every vault lifecycle event (grant_issued, grant_consumed, grant_expired,
grant_revoked, secret_fetched_from_keychain) produces a typed audit entry
in the session manifest's hash chain within 100ms.

## Trust Boundaries

### TB1 — WASM/Host boundary (PRIMARY)
The WIT interface is the primary trust boundary. WASM guests never see raw tokens.
The `Vault::substitute()` function is called ONLY from `loom-host`'s `net_request`
host function, which is the sole authorized caller.

### TB2 — Keychain boundary
The OS keychain (macOS Keychain, Linux Secret Service, Windows Credential Manager)
is the authoritative secret store. The vault crate never writes tokens to disk in
loom's own storage — it only fetches from the OS-managed store.

### TB3 — RPC/CLI boundary
The RPC server and CLI accept vault.grant() requests. All grant requests must carry
`threat_model_acknowledged: true` and valid session_id. The RPC auth middleware
(loom-rpc) enforces session authentication before any vault call reaches loom-core.

### TB4 — Manifest/log boundary
Manifests and logs are potentially readable by third parties (CI systems, operators,
telemetry pipelines). No secret material crosses this boundary — verified by the
presence test in AC-NFR-SEC-01.1 (grep for token substrings).

## Abuse Cases

### AB1 — WASM guest calls substitute() directly
**Scenario:** Compromised WASM guest tries to call a vault substitute API.
**Mitigated by:** TB1 — the `substitute()` function is NOT exposed via WIT.
Only the RPC/CLI path exposes vault.grant(). The `net_request` host function
calls substitute() internally; WASM cannot invoke it directly.

### AB2 — Origin laundering via grant reuse
**Scenario:** Attacker obtains a grant for `api.github.com` and tries to use it
against `api.github.com.evil.com` or `gist.github.com`.
**Mitigated by:** Exact-match origin check in substitute() (G2). Subdomains are
different origins and are rejected.

### AB3 — TTL manipulation
**Scenario:** Attacker tries to use a grant after TTL expiry by manipulating system clock.
**Mitigated by:** TTL computed from `issued_at_ms` (vault-controlled timestamp) + ttl_ms.
If the system clock is advanced, `now_ms() > issued_at_ms + ttl_ms` triggers expiry.
If the clock is rewound, the grant may appear valid for longer — acceptable within threat model
(clock tampering requires elevated privilege, at which point the attacker has wider access).

### AB4 — Secret grep in manifest
**Scenario:** Attacker reads the session manifest looking for token bytes.
**Mitigated by:** G1 + G5. No token bytes ever appear in `VaultAuditPayload` or any
manifest entry. Verified by AC-NFR-SEC-01.1 grep test.

### AB5 — Scope creep via grant recycling
**Scenario:** Attacker obtains a legitimate grant for `["repo:read"]` and tries to
use it for an action requiring `["repo:write"]`.
**Mitigated by:** Scope superset check in substitute() (G2). The request's required
scopes are checked against the grant's declared scopes; escalation returns
`VaultRejection` with `vault-scope-insufficient` context.
