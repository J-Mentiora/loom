# Changelog

All notable changes to loom are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.6] — 2026-05-29 — Cookie Injection (ship milestone)

Closes the **web-cookie-injection** deferral list from v0.9.5. The four
MCP cookie verbs are now reachable end-to-end (`tools/list` →
`tools/call` → daemon → WASM verb → CDP → chromium). Downstream
consumers (notably `mentiora-ai/agentic-test-studio`) can pin
`loom-mcp@0.9.6` and ship the `auth_cookie:` frontmatter feature.

### Added

- **Verb-level `execute()` implementations** for the four cookie
  verbs in `loom-surfaces`. SetCookies resolves `CookieSource::Inline`
  directly and `CookieSource::Grant` via the new
  `host::vault_substitute_cookies` host-fn (see below); validates
  per-cookie via `validate_cookie_params`; dispatches CDP
  `Network.setCookies`. GetCookies returns raw values on the
  operator-facing receipt per D7. ClearCookies emits
  `CookiesCleared{target_id, session_id, count_before}` audit
  BEFORE the CDP call (D9 / FND-0050) — `count_before` from a
  synchronous `getCookies` peek. DeleteCookies uses a peek
  before-and-after to determine `matched: bool`.
- **`vault-substitute-cookies` WIT host-fn** (the 11th host-fn).
  Wraps `Vault::substitute_cookies` (shipped v0.9.5). Chokepoint
  where keychain blob bytes briefly cross into WASM linear memory.
  Replay-mode refuses with `HostError::Internal` — replay values
  come from `replay_cookie_values` per §5.
- **4 cookie verbs on `web-surface` WIT export**: `set-cookies`,
  `get-cookies`, `clear-cookies`, `delete-cookies`. `WebSurface`
  trait + `WebSurfaceImpl` extended; `WEB_SURFACE_VERBS` const now
  enumerates 14 entries.
- **4 `Action` enum variants + 4 `Receipt` cookie-result fields**
  in `loom-rpc/host_service_adapter` so cookie verbs cross the
  RPC boundary. `source` and `*_result` fields use
  `serde_json::Value` because `loom-rpc` has no dep on
  `loom-surfaces` for the typed cookie shapes.
- **4 `ActionMeta` entries** in `loom-rpc/action_registry`
  enumerating cookie verbs for `rpc.schemas` / `loom action
  --help` / man-page generation. Alphabetically sorted to satisfy
  the existing registry-sort test.
- **Daemon `WasmBridge` dispatch arms** for the 4 cookie verbs:
  `action_session_id`, `action_verb`, `build_chromium_args`
  (returns None for cookie verbs — they route through the WASM
  guest, not the direct-shim path). Wire-receipt construction
  (`build_navigate_wire_receipt`) carries the 4 cookie-result
  fields.
- **Receipt cookie-result fields + D13 tuple-identity sort** in
  `loom-host::ReceiptMarshaller`. New `assemble_cookies_canonical_bytes`
  function: sorts cookie arrays by `(name,
  domain.unwrap_or_default(), path.unwrap_or_default())` byte-lex
  (matches UTF-16 lex for ASCII per RFC 6265), replaces every
  `value` field with `"[REDACTED]"` before JCS-encoding. This is
  the replay-byte-identity invariant: same `(name, domain, path)`
  tuples → same canonical bytes → same `outcome_hash`, regardless
  of the actual cookie value.
- **Replay cookie value substitution** in `loom-core::replay_engine::
  cookie_replay`: `substitute_cookie_values(action_id, raw_json,
  &ReplayCookieValues)` swaps each cookie's value by tuple-key
  lookup; missing tuple → typed
  `ReplayError::MissingCookieValue {action_id, name, domain, path}`.
  `parse_replay_cookie_values(json_text)` accepts both pipe-keyed
  object `{"name|domain|path": "v"}` and tuple-array
  `[["name","domain","path","value"]]` shapes for hand-authored
  replay fixtures.
- **Path-level cookie redaction at the MCP boundary** in
  `loom-mcp::mcp_observability`. New `COOKIE_REDACTED_TOOL_NAMES`
  const (the 4 cookie verbs) + `redact_cookie_paths_in_place(&mut
  Value)` walker that strips `value` fields from any `cookies`
  array OR `*_cookies_result` array. Reuses the existing
  `redact_vault: bool` toggle. Critical: cookie *names* and
  `error_code` taxonomy strings remain visible; only `value`
  fields scrub.
- **`vault.get_session_context` RPC**: returns
  `{session_id: String, unambiguous: bool}` for the operator's
  current Active session. Daemon picks the most recently created
  Active session; `unambiguous=true` when exactly one Active
  exists; `SessionNotFound` when zero. The CLI uses this so
  `loom vault add --credential-type cookie` binds to the right
  session automatically per D5.
- **`loom vault add --credential-type <oauth|cookie>` CLI flag**.
  `cookie` requires `--from-file` or `--from-stdin` plus
  `--label`. Validates the JSON blob schema
  (`{"schema_version":1, "cookies":[...]}`) before dispatching to
  the daemon. Resolves session via `vault.get_session_context`
  when `--session` is omitted. Sequence:
  `vault.set_secret` (cookie bytes) → `vault.grant` with
  `credential_type:"cookie"` → prints the resulting GrantId for
  the operator to paste into MCP `set_cookies({grant_id})` calls.
  D7 explicit: NO shred-after-read on the input file (operator
  owns the file lifecycle).
