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

/// Per-step outcome — used for the final stdout receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Skipped,
    Compiled(PathBuf),
    Downloaded,
    Wrote,
}

/// Outcome for the schema emission step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaStepOutcome {
    /// Schemas dir was created and populated with `count` method schema files.
    Populated(usize),
    /// Schemas dir already existed and contained schema files — skipped.
    Skipped,
}

/// Final receipt for `loom postinstall`. Emitted as canonical JSON.
#[derive(Debug, Clone)]
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

/// Runs the full postinstall pipeline. Idempotent — safe to re-run.
pub async fn run(opts: PostinstallOptions) -> Result<PostinstallReceipt, CliError> {
    let compile_outcomes = compile_step(&opts.surfaces_dir)?;

    let schemas = schema_step(&opts.schemas_dir)?;

    let chromium = if opts.skip_chromium {
        StepOutcome::Skipped
    } else {
        let downloader =
            ChromiumDownloader::new(crate::chromium_downloader::ChromiumDownloaderConfig {
                install_dir: opts.chromium_dir.clone(),
                binary_subpath: PathBuf::from("Chromium.app/Contents/MacOS/Chromium"),
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
    Ok(format!("{:x}", hasher.finalize()))
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
/// Idempotent: if the directory already contains `*.json` files, returns
/// `SchemaStepOutcome::Skipped` without overwriting. This matches the
/// idempotence contract for all postinstall steps.
///
/// Schemas are derived from the WIT surface interface (`wit/loom-surface.wit`)
/// and embedded as compile-time const data (WIT is source of truth).
pub fn schema_step(schemas_dir: &std::path::Path) -> Result<SchemaStepOutcome, CliError> {
    // Idempotence guard: if schemas dir already has .json files, skip.
    if schemas_dir.exists() {
        let has_json = std::fs::read_dir(schemas_dir)
            .map_err(|e| CliError::Internal(e.to_string()))?
            .flatten()
            .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"));
        if has_json {
            return Ok(SchemaStepOutcome::Skipped);
        }
    }

    std::fs::create_dir_all(schemas_dir).map_err(|e| CliError::Internal(e.to_string()))?;

    let mut count = 0usize;
    for (method, json_str) in BUILTIN_SCHEMAS {
        let file_path = schemas_dir.join(format!("{}.json", method));
        std::fs::write(&file_path, json_str.as_bytes())
            .map_err(|e| CliError::Internal(format!("schema write {}: {}", method, e)))?;
        count += 1;
    }

    Ok(SchemaStepOutcome::Populated(count))
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
pub const BUILTIN_SCHEMAS: &[(&str, &str)] = &[
    // web.navigate response documents the navigate-tier-2 wire
    // fields. `additionalProperties` is intentionally NOT declared on
    // the response so the wire receipt can grow further fields without
    // invalidating older schemas.
    (
        "web.navigate",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"url":{"type":"string"}},"required":["session","url"],"additionalProperties":false},"response":{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"},"timing_ticks":{"type":"integer"},"side_effects":{"type":"array"},"error":{"type":["object","null"]},"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"url":{"type":"string"},"final_url":{"type":"string"},"title":{"type":"string"},"status_code":{"type":"integer"},"dom_snapshot_hash":{"type":"string"},"screenshot_after_hash":{"type":"string"},"console_count":{"type":"integer"},"console_lines":{"type":"array"},"network_count":{"type":"integer"},"network_summary":{"type":"object","properties":{"total_count":{"type":"integer"},"total_bytes":{"type":"integer"},"error_count":{"type":"integer"}}}}}}"#,
    ),
    (
        "web.click",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // Canonical name `web.type` (was `web.type_text`); the legacy
    // `web.type_text` spelling resolves here via
    // `loom_shared::action_aliases::METHOD_ALIASES`.
    (
        "web.type",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"},"text":{"type":"string"}},"required":["session","selector","text"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.select",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"},"value":{"type":"string"}},"required":["session","selector","value"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.hover",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.scroll",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"},"delta_x":{"type":"integer"},"delta_y":{"type":"integer"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.wait",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"},"timeout_ms":{"type":"integer"}},"required":["session","selector"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.evaluate",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"expression":{"type":"string"}},"required":["session","expression"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"},"return_value_json":{"type":"string","description":"Canonical-JSON of the evaluated value. Numerics serialize as JSON strings (1+1 -> \"2\", Math.PI -> \"3.141...\"). Absent when value > 64KB and offloaded to content store; see return_value_blob_ref."},"return_value_blob_ref":{"type":"object","description":"ContentRef when canonical-JSON > 64KB. Truncation discriminator = this field present.","properties":{"sha256":{"type":"string"},"size_bytes":{"type":"integer"}},"required":["sha256","size_bytes"]}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.screenshot",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"selector":{"type":"string"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    (
        "web.snapshot",
        r#"{"request":{"type":"object","properties":{"session":{"type":"string"}},"required":["session"],"additionalProperties":false},"response":{"type":"object","properties":{"action_hash":{"type":"string"},"outcome_hash":{"type":"string"},"emitted_at_ms":{"type":"integer"}},"required":["action_hash","outcome_hash","emitted_at_ms"]}}"#,
    ),
    // `rpc.schemas` — JSON-RPC introspection. Wire-side
    // schema_validator treats it as a built-in (no param check); this
    // CLI-side schema is permissive so `loom action rpc.schemas` reaches
    // the daemon. `session` is accepted but ignored — `action_commands`
    // unconditionally inserts it, and rpc.schemas tolerates the extra
    // field. The response declares no required fields because the
    // SchemaRegistry envelope is shape-stable but field names are
    // implementation-defined.
    (
        "rpc.schemas",
        r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#,
    ),
];

/// The 5 step labels in stable order. Used by the final receipt and
/// by interface tests.
pub const STEP_LABELS: &[&str] = &[
    "compile_module",
    "chromium",
    "loom_binaries",
    "launchd",
    "manpages",
];
