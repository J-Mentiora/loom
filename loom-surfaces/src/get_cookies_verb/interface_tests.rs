use super::*;
use crate::safety::safety::SafetyProfile;

#[test]
fn action_round_trips_with_urls() {
    let a = GetCookiesAction {
        action_id: "ACT02".to_string(),
        urls: Some(vec!["http://127.0.0.1/".to_string()]),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Safe,
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: GetCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(back.urls.as_ref().unwrap().len(), 1);
}

#[test]
fn action_round_trips_with_no_urls() {
    let a = GetCookiesAction {
        action_id: "ACT03".to_string(),
        urls: None,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: GetCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert!(back.urls.is_none());
}
