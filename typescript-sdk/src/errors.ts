/**
 * Error types for the loom TypeScript SDK.
 *
 * LoomErrorCode values mirror LoomErrorCode in loom-rpc/src/error_translator/interfaces.rs.
 * All snake_case strings are stable wire values (BC-RPC-03).
 */

export class LoomError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LoomError";
  }
}

export class LoomRPCError extends LoomError {
  readonly code: string;
  readonly data: unknown;

  constructor(code: string, message: string, data: unknown = null) {
    super(`${code}: ${message}`);
    this.name = "LoomRPCError";
    this.code = code;
    this.data = data;
  }

  static fromEnvelope(envelope: Record<string, unknown>): LoomRPCError {
    const err = (envelope.error as Record<string, unknown>) ?? {};
    return new LoomRPCError(
      (err.code as string) ?? "internal_error",
      (err.message as string) ?? "unknown error",
      err.data ?? null,
    );
  }

  /**
   * Recognize the daemon's BARE `JsonRpcError` frame.
   *
   * On HELLO auth failure the daemon serializes the `JsonRpcError` struct
   * directly — `{"code": ..., "message": ...}` with NO `{"error": ...}`
   * wrapper and NO `id` (loom-rpc `connection_handler::send_error`) — then
   * closes the connection. Returns `null` when the frame is not that shape
   * (normal response envelopes carry `id` and `result`/`error`).
   */
  static fromBareFrame(envelope: Record<string, unknown>): LoomRPCError | null {
    if ("id" in envelope || "result" in envelope || "error" in envelope) return null;
    const code = envelope["code"];
    const message = envelope["message"];
    if (typeof code !== "string" || typeof message !== "string") return null;
    return new LoomRPCError(code, message, envelope["data"] ?? null);
  }
}

export class LoomConnectionError extends LoomError {
  constructor(message: string) {
    super(message);
    this.name = "LoomConnectionError";
  }
}

export class LoomTokenError extends LoomError {
  constructor(message: string) {
    super(message);
    this.name = "LoomTokenError";
  }
}

/**
 * Thrown when a `call()` is cancelled via its AbortSignal. Sets
 * `name = "AbortError"` so existing DOM-idiomatic patterns
 * (`catch (e) { if (e.name === "AbortError") ... }`) work alongside
 * the uniform `LoomError` surface. The optional `cause` captures the
 * underlying `LoomRPCError({code: "request-cancelled"})` if the daemon's
 * typed cancel-confirmation arrived before the local reject.
 */
export class LoomAbortError extends LoomError {
  readonly cause?: unknown;

  constructor(message: string = "request aborted", cause?: unknown) {
    super(message);
    this.name = "AbortError";
    this.cause = cause;
  }
}
