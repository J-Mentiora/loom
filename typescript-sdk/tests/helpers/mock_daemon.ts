/**
 * MockDaemon — loom-rpc protocol mock for tests.
 *
 * Wire protocol (mirrors Python conftest.py):
 *  1. Server listens on Unix socket.
 *  2. Client connects and sends first frame: `HELLO {token}` — 4-byte big-endian
 *     uint32 length prefix + UTF-8 payload, no trailing newline.
 *  3. Server reads HELLO frame. Token mismatch → error frame + close. On success →
 *     silently enter Authenticated state (no ACK).
 *  4. Loop: read framed JSON-RPC 2.0 request → dispatch → write framed JSON response.
 *
 * Key design: one persistent `data` listener per connection accumulates bytes into a
 * shared buffer. `readFrame()` drains complete frames from that buffer, correctly
 * handling the case where HELLO and the first request arrive in the same TCP segment.
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";

export const DAEMON_TOKEN = "test-token-abc123";

export const SCHEMA_REGISTRY = {
  methods: [
    {
      method: "session.create",
      request: {
        type: "object",
        properties: {
          profile: { type: "string" },
          network_mode: { type: "string" },
          capture: { type: "boolean" },
        },
      },
      response: {
        type: "object",
        properties: {
          session_id: { type: "string" },
          status: { type: "string" },
          created_at_ms: { type: "integer" },
        },
      },
    },
    {
      method: "session.list",
      request: { type: "object", properties: {} },
      response: { type: "array" },
    },
    {
      method: "rpc.schemas",
      request: { type: "object", properties: {} },
      response: {
        type: "object",
        properties: {
          methods: { type: "array" },
          source_wit_sha256: { type: "string" },
        },
      },
    },
  ],
  source_wit_sha256: "aabbccdd".repeat(8),
};

function sendFrame(socket: net.Socket, payload: string): void {
  const data = Buffer.from(payload, "utf8");
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(data.length, 0);
  socket.write(Buffer.concat([header, data]));
}

function makeErrorEnvelope(code: string, message: string, id?: number): string {
  const env: Record<string, unknown> = { error: { code, message } };
  if (id !== undefined) env["id"] = id;
  return JSON.stringify(env);
}

/**
 * The real daemon's HELLO-failure shape: a BARE serialized JsonRpcError —
 * `{"code", "message"}` with NO `{"error": ...}` wrapper and NO `id`
 * (loom-rpc `connection_handler::send_error`; pinned by the daemon's own
 * integration tests asserting top-level `resp["code"]`).
 */
function makeBareErrorFrame(code: string, message: string): string {
  return JSON.stringify({ code, message });
}

function makeResult(result: unknown, id?: number): string {
  const env: Record<string, unknown> = { result };
  if (id !== undefined) env["id"] = id;
  return JSON.stringify(env);
}

type Handler = (params: Record<string, unknown>) => unknown | Promise<unknown>;

/** Per-connection state: persistent buffer + queue of pending frame-read callbacks. */
function makeConnectionReader(conn: net.Socket): { readFrame: () => Promise<Buffer> } {
  let buf = Buffer.alloc(0);
  // Callbacks waiting for the next complete frame
  type Waiter = { resolve: (f: Buffer) => void; reject: (e: Error) => void };
  const waiters: Waiter[] = [];
  let closed = false;

  const tryFlush = () => {
    while (waiters.length > 0 && buf.length >= 4) {
      const length = buf.readUInt32BE(0);
      if (buf.length < 4 + length) break;
      const frame = Buffer.from(buf.subarray(4, 4 + length)); // copy out
      buf = buf.subarray(4 + length);
      const waiter = waiters.shift()!;
      waiter.resolve(frame);
    }
  };

  const rejectAll = (err: Error) => {
    closed = true;
    while (waiters.length > 0) {
      const w = waiters.shift()!;
      w.reject(err);
    }
  };

  conn.on("data", (chunk: Buffer) => {
    buf = Buffer.concat([buf, chunk]);
    tryFlush();
  });
  conn.on("close", () => rejectAll(new Error("connection closed")));
  conn.on("error", (err) => rejectAll(err));

  const readFrame = (): Promise<Buffer> =>
    new Promise((resolve, reject) => {
      if (closed) {
        reject(new Error("connection closed"));
        return;
      }
      waiters.push({ resolve, reject });
      tryFlush(); // may resolve immediately if data already in buf
    });

  return { readFrame };
}

export class MockDaemon {
  readonly token: string;
  readonly socketPath: string;
  readonly tokenFile: string;
  readonly sessions: Map<string, Record<string, unknown>> = new Map();

  private _server: net.Server | null = null;
  private _tmpDir: string;
  private _handlers: Map<string, Handler> = new Map();
  private _sessionCounter = 0;
  private _openConnections: Set<net.Socket> = new Set();