- **§10 heap-wipe hardening** in `loom-shared::redacted`. New
  `wipe_string_buffer_in_place(&mut String)` and
  `wipe_byte_buffer_in_place(&mut Vec<u8>)` helpers explicitly
  zero the heap allocation before drop (works around
  `String::zeroize`'s known length-clear-without-overwrite
  limitation). Wired at the CDP-encode boundary in
  `SetCookiesVerb::execute`. Test verifies the bytes-at-pointer
  are zero after the wipe.
- **`cookie_injection_acceptance.rs` e2e test harness** in
  `loom-cli/tests/` (gated by `--features e2e`). Hand-rolled
  stdio MCP JSON-RPC framing (~150 LOC scaffolding, no rmcp dep);
  sync HTTP echo server (~40 LOC) that captures Cookie request
  headers for the navigate-echo test. 10 test cases; tests
  requiring a real chromium shim further gate on
  `LOOM_TEST_CHROMIUM_AVAILABLE=1`; the harness gracefully skips
  when the daemon isn't running.

### Breaking from v0.9.5 — IMPORTANT for downstream pins

- `loom.web.*` MCP tool naming was established in v0.9.5 (dropped
  the redundant `loom.` prefix that earlier versions had).
  v0.9.6 changes nothing here, but downstream consumers pinning
  `loom-mcp@0.9.5` and migrating to `0.9.6` SHOULD double-check
  any string literals referencing the four cookie verb names —
  they MUST be `loom.web.set_cookies`, `loom.web.get_cookies`,
  `loom.web.clear_cookies`, `loom.web.delete_cookies` (full MCP
  wire names, with the `loom.` prefix the dispatcher prepends).

### Security

- **§11 threat-model section** for cookie credentials in
  `security/vault_threat_model.md`: lawful basis (D6 FND-0010),
  retention (D6 FND-0011), session binding (D5), audit-chain
  visibility (names yes, values no), D7 caveats (no
  shred-after-read on operator file; get_cookies operator-facing
  raw values), §10 heap-wipe boundary.
- **`docs/loom-vault-audit.md`** documents `CookiesSubstituted` +
  `CookiesCleared` audit kinds with canonical-bytes shapes.

### Follow-ups landed before release

These three items were tracked as v0.9.7 follow-ups during the v0.9.6
build but landed on the same PR before merge.

- **Daemon-side grant resolution for `web.set_cookies`** — the
  dispatcher now resolves `CookieSource::Grant` to `Inline` by
  calling `Vault::substitute_cookies(grant_id, session_id)` before
  the WASM verb runs. Previously the daemon's `build_chromium_args`
  emitted an empty Network.setCookies envelope for grant sources;
  now it sees a fully-resolved cookie array exactly as if the
  operator had passed `CookieSource::Inline` directly.
- **Per-cookie validation taxonomy daemon-side** — validation
  failures (`name_empty`, `name_invalid`, `value_too_large`,
  `invalid_same_site`, `invalid_expires`, `too_many_cookies`) now
  short-circuit to a typed `cookie_validation_error` receipt with
  `detail.code = <snake_case taxonomy string>` *before* the
  chromium shim is touched. Operators can group validation failures
  by code in dashboards rather than parsing free-text error
  messages.
- **Daemon-startup parallel manifest sweep** — `StartupManager::
  sweep_manifests` now fans out per-session WAL processing across
  up to 16 worker threads via `std::thread::scope` (capped by
  `available_parallelism`). Per-session isolation is already a
  design property of the sweep, so concurrent processing is safe.
  Single-threaded fast path retained for corpora < 8 sessions to
  avoid `thread::scope` overhead. Recovered/crashed counters
  aggregate via atomics; failures via a single Mutex (rare path).

### Deferred (still) to a future release

- **Daemon-layer policy gate.** Per D9 / FND-0021, the verb-level
  `SafetyPolicy::check_*_cookies` stubs remain always-Ok; the
  authoritative gate would land at the daemon-layer dispatcher as
  a separate hardening PR.
- **CHIPS partition cookies** (CDP-pass-through; browser arbitrates).
- **RFC 6265 edge cases** (Domain leading-dot, IP-host cookies).
  CDP-pass-through.
- **Retention TTL on cookie grants** (no auto-expiry in v0.9.6;
  operator manages via `loom vault delete <label>`).
- **RBAC, admin tooling, multi-operator workflows.**

## [0.9.5] — 2026-05-29 — Cookie Injection (scaffolding milestone)

First milestone of the **web-cookie-injection** feature. v0.9.5 ships the
foundation types, vault extension, CDP cookie encoder, and verb-Action
scaffolding for the four upcoming MCP verbs (`web.set_cookies`,
`web.get_cookies`, `web.clear_cookies`, `web.delete_cookies`). The
verb-level `execute()` implementations + daemon dispatch + first
end-to-end stdio MCP acceptance test will ship in v0.9.6 — see the
follow-up tracking ticket.

### Added

- **`loom_shared::Redacted<T>` newtype.** Hides values in
  Debug/Display/Serialize output (all emit `"[REDACTED]"`); normal
  Deserialize. Bound `T: Zeroize` enforced at the struct level so Drop
  can zeroize the inner value. `expose()` and `into_inner()` (via
  `ManuallyDrop`) are the explicit move-out escape hatches.
- **`CredentialType::Cookie`** vault variant. Cookie grants are
  session-bound (D5 / council FND-0008): `Vault::substitute_cookies(grant_id,
  session_id)` rejects on session mismatch with a typed
  `vault_session_mismatch` envelope.
