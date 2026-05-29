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

v0.9.4 splits G5 into two complementary halves so the SLA is testable:

#### G5a — post-op audit latency (testable invariant)
`audit_appended_ts - keychain_returned_ts < 100ms (p95)`,
`< 500ms (max)`. Measured by `loom-core/tests/audit_timing.rs` over
1000 iterations against `InMemoryKeychain`. CI fails on regression.

#### G5b — pre-op intent audit
Every credential op appends a `secret_op_pending{label, op}` audit
within 100ms of operator intent (the RPC arriving) and BEFORE any
blocking OS-keychain call. Operators reading the audit chain can
correlate intent → outcome even when the OS prompt blocked for an
unbounded duration. Implemented at
`loom-core/src/vault/impl_local.rs::set_secret/get_secret_direct/delete_secret`.

## Trust Boundaries

### TB1 — WASM/Host boundary (PRIMARY)
The WIT interface is the primary trust boundary. WASM guests never see raw tokens.
The `Vault::substitute()` function is called ONLY from `loom-host`'s `net_request`
host function, which is the sole authorized caller.

### TB2 — Keychain boundary
The OS keychain (macOS Keychain, Linux Secret Service, Windows Credential Manager)
is the authoritative secret store. The vault crate never writes tokens to disk in
loom's own storage — it only fetches from the OS-managed store.

**No caching.** Every `keychain.get_secret()`, `keychain.set_secret()`,
`keychain.delete_secret()`, and `keychain.list_labels()` invocation is a
fresh OS call. Caller-side caching of credential bytes is forbidden by
this trust boundary; the only short-lived in-memory copy is the
`Zeroizing<Vec<u8>>` held by `Vault::substitute()` for the duration of
a single `net_request` host-fn call.

### TB3 — RPC/CLI boundary
The RPC server and CLI accept vault.grant() requests. All grant requests must carry
`threat_model_acknowledged: true` and valid session_id. The RPC auth middleware
(loom-rpc) enforces session authentication before any vault call reaches loom-core.

### TB4 — Manifest/log boundary
Manifests and logs are potentially readable by third parties (CI systems, operators,
telemetry pipelines). No secret material crosses this boundary — verified by the
presence test in  (grep for token substrings).

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
manifest entry. Verified by  grep test.

### AB5 — Scope creep via grant recycling
**Scenario:** Attacker obtains a legitimate grant for `["repo:read"]` and tries to
use it for an action requiring `["repo:write"]`.
**Mitigated by:** Scope superset check in substitute() (G2). The request's required
scopes are checked against the grant's declared scopes; escalation returns
`VaultRejection` with `vault-scope-insufficient` context.

### AB6 — Same-user process exfiltration (v0.9.4 / D27)
**Scenario:** A different process running under the same UID as `loom-daemon`
(e.g. a malicious npm postinstall script the operator runs as themselves)
reads the OS keychain entries loom owns.
**Threat:** On macOS the login-keychain ACL is per-application by default,
but a same-user process can install its own ACL entry. On Linux, the
Secret Service has no per-process ACL — any same-user process can read
items it knows the attributes of.
**Mitigated by:**
  - **macOS:** `kSecAttrAccessible = WhenUnlockedThisDeviceOnly` +
    `kSecAttrSynchronizable = false` (D23) limit availability to the
    unlocked, on-device session. **v0.9.4 known gap:** the
    high-level `passwords::*` API doesn't expose those attributes;
    the lower-level `ItemAddOptions` path is tracked as a fast
    follow-up. See `loom-keychain/src/macos.rs:17-29`.
  - **Linux:** D-Bus owner-pinning of `org.freedesktop.secrets` at
    daemon startup + per-op `GetNameOwner` re-check (A-W3.1) blocks
    the bus-name-hijack variant.
  - **Audit trail:** every credential op emits a `secret_op_pending`
    + `secret_*_failed` audit chain that lets operators detect
    after-the-fact misuse.
  - **RPC uid-match via socket perms (TB3):** the daemon's Unix
    socket is `0600`, owned by the daemon's UID — no other UID can
    connect at all. Cross-UID attacks are out of scope.

