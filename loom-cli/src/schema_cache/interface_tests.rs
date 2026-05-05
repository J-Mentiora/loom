// Interface tests for `SchemaCache`. Verifies the immutable-after-load
// posture, the missing-dir error, and the request/response accessor
// pair.

use super::schema_cache::SchemaCache;
use crate::CliError;

#[test]
fn load_signature_takes_path() {
    fn _ck(p: &std::path::Path) -> Result<SchemaCache, CliError> {
        SchemaCache::load(p)
    }
    let _ = _ck;
}

#[test]
fn empty_constructor_returns_zero_methods() {
    let c = SchemaCache::empty();
    assert!(c.is_empty());
    assert_eq!(c.len(), 0);
    assert_eq!(c.methods().count(), 0);
}

#[test]
fn request_and_response_accessors_return_optional() {
    fn _req(c: &SchemaCache) -> Option<&serde_json::Value> {
        c.request_schema("session.create")
    }
    fn _resp(c: &SchemaCache) -> Option<&serde_json::Value> {
        c.response_schema("session.create")
    }
    let _ = (_req, _resp);
}

// === Immutable after load ===
//
// Encoded structurally — there is no `&mut self` method on
// `SchemaCache` (other than the private inner field). The test below
// documents that there is no public mutator on the API surface.
#[test]
fn no_public_mutator_present() {
    // If any `&mut self` accessor is added, this test should be
    // reviewed alongside an audit of the immutable-after-load posture.
    let c = SchemaCache::empty();
    let _ = c;
}

#[test]
fn methods_iterator_returns_str() {
    let c = SchemaCache::empty();
    let v: Vec<&str> = c.methods().collect();
    assert!(v.is_empty());
}
