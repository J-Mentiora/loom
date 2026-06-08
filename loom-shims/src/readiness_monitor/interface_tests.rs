//! Unit tests for the pure readiness state machine (settle-capture T-unit).
//! Scripted observation feed — NO browser, NO wall-clock. These prove every
//! control-flow branch + every `SettleOutcome`, per the test mandate.

use super::*;

/// quiet_ticks=3, ceiling=50, threshold=2 — small numbers keep the scripts readable.
fn cfg() -> SettleConfig {
    SettleConfig {
        idle_threshold: 2,
        quiet_ticks: 3,
        tick_ceiling: 50,
    }
}

/// Feed a fixed observation `n` times, returning the first terminal outcome (if any).
fn feed(m: &mut ReadinessMachine, obs: PageObservation, n: u32) -> Option<SettleOutcome> {
    let mut out = None;
    for _ in 0..n {
        if let Some(o) = m.step(obs) {
            out = Some(o);
            break;
        }
    }
    out
}

const LOADED_IDLE: PageObservation = PageObservation {
    load_fired: true,
    in_flight: 0,
    ready_complete: true,
    url_stable: true,
    dom_mutations: 0,
};

// ---- SettleMode parsing / defaults ----

#[test]
fn settle_mode_parse_roundtrip_and_default() {
    assert_eq!(SettleMode::parse("load"), Some(SettleMode::Load));
    assert_eq!(
        SettleMode::parse("networkidle"),
        Some(SettleMode::NetworkIdle)
    );
    assert_eq!(SettleMode::parse("settled"), Some(SettleMode::Settled));
    assert_eq!(SettleMode::parse("garbage"), None);
    assert_eq!(SettleMode::default(), SettleMode::Settled);
    assert_eq!(SettleMode::Settled.as_str(), "settled");
}

// ---- Load ----

#[test]
fn load_reached_when_load_event_fires() {
    let mut m = ReadinessMachine::new(SettleMode::Load, cfg());
    // Not loaded yet → no verdict.
    assert_eq!(m.step(PageObservation::default()), None);
    // Load fires → reached immediately (load doesn't care about net/dom quiet).
    let obs = PageObservation {
        load_fired: true,
        in_flight: 9,
        ..Default::default()
    };
    assert_eq!(m.step(obs), Some(SettleOutcome::Reached));
}

// ---- NetworkIdle: quiet-window + the reset-on-late-request branch (U3) ----

#[test]
fn networkidle_requires_quiet_window_then_reaches() {
    let mut m = ReadinessMachine::new(SettleMode::NetworkIdle, cfg());
    // Busy for a few ticks (in_flight over threshold) — never idle.
    let busy = PageObservation {
        load_fired: true,
        in_flight: 5,
        ..Default::default()
    };
    assert_eq!(feed(&mut m, busy, 4), None);
    // Now idle for quiet_ticks(3) consecutive ticks → reached on the 3rd.
    let idle = PageObservation {
        load_fired: true,
        in_flight: 1,
        ..Default::default()
    };
    assert_eq!(m.step(idle), None); // run=1
    assert_eq!(m.step(idle), None); // run=2
    assert_eq!(m.step(idle), Some(SettleOutcome::Reached)); // run=3
}

#[test]
fn networkidle_threshold_is_two_inclusive() {
    let mut m = ReadinessMachine::new(SettleMode::NetworkIdle, cfg());
    // Exactly 2 in-flight counts as idle (networkidle2).
    let two = PageObservation {
        load_fired: true,
        in_flight: 2,
        ..Default::default()
    };
    assert_eq!(feed(&mut m, two, 3), Some(SettleOutcome::Reached));
}

#[test]
fn networkidle_quiet_window_resets_on_late_request() {
    // The key reset branch: idle accumulates, a late request fires, the run
    // resets, and the machine must wait a fresh full quiet window.
    let mut m = ReadinessMachine::new(SettleMode::NetworkIdle, cfg());
    let idle = PageObservation {
        load_fired: true,
        in_flight: 0,
        ..Default::default()
    };
    let spike = PageObservation {
        load_fired: true,
        in_flight: 7,
        ..Default::default()
    };
    assert_eq!(m.step(idle), None); // run=1
    assert_eq!(m.step(idle), None); // run=2  (one more idle tick would reach)
    assert_eq!(m.step(spike), None); // late request → run resets to 0
    assert_eq!(m.step(idle), None); // run=1
    assert_eq!(m.step(idle), None); // run=2
    assert_eq!(m.step(idle), Some(SettleOutcome::Reached)); // run=3 → reached
}

#[test]
fn networkidle_not_counted_before_load() {
    // In-flight idle pre-load must NOT accumulate (page is busy by definition).
    let mut m = ReadinessMachine::new(SettleMode::NetworkIdle, cfg());
    let preload_idle = PageObservation {
        load_fired: false,
        in_flight: 0,
        ..Default::default()
    };
    assert_eq!(feed(&mut m, preload_idle, 10), None);
}

// ---- Settled: composite (readyState + url-stable + networkidle + dom-quiet) ----

#[test]
fn settled_requires_all_conditions() {
    let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
    // Network idle + complete but URL still changing (client redirects) → not settled.
    let redirecting = PageObservation {
        load_fired: true,
        in_flight: 0,
        ready_complete: true,
        url_stable: false,
        dom_mutations: 0,
    };
    assert_eq!(feed(&mut m, redirecting, 10), None);
    // URL stabilises and everything quiet → reached after the quiet window.
    assert_eq!(feed(&mut m, LOADED_IDLE, 3), Some(SettleOutcome::Reached));
}

