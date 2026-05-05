// Behavior tests for NetworkInterceptor blocklist — TDD.
//
// AC coverage:
//  .1     : default blocklist ships with >= 100 active entries
//   matched URLs carry a `reason` derived from the
//                     section header (e.g. GA → "analytics")
//   strict matcher only inspects the URL host, not
//                     path/query — page-level navigates with blocklisted
//                     domain in the query don't false-positive

use loom_shims::network_interceptor::network_interceptor::{
    parse_blocklist_with_categories, url_in_blocklist_strict,
};

static BLOCKLIST_TEXT: &str = include_str!("../assets/default_blocklist.txt");

// === default blocklist has >= 100 entries ===

#[test]
fn test_blocklist_applies_entry_count_ge_100() {
    let entries = parse_blocklist_with_categories(BLOCKLIST_TEXT);
    assert!(
        entries.len() >= 100,
        "default blocklist must have >= 100 active entries, got {}",
        entries.len()
    );
}

#[test]
fn test_blocklist_filters_google_analytics_url() {
    let patterns = parse_blocklist_with_categories(BLOCKLIST_TEXT);
    let url = "https://www.google-analytics.com/analytics.js";
    let hit = url_in_blocklist_strict(url, &patterns);
    assert!(
        hit.is_some(),
        "google-analytics.com must be matched by the default blocklist"
    );
}

#[test]
fn test_blocklist_filters_hotjar_url() {
    let patterns = parse_blocklist_with_categories(BLOCKLIST_TEXT);
    let url = "https://static.hotjar.com/c/hotjar-12345.js";
    assert!(
        url_in_blocklist_strict(url, &patterns).is_some(),
        "hotjar.com must be matched by the default blocklist"
    );
}

#[test]
fn test_parse_blocklist_ignores_comments_and_blanks() {
    let text = "# comment\n\n*.example.com\n   \n# another comment\ntracker.io\n";
    let entries = parse_blocklist_with_categories(text);
    assert_eq!(
        entries.len(),
        2,
        "only non-comment, non-blank entries should be parsed"
    );
    let patterns: Vec<&str> = entries.iter().map(|(_, p)| p.as_str()).collect();
    assert!(patterns.contains(&"*.example.com"));
    assert!(patterns.contains(&"tracker.io"));
}

#[test]
fn test_url_in_blocklist_strict_wildcard_match() {
    let patterns = vec![("misc".to_string(), "*.google-analytics.com".to_string())];
    assert!(url_in_blocklist_strict("https://ssl.google-analytics.com/ga.js", &patterns).is_some());
    assert!(url_in_blocklist_strict("https://example.com/ga.js", &patterns).is_none());
}

#[test]
fn test_url_in_blocklist_strict_exact_domain_match() {
    let patterns = vec![("misc".to_string(), "analytics.google.com".to_string())];
    assert!(url_in_blocklist_strict("https://analytics.google.com/collect", &patterns).is_some());
    assert!(url_in_blocklist_strict("https://example.com/collect", &patterns).is_none());
}

#[test]
fn test_url_in_blocklist_strict_empty_patterns_returns_none() {
    assert!(url_in_blocklist_strict("https://www.google-analytics.com/ga.js", &[]).is_none());
}

// === NEW (Round-2 plan, item I1) ===

/// GA section entries are tagged `reason="analytics"`,
/// DoubleClick advertising-section entries are tagged with the
/// advertising section's lowercased name. Wildcard `*.` patterns match
/// against the host suffix only (host-strict).
#[test]
fn test_parse_blocklist_with_categories_groups_by_section_header() {
    let entries = parse_blocklist_with_categories(BLOCKLIST_TEXT);
    let ga = entries
        .iter()
        .find(|(_, p)| p == "*.google-analytics.com")
        .expect("default blocklist must contain *.google-analytics.com");
    assert_eq!(
        ga.0, "analytics",
        "GA entries must inherit the 'Analytics' section header (lowercased)"
    );

    let doubleclick = entries
        .iter()
        .find(|(_, p)| p == "*.doubleclick.net")
        .expect("default blocklist must contain *.doubleclick.net");
    assert_eq!(
        doubleclick.0, "advertising / ad networks",
        "DoubleClick entries must inherit the 'Advertising / Ad Networks' section header"
    );
}

/// host-only matching (D3): a blocklisted DOMAIN
/// appearing as a query-string token MUST NOT match. The original
/// `str::contains` matcher would false-positive on this URL.
#[test]
fn test_url_in_blocklist_strict_rejects_path_match() {
    let patterns = vec![(
        "analytics".to_string(),
        "*.google-analytics.com".to_string(),
    )];
    assert!(
        url_in_blocklist_strict("https://example.com/?ref=google-analytics.com", &patterns)
            .is_none(),
        "blocklist entry appearing in query string must NOT match (host-only matching)"
    );
    assert!(
        url_in_blocklist_strict("https://example.com/path/google-analytics.com", &patterns)
            .is_none(),
        "blocklist entry appearing in path must NOT match (host-only matching)"
    );
}

/// wildcard `*.google-analytics.com` matches any
/// subdomain AND returns the category from the section header.
#[test]
fn test_url_in_blocklist_strict_accepts_subdomain_for_wildcard_with_category() {
    let entries = parse_blocklist_with_categories(BLOCKLIST_TEXT);
    let hit = url_in_blocklist_strict("https://www.google-analytics.com/collect", &entries)
        .expect("subdomain of *.google-analytics.com must match");
    assert_eq!(hit.0, "analytics", "matched category must be 'analytics'");
    assert_eq!(
        hit.1, "*.google-analytics.com",
        "matched_pattern must be the wildcard entry"
    );
}

/// Sanity: bare domain matches the apex but not arbitrary subdomains.
#[test]
fn test_url_in_blocklist_strict_bare_domain_does_not_match_subdomain() {
    let patterns = vec![("misc".to_string(), "tracker.io".to_string())];
    assert!(url_in_blocklist_strict("https://tracker.io/p", &patterns).is_some());
    assert!(
        url_in_blocklist_strict("https://sub.tracker.io/p", &patterns).is_none(),
        "bare domain entry must not match subdomains (use *.tracker.io for that)"
    );
}
