/**
 * Session — high-level loom client.
 *
 * Create via `await Session.create()`. Use `await using` or call `close()` explicitly.
 */
import { LoomTransport } from "./transport.js";
import type {
  SessionInfo,
  SessionInspection,
  DiffReport,
  ExportInfo,
  ValidationResult,
  GrantInfo,
  Receipt,
  ReceiptError,
  SchemaRegistry,
  JsonSchemaObject,
  DaemonHealthResult,
  ProbeStatus,
} from "./types.js";

function buildActionParams(
  sessionId: string,
  kind: string,
  payload: Record<string, unknown>,
  deadlineMs: number,
): Record<string, unknown> {
  return {
    session_id: sessionId,
    action: {
      kind,
      payload: Array.from(Buffer.from(JSON.stringify(payload), "utf8")),
      deadline_ms: deadlineMs,
    },
  };
}

function toSessionInfo(d: Record<string, unknown>): SessionInfo {
  return {
    sessionId: d["session_id"] as string,
    status: d["status"] as string,
    createdAtMs: (d["created_at_ms"] as number) ?? 0,
  };
}

function toReceiptError(raw: unknown): ReceiptError | undefined {
  // The daemon serializes `error: null` on success — treat null/non-object
  // the same as absent.
  if (raw === null || typeof raw !== "object") return undefined;
  const e = raw as Record<string, unknown>;
  return { kind: (e["kind"] as string) ?? "", detail: e["detail"] };
}

function toReceipt(d: Record<string, unknown>): Receipt {
  // Receipt-level outcome ("success" | "error" | "aborted"). Failed actions
  // return as a SUCCESSFUL JSON-RPC result whose receipt has status="error"
  // — surface it so callers can distinguish failures from successes.
  const status = (d["status"] as string) ?? "success";
  return {
    actionHash: (d["action_hash"] as string) ?? "",
    outcomeHash: (d["outcome_hash"] as string) ?? "",
    emittedAtMs: (d["emitted_at_ms"] as number) ?? 0,
    status,
    ok: status === "success",
    error: toReceiptError(d["error"]),
    // navigate tier-2 fields: absent (→ undefined) for non-navigate verbs.
    url: d["url"] as string | undefined,
    finalUrl: d["final_url"] as string | undefined,
    title: d["title"] as string | undefined,
    statusCode: d["status_code"] as number | undefined,
    domSnapshotHash: d["dom_snapshot_hash"] as string | undefined,
    screenshotAfterHash: d["screenshot_after_hash"] as string | undefined,
    // evaluate tier fields.
    returnValueJson: d["return_value_json"] as string | undefined,
    returnValueBlobRef: d["return_value_blob_ref"] as string | undefined,
    // settle-capture: present on navigate receipts; absent (→ undefined) on
    // verbs without a readiness gate.
    settleUntil: d["settle_until"] as string | undefined,
    settleOutcome: d["settle_outcome"] as string | undefined,
  };
}

export interface SessionCreateOptions {
  profile?: string;
  networkMode?: string;
  capture?: boolean;
  seed?: number;
  budget?: unknown;
  socketPath?: string;
  token?: string;
  /**
   * Disable determinism for this session (settle-capture). Determinism is ON
   * by default: loom freezes `Date.now`/animations and seeds `Math.random` so
   * captures are byte-reproducible. Set `true` for live/non-reproducible
   * capture (real clock + unseeded RNG). Such a session is recorded as
   * NON-REPLAYABLE — replay refuses it.
   */
  noDeterminism?: boolean;
}

export class Session {
  readonly sessionId: string;
  readonly status: string;
  private readonly _transport: LoomTransport;

  private constructor(sessionId: string, status: string, transport: LoomTransport) {
    this.sessionId = sessionId;
    this.status = status;
    this._transport = transport;
  }

  static async create(opts: SessionCreateOptions = {}): Promise<Session> {
    const transport = new LoomTransport(opts.socketPath, opts.token);
    await transport.connect();
    try {
      const params: Record<string, unknown> = {
        profile: opts.profile ?? "default",
        network_mode: opts.networkMode ?? "live",
        capture: opts.capture ?? true,
      };
      if (opts.seed !== undefined) params["seed"] = opts.seed;
      if (opts.budget !== undefined) params["budget"] = opts.budget;
      if (opts.noDeterminism) params["no_determinism"] = true;
      const result = (await transport.call("session.create", params)) as Record<string, unknown>;
      return new Session(
        result["session_id"] as string,
        (result["status"] as string) ?? "active",
        transport,
      );
    } catch (err) {
      // Don't leak the connected socket when the RPC fails (schema
      // violation, unknown profile, auth failure, …) — an open net.Socket
      // is an active libuv handle that keeps the process alive.
      await transport.close();
      throw err;
    }
  }

