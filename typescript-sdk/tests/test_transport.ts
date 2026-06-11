/**
 * Transport-layer tests: framing, HELLO auth, call/response, error handling.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { LoomTransport, LoomError, LoomRPCError, LoomConnectionError } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

describe("Transport: framing + auth + call/response", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("length-delimited framing: round-trip request receives response", async () => {
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    const result = await t.call("session.list", {});
    assert.ok(Array.isArray(result), "session.list must return an array");
    await t.close();
  });

  test("HELLO auth succeeds with correct token", async () => {
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    const result = await t.call("session.create", {
      profile: "default",
      network_mode: "live",
      capture: true,
    });
    assert.ok(result && typeof (result as Record<string, unknown>).session_id === "string");
    await t.close();
  });

  test("HELLO auth fails with wrong token → LoomRPCError(protocol_auth_required) at connect", async () => {
    const t = new LoomTransport(daemon.socketPath, "wrong-token");
    // The handshake probe surfaces the rejection at connect() — typed,
    // not deferred to the first call.
    await assert.rejects(
      () => t.connect(),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "protocol_auth_required");
        return true;
      },
    );
    await t.close();
  });

  test("unknown method returns LoomRPCError(method_not_found)", async () => {
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    await assert.rejects(
      () => t.call("no.such.method", {}),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "method_not_found");
        return true;
      },
    );
    await t.close();
  });

  test("multiple sequential calls on one connection all succeed", async () => {
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    const r1 = (await t.call("session.create", {
      profile: "default",
      network_mode: "live",
      capture: true,
    })) as Record<string, unknown>;
    const r2 = (await t.call("session.create", {
      profile: "default",
      network_mode: "live",
      capture: true,
    })) as Record<string, unknown>;
    const r3 = (await t.call("session.list", {})) as Array<Record<string, unknown>>;
    assert.notStrictEqual(r1.session_id, r2.session_id);
    const ids = r3.map((s) => s.session_id);
    assert.ok(ids.includes(r1.session_id));
    assert.ok(ids.includes(r2.session_id));
    await t.close();
  });

  test("connect to nonexistent socket raises LoomConnectionError", async () => {
    const t = new LoomTransport("/tmp/nonexistent-loom-abc123.sock", daemon.token);
    await assert.rejects(() => t.connect(), LoomConnectionError);
  });
});

// ─── bare daemon error frames (the real HELLO auth-failure wire shape) ────
// On HELLO auth failure the real daemon sends a BARE serialized JsonRpcError
// — {"code": ..., "message": ...} with NO {"error": ...} wrapper and NO id
// (loom-rpc connection_handler::send_error) — then closes the connection.
// The MockDaemon emits that exact shape, so these tests exercise the true
// wire contract.
describe("Transport: bare daemon error frames", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("bare auth-failure frame surfaces code AND message, not a generic close", async () => {
    const t = new LoomTransport(daemon.socketPath, "wrong-token");
    await assert.rejects(
      () => t.connect(),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "protocol_auth_required");
        assert.match((err as LoomRPCError).message, /token mismatch/);
        return true;
      },
    );
    await t.close();
  });

  test("auth failure is latched: calls after a failed connect re-throw the typed error", async () => {
    const t = new LoomTransport(daemon.socketPath, "wrong-token");
    await assert.rejects(() => t.connect(), LoomRPCError);
    // The latched daemon error must surface on calls too — typed, not a
    // generic connection error.
    await assert.rejects(
      () => t.call("session.list", {}),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "protocol_auth_required");
        return true;
      },
    );
    await t.close();
  });

  test("ack handshake: old daemon's method_not_found probe reply authenticates", async () => {
    // The default MockDaemon has no daemon.hello handler, so the probe
    // gets the pre-ack daemon's method_not_found envelope — connect()
    // must treat that as authenticated.
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    const result = await t.call("session.list", {});
    assert.ok(Array.isArray(result));
    await t.close();
  });

  test("ack handshake: new daemon's {hello: ok} ack authenticates", async () => {
    daemon.registerHandler("daemon.hello", () => ({ hello: "ok", server: "test" }));
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    const result = await t.call("session.list", {});
    assert.ok(Array.isArray(result));
    await t.close();
  });

  test("fromBareFrame rejects normal envelopes and non-string code/message", () => {
    assert.ok(
      LoomRPCError.fromBareFrame({ code: "protocol_auth_required", message: "nope" }) instanceof
        LoomRPCError,
    );
    assert.equal(LoomRPCError.fromBareFrame({ id: 1, result: {} }), null);
    assert.equal(
      LoomRPCError.fromBareFrame({ id: 1, error: { code: "x", message: "y" } }),
      null,
    );
    assert.equal(LoomRPCError.fromBareFrame({ error: { code: "x", message: "y" } }), null);
    assert.equal(LoomRPCError.fromBareFrame({ result: null }), null);
    assert.equal(LoomRPCError.fromBareFrame({ code: 401, message: "nope" }), null);
    assert.equal(LoomRPCError.fromBareFrame({ code: "x" }), null);
  });
});

// ─── dead-connection latch ────────────────────────────────────────────────
// The real daemon closes authenticated connections after 300s idle
// (AUTHENTICATED_IDLE_TIMEOUT) — agent workflows routinely exceed that
// between actions. Calls on the dead transport must fail fast with a typed
// connection-closed error, not Node's internal "Cannot call write after a
// stream was destroyed" an event-loop turn later.
describe("Transport: dead-connection latch", () => {
  test("calls after daemon hangup fail with a typed connection-closed error", async () => {
    const daemon = new MockDaemon();
    await daemon.start();
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    assert.ok(Array.isArray(await t.call("session.list", {})));

    // Daemon-side hangup (stop() destroys all open connections). The
    // first call after it either fails fast (close already observed) or
    // is rejected by the close handler — either way it must surface a
    // LoomConnectionError, and afterwards the dead state is latched.
    await daemon.stop();
    await assert.rejects(() => t.call("session.list", {}), LoomConnectionError);

    // The latch is now set: subsequent calls fail fast with the typed
    // dead-transport error, never the misleading stream-destroyed or
    // not-connected messages.
    await assert.rejects(
      () => t.call("session.list", {}),
      (err: unknown) => {
        assert.ok(err instanceof LoomConnectionError);
        assert.match((err as Error).message, /no longer usable/);
        assert.doesNotMatch(
          (err as Error).message,
          /stream was destroyed|not connected/i,
        );
        return true;
      },
    );
    await t.close();
  });

  test("client-side close() latches 'Transport closed', not a daemon hangup", async () => {
    const daemon = new MockDaemon();
    try {
      await daemon.start();
      const t = new LoomTransport(daemon.socketPath, daemon.token);
      await t.connect();
      await t.close();
      // The socket-destroy 'close' event must not re-label a deliberate
      // client close as a daemon hangup.
      await new Promise((r) => setImmediate(r));
      await assert.rejects(
        () => t.call("session.list", {}),
        (err: unknown) => {
          assert.ok(err instanceof LoomConnectionError);
          assert.match((err as Error).message, /Transport closed/);
          return true;
        },
      );
    } finally {
      await daemon.stop();
    }
  });

  test("reconnect after close() clears the latch", async () => {
    const daemon = new MockDaemon();
    try {
      await daemon.start();
      const t = new LoomTransport(daemon.socketPath, daemon.token);
      await t.connect();
      await t.close();
      await assert.rejects(() => t.call("session.list", {}), LoomConnectionError);
      // connect() must reset the latch so the transport is usable again.
      await t.connect();
      assert.ok(Array.isArray(await t.call("session.list", {})));
      await t.close();
    } finally {
      await daemon.stop();
    }
  });
});

// ─── non-serializable params ──────────────────────────────────────────────
// call() must serialize BEFORE registering the pending entry / attaching
// the abort listener: JSON.stringify throws synchronously on circular
// references and BigInt (reachable via user-controlled params such as
// SessionCreateOptions.budget), and an orphaned pending entry fires an
// unhandledRejection when the transport later closes.
describe("Transport: non-serializable params", () => {
  test("rejects typed, leaves no orphaned pending entry or abort listener", async () => {
    const daemon = new MockDaemon();
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    let unhandled: unknown = null;
    const trap = (reason: unknown) => {
      unhandled = reason;
    };
    process.on("unhandledRejection", trap);
    try {
      await daemon.start();
      await t.connect();

      const circular: Record<string, unknown> = {};
      circular["self"] = circular;
      const controller = new AbortController();
      await assert.rejects(
        () => t.call("session.list", circular, { signal: controller.signal }),
        (err: unknown) => {
          assert.ok(err instanceof LoomError, "must be a typed LoomError");
          assert.ok(!(err instanceof LoomRPCError), "must not masquerade as an RPC error");
          assert.match((err as Error).message, /not JSON-serializable/);
          return true;
        },
      );
      await assert.rejects(
        () => t.call("session.list", { big: BigInt(1) }),
        (err: unknown) => err instanceof LoomError,
      );

      // The transport must remain fully usable after the failed serialize.
      assert.ok(Array.isArray(await t.call("session.list", {})));

      // Aborting the signal must be a no-op (no listener was attached for
      // the failed call), and closing — which rejects every pending entry
      // — must find no orphan from it. Pre-fix, either fired an
      // unhandledRejection that kills the process by default.
      controller.abort();
      await t.close();
      await new Promise((r) => setImmediate(r));
      await new Promise((r) => setImmediate(r));
      assert.strictEqual(unhandled, null, `unhandledRejection fired: ${String(unhandled)}`);
    } finally {
      process.removeListener("unhandledRejection", trap);
      await t.close();
      await daemon.stop();
    }
  });
});
