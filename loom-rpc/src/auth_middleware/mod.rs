//! `auth_middleware` — see `systems/loom-rpc/modules/auth_middleware/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod auth_middleware;
pub use auth_middleware::*;

#[cfg(test)]
mod interface_tests;

use crate::error_translator::error_translator::{JsonRpcError, LoomErrorCode};
use rand::RngCore;
use subtle::ConstantTimeEq;

impl Token {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Token(hex::encode(bytes))
    }

    pub fn ct_eq(&self, other: &Token) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl AuthMiddlewareApi for AuthMiddleware {
    fn parse_hello(&self, frame: &[u8]) -> Result<HelloMessage, AuthError> {
        // Frame must start with "HELLO " followed by the token.
        let text = std::str::from_utf8(frame).map_err(|_| AuthError::Malformed)?;
        let token_str = text.strip_prefix("HELLO ").ok_or(AuthError::Malformed)?;
        if token_str.is_empty() {
            return Err(AuthError::Malformed);
        }
        Ok(HelloMessage {
            token: Token(token_str.to_string()),
        })
    }

    fn validate_hello(&self, msg: &HelloMessage) -> Result<(), AuthError> {
        if self.daemon_token.ct_eq(&msg.token) {
            Ok(())
        } else {
            Err(AuthError::TokenMismatch)
        }
    }

    fn auth_error_to_envelope(&self, _err: AuthError) -> JsonRpcError {
        JsonRpcError {
            code: LoomErrorCode::ProtocolAuthRequired,
            message: "authentication required: first message must be HELLO <token>".to_string(),
            data: None,
        }
    }
}