**Accepted-risk rationale:** mitigating same-UID reads via per-op OS
prompts breaks daemon-driven token substitution (the substitute path
is non-interactive by design). Operators who need stricter isolation
should run `loom-daemon` under a dedicated service UID.

### AB7 — Label injection into audits / renderers (v0.9.4 / D37)
**Scenario:** Attacker constructs a label like
`my-token[31mEVIL[0m` or `my-token\n[FAKE_AUDIT]` and
passes it via `loom vault add --label …`. If accepted, the malicious
bytes flow into the audit-chain payload OR into a TTY pretty-render,
forging visual context.
**Mitigated by:**
  - **CLI boundary (D37):** canonical regex `^[A-Za-z0-9:_-]{1,64}$`
    rejects non-printables, ANSI escapes, and overlong labels with
    exit code 2. Implemented at
    `loom-cli/src/vault_commands/vault_commands.rs::validate_label_cli`.
  - **Daemon wire boundary:** same regex re-checked at
    `loom-daemon/src/lib.rs::validate_label_canonical`.
  - **Manifest-writer defense-in-depth (A-W8.5):**
    `manifest_writer::append_audit` rejects any `Secret*` payload
    whose `label` field violates the canonical policy with
    `VaultInvalidLabel`. A future code path that bypasses CLI
    validation cannot silently slip a malformed label into the
    hash-chained audit.

### AB8 — Service-id squatting (v0.9.4 / A-W2.1 deferred)
**Scenario:** Another same-user process writes a macOS keychain item
with `kSecAttrService = "loom"` (the well-known service id) to confuse
loom's enumeration or to plant a credential loom would later treat as
its own.
**Planned mitigation:** add a `kSecAttrCreator = 0x4C4F4F4D` (FourCharCode
`'LOOM'`) discriminator to every `SecItemAdd` and require it on every
`SecItemCopyMatching`. Items written by other processes claiming
`service = "loom"` are filtered out of `list_labels` and return
`NotFound` from `get_secret`.
**Status:** **DEFERRED to v0.9.5** — the high-level `passwords::*` API
the v0.9.4 macOS backend uses doesn't expose `kSecAttrCreator`. The
deferral sits within the AB6 same-user-process accepted-risk band.
Tracked alongside the `kSecAttrAccessible` / `kSecAttrSynchronizable`
follow-up. See `loom-keychain/src/macos.rs:25-29`.

## SOC 2 / ISO 27001 control mapping (D35 / A-W8.4)

This table reframes "claimed controls" against current implementation
references and known gaps. Auditors reading this should treat the
"Known gaps" column as the authoritative scope statement.

| Control | Claim | Implementation | Known gaps |
|---|---|---|---|
| **SOC 2 CC6.1** (logical access — identity + access controls) | RPC socket is bound at mode `0600` in a per-user runtime directory; only the daemon-owning UID can establish a connection. **NOT a full peer-UID match** — same-UID processes share access (documented under AB6 as accepted residual risk for v0.9.4). Credential reads are gated through `Vault::substitute`, not a generic getter. | `loom-rpc/src/socket_server/socket_server.rs::SOCKET_MODE = 0o600`; `loom-daemon/src/lib.rs` 0600 startup probe on `hello.token` / `daemon.pid` (A-W8.1 / W8.5); `loom-core/src/vault/vault.rs::substitute()` is the sole raw-bytes call site. | Same-UID processes share the daemon's read access to the keychain — AB6 accepted-risk band. Per-peer-UID `SO_PEERCRED` enforcement (D42) is tracked for v0.9.5. |
| **SOC 2 CC6.7** (data-in-transit) | All vault RPC traffic stays on the local Unix socket; the daemon never opens a network listener for vault.* methods. | `loom-rpc/src/socket_server/mod.rs` — Unix-domain socket bound to a path under the operator's HOME. | No TLS — by design, the boundary is the local OS. Cross-machine deployments must tunnel through ssh or equivalent. |
| **SOC 2 CC7.2** (system operations monitoring) | Every vault lifecycle event appends a typed audit entry to the per-session hash-chained WAL within 100ms (G5a/G5b). | `loom-core/src/vault/impl_local.rs` G5a/G5b wiring; `loom-core/tests/audit_timing.rs` enforces the p95 < 100ms / max < 500ms SLA at CI time. | No cross-session aggregation — operators `jq` across `~/.local/share/loom/sessions/*` per the audit-doc runbook. Retention rotation deferred (D35e). |
| **ISO 27001 A.9.4.1** (secret authentication info management) | Tokens never appear in any persisted file: G1 invariant. `vault diagnose` exposes init status + label count without leaking secret bytes. | `loom-keychain/src/{macos,linux}.rs` delegate to OS keychain; `loom-cli/tests/keychain_e2e_hermetic.rs` byte-scans the data_root for a canary to enforce G1; `loom-rpc/src/core_service_adapter/core_service_adapter.rs::VaultDiagnoseInfo`. | Cross-DE Linux Secret Service (KDE kwallet, KeePassXC) not supported in v0.9.4 (D36) — documented as a known gap, not a control failure. |
| **ISO 27001 A.12.4.1** (event logging) | Audit chain is append-only and hash-chained — selective deletion of credential entries would break chain integrity. Audit entries can be validated post-hoc via `loom session verify <id>`. | `loom-core/src/manifest_writer/impl_local.rs::validate`; manifest WAL is fsynced on every append. | Session-level erasure (PRIVACY-doc-grade GDPR support) loses the audit trail by design — audit-entry-level erasure is incompatible with the hash-chain model (D35 / FND-0036). |

