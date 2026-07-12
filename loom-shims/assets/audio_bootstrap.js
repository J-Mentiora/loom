// audio_bootstrap.js — synthetic-microphone override for in-browser voice calls.
//
// voice-call-io task 03 (mic-override HALF only — Architecture §4 sections 1-3).
// Installed via `Page.addScriptToEvaluateOnNewDocument {runImmediately:true}`
// for `--audio` sessions ONLY, so it runs before the page's first
// `getUserMedia`. `__LOOM_AUDIO_NONCE__` is substituted with a fresh per-target
// nonce by `audio_bridge::render_bootstrap_script` before install.
//
// What this half does:
//   1. Mic override — `getUserMedia({audio})` resolves to a WebAudio
//      `MediaStreamAudioDestinationNode` track WE drive. A combined
//      `{audio, video}` request returns OUR synthetic audio track + the page's
//      REAL video track — the synthetic call NEVER delegates the whole request,
//      so the real microphone can never leak (plan-council FND-0022).
//   2. Provenance registry — every injected track joins a module WeakSet and
//      carries a nonce'd marker, so task 05's capture tap can exclude it.
//   3. enqueue/pending — decode caller audio and schedule it into the mic track;
//      `pending()` reports seconds still queued (the `await_playout` substrate).
//
// The public API is exposed as a NON-enumerable, NON-configurable, NON-writable
// `window.__loom_<nonce>` (plan-council FND-0019) — discoverable by design (a
// main-world override always is; AC9 non-echo holds for cooperative pages only),
// but not trivially enumerable.
//
// NOT in this half (task 05 — see the marked seam at the bottom): the capture
// tap (RTCPeerConnection ctor wrap), the ring buffer, and drain.
(function () {
  'use strict';

  var NONCE = '__LOOM_AUDIO_NONCE__';
  var API_KEY = '__loom_' + NONCE;

  // Idempotent: a belt-and-braces in-context re-eval (or a double install) is a
  // no-op once the nonce'd property exists. The exposed global is
  // `window.__loom_<nonce>` (the token is substituted at render).
  if (window.__loom___LOOM_AUDIO_NONCE__) {
    return;
  }

  // ── Section 1/2: WebAudio graph + provenance registry ────────────────────
  var ctx = null;
  var dest = null;
  var nextStart = 0;
  // AC10: an `enqueue` before any `getUserMedia` has no mic to feed → reject.
  var gumSeen = false;
  // Provenance registry (D10): injected tracks the task-05 capture tap excludes.
  var injectedTracks = (typeof WeakSet === 'function') ? new WeakSet() : null;

  function ensureGraph() {
    if (ctx) return;
    var AC = window.AudioContext || window.webkitAudioContext;
    ctx = new AC();
    dest = ctx.createMediaStreamDestination();
    // Gesture-free autoplay is enabled by the --autoplay-policy launch flag.
    try { ctx.resume(); } catch (e) {}
    nextStart = ctx.currentTime;
  }

  // Tag a track as loom-injected: WeakSet membership + a non-enumerable nonce'd
  // marker id. Task 05's capture tap consults BOTH so it never re-captures our
  // own outbound audio (AC9 non-echo).
  function tagInjectedTrack(track) {
    if (!track) return;
    try {
      if (injectedTracks) injectedTracks.add(track);
      Object.defineProperty(track, '__loomInjected', {
        value: NONCE,
        enumerable: false,
        configurable: false,
        writable: false
      });
    } catch (e) {}
  }

  // The synthetic mic: a FRESH MediaStream wrapping the live destination
  // track(s) on each call, so the page can add/stop/clone tracks without
  // reaching into our graph. Honors the page calling `track.stop()`/`ended`
  // (D14) — stopping the destination track simply ends our injection.
  function micStream() {
    ensureGraph();
    var tracks = dest.stream.getAudioTracks();
    for (var i = 0; i < tracks.length; i++) tagInjectedTrack(tracks[i]);
    return new MediaStream(tracks);
  }

  function wantsAudio(c) { return !!(c && c.audio); }
  function wantsVideo(c) { return !!(c && c.video); }

  // ── Section 1: getUserMedia override (modern promise API) ────────────────
  if (navigator.mediaDevices && navigator.mediaDevices.getUserMedia) {
    var origModern = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = function (constraints) {
      try {
        var a = wantsAudio(constraints);
        var v = wantsVideo(constraints);
        if (a && !v) {
          // Audio-only voice call → pure synthetic mic.
          gumSeen = true;
          return Promise.resolve(micStream());
        }
        if (a && v) {
          // FND-0022 real-mic-leak guard: request ONLY video from the real
          // device, then compose OUR synthetic audio track with the page's
          // real video track. Never delegate the whole call.
          gumSeen = true;
          return origModern({ video: constraints.video }).then(function (real) {
            var out = new MediaStream();
            ensureGraph();
            var at = dest.stream.getAudioTracks();
            for (var i = 0; i < at.length; i++) {
              tagInjectedTrack(at[i]);
              out.addTrack(at[i]);
            }
            var vt = real.getVideoTracks();
            for (var j = 0; j < vt.length; j++) out.addTrack(vt[j]);
            return out;
          });
        }
        // Pure-video (or neither) → delegate unchanged.
      } catch (e) {}
      return origModern(constraints);
    };
  }

  // Legacy callback API for older WebRTC libraries.
  var legacyGum =
    navigator.getUserMedia ||
    navigator.webkitGetUserMedia ||
    navigator.mozGetUserMedia;
  navigator.getUserMedia = function (constraints, ok, err) {
    try {
      var a = wantsAudio(constraints);
      var v = wantsVideo(constraints);
      if (a && !v) {
        gumSeen = true;
        ok(micStream());
        return;
      }
      if (a && v && legacyGum) {
        // Same leak guard on the legacy path: real video only, synthetic audio.
        gumSeen = true;
        legacyGum.call(navigator, { video: constraints.video }, function (real) {
          try {
            var out = new MediaStream();
            ensureGraph();
            var at = dest.stream.getAudioTracks();
            for (var i = 0; i < at.length; i++) {
              tagInjectedTrack(at[i]);
              out.addTrack(at[i]);
            }
            var vt = real.getVideoTracks();
            for (var j = 0; j < vt.length; j++) out.addTrack(vt[j]);
            ok(out);
          } catch (e) { if (err) err(e); }
        }, err);
        return;
      }
    } catch (e) {}
    if (legacyGum) legacyGum.call(navigator, constraints, ok, err);
    else if (err) err(new Error('getUserMedia unavailable'));
  };

  // ── Section 3: enqueue + pending ─────────────────────────────────────────
  // Decode base64 audio and schedule it into the mic track. Clips play
  // back-to-back (scheduled sequentially). Resolves on `start()`, or on `ended`
  // when `awaitPlayout` (the D11 await-playout substrate). Rejects with
  // `no_microphone_request` if the page has not called getUserMedia yet (AC10).
  function enqueue(b64, awaitPlayout) {
    return new Promise(function (resolve, reject) {
      if (!gumSeen) {
        reject(new Error('no_microphone_request'));
        return;
      }
      ensureGraph();
      var bytes;
      try {
        var bin = atob(b64);
        bytes = new Uint8Array(bin.length);
        for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      } catch (e) {
        reject(e);
        return;
      }
      ctx.decodeAudioData(
        bytes.buffer,
        function (buf) {
          var src = ctx.createBufferSource();
          src.buffer = buf;
          src.connect(dest);
          var t = Math.max(ctx.currentTime, nextStart);
          src.start(t);
          nextStart = t + buf.duration;
          if (awaitPlayout) {
            src.onended = function () { resolve(buf.duration); };
          } else {
            resolve(buf.duration);
          }
        },
        function (e) { reject(e || new Error('audio_decode_failed')); }
      );
    });
  }

  // Seconds of audio still queued ahead of playout (0 when idle) — lets a driver
  // wait for an utterance to finish before listening for the reply (D11).
  function pending() {
    if (!ctx) return 0;
    return Math.max(0, nextStart - ctx.currentTime);
  }

  // Whether `track` was injected by loom (task 05 capture-tap exclusion helper).
  function isInjectedTrack(track) {
    try {
      if (injectedTracks && injectedTracks.has(track)) return true;
      return !!(track && track.__loomInjected === NONCE);
    } catch (e) {
      return false;
    }
  }

  // ── task 05: capture tap (RTCPeerConnection ctor wrap) + drain go here. ───
  // The capture tap wraps `RTCPeerConnection`, attaches its OWN 'track' listener
  // to each instance, and — for every receiver track NOT in `injectedTracks` /
  // not carrying the nonce marker — pipes it through a ring buffer that `drain`
  // reads. Do NOT implement it in task 03.

  var api = {
    nonce: NONCE,
    enqueue: enqueue,
    pending: pending,
    isInjectedTrack: isInjectedTrack,
    ready: true
  };

  // FND-0019: expose the API non-enumerable / non-configurable / non-writable.
  Object.defineProperty(window, API_KEY, {
    value: api,
    enumerable: false,
    configurable: false,
    writable: false
  });
})();