  async close(): Promise<SessionInfo> {
    let result: Record<string, unknown> | null;
    try {
      result = (await this._transport.call("session.close", {
        session_id: this.sessionId,
      })) as Record<string, unknown> | null;
    } finally {
      // Always release the socket, even when the RPC fails.
      await this._transport.close();
    }
    if (result) return toSessionInfo(result);
    return { sessionId: this.sessionId, status: "closed", createdAtMs: 0 };
  }

  async abort(reason: string): Promise<SessionInfo> {
    const result = (await this._transport.call("session.abort", {
      session_id: this.sessionId,
      reason,
    })) as Record<string, unknown>;
    return toSessionInfo(result);
  }

  /**
   * Force-terminate a stuck session.
   *
   * Use `close()` for normal shutdown. Use `abort()` to cancel in-flight
   * actions while keeping the session. Use `kill()` ONLY when those
   * don't return — it tears down the shim with a 5s ceiling then SIGKILL.
   *
   * Delegates to {@link _doKillSession} to keep one RPC call site.
   */
  async kill(): Promise<void> {
    return _doKillSession(this._transport, this.sessionId);
  }

  async inspect(atAction?: number): Promise<SessionInspection> {
    const params: Record<string, unknown> = { session_id: this.sessionId };
    if (atAction !== undefined) params["at_action"] = atAction;
    const result = (await this._transport.call("session.inspect", params)) as Record<
      string,
      unknown
    >;
    return {
      sessionId: result["session_id"] as string,
      atAction: (result["at_action"] as number | null) ?? null,
      manifestSummary: (result["manifest_summary"] as Record<string, unknown>) ?? {},
    };
  }

  async export(format: string): Promise<ExportInfo> {
    const result = (await this._transport.call("session.export", {
      session_id: this.sessionId,
      format,
    })) as Record<string, unknown>;
    return {
      sessionId: result["session_id"] as string,
      format: result["format"] as string,
      artifactRef: (result["artifact_ref"] as string) ?? "",
    };
  }

  async validate(): Promise<ValidationResult> {
    const result = (await this._transport.call("session.validate", {
      session_id: this.sessionId,
    })) as Record<string, unknown>;
    return {
      sessionId: result["session_id"] as string,
      passed: (result["passed"] as boolean) ?? false,
      reasons: (result["reasons"] as string[]) ?? [],
    };
  }

  async replay(opts: { speed?: number; networkMode?: string } = {}): Promise<SessionInfo> {
    const result = (await this._transport.call("session.replay", {
      session_id: this.sessionId,
      speed: opts.speed ?? 1.0,
      network_mode: opts.networkMode ?? "replay",
    })) as Record<string, unknown>;
    return toSessionInfo(result);
  }

  async diff(
    otherSessionId: string,
    opts: { includeScreenshots?: boolean; showDomDiffs?: boolean } = {},
  ): Promise<DiffReport> {
    const result = (await this._transport.call("session.diff", {
      a: this.sessionId,
      b: otherSessionId,
      include_screenshots: opts.includeScreenshots ?? false,
      show_dom_diffs: opts.showDomDiffs ?? false,
    })) as Record<string, unknown>;
    return {
      a: result["a"] as string,
      b: result["b"] as string,
      diff: (result["diff"] as Record<string, unknown>) ?? {},
    };
  }

