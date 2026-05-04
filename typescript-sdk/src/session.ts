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
  SchemaRegistry,
  JsonSchemaObject,
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

function toReceipt(d: Record<string, unknown>): Receipt {
  return {
    actionHash: (d["action_hash"] as string) ?? "",
    outcomeHash: (d["outcome_hash"] as string) ?? "",
    emittedAtMs: (d["emitted_at_ms"] as number) ?? 0,
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
    const params: Record<string, unknown> = {
      profile: opts.profile ?? "default",
      network_mode: opts.networkMode ?? "live",
      capture: opts.capture ?? true,
    };
    if (opts.seed !== undefined) params["seed"] = opts.seed;
    if (opts.budget !== undefined) params["budget"] = opts.budget;
    const result = (await transport.call("session.create", params)) as Record<string, unknown>;
    return new Session(
      result["session_id"] as string,
      (result["status"] as string) ?? "active",
      transport,
    );
  }

  async close(): Promise<SessionInfo> {
    const result = (await this._transport.call("session.close", {
      session_id: this.sessionId,
    })) as Record<string, unknown> | null;
    await this._transport.close();
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

  async navigate(url: string, opts: { deadlineMs?: number } = {}): Promise<Receipt> {
    const result = (await this._transport.call(
      "action.web.navigate",
      buildActionParams(this.sessionId, "navigate", { url }, opts.deadlineMs ?? 5000),
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
  const result = (await transport.call("session.list", {})) as Array<Record<string, unknown>>;
  await transport.close();
  return (result ?? []).map(toSessionInfo);
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
  const result = (await transport.call("vault.grant", {
    session_id: sessionId,
    origin,
    scopes,
    ttl_seconds: ttlSeconds,
    label,
  })) as Record<string, unknown>;
  await transport.close();
  return {
    grantId: result["grant_id"] as string,
    origin: (result["origin"] as string) ?? "",
    scopes: (result["scopes"] as string[]) ?? [],
    ttlSeconds: (result["ttl_seconds"] as number) ?? 0,
    label: (result["label"] as string) ?? "",
  };
}

export async function vaultRevoke(
  grantId: string,
  reason: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<void> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  await transport.call("vault.revoke", { grant_id: grantId, reason });
  await transport.close();
}

export async function vaultListGrants(
  sessionId?: string,
  opts: { socketPath?: string; token?: string } = {},
): Promise<GrantInfo[]> {
  const transport = new LoomTransport(opts.socketPath, opts.token);
  await transport.connect();
  const params: Record<string, unknown> = {};
  if (sessionId !== undefined) params["session_id"] = sessionId;
  const result = (await transport.call("vault.list_grants", params)) as Array<
    Record<string, unknown>
  >;
  await transport.close();
  return (result ?? []).map((g) => ({
    grantId: g["grant_id"] as string,
    origin: (g["origin"] as string) ?? "",
    scopes: (g["scopes"] as string[]) ?? [],
    ttlSeconds: (g["ttl_seconds"] as number) ?? 0,
    label: (g["label"] as string) ?? "",
  }));
}
