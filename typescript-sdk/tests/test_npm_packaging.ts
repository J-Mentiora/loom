/**
 * Smoke test for the published TypeScript SDK.
 *
 * After `npm install @mentiora-ai/loom-sdk` on Node >= 20, importing `Session`
 * and calling `await Session.create()` should round-trip through the
 * daemon and return a Session whose `sessionId` matches.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session, LoomRPCError } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

describe("Session.create() returns a Session with matching sessionId", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("Session class is exported from @mentiora-ai/loom-sdk", () => {
    assert.ok(Session, "Session must be exported");
    assert.strictEqual(typeof Session.create, "function", "Session.create must be a static method");
  });

  test("Session.create() returns a Session with non-empty sessionId", async () => {
    const session = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    assert.ok(session.sessionId, "sessionId must be non-empty");
    assert.strictEqual(typeof session.sessionId, "string");
    await session.close();
  });

  test("sessionId matches the daemon's record", async () => {
    const session = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    assert.ok(
      daemon.sessions.has(session.sessionId),
      `sessionId '${session.sessionId}' must be in daemon.sessions`,
    );
    const record = daemon.sessions.get(session.sessionId)!;
    assert.strictEqual(record.session_id, session.sessionId);
    await session.close();
  });

  test("Session.create() populates status from daemon response", async () => {
    const session = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    assert.strictEqual(session.status, "active");
    await session.close();
  });

  test("Session.close() resolves", async () => {
    const session = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    const info = await session.close();
    assert.ok(info, "close() must return SessionInfo");
    assert.strictEqual(typeof info.sessionId, "string");
  });

  test("wrong token raises LoomRPCError with protocol_auth_required", async () => {
    await assert.rejects(
      () =>
        Session.create({
          socketPath: daemon.socketPath,
          token: "wrong-token",
        }),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError, "must be LoomRPCError");
        assert.strictEqual((err as LoomRPCError).code, "protocol_auth_required");
        return true;
      },
    );
  });

  test("await using Session auto-disposes on block exit", async () => {
    let capturedId: string | null = null;
    {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      await using session = await Session.create({
        socketPath: daemon.socketPath,
        token: daemon.token,
      });
      capturedId = session.sessionId;
      assert.ok(capturedId, "sessionId must be set inside block");
    }
    // After block exit, session.close() was called automatically
    // We can verify by attempting another session creation to ensure daemon still works
    const s2 = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    assert.ok(s2.sessionId !== capturedId, "new session gets a different ID");
    await s2.close();
  });
});

// ─── close() idempotency ──────────────────────────────────────────────────
// Disposable resources must tolerate double-dispose: the documented
// `await using` pattern combined with an explicit close() inside the block
// runs close() twice (the second via Symbol.asyncDispose at scope exit).
describe("Session.close() idempotency", () => {
  let daemon: MockDaemon;
  let closeCalls: string[];

  before(async () => {
    daemon = new MockDaemon();
    closeCalls = [];
    daemon.registerHandler("session.close", (params) => {
      const sid = (params["session_id"] as string) ?? "";
      closeCalls.push(sid);
      return { session_id: sid, status: "closed", created_at_ms: 0 };
    });
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("double explicit close() returns the latched SessionInfo, RPC fires once", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const before = closeCalls.length;
    const first = await s.close();
    const second = await s.close();
    assert.deepStrictEqual(second, first);
    assert.strictEqual(closeCalls.length, before + 1, "session.close RPC must fire exactly once");
  });

  test("await using + explicit close() inside the block does not throw at scope exit", async () => {
    let info: { sessionId: string; status: string } | null = null;
    {
      await using session = await Session.create({
        socketPath: daemon.socketPath,
        token: daemon.token,
      });
      // Explicit close inside the block (e.g. to read the SessionInfo);
      // disposal at scope exit must be a no-op, not a thrown
      // LoomConnectionError / SuppressedError.
      info = await session.close();
    }
    assert.ok(info);
    assert.strictEqual(info.status, "closed");
  });

  test("failed close() still latches: second close() resolves with synthetic info", async () => {
    daemon.registerHandler("session.close", () => {
      throw new Error("boom");
    });
    try {
      const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
      await assert.rejects(() => s.close(), LoomRPCError);
      // Double-dispose after a failed close must not throw again.
      const info = await s.close();
      assert.strictEqual(info.sessionId, s.sessionId);
      assert.strictEqual(info.status, "closed");
    } finally {
      daemon.registerHandler("session.close", (params) => {
        const sid = (params["session_id"] as string) ?? "";
        closeCalls.push(sid);
        return { session_id: sid, status: "closed", created_at_ms: 0 };
      });
    }
  });
});
