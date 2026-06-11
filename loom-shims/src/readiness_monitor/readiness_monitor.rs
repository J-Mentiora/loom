// ReadinessMonitor — deterministic, readiness-gated settle detection.
//
// # Why this exists (settle-capture)
// loom used to capture DOM + screenshot at navigation commit/load time, which
// yields blank SPA shells and arbitrary animation frames. This module decides
// when a page has reached a requested *readiness state* (`load`, `networkidle`,
// or `settled`) so the capture is gated on real readiness instead.
//
// # The single load-bearing correctness property (DET-CORE)
// The settle VERDICT is a pure function of the recorded sequence of browser
// observations counted in **discrete virtual ticks** — NEVER wall-clock time.
// A "tick" is one iteration of a fixed-cadence poll over the drained CDP-event
// state (the I/O layer may `await` between polls to avoid busy-spin, but the
// verdict reads only tick/observation COUNTS). Because the CDP event ordering
// is recorded on the replay tape, a verdict that depends only on that discrete
// sequence reproduces identically on replay. `settle_ms` (a wall/virtual-time
// derived diagnostic) is computed host-side and is EXCLUDED from `outcome_hash`,
// exactly as the screenshot content hash already is.
//
// The pure state machine ([`ReadinessMachine`]) is deliberately free of any CDP
// / tokio dependency so it is exhaustively unit-testable from a scripted
// observation feed with no browser and no clock (see `interface_tests.rs`).

use serde::{Deserialize, Serialize};

/// Readiness state a caller can wait for. Wire/string form is snake_case
/// (`load` | `networkidle` | `settled`); `settled` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettleMode {
    /// The `Page.loadEventFired` event has fired.
    Load,
    /// `load` + no more than `idle_threshold` in-flight requests held quiet for
    /// `quiet_ticks` consecutive ticks (WebSocket/EventSource excluded).
    NetworkIdle,
    /// `networkidle` + `document.readyState == complete` + a stable final URL
    /// (after client-side redirects) + a DOM with no mutations for
    /// `quiet_ticks` consecutive ticks. The default.
    #[default]
    Settled,
}

impl SettleMode {
    /// Parse the wire string. Unknown values map to `None` (callers treat that
    /// as "use the default"); the RPC layer already rejects invalid `until`
    /// values, so this is a defensive fallback, not the validation boundary.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "load" => Some(Self::Load),
            "networkidle" => Some(Self::NetworkIdle),
            "settled" => Some(Self::Settled),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::NetworkIdle => "networkidle",
            Self::Settled => "settled",
        }
    }
}

/// Why a readiness wait ended. The *target* (what was requested) is carried
/// separately as [`SettleMode`] so the receipt never conflates "what I waited
/// for" with "how the wait ended" (council R-a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettleOutcome {
    /// The requested readiness state was satisfied.
    Reached,
    /// The bound (`tick_ceiling`) was hit while the network/load condition was
    /// never quiet — e.g. a persistent connection or perpetual polling.
    Timeout,
    /// The bound was hit while the DOM kept mutating (network was quiet and the
    /// document was complete) — e.g. a perpetually-animating or re-rendering
    /// page. Distinct from `Timeout` so callers can tell the two apart.
    DomUnstable,
}

impl SettleOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reached => "reached",
            Self::Timeout => "timeout",
            Self::DomUnstable => "dom_unstable",
        }
    }
}

/// Tunables for the readiness machine. Per council R-b/B4 these are hardcoded
/// defaults (no env-var "configuration theater"); only the overall bound is
/// caller-tunable via `timeout_ms`, which the I/O layer maps to `tick_ceiling`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleConfig {
    /// Max in-flight requests still considered "idle" (Puppeteer networkidle2
    /// semantics — tolerates analytics/keepalive trickle).
    pub idle_threshold: u32,
    /// Consecutive quiet ticks required before a quiet condition counts as met.
    pub quiet_ticks: u32,
    /// Hard bound: once `tick` reaches this, the wait returns a typed outcome
    /// (`Timeout`/`DomUnstable`) rather than hanging.
    pub tick_ceiling: u32,
}

impl SettleConfig {
    pub const DEFAULT_IDLE_THRESHOLD: u32 = 2;
    pub const DEFAULT_QUIET_TICKS: u32 = 5;
    pub const DEFAULT_TICK_CEILING: u32 = 300;

    pub fn with_ceiling(tick_ceiling: u32) -> Self {
        Self {
            idle_threshold: Self::DEFAULT_IDLE_THRESHOLD,
            quiet_ticks: Self::DEFAULT_QUIET_TICKS,
            // A pathological caller timeout of 0 would never allow a reach;
            // clamp to at least one tick so a clean page can still settle.
            tick_ceiling: tick_ceiling.max(1),
        }
    }
}

