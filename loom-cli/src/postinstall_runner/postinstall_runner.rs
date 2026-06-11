// PostinstallRunner — `loom postinstall` orchestrator.
//
// # Contract semantics
// - **Idempotent installer.** Four steps, each with its
//   own idempotence guard:
//   1. **Compile WASM modules.** Calls
//      `loom-host::WasmHost::compile_module(source, dest)` for each
//      surface module. Skip if `.cwasm` source-sha xattr matches.
//   2. **Schemas dir.** Creates `~/.config/loom/schemas/v1/` and
//      emits built-in JSON schemas. Skipped if dir already populated.
//   3. **Chromium download + sha256 verify.** Skip if binary present
//      and sha256 matches pinned. Mismatch → `SupplyChainViolation`.
//   4. **macOS launchd plist.** Skip if file present with matching
//      `Label`.
// - **Pre-warm.** Chromium pre-warmed in step 3 — first
//   `loom action` reuses the cached binary; cold-Chromium spawn on
//   first action is structurally impossible (binary path is absent →
//   `DoctorRunner` fails before action dispatches).
// - **Structural isolation.** `compile_module` is unreachable from
//   the action dispatch path. The postinstall cargo feature gates
//   loom-host linkage (default = ["postinstall"]).
//   This is preserved by code structure: compile_step is only
//   called from Command::Postinstall; the action → RPC dispatch path
//   has no edge into compile_step or compile_module.

use std::path::PathBuf;

use crate::chromium_downloader::ChromiumDownloader;
use crate::launchd_plist_writer::LaunchdPlistWriter;
use crate::CliError;

/// `loom postinstall` resolved options.
#[derive(Debug, Clone)]
pub struct PostinstallOptions {
    /// Surfaces directory containing `.wasm` source files. Default:
    /// `~/Library/Application Support/loom/surfaces/`.
    pub surfaces_dir: PathBuf,
    /// Schemas directory for JSON schema files.
    /// Default: `~/.config/loom/schemas/v1/`.
    pub schemas_dir: PathBuf,
    /// Pinned Chromium download URL.
    pub chromium_url: String,
    /// Expected sha256 of the Chromium archive (lowercase hex).
    pub chromium_expected_sha256: String,
    /// Chromium install root.
    pub chromium_dir: PathBuf,
    /// macOS plist destination
    /// (`/Library/LaunchDaemons/com.loom.daemon.plist`).
    pub plist_path: PathBuf,
    /// Workspace crate version, used to construct the GH
    /// Release URL `releases/download/v{version}/dist-manifest.json`.
    /// Typically `env!("CARGO_PKG_VERSION")` from the caller.
    pub loom_binaries_version: String,
    /// Rustc-style host triple for tarball selection
    /// (e.g. `"aarch64-apple-darwin"`).
    pub loom_binaries_target_triple: String,
    /// Directory the 3 sibling binaries are extracted to.
    /// Default: `dirs::data_local_dir().join("loom/bin/")`.
    pub loom_binaries_install_dir: PathBuf,
    /// `--skip-chromium`: bypass step 3.
    pub skip_chromium: bool,
    /// `--skip-binaries`: bypass the loom-binaries download step.
    pub skip_binaries: bool,
    /// Optional override for the man-page install dir; falls back to
    /// `$LOOM_MAN_DIR` / `$PREFIX/share/man` / `~/.local/share/man`.
    pub man_install_dir: Option<PathBuf>,
}

/// Per-step outcome — used for the final stdout receipt. Unit variants
/// serialise as bare strings (`"skipped"`); `Compiled` carries the
/// `.cwasm` destination path (`{"compiled": "<path>"}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Skipped,
    Compiled(PathBuf),
    Downloaded,
    Wrote,
}

/// Outcome for the schema emission step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaStepOutcome {
    /// Schemas dir was created and populated with `count` method schema files.
    Populated(usize),
    /// At least one existing schema file was stale (content differed from the
    /// builtin) and was refreshed; `populated` counts newly written files.
    Refreshed { populated: usize, refreshed: usize },
    /// Schemas dir already existed and every file matched the builtins — skipped.
    Skipped,
}

