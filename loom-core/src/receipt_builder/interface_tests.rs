// Interface-level tests for receipt_builder.
// Exercises: builder methods, capture profile, canonical bytes.

#[cfg(test)]
mod tests {
    use crate::content_store::ContentRef;
    use crate::error_types::{ReceiptCode, ReceiptSurface};
    use crate::receipt_builder::{
        CaptureProfile, ConsoleLine, NetworkEvent, ReceiptBuilder, ReceiptStatus,
    };

    fn dom_ref() -> ContentRef {
        ContentRef {
            sha256: "a".repeat(64),
            size_bytes: 4096,
        }
    }

    fn ss_ref() -> ContentRef {
        ContentRef {
            sha256: "b".repeat(64),
            size_bytes: 1024,
        }
    }

    #[test]
    fn click_receipt_has_hash_fields_not_blob_fields() {
        let r = ReceiptBuilder::build_click_receipt(
            "a1".to_string(),
            100,
            "0".repeat(64),
            "1".repeat(64),
        );
        assert!(r.dom_after_hash.is_some());
        assert!(r.dom_after_blob_ref.is_none());
        assert_eq!(r.status, ReceiptStatus::Ok);
        assert_eq!(r.code, ReceiptCode::WebActionCompleted);
    }

    #[test]
    fn navigate_receipt_has_blob_fields_not_hash_fields() {
        let r = ReceiptBuilder::build_navigate_receipt(
            "a2".to_string(),
            200,
            dom_ref(),
            ss_ref(),
            vec![],
            vec![],
        );
        assert!(r.dom_after_blob_ref.is_some());
        assert!(r.dom_after_hash.is_none());
        assert_eq!(r.status, ReceiptStatus::Ok);
    }

    #[test]
    fn evaluate_receipt_has_return_value_json_and_console_lines() {
        let r = ReceiptBuilder::build_evaluate_receipt(
            "a3".to_string(),
            300,
            Some("42".to_string()),
            None,
            vec![ConsoleLine {
                level: "log".to_string(),
                message: "hi".to_string(),
                timing_ticks: 1,
            }],
        );
        assert_eq!(r.return_value_json.as_deref(), Some("42"));
        assert!(r.return_value_blob_ref.is_none());
        assert_eq!(r.console_lines.len(), 1);
        assert!(r.dom_after_hash.is_none());
        assert!(r.network_events.is_empty());
    }

    #[test]
    fn error_receipt_has_message_and_error_status() {
        let r = ReceiptBuilder::build_error_receipt(
            "err".to_string(),
            50,
            ReceiptCode::WebNavigationFailed,
            "navigation failed".to_string(),
            ReceiptSurface::Web,
            None,
        );
        assert_eq!(r.status, ReceiptStatus::Error);
        assert_eq!(r.code, ReceiptCode::WebNavigationFailed);
        assert!(r.message.is_some());
    }

    #[test]
    fn message_truncated_to_280_chars() {
        let long = "x".repeat(400);
        let r = ReceiptBuilder::build_error_receipt(
            "e2".to_string(),
            0,
            ReceiptCode::SchemaViolation,
            long,
            ReceiptSurface::Core,
            None,
        );
        assert_eq!(r.message.unwrap().len(), 280);
    }

    #[test]
    fn minimal_profile_strips_blob_refs_keeps_hashes() {
        let net = NetworkEvent {
            method: "GET".to_string(),
            url: "https://ex.com".to_string(),
            status_code: 200,
            response_body_sha256_hex: "c".repeat(64),
            response_body_size_bytes: 100,
            response_body_ref: Some(ContentRef {
                sha256: "c".repeat(64),
                size_bytes: 100,
            }),
            timing_ticks: 10,
            content_type: String::new(),
        };
        let mut r = ReceiptBuilder::build_navigate_receipt(
            "a4".to_string(),
            400,
            dom_ref(),
            ss_ref(),
            vec![net],
            vec![ConsoleLine {
                level: "log".to_string(),
                message: "m".to_string(),
                timing_ticks: 1,
            }],
        );
        r.apply_capture_profile(CaptureProfile::Minimal);

        assert!(r.dom_after_blob_ref.is_none());
        assert!(r.dom_after_hash.is_some());
        assert!(r.network_events[0].response_body_ref.is_none());
        assert!(r.console_lines.is_empty());
    }

    #[test]
    fn canonical_bytes_are_deterministic_and_sorted() {
        let r = ReceiptBuilder::build_click_receipt(
            "ord".to_string(),
            500,
            "z".repeat(64),
            "a".repeat(64),
        );
        let b1 = r.canonical_bytes().unwrap();
        let b2 = r.canonical_bytes().unwrap();
        assert_eq!(b1, b2);

        let json: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&b1).unwrap();
        let keys: Vec<_> = json.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            keys, sorted,
            "JCS must produce lexicographically ordered keys"
        );
    }

    #[test]
    fn all_numeric_fields_are_integers() {
        let r = ReceiptBuilder::build_click_receipt(
            "int".to_string(),
            9999,
            "0".repeat(64),
            "1".repeat(64),
        );
        let v = serde_json::to_value(&r).unwrap();
        fn check(v: &serde_json::Value) {
            match v {
                serde_json::Value::Number(n) => assert!(n.is_u64() || n.is_i64()),
                serde_json::Value::Array(a) => a.iter().for_each(check),
                serde_json::Value::Object(o) => o.values().for_each(check),
                _ => {}
            }
        }
        check(&v);
    }
}
