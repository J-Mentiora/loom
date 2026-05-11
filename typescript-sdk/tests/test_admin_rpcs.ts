/**
 * Tests for the admin RPC wrappers: Session.kill, killSession,
 * daemonHealth + AbortSignal integration.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import {
  Session,
  LoomTransport,
  LoomAbortError,
  killSession,
  daemonHealth,
} from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

describe("Admin RPCs: session.kill, daemon.health, AbortSignal", () => {
  let daemon: MockDaemon;
  const killCalls: string[] = [];
  const healthCalls: Record<string, unknown>[] = [];

  before(async () => {
    daemon = new MockDaemon();
    daemon.registerHandler("session.kill", (params) => {
      const sid = (params["session_id"] as string) ?? "";
      killCalls.push(sid);
      const s = daemon.sessions.get(sid);
      if (s) {
        s["status"] = "killed";
        return { ...s };
      }
      return { session_id: sid, status: "killed", created_at_ms: 0 };
    });
    daemon.registerHandler("daemon.health", (params) => {
      healthCalls.push(params);
      const out: Record<string, unknown> = {
        active_sessions: Array.from(daemon.sessions.values()).filter(
          (s) => s["status"] === "active",
        ).length,
        shim_breaker_states: [],
        otel_exporter: "disabled",
        deep: null,
      };
      if (params["deep"]) {
        out["deep"] = [
          {
            shim_id: "chromium-test-01",
            daemon_restart_count: 2,
            daemon_last_restart_at_ms: 1_730_000_000_000,
            shim_uptime_ms: 12_345,
            shim_requests_served: 7,
            shim_last_request_at_ms: 1_730_000_005_000,
            probe_status: "ok",
          },
        ];
      }
      return out;
    });
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("Session.kill() calls session.kill RPC and SIGKILLs the session", async () => {
    const before = killCalls.length;
    const s = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    await s.kill();
    assert.strictEqual(killCalls.length, before + 1);
    assert.ok(killCalls[before].startsWith("01TEST"));
  });

  test("killSession() free function targets a sessionId without a handle", async () => {
    // Create a session via one connection.
    const s = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    const sid = s.sessionId;
    await s.close();
    // Kill it admin-style via a separate connection.
    const before = killCalls.length;
    await killSession(sid, { socketPath: daemon.socketPath, token: daemon.token });
    assert.strictEqual(killCalls.length, before + 1);
    assert.strictEqual(killCalls[before], sid);
  });

  test("daemonHealth() shallow: returns active_sessions + breaker states + deep:null", async () => {
    const before = healthCalls.length;
    const h = await daemonHealth({
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    assert.ok(typeof h.activeSessions === "number");
    assert.ok(Array.isArray(h.shimBreakerStates));
    assert.strictEqual(h.deep, null);
    assert.strictEqual(healthCalls.length, before + 1);
    assert.strictEqual(healthCalls[before]["deep"], false);
  });

  test("daemonHealth({deep:true}): typed ShimDeepHealth array with probe_status enum", async () => {
    const h = await daemonHealth({
      deep: true,
      socketPath: daemon.socketPath,
      token: daemon.token,
    });
    assert.ok(h.deep);
    assert.strictEqual(h.deep.length, 1);
    const s = h.deep[0];
    assert.strictEqual(s.shimId, "chromium-test-01");
    assert.strictEqual(s.daemonRestartCount, 2);
    assert.strictEqual(s.shimUptimeMs, 12_345);
    assert.strictEqual(s.probeStatus, "ok");
  });

  test("AbortSignal pre-aborted: rejects synchronously with LoomAbortError", async () => {
    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    try {
      const controller = new AbortController();
      controller.abort();
      await assert.rejects(
        () => t.call("session.list", {}, { signal: controller.signal }),
        (err: unknown) => {
          assert.ok(err instanceof LoomAbortError);
          assert.strictEqual((err as Error).name, "AbortError");
          return true;
        },
      );
    } finally {
      await t.close();
    }
  });

  test("AbortSignal mid-flight: aborts and emits request.cancel", async () => {
    const cancelCalls: Record<string, unknown>[] = [];
    daemon.registerHandler("request.cancel", (params) => {
      cancelCalls.push(params);
      return { cancelled: true };
    });
    // Slow handler: resolves after 200ms; abort fires at 30ms so the
    // test sees the cancel-driven reject, not the eventual response.
    // .unref() the timer so a leaked one doesn't keep the test runner alive.
    daemon.registerHandler("test.slow", () => {
      return new Promise((resolve) => {
        const t = setTimeout(() => resolve({ done: true }), 200);
        t.unref();
      });
    });

    const t = new LoomTransport(daemon.socketPath, daemon.token);
    await t.connect();
    try {
      const controller = new AbortController();
      const callPromise = t.call("test.slow", {}, { signal: controller.signal });
      // Abort shortly after dispatch.
      setTimeout(() => controller.abort(), 30);
      await assert.rejects(
        () => callPromise,
        (err: unknown) => {
          assert.ok(err instanceof LoomAbortError);
          return true;
        },
      );
      // Give the transport a tick to flush the cancel.
      await new Promise((r) => setTimeout(r, 50));
      assert.ok(cancelCalls.length >= 1, "request.cancel envelope was not sent");
      assert.ok("request_id" in cancelCalls[0]);
    } finally {
      await t.close();
    }
  });
});
