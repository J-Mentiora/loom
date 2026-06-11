/**
 * TypeScript types for the loom SDK.
 *
 * Types mirror wire shapes in:
 *   - systems/loom-rpc/modules/core_service_adapter/interfaces.rs
 *   - systems/loom-rpc/modules/host_service_adapter/interfaces.rs
 *   - loom-shared/src/error_format.rs (`LoomErrorCode::as_wire`)
 *
 * camelCase property names are used for TypeScript idiom; wire uses snake_case
 * (transformation happens in transport/session layers).
 */

/**
 * Full wire vocabulary of daemon error codes (`LoomRPCError.code`).
 *
 * Source of truth: `LoomErrorCode::as_wire` in
 * `loom-shared/src/error_format.rs` — `tests/test_types_drift.ts` pins this
 * array to the Rust source so it cannot drift again. Unknown future codes
 * still arrive as plain strings on `LoomRPCError.code`; treat this union as
 * the known vocabulary, not an exhaustiveness guarantee across daemon
 * versions.
 */
export const LOOM_ERROR_CODES = [
  // ---- Core / lifecycle ----
  "session_not_found",
  "session_already_closed",
  "session_aborted",
  "session_killed",
  "session_closed",
  "surface_trap",
  "surface_unavailable",
  // ---- Vault ----
  "vault_rejection",
  "vault_grant_expired",
  "vault_grant_revoked",
  "vault_grant_not_found",
  "vault_unknown_label",
  "vault_permission_denied",
  "vault_backend_unavailable",
  "vault_backend_timeout",
  "vault_non_interactive_prompt",
  "vault_internal",
  "vault_invalid_label",
  "vault_credential_type_unsupported",
  // ---- Budget ----
  "budget_exceeded",
  "budget_rate_limited",
  // ---- Content store ----
  "store_integrity_failed",
  "store_not_found",
  "store_full_no_evictable",
  // ---- Manifest / replay ----
  "manifest_corrupt",
  "replay_divergence",
  "replay_missing_blob",
  // ---- LLM cache ----
  "llm_cache_miss",
  // ---- Shim / transport ----
  "shim_failure",
  "shim_timeout",
  "shim_breaker_open",
  // ---- RPC / IO ----
  "rpc_invalid_request",
  "rpc_auth_failed",
  "rpc_schema_violation",
  "request_timeout",
  "request_cancelled",
  "too_many_requests",
  "transport_dropped",
  "protocol_auth_required",
  "protocol_malformed",
  "method_not_found",
  "io",
  // ---- Validation / profile ----
  "schema_violation",
  "safe_profile_download_blocked",
  "profile_restricted",
  "browser_not_found",
  "invalid_argument",
  "unsupported",
  "unknown_profile",
  "invalid_network_mode",
  "invalid_budget_key",
  "invalid_capture_policy",
  // ---- Catch-all ----
  "internal",
  "internal_error",
  // ---- In-flight (introduced by daemon branches that may not have
  //      landed; the drift test tolerates their absence from Rust) ----
  "session_cap_exceeded",
  "not_replayable",
] as const;

export type LoomErrorCode = (typeof LOOM_ERROR_CODES)[number];

export interface SessionInfo {
  sessionId: string;
  status: string;
  createdAtMs: number;
}

export interface SessionInspection {
  sessionId: string;
  atAction: number | null;
  manifestSummary: Record<string, unknown>;
}

export interface DiffReport {
  a: string;
  b: string;
  diff: Record<string, unknown>;
}

export interface ExportInfo {
  sessionId: string;
  format: string;
  artifactRef: string;
}

export interface ValidationResult {
  sessionId: string;
  passed: boolean;
  reasons: string[];
  /** PASS ≠ replayable: a --no-determinism recording validates but can
   *  never be replay-equal. Default-true for daemons predating the field. */
  replayable: boolean;
  notReplayableReason?: string;
}

export interface GrantInfo {
  grantId: string;
  origin: string;
  scopes: string[];
  ttlSeconds: number;
  label: string;
}

/**
 * Wire-shape error payload on a Receipt (host_service_adapter ReceiptError).
 */
export interface ReceiptError {
  /**
   * Stable typed failure kind, e.g. `"http_status"`, `"dns_failure"`,
   * `"connect_refused"`, `"tls_error"`, `"url_blocked"`, `"shim_failure"`.
   */
  kind: string;
  /**
   * Kind-specific fields (e.g. `{status_code, url}` for `"http_status"`,
   * `{url, chromium_error}` for transport-layer kinds). `undefined` for
   * kinds with no kind-specific data.
   */
  detail?: unknown;
}

