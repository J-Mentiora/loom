//! `set_cookies_verb` — v0.9.5 web-cookie-injection feature.
//!
//! SCAFFOLDING ONLY. The Action struct is defined; `execute()` is a Phase 3
//! follow-up. The authoritative dispatch lives at the daemon layer per the
//! EvaluateVerb dead-code pattern (see `evaluate_verb.rs:3-14`).
pub mod set_cookies_verb;
pub use set_cookies_verb::*;

#[cfg(test)]
mod interface_tests;
