/**
 * LoomTransport — async Unix socket transport for the loom JSON-RPC 2.0 protocol.
 *
 * Wire protocol:
 *  - Framing: 4-byte big-endian uint32 length prefix + payload bytes.
 *  - HELLO handshake: first frame sent MUST be `HELLO {token}` (UTF-8,
 *    no trailing newline). Server sends NO ACK on success. On auth failure,
 *    server sends one error frame then closes.
 *  - All subsequent frames are JSON-RPC 2.0 request/response pairs.
 *
 * Token discovery order (if token not supplied explicitly):
 *  1. `~/.loom/loom.token` file (daemon writes this at startup).
 *  2. `LOOM_TOKEN` environment variable.
 *  3. Throws LoomTokenError if neither found.
 *
 * Socket path defaults (if socketPath not supplied):
 *  - macOS: ~/Library/Caches/loom/loom.sock
 *  - Linux: $XDG_RUNTIME_DIR/loom.sock
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import {
  LoomAbortError,
  LoomConnectionError,
  LoomError,
  LoomRPCError,
  LoomTokenError,
} from "./errors.js";

function defaultSocketPath(): string {
  if (process.platform === "darwin") {
    return path.join(os.homedir(), "Library", "Caches", "loom", "loom.sock");
  }
  const xdg = process.env["XDG_RUNTIME_DIR"] ?? "/tmp";
  return path.join(xdg, "loom.sock");
}

function resolveToken(token: string | undefined): string {
  if (token !== undefined) return token;
  const tokenFile = path.join(os.homedir(), ".loom", "loom.token");
  if (fs.existsSync(tokenFile)) {
    return fs.readFileSync(tokenFile, "utf8").trim();
  }
  const envToken = process.env["LOOM_TOKEN"];
  if (envToken) return envToken;
  throw new LoomTokenError(
    "No loom token found. Pass token explicitly, set LOOM_TOKEN env var, " +
      "or ensure the daemon has written ~/.loom/loom.token.",
  );
}

interface PendingCall {
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
  settled: boolean;
}

export interface CallOptions {
  /** Cancel the in-flight call. On abort, the transport fires a
   * `request.cancel` JSON-RPC notification at the daemon and rejects the
   * returned promise with `LoomAbortError`. The cancel is fire-and-forget;
   * the daemon's idempotent cancel handler returns `{cancelled: bool}`
   * which the transport silently discards. */
  signal?: AbortSignal;
}

export class LoomTransport {
  private readonly _path: string;
  private readonly _token: string;
  private _socket: net.Socket | null = null;
  private _recvBuffer: Buffer = Buffer.alloc(0);

  // ─── MULTIPLEXER STATE ─────────────────────────────────────────────────
  // Monotonic id allocator + id-keyed pending map + single persistent data
  // listener. Required for `request.cancel` correlation: the cancel
  // response can arrive on the same connection while the original call is
  // still awaiting, and per-call listeners would mis-route the frame.
  // Mirror in python-sdk/loom/_async_transport.py.
  // ───────────────────────────────────────────────────────────────────────
  private _nextId = 1;
  private _pending = new Map<number, PendingCall>();
  // Latched daemon-level protocol error (e.g. the HELLO auth-failure
  // frame — a bare JsonRpcError with no id, after which the daemon
  // closes). Once set, every subsequent call() throws it instead of a
  // generic connection error, even when no call was pending when the
  // frame arrived.
  private _daemonError: LoomRPCError | null = null;
  // Latched terminal connection state. Set when the socket errors or the
  // daemon hangs up (e.g. its ~300s authenticated-idle timeout silently
  // closes the connection) and on client-side close(). Once set, call()
  // fails fast with this typed error instead of writing to a destroyed
  // stream and surfacing Node's misleading "Cannot call write after a
  // stream was destroyed" an event-loop turn later. First cause wins
  // (`??=`): a client close() is not re-labelled as a daemon hangup by
  // the subsequent 'close' event.
  private _closeReason: LoomConnectionError | null = null;

