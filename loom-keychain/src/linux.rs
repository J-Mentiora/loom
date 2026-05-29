//! Linux Secret Service backend — D-Bus `org.freedesktop.secrets`.
//!
//! Identity model (D11): `attributes["service"] = service_id` (default
//! `"loom"`), `attributes["account"] = label`. The `(service_id, account)`
//! pair is the unique key, mirroring macOS's `(kSecAttrService, kSecAttrAccount)`.
//! The display label given to the OS UI is `"<service_id>/<label>"`.
//!
//! Collection (D36): `Service::default_collection()` — the login keyring on
//! GNOME. No `LOOM_KEYCHAIN_COLLECTION` env-var; cross-DE (KDE kwallet) is
//! a documented v0.9.4 known gap, not a configurable.
//!
//! **D-Bus hijack mitigation (D24 + A-W3.1).** A malicious user-level process
//! can claim the well-known name `org.freedesktop.secrets` if the legitimate
//! daemon is killed. To prevent silent interception, we pin the bus name's
//! *unique* owner (e.g. `:1.42`) at construction via `org.freedesktop.DBus
//! GetNameOwner` and re-verify on every op. If the owner has changed mid-flight,
//! the op refuses with `Unavailable` and emits a structured `tracing::error!`
//! — operator must restart `loom-daemon` to re-pin.
//!
//! The original plan called for a `NameOwnerChanged` background watcher, but
//! that conflicts with the `secret-service` crate's sync `blocking::*` API
//! (a background task needs its own runtime). A-W3.1 replaces it with per-op
//! `GetNameOwner` re-check: cheaper, no shared connection state, and the
//! per-op overhead is one D-Bus round-trip.
//!
//! Per W3.8 `list_labels` does attribute-only search (`item.get_attributes()`),
//! NEVER `item.get_secret()` — so listing does not trigger an unlock prompt.
//!
//! Error mapping (D31 / A-W5.2):
//! - `secret_service::Error::NoResult` → `NotFound`
//! - `secret_service::Error::Locked` → `Denied` (we never trigger an unlock
//!   prompt from the daemon — operator must unlock the login keyring out of
//!   band before the daemon's first op)
//! - `secret_service::Error::Prompt(_)` → `NonInteractivePrompt`
//! - everything else (zbus transport, parse, crypto) → `Internal{hash}`
//!
//! `zbus` is a direct dependency (alongside `secret-service`'s transitive
//! pull) because the SS crate does not expose its internal connection;
//! the per-op `GetNameOwner` check uses its own small connection. Aligned
//! to zbus 5.x so cargo dedupes.

use crate::{KeychainAccess, KeychainError, KeychainErrorKind};
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;
use std::collections::HashMap;
use zeroize::Zeroizing;

/// Well-known D-Bus name of the FreeDesktop Secret Service.
const SECRETS_BUS_NAME: &str = "org.freedesktop.secrets";

pub struct LinuxKeychain {
    service_id: &'static str,
    #[allow(dead_code)]
    allow_prompt: bool,
    pinned_owner: String,
}

impl LinuxKeychain {
    /// Connect to `org.freedesktop.secrets`, resolve and pin its unique
    /// owner. Returns `Unavailable` if no process owns the bus name (the
    /// daemon isn't running or isn't reachable on this session bus).
    pub fn new(service_id: &'static str, allow_prompt: bool) -> Result<Self, KeychainError> {
        let pinned_owner = resolve_secrets_owner()?;
        tracing::info!(
            pinned_owner = %pinned_owner,
            service_id = %service_id,
            "LinuxKeychain initialised; pinned secret-service owner"
        );
        Ok(Self {
            service_id,
            allow_prompt,
            pinned_owner,
        })
    }

    pub fn service_id(&self) -> &'static str {
        self.service_id
    }

    pub fn allow_prompt(&self) -> bool {
        self.allow_prompt
    }

    /// Pinned D-Bus unique name (`:1.NN`) of the secret-service daemon at
    /// construction time. Exposed for diagnostics + tests.
    pub fn pinned_owner(&self) -> &str {
        &self.pinned_owner
    }

    /// A-W3.1 per-op verification: fresh `GetNameOwner` call; if the bus
    /// name's owner has drifted from the pinned value, refuse the op. The
    /// vault layer (W5) translates this `Unavailable` into a
    /// `secret_service_owner_changed` audit entry.
    fn verify_owner(&self) -> Result<(), KeychainError> {
        let current = resolve_secrets_owner()?;
        if current != self.pinned_owner {
            tracing::error!(
                pinned = %self.pinned_owner,
                current = %current,
                "secret_service_owner_changed; refusing op (restart loom-daemon to re-pin)"
            );
            return Err(KeychainError::new(
                KeychainErrorKind::Unavailable,
                format!(
                    "secret-service D-Bus owner changed (pinned={}, current={}); \
                     restart loom-daemon to re-pin",
                    self.pinned_owner, current
                ),
            ));
        }
        Ok(())
    }

    /// Open a SecretService connection + default collection, then run a
    /// closure against them. Each op gets a fresh connection — the per-op
    /// owner check is the cost; the connect itself is cheap on a local
    /// session bus.
    fn with_default_collection<F, T>(&self, f: F) -> Result<T, KeychainError>
    where
        F: FnOnce(
            &SecretService<'_>,
            &secret_service::blocking::Collection<'_>,
        ) -> Result<T, KeychainError>,
    {
        let svc = SecretService::connect(EncryptionType::Dh).map_err(map_ss_error)?;
        let collection = svc.get_default_collection().map_err(map_ss_error)?;
        f(&svc, &collection)
    }
}