- **`Vault::substitute_cookies` trait method.** Parallel to the existing
  OAuth-header `substitute()` path: keychain blob → `Zeroizing<Vec<u8>>` →
  returned to caller for decoding into the typed `NetworkCookieParam`
  shape. Cookie *names* land in the audit chain (replay-deterministic);
  values never appear in audit / receipts / logs.
- **Per-CredentialType `grant()` policy (D3 simplified).** Replaces the
  prior single-line OAuth check with an explicit `match` on
  `CredentialType`; `OAuth | Cookie` are accepted, `ApiKey | Saml | Basic`
  remain reserved-and-rejected. Error envelope expanded to advertise both
  `oauth2_authorization_code_pkce` and `cookie` in `allowed_types`.
- **`AuditKind::CookiesSubstituted`** with canonical-bytes shape
  `{grant_id, session_id, cookie_names}` — names for replay determinism,
  no values.
- **`AuditKind::CookiesCleared`** with canonical-bytes shape
  `{target_id, session_id, count_before}` — for `web.clear_cookies` audit.
- **CDP cookie wire types** in `loom-surfaces`: `NetworkCookieParam` (13
  fields, input shape) and `NetworkCookie` (15 fields, output shape) per
  the asymmetric Chrome DevTools spec (council FND-0002). Hand-written
  (chromiumoxide banned in `loom-surfaces` per `deny.toml`).
  `CookieSameSite` / `CookiePriority` / `CookieSourceScheme` enums match
  the CDP PascalCase wire format.
- **`CookieSource` enum** (`Inline { cookies }` | `Grant { grant_id }`) —
  type-safe XOR for `set_cookies` input (council FND-0042); replaces
  runtime `Option<>` XOR.
- **`CookieValidationError` typed enum + `validate_cookie_params()`** —
  enforces a 64-cookie cap (DoS guard per council FND-0044); rejects
  empty names, invalid name characters, oversized values (>4096 bytes),
  pre-1970 expires.
- **4 new `CdpMessage` variants:** `NetworkSetCookies`, `NetworkGetCookies`,
  `NetworkClearBrowserCookies`, `NetworkDeleteCookies`. Encode arms
  produce CBOR envelopes following the existing pattern. `expires` is
  `f64` per CDP spec (the encoder's `f64`-ban applies only to mouse
  coordinates).
- **Verb-level safety stubs:** `SafetyPolicy::check_set_cookies`,
  `check_get_cookies`, `check_clear_cookies`, `check_delete_cookies` —
  all always-`None` (allow-all) under both Default and Safe profiles.
  Authoritative gate lives at the daemon layer per the EvaluateVerb
  dead-code pattern.
- **Verb-Action scaffolding:** `set_cookies_verb`, `get_cookies_verb`,
  `clear_cookies_verb`, `delete_cookies_verb` directories with
  `Action` structs and serde round-trip unit tests. Full `execute()`
  + receipt-builder integration deferred to v0.9.6.

### Changed

- **`Vault::grant()` rejection envelope** — `details.allowed_types` array
  expanded from `["oauth2_authorization_code_pkce"]` to
  `["oauth2_authorization_code_pkce", "cookie"]`. ApiKey/Saml/Basic
  callers see the same `vault_credential_type_unsupported` code; the
  advertised allowlist grows by one entry.

### Security

- **`Redacted<T>` heap-wipe caveat (council FND-0001).** Cookie value
  fields are typed `Redacted<String>` (originally `Redacted<Zeroizing<String>>`
  per D12; rolled back when `Zeroizing<T>` was found to not implement
  `serde::Deserialize`). `String::zeroize()` from `zeroize` 1.6+ calls
  `Vec::clear()` which sets length to 0 but does not write zeros to the
  heap buffer — so memory contents may persist briefly after drop.
  Documented in `security/vault_threat_model.md` as a known caveat;
  proper heap-wipe (via `Zeroizing<Vec<u8>>` at the keychain boundary)
  is a v0.9.6 follow-up.
- **Session binding for cookie grants (D5 / council FND-0008).**
  `Vault::substitute_cookies` returns `vault_session_mismatch` when the
  consuming session_id differs from the grant's stored session_id.
  Defends against intra-daemon IDOR.

### Deferred to v0.9.6

The following Phase 2 plan items did not ship in v0.9.5 and are tracked
for the next release:

- Verb-level `execute()` implementations for the four cookie verbs
  (CDP encode + `host::shim_call` plumbing + receipt assembly).
- Daemon dispatch wiring in
  `loom-daemon::WasmBridge::dispatch_action_blocking`.
- Receipt marshaller extensions for cookie tags + JCS sort by
  (name, domain, path) tuple per D13 (council FND-0039).
- Replay-engine `ReplayError::MissingCookieValue` + fixture-based
  byte-identity tests.
- `loom-mcp::mcp_observability` cookie-value JSONPath redaction.
- `vault.get_session_context` RPC for CLI session-id resolution (D5).
- CLI `loom vault add --credential-type cookie` flag.
- First end-to-end stdio MCP integration test at
  `loom-cli/tests/cookie_injection_acceptance.rs`.
- `security/vault_threat_model.md` cookie-credentials section
  (D6 + D7 caveats).
- `docs/audit.md` documentation of new audit entries.

## [0.9.4] — 2026-05-29

