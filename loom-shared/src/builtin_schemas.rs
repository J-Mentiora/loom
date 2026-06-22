// BUILTIN_SCHEMAS — the compile-time source of truth for every builtin
// action method's request/response JSON Schemas.
//
// Moved here from loom-cli::postinstall_runner (mcp-navigate-schema-regression)
// so the daemon can validate against the binary's OWN schemas (embedded-first,
// see loom-rpc::schema_provider::load_embedded_with_overlay) instead of
// trusting on-disk files that go stale across upgrades. loom-cli re-exports
// this const for postinstall (which mirrors it to disk for CLI use) and for
// the schema_registry_drift tests.
//
// Derived from wit/loom-surface.wit. Each entry is (method_name, json_str)
// where json_str has top-level "request" and "response" JSON Schema objects.
// WIT is the source of truth; these are the WIT-derived schemas.

pub const BUILTIN_SCHEMAS: &[(&str, &str)] = &[
    // web.navigate response documents the navigate-tier-2 wire
    // fields. `additionalProperties` is intentionally NOT declared on
    // the response so the wire receipt can grow further fields without
    // invalidating older schemas.
    (
        "web.navigate",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"url":{"type":"string"},"until":{"type":"string","enum":["load","networkidle","settled"]},"timeout_ms":{"type":"integer"}},"required":["session","url"],"additionalProperties":false},"response":{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"},"timing_ticks":{"type":"integer"},"side_effects":{"type":"array"},"error":{"type":["object","null"]},"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"url":{"type":"string"},"final_url":{"type":"string"},"title":{"type":"string"},"status_code":{"type":"integer"},"dom_snapshot_hash":{"type":"string"},"screenshot_after_hash":{"type":"string"},"console_count":{"type":"integer"},"console_lines":{"type":"array"},"network_count":{"type":"integer"},"network_summary":{"type":"object","properties":{"total_count":{"type":"integer"},"total_bytes":{"type":"integer"},"error_count":{"type":"integer"}}},"settle_until":{"type":"string"},"settle_outcome":{"type":"string"}}}}"#,
    ),
    (
        "web.click",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"dom_after_hash":{"type":"string","description":"sha256 of the normalized post-action DOM; present only under capture-policy=fingerprint"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // Canonical name `web.type` (was `web.type_text`); the legacy
    // `web.type_text` spelling resolves here via
    // `loom_shared::action_aliases::METHOD_ALIASES`.
    (
        "web.type",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"text":{"type":"string"},"mode":{"type":"string","enum":["value","keystrokes"]}},"required":["session","selector","text"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"dom_after_hash":{"type":"string","description":"sha256 of the normalized post-action DOM; present only under capture-policy=fingerprint"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // cdp-trusted-input: web.press_key — `key` required; `selector` + `modifiers`
    // optional. Real CDP Input.dispatchKeyEvent (isTrusted:true), host-side.
    (
        "web.press_key",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"key":{"type":"string"},"selector":{"type":"string"},"modifiers":{"type":"array","items":{"type":"string"}}},"required":["session","key"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.select",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"value":{"type":"string"}},"required":["session","selector","value"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"dom_after_hash":{"type":"string","description":"sha256 of the normalized post-action DOM; present only under capture-policy=fingerprint"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.hover",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"dom_after_hash":{"type":"string","description":"sha256 of the normalized post-action DOM; present only under capture-policy=fingerprint"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.scroll",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"delta_x":{"type":"integer"},"delta_y":{"type":"integer"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.wait",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"timeout_ms":{"type":"integer"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // settle-capture slice 2: standalone readiness wait on the current page.
    (
        "web.wait_for",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"until":{"type":"string","enum":["load","networkidle","settled"]},"timeout_ms":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"settle_until":{"type":"string"},"settle_outcome":{"type":"string"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.evaluate",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"expression":{"type":"string"}},"required":["session","expression"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"return_value_json":{"type":"string","description":"Canonical-JSON of the evaluated value. Numerics serialize as JSON strings (1+1 -> \"2\", Math.PI -> \"3.141...\"). Absent when value > 64KB and offloaded to content store; see return_value_blob_ref."},"return_value_blob_ref":{"type":"object","description":"ContentRef when canonical-JSON > 64KB. Truncation discriminator = this field present.","properties":{"sha256":{"type":"string"},"size_bytes":{"type":"integer"}},"required":["sha256","size_bytes"]}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // web.set_input_files: `paths` is a required array of absolute file-path
    // strings. Daemon-side `upload_guard` validates them against
    // LOOM_UPLOAD_ROOT (fail-closed, canonicalized, capped) before dispatch.
    (
        "web.set_input_files",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}}},"required":["session","selector","paths"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.screenshot",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.snapshot",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // video-capture: web.start_recording — session + optional cap overrides
    // (all integers). web.stop_recording — session only. Mirror the parse_action
    // arms + ActionMeta params.
    (
        "web.start_recording",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"max_duration_ms":{"type":"integer"},"max_bytes":{"type":"integer"},"frame_rate":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.stop_recording",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.network_log",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // `rpc.schemas` — JSON-RPC introspection. Wire-side
    // schema_validator treats it as a built-in (no param check); this
    // CLI-side schema is permissive so `loom action rpc.schemas` reaches
    // the daemon. `session` is accepted but ignored — `action_commands`
    // unconditionally inserts it, and rpc.schemas tolerates the extra
    // field. The response declares no required fields because the
    // SchemaRegistry envelope is shape-stable but field names are
    // implementation-defined.
    (
        "rpc.schemas",
        r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#,
    ),
    // v0.9.6 web-cookie-injection: 4 cookie verbs. Request shapes
    // mirror the `parse_action` arms in
    // `loom-rpc::request_router::parse_action` and the `ActionMeta`
    // entries in `action_registry`. `source` for set_cookies is the
    // typed XOR `CookieSource` JSON object (validated daemon-side);
    // the JSON-Schema here accepts an object loosely so the wire
    // contract isn't over-tight. Response shape is the standard
    // hash-only triple plus the verb-specific result field.
    (
        "web.set_cookies",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"source":{"type":"object"}},"required":["session","source"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"set_cookies_result":{"type":"array"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.get_cookies",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"urls":{"type":"array","items":{"type":"string"}}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"get_cookies_result":{"type":"array"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.clear_cookies",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"clear_cookies_result":{"type":"object","properties":{"cleared_count":{"type":"integer"}}}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.delete_cookies",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"deadline_ms":{"type":"integer"},"name":{"type":"string"},"url":{"type":"string"},"domain":{"type":"string"},"path":{"type":"string"}},"required":["session","name"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"delete_cookies_result":{"type":"object","properties":{"name":{"type":"string"},"matched":{"type":"boolean"}}}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
];
