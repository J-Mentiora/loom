// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/LaunchdPlistWriter/interfaces.rs` instead.
// LaunchdPlistWriter — `cfg(target_os = "macos")`-gated plist
// emission.
//
// # Contract semantics
// - **Plist contents** (byte-equal):
//   - `<key>RunAtLoad</key><true/>`
//   - `<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>`
//   - `<key>ProgramArguments</key><array><string>{abs_loom}</string><string>serve</string></array>`
//   - `<key>Label</key><string>com.loom.daemon</string>`
// - **macOS-only.** `cfg(target_os = "macos")`. On other platforms
//   the symbol exists but `write` returns
//   `CliError::Internal("launchd plist is macOS-only")`.
// - **Idempotent.** `write` skips if the file exists with matching
//   `Label`. Mismatched `Label` is treated as a third-party plist —
//   `write` returns `CliError::Internal` rather than overwriting.

// macOS-only module; dead_code allowed on other platforms.

use std::path::PathBuf;

use crate::CliError;

/// Stable plist constants. Locked here so `interface_tests` can
/// byte-equal assert.
pub const PLIST_LABEL: &str = "com.loom.daemon";
pub const PLIST_RUN_AT_LOAD: bool = true;
pub const PLIST_KEEP_ALIVE_ON_SUCCESSFUL_EXIT: bool = false;

/// Configuration for the writer.
#[derive(Debug, Clone)]
pub struct LaunchdPlistConfig {
    /// Absolute path to the `loom` binary — first
    /// `ProgramArguments` entry.
    pub loom_binary: PathBuf,
    /// Plist destination
    /// (`/Library/LaunchDaemons/com.loom.daemon.plist`).
    pub plist_path: PathBuf,
}

/// Writer instance. Stateless beyond config.
pub struct LaunchdPlistWriter {
    pub(crate) config: LaunchdPlistConfig,
}

/// Outcome of `write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Plist already present with matching label.
    Skipped,
    /// Plist successfully written.
    Wrote,
}

impl LaunchdPlistWriter {
    pub fn new(config: LaunchdPlistConfig) -> Self {
        Self { config }
    }

    /// Write the plist. Idempotent — skip if file exists with
    /// matching `Label`. Returns `CliError::PermissionDenied` (not
    /// `CliError::Internal`) when any filesystem operation is denied due to
    /// insufficient privileges, so `postinstall_runner::plist_step` can
    /// degrade gracefully for non-root users.
    #[cfg(target_os = "macos")]
    pub fn write(&self) -> Result<WriteOutcome, CliError> {
        if self.config.plist_path.exists() {
            let content = std::fs::read_to_string(&self.config.plist_path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    CliError::PermissionDenied(format!(
                        "read plist: {}",
                        self.config.plist_path.display()
                    ))
                } else {
                    CliError::Internal(format!("read plist: {e}"))
                }
            })?;
            if content.contains(&format!("<string>{PLIST_LABEL}</string>")) {
                return Ok(WriteOutcome::Skipped);
            }
            return Err(CliError::Internal(
                "plist exists with different label — not overwriting".to_string(),
            ));
        }
        if let Some(parent) = self.config.plist_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    CliError::PermissionDenied(format!("create plist dir: {}", parent.display()))
                } else {
                    CliError::Internal(format!("create plist dir: {e}"))
                }
            })?;
        }
        std::fs::write(&self.config.plist_path, self.render()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                CliError::PermissionDenied(format!(
                    "write plist: {}",
                    self.config.plist_path.display()
                ))
            } else {
                CliError::Internal(format!("write plist: {e}"))
            }
        })?;
        Ok(WriteOutcome::Wrote)
    }

    /// Non-macOS stub. Returns `CliError::Internal`.
    #[cfg(not(target_os = "macos"))]
    pub fn write(&self) -> Result<WriteOutcome, CliError> {
        Err(CliError::Internal(
            "launchd plist is macOS-only".to_string(),
        ))
    }

    /// Pure helper: render the plist XML for a given config. Used by
    /// unit tests + the byte-equal assertion.
    pub fn render(&self) -> String {
        let loom_binary = self.config.loom_binary.to_string_lossy();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key><string>{PLIST_LABEL}</string>\n\
             \t<key>RunAtLoad</key><true/>\n\
             \t<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
             \t<key>ProgramArguments</key>\
             <array><string>{loom_binary}</string><string>serve</string></array>\n\
             </dict>\n\
             </plist>\n"
        )
    }
}