/// Final receipt for `loom postinstall`. Emitted as canonical JSON on
/// stdout by `CommandRouter` (same `OutputFormatter` path as `doctor`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostinstallReceipt {
    pub status: String,
    pub steps: Vec<&'static str>,
    pub compile_outcomes: Vec<StepOutcome>,
    pub schemas: SchemaStepOutcome,
    pub chromium: StepOutcome,
    /// Outcome of the loom-binaries download step. `None` when
    /// the step was skipped via `--skip-binaries`.
    pub loom_binaries: Option<StepOutcome>,
    pub launchd: Option<StepOutcome>,
    pub manpages: StepOutcome,
}

/// Decide whether to warn that this `loom` was installed from a non-release
/// commit (AC6 / R6).
///
/// `release_marker` is the `build.rs`-stamped `LOOM_RELEASE_BUILD`:
/// - `"1"` — HEAD was on a `v*` release tag → a real release build.
/// - `"0"` — git present but off-tag → a non-release commit (the tagless
///   `cargo install --git ... loom-cli` path).
/// - `"unknown"` — no `.git` at build time (cargo-dist source tarball) → treated
///   as a release install; we do NOT warn (avoids false positives on legitimate
///   tarball releases).
///
/// Also warns when `version` carries a SemVer pre-release segment (e.g.
/// `0.10.0-dev`), so the warning still fires under a future `-dev`-on-`main`
/// convention even on builds without git.
///
/// The returned message intentionally contains the stable substrings
/// `non-release` and `--tag` (FND-0001) so a test can lock the contract without
/// pinning the full wording.
pub fn tagless_install_warning(version: &str, release_marker: &str) -> Option<String> {
    let pre_release_version = version.contains('-');
    let non_release_commit = release_marker == "0";
    if !(pre_release_version || non_release_commit) {
        return None;
    }
    Some(format!(
        "warning: this `loom` ({version}) was installed from a non-release commit. \
         Installs from a moving branch are not reproducible — pin a tagged release by \
         passing `--tag vX.Y.Z`, e.g. \
         `cargo install --git https://github.com/mentiora-ai/loom --tag vX.Y.Z loom-cli`."
    ))
}

/// Runs the full postinstall pipeline. Idempotent — safe to re-run.
pub async fn run(opts: PostinstallOptions) -> Result<PostinstallReceipt, CliError> {
    // Up front, before any work: warn if this binary came from a non-release
    // commit (tagless `cargo install --git`). Advisory only — stderr, never
    // blocks the install (AC6 / R6).
    if let Some(warning) =
        tagless_install_warning(env!("CARGO_PKG_VERSION"), env!("LOOM_RELEASE_BUILD"))
    {
        eprintln!("{warning}");
    }

    let compile_outcomes = compile_step(&opts.surfaces_dir)?;

    let schemas = schema_step(&opts.schemas_dir)?;

    let chromium = if opts.skip_chromium {
        StepOutcome::Skipped
    } else {
        // Per-platform layout of the extracted Chromium archive (macOS →
        // `chrome-mac/Chromium.app/...`, Linux → `chrome-linux/chrome`).
        // `binary_subpath` is consulted by `ChromiumDownloader::ensure`'s
        // idempotency check (binary present + sentinel matches → skip).
        // A wrong path here makes every postinstall re-extract the zip,
        // which fails on cache-restored read-only files on macOS CI runners.
        // Shared with `loom doctor` + the launch resolver via
        // `chromium_binary_subpath()` so the three cannot disagree.
        let binary_subpath = loom_shared::chromium_resolver::chromium_binary_subpath();

        let downloader =
            ChromiumDownloader::new(crate::chromium_downloader::ChromiumDownloaderConfig {
                install_dir: opts.chromium_dir.clone(),
                binary_subpath,
            });
        chromium_step(
            &downloader,
            &opts.chromium_url,
            &opts.chromium_expected_sha256,
        )
        .await?
    };

    let loom_binaries = if opts.skip_binaries {
        None
    } else {
        Some(
            loom_binaries_step(
                &opts.loom_binaries_version,
                &opts.loom_binaries_target_triple,
                &opts.loom_binaries_install_dir,
            )
            .await?,
        )
    };

    let writer = LaunchdPlistWriter::new(crate::launchd_plist_writer::LaunchdPlistConfig {
        loom_binary: std::env::current_exe().unwrap_or_default(),
        plist_path: opts.plist_path.clone(),
    });
    let launchd = Some(plist_step(&writer)?);

    // Install generated man pages so `man loom` works after
    // postinstall. Soft-failure on permission / disk errors so the rest of
    // the chain isn't blocked by a docs enhancement.
    let manpages = crate::manpage_step::manpage_step(opts.man_install_dir.as_deref())?;

    Ok(PostinstallReceipt {
        status: "ok".to_string(),
        steps: STEP_LABELS.to_vec(),
        compile_outcomes,
        schemas,
        chromium,
        loom_binaries,
        launchd,
        manpages,
    })
}

