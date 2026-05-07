//! Canonical metadata for every JSON-RPC action loom exposes.
//!
//! Single source of truth for `docs/actions.md`, the generated
//! `loom.1` man page, and the registry-driven help paths in
//! `loom action --help` / `loom action <name> --help`.
//!
//! The Rust dispatch enum (`Action`) and the request-router match-arms
//! remain authoritative for execution; this registry is *additive*
//! metadata. The unit test `registry_required_flags_match_router`
//! enforces equality of the required-param sets between this registry
//! and the router so the two cannot silently diverge.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    String,
    I64,
    U64,
}

impl fmt::Display for ParamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParamType::String => "string",
            ParamType::I64 => "i64",
            ParamType::U64 => "u64",
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ParamMeta {
    pub name: &'static str,
    pub ty: ParamType,
    pub doc: &'static str,
    pub required: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionMeta {
    pub name: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamMeta],
    pub returns: &'static str,
    pub example: &'static [&'static str],
}

impl ActionMeta {
    pub fn surface_prefix(&self) -> &'static str {
        self.name.split('.').next().unwrap_or(self.name)
    }

    pub fn required_param_names(&self) -> impl Iterator<Item = &'static str> {
        self.params.iter().filter(|p| p.required).map(|p| p.name)
    }
}

pub fn find(name: &str) -> Option<&'static ActionMeta> {
    ACTIONS.iter().find(|a| a.name == name)
}

// FUTURE: as more surfaces land (file.*, cloud.*, ...), consider
// grouping into per-surface registries and re-exporting a flat `ACTIONS`
// slice for backward compatibility. For 10 web.* actions today, one flat
// table is the simplest fit — and the `loom action --help` renderer
// already groups output by surface prefix so the UX scales.
//
// FUTURE: ParamType today only models String/I64/U64 because that is
// every type the router actually parses. New variants (e.g. Bool for a
// future `--force` valueless flag, or StringList for repeatable args)
// can be added when a new action introduces them. Adding a variant
// without updating the CLI's `build_action_command` helper is caught
// by the `paramtype_match_is_exhaustive` test in loom-cli.
//
// FUTURE: a `positional: bool` field on ParamMeta would unlock
// natural-CLI invocations like `loom action web.navigate <url>`. Out of
// scope for the initial registry; the existing CLI uniformly takes
// `--key value` so introducing positionals is a separate UX call.

