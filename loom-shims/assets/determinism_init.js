// determinism_init.js — injected via Page.addScriptToEvaluateOnNewDocument
// with runImmediately: true (IC-SHIM-05). Overrides non-deterministic browser
// APIs so session replay is bit-equal.
//
// Three template tokens are substituted by `render_determinism_script`
// before injection. Pre-substitution, the tokens MUST be present; the
// `is_determinism_template` validator gates boot.
//
// AC coverage:
//   AC-DET-04.1: animation-duration: 0 + requestAnimationFrame clamping
//   AC-DET-01.1: Date.now / performance.now frozen to session-fixed epoch
//   AC-DET-02.1: Math.random seeded (sfc32 from per-session seed)

(function () {
  'use strict';

  // --- Virtual clock (session-fixed) ---
  var _epoch_ms = __LOOM_EPOCH_MS__;
  Date.now = function () { return _epoch_ms; };
  Date.prototype.getTime = function () { return _epoch_ms; };
  if (typeof performance !== 'undefined') {
    Object.defineProperty(performance, 'timeOrigin', {
      get: function () { return _epoch_ms; },
      configurable: true,
    });
    performance.now = function () { return 0; };
  }

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

  // --- Animation disabling (AC-DET-04.1) ---
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

  var _raf_tick = 0;
  window.requestAnimationFrame = function (cb) {
    _raf_tick += 16;
    var t = _raf_tick;
    setTimeout(function () { cb(t); }, 0);
    return _raf_tick;
  };
  window.cancelAnimationFrame = function (_id) {};

  if (typeof document !== 'undefined') {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', injectAnimationCss);
    } else {
      injectAnimationCss();
    }
  }
})();
