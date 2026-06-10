/**
 * Connected-socket cleanup: Session.create/close and the sessionList/vault*
 * free functions must always destroy their transport when the wrapped RPC
 * fails — an open net.Socket is an active libuv handle that keeps the
 * process alive and leaks an fd per retry.
 *
 * Observation: the MockDaemon counts open server-side connections; a
 * client-side transport.close() (socket destroy) drops the count back to
 * zero.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session, sessionList, vaultGrant, LoomRPCError } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

async function waitForNoOpenConnections(daemon: MockDaemon, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (daemon.openConnectionCount > 0) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(
        `timed out: daemon still sees ${daemon.openConnectionCount} open connection(s) — ` +
          "a transport was leaked",
      );
    }
    await new Promise((r) => setTimeout(r, 10));
  }
}

describe("socket cleanup on RPC failure", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("Session.create failure closes the connected transport", async () => {
    daemon.registerHandler("session.create", () => {
      throw new Error("unknown_profile");
    });
    try {
      await assert.rejects(
        () => Session.create({ socketPath: daemon.socketPath, token: daemon.token }),
        LoomRPCError,
      );
      await waitForNoOpenConnections(daemon);
    } finally {
      // Restore the default handler for the rest of the suite.
      daemon.registerHandler("session.create", () => ({
        session_id: "01TEST" + "1".repeat(20),
        status: "active",
        created_at_ms: Date.now(),
      }));
    }
  });

  test("Session.close failure still closes the transport", async () => {
    daemon.registerHandler("session.close", () => {
      throw new Error("boom");
    });
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    await assert.rejects(() => s.close(), LoomRPCError);
    await waitForNoOpenConnections(daemon);
  });

  test("sessionList failure closes the connected transport", async () => {
    daemon.registerHandler("session.list", () => {
      throw new Error("boom");
    });
    await assert.rejects(
      () => sessionList({ socketPath: daemon.socketPath, token: daemon.token }),
      LoomRPCError,
    );
    await waitForNoOpenConnections(daemon);
  });

  test("vaultGrant failure closes the connected transport", async () => {
    // No vault.grant handler registered → method_not_found error envelope.
    await assert.rejects(
      () =>
        vaultGrant("01TEST", "https://example.com", ["read"], 60, "label", {
          socketPath: daemon.socketPath,
          token: daemon.token,
        }),
      (err: unknown) => {
        assert.ok(err instanceof LoomRPCError);
        assert.strictEqual((err as LoomRPCError).code, "method_not_found");
        return true;
      },
    );
    await waitForNoOpenConnections(daemon);
  });
});