#[test]
fn settled_waits_for_readystate_complete() {
    let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
    let incomplete = PageObservation {
        ready_complete: false,
        ..LOADED_IDLE
    };
    assert_eq!(feed(&mut m, incomplete, 10), None);
}

#[test]
fn settled_url_stable_across_n_client_redirects() {
    // Simulate N client-side redirects: url_stable=false for the redirect ticks,
    // then stable. settled must only fire after the final URL holds.
    for n in [0u32, 1, 3] {
        let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
        let redirecting = PageObservation {
            url_stable: false,
            ..LOADED_IDLE
        };
        assert_eq!(
            feed(&mut m, redirecting, n),
            None,
            "n={n} redirects pending"
        );
        // Final URL holds → reaches after quiet_ticks.
        assert_eq!(
            feed(&mut m, LOADED_IDLE, 3),
            Some(SettleOutcome::Reached),
            "n={n} should settle once URL is stable"
        );
    }
}

#[test]
fn settled_waits_for_dom_quiescence() {
    // Network-idle + complete + url-stable but DOM keeps mutating a few ticks,
    // then quiets — must wait for the DOM quiet window (the async-render shape).
    let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
    let mutating = PageObservation {
        dom_mutations: 4,
        ..LOADED_IDLE
    };
    assert_eq!(feed(&mut m, mutating, 6), None);
    assert_eq!(feed(&mut m, LOADED_IDLE, 3), Some(SettleOutcome::Reached));
}

// ---- Bounded fallbacks: every terminal outcome provoked ----

#[test]
fn networkidle_never_idle_hits_timeout() {
    // Persistent connection: in_flight stays above threshold forever → Timeout
    // at the ceiling, never a hang.
    let mut m = ReadinessMachine::new(SettleMode::NetworkIdle, cfg());
    let busy = PageObservation {
        load_fired: true,
        in_flight: 9,
        ..Default::default()
    };
    let out = feed(&mut m, busy, 1000);
    assert_eq!(out, Some(SettleOutcome::Timeout));
    assert_eq!(m.ticks(), cfg().tick_ceiling);
}

#[test]
fn settled_perpetual_dom_mutation_hits_dom_unstable() {
    // Network quiet + complete + url-stable, but the DOM mutates every tick
    // forever → DomUnstable (NOT a generic timeout), and does not hang.
    let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
    let churning = PageObservation {
        dom_mutations: 1,
        ..LOADED_IDLE
    };
    let out = feed(&mut m, churning, 1000);
    assert_eq!(out, Some(SettleOutcome::DomUnstable));
    assert_eq!(m.ticks(), cfg().tick_ceiling);
}

#[test]
fn settled_network_never_idle_is_timeout_not_dom_unstable() {
    // If the network never idles, the blocker is the network, so it's Timeout
    // even in settled mode (DomUnstable is reserved for net-quiet-but-dom-churns).
    let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
    let busy = PageObservation {
        in_flight: 9,
        dom_mutations: 1,
        ..LOADED_IDLE
    };
    assert_eq!(feed(&mut m, busy, 1000), Some(SettleOutcome::Timeout));
}

#[test]
fn ceiling_clamped_to_at_least_one_tick() {
    // timeout_ms=0 → ceiling clamps to 1 so a clean page can still resolve.
    let c = SettleConfig::with_ceiling(0);
    assert_eq!(c.tick_ceiling, 1);
}

#[test]
fn clean_page_settles_promptly_after_quiet_window() {
    let mut m = ReadinessMachine::new(SettleMode::Settled, SettleConfig::default());
    // Two quiet ticks short of the window, then the third reaches.
    assert_eq!(m.step(LOADED_IDLE), None);
    assert_eq!(m.step(LOADED_IDLE), None);
    assert_eq!(m.step(LOADED_IDLE), None);
    assert_eq!(m.step(LOADED_IDLE), None);
    assert_eq!(m.step(LOADED_IDLE), Some(SettleOutcome::Reached));
    // Default quiet_ticks is 5, so it reaches on the 5th idle tick.
    assert_eq!(m.ticks(), 5);
}

// ---- Determinism: identical observation sequence → identical verdict ----

#[test]
fn identical_observation_sequence_is_replay_equal() {
    let script = [
        PageObservation {
            load_fired: true,
            in_flight: 3,
            ready_complete: false,
            url_stable: false,
            dom_mutations: 2,
        },
        PageObservation {
            load_fired: true,
            in_flight: 1,
            ready_complete: true,
            url_stable: false,
            dom_mutations: 1,
        },
        PageObservation {
            load_fired: true,
            in_flight: 0,
            ready_complete: true,
            url_stable: true,
            dom_mutations: 0,
        },
        LOADED_IDLE,
        LOADED_IDLE,
        LOADED_IDLE,
    ];
    let run = || {
        let mut m = ReadinessMachine::new(SettleMode::Settled, cfg());
        let mut trace = Vec::new();
        for obs in script {
            trace.push(m.step(obs));
        }
        (trace, m.ticks())
    };
    assert_eq!(
        run(),
        run(),
        "same observation sequence must yield same verdict + tick count"
    );
}
