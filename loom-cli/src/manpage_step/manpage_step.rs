//! Postinstall step that writes the embedded man pages to a man-path
//! directory so `man loom` works after `loom postinstall`. Hardened
//! against permission errors, read-only filesystems, disk-full, missing
//! payloads, and Windows (where loom doesn't ship anyway).
//!
//! Resolution chain for `man_install_dir`:
//!
//! 1. `$LOOM_MAN_DIR` (tests / explicit override).
//! 2. `$PREFIX/share/man` if `$PREFIX` is set (cargo-install convention).
//! 3. `~/.local/share/man` (XDG default).
//!
//! All filesystem errors are downgraded to a soft warning + `Skipped`
//! per Council finding "Postinstall fails on disk-full" — the binary is
//! still functional without man pages, so we never fail the chain.

use std::io;
use std::path::{Path, PathBuf};

use crate::postinstall_runner::StepOutcome;
use crate::CliError;

/// Embedded man-page payloads. `build.rs` writes empty bytes when the
/// source is missing; we treat empty as "skip this file" rather than
/// writing 0-byte stubs that would confuse `man`.
const EMBEDDED_LOOM_1: &[u8] = include_bytes!(env!("LOOM_CLI_EMBEDDED_MANPAGE_LOOM"));
const EMBEDDED_LOOM_ACTION_1: &[u8] = include_bytes!(env!("LOOM_CLI_EMBEDDED_MANPAGE_ACTION"));

#[derive(Debug, Clone, Copy)]
struct ManFile {
    /// Filename written under `<dir>/man1/`, e.g. `loom.1`.
    name: &'static str,
    /// Embedded payload. Empty means "not generated yet — skip".
    bytes: &'static [u8],
}

const MAN_FILES: &[ManFile] = &[
    ManFile {
        name: "loom.1",
        bytes: EMBEDDED_LOOM_1,
    },
    ManFile {
        name: "loom-action.1",
        bytes: EMBEDDED_LOOM_ACTION_1,
    },
];

/// Resolve the install dir from env. `None` means "no writable target found —
/// step should be a no-op". Caller logs a soft warning.
pub fn resolve_install_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return None;
    }
    if let Ok(explicit) = std::env::var("LOOM_MAN_DIR") {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    if let Ok(prefix) = std::env::var("PREFIX") {
        if !prefix.is_empty() {
            return Some(PathBuf::from(prefix).join("share/man"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".local/share/man"));
    }
    None
}

/// Write the embedded man pages to `<dir>/man1/<name>`.
///
/// Idempotent: if the target file already exists with byte-identical content,
/// the file is left alone. Atomic: each file is written via a `<target>.tmp`
/// then `rename` so a crash partway through can't leave a half-written file.
///
/// All filesystem errors are downgraded to a non-fatal warning logged via
/// `eprintln!` and the step returns `Skipped`. The postinstall chain
/// continues regardless — man pages are an enhancement, not a requirement.
pub fn manpage_step(opts_dir: Option<&Path>) -> Result<StepOutcome, CliError> {
    if cfg!(target_os = "windows") {
        eprintln!(
            "note: man page install is a no-op on Windows; loom uses Unix man path conventions"
        );
        return Ok(StepOutcome::Skipped);
    }

    let dir = match opts_dir {
        Some(d) => d.to_path_buf(),
        None => match resolve_install_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "note: no writable man dir found; man pages embedded but not installed. \
                     Set $LOOM_MAN_DIR or $PREFIX, or copy the embedded pages manually."
                );
                return Ok(StepOutcome::Skipped);
            }
        },
    };

    let man1 = dir.join("man1");
    if let Err(e) = std::fs::create_dir_all(&man1) {
        soft_warn_and_skip(&man1, &e);
        return Ok(StepOutcome::Skipped);
    }

    let mut wrote_any = false;
    let mut all_empty = true;
    for f in MAN_FILES {
        if f.bytes.is_empty() {
            eprintln!(
                "note: man page {} not embedded; run `cargo run --example gen-docs -p loom-cli` and rebuild",
                f.name
            );
            continue;
        }
        all_empty = false;
        let target = man1.join(f.name);
        match write_atomic_if_changed(&target, f.bytes) {
            Ok(true) => wrote_any = true,
            Ok(false) => { /* unchanged — idempotent skip */ }
            Err(e) => {
                soft_warn_and_skip(&target, &e);
                return Ok(StepOutcome::Skipped);
            }
        }
    }

    if all_empty {
        Ok(StepOutcome::Skipped)
    } else if wrote_any {
        Ok(StepOutcome::Wrote)
    } else {
        Ok(StepOutcome::Skipped)
    }
}

/// Atomically write `bytes` to `target` if the existing content differs (or
/// the file doesn't exist). Returns `Ok(true)` if it wrote, `Ok(false)` if
/// the file already matched.
///
/// The temp filename includes the process id so two concurrent `loom
/// postinstall` invocations don't write to the same `.tmp` and clobber each
/// other before the atomic rename (per code-council finding on race
/// conditions). The rename itself is atomic on POSIX.
fn write_atomic_if_changed(target: &Path, bytes: &[u8]) -> io::Result<bool> {
    if let Ok(existing) = std::fs::read(target) {
        if existing == bytes {
            return Ok(false);
        }
    }
    let pid = std::process::id();
    let tmp = target.with_extension(
        target
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| format!("{e}.tmp.{pid}"))
            .unwrap_or_else(|| format!("tmp.{pid}")),
    );
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, target)?;
    Ok(true)
}

/// Log a non-fatal warning that the man-page install was skipped. The
/// message intentionally avoids leaking absolute paths from `target` —
/// only the basename or a generic phrase. Per security council finding 1.
fn soft_warn_and_skip(target: &Path, err: &io::Error) {
    let basename = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("man page");
    eprintln!(
        "note: man page install skipped ({basename}): {kind}; \
         pass $LOOM_MAN_DIR=<writable path> to install elsewhere",
        kind = err.kind()
    );
}

/// Whether `manpage_step` would actually have content to install. Used by
/// `loom doctor` to check whether the user is missing man pages because
/// (a) they haven't run postinstall, vs (b) the build has empty embedded
/// payloads (still useful to tell them to regenerate).
pub fn has_embedded_content() -> bool {
    MAN_FILES.iter().any(|f| !f.bytes.is_empty())
}

/// Whether the target dir has the man pages already. Used by `loom doctor`
/// to decide whether to soft-warn that `man loom` won't work.
pub fn man_pages_installed_at(dir: &Path) -> bool {
    MAN_FILES.iter().all(|f| {
        if f.bytes.is_empty() {
            return true; // nothing to check
        }
        let target = dir.join("man1").join(f.name);
        std::fs::read(&target)
            .map(|c| c == f.bytes)
            .unwrap_or(false)
    })
}
