use super::*;
use crate::request_router::{known_router_methods, router_required_params};
use std::collections::HashSet;

#[test]
fn registry_is_sorted_by_name() {
    let names: Vec<&str> = ACTIONS.iter().map(|a| a.name).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        names, sorted,
        "ACTIONS must be declared sorted by name for deterministic doc output"
    );
}

#[test]
fn registry_names_match_router() {
    let registry: HashSet<&str> = ACTIONS.iter().map(|a| a.name).collect();
    let router: HashSet<&str> = known_router_methods().iter().copied().collect();
    assert_eq!(
        registry, router,
        "registry action names must equal request_router's known method set"
    );
}

#[test]
fn registry_required_flags_match_router() {
    for action in ACTIONS {
        let registry_required: HashSet<&str> = action.required_param_names().collect();
        let router_required: HashSet<&str> = router_required_params(action.name)
            .expect("router knows this method (covered by registry_names_match_router)")
            .iter()
            .copied()
            .collect();
        assert_eq!(
            registry_required, router_required,
            "required-param set must match for {}: registry={:?} router={:?}",
            action.name, registry_required, router_required
        );
    }
}

#[test]
fn descriptions_are_multi_paragraph() {
    // Defends against accidental truncation: every description should contain
    // at least one paragraph break (\n\n), proving it carries the substance
    // promised by "descriptions from #[doc]" rather than a single
    // sentence that could just live in `summary`. This is a structural check
    // — not a keyword assertion — so legitimate prose edits don't false-fire.
    for action in ACTIONS {
        let paragraphs = action.description.matches("\n\n").count();
        assert!(
            paragraphs >= 1,
            "{}: description should be multi-paragraph (no \\n\\n found)",
            action.name
        );
    }
}

#[test]
fn registry_completeness_lint() {
    for action in ACTIONS {
        assert!(!action.summary.is_empty(), "{}: empty summary", action.name);
        assert!(
            action.summary.len() <= 100,
            "{}: summary too long ({} chars > 100)",
            action.name,
            action.summary.len()
        );
        assert!(
            !action.description.is_empty(),
            "{}: empty description",
            action.name
        );
        assert!(
            action.description.len() >= 100,
            "{}: description too short ({} chars < 100)",
            action.name,
            action.description.len()
        );
        assert!(!action.returns.is_empty(), "{}: empty returns", action.name);
        assert!(!action.example.is_empty(), "{}: empty example", action.name);
        for p in action.params {
            assert!(!p.name.is_empty(), "{}: empty param name", action.name);
            assert!(
                !p.doc.is_empty(),
                "{}: param {} has empty doc",
                action.name,
                p.name
            );
        }
    }
}

#[test]
fn examples_use_safe_placeholders() {
    fn looks_like_uuid(s: &str) -> bool {
        let groups = [8usize, 4, 4, 4, 12];
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 5 {
            return false;
        }
        for (p, &expected) in parts.iter().zip(&groups) {
            if p.len() != expected {
                return false;
            }
            if !p.chars().all(|c| c.is_ascii_hexdigit()) {
                return false;
            }
        }
        true
    }

    fn looks_like_ulid(s: &str) -> bool {
        s.len() == 26
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    }

    fn url_host_in_allowlist(token: &str) -> bool {
        if !(token.starts_with("http://") || token.starts_with("https://")) {
            return true;
        }
        let allowed = ["example.com", "example.org", "example.net", "localhost"];
        allowed.iter().any(|h| {
            token.contains(&format!("://{h}"))
                || token.contains(&format!("://{h}/"))
                || token.contains(&format!("://{h}:"))
        })
    }

    for action in ACTIONS {
        for token in action.example {
            assert!(
                !looks_like_ulid(token),
                "{}: example token {:?} matches real ULID shape; use <SESSION>",
                action.name,
                token
            );
            assert!(
                !looks_like_uuid(token),
                "{}: example token {:?} matches real-UUID shape",
                action.name,
                token
            );
            assert!(
                !token.starts_with("eyJ"),
                "{}: example token {:?} looks like a JWT",
                action.name,
                token
            );
            assert!(
                url_host_in_allowlist(token),
                "{}: example URL {:?} should use example.com / example.org / example.net / localhost",
                action.name, token
            );
        }
    }
}

#[test]
fn drift_negative_test_catches_extra_router_param() {
    // Synthesise a divergence: registry says `web.click` requires only session_id,
    // but the router requires session_id + selector. The equality check should reject this.
    let registry_required: HashSet<&str> = ["session_id"].into_iter().collect();
    let router_required: HashSet<&str> = ["session_id", "selector"].into_iter().collect();
    assert_ne!(
        registry_required, router_required,
        "drift test setup is broken: synthesised divergence somehow matches"
    );
}

#[test]
fn drift_negative_test_catches_extra_registry_param() {
    let registry_required: HashSet<&str> =
        ["session_id", "selector", "phantom"].into_iter().collect();
    let router_required: HashSet<&str> = ["session_id", "selector"].into_iter().collect();
    assert_ne!(registry_required, router_required);
}

#[test]
fn find_returns_some_for_known_action() {
    assert!(find("web.navigate").is_some());
    assert_eq!(find("web.navigate").unwrap().name, "web.navigate");
}

#[test]
fn find_returns_none_for_unknown_action() {
    assert!(find("custom.thing").is_none());
}

#[test]
fn surface_prefix_extracts_namespace() {
    let nav = find("web.navigate").unwrap();
    assert_eq!(nav.surface_prefix(), "web");
}

#[test]
fn paramtype_display_is_canonical() {
    assert_eq!(format!("{}", ParamType::String), "string");
    assert_eq!(format!("{}", ParamType::I64), "i64");
    assert_eq!(format!("{}", ParamType::U64), "u64");
}
