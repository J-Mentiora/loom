# loom vault — audit-chain reference (v0.9.4)

Operator-facing reference for the v0.9.4 credential-lifecycle audit
entries: which `AuditKind`s exist, what fields each canonical payload
carries, when entries are appended, and how to `jq` against a session
WAL for forensic queries.

For threat-model context see
[`security/vault_threat_model.md`](../security/vault_threat_model.md).
For the privacy / DSAR view see
[`PRIVACY-loom-vault.md`](PRIVACY-loom-vault.md).

## 1. Where audit entries land

```
~/.local/share/loom/sessions/<session-id>/manifest.wal
~/.local/share/loom/sessions/<session-id>/manifest.jsonl  (checkpoint)
```

(macOS: `~/Library/Application Support/loom/sessions/<session-id>/`.)

Each line is a JSON object; `AuditEntry` variants look like:

```json
{
  "kind": "audit_entry",
  "action_id_ref": null,
  "emitted_at_ms": 1700000000123,
  "audit_kind": "secret_stored",
  "canonical_bytes": [123,34,101,118, ...],
  "prev_hash": "<sha256-hex>"
}
```

`canonical_bytes` is the JCS-encoded payload as a byte array. To
decode, slurp the array → bytes → UTF-8 → JSON:

```bash
jq -r '
  select(.kind == "audit_entry") |
  .canonical_bytes | implode | fromjson
' manifest.wal
```

Or use the helper recipe in §5.

## 2. AuditKind reference (v0.9.4 surface)

The Grant lifecycle kinds (`grant_issued`, `grant_consumed`,
`grant_expired`, `grant_revoked`, `grant_rejected`, `fsm_transition`,
`blocked_url`) are unchanged from v0.9.3. v0.9.4 adds the credential-
lifecycle surface below. The forward-compatibility variant `unknown`
(`#[serde(other)]` per D39) catches any tag from a newer daemon so
older binaries don't break hash-chain validation.

| `audit_kind` | Direction | Payload fields | When appended |
|---|---|---|---|
| `secret_op_pending` | G5b pre-op | `event`, `label`, `op ∈ {set, get, delete, list}` | Within 100ms of the RPC arriving, BEFORE the keychain call. Records operator intent so the audit chain shows intent → outcome even when the OS prompt blocked indefinitely. |
| `secret_stored` | G5a success | `event`, `label`, `size_bucket ∈ {small, medium, large}`, `replaced: bool` | After a successful `set_secret`. `size_bucket` per D24 — no exact byte counts in the hash chain (side-channel mitigation). |
| `secret_replaced` | G5a success | same shape as `secret_stored` (always `replaced: true`) | In addition to `secret_stored` when the set overwrote an existing entry. Operators `jq` this kind to grep replace events specifically. |
| `secret_fetched` | G5a success | `event`, `label` | After a successful direct `get_secret_direct` (the CLI's `vault.get` path — distinct from the `Vault::substitute` token-substitution path which uses the `Grant*` kinds). |
| `secret_deleted` | G5a success | `event`, `label`, `cascade_revoked_grants: u32` | After a successful `delete_secret`. `cascade_revoked_grants` > 0 when `--force` triggered a cascade revoke of referencing grants. |
| `secrets_listed` | G5a success | `event`, `count: u32`, `service_id: string` | After a successful `list_labels`. Per D14: ONE audit per call carrying aggregate counts — no per-label entries. |
| `secret_store_failed` | G5a failure | `event`, `label`, `reason ∈ {not_found, denied, unavailable, timed_out, non_interactive_prompt, internal}`, `internal_hash?: hex` | After a failed `set_secret`. `reason` is a typed enum per D30 — NOT a free-form error string (prevents leaking third-party error message bytes into the manifest). |
| `secret_delete_failed` | G5a failure | same shape as `secret_store_failed` | After a failed `delete_secret`. |
| `secret_fetch_failed` | G5a failure | same shape as `secret_store_failed` | After a failed `get_secret_direct` or `list_labels`. |
| `prompt_blocked` | refusal | `event`, `label`, `op` | The daemon refused to trigger an OS unlock prompt because `allow_prompt = false` (default in non-TTY). Per D26. |
| `secret_service_owner_changed` | Linux drift | `event`, `pinned`, `current` | Linux backend's per-op D-Bus owner re-check (A-W3.1) detected the `org.freedesktop.secrets` owner has drifted from the value pinned at startup. The op is refused; operator must restart `loom-daemon` to re-pin. |
| `cookies_substituted` | G5a success (v0.9.7) | `event`, `grant_id`, `session_id`, `cookie_names: [string]` | After a successful `Vault::substitute_cookies` resolution for `web.set_cookies` with a `CookieSource::Grant`. Cookie *names* land in the chain by design (replay determinism per D5); *values* never appear. The verb dispatches CDP `Network.setCookies` only after this entry is appended. |
| `cookies_cleared` | G5a success (v0.9.7) | `event`, `target_id`, `session_id`, `count_before: u32` | Emitted by `ClearCookiesVerb::execute` via `host::log_emit` BEFORE the destructive `Network.clearBrowserCookies` CDP call. `count_before` comes from a synchronous `getCookies` peek so the audit chain captures the pre-clear count even if the clear call itself fails (D9 / FND-0050). Cookie names NOT included — operators wanting a name-level record should issue `web.get_cookies` first. |
| `unknown` | forward-compat | — | The current daemon's `AuditKind` enum doesn't know the tag this entry carries. Validators MUST treat as opaque-but-valid. Per D39. |

### Failure-reason → support correlation

When `reason = "internal"`, the failure payload carries an
`internal_hash` field — the SHA-256 hex of the original third-party
error message. The message itself is **NEVER** persisted; only the
hash. To recover the original message:

1. Read the hash from the audit entry (or from `vault diagnose`'s
   `last_keychain_error.internal_hash`).
2. Search the daemon's structured log (stderr or the configured log
   destination) for that hash. A-W6.3 / A-W6.4 commits the daemon to
   emitting `tracing::error!(internal_hash = %hash, original_message
   = %msg, …)` whenever an `Internal`-kind error fires, so the log
   line is the authoritative correlation target.

