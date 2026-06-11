/**
 * settle-capture (4b): Session.create({ noDeterminism }) forwards the
 * `no_determinism` flag to the session.create RPC. Default sessions omit it.
 *
 * Cross-run determinism v2: Session.create({ clockAnchor }) forwards
 * `clock_anchor` (the fixed epoch-ms that pins the injected browser clock).
 * Default sessions omit it (wall-clock epoch, unchanged wire shape).
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

describe("settle-capture: --no-determinism session opt-out", () => {
  let daemon: MockDaemon;
  let lastCreateParams: Record<string, unknown> = {};
  let counter = 0;

  before(async () => {
    daemon = new MockDaemon();
    daemon.registerHandler("session.create", (params) => {
      lastCreateParams = params;
      counter += 1;
      return {
        session_id: `01TEST${String(counter).padStart(20, "0")}`,
        status: "active",
        created_at_ms: 0,
      };
    });
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("default session create omits no_determinism (determinism ON)", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    await s.close();
    assert.equal(lastCreateParams["no_determinism"], undefined);
  });

  test("noDeterminism: true forwards no_determinism to the RPC", async () => {
    const s = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
      noDeterminism: true,
    });
    await s.close();
    assert.equal(lastCreateParams["no_determinism"], true);
  });

  test("default session create omits clock_anchor (wall-clock epoch)", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    await s.close();
    assert.equal(lastCreateParams["clock_anchor"], undefined);
  });

  test("clockAnchor forwards clock_anchor alongside seed", async () => {
    const s = await Session.create({
      socketPath: daemon.socketPath,
      token: daemon.token,
      seed: 42,
      clockAnchor: 1_700_000_000_000,
    });
    await s.close();
    assert.equal(lastCreateParams["seed"], 42);
    assert.equal(lastCreateParams["clock_anchor"], 1_700_000_000_000);
  });
});
