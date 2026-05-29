# Privacy notice — loom vault (v0.9.4)

> **Audience.** Operators evaluating loom for use under GDPR / UK-DPA /
> CCPA / SOC 2 / ISO 27001 obligations. This document describes how the
> vault subsystem (`loom vault add` / `delete` / `list-labels` /
> `diagnose` and the `Vault::substitute` substitution path) handles
> personal-data-grade credentials. It is **not** a hosted-service
> privacy policy — loom is an OSS CLI; the legal entity collecting
> data is whoever runs the daemon.

## 1. Lawful basis for processing

**GDPR Art. 6(1)(a) — explicit consent.** The CLI invocation IS the
consent moment: when an operator types `loom vault add` (or pipes a
credential through `--from-stdin`), they explicitly direct loom to
store that credential in their OS keychain. No background processing,
no telemetry, no remote upload — every action requires an operator-
initiated CLI command.

**v0.9.4 known gap:** the one-time informational message on first-ever
`loom vault add` (planned per A-W8.3 / FND-0037 to make the consent
moment more explicit) is **deferred to v0.9.5**. Operators are expected
to read this document and the manpage before the first invocation.

## 2. Data categories

| Category | Stored | Where | Retention |
|---|---|---|---|
| OAuth bearer tokens, refresh tokens, API keys, session cookies | YES | OS keychain (macOS Login Keychain / Linux Secret Service) | Until operator runs `loom vault delete <label>` or removes via OS UI |
| Credential **labels** (e.g. `github-oauth`) | YES | OS keychain attribute `account=<label>`; audit-chain manifests | Same as parent credential; audit history persists per §4 |
| Credential **byte counts** | NO — only `size_bucket ∈ {small, medium, large}` per D24 | Audit-chain manifests | Per-session retention |
| Credential **bytes themselves** | NO — never written to any loom-managed file (G1 invariant) | — | — |
| Per-op timestamps + outcomes | YES | Audit-chain manifests (`secret_op_pending`, `secret_stored`, `secret_*_failed`, etc.) | Per-session retention |
| Originating session id (when in-session) | YES | The session's WAL only | Per-session retention |

## 3. Retention

**loom does not enforce a retention policy on credentials.** They
persist in the OS keychain until the operator explicitly removes them:

- `loom vault delete <label>` (single-credential delete; cascade-
  revokes referencing grants with `--force`);
- the OS keychain UI: macOS *Keychain Access.app*, Linux
  `secret-tool clear service loom <label>`.

**Audit entries** follow the session's retention. The default loom
flow archives sessions when the operator runs `loom session close`;
manifests under `~/.local/share/loom/sessions/<sid>/` remain on disk
until manually deleted.

**Automatic credential rotation / TTL-driven cleanup** is **not in
scope for v0.9.4** — tracked for v0.10.x. Operators with rotation
obligations should script around `loom vault delete` + `loom vault
add` on a cron-like schedule.

## 4. Cross-border transfer disclosure

The OS keychain is local to the operator's device by default. However:

- **macOS / iCloud Keychain sync.** Apple's iCloud Keychain CAN sync
  generic-password items between the operator's signed-in Apple
  devices. loom v0.9.4 **intends to set** `kSecAttrSynchronizable =
  false` on every `SecItemAdd` to opt out of this sync.
  - **v0.9.4 known gap:** the high-level `passwords::*` API does NOT
    expose `kSecAttrSynchronizable`; the lower-level
    `ItemAddOptions` path is a fast follow-up. Until then, items
    stored by v0.9.4 use the API's default (which empirically does
    NOT sync without explicit opt-in by the OS, but is not
    guaranteed by Apple).
- **Time Machine / OS backups.** `Synchronizable=false` only disables
  iCloud sync; it does NOT prevent the keychain database itself from
  being copied by Time Machine, third-party backup tools, or
  enterprise MDM agents. Operators with backup-residency obligations
  must configure their backup tool's keychain exclusion rules.
- **Linux desktop-environment sync.** Some DEs (e.g. GNOME Online
  Accounts) sync the Secret Service collection across the operator's
  signed-in cloud accounts. loom v0.9.4 has no programmatic way to
  opt out at the per-item level — operators must configure the DE.

## 5. Subject access — DSAR

GDPR Art. 15 (right of access), Art. 17 (right to erasure), Art. 16
(right to rectification), Art. 20 (right to portability):

- **Inventory (Art. 15).** `loom vault list-labels` returns every
  credential label stored under the loom service id. Labels are the
  only identifying metadata operators can enumerate without reading
  the bytes; the audit chain is the source-of-truth for
  per-credential history (use `loom vault diagnose` for the latest
  backend error / state, then `jq` against the session WAL for the
  full timeline — see [`docs/loom-vault-audit.md`](loom-vault-audit.md)).
- **Erasure (Art. 17).** `loom vault delete <label>` removes the
  credential bytes from the OS keychain. **Trade-off:** the audit
  history (label, timestamps, op kinds) remains in any session
  manifest that referenced the credential. The audit chain is
  append-only and hash-chained — selective deletion of past entries
  would break hash-chain integrity. For full GDPR-style erasure,
  delete the session manifest as well; this loses the audit trail
  for that session.
- **Rectification (Art. 16).** loom does not support in-place
  credential edit. The operator runs `loom vault delete <label>`
  then `loom vault add --label <label> --from-stdin` with the
  corrected value. The audit chain records both events.
- **Portability (Art. 20).** loom does **not** ship a credential-
  export tool (Non-Goal per D34). Operators export via the OS
  keychain UI:
  - **macOS:** `security find-generic-password -s loom -a <label> -w`
    (prompts for keychain unlock).
  - **Linux:** `secret-tool lookup service loom account <label>`.

## 6. Access denial

The Unix socket the daemon binds is `0600`, owned by the daemon UID.
No other UID can connect at all; same-UID processes share the
daemon's access scope per AB6. Operators who need stricter isolation
should run `loom-daemon` under a dedicated service UID.

The `0600` startup probe (A-W8.1 / W8.5) refuses to start the daemon
if `hello.token` or `daemon.pid` have loose permissions on disk — an
operator who accidentally `rsync`'d their HOME with default umask will
get a clear error, not a silent leak.

## 7. Telemetry

loom v0.9.4 ships **no telemetry pipeline.** No data is uploaded to
any remote service by the vault subsystem. Operators with corporate
DLP / SOC obligations can verify by:

1. Running the keychain hermetic e2e test
   (`loom-cli/tests/keychain_e2e_hermetic.rs`) — the G1 byte-scan
   asserts the canary substring NEVER appears in any persisted file
   under the daemon's data root.
2. `tcpdump`/`strace`-attaching the daemon while running
   `loom vault add` and verifying zero network egress.

## 8. Notification of changes

This document is versioned alongside the workspace
(`Cargo.toml::version`). Material privacy-relevant changes (new data
categories, new persistence sites, change of lawful basis) require a
CHANGELOG entry tagged `PRIVACY:`. Breaking the audit chain integrity
contract (e.g. introducing selective audit-entry erasure) requires a
SemVer-major bump.

## 9. References

- [`security/vault_threat_model.md`](../security/vault_threat_model.md)
  — primary threat model + SOC 2 / ISO 27001 control mapping.
- [`docs/loom-vault-audit.md`](loom-vault-audit.md) — audit-kind
  reference + `vault diagnose` JSON schema + `jq` query recipes.
- [`docs/loom-vault.1`](loom-vault.1) — `loom-vault(1)` manpage:
  subcommands, env vars, exit codes, TROUBLESHOOTING.