## 3. `vault diagnose` JSON schema (A-W6.4)

`loom vault diagnose --json` returns a stable JSON object suitable for
`jq` automation across patch releases:

```json
{
  "backend": "macos" | "linux" | "stub" | "in_memory",
  "init_status": "ok" | { "error": { "reason": "<string>" } },
  "service_id": "loom",
  "label_count": 0,
  "last_keychain_error": null | {
    "kind": "denied" | "timed_out" | "not_found" | "unavailable" | "non_interactive_prompt" | "internal",
    "when_ts": "<iso8601>",
    "internal_hash": "<hex>" | null
  }
}
```

### Schema stability promise

- New top-level keys MAY be added in a SemVer-minor (operators
  consuming `jq -e .X` must tolerate extras).
- Existing keys, their types, and their wire-string values are
  stable across patch releases (`0.9.x`) and across SemVer-minor
  bumps within the same major (`0.x.y`).
- A SemVer-major bump MAY rename / restructure. The CHANGELOG carries
  a `BREAKING (vault diagnose schema):` block when this happens.

## 4. Operator runbooks

### "I want to grep for every failure in the last hour"

```bash
jq -r '
  select(.kind == "audit_entry") |
  select(.audit_kind | startswith("secret_") and endswith("_failed")) |
  select(.emitted_at_ms > (now * 1000 - 3600000)) |
  "\(.emitted_at_ms) \(.audit_kind) \((.canonical_bytes | implode | fromjson).label)"
' manifest.wal
```

### "I want to see every `set_secret` that overwrote an existing entry"

```bash
jq -r '
  select(.kind == "audit_entry") |
  select(.audit_kind == "secret_replaced") |
  (.canonical_bytes | implode | fromjson) as $p |
  "\(.emitted_at_ms) replaced label=\($p.label) size=\($p.size_bucket)"
' manifest.wal
```

### "I want to confirm the G1 invariant on this manifest"

```bash
# Confirm no entry carries a `secret_hex` / `bytes` / `value` field —
# the G1 invariant says raw bytes never reach the manifest.
jq -r '
  select(.kind == "audit_entry") |
  (.canonical_bytes | implode | fromjson) |
  if has("bytes") or has("secret_hex") or has("value") then
    "VIOLATION: \(.)"
  else empty end
' manifest.wal
```

### "I want to recover the original error message behind an `internal_hash`"

```bash
HASH=<paste from `vault diagnose`>
# loom-daemon writes to STDERR by default. The grep target depends on
# how the daemon was started; the four common cases:
grep "$HASH" /tmp/loom-daemon.log                  # `loom serve` default redirect
grep "$HASH" ~/.local/share/loom/daemon.log        # if you redirected stderr there
journalctl -u loom-daemon | grep "$HASH"           # systemd-managed install
launchctl list | grep loom-daemon                  # macOS — find the LaunchAgent
                                                   # stderr path in the .plist
```

Daemon emits a structured `tracing::error!(internal_hash = ...)` event
at the failure site. Per council ship-review R2-#2, the original message
itself is **NOT** included in the log (D30: the hash IS the correlation
handle; the plaintext message never reaches a persistent surface,
audit chain OR daemon log). To map a hash back to the original message,
the OS keychain backend's own diagnostic channel is the source of truth:
- macOS: Console.app, filter `subsystem:com.apple.security`
- Linux: `journalctl _COMM=gnome-keyring-daemon` or `--user-unit`

**Retention.** Daemon stderr is **not** rotated by loom; if the operator
hasn't configured logrotate / journald retention, older `internal_hash`
entries may be unrecoverable. Production deployments should snapshot
stderr to a rotating sink (or use the `journalctl`/`launchd` paths above).

### "I want to see grant revokes that were cascaded by a `vault delete`"

```bash
jq -r '
  select(.kind == "audit_entry") |
  select(.audit_kind == "grant_revoked") |
  (.canonical_bytes | implode | fromjson) as $p |
  select($p.result == "credential_deleted") |
  "\(.emitted_at_ms) grant=\($p.grant_id) revoked-by=delete-cascade"
' manifest.wal
```

## 5. Helper: decode-all-audit-payloads

Save as `~/bin/loom-audit-decode`:

```bash
#!/usr/bin/env bash
# Stream a manifest.wal and emit one JSON object per audit entry with
# the canonical payload inlined under .payload.
jq -c '
  select(.kind == "audit_entry") |
  {
    ts: .emitted_at_ms,
    kind: .audit_kind,
    payload: (.canonical_bytes | implode | fromjson)
  }
' "${1:-/dev/stdin}"
```

Then:

```bash
loom-audit-decode ~/.local/share/loom/sessions/<sid>/manifest.wal | less
```
