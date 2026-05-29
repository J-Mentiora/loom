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
        let r: Redacted<Zeroizing<String>> =
            Redacted::new(Zeroizing::new("hunter2".to_string()));
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
}
