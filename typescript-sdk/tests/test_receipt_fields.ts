/**
 * Receipt wire-field parsing: status / error / navigate tier-2 / evaluate fields.
 *
 * The daemon returns failed actions as SUCCESSFUL JSON-RPC results whose
 * receipt carries `status="error"` plus a typed error payload (loom-rpc
 * host_service_adapter Receipt / ReceiptStatus / ReceiptError) — the SDK must
 * surface those fields so failed actions are distinguishable from successes.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

describe("Receipt: status/error + navigate tier-2 + evaluate fields", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    // Wire-shaped error receipt mirroring host_service_adapter::Receipt
    // (snake_case keys; status enum is success|error|aborted).
    daemon.registerHandler("action.web.navigate", () => ({
      action_id: 7,
      session_id: "01TEST" + "0".repeat(20),
      status: "error",
      timing_ticks: 12,
      side_effects: [],
      error: {
        kind: "http_status",
        detail: { status_code: 404, url: "https://example.com/missing" },
      },
      action_hash: "a".repeat(64),
      outcome_hash: "b".repeat(64),
      emitted_at_ms: 1_730_000_000_000,
      url: "https://example.com/missing",
      final_url: "https://example.com/missing",
      title: "Not Found",
      status_code: 404,
      dom_snapshot_hash: "c".repeat(64),
      screenshot_after_hash: "d".repeat(64),
      settle_until: "settled",
      settle_outcome: "reached",
    }));
    daemon.registerHandler("action.web.evaluate", () => ({
      status: "success",
      error: null, // the daemon serializes error: null on success
      action_hash: "e".repeat(64),
      outcome_hash: "f".repeat(64),
      emitted_at_ms: 1,
      return_value_json: "42",
    }));
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("navigate failure receipt surfaces status, ok and typed error", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.navigate("https://example.com/missing");
    await s.close();

    assert.equal(receipt.status, "error");
    assert.equal(receipt.ok, false);
    assert.ok(receipt.error);
    assert.equal(receipt.error.kind, "http_status");
    assert.deepEqual(receipt.error.detail, {
      status_code: 404,
      url: "https://example.com/missing",
    });
    // Navigate tier-2 fields flow through.
    assert.equal(receipt.url, "https://example.com/missing");
    assert.equal(receipt.finalUrl, "https://example.com/missing");
    assert.equal(receipt.title, "Not Found");
    assert.equal(receipt.statusCode, 404);
    assert.equal(receipt.domSnapshotHash, "c".repeat(64));
    assert.equal(receipt.screenshotAfterHash, "d".repeat(64));
    // Existing fields still parse.
    assert.equal(receipt.actionHash, "a".repeat(64));
    assert.equal(receipt.settleOutcome, "reached");
  });

  test("evaluate success receipt surfaces ok and return_value_json", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.evaluate("6 * 7");
    await s.close();

    assert.equal(receipt.status, "success");
    assert.equal(receipt.ok, true);
    assert.equal(receipt.error, undefined);
    assert.equal(receipt.returnValueJson, "42");
    assert.equal(receipt.returnValueBlobRef, undefined);
  });

  test("legacy receipt without status defaults to success (backward compat)", async () => {
    daemon.registerHandler("action.web.click", () => ({
      action_hash: "a".repeat(64),
      outcome_hash: "b".repeat(64),
      emitted_at_ms: 5,
    }));
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.click("#button");
    await s.close();

    assert.equal(receipt.status, "success");
    assert.equal(receipt.ok, true);
    assert.equal(receipt.error, undefined);
    assert.equal(receipt.url, undefined);
  });
});
