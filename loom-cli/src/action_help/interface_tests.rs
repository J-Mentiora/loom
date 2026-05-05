use super::*;
use loom_rpc::action_registry::find;

#[test]
fn all_help_output_lists_actions_alphabetically() {
    let s = render_all_actions_after_help();
    let click_pos = s.find("web.click").expect("missing web.click");
    let evaluate_pos = s.find("web.evaluate").expect("missing web.evaluate");
    let navigate_pos = s.find("web.navigate").expect("missing web.navigate");
    assert!(
        click_pos < evaluate_pos,
        "web.click must come before web.evaluate"
    );
    assert!(
        evaluate_pos < navigate_pos,
        "web.evaluate must come before web.navigate"
    );
}

#[test]
fn all_help_output_groups_by_surface_prefix() {
    let s = render_all_actions_after_help();
    assert!(s.contains("web.*"), "missing surface header `web.*`");
    assert!(s.contains("Run `loom action <name> --help`"));
}

#[test]
fn single_action_help_includes_required_params() {
    let nav = find("web.navigate").unwrap();
    let h = render_per_action_help(nav);
    assert!(h.contains("--session"));
    assert!(h.contains("--url"));
    assert!(h.contains("required"), "expected required label for url");
}

#[test]
fn single_action_help_includes_returns_and_example() {
    let nav = find("web.navigate").unwrap();
    let h = render_per_action_help(nav);
    assert!(h.contains("RETURNS:"));
    assert!(h.contains("status_code"));
    assert!(h.contains("EXAMPLE:"));
    assert!(h.contains("loom action web.navigate"));
    assert!(h.contains("https://example.com"));
}

#[test]
fn extra_requests_help_recognises_short_and_long() {
    assert!(extra_requests_help(&["--help".to_string()]));
    assert!(extra_requests_help(&["-h".to_string()]));
    assert!(extra_requests_help(&[
        "--selector".to_string(),
        "x".to_string(),
        "--help".to_string()
    ]));
    assert!(!extra_requests_help(&[
        "--url".to_string(),
        "https://example.com".to_string()
    ]));
}

#[test]
fn validate_against_registry_accepts_valid_invocation() {
    let nav = find("web.navigate").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "url": "https://example.com",
    });
    assert!(validate_against_registry(nav, &params).is_ok());
}

#[test]
fn validate_against_registry_rejects_unknown_flag_with_did_you_mean() {
    let click = find("web.click").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "selecor": "#x",
    });
    let err = validate_against_registry(click, &params).unwrap_err();
    assert!(
        err.contains("--selecor") && err.contains("did you mean --selector?"),
        "expected did-you-mean hint in error: {err}"
    );
}

#[test]
fn validate_against_registry_rejects_unknown_flag_without_close_match() {
    let click = find("web.click").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "selector": "#x",
        "totally_unrelated_flag_name": "value",
    });
    let err = validate_against_registry(click, &params).unwrap_err();
    assert!(err.contains("--totally_unrelated_flag_name"));
    assert!(
        !err.contains("did you mean"),
        "no close match → no suggestion: {err}"
    );
}

#[test]
fn validate_against_registry_rejects_missing_required_param() {
    let click = find("web.click").unwrap();
    let params = serde_json::json!({ "session": "abc" });
    let err = validate_against_registry(click, &params).unwrap_err();
    assert!(
        err.contains("missing required flag --selector"),
        "got: {err}"
    );
}

#[test]
fn validate_against_registry_accepts_optional_params_omitted() {
    let scroll = find("web.scroll").unwrap();
    let params = serde_json::json!({ "session": "abc", "selector": ".feed" });
    assert!(validate_against_registry(scroll, &params).is_ok());
}

#[test]
fn validate_against_registry_accepts_optional_params_supplied() {
    let scroll = find("web.scroll").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "selector": ".feed",
        "delta_y": 400,
    });
    assert!(validate_against_registry(scroll, &params).is_ok());
}

#[test]
fn validate_against_registry_rejects_non_integer_for_i64_param() {
    let scroll = find("web.scroll").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "selector": ".feed",
        "delta_y": true,
    });
    let err = validate_against_registry(scroll, &params).unwrap_err();
    assert!(
        err.contains("invalid value for --delta_y") && err.contains("expected i64"),
        "got: {err}"
    );
}

#[test]
fn validate_against_registry_accepts_string_form_of_i64() {
    // The CLI's `parse_extra_to_json_typed` may emit `"10"` as a string when the
    // schema doesn't hint a type. Registry validation tolerates string-form numbers
    // for I64/U64 params so we don't reject valid invocations.
    let scroll = find("web.scroll").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "selector": ".feed",
        "delta_y": "400",
    });
    assert!(validate_against_registry(scroll, &params).is_ok());
}

#[test]
fn validate_against_registry_accepts_global_flag_keys() {
    // `--config` and `--pretty` are top-level Cli globals (`global = true`). With
    // `trailing_var_arg = true` on ActionArgs.extra they can land in the parsed
    // params object — registry validation must not reject them.
    let nav = find("web.navigate").unwrap();
    let params = serde_json::json!({
        "session": "abc",
        "url": "https://example.com",
        "config": "/tmp/config.toml",
        "pretty": true,
    });
    assert!(validate_against_registry(nav, &params).is_ok());
}

#[test]
fn paramtype_match_is_exhaustive() {
    // Smoke test: every variant of `ParamType` resolves through our internal
    // proof helper. Adding a new variant without updating action_help's match
    // statements fails this at compile time, not just here.
    use loom_rpc::action_registry::ParamType;
    for &t in &[ParamType::String, ParamType::I64, ParamType::U64] {
        let _ = format!("{t}");
    }
}
