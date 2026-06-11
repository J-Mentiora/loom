/**
 * Transport-layer tests: framing, HELLO auth, call/response, error handling.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { LoomTransport, LoomRPCError, LoomConnectionError } from "../src/index.js";
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

  test("HELLO auth fails with wrong token → LoomRPCError(protocol_auth_required)", async () => {
    const t = new LoomTransport(daemon.socketPath, "wrong-token");
    await t.connect();
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
    await t.connect();
    await assert.rejects(
      () => t.call("session.list", {}),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "protocol_auth_required");
        assert.match((err as LoomRPCError).message, /token mismatch/);
        return true;
      },
    );
    await t.close();
  });

  test("auth failure is latched: subsequent calls re-throw the typed error", async () => {
    const t = new LoomTransport(daemon.socketPath, "wrong-token");
    await t.connect();
    await assert.rejects(() => t.call("session.list", {}), LoomRPCError);
    // The daemon already closed the connection; the second call must
    // surface the same typed auth error, not a generic connection error.
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
