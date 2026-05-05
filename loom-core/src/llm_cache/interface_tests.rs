// Interface tests for `LlmCache`.

use super::llm_cache::LlmCache;
use loom_shared::llm_types::{LlmCacheKey, LlmMode, LlmResponse};
use loom_shared::LoomErrorCode;

// 64-char sha256-shaped hex strings for test fixtures.
const HASH_A: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
const HASH_B: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn make_key(model: &str, prompt_hash: &str, tsv: &str) -> LlmCacheKey {
    LlmCacheKey {
        model: model.to_string(),
        prompt_hash: prompt_hash.to_string(),
        tool_schema_version: tsv.to_string(),
    }
}

fn make_response(body: serde_json::Value) -> LlmResponse {
    LlmResponse { body_json: body }
}

/// Cache hit in Recorded mode returns stored response + cache_hit=true.
#[test]
fn test_cache_hit_returns_stored_response_without_network_call() {
    let cache = LlmCache::new(LlmMode::Recorded);
    let key = make_key("claude-opus-4", HASH_A, "v1");
    let body = serde_json::json!({"content": "hello world"});
    cache.insert(&key, make_response(body.clone()));

    let result = cache.call(&key).expect("should hit cache");
    assert!(result.cache_hit, "cache_hit must be true on cache hit");
    assert_eq!(result.response.body_json, body);
}

/// Cache miss in Recorded mode returns LlmCacheMiss error.
#[test]
fn test_cache_miss_in_recorded_mode_returns_error() {
    let cache = LlmCache::new(LlmMode::Recorded);
    let key = make_key("claude-opus-4", HASH_B, "v1");

    let err = cache
        .call(&key)
        .expect_err("must error on miss in recorded mode");
    assert_eq!(err.code, LoomErrorCode::LlmCacheMiss);
}

/// Cache key string is colon-delimited with model:prompt_hash:tsv format.
#[test]
fn test_cache_key_string_is_colon_delimited() {
    let key = make_key("claude-opus-4", HASH_A, "v1");
    assert_eq!(
        key.cache_key_string(),
        format!("claude-opus-4:{}:v1", HASH_A)
    );
}

/// TapeFrame::LlmResponse serde roundtrip.
#[test]
fn test_tape_frame_llm_response_roundtrips() {
    use loom_core::determinism_harness::TapeFrame;

    let frame = TapeFrame::LlmResponse {
        model: "claude-opus-4".to_string(),
        prompt_hash: HASH_A.to_string(),
        tool_schema_version: "v1".to_string(),
        body_json: serde_json::json!({"content": "test"}),
    };

    let json = serde_json::to_string(&frame).expect("serialize");
    let back: TapeFrame = serde_json::from_str(&json).expect("deserialize");

    if let TapeFrame::LlmResponse {
        model,
        prompt_hash,
        tool_schema_version,
        body_json,
    } = back
    {
        assert_eq!(model, "claude-opus-4");
        assert_eq!(prompt_hash, HASH_A);
        assert_eq!(tool_schema_version, "v1");
        assert_eq!(body_json, serde_json::json!({"content": "test"}));
    } else {
        panic!("wrong variant after roundtrip");
    }
}

/// ReceiptPayload includes llm_cache_hit field and it serializes correctly.
#[test]
fn test_receipt_records_llm_cache_hit_true() {
    use loom_core::receipt_builder::ReceiptBuilder;

    let receipt = ReceiptBuilder::build_llm_receipt(
        "action-01".to_string(),
        1000,
        serde_json::json!({"content": "test"}),
        true,
    );

    let bytes = receipt.canonical_bytes().expect("canonical bytes");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("parse");
    assert_eq!(json["llm_cache_hit"], serde_json::json!(true));
}
