//! macOS Security Framework backend — `kSecClassGenericPassword` storage.
//!
//! Uses the [`security-framework`](https://docs.rs/security-framework) 3.x
//! crate's high-level `passwords::*` API to talk to the Apple Security
//! Framework via SecItem. Storage shape per plan §6 W2 + D11:
//!
//! - **Identity:** `kSecAttrService = service_id` (default `"loom"`),
//!   `kSecAttrAccount = label`. The `(service_id, account)` pair is the
//!   unique key.
//! - **Access:** Items are pinned to this device only. Every op queries
//!   with an explicit `kSecAttrSynchronizable = false`, so get/set/delete
//!   only ever touch the non-iCloud store — with the attribute unset, the
//!   high-level helpers read from (and, on upsert, write into) the
//!   cloud-synchronized store too. Items written before this hardening
//!   carried no synchronizable attribute, which the keychain treats as
//!   false, so they still match the pinned queries. `set_secret`
//!   additionally stamps `kSecAttrAccessible =
//!   AfterFirstUnlockThisDeviceOnly` (a background daemon must keep
//!   working after reboot + first unlock; `ThisDeviceOnly` forbids any
//!   migration into a syncing store). The file-based login keychain
//!   accepts-but-ignores `kSecAttrAccessible`; it is stamped anyway so the
//!   constraint already holds if storage ever moves to the data-protection
//!   keychain (`kSecUseDataProtectionKeychain`).
//!
//! **Known v0.9.4 limitations** (Deviations from plan A-W2.1, accepted for
//! shipping the persistence path; tracked as follow-ups):
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
//! **Prompt gating (`allow_prompt`, A-W5.2).** When constructed with
//! `allow_prompt == false` (the daemon's non-TTY default), the backend
//! disables Security-framework UI for its lifetime via
//! `SecKeychainSetUserInteractionAllowed(false)` (held as a RAII
//! [`KeychainUserInteractionLock`], re-enabled on drop). Any op that would
//! otherwise block the daemon on a keychain unlock / ACL dialog then fails
//! fast with `errSecInteractionNotAllowed` (-25308), which the error mapping
//! below surfaces as `NonInteractivePrompt` — mirroring the Linux backend's
//! `Error::Prompt → NonInteractivePrompt` refusal. The flag is process-global,
//! which matches its semantics: one daemon, one `KeychainConfig.allow_prompt`.
//!
//! Error mapping per A-W5.2 / D31:
//!   `errSecItemNotFound → NotFound`,
//!   `errSecAuthFailed | errSecUserCanceled → Denied`,
//!   `errSecInteractionNotAllowed → Unavailable{NonInteractivePrompt}`,
//!   `errSecNotAvailable → Unavailable`,
//!   other → `Internal{internal_hash}`.

use crate::{KeychainAccess, KeychainError, KeychainErrorKind};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::string::CFStringRef;
use security_framework::base::Error as SfError;
use security_framework::os::macos::keychain::{KeychainUserInteractionLock, SecKeychain};
use security_framework::passwords::{
    delete_generic_password_options, generic_password, set_generic_password_options,
    PasswordOptions,
};
use security_framework_sys::access_control::kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly;
use zeroize::Zeroizing;

// `security-framework-sys` 2.x binds the accessibility *values*
// (`kSecAttrAccessible*ThisDeviceOnly`, in its access_control module) but
// not the `kSecAttrAccessible` dictionary key itself; bind the key here.
// Security.framework is already linked by the sys crate.
#[allow(non_upper_case_globals)]
#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecAttrAccessible: CFStringRef;
}

/// Base query shared by all ops: the `(service, account)` identity pinned to
/// the non-iCloud store via `kSecAttrSynchronizable = false` (see module doc,
/// "Access" — legacy items without the attribute still match).
fn device_only_options(service_id: &str, label: &str) -> PasswordOptions {
    let mut opts = PasswordOptions::new_generic_password(service_id, label);
    opts.set_access_synchronized(Some(false));
    opts
}

pub struct MacOsKeychain {
    service_id: &'static str,
    allow_prompt: bool,
    /// Held while `allow_prompt == false`: suppresses Security-framework UI
    /// (see module doc, "Prompt gating"). `None` when prompts are allowed.
    _ui_lock: Option<KeychainUserInteractionLock>,
}

impl MacOsKeychain {
    pub fn new(service_id: &'static str, allow_prompt: bool) -> Result<Self, KeychainError> {
        // Non-interactive mode: refuse OS prompts for the backend's lifetime
        // so a headless daemon can never be blocked behind a GUI dialog —
        // would-be prompts surface as errSecInteractionNotAllowed (-25308)
        // → NonInteractivePrompt via map_sf_error.
        let ui_lock = if allow_prompt {
            None
        } else {
            Some(SecKeychain::disable_user_interaction().map_err(map_sf_error)?)
        };
        Ok(Self {
            service_id,
            allow_prompt,
            _ui_lock: ui_lock,
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
        match generic_password(device_only_options(self.service_id, label)) {
            Ok(bytes) => Ok(Zeroizing::new(bytes)),
            Err(e) => Err(map_sf_error(e)),
        }
    }

    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        let mut opts = device_only_options(self.service_id, label);
        // Stamp this-device-only-after-first-unlock accessibility on the
        // stored item. `PasswordOptions` has no setter for the key, so push
        // the pair onto its (deprecated-but-public) query Vec. On the upsert
        // arm (errSecDuplicateItem → SecItemUpdate) the pair rides in the
        // search query, where the file-based keychain ignores it — legacy
        // items therefore stay replaceable.
        #[allow(deprecated)]
        opts.query.push((
            unsafe { CFString::wrap_under_get_rule(kSecAttrAccessible) },
            unsafe {
                CFString::wrap_under_get_rule(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly)
            }
            .into_CFType(),
        ));
        match set_generic_password_options(&secret, opts) {
            Ok(()) => Ok(()),
            Err(e) => Err(map_sf_error(e)),
        }
    }

    fn delete_secret(&self, label: &str) -> Result<(), KeychainError> {
        match delete_generic_password_options(device_only_options(self.service_id, label)) {
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
