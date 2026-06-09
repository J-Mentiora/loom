// determinism_init.js — injected via Page.addScriptToEvaluateOnNewDocument
// with runImmediately: true. Makes session replay bit-equal.
//
// Clock determinism is provided by CDP `Emulation.setVirtualTimePolicy`
// (faithful-entrance-animations): a single coherent, deterministic,
// epoch-pinned virtual timeline drives `Date.now`, `performance.now`,
// `requestAnimationFrame`, and `setTimeout`/`setInterval`. Because that
// timeline advances (instead of being frozen), client-side entrance
// animations actually run to their final frame and screenshots capture the
// real rendered page — while still replaying bit-equal (virtual time is a
// pure function of the task graph + budget + seed). So this script NO LONGER
// overrides those clocks; it installs only what virtual time does not give:
//
//   1. Seeded RNG (`Math.random` derived from session seed).
//   2. CSS animation/transition flattening — CSS-declared animations resolve
//      instantly to their end state (their *final* frame is what a capture
//      wants), which also keeps them off the compositor's non-deterministic
//      wall-clock path. JS-driven (framer-motion / WAAPI) animations are NOT
//      affected here — they run on the virtual timeline and are awaited by the
//      readiness gate's animation-quiescence signal.
//
// Two template tokens (`__LOOM_SEED_LO__`, `__LOOM_SEED_HI__`) are substituted
// by `render_determinism_script` before injection. Pre-substitution the tokens
// MUST be present; the `is_determinism_template` validator gates boot.

(function () {
  'use strict';

  // --- sfc32 seeded RNG (PCG family). V8-safe via |0 / >>> 0 / Math.imul.
  // State: [a, b, c, d] = [0xa3c59ac3, 0x6c93b87b, seed_lo, seed_hi].
  // Followed by 12 warmup iterations (canonical sfc32 protocol) so seed
  // bits diffuse before the first observable output.
  var _a = 0xa3c59ac3 | 0;
  var _b = 0x6c93b87b | 0;
  var _c = (__LOOM_SEED_LO__) | 0;
  var _d = (__LOOM_SEED_HI__) | 0;

  function _sfc32() {
    var t = (_a + _b | 0) + _d | 0;
    _d = _d + 1 | 0;
    _a = _b ^ (_b >>> 9);
    _b = (_c + ((_c << 3) | 0)) | 0;
    _c = ((_c << 21) | (_c >>> 11));
    _c = (_c + t) | 0;
    return (t >>> 0) / 4294967296;
  }
  for (var _i = 0; _i < 12; _i++) { _sfc32(); }
  Math.random = _sfc32;

  // --- CSS animation/transition flattening ---
  // Force CSS-declared animations/transitions to resolve instantly to their
  // end state. The virtual-time clock drives JS animations to completion; CSS
  // keyframe/transition timing is collapsed here so a capture never lands on a
  // mid-transition frame and never depends on the compositor's wall clock.
  function injectAnimationCss() {
    if (typeof document === 'undefined' || !document.head) { return; }
    var style = document.createElement('style');
    style.textContent =
      '*, *::before, *::after {' +
      '  animation-duration: 0s !important;' +
      '  animation-delay: 0s !important;' +
      '  transition-duration: 0s !important;' +
      '  transition-delay: 0s !important;' +
      '}';
    document.head.appendChild(style);
  }

  if (typeof document !== 'undefined') {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', injectAnimationCss);
    } else {
      injectAnimationCss();
    }
  }
})();
