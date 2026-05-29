// SetCookiesVerb — implements `web-surface::set_cookies` (v0.9.5).
//
// Scaffolding only — the daemon-layer dispatch is the production path
// for v0.9.5; verb-level execute() lands in a follow-up.

extern crate alloc;

use crate::cookie_types::CookieSource;
use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

/// `web-surface::action` carrying set-cookies parameters. `source` is the
/// typed XOR enum from `cookie_types`: `Inline { cookies }` or
/// `Grant { grant_id }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetCookiesAction {
    pub action_id: String,
    pub source: CookieSource,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
}

impl Serialize for SafetyProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => s.serialize_str("default"),
            Self::Safe => s.serialize_str("safe"),
        }
    }
}

impl<'de> Deserialize<'de> for SafetyProfile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "default" => Ok(Self::Default),
            "safe" => Ok(Self::Safe),
            other => Err(serde::de::Error::custom(alloc::format!(
                "unknown SafetyProfile: {other}"
            ))),
        }
    }
}
