use super::*;
use crate::cookie_types::CookieSource;
use crate::safety::safety::SafetyProfile;

#[test]
fn action_struct_round_trips_serde() {
    let a = SetCookiesAction {
        action_id: "ACT01".to_string(),
        source: CookieSource::Inline { cookies: vec![] },
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    let json = serde_json::to_string(&a).expect("serialize");
    let back: SetCookiesAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.action_id, "ACT01");
    assert_eq!(back.timeout_ticks, 5_000);
    assert!(matches!(back.profile, SafetyProfile::Default));
}

#[test]
fn safety_profile_serializes_to_lowercase() {
    assert_eq!(
        serde_json::to_string(&SafetyProfile::Default).unwrap(),
        "\"default\""
    );
    assert_eq!(
        serde_json::to_string(&SafetyProfile::Safe).unwrap(),
        "\"safe\""
    );
}
