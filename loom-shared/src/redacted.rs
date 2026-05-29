//! `Redacted<T>` — a wrapper that hides its inner value in `Debug`/`Display`/
//! `Serialize` output. Intentional asymmetry: outputs are redacted, inputs are
//! accepted normally. The deserializer round-trips `T` so values can flow in
//! from JSON / keychain blobs.
//!
//! - `Debug` / `Display` → `"[REDACTED]"`
//! - `Serialize` → `"[REDACTED]"` (NOT `T`)
//! - `Deserialize` → normal `T::deserialize`
//! - `Drop` → `T::zeroize()` when `T: Zeroize`
//!
//! Used at every layer that handles cookie values (per D4 / D12). The cookie
//! `value` field is typed `Redacted<Zeroizing<String>>`; the inner `Zeroizing`
//! enforces the heap-buffer wipe, the outer `Redacted` enforces output
//! scrubbing.
//!
//! Counterpart to `secrecy::SecretBox<T>` from the ecosystem, but with the
//! opposite Serialize choice: secrecy refuses Serialize (serde errors), we
//! emit `"[REDACTED]"` so receipts and structured logs stay structurally
//! valid while still hiding the secret.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

#[derive(Clone, PartialEq, Eq)]
pub struct Redacted<T: Zeroize>(T);

impl<T: Zeroize> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Expose the wrapped value. Use only at boundaries where the raw value
    /// is structurally required (e.g. encoding a CDP `Network.setCookies`
    /// envelope). Never call `expose()` in a serialization path.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn expose_mut(&mut self) -> &mut T {
        &mut self.0
    }

    /// Move-out of the wrapper. Uses `ManuallyDrop` to suppress the wrapper's
    /// zeroize-on-drop, since the returned value takes ownership of the
    /// memory. Use sparingly; prefer `expose()` + clone at boundaries.
    pub fn into_inner(self) -> T {
        let me = std::mem::ManuallyDrop::new(self);
        // SAFETY: `ManuallyDrop` suppresses the wrapper's Drop, so `T` is not
        // double-dropped; `ptr::read` produces a single ownership transfer.
        unsafe { std::ptr::read(&me.0) }
    }
}

impl<T: Zeroize> std::fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T: Zeroize> std::fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T: Zeroize> Serialize for Redacted<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("[REDACTED]")
    }
}

impl<'de, T: Deserialize<'de> + Zeroize> Deserialize<'de> for Redacted<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self)
    }
}

impl<T: Zeroize> Zeroize for Redacted<T> {
    fn zeroize(&mut self) {
        self.0.zeroize()
    }
}

impl<T: Zeroize> Drop for Redacted<T> {
    // Drop requires the same Zeroize bound as the struct (E0367-safe).
    fn drop(&mut self) {
        self.0.zeroize()
    }
}

/// v0.9.6 §10 heap-wipe hardening.
///
/// `String::zeroize()` from zeroize 1.6+ calls `Vec::clear()` which sets the
/// length to 0 but DOES NOT overwrite the heap-allocated buffer. The bytes
/// can linger in process memory until the allocator reclaims them.
///
/// This helper forces the heap wipe by calling `as_mut_vec().fill(0)` BEFORE
/// the wrapper's Drop runs. Call this explicitly at every CDP-encode boundary
/// where a cookie value's `String` buffer is about to leave scope — the
/// cookie value will not survive in process memory between drop and
/// allocator reuse, only as a best-effort window (the allocator may have
/// already moved on by the time we run).
///
/// Caveats:
///   - "Best-effort". A compiler optimisation, a non-tracked copy on the
///     stack, or a heap realloc that happened earlier in the value's
///     lifetime may have left residues elsewhere. This wipe addresses the
///     specific buffer pointed to by the String at call time.
///   - Cookie *names* are NOT wiped — they're audit-chain material.
///   - The wipe runs inside Vault::substitute_cookies and the verb-side
///     CDP-encode path; it does NOT cover the bytes once they cross WIT
///     into wasmtime-managed linear memory.
///
/// Documented in `security/vault_threat_model.md` (Tier 11).
pub fn wipe_string_buffer_in_place(s: &mut String) {
    // SAFETY: the bytes we overwrite are already owned by `s`. We're not
    // mutating the byte length (which would change UTF-8 validity) — we
    // overwrite every existing byte with 0 (still valid UTF-8 because
    // \0 is a 1-byte ASCII codepoint), then clear() sets the length to
    // 0 without freeing the buffer.
    unsafe {
        let v = s.as_mut_vec();
        for b in v.iter_mut() {
            *b = 0;
        }
        v.clear();
    }
}