export interface Receipt {
  actionHash: string;
  outcomeHash: string;
  emittedAtMs: number;
  /**
   * Receipt-level outcome: `"success" | "error" | "aborted"` (ReceiptStatus
   * in loom-rpc host_service_adapter). Failed actions (DNS failure,
   * HTTP 4xx/5xx, blocked URL, shim failure) return as a SUCCESSFUL JSON-RPC
   * result whose receipt has `status === "error"` — check `ok`/`status`
   * instead of relying on a thrown error. Optional for backward
   * compatibility; populated on every receipt the SDK parses.
   */
  status?: string;
  /** Convenience: `status === "success"`. Populated on every receipt. */
  ok?: boolean;
  /** Typed failure payload when `status !== "success"`; absent on success. */
  error?: ReceiptError;
  // ---- navigate tier-2 fields (absent for non-navigate verbs) ----
  url?: string;
  finalUrl?: string;
  title?: string;
  statusCode?: number;
  domSnapshotHash?: string;
  screenshotAfterHash?: string;
  /**
   * Evaluate tier: JS expression result, canonical-JSON encoded. `undefined`
   * means "not an evaluate action" or "result offloaded to the content
   * store" (in which case `returnValueBlobRef` carries the SHA-256).
   */
  returnValueJson?: string;
  returnValueBlobRef?: string;
  /**
   * settle-capture: the readiness mode the capture was gated on
   * (`"load" | "networkidle" | "settled"`). Present on `navigate` receipts;
   * `undefined` for verbs without a readiness gate.
   */
  settleUntil?: string;
  /**
   * settle-capture: how the readiness wait ended
   * (`"reached" | "timeout" | "dom_unstable"`). `"timeout"`/`"dom_unstable"`
   * mean the bounded fallback fired — the page never reached the requested
   * readiness state (e.g. a persistent connection or a perpetually-animating
   * DOM). Present on `navigate` receipts.
   */
  settleOutcome?: string;
}

/**
 * A JSON Schema object as returned by rpc.schemas(). Covers the common fields;
 * additional keywords are carried in the extension index signature.
 */
export interface JsonSchemaObject {
  type?: "string" | "number" | "integer" | "boolean" | "array" | "object" | "null";
  properties?: Record<string, JsonSchemaObject>;
  items?: JsonSchemaObject;
  required?: string[];
  description?: string;
  enum?: unknown[];
}

export interface MethodSchema {
  method: string;
  request: JsonSchemaObject;
  response: JsonSchemaObject;
}

export interface SchemaRegistry {
  methods: MethodSchema[];
  sourceWitSha256: string;
}

// ─── daemon.health payload (HAND-WRITTEN — see plan.md decision OOS-1) ───

export type ProbeStatus = "ok" | "timeout" | "error";

export interface ShimBreakerSnapshot {
  shimId: string;
  /** `"closed" | "open" | "half-open"` */
  state: string;
  consecutiveFailures: number;
  openedAtMs: number | null;
}

export interface ShimDeepHealth {
  shimId: string;
  daemonRestartCount: number;
  daemonLastRestartAtMs: number | null;
  shimUptimeMs: number;
  shimRequestsServed: number;
  shimLastRequestAtMs: number | null;
  probeStatus: ProbeStatus;
}

export interface DaemonHealthResult {
  activeSessions: number;
  shimBreakerStates: ShimBreakerSnapshot[];
  /** `"enabled" | "disabled" | "unwired"` */
  otelExporter: string;
  /**
   * Count of orphan Chromium trees: `loom-chromium-*` user-data-dirs whose
   * session is no longer live but whose browser process is still running.
   * Optional for back-compat with older daemons that predate the field
   * (`#[serde(default)]` on the wire); the SDK mapper defaults it to 0.
   */
  orphanBrowserTrees?: number;
  /**
   * Age in seconds of the oldest Active session (by last activity), or
   * `null` if there are no active sessions. A large value flags
   * leaked/stuck sessions. Optional for back-compat with older daemons.
   */
  oldestActiveSessionAgeSecs?: number | null;
  /** Populated only when `daemonHealth({deep: true})` is called AND the
   *  daemon has wired its async deep-probe provider. */
  deep: ShimDeepHealth[] | null;
}