pub const ACTIONS: &[ActionMeta] = &[
    ActionMeta {
        name: "web.click",
        summary: "Click an element by CSS selector.",
        description: "\
Resolves a CSS query selector against the active page and dispatches \
a synthetic click on the matched element. Surfaces selector misses as \
a typed `js_throw` host error rather than a generic 500 — clients can \
distinguish \"no such element\" from \"element raised during click \
handler\" by inspecting the `kind` field of the host error.\n\n\
Animations and transitions are forced to 0s under loom's deterministic \
profile, so click handlers complete synchronously. The receipt's \
`side_effects` records any DOM mutations triggered by the handler.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector for the target element. Standard CSS Level 3 syntax.",
                required: true,
            },
        ],
        returns: "Receipt with `status: \"ok\"` and `side_effects` populated when the click triggered DOM mutations. Selector miss → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.click", "--session", "<SESSION>", "--selector", "#submit"],
    },
    ActionMeta {
        name: "web.evaluate",
        summary: "Run a JavaScript expression in the page and return the value.",
        description: "\
Executes the supplied JavaScript expression via `Runtime.evaluate` in \
the page's JS context and returns the result as canonical JSON. \
Results larger than 64 KB are returned as a content-addressed blob \
reference (`content_ref`) instead of inline.\n\n\
Failure modes: an uncaught exception in the expression surfaces as \
`kind: \"js_throw\"`. Under the `safe` profile (default for `loom \
session create`) loom blocks destructive patterns — writes to \
`window.location`, `document.write`, and similar — before the \
expression reaches the page. The `standard` profile lifts the \
denylist; `full` removes all guards.\n\n\
Determinism: `Math.random()` is sfc32-seeded from the session seed, \
`Date.now()` and `performance.now()` return the frozen session epoch, \
and `requestAnimationFrame` ticks at fixed 16 ms intervals. Two \
sessions created with the same seed will produce identical results \
for an identical expression.\n\n\
Security: the expression is executed verbatim in the page. Treat it \
as untrusted code if any portion comes from user input — escape \
appropriately, or prefer `web.click` / `web.type` for typed \
interactions.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "expression",
                ty: ParamType::String,
                doc: "JavaScript expression. Returned value is JSON-canonicalised; >64 KB → content blob ref.",
                required: true,
            },
        ],
        returns: "Receipt with `return_value_json` — a JSON-string-encoded value (e.g. `\"\\\"hello\\\"\"` for a string `\"hello\"`, `\"42\"` for the number 42). Decode with one extra `JSON.parse`. Returns ≥64 KB are stored in CAS and `return_value_json` carries a `{\"content_ref\":\"<sha256>\"}` wrapper instead of inline bytes. `kind: \"js_throw\"` on uncaught exception.",
        example: &["loom", "action", "web.evaluate", "--session", "<SESSION>", "--expression", "document.title"],
    },
    ActionMeta {
        name: "web.hover",
        summary: "Dispatch a mouseover event at a CSS selector.",
        description: "\
Resolves a CSS query selector and dispatches a synthetic `mouseover` \
event at the matched element. Useful for triggering hover-state UI \
(menus, tooltips) before a follow-up `web.click`.\n\n\
Failure mode: selector miss surfaces as `kind: \"js_throw\"`. The \
hover does not wait for the resulting state — pair with `web.wait` \
on a predicate that observes the hover-induced change if you need to \
synchronise.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector for the element to hover.",
                required: true,
            },
        ],
        returns: "Receipt with `status: \"ok\"`. Selector miss → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.hover", "--session", "<SESSION>", "--selector", ".menu-toggle"],
    },
    ActionMeta {
        name: "web.navigate",
        summary: "Load a URL, follow redirects, capture DOM and screenshot.",
        description: "\
Navigates the active page to `url`, follows redirects, and captures \
both the resulting DOM snapshot and a viewport screenshot. The \
receipt records the final URL after redirects, the HTTP status code, \
and whether redirection occurred.\n\n\
URL allowlist: only `http`, `https`, and `about:blank` are accepted. \
Other schemes (`javascript:`, `file:`, `data:`, etc.) are rejected at \
the CLI before any network activity — surfaces as `kind: \
\"url_blocked\"` on the receipt.\n\n\
Typed errors: HTTP error responses surface as `kind: \"http_status\"` \
with the integer status code; DNS resolution failures surface as `kind: \
\"dns_failure\"` with the underlying Chromium error name (e.g. \
`net::ERR_NAME_NOT_RESOLVED`); other low-level network failures (TLS, \
timeout) surface as `kind: \"network_failure\"`. None of these are \
generic 500s.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "url",
                ty: ParamType::String,
                doc: "Target URL. Must be `http://`, `https://`, or `about:blank` — other schemes are rejected.",
                required: true,
            },
        ],
        returns: "Receipt with `url` (final URL after redirects), `status_code`, `redirected: bool`. Failure modes: `kind: \"http_status\"|\"dns_failure\"|\"network_failure\"|\"url_blocked\"`.",
        example: &["loom", "action", "web.navigate", "--session", "<SESSION>", "--url", "https://example.com"],
    },
    ActionMeta {
        name: "web.screenshot",
        summary: "Capture a PNG screenshot of the page or a selected element.",
        description: "\
Calls `Page.captureScreenshot`. Without `selector`, captures the full \
viewport. With `selector`, restricts the capture to the element's \
bounding rect (fails with `kind: \"js_throw\"` if the selector misses).\n\n\
The PNG is stored in the content-addressed blob store; the receipt \
carries a `screenshot_ref` (SHA-256) rather than inline bytes. \
Determinism: under the deterministic profile animations are 0s and \
`requestAnimationFrame` is locked to 16 ms ticks, so two sessions \
with the same seed produce byte-identical screenshots for the same \
page state.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "Optional CSS selector. When set, screenshot is clipped to the element's bounding rect.",
                required: false,
            },
        ],
        returns: "Receipt with `screenshot_after_hash: <sha256>` pointing to the PNG in CAS — fetch via `loom blob get <hash>`. With `selector` and a miss → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.screenshot", "--session", "<SESSION>"],
    },
    ActionMeta {
        name: "web.scroll",
        summary: "Scroll an element by a (delta_x, delta_y) offset.",
        description: "\
Calls `element.scrollBy(delta_x, delta_y)` on the resolved selector. \
Both deltas are optional and default to 0; passing only one is fine. \
Useful for revealing virtualised list rows or triggering scroll-based \
lazy loading before observing the result.\n\n\
Failure mode: selector miss → `kind: \"js_throw\"`. The scroll does \
not wait for any subsequent layout — pair with `web.wait` on a \
predicate that checks the post-scroll state if needed.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector for the scrollable element.",
                required: true,
            },
            ParamMeta {
                name: "delta_x",
                ty: ParamType::I64,
                doc: "Horizontal scroll offset in CSS pixels. Optional, defaults to 0.",
                required: false,
            },
            ParamMeta {
                name: "delta_y",
                ty: ParamType::I64,
                doc: "Vertical scroll offset in CSS pixels. Optional, defaults to 0.",
                required: false,
            },
        ],
        returns: "Receipt with `status: \"ok\"`. Selector miss → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.scroll", "--session", "<SESSION>", "--selector", ".feed", "--delta_y", "400"],
    },
    ActionMeta {
        name: "web.select",
        summary: "Set the value of a `<select>` element and dispatch `change`.",
        description: "\
Sets the resolved `<select>` element's `.value` to `value` and \
dispatches a `change` event so any framework-bound listeners (React, \
Vue, etc.) update accordingly. The element must be a `<select>`; a \
non-select target raises `kind: \"js_throw\"`.\n\n\
Loom does not validate that `value` matches one of the `<option>` \
values; the host engine accepts the assignment, and clients should \
either know the valid set or use `web.evaluate` to enumerate options \
first.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector for the `<select>` element.",
                required: true,
            },
            ParamMeta {
                name: "value",
                ty: ParamType::String,
                doc: "Value to assign. Should match one of the `<option>` `value` attributes.",
                required: true,
            },
        ],
        returns: "Receipt with `status: \"ok\"`. Selector miss / non-select target → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.select", "--session", "<SESSION>", "--selector", "#country", "--value", "GB"],
    },
    ActionMeta {
        name: "web.snapshot",
        summary: "Capture a full DOM snapshot of the active page.",
        description: "\
Calls `DOM.getDocument` and serialises the resulting tree into a \
content-addressed blob. The receipt carries a `content_ref` (SHA-256) \
plus a top-level hash so callers can detect DOM-state changes \
without comparing full snapshots.\n\n\
Snapshots include the deterministic profile's effects — frozen time, \
seeded randomness, 0-duration animations — so two snapshots from \
sessions with the same seed and action chain are bit-identical at \
this level.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
        ],
        returns: "Receipt with `dom_snapshot_hash: <sha256>` pointing to the serialised DOM in CAS — fetch via `loom blob get <hash>`.",
        example: &["loom", "action", "web.snapshot", "--session", "<SESSION>"],
    },
    ActionMeta {
        name: "web.type",
        summary: "Focus an input and type text into it.",
        description: "\
Resolves the selector, focuses the element, sets its `.value` to \
`text`, and dispatches `input` and `change` events so framework-bound \
listeners observe the update.\n\n\
The text is sent in one batch — loom does not simulate per-keystroke \
input events for fidelity / determinism reasons. Tests that need \
keystroke-level dispatch (e.g. type-ahead UIs that react to each \
character) should issue multiple `web.type` calls with single-char \
appended text.\n\n\
Failure mode: selector miss → `kind: \"js_throw\"`. Non-input targets \
also surface as `js_throw` from the underlying setter.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector for the input element.",
                required: true,
            },
            ParamMeta {
                name: "text",
                ty: ParamType::String,
                doc: "Text to set as the element's value. Sent in one batch (not keystroke-by-keystroke).",
                required: true,
            },
        ],
        returns: "Receipt with `status: \"ok\"`. Selector miss / non-input target → `kind: \"js_throw\"`.",
        example: &["loom", "action", "web.type", "--session", "<SESSION>", "--selector", "#email", "--text", "user@example.com"],
    },
    ActionMeta {
        name: "web.wait",
        summary: "Wait until a CSS selector resolves (or until timeout).",
        description: "\
Polls the page until the supplied CSS selector matches at least one \
element, or until `timeout_ms` milliseconds elapse. When `timeout_ms` \
is omitted, loom uses the daemon-configured default (typically 30 s).\n\n\
Polling cadence is fixed under the deterministic profile so two \
sessions with the same seed produce identical poll counts. The wait \
returns as soon as the predicate is true on any poll iteration.\n\n\
Typed error: `kind: \"wait_predicate_false\"` if the selector never \
resolves before the timeout. Use this to fail loud rather than \
chaining a brittle `web.click` against an element that is not yet \
present.",
        params: &[
            ParamMeta {
                name: "session_id",
                ty: ParamType::String,
                doc: "Session created via `loom session create`. 26-char ULID format.",
                required: true,
            },
            ParamMeta {
                name: "selector",
                ty: ParamType::String,
                doc: "CSS query selector. Wait succeeds the first poll where this resolves.",
                required: true,
            },
            ParamMeta {
                name: "timeout_ms",
                ty: ParamType::U64,
                doc: "Maximum wait time in milliseconds. Optional; defaults to the daemon's configured wait timeout.",
                required: false,
            },
        ],
        returns: "Receipt with `status: \"ok\"` once the selector resolves. Timeout → `kind: \"wait_predicate_false\"`.",
        example: &["loom", "action", "web.wait", "--session", "<SESSION>", "--selector", "#results", "--timeout_ms", "10000"],
    },
];
