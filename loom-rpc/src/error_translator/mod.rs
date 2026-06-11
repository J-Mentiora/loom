//! `error_translator` — see crate root.
pub mod error_translator;
pub use error_translator::*;

#[cfg(test)]
mod interface_tests;

use std::any::Any;

impl ErrorTranslator {
    pub fn from_loom_error(err: &LoomErrorRef<'_>) -> JsonRpcError {
        JsonRpcError {
            code: err.0.code(),
            message: Self::truncate_message(&err.0.message()),
            data: err.0.data(),
        }
    }

    pub fn from_schema_violation(detail: SchemaViolationDetail) -> JsonRpcError {
        let message = Self::truncate_message(&format!(
            "schema violation: field '{}' expected {} got {}",
            detail.field, detail.expected, detail.actual
        ));
        JsonRpcError {
            code: LoomErrorCode::SchemaViolation,
            message,
            data: Some(serde_json::to_value(&detail).unwrap_or(serde_json::Value::Null)),
        }
    }

    pub fn catch_panic_into_envelope(payload: Box<dyn Any + Send>) -> JsonRpcError {
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "internal panic".to_string()
        };
        JsonRpcError {
            code: LoomErrorCode::InternalError,
            message: Self::truncate_message(&msg),
            data: None,
        }
    }

    /// Construct an `unknown_profile` envelope.
    /// `data` carries the provided value plus the canonical allowlist.
    pub fn from_unknown_profile(provided: &str, available: &[&str]) -> JsonRpcError {
        let message = Self::truncate_message(&format!("unknown profile: {provided}"));
        JsonRpcError {
            code: LoomErrorCode::UnknownProfile,
            message,
            data: Some(serde_json::json!({
                "provided": provided,
                "available": available,
            })),
        }
    }

    /// Construct an `invalid_network_mode` envelope. The message spells
    /// out WHY only `live` exists — `recorded`/`mixed` were historically
    /// accepted-but-inert, so a bare "invalid" would read like a typo
    /// hint rather than the truth (page-network replay is unimplemented).
    pub fn from_invalid_network_mode(provided: &str, available: &[&str]) -> JsonRpcError {
        let message = Self::truncate_message(&format!(
            "invalid network mode: {provided} — page traffic is always live; \
             page-network record/replay is not implemented"
        ));
        JsonRpcError {
            code: LoomErrorCode::InvalidNetworkMode,
            message,
            data: Some(serde_json::json!({
                "provided": provided,
                "available": available,
            })),
        }
    }

    /// Construct an `invalid_budget_key` envelope.
    pub fn from_invalid_budget_key(provided: &str, available: &[&str]) -> JsonRpcError {
        let message = Self::truncate_message(&format!("invalid budget key: {provided}"));
        JsonRpcError {
            code: LoomErrorCode::InvalidBudgetKey,
            message,
            data: Some(serde_json::json!({
                "provided": provided,
                "available": available,
            })),
        }
    }

    /// Construct an `invalid_capture_policy` envelope.
    pub fn from_invalid_capture_policy(provided: &str, available: &[&str]) -> JsonRpcError {
        let message = Self::truncate_message(&format!("invalid capture policy: {provided}"));
        JsonRpcError {
            code: LoomErrorCode::InvalidCapturePolicy,
            message,
            data: Some(serde_json::json!({
                "provided": provided,
                "available": available,
            })),
        }
    }

    pub fn truncate_message(s: &str) -> String {
        if s.len() <= MAX_MESSAGE_LEN {
            s.to_string()
        } else {
            // Back up to a char boundary: messages embed client-controlled
            // strings (profile names, URLs), the fixed cut point can land
            // inside a multi-byte char, and a str-slice panic aborts the
            // daemon (panic = "abort").
            let mut end = MAX_MESSAGE_LEN.saturating_sub(3);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &s[..end])
        }
    }
}
