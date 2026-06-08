/**
 * settle-capture: Session.navigate readiness options + settle receipt fields.
 *
 * Verifies the SDK threads `until` / `timeoutMs` into the `web.navigate`
 * action payload, and surfaces the wire receipt's `settle_until` /
 * `settle_outcome` onto `Receipt.settleUntil` / `Receipt.settleOutcome`.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

/** Decode the JSON action payload the SDK packs into `action.payload` bytes. */
function decodePayload(params: Record<string, unknown>): Record<string, unknown> {
  const action = params["action"] as Record<string, unknown>;
  const bytes = action["payload"] as number[];
  return JSON.parse(Buffer.from(bytes).toString("utf8")) as Record<string, unknown>;
}

describe("settle-capture: navigate readiness options + receipt fields", () => {
  let daemon: MockDaemon;
  let lastNavigatePayload: Record<string, unknown> = {};

  before(async () => {
    daemon = new MockDaemon();
    daemon.registerHandler("action.web.navigate", (params) => {
      lastNavigatePayload = decodePayload(params);
      // Echo a synthetic navigate receipt. The settle_outcome reflects the
      // requested readiness mode so the test can assert end-to-end surfacing.
      const until = (lastNavigatePayload["until"] as string) ?? "settled";
      return {
        action_hash: "a".repeat(64),
        outcome_hash: "b".repeat(64),
        emitted_at_ms: 1_730_000_000_000,
        settle_until: until,
        settle_outcome: "reached",
      };
    });
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("default navigate omits until/timeout_ms and surfaces settle receipt fields", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.navigate("https://example.com");
    await s.close();

    // The SDK does not inject a default `until`; the daemon applies `settled`.
    assert.equal(lastNavigatePayload["until"], undefined);
    assert.equal(lastNavigatePayload["timeout_ms"], undefined);
    assert.equal(lastNavigatePayload["url"], "https://example.com");

    // Settle fields flow from the wire receipt onto the typed Receipt.
    assert.equal(receipt.settleUntil, "settled");
    assert.equal(receipt.settleOutcome, "reached");
  });

  test("navigate threads until + timeoutMs into the action payload", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.navigate("https://spa.example.com", {
      until: "networkidle",
      timeoutMs: 1234,
    });
    await s.close();

    assert.equal(lastNavigatePayload["until"], "networkidle");
    assert.equal(lastNavigatePayload["timeout_ms"], 1234);
    // The receipt echoes the requested readiness mode.
    assert.equal(receipt.settleUntil, "networkidle");
    assert.equal(receipt.settleOutcome, "reached");
  });
});

describe("settle-capture: web.wait_for standalone readiness verb", () => {
  let daemon: MockDaemon;
  let lastWaitPayload: Record<string, unknown> = {};

  before(async () => {
    daemon = new MockDaemon();
    daemon.registerHandler("action.web.wait_for", (params) => {
      lastWaitPayload = decodePayload(params);
      const until = (lastWaitPayload["until"] as string) ?? "settled";
      return {
        action_hash: "c".repeat(64),
        outcome_hash: "d".repeat(64),
        emitted_at_ms: 1_730_000_000_000,
        settle_until: until,
        settle_outcome: "reached",
      };
    });
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("waitFor() defaults omit options and surface the settle verdict", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.waitFor();
    await s.close();

    assert.equal(lastWaitPayload["until"], undefined);
    assert.equal(lastWaitPayload["timeout_ms"], undefined);
    assert.equal(receipt.settleUntil, "settled");
    assert.equal(receipt.settleOutcome, "reached");
  });

  test("waitFor() threads until + timeoutMs into the action payload", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.waitFor({ until: "networkidle", timeoutMs: 2500 });
    await s.close();

    assert.equal(lastWaitPayload["until"], "networkidle");
    assert.equal(lastWaitPayload["timeout_ms"], 2500);
    assert.equal(receipt.settleUntil, "networkidle");
    assert.equal(receipt.settleOutcome, "reached");
  });
});