  /**
   * Navigate the page and capture DOM + screenshot, gating the capture on a
   * readiness state (settle-capture). By default loom waits until the page is
   * `"settled"` (network quiet + `readyState` complete + the final URL stable
   * after client-side redirects + the DOM quiescent) so the capture is a real
   * rendered page, not a blank SPA shell or an arbitrary animation frame.
   *
   * - `until`: `"load"` | `"networkidle"` | `"settled"` (default `"settled"`).
   * - `timeoutMs`: bound on the readiness wait. If readiness is never reached
   *   (persistent connection, perpetual animation) the call still returns — the
   *   receipt's {@link Receipt.settleOutcome} is `"timeout"` / `"dom_unstable"`
   *   instead of `"reached"`. It never hangs.
   */
  async navigate(
    url: string,
    opts: { deadlineMs?: number; until?: "load" | "networkidle" | "settled"; timeoutMs?: number } = {},
  ): Promise<Receipt> {
    const payload: Record<string, unknown> = { url };
    if (opts.until !== undefined) payload["until"] = opts.until;
    if (opts.timeoutMs !== undefined) payload["timeout_ms"] = opts.timeoutMs;
    const result = (await this._transport.call(
      "action.web.navigate",
      buildActionParams(this.sessionId, "navigate", payload, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  /**
   * Wait for the CURRENT page to reach a readiness state (settle-capture),
   * without navigating. Use after a navigate or an interaction that triggers
   * async re-render to gate a subsequent screenshot/snapshot on real
   * readiness instead of a magic sleep.
   *
   * - `until`: `"load"` | `"networkidle"` | `"settled"` (default `"settled"`).
   * - `timeoutMs`: bound on the wait. If readiness is never reached the call
   *   still returns — the receipt's {@link Receipt.settleOutcome} is
   *   `"timeout"` / `"dom_unstable"` instead of `"reached"`. It never hangs.
   */
  async waitFor(
    opts: { deadlineMs?: number; until?: "load" | "networkidle" | "settled"; timeoutMs?: number } = {},
  ): Promise<Receipt> {
    const payload: Record<string, unknown> = {};
    if (opts.until !== undefined) payload["until"] = opts.until;
    if (opts.timeoutMs !== undefined) payload["timeout_ms"] = opts.timeoutMs;
    const result = (await this._transport.call(
      "action.web.wait_for",
      buildActionParams(this.sessionId, "wait_for", payload, opts.deadlineMs ?? 30000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async click(selector: string, opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.click",
      buildActionParams(this.sessionId, "click", { selector }, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async typeText(
    selector: string,
    text: string,
    opts: { deadlineMs?: number } = {},
  ): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.type_text",
      buildActionParams(this.sessionId, "type_text", { selector, text }, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async select(
    selector: string,
    value: string,
    opts: { deadlineMs?: number } = {},
  ): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.select",
      buildActionParams(this.sessionId, "select", { selector, value }, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async hover(selector: string, opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.hover",
      buildActionParams(this.sessionId, "hover", { selector }, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async scroll(
    selector: string,
    opts: { deltaY?: number; deadlineMs?: number } = {},
  ): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.scroll",
      buildActionParams(
        this.sessionId,
        "scroll",
        { selector, delta_y: opts.deltaY ?? 300 },
        opts.deadlineMs ?? 5000,
      ),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async wait(selector: string, opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.wait",
      buildActionParams(this.sessionId, "wait", { selector }, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async evaluate(expression: string, opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.evaluate",
      buildActionParams(
        this.sessionId,
        "evaluate",
        { expression },
        opts.deadlineMs ?? 5000,
      ),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async screenshot(opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.screenshot",
      buildActionParams(this.sessionId, "screenshot", {}, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async snapshot(opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.snapshot",
      buildActionParams(this.sessionId, "snapshot", {}, opts.deadlineMs ?? 5000),
    )) as Record<string, unknown>;
    return toReceipt(result);
  }

  async schemas(): Promise<SchemaRegistry> {
    const result = (await this._transport.call("rpc.schemas", {})) as Record<string, unknown>;
    return {
      methods: (result["methods"] as Array<Record<string, unknown>>).map((m) => ({
        method: m["method"] as string,
        request: (m["request"] as JsonSchemaObject) ?? {},
        response: (m["response"] as JsonSchemaObject) ?? {},
      })),
      sourceWitSha256: (result["source_wit_sha256"] as string) ?? "",
    };
  }

  [Symbol.asyncDispose](): Promise<SessionInfo> {
    return this.close();
  }
}

export async function sessionList(opts: {
  socketPath?: string;
  token?: string;
} = {}): Promise<SessionInfo[]> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    const result = (await transport.call("session.list", {})) as Array<Record<string, unknown>>;
    return (result ?? []).map(toSessionInfo);
  } finally {
    await transport.close();
  }
}

export async function vaultGrant(
  sessionId: string,
  origin: string,
  scopes: string[],
  ttlSeconds: number,
  label: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<GrantInfo> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    const result = (await transport.call("vault.grant", {
      session_id: sessionId,
      origin,
      scopes,
      ttl_seconds: ttlSeconds,
      label,
    })) as Record<string, unknown>;
    return {
      grantId: result["grant_id"] as string,
      origin: (result["origin"] as string) ?? "",
      scopes: (result["scopes"] as string[]) ?? [],
      ttlSeconds: (result["ttl_seconds"] as number) ?? 0,
      label: (result["label"] as string) ?? "",
    };
  } finally {
    await transport.close();
  }
}

export async function vaultRevoke(
  grantId: string,
  reason: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<void> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    await transport.call("vault.revoke", { grant_id: grantId, reason });
  } finally {
    await transport.close();
  }
}

export async function vaultListGrants(
  sessionId?: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<GrantInfo[]> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    const params: Record<string, unknown> = {};
    if (sessionId !== undefined) params["session_id"] = sessionId;
    const result = (await transport.call("vault.list_grants", params)) as Array<
      Record<string, unknown>
    >;
    return (result ?? []).map((g) => ({
      grantId: g["grant_id"] as string,
      origin: (g["origin"] as string) ?? "",
      scopes: (g["scopes"] as string[]) ?? [],
      ttlSeconds: (g["ttl_seconds"] as number) ?? 0,
      label: (g["label"] as string) ?? "",
    }));
  } finally {
    await transport.close();
  }
}

// ─── admin RPCs (session.kill, daemon.health) ────────────────────────────

/** Internal: single `session.kill` call site shared by Session.kill() and
 *  the top-level killSession() free function. */
async function _doKillSession(transport: LoomTransport, sessionId: string): Promise<void> {
  await transport.call("session.kill", { session_id: sessionId });
}

/**
 * ADMIN ESCAPE HATCH — force-terminate a stuck session by id without
 * holding a Session handle. Performs the abort flow plus a blocking 5s
 * shim-teardown ceiling, then SIGKILL. Prefer `session.close()` for normal
 * shutdown; reach for `killSession()` only when normal shutdown is wedged.
 *
 * The daemon authenticates the calling transport at the connection level
 * (HELLO token handshake) — there is no separate per-call auth on this
 * admin function.
 */
export async function killSession(
  sessionId: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<void> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    await _doKillSession(transport, sessionId);
  } finally {
    await transport.close();
  }
}

/**
 * Query daemon health. Shallow path is non-blocking. `{deep: true}` fans
 * out a per-shim probe (1s budget per shim, 3s overall) and returns
 * uptime/requests-served counters per running shim.
 *
 * The optional `signal` cancels the in-flight call: the transport fires a
 * `request.cancel` envelope and rejects with `LoomAbortError`.
 *
 * Auth: requires the existing socket-auth token; no separate per-call gate.
 */
export async function daemonHealth(
  opts: {
    deep?: boolean;
    signal?: AbortSignal;
    socketPath?: string;
    token?: string;
  } = {},
): Promise<DaemonHealthResult> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  try {
    const result = (await transport.call(
      "daemon.health",
      { deep: opts.deep ?? false },
      { signal: opts.signal },
    )) as Record<string, unknown>;
    return toDaemonHealthResult(result);
  } finally {
    await transport.close();
  }
}

function toDaemonHealthResult(d: Record<string, unknown>): DaemonHealthResult {
  return {
    activeSessions: (d["active_sessions"] as number) ?? 0,
    shimBreakerStates: ((d["shim_breaker_states"] as Array<Record<string, unknown>>) ?? []).map(
      (s) => ({
        shimId: s["shim_id"] as string,
        state: s["state"] as string,
        consecutiveFailures: (s["consecutive_failures"] as number) ?? 0,
        openedAtMs: (s["opened_at_ms"] as number | null) ?? null,
      }),
    ),
    otelExporter: (d["otel_exporter"] as string) ?? "unknown",
    deep:
      d["deep"] === null || d["deep"] === undefined
        ? null
        : (d["deep"] as Array<Record<string, unknown>>).map((s) => ({
            shimId: s["shim_id"] as string,
            daemonRestartCount: (s["daemon_restart_count"] as number) ?? 0,
            daemonLastRestartAtMs: (s["daemon_last_restart_at_ms"] as number | null) ?? null,
            shimUptimeMs: (s["shim_uptime_ms"] as number) ?? 0,
            shimRequestsServed: (s["shim_requests_served"] as number) ?? 0,
            shimLastRequestAtMs: (s["shim_last_request_at_ms"] as number | null) ?? null,
            probeStatus: s["probe_status"] as ProbeStatus,
          })),
  };
}
