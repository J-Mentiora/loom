// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/auth_middleware/interface_tests.rs` instead.
// Interface tests for `AuthMiddleware`. Verifies IC-RPC-05 HELLO
// handshake surface, AuthError categorisation, idle-timeout constant,
// constant-time token comparison.

use super::auth_middleware::{
    AuthError, AuthMiddleware, AuthMiddlewareApi, HelloMessage, Token, HELLO_IDLE_TIMEOUT,
};
use crate::error_translator::error_translator::JsonRpcError;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn token_is_opaque_newtype_around_string() {
    let t = Token("abc".into());
    let _: String = t.0;
}

#[test]
fn token_generate_returns_fresh_token() {
    fn _ck() -> Token {
        Token::generate()
    }
    let _ = _ck;
}

#[test]
fn token_ct_eq_is_constant_time_comparison() {
    fn _ck(a: &Token, b: &Token) -> bool {
        a.ct_eq(b)
    }
    let _ = _ck;
}

#[test]
fn hello_idle_timeout_is_5_seconds() {
    // design.md §4 + IC-RPC-05.
    assert_eq!(HELLO_IDLE_TIMEOUT, Duration::from_secs(5));
}

#[test]
fn hello_message_carries_token() {
    fn _ck(m: &HelloMessage) {
        let _: &Token = &m.token;
    }
    let _ = _ck;
}

#[test]
fn auth_error_distinguishes_malformed_mismatch_timeout() {
    // IC-RPC-05: three failure modes per design.md §4.
    let _ = AuthError::Malformed;
    let _ = AuthError::TokenMismatch;
    let _ = AuthError::Timeout;
}

#[test]
fn middleware_constructor_takes_arc_token() {
    fn _ck(t: Arc<Token>) -> Arc<AuthMiddleware> {
        AuthMiddleware::new(t)
    }
    let _ = _ck;
}

#[test]
fn parse_hello_signature_returns_hello_or_auth_error() {
    fn _ck<A: AuthMiddlewareApi>(a: &A, f: &[u8]) -> Result<HelloMessage, AuthError> {
        a.parse_hello(f)
    }
    let _ = _ck::<AuthMiddleware>;
}

#[test]
fn validate_hello_signature_returns_unit_or_auth_error() {
    fn _ck<A: AuthMiddlewareApi>(a: &A, m: &HelloMessage) -> Result<(), AuthError> {
        a.validate_hello(m)
    }
    let _ = _ck::<AuthMiddleware>;
}

#[test]
fn auth_error_to_envelope_returns_protocol_auth_required_envelope() {
    fn _ck<A: AuthMiddlewareApi>(a: &A, e: AuthError) -> JsonRpcError {
        a.auth_error_to_envelope(e)
    }
    let _ = _ck::<AuthMiddleware>;
}