// Embedded WASM surface bytes compiled into the binary by build.rs.
// Used as the fallback when LOOM_WASM_DIR is not set and the Cargo convention
// target path does not exist (i.e., standard end-user install with no dev env).
#[cfg(feature = "postinstall")]
const EMBEDDED_SURFACE_WEB: &[u8] = include_bytes!(env!("LOOM_CLI_EMBEDDED_SURFACE_WEB"));

/// Compile WASM surface modules to `.cwasm` in `surfaces_dir`.
///
/// WASM source discovery order:
/// 1. `LOOM_WASM_DIR` environment variable (tests / CI).
/// 2. `$CARGO_MANIFEST_DIR/../target/wasm32-wasip2/release/` (dev builds).
/// 3. Embedded bytes compiled into the binary via `build.rs` (end-user install).
///
/// Each source is AOT-compiled via `loom_host::compiler::Compiler`. A `.sha256`
/// sidecar guards idempotence: if the sidecar matches the source
/// hash the surface is skipped.
///
/// Structural guard: `compile_module` is NOT reachable from the action dispatch
/// path. It is only called here, from `Command::Postinstall`. The action →
/// RPC path loads pre-compiled `.cwasm` files via `ModuleLibrary::load_all`.
#[cfg(feature = "postinstall")]
pub fn compile_step(surfaces_dir: &std::path::Path) -> Result<Vec<StepOutcome>, CliError> {
    use loom_host::compiler::Compiler;
    use loom_host::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};

    // Discover WASM sources. If paths 1 and 2 are empty, fall back to
    // embedded bytes (path 3).
    let mut embedded_tmp: Option<std::path::PathBuf> = None;
    let wasm_sources = {
        let disk_sources = discover_wasm_sources()?;
        if disk_sources.is_empty() {
            // Path 3: extract embedded WASM bytes to a stable temp location.
            let tmp_dir = std::env::temp_dir()
                .join(format!("loom-postinstall-surfaces-{}", std::process::id()));
            std::fs::create_dir_all(&tmp_dir).map_err(|e| CliError::Internal(e.to_string()))?;
            let wasm_path = tmp_dir.join("loom_surface_web.wasm");
            std::fs::write(&wasm_path, EMBEDDED_SURFACE_WEB)
                .map_err(|e| CliError::Internal(e.to_string()))?;
            embedded_tmp = Some(tmp_dir);
            vec![("loom_surface_web".to_owned(), wasm_path)]
        } else {
            disk_sources
        }
    };

    std::fs::create_dir_all(surfaces_dir).map_err(|e| CliError::Internal(e.to_string()))?;

    let runtime = WasmRuntime::new(WasmRuntimeConfig::default())
        .map_err(|e| CliError::Internal(e.message))?;
    let compiler = Compiler::new(runtime);

    let mut outcomes = Vec::new();
    for (name, wasm_path) in wasm_sources {
        let dest = surfaces_dir.join(format!("{}.cwasm", name));
        let sidecar = surfaces_dir.join(format!("{}.sha256", name));

        if is_up_to_date(&wasm_path, &sidecar)? {
            outcomes.push(StepOutcome::Skipped);
            continue;
        }

        compiler
            .compile_module(&wasm_path, &dest)
            .map_err(|e| CliError::Internal(e.message))?;

        let sha = sha256_file(&wasm_path)?;
        std::fs::write(&sidecar, sha.as_bytes()).map_err(|e| CliError::Internal(e.to_string()))?;

        outcomes.push(StepOutcome::Compiled(dest));
    }

    // Clean up the embedded temp dir (best-effort; OS will clean eventually).
    if let Some(tmp) = embedded_tmp {
        let _ = std::fs::remove_dir_all(tmp);
    }

    Ok(outcomes)
}