## Cookie credentials (v0.9.7 web-cookie-injection)

The v0.9.7 release ships first-class support for storing browser
cookies in the vault and substituting them at action-dispatch time.
This section enumerates the threat model amendments that apply to the
cookie credential type, which are additive to the OAuth-token mitigations
above (the AB1–AB8 abuse cases apply identically to cookie credentials).

### Lawful basis (D6 / FND-0010)

Cookies enter the vault only via explicit operator action through
`loom vault add --credential-type cookie --from-file <PATH>` or
`--from-stdin`. The vault never auto-acquires cookie material from a
browser session, and there is no "import current Chrome session" path.
Lawful basis for storage is therefore operator contractual / legitimate
interest. Operators retain full control over the on-disk source file —
the CLI reads it, does not write it back, and does not delete it
(see "Caveats" below).

### Retention (D6 / FND-0011)

v0.9.7 ships **no automatic expiry** on cookie grants. The TTL passed
to `vault.grant` (default 30 days from the CLI cookie flow) only gates
*grant validity*, not *secret deletion*; deleting the underlying cookie
blob is the operator's responsibility via
`loom vault delete <label>`. A grant-level TTL alone is insufficient
because RFC 6265 cookies have their own `expires` / `max-age` semantics
that the browser will enforce on the eventual `set_cookies` dispatch
anyway. Automatic per-grant expiry is tracked as a v0.10.x follow-up.

### Session binding (D5 / FND-0008)

Cookie grants are session-bound — each grant carries the `SessionId` it
was issued under, and `Vault::substitute_cookies(grant, session)`
rejects with `LoomErrorCode::VaultOriginViolation` when the consuming
session does not match. This prevents a grant issued in one operator
session from being silently consumed by another (e.g. a parallel CI run
sharing the same daemon). The wire envelope for the rejection is the
canonical `vault_session_mismatch` shape; the CLI surfaces the typed
error rather than collapsing to a generic 500.

The current session-binding mechanism uses `SessionId` equality — i.e.
the operator's `SessionId` string. A more robust binding (using an
unforgeable opaque session handle immune to cross-process forgery) is
tracked as a separate hardening PR; the same-daemon scope is documented
under AB6 (same-UID same-daemon shared-trust band).

### Audit chain

Two new `AuditKind` variants are emitted on cookie-relevant operations:

- **`CookiesSubstituted`** is appended by `Vault::substitute_cookies`
  on successful grant resolution. Canonical fields:
  `{grant_id, session_id, cookie_names}`. Cookie *names* land in the
  audit chain BY DESIGN because they're needed for replay determinism
  (D5) and for `loom session verify` to validate that a replayed
  cookie-bearing action references the same cookies the recording did.
  Cookie *values* never appear in the audit chain — Redacted serialise
  pipeline + the §10 wipe path together guarantee this.
