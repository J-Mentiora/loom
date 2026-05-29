extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

/// Single-cookie targeted delete. Maps to CDP `Network.deleteCookies(name, url?, domain?, path?)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteCookiesAction {
    pub action_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
}