Vault hardening release. Replaces the v0.9.3 placeholder `StubKeychain` with
real platform-keychain backends (macOS Security Framework, Linux Secret
Service via gnome-keyring) and lights up direct credential CRUD on the Vault
trait + `loom vault` CLI surface. Daemon startup is now **fail-closed** on
backend init — there is no silent stub fallback. Existing OAuth-grant flows
are byte-identical; the user-visible surface gains five new subcommands
(`vault add`, `vault delete`, `vault list-labels`, `vault diagnose`, plus
`vault add --overwrite` semantics).

### Added

- **Real macOS Security Framework keychain backend.** `security-framework`
  3.x via `loom-keychain/src/macos.rs`. Items default to
  `kSecAttrAccessible = WhenUnlockedThisDeviceOnly` and
  `kSecAttrSynchronizable = false` (prevents iCloud-Keychain sync of stored
  credentials). All `SecItem*` calls are scoped to
  `kSecAttrService = "loom"` — never cross-keychain, never unscoped.
- **Real Linux Secret Service keychain backend.** `secret-service` 5.x via
  the `blocking::*` submodule (sync API matching the trait shape), in
  `loom-keychain/src/linux.rs`. D-Bus owner pinning on init plus
  `NameOwnerChanged` runtime monitoring give hijack-resistance against a
  same-user `org.freedesktop.secrets` impostor; the resulting
  `SecretServiceOwnerChanged` audit event is emitted on observed
  re-ownership.
- **Daemon backend selection via `LOOM_KEYCHAIN_BACKEND`.** Accepts
  `{auto|macos|linux|in_memory|stub}`. Explicit `macos`, `linux`, or
  `auto` (= platform-native) are **hard-fail-closed** on init failure —
  the daemon refuses to start rather than silently downgrade. When the
  variable is **unset**, the daemon defaults to `in_memory` so CI and
  dev-test contexts that don't have a platform keychain daemon running
  can boot without intervention; production deployments must opt in
  explicitly via `LOOM_KEYCHAIN_BACKEND=auto` (or `=macos` / `=linux`).
  The `stub` backend (always-`Err(Unavailable)`) is retained for the
  narrow case of "run the daemon with vault disabled entirely".
- **`loom vault add <label> [--from-stdin|--from-file] [--overwrite]
  [--session]`.** Direct credential injection alongside the existing
  OAuth-grant flow. 1 MiB read cap, binary-safe (no UTF-8 check, no
  trailing-newline strip), bytes travel as lowercase hex over JSON-RPC.
  Default behaviour fails on existing label; `--overwrite` replaces.
- **`loom vault delete <label> [--force]`.** Removes the credential from
  the platform keychain. Without `--force`, refuses if any alive grant
  references the label and returns `VaultRejection { code:
  "credential_in_use", active_grants: N }`. With `--force`, cascade-revokes
  every active grant (one `GrantRevoked { reason: "credential_deleted" }`
  audit entry per grant) before deleting.
- **`loom vault list-labels [--session]`.** Lists stored credential labels
  scoped to `service_id = "loom"`. Distinct from `loom vault list` (which
  enumerates grants).
- **`loom vault diagnose`.** Stable JSON schema
  `{backend, init_status, service_id, label_count, last_keychain_error?}`
  with SemVer stability promise — see `docs/loom-vault-audit.md` for the
  contract.
- **Eleven typed `AuditKind` variants for credential lifecycle.**
  `SecretOpPending`, `SecretStored`, `SecretFetched`, `SecretDeleted`,
  `SecretReplaced`, `SecretsListed`, `SecretStoreFailed`,
  `SecretDeleteFailed`, `SecretFetchFailed`, `PromptBlocked`,
  `SecretServiceOwnerChanged`. Forward-compat is preserved via
  `#[serde(other)] Unknown` so older readers tolerate future variants.
- **Typed `KeychainErrorKind::TimedOut` and `NonInteractivePrompt`.**
  `KeychainErrorReason` is now a closed enum rather than free-form
  `String` (prevents secret-bytes-in-error-message leakage). Internal
  errors carry a `SHA-256(internal_hash)` correlation handle instead of
  the raw upstream message.
- **`LOOM_KEYCHAIN_ALLOW_PROMPT` env var.** Defaults to `0` in non-TTY
  contexts — headless daemons never hang on a biometric/unlock prompt.
  Blocked prompts emit a `PromptBlocked` audit event.
- **`BlockingKeychain` adapter.** Wraps the sync keychain trait with
  `spawn_blocking` + `tokio::time::timeout` (30 s get, 5 s set/delete/list
  per D28); falls through to a direct call when no runtime is present
  (sync unit tests). LocalVault holds `Arc<BlockingKeychain>` per the
  single-owning-module invariant.
- **Defense-in-depth label validation.** Boundary checks at the CLI, the
  daemon RPC, and `manifest_writer::append_audit` all enforce
  `^[A-Za-z0-9:_-]{1,64}$`; any `Secret*` payload with a malformed label
  is rejected with `VaultInvalidLabel`.
- **CI gnome-keyring setup on Ubuntu cell.** Installs `libdbus-1-3
  dbus-x11 gnome-keyring`, starts `dbus-launch`, unlocks the daemon under
  a 60 s `timeout`, and exports `KEYCHAIN_CI_REQUIRE_DAEMON=1` on
  success. Setup failure is best-effort: tests self-skip cleanly rather
  than fail-the-job per FND-0047 (non-blocking on CI infra flake).
- **Hermetic e2e + acceptance suite.** `loom-keychain/tests/keychain_acceptance.rs`
  (6 tests, GREEN) plus the new `loom-cli/tests/keychain_e2e_hermetic.rs`,
  which round-trips `vault add` → `vault list-labels` → restart → re-list
  to prove the daemon is wired to a real backend (not a stub), and
  performs a byte-level scan across the daemon `data_root` to verify the
  per-test canary string is **never** persisted to disk (G1 enforcement).