/// Non-postinstall stub — compile_step is a no-op when loom-host is not
/// linked. This code path is only reached with `--no-default-features`;
/// the shipped binary always includes the postinstall feature (default = ["postinstall"]).
#[cfg(not(feature = "postinstall"))]
pub fn compile_step(surfaces_dir: &std::path::Path) -> Result<Vec<StepOutcome>, CliError> {
    if !surfaces_dir.exists() {
        return Ok(vec![]);
    }
    Ok(vec![StepOutcome::Skipped])
}

// ---------------------------------------------------------------------------
// Helpers (compile_step internals — postinstall feature only)
// ---------------------------------------------------------------------------

#[cfg(feature = "postinstall")]
/// Enumerate `.wasm` source files for AOT compilation.
///
/// Search order:
/// 1. `LOOM_WASM_DIR` environment variable (set in tests / CI).
/// 2. Convention path: `$CARGO_MANIFEST_DIR/../target/wasm32-wasip2/release/`.
///
/// Returns `(stem_name, absolute_path)` pairs.
fn discover_wasm_sources() -> Result<Vec<(String, std::path::PathBuf)>, CliError> {
    let wasm_dir = if let Ok(dir) = std::env::var("LOOM_WASM_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        match std::path::Path::new(&manifest_dir).parent() {
            Some(parent) => parent.join("target/wasm32-wasip2/release"),
            None => return Ok(vec![]),
        }
    };

    if !wasm_dir.exists() {
        return Ok(vec![]);
    }

    let mut sources = Vec::new();
    for entry in std::fs::read_dir(&wasm_dir)
        .map_err(|e| CliError::Internal(e.to_string()))?
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if !stem.is_empty() {
                sources.push((stem, path));
            }
        }
    }
    Ok(sources)
}

#[cfg(feature = "postinstall")]
/// Returns `true` if the `.sha256` sidecar exists and matches the current
/// SHA-256 of `wasm_path` — the compile_step idempotence guard.
fn is_up_to_date(wasm_path: &std::path::Path, sidecar: &std::path::Path) -> Result<bool, CliError> {
    if !sidecar.exists() {
        return Ok(false);
    }
    let recorded =
        std::fs::read_to_string(sidecar).map_err(|e| CliError::Internal(e.to_string()))?;
    let current = sha256_file(wasm_path)?;
    Ok(current == recorded.trim())
}

#[cfg(feature = "postinstall")]
/// SHA-256 hex digest of a file's bytes.
fn sha256_file(path: &std::path::Path) -> Result<String, CliError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| CliError::Internal(e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    // sha2 0.11: `finalize()` returns `digest::Array<u8, _>` (no `LowerHex`
    // impl); 0.10 returned `GenericArray<u8, _>` which did. `hex::encode`
    // works for both.
    Ok(hex::encode(hasher.finalize()))
}

pub async fn chromium_step(
    downloader: &ChromiumDownloader,
    url: &str,
    expected_sha256: &str,
) -> Result<StepOutcome, CliError> {
    match downloader.ensure(url, expected_sha256).await? {
        crate::chromium_downloader::DownloadOutcome::Skipped => Ok(StepOutcome::Skipped),
        crate::chromium_downloader::DownloadOutcome::Downloaded(_p) => Ok(StepOutcome::Downloaded),
    }
}