/// Same as `wipe_string_buffer_in_place` but for a `Vec<u8>` containing
/// cookie value bytes (e.g. the keychain blob `Vec<u8>` returned by
/// `Vault::substitute_cookies` after the Zeroizing wrapper is dropped
/// downstream).
pub fn wipe_byte_buffer_in_place(v: &mut Vec<u8>) {
    for b in v.iter_mut() {
        *b = 0;
    }
    v.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    #[test]
    fn debug_emits_redacted() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        assert_eq!(format!("{r:?}"), "[REDACTED]");
    }

    #[test]
    fn display_emits_redacted() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        assert_eq!(format!("{r}"), "[REDACTED]");
    }

    #[test]
    fn serialize_emits_redacted_string() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        let json = serde_json::to_string(&r).expect("serialize");
        assert_eq!(json, "\"[REDACTED]\"");
    }

    #[test]
    fn deserialize_roundtrips_inner_value() {
        let json = "\"hunter2\"";
        let r: Redacted<String> = serde_json::from_str(json).expect("deserialize");
        assert_eq!(r.expose(), "hunter2");
    }

    #[test]
    fn expose_returns_inner() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        assert_eq!(r.expose(), "hunter2");
    }

    #[test]
    fn into_inner_yields_owned_value() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        let s: String = r.into_inner();
        assert_eq!(s, "hunter2");
    }

    #[test]
    fn nested_with_zeroizing_serialize_still_redacts() {
        let r: Redacted<Zeroizing<String>> = Redacted::new(Zeroizing::new("hunter2".to_string()));
        let json = serde_json::to_string(&r).expect("serialize");
        assert_eq!(json, "\"[REDACTED]\"");
    }

    #[test]
    fn zeroize_resets_to_default_for_string() {
        let mut r: Redacted<String> = Redacted::new("hunter2".to_string());
        r.zeroize();
        assert_eq!(r.expose(), ""); // String::zeroize() wipes + clears
    }

    #[test]
    fn drop_zeroizes_string_value() {
        // Drop wipes the heap buffer. We can't observe the wipe directly
        // (the value goes out of scope), but we can verify the call path
        // doesn't panic and the type implements Drop by smoke-testing a
        // value that goes out of scope.
        {
            let _r: Redacted<String> = Redacted::new("hunter2".to_string());
        } // drop fires here
          // No assert needed — compilation + clean drop is the contract.
    }

    #[test]
    fn clone_preserves_inner() {
        let r: Redacted<String> = Redacted::new("hunter2".to_string());
        let c = r.clone();
        assert_eq!(c.expose(), "hunter2");
    }

    // === v0.9.6 §10 heap-wipe hardening ===

    #[test]
    fn wipe_string_buffer_in_place_zeros_the_underlying_bytes() {
        let mut s = String::from("super-secret-session-token");
        // Capture the buffer pointer + capacity BEFORE wiping so we can
        // verify the bytes-at-that-address are zeroed.
        let ptr = s.as_ptr();
        let cap = s.capacity();
        assert!(cap >= s.len(), "cap must hold the bytes");
        let original_len = s.len();

        wipe_string_buffer_in_place(&mut s);

        // After the wipe: length is 0 (cleared), capacity unchanged
        // (buffer not freed).
        assert_eq!(s.len(), 0);
        assert_eq!(s.capacity(), cap);

        // The buffer's first `original_len` bytes are zero. We read them
        // back via the saved pointer. SAFETY: the buffer is still owned
        // by `s` (not freed), so the pointer is valid, and we're reading
        // bytes we ourselves zeroed.
        unsafe {
            let bytes = std::slice::from_raw_parts(ptr, original_len);
            for (i, b) in bytes.iter().enumerate() {
                assert_eq!(*b, 0, "byte {i} should be zero, got {b:#x}");
            }
        }
    }

    #[test]
    fn wipe_string_buffer_in_place_on_empty_string_is_noop() {
        let mut s = String::new();
        wipe_string_buffer_in_place(&mut s);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn wipe_byte_buffer_in_place_zeros_the_underlying_bytes() {
        let mut v: Vec<u8> = b"keychain blob bytes".to_vec();
        let ptr = v.as_ptr();
        let cap = v.capacity();
        let original_len = v.len();

        wipe_byte_buffer_in_place(&mut v);

        assert_eq!(v.len(), 0);
        assert_eq!(v.capacity(), cap);

        unsafe {
            let bytes = std::slice::from_raw_parts(ptr, original_len);
            for (i, b) in bytes.iter().enumerate() {
                assert_eq!(*b, 0, "byte {i} should be zero");
            }
        }
    }

    #[test]
    fn wipe_string_buffer_preserves_utf8_invariant() {
        // After wipe, the String must still be valid UTF-8 (every byte
        // becomes \0 which is valid 1-byte UTF-8, then length is set
        // to 0). The String continues to be safely usable.
        let mut s = String::from("über-secret");
        wipe_string_buffer_in_place(&mut s);
        assert_eq!(s, "");
        // Push something else — verifies the String is still usable
        // (no internal invariants broken).
        s.push_str("ok");
        assert_eq!(s, "ok");
    }
}
