/**
 * Drift guard for the hand-maintained LoomErrorCode wire union.
 *
 * The daemon serializes error codes via `LoomErrorCode::as_wire`
 * (loom-shared/src/error_format.rs). `LOOM_ERROR_CODES` (the runtime array
 * the `LoomErrorCode` type is derived from) must carry the full wire set so
 * users can narrow `err.code` without undocumented string literals — the
 * audit found the union stale at 14 of ~54 codes.
 *
 * Mirrors python-sdk/tests/test_types_drift.py: the Rust source is parsed
 * by regex (no compilation), and the test skips when the Rust tree is not
 * co-located (running from a packed tarball).
 */
import { test, describe } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { LOOM_ERROR_CODES } from "../src/index.js";

const RS_PATH = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "loom-shared",
  "src",
  "error_format.rs",
);

// Codes introduced by in-flight daemon branches: carried in the SDK ahead
// of the Rust enum, tolerated as SDK-only until they land. Remove entries
// here once they appear in `as_wire`.
const IN_FLIGHT_CODES = new Set(["session_cap_exceeded", "not_replayable"]);

/** Regex-extract the wire strings from the `as_wire` match arms. */
function extractAsWireCodes(src: string): Set<string> {
  const start = src.indexOf("pub fn as_wire");
  const end = src.indexOf("pub fn from_wire");
  assert.ok(
    start >= 0 && end > start,
    "failed to locate as_wire/from_wire in error_format.rs — did the enum move?",
  );
  const codes = new Set<string>();
  for (const m of src.slice(start, end).matchAll(/=>\s*"([a-z0-9_]+)"/g)) {
    codes.add(m[1]);
  }
  return codes;
}

describe("LoomErrorCode wire-set drift guard", () => {
  test("LOOM_ERROR_CODES == Rust as_wire set (modulo in-flight codes)", (t) => {
    if (!fs.existsSync(RS_PATH)) {
      t.skip("Rust source not co-located (running from a packed tarball?)");
      return;
    }
    const rust = extractAsWireCodes(fs.readFileSync(RS_PATH, "utf8"));
    assert.ok(rust.size > 0, "extracted zero as_wire arms from error_format.rs");

    const ts = new Set<string>(LOOM_ERROR_CODES);
    const rustOnly = [...rust].filter((c) => !ts.has(c)).sort();
    const tsOnly = [...ts].filter((c) => !rust.has(c) && !IN_FLIGHT_CODES.has(c)).sort();
    assert.deepStrictEqual(
      rustOnly,
      [],
      `daemon wire codes missing from LOOM_ERROR_CODES: ${rustOnly.join(", ")}`,
    );
    assert.deepStrictEqual(
      tsOnly,
      [],
      "LOOM_ERROR_CODES entries absent from Rust as_wire and not allow-listed " +
        `as in-flight: ${tsOnly.join(", ")}`,
    );
  });

  test("LOOM_ERROR_CODES has no duplicates", () => {
    assert.strictEqual(
      new Set(LOOM_ERROR_CODES).size,
      LOOM_ERROR_CODES.length,
      "duplicate entries in LOOM_ERROR_CODES",
    );
  });
});
