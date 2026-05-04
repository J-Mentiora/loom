//! Converters between `ciborium::value::Value` (CDP messages on the
//! shim's CBOR wire) and `serde_json::Value` (CDP messages on the
//! WebSocket wire). Lossy at the edges (CBOR has no equivalent for
//! NaN floats, JSON has no equivalent for raw bytes), but Chrome's CDP
//! never emits either of those.

use ciborium::value::Value as CborValue;
use serde_json::{Number, Value as JsonValue};

/// Best-effort CBOR → JSON. Tagged values are unwrapped; bytes become
/// hex strings. Returns Null on unrepresentable shapes (NaN, Infinity).
pub fn cbor_to_json(v: &CborValue) -> JsonValue {
    match v {
        CborValue::Null => JsonValue::Null,
        CborValue::Bool(b) => JsonValue::Bool(*b),
        CborValue::Integer(i) => {
            // ciborium::Integer can hold up to i128 / u128; JSON numbers
            // are i64/u64/f64. Try u64 first, then i64, fallback to f64.
            if let Ok(u) = u64::try_from(*i) {
                JsonValue::Number(Number::from(u))
            } else if let Ok(s) = i64::try_from(*i) {
                JsonValue::Number(Number::from(s))
            } else {
                let as_i128: i128 = (*i).into();
                Number::from_f64(as_i128 as f64)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            }
        }
        CborValue::Float(f) => Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        CborValue::Text(s) => JsonValue::String(s.clone()),
        CborValue::Bytes(b) => {
            // Hex-encode raw bytes — Chrome CDP base64-encodes screenshots
            // server-side, so this branch only fires for tests writing
            // raw bytes; lossy is fine.
            let mut out = String::with_capacity(b.len() * 2);
            for byte in b {
                out.push_str(&format!("{byte:02x}"));
            }
            JsonValue::String(out)
        }
        CborValue::Array(arr) => JsonValue::Array(arr.iter().map(cbor_to_json).collect()),
        CborValue::Map(entries) => {
            let mut obj = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                let key = match k {
                    CborValue::Text(s) => s.clone(),
                    CborValue::Integer(i) => i128::from(*i).to_string(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                obj.insert(key, cbor_to_json(v));
            }
            JsonValue::Object(obj)
        }
        CborValue::Tag(_, inner) => cbor_to_json(inner),
        _ => JsonValue::Null,
    }
}

/// JSON → CBOR. JSON numbers are encoded as integer when representable,
/// float otherwise.
pub fn json_to_cbor(v: &JsonValue) -> CborValue {
    match v {
        JsonValue::Null => CborValue::Null,
        JsonValue::Bool(b) => CborValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(u) = n.as_u64() {
                CborValue::Integer(u.into())
            } else if let Some(s) = n.as_i64() {
                CborValue::Integer(s.into())
            } else if let Some(f) = n.as_f64() {
                CborValue::Float(f)
            } else {
                CborValue::Null
            }
        }
        JsonValue::String(s) => CborValue::Text(s.clone()),
        JsonValue::Array(arr) => {
            CborValue::Array(arr.iter().map(json_to_cbor).collect())
        }
        JsonValue::Object(obj) => {
            let entries = obj
                .iter()
                .map(|(k, v)| (CborValue::Text(k.clone()), json_to_cbor(v)))
                .collect();
            CborValue::Map(entries)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_object_round_trips_through_cbor() {
        let original = json!({
            "frameId": "abc",
            "loaderId": "xyz",
            "depth": -1,
            "pierce": true,
            "data": null,
        });
        let cbor = json_to_cbor(&original);
        let back = cbor_to_json(&cbor);
        assert_eq!(back, original);
    }

    #[test]
    fn cbor_text_to_json_string() {
        let c = CborValue::Text("hello".into());
        assert_eq!(cbor_to_json(&c), json!("hello"));
    }

    #[test]
    fn cbor_array_round_trips() {
        let original = json!([1, 2, 3, "four", null, true]);
        let cbor = json_to_cbor(&original);
        let back = cbor_to_json(&cbor);
        assert_eq!(back, original);
    }
}