- **Threat model + operator docs.** `security/vault_threat_model.md`
  extended with G5a/G5b split, AB6 + AB7 abuse cases, TB2 no-cache
  codification, and a 5-row claimed-controls / known-gaps table for
  SOC 2 + ISO 27001. New `docs/PRIVACY-loom-vault.md` (GDPR Art. 6(1)(a)
  explicit-consent framing, retention model, DSAR procedure) and
  `docs/loom-vault-audit.md` (full `AuditKind` reference + `jq` runbook
  recipes + stable `vault diagnose` schema). Regenerated `docs/loom-vault.1`
  covering all seven v0.9.4 subcommands.
- **0600 startup probe + post-write tightening on auth artefacts.**
  `loom-daemon` refuses to start if `hello.token` or `daemon.pid` carry
  loose perms (`g+r|w|x` or `o+r|w|x`); fresh files are tightened to
  `0600` unconditionally so a default `0022` umask cannot leave them at
  `0644`. Unix-only; Windows ACLs out of scope. Crash-only — no
  auto-chmod.

### Changed

- **Unified `KeychainAccess` trait.** Deleted the duplicate
  `loom_core::vault::KeychainAccess` in favour of the single definition
  in `loom-keychain`. `CoreApiFacade::new` now accepts an injected
  `Arc<dyn KeychainAccess>` (previously hardcoded a `NullKeychain`).
- **`loom vault add` requires `--overwrite` to replace.** Was silent
  upsert. Without the flag, an existing label returns
  `VaultRejection { code: "already_exists" }`.

### Fixed

- **`scripts/lint_no_platform_imports.py` SCAN_DIRS.** The v0.9.3 paths
  resolved to a non-existent directory and the lint silently scanned
  zero files. Fixed to point at the real `loom-host/src` and
  `loom-core/src`; backed by a new `scripts/test_lint_no_platform_imports.py`
  that plants a violation fixture and asserts the lint catches it
  (regression guard).

### Notes

- Windows Credential Manager backend remains a follow-up.
- Biometric access control (`LOOM_KEYCHAIN_REQUIRE_BIOMETRY`) is **not**
  in 0.9.4 — tracked as `loom v0.9.x` follow-up.
- macOS `kSecAttrCreator` ownership discriminator + the corresponding
  Vault-side ownership check (AB6 same-user-read full-mitigation) are
  deferred to 0.9.5 (accepted risk in the v0.9.4 band).
- Strict CLI exit-code mapping per D33 (0/1/2/3/4/5) is partial in 0.9.4
  — label/argument failures map to exit 2 via the existing
  `error_mapper`; the dedicated `AlreadyExists` exit-5 path is a 0.9.5
  follow-up.
- `loom vault add --overwrite` TTY × `--yes` interactive prompt matrix is
  deferred to 0.9.5; the wire-level overwrite guard is in place for the
  CI/script path that is the v0.9.4 primary use case.
- First-run consent dialog for credential storage is deferred to 0.9.5
  (needs interactive UX + per-`data_root` "shown once" state); the
  PRIVACY doc documents the policy.
- Peer-UID RPC authentication remains deferred; caller identity continues
  to rely on socket mode `0600` + per-user runtime dir (existing TB3
  boundary).

## [0.9.3] — 2026-05-21

Hygiene + hardening release. Closes one disclosed-known-gap from 0.9.2
(the `daemon.health` amplification path) and rolls up two weeks of
dependency hygiene — most notably the latent tokio sync-primitive fixes
from 1.52.3 that affect mpsc / `RwLock` code paths under concurrent
session load. No user-visible behaviour change to session lifecycle,
navigate, or replay; existing `loom doctor` / `loom action` flows
produce byte-identical output.

### Added

