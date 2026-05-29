use super::*;
use crate::safety::safety::SafetyProfile;

#[test]
fn action_round_trips_with_full_scoping() {
    let a = DeleteCookiesAction {
        action_id: "ACT05".to_string(),
        name: "sid".to_string(),
        url: None,
        domain: Some("127.0.0.1".to_string()),
        path: Some("/".to_string()),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: DeleteCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(back.name, "sid");
    assert_eq!(back.domain.as_deref(), Some("127.0.0.1"));
}

#[test]
fn action_serialization_skips_none_optional_fields() {
    let a = DeleteCookiesAction {
        action_id: "ACT06".to_string(),
        name: "sid".to_string(),
        url: None,
        domain: None,
        path: None,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    let j = serde_json::to_string(&a).expect("serialize");
    assert!(!j.contains("\"url\""));
    assert!(!j.contains("\"domain\""));
    assert!(!j.contains("\"path\""));
}
