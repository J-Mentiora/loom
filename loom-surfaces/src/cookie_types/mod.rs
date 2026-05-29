//! CDP cookie types hand-written for v0.9.5.
//!
//! `chromiumoxide` is banned in `loom-surfaces` per `deny.toml`; the types
//! that ride the CDP wire (Network.Cookie / Network.CookieParam plus three
//! enums) are reproduced here as plain serde structs. JSON field naming uses
//! `#[serde(rename_all = "camelCase")]` to match Chrome's wire format.

pub mod cookie_types;
pub use cookie_types::*;

#[cfg(test)]
mod interface_tests;