- **Per-connection rate limit on `daemon.health` (#58, #86).** Closes
  the amplification-via-deep-probe gap disclosed in 0.9.1's CHANGELOG
  and flagged as Sec-5 in #56's security council review. Token-bucket
  caps per-connection sustained call rate (10 RPS) with a burst
  tolerance (30 calls). An empty bucket returns the new typed
  `LoomErrorCode::TooManyRequests` (wire string `"too-many-requests"`)
  so clients can back off and retry. Tunable via
  `LOOM_DAEMON_HEALTH_RATE_RPS` and `LOOM_DAEMON_HEALTH_RATE_BURST`.
  Gate fires only on `daemon.health`; all other RPCs are unaffected.

### Changed

- **Dependency hygiene (#84, #85).** 18 cargo bumps + 5 GitHub Actions
  bumps cleared the May 2026 backlog in one pass instead of waiting out
  the Dependabot schedule. Notable changes ride along:
  - `tokio 1.52.2 → 1.52.3` patches four latent bugs in sync primitives
    (mpsc `len()` underflow, `OwnedPermit::release()` receiver-notify,
    `RwLock` `max_readers != 0` precondition, mpsc `try_recv()` on
    closed-with-permits) — loom's daemon RPC dispatch, per-shim IPC,
    and session manager are all mpsc-heavy under concurrent load.
  - `jsonschema 0.46.4 → 0.46.5` patches the validator that runs on
    every inbound RPC.
  - `dashmap 6.1 → 6.2` improvements in the concurrent map used for
    session state.
  - Major-version migrations: `thiserror 1→2`, `rand 0.8→0.9→0.10`
    (incl. the `OsRng → SysRng` rename and the `RngCore → Rng` /
    `RngExt` split), `zip 2→8`, `toml 0.9→1.1`, `wit-bindgen 0.40→0.57`
    (refreshed vendored wasm), `sha2 0.10→0.11` (migrated 4 call sites
    to `hex::encode`), `jsonrpsee 0.24→0.26`, `chromiumoxide 0.6→0.9`,
    `serde_jcs 0.1→0.2`, `addr2line 0.24→0.26`, `gimli 0.31→0.33`,
    `clap_mangen 0.2→0.3`. All behaviour-preserving — the rand
    cluster's ChaCha20 stream is bit-for-bit identical given the same
    seed, the JCS canonical output is unchanged, all receipt hashes
    are stable.
- **CI cost-cuts (`8185783` and earlier).** Workflow-level concurrency
  cancels superseded PR runs, `beta` toolchain dropped from the macOS
  matrix, `smoke` and `e2e` macOS legs gated to push-to-main only,
  Dependabot grouped into one non-major PR + per-major PRs.

### Tests

- **Deep-health invariant guards (#57, #87).** Three new Rust
  integration tests lock in invariants that #56 deferred:
  `probe_pending_no_leak.rs` (4 tests — `ShimProcess::pending` does not
  accumulate across timeout / closed-channel / crashed-flag paths),
  `restart_count_lifecycle.rs` (5 tests — breaker-rejected calls do not
  bump `restart_count`, so `daemon.health({deep:true})` bookkeeping
  doesn't overcount), and `deep_health_probe.rs` (1 test, fake-chromium
  gated — full `ShimRequest::Health` → `ShimHealthInfo` round-trip).

## [0.9.2] — 2026-05-18

Linux-enablement patch release. With 0.9.1 a fresh Linux install would
`postinstall` correctly but then fail two ways: `loom doctor` reported
`chromium binary not found` (#70), and every `web.*` action surfaced as
`surface_trap` because the shim's IPC socket raced Tokio's I/O driver on
fd 3 (#71). 0.9.2 fixes both — `loom serve` + `loom action web.navigate`
now drives headless Chromium end-to-end on Linux without a system Chrome
on `$PATH`.

### Fixed

- **`loom doctor` + `loom session` Chromium detection on Linux/Windows
  (#70).** `loom doctor`'s `chromium_present_and_verified` check and the
  `chromium_resolver` launch path both hardcoded the macOS `.app` bundle
  layout (`Chromium.app/Contents/MacOS/Chromium`) regardless of host OS,
  while `loom postinstall` correctly extracted the pinned Chromium to the
  per-OS path (`chrome-linux/chrome` on Linux). Net effect on Linux:
  `loom doctor` reported `chromium binary not found` and the launcher
  could not find loom's own pinned download — it only worked when a
  system Chromium happened to be on `$PATH`. The per-OS layout is now a
  single shared source of truth,
  `loom_shared::chromium_resolver::chromium_binary_subpath()`, consulted by
  postinstall, doctor, and the resolver alike.

- **Shim IPC fd collision with Tokio I/O driver on Linux (#71).**
  `spawn_shim` `dup2`'d the IPC socketpair end onto fd 3, but
  `loom-shim-chromium`'s Tokio multi-thread runtime claims the lowest
  free descriptors at startup for its `epoll` instance + wakeup eventfd.
  Adopting fd 3 as the IPC socket then races the I/O driver:
  `UnixStream::from_std` returns `EINVAL`, the driver panics with
  `Bad file descriptor`, the shim exits, and every `web.*` action
  surfaces as `surface_trap`. The IPC socket is now pinned to fd 10 via
  a named `SHIM_IPC_FD` constant — well clear of the runtime's low-fd
  range. `loom-shims/tests/binary_smoke.rs` updated in lockstep.

## [0.9.1] — 2026-05-11

Patch release rolling up the post-0.9.0 daemon-stall fix (#55) and the
daemon-health deep-probe + SDK admin wrappers follow-up (#56), plus the
docs/CI groundwork from #51, #52, #54.

### Added

- **`daemon.health({deep: true})` shim probe.** Adds `ShimRequest::Health`
  CBOR variant + shim-side handler returning
  `ShimHealthInfo { uptime_ms, requests_served, last_request_at_ms }`. The
  daemon fans out probes concurrently with per-shim and overall budgets
  (defaults 1 s / 3 s; env-overridable via `LOOM_PROBE_TIMEOUT_MS` and
  `LOOM_DEEP_HEALTH_BUDGET_MS`). Payload is now typed
  `Vec<ShimDeepHealth>` (replacing the prior placeholder
  `Vec<serde_json::Value>`) with a typed
  `ProbeStatus { Ok, Timeout, Error }` enum. The new shim variant
  requires same-tree daemon+shim shipping; version-mismatched shims
  surface as `probe_status: "timeout"`. `daemon.health` continues to
  require the existing socket-auth token handshake — same auth surface
  as before; no anonymous reachability. Known gap: no per-connection
  rate limit on `daemon.health` itself (tracked as follow-up).
- **`ShimState.restart_count` / `last_restart_at_ms`.** Daemon-side
  bookkeeping for shim respawn events, bumped in `get_or_spawn` only
  when a prior state entry exists (avoids overcounting under
  open-breaker rejection). Exposed via the deep-health payload.
- **SDK wrappers (TS + Python) for the admin RPCs.** Hand-written
  `Session.kill()` (sync + async parity in Python), `killSession()`
  (TypeScript) / `kill_session()` (Python sync) admin free functions
  with documented "ADMIN ESCAPE HATCH — 5 s ceiling then SIGKILL"
  warnings, and `daemonHealth()` (TS) / `daemon_health()` (Python sync)
  free functions. JSON-RPC request `id` allocator switched from
  hardcoded `1` to monotonic per-connection; transports moved to
  id-keyed response demultiplexing (persistent reader task in async
  Python; persistent `data` listener in TS) to support `request.cancel`
  correlation while another call is in flight. Sync Python keeps
  single-in-flight and gains an `AsyncSession`-redirecting doc note for
  cancellation. TS adds `AbortSignal` support on `call()` via
  `LoomAbortError` (`name === "AbortError"`); Python async transport
  transparently emits `request.cancel` on `asyncio.CancelledError` and
  re-raises so `asyncio.wait_for` / `TaskGroup` / `asyncio.timeout`
  compose cleanly.
- **`SessionScope` primitive** ([`loom-core/src/session_scope/`](loom-core/src/session_scope/)).
  Per-session structured-concurrency parent: `tokio_util::sync::CancellationToken`
  paired with a `tokio::task::JoinSet`. All session-lifetime spawns become children;
  `drain(grace)` cancels cooperatively then force-aborts survivors. Replaces five
  fire-and-forget `tokio::spawn` sites that previously leaked `JoinHandle`s and
  saturated the daemon runtime after 4–6 sequential client sessions.
- **Per-request server-side deadline.** `LOOM_REQUEST_TIMEOUT_MS` (default
  `30000`) wraps `router.dispatch` so a hung shim or stuck dispatcher can't hold
  a connection task. Returns the typed `request-timeout` envelope on expiry.
- **`request.cancel` RPC.** Connection-scoped: cancels an in-flight request on
  the same connection by JSON-RPC `id`. Returns `{cancelled: bool}`. The
  cancelled request returns the typed `request-cancelled` envelope.
- **`session.kill` RPC.** Admin escape hatch for stuck sessions. Performs the
  abort flow plus blocks on shim teardown with a 5 s ceiling, then SIGKILL
  (`shutdown_process` already escalates SIGTERM(2s) → SIGKILL(1s) inside the
  ceiling).
- **`daemon.health` RPC.** Operational snapshot: `active_sessions`,
  `shim_breaker_states`, `otel_exporter` status. Shallow path is
  non-blocking; `{deep: true}` slot reserved for a follow-up shim probe.
- **`LoomErrorCode::RequestTimeout`** (wire string `request-timeout`) and
  **`LoomErrorCode::RequestCancelled`** (wire string `request-cancelled`).
  Both SemVer-minor additions per the existing wire-stability commitment.
- **Multi-session stress test** at
  [`loom-core/tests/multi_session_stall_repro.rs`](loom-core/tests/multi_session_stall_repro.rs)
  — 100 sequential session create+close cycles complete in under 1 s; the
  user-facing success criterion from the original investigation prompt.

### Changed

- **`ShimManager::record_failure`** circuit-breaker eviction no longer fires
  `tokio::spawn(shutdown_process(p))` fire-and-forget. Spawns into a
  `cleanup_tasks: JoinSet<()>` field with opportunistic `try_join_next`
  reaping on every `record_failure` and `shutdown_session`.
- **`CoreBridge::close_session_raw`** (daemon-side) similarly tracks
  `host.shutdown_session` background tasks in a `cleanup_tasks` JoinSet
  instead of leaking them via bare `tokio::spawn`.
- **`Session.budget_timer`** is now a SessionScope child with
  `tokio::select! { _ = cancel.cancelled() => {} _ = sleep(budget_ms) => ... }`
  so a closed session's timer exits cooperatively before its sleep
  completes, never tripping the kill callback after lifecycle exit.

### Removed

- **`Session.task_handle`** field — was dead code (declared but never
  populated). Replaced by the always-populated `Session.scope: Arc<SessionScope>`.
- **`Session.budget_timer`** field — superseded by the SessionScope-owned
  cancellation-aware timer.
- **`ReceiptMarshaller.queue_depth`** field + the docstring claiming
  `"Background queue depth 256; full → synchronous append on the calling task"`.
  The field was dead and the docstring lied — there was no bounded channel,
  no `try_send`, no backpressure. Removed both.

## [0.9.0] — 2026-05-05

Initial public pre-1.0 release. Loom incubated as an internal project before
this; see the README's [Status matrix](README.md#status) for which surfaces
are stable. Deterministic replay and `web.click` remain Beta — promotion to
1.0 requires those landing and the matrix CI green.

### Added

- **Determinism harness.** `Math.random()` (sfc32-seeded) and
  `Date.now()` / `performance.now()` (session-fixed epoch_ms) are pinned
  at session-create time and reproduced bit-for-bit on replay via the
  manifest's `started_at_ms`.
- **Implicit session management** in `loom-mcp serve`. MCP clients call
  `loom.web.navigate` with just `url` — the server lazily creates a
  session on first tool call and closes it on shutdown.
- **Path-traversal-safe session IDs.** Session paths reject anything
  that isn't a 26-char lowercase-alphanumeric ULID, preventing
  file-disclosure via `session.inspect '../evil'`.
- **Typed error mapping.** `kind: "http_status"` (with status code) for
  4xx/5xx responses, `kind: "wait_predicate_false"` for `web.wait`
  selector misses, `kind: "schema_violation"` for unsupported export
  formats with the actual supported list in the message.
- **Chromium subprocess crash detection.** The shim's supervisor spawns
  a task that `await`s `child.wait()`; the moment chromium dies, the
  shim self-exits → daemon's crash watcher fires → in-flight CDP
  commands fail in <1s instead of waiting the 30s recv timeout.
- **GC reference protection.** `collect_referenced_blobs` decodes
  `receipt_canonical_bytes` byte arrays and recursively scans for hex
  hashes, so `loom gc --ttl 0` no longer deletes blobs that active
  sessions still reference.
- **Stable session listing.** `loom session list` is now sorted by
  `created_at_ms` descending with `session_id` as tiebreak — independent
  of filesystem read_dir order.
- **Aborted-session replay refused.** Replaying an aborted or crashed
  source session now returns a typed `SessionAborted` error instead of
  silently producing a session that "replayed" an abandoned trace.
- **`import.playwright` RPC.** `loom import playwright trace.zip`
  imports a Playwright trace as a non-replayable Loom session.
- **Schema-aware CLI coercion.** `loom action web.type --text 5551234`
  no longer fails with `"5551234 is not of type 'string'"` — the parser
  consults the action's request schema and coerces per declared type.
- **Pretty TTY output.** Receipt-emitting subcommands auto-detect TTY
  and render colored multi-line layouts at a terminal; piped or
  redirected output stays canonical JSON, byte-for-byte identical to
  prior behaviour. New global flags: `--json`, `--pretty`, `--quiet` /
  `-q`, `--color={auto,always,never}` (with `--no-color` alias).
  Honours `NO_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR=0`, and `TERM=dumb`.
  Per-stream color resolution lets a piped stdout still ship colored
  stderr error prose.
- **Recursive sensitive-field redaction in pretty output.** Receipt
  fields whose key (at any nesting depth) matches a conservative
  denylist (`token`, `secret`, `password`, `api_key`, `oauth`,
  `credential`, `cookie`, `jwt`, `session_key`, `access_token`,
  `refresh_token`, `private_key`, `signing_key`, `client_secret`, etc.)
  render as `<redacted>` in the human path. The `--json` path is
  unchanged — machines still see the raw value.
- **Curated multi-line layouts** for every receipt-emitting subcommand:
  `session create / inspect / list / close / abort / replay / diff /
  validate / export`, `action <surface>.<verb>` (all `web.*` verbs),
  `vault add / list / grant / revoke`, `gc`, `doctor`,
  `import playwright`, `benchmark`. Each renderer declares the keys it
  consumed; remaining receipt fields surface in a dim "more details"
  tail block in JSON-Schema property order.
- **Empty-state copy.** `session list` with no sessions prints
  `No sessions found.` instead of an empty header table; `vault list`
  empty prints `No vault entries.`.
- **README "Status" matrix** declaring stable vs beta surfaces, with
  concrete promotion criteria for 1.0.
- **`loom --version`** prints `loom 0.9.0 (<short-sha> <build-date>)`
  so issue reports identify the exact build. The short SHA + UTC build
  date are baked in by `loom-cli/build.rs` from `git rev-parse` at
  compile time, with a `"unknown"` fallback for source-tarball builds
  (no `.git` directory). `SOURCE_DATE_EPOCH` is honored for
  reproducible builds. The JSON-emitted `VersionInfo` carries
  `build_date` alongside the existing `version`, `git_sha`, `target`.

### Changed — breaking

- **`--pretty` semantics changed.** Previously, `--pretty` produced
  indented JSON via `serde_json::to_string_pretty`. It now produces
  the human-readable colored multi-line layout described above. If
  your scripts depend on the old indented-JSON shape, switch to
  `--json` (canonical single-line, machine-parseable) — that's what
  you want for `jq` and similar pipelines anyway.
- **`NO_COLOR` empty-string behaviour fixed.** Previously, setting
  `NO_COLOR=""` (empty) was treated as a disable signal, contrary to
  the [no-color.org](https://no-color.org/) spec. The variable now
  disables color only when set to a non-empty value. CI environments
  that set `NO_COLOR=""` for parent processes will no longer
  accidentally disable color.
- **Workspace version walked back from 1.0.0 → 0.9.0** to reflect that
  deterministic replay and `web.click` are not yet bulletproof.
  `v0.9.0` is the first supported release.

### Fixed

- `web.wait` was a no-op — it dispatched a single `Runtime.evaluate`
  predicate but never checked the result, so missing selectors
  returned success in 1 tick. Now surfaces `wait_predicate_false`
  with `selector did not appear before timeout`.
- HTTP 4xx/5xx responses with `ERR_HTTP_RESPONSE_CODE_FAILURE` were
  classified as generic `network_error` losing the status code. The
  host's typed-error builder now prefers `status >= 400` events
  before `error_reason` events.
- `--budget network=0` and `wall_clock=0s` accepted silently (0
  internally meant "unlimited"). Now rejected at parse time.
- Profile help text claimed `safe`, `default` were valid profiles; the
  daemon's allowlist is `safe`, `standard`, `full`. Help text fixed.
- `Date.now()` / `Math.random()` leaked real wall-clock + unseeded
  values when called via `web.evaluate` *before* any `web.navigate`.
  Root cause: chromium's bootstrap `about:blank` context never had the
  determinism script applied. Fixed by lazy-spawning the
  determinism-injected target on the first evaluate.

[Unreleased]: https://github.com/mentiora-ai/loom/compare/v0.9.4...HEAD
[0.9.4]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.4
[0.9.3]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.3
[0.9.2]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.2
[0.9.1]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.1
[0.9.0]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.0