  constructor(token: string = DAEMON_TOKEN) {
    this.token = token;
    this._tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "loom-mock-"));
    this.socketPath = path.join(this._tmpDir, "loom.sock");
    this.tokenFile = path.join(this._tmpDir, "loom.token");
    fs.writeFileSync(this.tokenFile, this.token);
    this._registerDefaults();
  }

  start(): Promise<void> {
    return new Promise((resolve) => {
      this._server = net.createServer((conn) => {
        this._openConnections.add(conn);
        conn.once("close", () => this._openConnections.delete(conn));
        void this._handleConnection(conn);
      });
      this._server.listen(this.socketPath, () => resolve());
    });
  }

  stop(): Promise<void> {
    return new Promise((resolve) => {
      if (!this._server) {
        resolve();
        return;
      }
      // Force-destroy any lingering open connections so server.close()
      // can complete promptly. Without this, an async handler still
      // awaiting (e.g. a setTimeout in a test "slow handler") would
      // keep the connection — and thus the test runner — alive.
      for (const c of this._openConnections) {
        c.destroy();
      }
      this._openConnections.clear();
      this._server.close(() => {
        fs.rmSync(this._tmpDir, { recursive: true, force: true });
        resolve();
      });
    });
  }

  registerHandler(method: string, fn: Handler): void {
    this._handlers.set(method, fn);
  }

  /** Number of client connections the daemon currently sees as open.
   *  Used by socket-cleanup tests: a client-side `transport.close()`
   *  (socket destroy) drops this back to zero. */
  get openConnectionCount(): number {
    return this._openConnections.size;
  }

  private _registerDefaults(): void {
    this._handlers.set("session.create", () => {
      this._sessionCounter += 1;
      const sid = `01TEST${String(this._sessionCounter).padStart(20, "0")}`;
      const session: Record<string, unknown> = {
        session_id: sid,
        status: "active",
        created_at_ms: Date.now(),
      };
      this.sessions.set(sid, session);
      return { ...session };
    });

    this._handlers.set("session.list", () => Array.from(this.sessions.values()));

    this._handlers.set("session.close", (params) => {
      const sid = (params["session_id"] as string) ?? "";
      if (this.sessions.has(sid)) {
        const s = this.sessions.get(sid)!;
        s["status"] = "closed";
        return { ...s };
      }
      return { session_id: sid, status: "closed", created_at_ms: 0 };
    });

    this._handlers.set("session.abort", (params) => {
      const sid = (params["session_id"] as string) ?? "";
      if (this.sessions.has(sid)) {
        const s = this.sessions.get(sid)!;
        s["status"] = "aborted";
        return { ...s };
      }
      return { session_id: sid, status: "aborted", created_at_ms: 0 };
    });

    this._handlers.set("rpc.schemas", () => SCHEMA_REGISTRY);
  }

  private async _handleConnection(conn: net.Socket): Promise<void> {
    const { readFrame } = makeConnectionReader(conn);

    try {
      // AwaitingHello: read first frame
      const helloBytes = await readFrame();
      const hello = helloBytes.toString("utf8");
      const parts = hello.split(" ");
      if (parts.length !== 2 || parts[0] !== "HELLO") {
        sendFrame(conn, makeBareErrorFrame("protocol_auth_required", "malformed hello"));
        conn.end();
        return;
      }
      if (parts[1] !== this.token) {
        sendFrame(conn, makeBareErrorFrame("protocol_auth_required", "token mismatch"));
        conn.end();
        return;
      }

      // Authenticated: request dispatch loop
      while (true) {
        let reqBytes: Buffer;
        try {
          reqBytes = await readFrame();
        } catch {
          break;
        }

        let req: Record<string, unknown>;
        try {
          req = JSON.parse(reqBytes.toString("utf8")) as Record<string, unknown>;
        } catch {
          sendFrame(conn, makeErrorEnvelope("protocol_malformed", "invalid JSON"));
          continue;
        }

        const method = (req["method"] as string) ?? "";
        const params = (req["params"] as Record<string, unknown>) ?? {};
        const reqId = typeof req["id"] === "number" ? (req["id"] as number) : undefined;
        const handler = this._handlers.get(method);
        if (!handler) {
          sendFrame(
            conn,
            makeErrorEnvelope("method_not_found", `unknown method: ${method}`, reqId),
          );
          continue;
        }
        // Fire and forget — handlers may be async; dispatch each on its
        // own microtask so concurrent in-flight requests are supported
        // (tests for the new id-keyed demux rely on this).
        void (async () => {
          try {
            const result = await handler(params);
            sendFrame(conn, makeResult(result, reqId));
          } catch (err) {
            sendFrame(conn, makeErrorEnvelope("internal_error", String(err), reqId));
          }
        })();
      }
    } catch {
      // connection-level error — close silently
    } finally {
      conn.destroy();
    }
  }
}
