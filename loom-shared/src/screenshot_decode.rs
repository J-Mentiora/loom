//! Decode a CDP `Page.captureScreenshot` response into raw PNG bytes.
//!
//! Chromium's DevTools Protocol returns the screenshot as a JSON object
//! `{ "data": "<base64-PNG>" }`. loom's shim layer converts that JSON into
//! CBOR before it crosses a process / WASM boundary, so what actually reaches
//! the content store is `CBOR({ "data": "<base64-PNG>" })` — a double-encoded
//! envelope, NOT a renderable image.
//!
//! This helper is the single place that unwraps that envelope: CBOR-decode →
//! read the `data` text → base64-decode → raw PNG. Both the host-native
//! navigate path (`loom-shims`) and the typed `record-screenshot` host import
//! (`loom-host`) call it so the bytes stored in the content store are a raw
//! `image/png`.
//!
//! The WASM guest deliberately ships without a CBOR or base64 decoder; keeping
//! this logic host-side preserves that (see `loom-surface-web`).

use base64::Engine as _;
use ciborium::value::Value as CborValue;

/// Upper bound on the CBOR response we will attempt to decode. A real
/// screenshot of a large page is a few MB; this is a generous ceiling that
/// stops a malformed / hostile shim response from forcing an unbounded
/// allocation (council R1/R3 — DoS guard).
pub const MAX_CDP_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;

/// PNG magic number — the first 8 bytes of every PNG file.
pub const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Why a CDP screenshot response could not be decoded into PNG bytes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScreenshotDecodeError {
    #[error("CDP screenshot response too large: {0} bytes (max {MAX_CDP_SCREENSHOT_BYTES})")]
    TooLarge(usize),
    #[error("CDP screenshot response was not valid CBOR")]
    NotCbor,
    #[error("CDP screenshot response CBOR had no string `data` field")]
    MissingData,
    #[error("CDP screenshot `data` field was not valid base64")]
    BadBase64,
}

/// Returns `true` if `bytes` begins with the PNG magic number.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_MAGIC.len() && bytes[..PNG_MAGIC.len()] == PNG_MAGIC
}

/// Decode a CBOR-encoded CDP `Page.captureScreenshot` response into raw PNG
/// bytes.
///
/// Accepts the CBOR encoding of `{ "data": "<base64-PNG>" }`. If the `data`
/// field is already raw CBOR bytes (a future-proofing path — some CDP
/// transports can deliver binary directly), those bytes are returned verbatim.
pub fn decode_cdp_screenshot(cbor_bytes: &[u8]) -> Result<Vec<u8>, ScreenshotDecodeError> {
    if cbor_bytes.len() > MAX_CDP_SCREENSHOT_BYTES {
        return Err(ScreenshotDecodeError::TooLarge(cbor_bytes.len()));
    }
    let value: CborValue =
        ciborium::de::from_reader(cbor_bytes).map_err(|_| ScreenshotDecodeError::NotCbor)?;
    let map = match value {
        CborValue::Map(entries) => entries,
        _ => return Err(ScreenshotDecodeError::MissingData),
    };
    for (k, v) in map {
        if matches!(&k, CborValue::Text(s) if s == "data") {
            return match v {
                // Standard CDP path: base64 string.
                CborValue::Text(b64) => base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|_| ScreenshotDecodeError::BadBase64),
                // Already-binary path: return the raw bytes.
                CborValue::Bytes(raw) => Ok(raw),
                _ => Err(ScreenshotDecodeError::MissingData),
            };
        }
    }
    Err(ScreenshotDecodeError::MissingData)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the CBOR encoding of `{ "data": <base64 of `png`> }` exactly as
    /// the shim layer produces it.
    fn cbor_data_envelope(png: &[u8]) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let value = CborValue::Map(vec![(CborValue::Text("data".into()), CborValue::Text(b64))]);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&value, &mut out).unwrap();
        out
    }

    fn fake_png() -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(b"\x00\x00\x00\x0dIHDR rest of a fake png payload");
        v
    }

    #[test]
    fn decodes_base64_envelope_to_raw_png() {
        let png = fake_png();
        let env = cbor_data_envelope(&png);
        // Sanity: the envelope is NOT itself a PNG (that's the whole bug).
        assert!(!is_png(&env));
        let decoded = decode_cdp_screenshot(&env).unwrap();
        assert_eq!(decoded, png);
        assert!(is_png(&decoded));
    }

    #[test]
    fn rejects_non_cbor() {
        assert_eq!(
            decode_cdp_screenshot(&[0xff, 0xfe, 0xfd]),
            Err(ScreenshotDecodeError::NotCbor)
        );
    }

    #[test]
    fn rejects_cbor_without_data_field() {
        let value = CborValue::Map(vec![(
            CborValue::Text("notdata".into()),
            CborValue::Text("x".into()),
        )]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        assert_eq!(
            decode_cdp_screenshot(&bytes),
            Err(ScreenshotDecodeError::MissingData)
        );
    }

    #[test]
    fn rejects_bad_base64() {
        let value = CborValue::Map(vec![(
            CborValue::Text("data".into()),
            CborValue::Text("not valid base64!!!".into()),
        )]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        assert_eq!(
            decode_cdp_screenshot(&bytes),
            Err(ScreenshotDecodeError::BadBase64)
        );
    }

    #[test]
    fn rejects_oversize_input() {
        let big = vec![0u8; MAX_CDP_SCREENSHOT_BYTES + 1];
        assert!(matches!(
            decode_cdp_screenshot(&big),
            Err(ScreenshotDecodeError::TooLarge(_))
        ));
    }

    #[test]
    fn accepts_raw_bytes_data_field() {
        let png = fake_png();
        let value = CborValue::Map(vec![(
            CborValue::Text("data".into()),
            CborValue::Bytes(png.clone()),
        )]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        assert_eq!(decode_cdp_screenshot(&bytes).unwrap(), png);
    }
}
