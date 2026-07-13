/**
 * Audio verbs + retrieval helpers (voice-call-io task 08, AC8/AC11).
 *
 * Verifies the SDK exposes `injectAudio` / `startAudioCapture` /
 * `stopAudioCapture`, surfaces the wire receipt's `audio_after_hash` /
 * `audio_stop_reason` as `audioAfterHash` / `audioStopReason`, warns
 * loudly on cap truncation, and fetches the captured WAV bytes via
 * `content.get` (`fetchAudioCapture` / `saveAudioCapture`).
 */
import { test, describe, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Session, type Receipt } from "../src/index.js";
import { MockDaemon } from "./helpers/mock_daemon.js";

const AUDIO_HASH = "9f".repeat(32);
const WAV_BYTES = Buffer.concat([Buffer.from("RIFF\x24\x00\x00\x00WAVEfmt "), Buffer.alloc(28)]);

/** Decode the JSON action payload the SDK packs into `action.payload` bytes. */
function decodePayload(params: Record<string, unknown>): Record<string, unknown> {
  const action = params["action"] as Record<string, unknown>;
  const bytes = action["payload"] as number[];
  return JSON.parse(Buffer.from(bytes).toString("utf8")) as Record<string, unknown>;
}

function stopReceipt(stopReason: string): Record<string, unknown> {
  return {
    action_hash: "a".repeat(64),
    outcome_hash: "b".repeat(64),
    emitted_at_ms: 1_730_000_000_000,
    audio_after_hash: AUDIO_HASH,
    audio_stop_reason: stopReason,
  };
}

describe("audio verbs + retrieval helpers", () => {
  let daemon: MockDaemon;
  let injectPayload: Record<string, unknown> = {};
  let startPayload: Record<string, unknown> = {};
  let stopReason = "explicit";
  let contentGetRef: string | undefined;
  let warnings: string[] = [];
  const origWarn = console.warn;

  beforeEach(async () => {
    daemon = new MockDaemon();
    daemon.registerHandler("action.web.inject_audio", (params) => {
      injectPayload = decodePayload(params);
      return { action_hash: "a".repeat(64), outcome_hash: "b".repeat(64), emitted_at_ms: 1 };
    });
    daemon.registerHandler("action.web.start_audio_capture", (params) => {
      startPayload = decodePayload(params);
      return { action_hash: "a".repeat(64), outcome_hash: "b".repeat(64), emitted_at_ms: 1 };
    });
    daemon.registerHandler("action.web.stop_audio_capture", () => stopReceipt(stopReason));
    daemon.registerHandler("content.get", (params) => {
      contentGetRef = params["artifact_ref"] as string;
      return {
        artifact_ref: contentGetRef,
        data_hex: WAV_BYTES.toString("hex"),
        size_bytes: WAV_BYTES.length,
      };
    });
    await daemon.start();
    stopReason = "explicit";
    contentGetRef = undefined;
    warnings = [];
    console.warn = (...args: unknown[]) => {
      warnings.push(args.map(String).join(" "));
    };
  });

  afterEach(async () => {
    console.warn = origWarn;
    await daemon.stop();
  });

  test("injectAudio threads blob_ref/await_playout into the payload", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.injectAudio({ blobRef: AUDIO_HASH, awaitPlayout: true });
    await s.close();

    assert.equal(injectPayload["blob_ref"], AUDIO_HASH);
    assert.equal(injectPayload["await_playout"], true);
    assert.equal("audio_b64" in injectPayload, false);
    assert.equal(receipt.ok, true);
  });

  test("startAudioCapture threads caps into the payload", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    await s.startAudioCapture({ maxDurationMs: 30_000, maxBytes: 1_000_000 });
    await s.close();

    assert.equal(startPayload["max_duration_ms"], 30_000);
    assert.equal(startPayload["max_bytes"], 1_000_000);
  });

  test("stopAudioCapture surfaces audioAfterHash + audioStopReason", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.stopAudioCapture();
    await s.close();

    assert.equal(receipt.audioAfterHash, AUDIO_HASH);
    assert.equal(receipt.audioStopReason, "explicit");
    assert.equal(warnings.length, 0, `explicit stop must not warn: ${warnings}`);
  });

  for (const reason of ["byte_cap", "duration_cap"]) {
    test(`stopAudioCapture warns loudly on ${reason} truncation`, async () => {
      stopReason = reason;
      const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
      const receipt = await s.stopAudioCapture();
      await s.close();

      assert.equal(receipt.audioStopReason, reason);
      assert.equal(warnings.length, 1);
      assert.ok(warnings[0].includes(reason), `warning must name the reason: ${warnings[0]}`);
      assert.ok(warnings[0].includes("truncated"), `warning must say truncated: ${warnings[0]}`);
    });
  }

  test("fetchAudioCapture returns the WAV bytes via content.get", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.stopAudioCapture();
    const bytes = await s.fetchAudioCapture(receipt);
    await s.close();

    assert.deepEqual(Buffer.from(bytes), WAV_BYTES);
    assert.equal(contentGetRef, AUDIO_HASH);
  });

  test("saveAudioCapture writes the WAV to disk", async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "loom-audio-"));
    const out = path.join(dir, "answer.wav");
    try {
      const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
      const receipt = await s.stopAudioCapture();
      await s.saveAudioCapture(receipt, out);
      await s.close();

      assert.deepEqual(fs.readFileSync(out), WAV_BYTES);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  test("fetchAudioCapture throws on malformed data_hex instead of zero-filling", async () => {
    daemon.registerHandler("content.get", (params) => ({
      artifact_ref: params["artifact_ref"],
      data_hex: "zz-not-hex",
      size_bytes: 5,
    }));
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const receipt = await s.stopAudioCapture();
    await assert.rejects(
      () => s.fetchAudioCapture(receipt),
      (err: Error) => err.message.includes("malformed data_hex"),
    );
    await s.close();
  });

  test("fetchAudioCapture without audioAfterHash throws naming the field", async () => {
    const s = await Session.create({ socketPath: daemon.socketPath, token: daemon.token });
    const bare = { actionHash: "a".repeat(64), outcomeHash: "b".repeat(64), emittedAtMs: 1 } as Receipt;
    await assert.rejects(
      () => s.fetchAudioCapture(bare),
      (err: Error) => err.message.includes("audioAfterHash"),
    );
    await s.close();
  });
});