impl Default for SettleConfig {
    fn default() -> Self {
        Self::with_ceiling(Self::DEFAULT_TICK_CEILING)
    }
}

/// The page state observed at one tick, drained from CDP events by the I/O
/// layer. Every field is a discrete, replay-stable quantity — there is no
/// elapsed-time input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageObservation {
    /// `Page.loadEventFired` has fired.
    pub load_fired: bool,
    /// In-flight request count, EXCLUDING WebSocket/EventSource (those are
    /// long-lived by design and would never idle).
    pub in_flight: u32,
    /// `document.readyState == "complete"`.
    pub ready_complete: bool,
    /// The final URL has been stable (no further `frameNavigated`) since the
    /// last observation — i.e. client-side redirects have quiesced.
    pub url_stable: bool,
    /// Count of meaningful DOM mutations reported in the interval ending at this
    /// tick. Zero means the DOM was quiet for this tick.
    pub dom_mutations: u32,
}

/// Pure readiness state machine. Fed one [`PageObservation`] per tick; returns
/// `Some(outcome)` the first tick a terminal verdict is reached, `None` while
/// still waiting. Holds NO clock and NO I/O — the tick count is the only notion
/// of time, which is what makes the verdict replay-equal.
#[derive(Debug, Clone)]
pub struct ReadinessMachine {
    mode: SettleMode,
    cfg: SettleConfig,
    tick: u32,
    /// Consecutive ticks the network has been idle (in_flight <= threshold,
    /// after load). Reset to 0 by a late request — this is the networkidle
    /// quiet-window reset.
    net_quiet_run: u32,
    /// Consecutive ticks the DOM has been quiet (zero mutations). Reset to 0 by
    /// any mutation.
    dom_quiet_run: u32,
}

impl ReadinessMachine {
    pub fn new(mode: SettleMode, cfg: SettleConfig) -> Self {
        Self {
            mode,
            cfg,
            tick: 0,
            net_quiet_run: 0,
            dom_quiet_run: 0,
        }
    }

    pub fn mode(&self) -> SettleMode {
        self.mode
    }

    /// Current tick count (number of observations fed). The I/O layer maps this
    /// to `settle_ms` host-side; it never enters `outcome_hash`.
    pub fn ticks(&self) -> u32 {
        self.tick
    }

    /// Advance one tick with the latest observation. Returns the terminal
    /// outcome the first tick it is decided, else `None`.
    pub fn step(&mut self, obs: PageObservation) -> Option<SettleOutcome> {
        self.tick += 1;

        // Quiet-run bookkeeping. Network idleness only "counts" after load has
        // fired (pre-load the page is definitionally busy). A late request
        // (in_flight > threshold) resets the run — the networkidle reset.
        if obs.load_fired && obs.in_flight <= self.cfg.idle_threshold {
            self.net_quiet_run += 1;
        } else {
            self.net_quiet_run = 0;
        }
        if obs.dom_mutations == 0 {
            self.dom_quiet_run += 1;
        } else {
            self.dom_quiet_run = 0;
        }

        let net_idle = obs.load_fired && self.net_quiet_run >= self.cfg.quiet_ticks;
        let dom_quiet = self.dom_quiet_run >= self.cfg.quiet_ticks;

        let reached = match self.mode {
            SettleMode::Load => obs.load_fired,
            SettleMode::NetworkIdle => net_idle,
            SettleMode::Settled => net_idle && obs.ready_complete && obs.url_stable && dom_quiet,
        };
        if reached {
            return Some(SettleOutcome::Reached);
        }

        if self.tick >= self.cfg.tick_ceiling {
            return Some(self.timeout_verdict(obs));
        }

        None
    }

    /// Terminal classification when a BOUND ends the wait — the tick ceiling
    /// here, or the I/O layer's wall-clock guard (see `wait_for_settle`).
    /// Distinguishes a DOM-churn timeout from a network/load timeout so the
    /// receipt's `settle_outcome` is precise: `DomUnstable` applies only in
    /// `Settled` mode, when everything EXCEPT the DOM had quiesced. Pure —
    /// reads the quiet-run state as of the last `step`, never advances it.
    pub fn timeout_verdict(&self, obs: PageObservation) -> SettleOutcome {
        let net_idle = obs.load_fired && self.net_quiet_run >= self.cfg.quiet_ticks;
        let dom_quiet = self.dom_quiet_run >= self.cfg.quiet_ticks;
        if self.mode == SettleMode::Settled
            && net_idle
            && obs.ready_complete
            && obs.url_stable
            && !dom_quiet
        {
            return SettleOutcome::DomUnstable;
        }
        SettleOutcome::Timeout
    }
}

#[cfg(test)]
#[path = "interface_tests.rs"]
mod interface_tests;
