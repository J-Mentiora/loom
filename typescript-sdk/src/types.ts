/**
 * TypeScript types for the loom SDK.
 *
 * Types mirror wire shapes in:
 *   - systems/loom-rpc/modules/core_service_adapter/interfaces.rs
 *   - systems/loom-rpc/modules/host_service_adapter/interfaces.rs
 *   - systems/loom-rpc/modules/error_translator/interfaces.rs
 *
 * camelCase property names are used for TypeScript idiom; wire uses snake_case
 * (transformation happens in transport/session layers).
 */

export type LoomErrorCode =
  | "protocol_auth_required"
  | "protocol_malformed"
  | "schema_violation"
  | "method_not_found"
  | "session_not_found"
  | "session_aborted"
  | "budget_exceeded"
  | "surface_trap"
  | "surface_unavailable"
  | "vault_grant_not_found"
  | "vault_grant_revoked"
  | "vault_credential_type_unsupported"
  | "store_integrity_failed"
  | "internal_error";

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
  /** Populated only when `daemonHealth({deep: true})` is called AND the
   *  daemon has wired its async deep-probe provider. */
  deep: ShimDeepHealth[] | null;
}