/// Ask the session bus daemon `org.freedesktop.DBus.GetNameOwner` for the
/// current unique-name owner of `org.freedesktop.secrets`. On any failure
/// (no owner, transport error) return `Unavailable` so the daemon refuses
/// to start without a working keychain backend (D7 hard-fail-closed).
fn resolve_secrets_owner() -> Result<String, KeychainError> {
    let conn = zbus::blocking::Connection::session().map_err(|e| {
        KeychainError::new(
            KeychainErrorKind::Unavailable,
            format!("zbus session bus connect failed: {e}"),
        )
    })?;
    let proxy = zbus::blocking::fdo::DBusProxy::new(&conn).map_err(|e| {
        KeychainError::internal_from_message(format!("zbus DBusProxy construct failed: {e}"))
    })?;
    let bus_name = SECRETS_BUS_NAME.try_into().map_err(|e| {
        KeychainError::internal_from_message(format!("invalid bus-name literal: {e}"))
    })?;
    match proxy.get_name_owner(bus_name) {
        Ok(owner) => Ok(owner.to_string()),
        Err(e) => Err(KeychainError::new(
            KeychainErrorKind::Unavailable,
            format!(
                "secret-service D-Bus name '{SECRETS_BUS_NAME}' has no owner ({e}); \
                 start gnome-keyring or another Secret Service provider, then retry"
            ),
        )),
    }
}

/// Map a `secret_service::Error` to our typed `KeychainError`.
///
/// Catch-all bucket (`Internal`) carries a SHA-256 of the original message
/// so support can correlate audits to daemon logs without the third-party
/// error string ever reaching the persistent manifest (D31 / A-W6.3).
fn map_ss_error(err: secret_service::Error) -> KeychainError {
    use secret_service::Error;
    match err {
        Error::NoResult => KeychainError::new(
            KeychainErrorKind::NotFound,
            "no matching item in secret-service",
        ),
        Error::Locked => KeychainError::new(
            KeychainErrorKind::Denied,
            "secret-service collection is locked; unlock the login keyring \
             via the OS UI before running loom-daemon",
        ),
        Error::Prompt => KeychainError::new(
            KeychainErrorKind::NonInteractivePrompt,
            "secret-service would prompt to unlock; loom-daemon does not trigger \
             interactive prompts (set LOOM_KEYCHAIN_ALLOW_PROMPT=1 only in a \
             genuinely interactive context)",
        ),
        other => KeychainError::internal_from_message(other.to_string()),
    }
}

impl KeychainAccess for LinuxKeychain {
    fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
        self.verify_owner()?;
        let service_id = self.service_id;
        self.with_default_collection(|_svc, collection| {
            let attrs = HashMap::from([("service", service_id), ("account", label)]);
            let items = collection.search_items(attrs).map_err(map_ss_error)?;
            match items.len() {
                0 => Err(KeychainError::new(
                    KeychainErrorKind::NotFound,
                    format!("no entry for label='{label}' in service='{service_id}'"),
                )),
                1 => {
                    let bytes = items[0].get_secret().map_err(map_ss_error)?;
                    Ok(Zeroizing::new(bytes))
                }
                n => Err(KeychainError::internal_from_message(format!(
                    "duplicate_account: {n} items match service={service_id}, account={label}"
                ))),
            }
        })
    }

    fn set_secret(&self, label: &str, secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
        self.verify_owner()?;
        let service_id = self.service_id;
        self.with_default_collection(|_svc, collection| {
            let attrs = HashMap::from([("service", service_id), ("account", label)]);
            let display_label = format!("{service_id}/{label}");
            collection
                .create_item(
                    &display_label,
                    attrs,
                    &secret,
                    true, // replace existing on (service, account) match
                    "application/octet-stream",
                )
                .map_err(map_ss_error)?;
            Ok(())
        })
    }

    fn delete_secret(&self, label: &str) -> Result<(), KeychainError> {
        self.verify_owner()?;
        let service_id = self.service_id;
        self.with_default_collection(|_svc, collection| {
            let attrs = HashMap::from([("service", service_id), ("account", label)]);
            let items = collection.search_items(attrs).map_err(map_ss_error)?;
            // Idempotent: zero matches → Ok (silent). Multiple matches → delete
            // all (cleans up any duplicate_account state left by a prior bug).
            for item in items {
                item.delete().map_err(map_ss_error)?;
            }
            Ok(())
        })
    }

    fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
        self.verify_owner()?;
        let service_id = self.service_id;
        self.with_default_collection(|_svc, collection| {
            let attrs = HashMap::from([("service", service_id)]);
            let items = collection.search_items(attrs).map_err(map_ss_error)?;
            let mut labels = Vec::with_capacity(items.len());
            for item in items {
                // W3.8: attribute-only read; NEVER get_secret() in list path.
                // Attribute lookup does not require collection unlock.
                let item_attrs = item.get_attributes().map_err(map_ss_error)?;
                if let Some(account) = item_attrs.get("account") {
                    labels.push(account.clone());
                }
            }
            Ok(labels)
        })
    }
}
