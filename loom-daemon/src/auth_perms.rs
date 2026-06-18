//! Auth-file permission helpers + vault-label wire-boundary validation.
//!
//! Split out of `lib.rs` (large-file refactor), unchanged:
//!   * `probe_auth_perms_or_refuse` / `write_auth_file_0600` — the
//!     `cfg(unix)` / `cfg(not(unix))` pairs that enforce the A-W8.1 0600
//!     startup-perms contract on `hello.token` / `daemon.pid`.
//!   * `validate_label_canonical` / `validate_label_or_rpc_err` — the D37
//!     canonical vault-label policy enforced at the wire boundary (the
//!     latter is re-exported from the crate root and consumed by the
//!     `vault_bridge` submodule via `crate::validate_label_or_rpc_err`).

use crate::map_loom_error;
use anyhow::{Context, Result};
use loom_core::error::LoomError;
use loom_rpc::core_service_adapter::core_service_adapter::AdapterError;

// ─── A-W8.1 / W8.5 auth-file permission helpers ────────────────────────────

/// Refuse to start when an existing auth file has loose perms
/// (any of `g+r g+w g+x o+r o+w o+x` set). On a fresh install the file
/// doesn't exist yet → no-op. On Unix only; Windows ACLs are out of
/// scope for v0.9.4.
#[cfg(unix)]
pub(crate) fn probe_auth_perms_or_refuse(path: &std::path::Path, what: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "auth-file stat failed for {} at {}: {}",
                what,
                path.display(),
                e
            ));
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::error!(
            path = %path.display(),
            mode = format!("{:04o}", mode),
            "auth file has loose permissions; refusing to start"
        );
        anyhow::bail!(
            "{} at {} has mode {:04o} (group/world bits set); \
             expected 0600. Run `chmod 600 {}` and restart.",
            what,
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn probe_auth_perms_or_refuse(_path: &std::path::Path, _what: &str) -> Result<()> {
    // Windows ACLs aren't a chmod analogue; v0.9.4 leaves the probe a
    // no-op there. A follow-up that uses `windows::Win32::Security` can
    // implement a similar refuse-on-non-private-DACL check.
    Ok(())
}

/// Write an auth artefact CREATED with mode 0600 (atomically, via
/// `OpenOptions::mode` — the mode applies at `open(O_CREAT)` time, before
/// any byte lands). The old write-then-chmod sequence left a transient
/// umask-mode (typically 0644) window in which any local user could open
/// the daemon's bearer token and keep the fd past the chmod. `mode` only
/// applies to newly-created files; a pre-existing file already passed
/// `probe_auth_perms_or_refuse` (no group/world bits), so truncate+rewrite
/// preserves its ≤0600 mode.
/// Unix only; Windows uses ACLs and is out of scope for v0.9.4.
#[cfg(unix)]
pub(crate) fn write_auth_file_0600(
    path: &std::path::Path,
    contents: &[u8],
    what: &str,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| {
            format!(
                "create {} (mode 0600) at {}; required by the A-W8.1 startup-perms contract",
                what,
                path.display()
            )
        })?;
    f.write_all(contents)
        .with_context(|| format!("write {} to {}", what, path.display()))
}

#[cfg(not(unix))]
pub(crate) fn write_auth_file_0600(
    path: &std::path::Path,
    contents: &[u8],
    what: &str,
) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {} to {}", what, path.display()))
}

// ─── Vault W6 wire-boundary helpers ────────────────────────────────────────

/// D37 canonical label policy enforced at the wire boundary. The
/// `manifest_writer::append_audit` gate (W5.10 / A-W8.5) catches the
/// same shape as belt-and-suspenders if a future code path bypasses
/// this check.
pub(crate) fn validate_label_canonical(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("label is empty".into());
    }
    if label.len() > 64 {
        return Err(format!("label exceeds 64 chars ({} chars)", label.len()));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
    {
        return Err(format!(
            "label {label:?} fails canonical validation ^[A-Za-z0-9:_-]{{1,64}}$"
        ));
    }
    Ok(())
}

/// Validate a vault label at the wire boundary and map any rejection to the RPC
/// adapter error (`InvalidArgument`). Shared by `vault_set_secret` /
/// `vault_delete_secret` (D37) so the error mapping lives in one place.
// pub(crate): shared with the vault_bridge submodule (large-file split).
pub(crate) fn validate_label_or_rpc_err(label: &str) -> Result<(), AdapterError> {
    validate_label_canonical(label).map_err(|e| {
        map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            e,
        ))
    })
}
