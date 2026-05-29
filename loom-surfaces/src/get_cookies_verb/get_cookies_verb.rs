extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetCookiesAction {
    pub action_id: String,
    /// Optional URL filter — passes through to CDP `Network.getCookies(urls)`.
    pub urls: Option<Vec<String>>,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
}