/// Inline-postinstall pre-flight for `loom serve` / first `loom session create`
/// (PRD R5 / AC5).
///
/// When no Chromium can be resolved (env override → pinned dir → PATH →
/// `/Applications`), this downloads the pinned build inline with visible
/// stderr progress so a brand-new user reaches a working browser without a
/// separate `loom postinstall` round-trip. When Chromium is already present —
/// the common case, and what every daemon e2e hits via `LOOM_CHROMIUM_PATH` —
/// it is a cheap no-op.
///
/// Gating (so this never surprises automation):
/// - **Already resolvable** → no-op (also keeps CI/tests from ever fetching).
/// - **`LOOM_NO_INLINE_CHROMIUM` set** → never fetch; print the `loom
///   postinstall` remedy and continue.
/// - **Non-interactive stderr** (no TTY) and not forced → print the remedy and
///   continue, rather than triggering a surprise ~150 MB download in a script
///   or CI job. Set `LOOM_INLINE_CHROMIUM=1` to force the inline fetch.
///
/// Non-blocking by design: a failed inline fetch prints a precise remedy and
/// returns `Ok(())` so daemon startup / session creation still proceeds (the
/// daemon's `loom doctor` path gives the actionable missing-binary error at
/// first action). The explicit `loom postinstall` command remains the path
/// that hard-fails on a supply-chain mismatch.
pub async fn ensure_chromium_inline(
    chromium_dir: &std::path::Path,
    url: &str,
    expected_sha256: &str,
) -> Result<(), CliError> {
    use std::io::IsTerminal as _;

    // Already resolvable anywhere on the standard search path → nothing to do.
    if loom_shared::chromium_resolver::resolve_chromium(chromium_dir).is_ok() {
        return Ok(());
    }

    // Hard opt-out: never auto-fetch.
    if std::env::var_os("LOOM_NO_INLINE_CHROMIUM").is_some() {
        eprintln!(
            "Chromium is not installed. Run `loom postinstall` to download the pinned build."
        );
        return Ok(());
    }

    // Only auto-fetch when interactive or explicitly forced.
    let forced = std::env::var_os("LOOM_INLINE_CHROMIUM").is_some();
    if !forced && !std::io::stderr().is_terminal() {
        eprintln!(
            "Chromium is not installed. Run `loom postinstall` to download the pinned build \
             (or set LOOM_INLINE_CHROMIUM=1 to fetch it inline)."
        );
        return Ok(());
    }

    eprintln!("Chromium is not installed; downloading the pinned build inline…");
    let binary_subpath = loom_shared::chromium_resolver::chromium_binary_subpath();
    let downloader =
        ChromiumDownloader::new(crate::chromium_downloader::ChromiumDownloaderConfig {
            install_dir: chromium_dir.to_path_buf(),
            binary_subpath,
        });
    let mut reporter =
        crate::chromium_downloader::StderrProgressReporter::new("Downloading Chromium…");
    let outcome = downloader
        .ensure_with_progress(url, expected_sha256, &mut reporter)
        .await;
    reporter.finish();
    match outcome {
        Ok(_) => {
            eprintln!("Chromium ready.");
            Ok(())
        }
        Err(e) => {
            eprintln!("Inline Chromium download failed: {e}. Run `loom postinstall` to retry.");
            Ok(())
        }
    }
}

