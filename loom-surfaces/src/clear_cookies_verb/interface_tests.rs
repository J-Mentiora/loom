use super::*;
use crate::safety::safety::SafetyProfile;

#[test]
fn action_round_trips() {
    let a = ClearCookiesAction {
        action_id: "ACT04".to_string(),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: ClearCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(back.action_id, "ACT04");
}