- **`CookiesCleared`** is appended by the verb-side
  `host::log_emit("CookiesCleared", {session_id, count_before})` call
  in `ClearCookiesVerb::execute`. Emitted BEFORE the destructive CDP
  `Network.clearBrowserCookies` so the audit chain captures the
  pre-clear count even if the clear call itself fails (D9 / FND-0050).
  Canonical fields: `{target_id, session_id, count_before}`. No cookie
  names land here (only the count); operators wanting a name-level
  record before clearing should issue `web.get_cookies` first.

### Caveats (D7 — per operator instruction)

Two intentional limitations operators should be aware of:

1. **No shred-after-read on the input file.** The CLI reads the cookie
   blob bytes from the operator-supplied `--from-file <PATH>` and drops
   the in-memory copy after `vault.set_secret` succeeds, but it does
   NOT delete or scramble the on-disk file. This is by D7 — the
   operator owns the file's lifecycle. Operators must either pass
   `--from-stdin` (no on-disk artifact) or delete the file themselves
   after `loom vault add` returns. CI systems writing cookies.json to
   `mktemp` directories and `rm`-ing them on shell exit are well within
   the design intent.
2. **`get_cookies` receipts contain raw values.** The
   operator-facing JSON receipt returned over JSON-RPC carries the
   actual cookie values for `web.get_cookies`. This is D7 — the verb's
   purpose is grant inspection and replay-fidelity checks; redacting
   values at the receipt level would defeat the use case. Structured
   *tracing logs* of the same call ARE scrubbed via
   `mcp_observability::redact_cookie_paths_in_place`, so log
   aggregators never see raw values even when receipts carry them.
   The split is:
   - Wire receipt JSON returned to MCP/CLI → RAW values.
   - tracing events that mirror the tools/call → scrubbed values.
   The `redact_vault: bool` toggle controls log scrubbing for both
   OAuth tokens and cookie values (D11 — same credential family).

### Heap-wipe boundary (§10 / D12)

`Vault::substitute_cookies` returns `Zeroizing<Vec<u8>>` (shipped
v0.9.5) — the keychain blob bytes are wiped on drop in daemon memory.
v0.9.7 closes the v0.9.5 D12 deferral by adding the surface-side
companion: when `SetCookiesVerb::execute` is about to drop the
`Vec<NetworkCookieParam>` after dispatching CDP `Network.setCookies`,
it iterates over each cookie's value `Redacted<String>` and calls
`loom_shared::wipe_string_buffer_in_place(value.expose_mut())`. The
helper uses `String::as_mut_vec().iter_mut()` to overwrite every byte
in the heap allocation with `0` before `Vec::clear()` resets the
length. This addresses the well-known limitation that `String::zeroize`
from `zeroize 1.6+` (which loom-shared::Redacted calls on Drop) only
sets the length to 0 without overwriting the allocated bytes — so
process memory could otherwise retain cookie values until allocator
reuse.

The wipe is **best-effort**:

- It addresses the specific buffer pointed to by the String at the
  call site. A heap realloc that happened earlier in the value's
  lifetime may have left residues elsewhere; those are not wiped.
- The encoder (`CdpMessageEncoder::encode`) clones + serialises cookie
  values into the CBOR envelope handed to `host::shim_call`; that
  intermediate buffer is in WIT-marshalled memory we cannot wipe from
  the guest. The chromium shim subprocess owns the bytes once they
  cross the boundary; cleanup there depends on the shim's own
  zeroization discipline.
- This is a tightening, not an absolute guarantee. The threat-model
  improvement is reducing the window during which cookie values are
  recoverable from process memory dumps captured after the verb returns
  — not eliminating it.

A unit test
(`loom-shared::redacted::tests::wipe_string_buffer_in_place_zeros_the_underlying_bytes`)
verifies the helper writes zeros into the underlying buffer by
capturing the buffer pointer + capacity BEFORE the wipe, then reading
the bytes-at-pointer via `slice::from_raw_parts` after the wipe (safe
because `clear()` does not free the buffer). This is the most-direct
in-process verification possible without a full debugger / mprotect
harness.
