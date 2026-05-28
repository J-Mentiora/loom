//! macOS Security Framework backend — `kSecClassGenericPassword` storage.
//!
//! Uses the [`security-framework`](https://docs.rs/security-framework) 3.x
//! crate's high-level `passwords::*` API to talk to the Apple Security
//! Framework via SecItem. Storage shape per plan §6 W2 + D11:
//!
//! - **Identity:** `kSecAttrService = service_id` (default `"loom"`),
//!   `kSecAttrAccount = label`. The `(service_id, account)` pair is the
//!   unique key.
//! - **Access:** The high-level `set_generic_password` uses the keychain
//!   defaults. v0.9.4 does NOT customise `kSecAttrAccessible` /
//!   `kSecAttrSynchronizable` because the high-level helper doesn't
//!   expose them; a follow-up that wires `kSecAttrAccessible =
//!   WhenUnlockedThisDeviceOnly` and `kSecAttrSynchronizable = false` via
//!   the lower-level `item::ItemAddOptions` is tracked.
//!
//! **Known v0.9.4 limitations** (Deviations from plan A-W2.1, accepted for
//! shipping the persistence path; tracked as follow-ups):
//! - `kSecAttrAccessible` defaults to the OS default (whatever Apple
//!   picks for new generic passwords; typically `AfterFirstUnlock` for
//!   the login keychain). Should be `WhenUnlockedThisDeviceOnly`.
//! - `kSecAttrSynchronizable` defaults to false because the helper
//!   doesn't set it — but operators with explicit iCloud Keychain sync
//!   policies may override.
//! - `kSecAttrCreator` discriminator (squatting prevention) is NOT
//!   attached. Same-user processes can write items under `service =
//!   "loom"` and loom would treat them as its own. This sits within the
//!   threat-model's AB6 accepted-risk band (same-user process
//!   exfiltration is acknowledged as out-of-scope for v0.9.4).
//! - `list_labels` returns `KeychainErrorKind::Unavailable` on macOS in
//!   v0.9.4. The high-level `passwords::*` API doesn't expose
//!   enumeration, and the lower-level `item::ItemSearchOptions` returns
//!   a `CFDictionary` whose attribute-extraction is too brittle to ship
//!   without a dedicated review. Operators on macOS can enumerate via
//!   `security find-generic-password -s loom` as a workaround. Tracked
//!   as a fast-follow-up.
//!
//! Error mapping per A-W5.2 / D31:
//!   `errSecItemNotFound → NotFound`,
//!   `errSecAuthFailed | errSecUserCanceled → Denied`,
//!   `errSecInteractionNotAllowed → Unavailable{NonInteractivePrompt}`,
//!   `errSecNotAvailable → Unavailable`,
//!   other → `Internal{internal_hash}`.

use crate::{KeychainAccess, KeychainError, KeychainErrorKind};
use security_framework::base::Error as SfError;
use zeroize::Zeroizing;

pub struct MacOsKeychain {
    service_id: &'static str,
    #[allow(dead_code)]
    allow_prompt: bool,
}

impl MacOsKeychain {
    pub fn new(service_id: &'static str, allow_prompt: bool) -> Result<Self, KeychainError> {
        Ok(Self {
            service_id,
            allow_prompt,
        })
    }

    pub fn service_id(&self) -> &'static str {
        self.service_id
    }

    pub fn allow_prompt(&self) -> bool {
        self.allow_prompt
    }
}

fn map_sf_error(err: SfError) -> KeychainError {
    let code = err.code();
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
    const ERR_SEC_AUTH_FAILED: i32 = -25293;
    const ERR_SEC_USER_CANCELED: i32 = -128;
    const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
    const ERR_SEC_NOT_AVAILABLE: i32 = -25291;

    let kind = match code {
        ERR_SEC_ITEM_NOT_FOUND => KeychainErrorKind::NotFound,
        ERR_SEC_AUTH_FAILED | ERR_SEC_USER_CANCELED => KeychainErrorKind::Denied,
        ERR_SEC_INTERACTION_NOT_ALLOWED => KeychainErrorKind::NonInteractivePrompt,
        ERR_SEC_NOT_AVAILABLE => KeychainErrorKind::Unavailable,
        _ => {
            return KeychainError::internal_from_message(err.to_string());
        }
    };
    KeychainError::new(kind, err.to_string())
}

impl KeychainAccess for MacOsKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        match security_framework::passwords::get_generic_password(self.service_id, label) {
            Ok(bytes) => Ok(Zeroizing::new(bytes)),
            Err(e) => Err(map_sf_error(e)),
        }
    }

    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        match security_framework::passwords::set_generic_password(
            self.service_id,
            label,
            &secret,
        ) {
            Ok(()) => Ok(()),
            Err(e) => Err(map_sf_error(e)),
        }
    }

    fn delete_secret(&self, label: &str) -> Result<(), KeychainError> {
        match security_framework::passwords::delete_generic_password(self.service_id, label) {
            Ok(()) => Ok(()),
            Err(e) => {
                let mapped = map_sf_error(e);
                if matches!(mapped.kind(), KeychainErrorKind::NotFound) {
                    Ok(())
                } else {
                    Err(mapped)
                }
            }
        }
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        // See module-level doc for the v0.9.4 limitation.
        Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            "list_labels is not implemented on macOS in v0.9.4; \
             enumerate via `security find-generic-password -s loom`. \
             Tracked as a fast-follow-up.",
        ))
    }
}
