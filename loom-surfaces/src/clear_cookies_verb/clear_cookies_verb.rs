extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearCookiesAction {
    pub action_id: String,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
}
