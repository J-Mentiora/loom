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
import { LoomConnectionError, LoomRPCError, LoomTokenError } from "./errors.js";

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

export class LoomTransport {
  private readonly _path: string;
  private readonly _token: string;
  private _socket: net.Socket | null = null;
  private _recvBuffer: Buffer = Buffer.alloc(0);

  constructor(socketPath?: string, token?: string) {
    this._path = socketPath ?? defaultSocketPath();
    this._token = resolveToken(token);
  }

  async connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const sock = net.createConnection({ path: this._path });
      sock.once("connect", () => {
        this._socket = sock;
        // Send HELLO frame — no ACK expected from server.
        const hello = Buffer.from(`HELLO ${this._token}`, "utf8");
        this._sendFrame(hello);
        resolve();
      });
      sock.once("error", (err) => {
        reject(new LoomConnectionError(`Cannot connect to loom socket at ${this._path}: ${err.message}`));
      });
    });
  }

  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    if (!this._socket) {
      throw new LoomConnectionError("Transport not connected. Call connect() first.");
    }
    const request = JSON.stringify({ jsonrpc: "2.0", method, params, id: 1 });
    this._sendFrame(Buffer.from(request, "utf8"));
    const responseBytes = await this._recvFrame();
    const response = JSON.parse(responseBytes.toString("utf8")) as Record<string, unknown>;
    if ("error" in response) {
      throw LoomRPCError.fromEnvelope(response);
    }
    return response["result"];
  }

  async close(): Promise<void> {
    if (this._socket) {
      this._socket.destroy();
      this._socket = null;
    }
  }

  // ------------------------------------------------------------------ //
  // Internal framing

  private _sendFrame(data: Buffer): void {
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(data.length, 0);
    this._socket!.write(Buffer.concat([header, data]));
  }

  private _recvFrame(): Promise<Buffer> {
    return new Promise((resolve, reject) => {
      const tryConsume = () => {
        if (this._recvBuffer.length >= 4) {
          const length = this._recvBuffer.readUInt32BE(0);
          if (this._recvBuffer.length >= 4 + length) {
            const frame = this._recvBuffer.subarray(4, 4 + length);
            this._recvBuffer = this._recvBuffer.subarray(4 + length);
            cleanup();
            resolve(frame);
          }
        }
      };

      const onData = (chunk: Buffer) => {
        this._recvBuffer = Buffer.concat([this._recvBuffer, chunk]);
        tryConsume();
      };

      const onError = (err: Error) => {
        cleanup();
        reject(new LoomConnectionError(`Connection error mid-frame: ${err.message}`));
      };

      const onClose = () => {
        cleanup();
        reject(new LoomConnectionError("Connection closed by daemon mid-frame"));
      };

      const cleanup = () => {
        this._socket?.removeListener("data", onData);
        this._socket?.removeListener("error", onError);
        this._socket?.removeListener("close", onClose);
      };

      this._socket!.on("data", onData);
      this._socket!.on("error", onError);
      this._socket!.on("close", onClose);

      // Try consuming already-buffered data
      tryConsume();
    });
  }
}
