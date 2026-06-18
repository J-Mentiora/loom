//! Drift guard: every action-registry verb + param must have a matching
//! `BUILTIN_SCHEMA` entry that PERMITS it.
//!
//! # Why this exists (settle-capture)
//! The action registry (`loom_rpc::action_registry::ACTIONS`) and the daemon's
//! JSON schemas (`loom_cli::postinstall_runner::BUILTIN_SCHEMAS`) are
//! hand-maintained in SEPARATE places. A param added to the registry but not
//! the schema compiles fine and passes every host-level / mock-daemon test —
//! but the REAL daemon validates RPC args against the schema with
//! `additionalProperties:false`, so it rejects the unknown field at the wire
//! boundary (`"Additional properties are not allowed ('until' was
//! unexpected)"`). That exact bug shipped `web.navigate --until` / `web.wait_for`
//! past the whole hermetic suite and was only caught by a real-browser run.
//!
//! This test closes that gap hermetically: it fails the moment a registry verb
//! or param is missing from (or not permitted by) its schema.

use loom_cli::postinstall_runner::BUILTIN_SCHEMAS;
use loom_rpc::action_registry::ACTIONS;
use serde_json::Value;

fn schema_for(method: &str) -> Value {
    let (_, json) = BUILTIN_SCHEMAS
        .iter()
        .find(|(m, _)| *m == method)
        .unwrap_or_else(|| {
            panic!(
                "registry verb `{method}` has NO BUILTIN_SCHEMA entry — the daemon would \
                 reject it as method-not-found / unschema'd. Add it to \
                 loom-cli/src/postinstall_runner/postinstall_runner.rs::BUILTIN_SCHEMAS."
            )
        });
    serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("schema for {method} is invalid JSON: {e}"))
}

#[test]
fn every_registry_verb_has_a_builtin_schema_permitting_its_params() {
    for action in ACTIONS {
        let schema = schema_for(action.name);
        let request = &schema["request"];
        // `additionalProperties:false` (the default for our request schemas) is
        // what makes a missing property a HARD rejection at the daemon.
        let closed = request["additionalProperties"] == Value::Bool(false);
        let props = request.get("properties");

        for param in action.params {
            // The registry param is `session_id`; the wire/schema uses `session`
            // (the router accepts both). Skip the alias.
            if param.name == "session_id" {
                continue;
            }
            if closed {
                let present = props.and_then(|p| p.get(param.name)).is_some();
                assert!(
                    present,
                    "verb `{}` param `{}` is declared in the action registry but is MISSING from \
                     its request schema (which has additionalProperties:false). The real daemon \
                     would reject `{}` at the RPC boundary. Add it to the BUILTIN_SCHEMAS entry.",
                    action.name, param.name, param.name
                );
            }
        }
    }
}

#[test]
fn deadline_ms_is_permitted_by_every_web_request_schema() {
    // The per-action kill deadline rides as a flat `deadline_ms` arg on every
    // web.* call. With `additionalProperties:false`, the daemon would reject it
    // at the RPC boundary unless each web request schema declares it (optional).
    // Pin that here so a future schema edit that drops it fails loudly.
    for (method, json) in BUILTIN_SCHEMAS {
        if !method.starts_with("web.") {
            continue;
        }
        let schema: Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("schema {method} invalid: {e}"));
        let props = &schema["request"]["properties"];
        assert!(
            props.get("deadline_ms").is_some(),
            "web verb `{method}` request schema must permit `deadline_ms` (the daemon \
             would otherwise reject the per-action deadline at the RPC boundary)"
        );
        // It MUST stay optional — never in the required set.
        let required = schema["request"]["required"].as_array();
        let in_required = required
            .map(|r| r.iter().any(|v| v == "deadline_ms"))
            .unwrap_or(false);
        assert!(
            !in_required,
            "`deadline_ms` must be OPTIONAL on `{method}` (not in `required`)"
        );
    }
}

#[test]
fn settle_capture_params_are_in_the_navigate_and_wait_for_schemas() {
    // Pin the exact settle-capture additions so a future schema edit that drops
    // them fails loudly (this is the precise regression the real-browser run hit).
    let nav = schema_for("web.navigate");
    let nav_props = &nav["request"]["properties"];
    assert!(
        nav_props.get("until").is_some(),
        "web.navigate schema must permit `until`"
    );
    assert!(
        nav_props.get("timeout_ms").is_some(),
        "web.navigate schema must permit `timeout_ms`"
    );

    let wait = schema_for("web.wait_for");
    let wait_props = &wait["request"]["properties"];
    assert!(
        wait_props.get("until").is_some(),
        "web.wait_for schema must permit `until`"
    );
    assert!(
        wait_props.get("timeout_ms").is_some(),
        "web.wait_for schema must permit `timeout_ms`"
    );
}
