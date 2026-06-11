/**
 * Session.schemas(): rpc.schemas mapping, including the defensive path for
 * a malformed response that omits `methods` (version-skewed daemon) —
 * which must surface a typed LoomError, not a raw TypeError.
 */
import { test, describe, before, after } from "node:test";
import assert from "node:assert/strict";
import { Session, LoomError } from "../src/index.js";
import { MockDaemon, SCHEMA_REGISTRY } from "./helpers/mock_daemon.js";

describe("Session.schemas()", () => {
  let daemon: MockDaemon;

  before(async () => {
    daemon = new MockDaemon();
    await daemon.start();
  });

  after(async () => {
    await daemon.stop();
  });

  test("maps methods + sourceWitSha256 from the registry", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    try {
      const reg = await s.schemas();
      assert.strictEqual(reg.methods.length, SCHEMA_REGISTRY.methods.length);
      assert.strictEqual(reg.methods[0].method, "session.create");
      assert.strictEqual(reg.sourceWitSha256, SCHEMA_REGISTRY.source_wit_sha256);
    } finally {
      await s.close();
    }
  });

  test("response without 'methods' rejects with typed LoomError, not TypeError", async () => {
    daemon.registerHandler("rpc.schemas", () => ({ source_wit_sha256: "ab".repeat(32) }));
    try {
      const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
      try {
        await assert.rejects(
          () => s.schemas(),
          (err: unknown) => {
            assert.ok(err instanceof LoomError, "must be a typed LoomError");
            assert.ok(!(err instanceof TypeError));
            assert.match((err as Error).message, /missing 'methods'/);
            return true;
          },
        );
      } finally {
        await s.close();
      }
    } finally {
      daemon.registerHandler("rpc.schemas", () => SCHEMA_REGISTRY);
    }
  });

  test("non-array 'methods' is also rejected with the typed error", async () => {
    daemon.registerHandler("rpc.schemas", () => ({ methods: "nope" }));
    try {
      const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
      try {
        await assert.rejects(() => s.schemas(), LoomError);
      } finally {
        await s.close();
      }
    } finally {
      daemon.registerHandler("rpc.schemas", () => SCHEMA_REGISTRY);
    }
  });
});
