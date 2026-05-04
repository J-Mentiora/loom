/**
 * Error hierarchy tests.
 */
import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { LoomError, LoomRPCError, LoomConnectionError, LoomTokenError } from "../src/index.js";

describe("Error hierarchy", () => {
  test("LoomError is base Error subclass", () => {
    const e = new LoomError("base");
    assert.ok(e instanceof Error);
    assert.ok(e instanceof LoomError);
  });

  test("LoomRPCError extends LoomError and carries code + data", () => {
    const e = new LoomRPCError("schema_violation", "bad field", { field: "x" });
    assert.ok(e instanceof LoomError);
    assert.ok(e instanceof LoomRPCError);
    assert.strictEqual(e.code, "schema_violation");
    assert.strictEqual(e.message, "schema_violation: bad field");
    assert.deepStrictEqual(e.data, { field: "x" });
  });

  test("LoomRPCError.fromEnvelope parses JSON-RPC error object", () => {
    const envelope = { error: { code: "session_not_found", message: "not found", data: null } };
    const e = LoomRPCError.fromEnvelope(envelope);
    assert.strictEqual(e.code, "session_not_found");
    assert.ok(e.message.includes("not found"));
  });

  test("LoomRPCError.fromEnvelope defaults to internal_error when fields missing", () => {
    const e = LoomRPCError.fromEnvelope({ error: {} });
    assert.strictEqual(e.code, "internal_error");
  });

  test("LoomConnectionError extends LoomError", () => {
    const e = new LoomConnectionError("cannot connect");
    assert.ok(e instanceof LoomError);
    assert.ok(e instanceof LoomConnectionError);
  });

  test("LoomTokenError extends LoomError", () => {
    const e = new LoomTokenError("no token file");
    assert.ok(e instanceof LoomError);
    assert.ok(e instanceof LoomTokenError);
  });
});