/// Download + extract `loom-daemon`, `loom-mcp`,
/// `loom-shim-chromium` from the GH Release tagged `v{version}`. Skips
/// when the 3 siblings are already co-located next to the running `loom`
/// binary (brew/manual install path) — only the cargo-install path
/// actually fetches.
pub async fn loom_binaries_step(
    version: &str,
    target_triple: &str,
    install_dir: &std::path::Path,
) -> Result<StepOutcome, CliError> {
    use crate::loom_binaries_downloader::{ensure, AUX_BINARY_NAMES};

    // Detect: brew/manual already co-locate all 4 binaries next to `loom`.
    if let Some(exe_parent) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        if AUX_BINARY_NAMES
            .iter()
            .all(|n| exe_parent.join(n).is_file())
        {
            return Ok(StepOutcome::Skipped);
        }
    }

    match ensure(version, target_triple, install_dir).await? {
        crate::loom_binaries_downloader::DownloadOutcome::Skipped => Ok(StepOutcome::Skipped),
        crate::loom_binaries_downloader::DownloadOutcome::Downloaded => Ok(StepOutcome::Downloaded),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn plist_step(_writer: &LaunchdPlistWriter) -> Result<StepOutcome, CliError> {
    // launchd is macOS-only; on every other platform `loom postinstall` skips
    // this step rather than surfacing the writer-stub's
    // "launchd plist is macOS-only" Internal error to the user.
    Ok(StepOutcome::Skipped)
}

#[cfg(target_os = "macos")]
pub fn plist_step(writer: &LaunchdPlistWriter) -> Result<StepOutcome, CliError> {
    // Non-root users get PermissionDenied from the writer;
    // degrade gracefully to Skipped rather than hard-failing postinstall.
    match writer.write() {
        Ok(crate::launchd_plist_writer::WriteOutcome::Skipped) => Ok(StepOutcome::Skipped),
        Ok(crate::launchd_plist_writer::WriteOutcome::Wrote) => Ok(StepOutcome::Wrote),
        Err(CliError::PermissionDenied(_)) => Ok(StepOutcome::Skipped),
        Err(e) => Err(e),
    }
}

/// Create and populate the schemas directory with JSON schemas for all known
/// loom action methods.
///
/// Per-method idempotence (v0.9.6) writes any missing schema file; content
/// refresh (v0.11.1) additionally overwrites a file whose content no longer
/// matches the builtin. The old "leave existing ones alone" rule froze a
/// pre-settle-capture `web.navigate.json` (no `until`/`timeout_ms`) across
/// upgrades while newer method files were written fresh — the daemon then
/// rejected documented navigate args while wait_for accepted them. The disk
/// copy is a mirror for CLI use; the daemon validates from the embedded
/// builtins (`SchemaProvider::load_embedded_with_overlay`).
///
/// Schemas are derived from the WIT surface interface (`wit/loom-surface.wit`)
/// and embedded as compile-time const data (WIT is source of truth).
pub fn schema_step(schemas_dir: &std::path::Path) -> Result<SchemaStepOutcome, CliError> {
    std::fs::create_dir_all(schemas_dir).map_err(|e| CliError::Internal(e.to_string()))?;

    let mut populated = 0usize;
    let mut refreshed = 0usize;
    for (method, json_str) in BUILTIN_SCHEMAS {
        let file_path = schemas_dir.join(format!("{}.json", method));
        let existing = std::fs::read_to_string(&file_path).ok();
        match existing {
            Some(current) if current == *json_str => continue,
            Some(_) => {
                // Stale mirror: write atomically (tmp + rename) so a crashed
                // postinstall can't leave a torn schema file.
                let tmp = schemas_dir.join(format!(".{}.json.tmp", method));
                std::fs::write(&tmp, json_str.as_bytes())
                    .map_err(|e| CliError::Internal(format!("schema write {}: {}", method, e)))?;
                std::fs::rename(&tmp, &file_path)
                    .map_err(|e| CliError::Internal(format!("schema rename {}: {}", method, e)))?;
                refreshed += 1;
            }
            None => {
                std::fs::write(&file_path, json_str.as_bytes())
                    .map_err(|e| CliError::Internal(format!("schema write {}: {}", method, e)))?;
                populated += 1;
            }
        }
    }

    match (populated, refreshed) {
        // Preserve the prior wire-receipt values for the existing cases.
        (0, 0) => Ok(SchemaStepOutcome::Skipped),
        (n, 0) => Ok(SchemaStepOutcome::Populated(n)),
        (n, r) => Ok(SchemaStepOutcome::Refreshed {
            populated: n,
            refreshed: r,
        }),
    }
}

// ---------------------------------------------------------------------------
// Built-in JSON schemas
//
// Derived from wit/loom-surface.wit. Each entry is (method_name, json_str)
// where json_str has top-level "request" and "response" JSON Schema objects.
// WIT is the source of truth; these are the WIT-derived schemas.
// ---------------------------------------------------------------------------

/// Built-in JSON schemas for all loom action methods.
/// Format: (method_name, json_schema_string).
/// Each JSON has top-level "request" and "response" keys (JSON Schema objects).
///
/// The const itself lives in `loom_shared::builtin_schemas` (single source of
/// truth, embedded into the daemon for validation — see
/// `SchemaProvider::load_embedded_with_overlay`); re-exported here so
/// postinstall and the schema_registry_drift tests keep their import path.
pub use loom_shared::builtin_schemas::BUILTIN_SCHEMAS;

/// The 5 step labels in stable order. Used by the final receipt and
/// by interface tests.
pub const STEP_LABELS: &[&str] = &[
    "compile_module",
    "chromium",
    "loom_binaries",
    "launchd",
    "manpages",
];