  constructor(socketPath?: string, token?: string) {
    this._path = socketPath ?? defaultSocketPath();
    this._token = resolveToken(token);
  }

  async connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const sock = net.createConnection({ path: this._path });
      sock.once("connect", () => {
        this._socket = sock;
        // Reset terminal state + any stale partial frame from a previous
        // connection so a reconnect on the same transport is usable.
        this._closeReason = null;
        this._recvBuffer = Buffer.alloc(0);
        // Install persistent listeners ONCE — they live for the
        // lifetime of the connection. Per-call listener attach/detach
        // would scramble frames when `request.cancel` is sent while
        // another call is in flight.
        sock.on("data", this._onData);
        sock.on("error", this._onError);
        sock.on("close", this._onClose);
        // Send HELLO frame — no ACK expected from server.
        const hello = Buffer.from(`HELLO ${this._token}`, "utf8");
        this._sendFrame(hello);
        resolve();
      });
      sock.once("error", (err) => {
        reject(
          new LoomConnectionError(
            `Cannot connect to loom socket at ${this._path}: ${err.message}`,
          ),
        );
      });
    });
  }

  async call(
    method: string,
    params: Record<string, unknown>,
    opts?: CallOptions,
  ): Promise<unknown> {
    if (this._daemonError) {
      throw this._daemonError;
    }
    if (this._closeReason) {
      throw this._closeReason;
    }
    if (!this._socket) {
      throw new LoomConnectionError("Transport not connected. Call connect() first.");
    }
    // Pre-aborted signal: synchronous reject; no frame sent.
    if (opts?.signal?.aborted) {
      throw new LoomAbortError("request aborted before send");
    }

    const id = this._nextId++;

    // Serialize BEFORE mutating any transport state: JSON.stringify throws
    // synchronously on circular references / BigInt (reachable via
    // user-controlled params such as SessionCreateOptions.budget). If the
    // pending entry were registered first, the throw would orphan it and a
    // later transport close/error would reject a promise nobody awaits —
    // an unhandledRejection that kills the process by default.
    let request: string;
    try {
      request = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    } catch (err) {
      throw new LoomError(
        `params for '${method}' are not JSON-serializable: ${(err as Error).message}`,
      );
    }

    let abortListener: (() => void) | undefined;

    const settledFlag: PendingCall = {
      resolve: () => {},
      reject: () => {},
      settled: false,
    };

    const promise = new Promise<unknown>((resolve, reject) => {
      settledFlag.resolve = (v) => {
        if (settledFlag.settled) return;
        settledFlag.settled = true;
        if (abortListener && opts?.signal) {
          opts.signal.removeEventListener("abort", abortListener);
        }
        resolve(v);
      };
      settledFlag.reject = (e) => {
        if (settledFlag.settled) return;
        settledFlag.settled = true;
        if (abortListener && opts?.signal) {
          opts.signal.removeEventListener("abort", abortListener);
        }
        reject(e);
      };
    });

    this._pending.set(id, settledFlag);

    if (opts?.signal) {
      abortListener = () => {
        // If the call already settled, no-op. The settled flag inside
        // resolve/reject above also guards this.
        if (settledFlag.settled) return;
        // Fire-and-forget cancel envelope; daemon is idempotent.
        this._sendCancel(id);
        settledFlag.reject(new LoomAbortError("request aborted"));
        this._pending.delete(id);
      };
      opts.signal.addEventListener("abort", abortListener, { once: true });
    }

    this._sendFrame(Buffer.from(request, "utf8"));
    return promise;
  }

  async close(): Promise<void> {
    this._closeReason ??= new LoomConnectionError("Transport closed");
    if (this._socket) {
      // Detach the persistent listeners BEFORE destroying: the old
      // socket's trailing 'close' event must not re-latch a dead state
      // (or null a fresh socket) after a reconnect.
      this._socket.off("data", this._onData);
      this._socket.off("error", this._onError);
      this._socket.off("close", this._onClose);
      this._socket.destroy();
      this._socket = null;
    }
    // Reject any pending calls so awaiters don't hang.
    for (const [, entry] of this._pending) {
      entry.reject(new LoomConnectionError("Transport closed"));
    }
    this._pending.clear();
  }

  // ------------------------------------------------------------------ //
  // Internal framing

  private _sendFrame(data: Buffer): void {
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(data.length, 0);
    this._socket!.write(Buffer.concat([header, data]));
  }

  private _sendCancel(targetId: number): void {
    if (!this._socket) return;
    // Allocate a fresh id for the cancel envelope itself; the daemon's
    // response (`{cancelled: bool}`) will hit the reader and be silently
    // discarded since we don't register a pending entry for it.
    const id = this._nextId++;
    const env = JSON.stringify({
      jsonrpc: "2.0",
      method: "request.cancel",
      params: { request_id: targetId },
      id,
    });
    try {
      this._sendFrame(Buffer.from(env, "utf8"));
    } catch {
      // Cancel is best-effort. If the socket is wedged the original
      // promise has already been rejected anyway.
    }
  }

  // Persistent listeners (arrow-bound so `this` is captured cleanly).
  private _onData = (chunk: Buffer): void => {
    this._recvBuffer = Buffer.concat([this._recvBuffer, chunk]);
    // Drain as many complete frames as the buffer contains.
    while (this._recvBuffer.length >= 4) {
      const length = this._recvBuffer.readUInt32BE(0);
      if (this._recvBuffer.length < 4 + length) break;
      const frame = this._recvBuffer.subarray(4, 4 + length);
      this._recvBuffer = this._recvBuffer.subarray(4 + length);
      this._dispatchFrame(frame);
    }
  };

  private _onError = (err: Error): void => {
    this._socket = null;
    this._closeReason ??= new LoomConnectionError(
      `Connection error: ${err.message} — transport is no longer usable; create a new transport`,
    );
    // Prefer the latched daemon error (e.g. auth failure) — the daemon
    // closes right after sending it, so the socket error is a symptom.
    const e = this._daemonError ?? this._closeReason;
    for (const [, entry] of this._pending) {
      entry.reject(e);
    }
    this._pending.clear();
  };

  private _onClose = (): void => {
    this._socket = null;
    this._closeReason ??= new LoomConnectionError(
      "Connection closed by daemon — transport is no longer usable; create a new transport",
    );
    const e = this._daemonError ?? this._closeReason;
    for (const [, entry] of this._pending) {
      entry.reject(e);
    }
    this._pending.clear();
  };

  private _dispatchFrame(frame: Buffer): void {
    let envelope: Record<string, unknown>;
    try {
      envelope = JSON.parse(frame.toString("utf8")) as Record<string, unknown>;
    } catch {
      // Malformed frame — drop. Logging via console would pollute output;
      // a future debug-log hook could surface it.
      return;
    }
    const id = envelope["id"];
    if (typeof id !== "number") {
      // No id. A no-id ERROR frame is a daemon-level protocol error
      // (auth failure, malformed request) — reject ALL pending with it
      // so the first pending call surfaces the typed envelope. The real
      // daemon's HELLO auth-failure frame is a BARE serialized
      // JsonRpcError — {"code": ..., "message": ...} with no
      // {"error": ...} wrapper (loom-rpc connection_handler send_error)
      // — so recognize both shapes. Latch the error so a later call()
      // surfaces it even when nothing was pending when the frame
      // arrived. Anything else (notification, malformed) we silently
      // drop.
      const e =
        "error" in envelope
          ? LoomRPCError.fromEnvelope(envelope)
          : LoomRPCError.fromBareFrame(envelope);
      if (e) {
        this._daemonError = e;
        for (const [, entry] of this._pending) {
          entry.reject(e);
        }
        this._pending.clear();
      }
      return;
    }
    const entry = this._pending.get(id);
    if (!entry) {
      // Unknown id — likely a `request.cancel` response that we didn't
      // register a pending for. Silently discard.
      return;
    }
    this._pending.delete(id);
    if ("error" in envelope) {
      entry.reject(LoomRPCError.fromEnvelope(envelope));
    } else {
      entry.resolve(envelope["result"]);
    }
  }
}
