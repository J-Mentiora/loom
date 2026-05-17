//! End-to-end integration test for navigate-error receipts through the
//! loom CLI binary, with a real loom-daemon subprocess.
//!
//! Complementary to the (lighter) shim-level test at
//! `loom-host/tests/integration_navigate_status_codes.rs` (which drives
//! `ShimManager::send_navigate` directly). This test exercises the
//! daemon + CLI binary boundary too, asserting on the JSON receipt the
//! CLI prints to stdout.
//!
//! ## Architecture
//!
//! Everything is kept hermetic by overriding `HOME` for both the
//! daemon and the CLI subprocesses:
//!
//! - `dirs::data_dir()` (used by `AuthManager` for `hello.token` /
//!   `daemon.pid`) on macOS resolves to `$HOME/Library/Application Support`.
//! - `dirs::home_dir()` (used by the daemon to find
//!   `<HOME>/.config/loom/{surfaces,chromium}`) is also `$HOME`.
//!
//! Per-test layout (`<HOME>` = `tempdir`):
//!
//! ```text
//! <HOME>/.config/loom/schemas/v1/{*.json}                 (BUILTIN_SCHEMAS)
//! <HOME>/.config/loom/surfaces/loom_surface_web.cwasm     (precompiled artifact)
//! <HOME>/.config/loom/chromium/<per-OS subpath>                       (-> fake-chromium)
//!   macOS: chrome-mac/Chromium.app/Contents/MacOS/Chromium
//!   Linux: chrome-linux/chrome
//! <HOME>/Library/Application Support/loom/auth/{hello.token, daemon.pid}  (daemon writes)
//! <HOME>/loom-test.sock                                    (daemon socket)
//! ```
//!
//! ## Build prereqs
//!
//! Run before this test:
//! ```sh
//! cargo build --release --bin loom -p loom-cli
//! cargo build --release --bin loom-daemon -p loom-host
//! cargo build --release -p loom-shims --features fake-chromium-bin --bin fake-chromium
//! cargo build --release -p loom-cli --bin loom-shim-chromium
//! cargo build --release --target wasm32-wasip2 -p loom-surface-web
//! cargo test -p loom-cli --test integration_naverr_cli_e2e -- --ignored
//! ```
//!
//! Note: as of the SHA-256 build-pipeline fix, `cargo build --release
//! -p loom-cli` auto-builds the wasm32-wasip2 cdylib via a recursive
//! cargo invocation in `loom-cli/build.rs`. The explicit
//! `--target wasm32-wasip2 -p loom-surface-web` line is kept here
//! because `cargo test -p loom-cli --test ...` builds only the test
//! binary, not the loom-cli main bin whose build.rs runs the wasm
//! build — the test fixture's `wasm_path.exists()` precondition still
//! requires the convention-path artifact be present before the test
//! runs.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Mirror of `loom-daemon::data_root_default()` semantics applied to a custom
/// HOME (we can't call `dirs::data_dir()` because it reads the test process's
/// HOME, not the daemon-subprocess's). macOS: `Library/Application Support`;
/// Linux: XDG `.local/share` (the test sets XDG_DATA_HOME below to match).
fn auth_dir_for(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/loom/auth")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local/share/loom/auth")
    }
}

/// Mirror of `loom-rpc::socket_server::default_socket_path` semantics.
/// macOS: `dirs::cache_dir()/loom/loom.sock` = `~/Library/Caches/loom/loom.sock`.
/// Linux: `$XDG_RUNTIME_DIR/loom.sock`. The test sets XDG_RUNTIME_DIR below
/// to point at this same dir so the CLI subprocess agrees with the daemon.
fn socket_path_for(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Caches/loom/loom.sock")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".runtime/loom.sock")
    }
}

/// Parent dir of `socket_path_for(home)`, used both for creating the dir at
/// sandbox setup and for the `XDG_RUNTIME_DIR` env var on Linux.
fn socket_parent_for(home: &Path) -> PathBuf {
    socket_path_for(home)
        .parent()
        .expect("socket parent")
        .to_path_buf()
}

// ─── Fixture: built-once-per-test-binary state ──────────────────────────────

/// Cached AOT-compiled cwasm path + binary paths. Computed on first
/// access; reused across tests.
struct Fixture {
    loom_bin: PathBuf,
    daemon_bin: PathBuf,
    fake_chromium_bin: PathBuf,
    cwasm_path: PathBuf,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(build_fixture)
}

fn build_fixture() -> Fixture {
    let target_bin = target_bin_dir();
    let loom_bin = target_bin.join("loom");
    let daemon_bin = target_bin.join("loom-daemon");
    let fake_chromium_bin = target_bin.join("fake-chromium");
    let shim_bin = target_bin.join("loom-shim-chromium");

    for (name, path) in [
        ("loom", &loom_bin),
        ("loom-daemon", &daemon_bin),
        ("fake-chromium", &fake_chromium_bin),
        ("loom-shim-chromium", &shim_bin),
    ] {
        if !path.exists() {
            panic!(
                "{name} binary not built at {}; see file-header for build commands",
                path.display()
            );
        }
    }

    let workspace_root = workspace_root();
    let wasm_path = workspace_root.join("target/wasm32-wasip2/release/loom_surface_web.wasm");
    if !wasm_path.exists() {
        panic!(
            "wasm32-wasip2 surface artifact not built at {}; run \
             `cargo build --target wasm32-wasip2 -p loom-surface-web --release`",
            wasm_path.display()
        );
    }

    // AOT-compile to a shared cwasm under the test target dir. Per-test
    // HOME setup symlinks this into <HOME>/.config/loom/surfaces/.
    let cwasm_path = workspace_root.join("target/loom-naverr-e2e-cwasm/loom_surface_web.cwasm");
    std::fs::create_dir_all(cwasm_path.parent().unwrap()).expect("create cwasm cache dir");
    if !cwasm_path.exists() {
        // Use loom-host's Compiler to ensure compatibility with the runtime
        // the daemon will use (same wasmtime version, same engine settings).
        use loom_host::compiler::Compiler;
        use loom_host::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
        let runtime = WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime::new");
        let compiler = Compiler::new(runtime);
        compiler
            .compile_module(&wasm_path, &cwasm_path)
            .expect("AOT compile loom_surface_web.wasm -> cwasm");
    }

    Fixture {
        loom_bin,
        daemon_bin,
        fake_chromium_bin,
        cwasm_path,
    }
}

fn target_bin_dir() -> PathBuf {
    // The test binary lives at <target>/debug/deps/<test>-<hash>; bin dir
    // is two parents up.
    let test_exe = std::env::current_exe().expect("current_exe");
    test_exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target/debug")
        .to_path_buf()
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points to <workspace>/loom-cli; workspace
    // root is its parent (<workspace>).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

// ─── Per-test sandbox ───────────────────────────────────────────────────────

struct Sandbox {
    home: tempfile::TempDir,
    socket: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        // Daemon and CLI must agree on the socket path without us passing
        // `--socket` to the CLI (that flag exists only on `loom serve`).
        // socket_path_for() mirrors loom_rpc::socket_server::default_socket_path
        // per platform: macOS uses dirs::cache_dir()/loom/loom.sock under HOME;
        // Linux uses $XDG_RUNTIME_DIR/loom.sock — the daemon + CLI envs below
        // both point XDG_RUNTIME_DIR at this dir so all three sides agree.
        std::fs::create_dir_all(socket_parent_for(home.path())).expect("mkdir socket parent");
        let socket = socket_path_for(home.path());

        // Layout the directories the daemon + CLI expect.
        let cfg_loom = home.path().join(".config/loom");
        std::fs::create_dir_all(cfg_loom.join("surfaces")).expect("mkdir surfaces");
        std::fs::create_dir_all(cfg_loom.join("schemas/v1")).expect("mkdir schemas/v1");
        // Fake-chromium fixture path must match the per-OS layout the daemon's
        // resolver looks at — `loom_shared::chromium_resolver::resolve_chromium`
        // step 2 joins `chromium_dir` with `chromium_binary_subpath()`
        // (`chrome-linux/chrome` on Linux, `chrome-mac/Chromium.app/...` on
        // macOS). Hardcoding `Chromium.app/...` here would make the resolver
        // miss the fixture on Linux and fall through to PATH — picking up the
        // GitHub runner's real `/usr/bin/google-chrome` and breaking the
        // navigate-error scripted responses these tests depend on.
        let chromium_link = cfg_loom
            .join("chromium")
            .join(loom_shared::chromium_resolver::chromium_binary_subpath());
        let chromium_bundle = chromium_link
            .parent()
            .expect("chromium subpath has parent");
        std::fs::create_dir_all(chromium_bundle).expect("mkdir chromium bundle");
        std::fs::create_dir_all(auth_dir_for(home.path())).expect("mkdir auth");

        // Symlink fake-chromium into the path the daemon's
        // `build_host_bridge` resolves to (~/.config/loom/chromium/...).
        std::os::unix::fs::symlink(&fixture().fake_chromium_bin, &chromium_link)
            .expect("symlink fake-chromium");

        // Symlink the precompiled cwasm into the surfaces dir.
        std::os::unix::fs::symlink(
            &fixture().cwasm_path,
            cfg_loom.join("surfaces/loom_surface_web.cwasm"),
        )
        .expect("symlink cwasm");

        // Emit BUILTIN_SCHEMAS (web.* + rpc.schemas) so `SchemaCache::load`
        // succeeds on the CLI side and the daemon's RequestRouter registers
        // them. This is what `loom postinstall` does at step 2.
        let schemas_dir = cfg_loom.join("schemas/v1");
        loom_cli::postinstall_runner::schema_step(&schemas_dir).expect("schema_step");

        // BUILTIN_SCHEMAS doesn't include session.* / vault.* / etc. — the
        // daemon's SchemaValidator gates those as MethodNotFound when the
        // registry is non-empty. Drop in permissive schemas for the
        // session lifecycle methods this test uses so the daemon accepts
        // them and dispatches to the hardcoded match arms in
        // `RequestRouter::dispatch`.
        let permissive = r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#;
        for method in ["session.create", "session.list", "session.close"] {
            std::fs::write(schemas_dir.join(format!("{method}.json")), permissive)
                .expect("write permissive schema");
        }

        Sandbox { home, socket }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }
}

// ─── Daemon RAII guard ──────────────────────────────────────────────────────

struct Daemon {
    child: Child,
    /// Hold a strong reference to the sandbox so HOME tempdir isn't
    /// dropped while the daemon is running.
    _sandbox: Sandbox,
}

impl Daemon {
    fn spawn(sandbox: Sandbox) -> Self {
        let mut child = Command::new(&fixture().daemon_bin)
            .args(["--socket", &sandbox.socket.display().to_string()])
            .env("HOME", sandbox.home_path())
            // Ensure Linux + macOS both resolve dirs::* under the override.
            .env("XDG_DATA_HOME", sandbox.home_path().join(".local/share"))
            .env("XDG_CONFIG_HOME", sandbox.home_path().join(".config"))
            .env("XDG_CACHE_HOME", sandbox.home_path().join(".cache"))
            .env("XDG_RUNTIME_DIR", socket_parent_for(sandbox.home_path()))
            // Stream daemon stderr to a file in the sandbox; we drain it
            // into the panic message when something fails downstream.
            .env(
                "RUST_LOG",
                "loom_host=debug,loom_rpc=info,loom_daemon=info,warn",
            )
            .stdout(Stdio::piped())
            .stderr(
                std::fs::File::create(sandbox.home_path().join("daemon.stderr"))
                    .map(Stdio::from)
                    .expect("create daemon.stderr"),
            )
            .spawn()
            .expect("spawn loom-daemon");

        // Wait for HELLO_TOKEN line on stdout.
        let stdout = child.stdout.take().expect("daemon stdout piped");
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut hello_line = String::new();
        loop {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("loom-daemon did not print HELLO_TOKEN within 15s");
            }
            hello_line.clear();
            match reader.read_line(&mut hello_line) {
                Ok(0) => {
                    let _ = child.kill();
                    panic!("loom-daemon stdout EOF before HELLO_TOKEN; check stderr above");
                }
                Ok(_) => {
                    if hello_line.starts_with("HELLO_TOKEN=") {
                        break;
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    panic!("daemon stdout read error: {e}");
                }
            }
        }

        // Wait for hello.token + daemon.pid files (AuthManager::daemon_alive
        // checks the pid via `kill -0`).
        let auth_dir = auth_dir_for(sandbox.home_path());
        let token_path = auth_dir.join("hello.token");
        let pid_path = auth_dir.join("daemon.pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if token_path.exists() && pid_path.exists() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!(
                    "auth files not present after HELLO_TOKEN: token={} pid={}",
                    token_path.exists(),
                    pid_path.exists()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Daemon {
            child,
            _sandbox: sandbox,
        }
    }

    fn home_path(&self) -> &Path {
        self._sandbox.home_path()
    }

    fn cli(&self, args: &[&str]) -> CliOutput {
        // No --socket flag: not all subcommands accept it. Both daemon
        // and CLI compute the same default socket path from HOME.
        let output = Command::new(&fixture().loom_bin)
            .args(args)
            .env("HOME", self.home_path())
            .env("XDG_DATA_HOME", self.home_path().join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home_path().join(".config"))
            .env("XDG_CACHE_HOME", self.home_path().join(".cache"))
            .env("XDG_RUNTIME_DIR", socket_parent_for(self.home_path()))
            .env("RUST_LOG", "warn")
            .output()
            .expect("spawn loom CLI");
        CliOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn create_session(&self) -> String {
        let out = self.cli(&["session", "create"]);
        if out.status != 0 {
            panic!(
                "loom session create failed: status={} stdout={:?} cli_stderr={:?}\n--- daemon stderr ---\n{}",
                out.status, out.stdout, out.stderr, self.daemon_stderr()
            );
        }
        let value: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
            panic!("session create stdout not JSON: {e}; raw={:?}", out.stdout)
        });
        value
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "session_id missing in response: {value}\n--- daemon stderr (tail) ---\n{}",
                    tail_lines(&self.daemon_stderr(), 30)
                )
            })
            .to_string()
    }

    fn daemon_stderr(&self) -> String {
        std::fs::read_to_string(self.home_path().join("daemon.stderr")).unwrap_or_default()
    }

    fn navigate(&self, session_id: &str, url: &str) -> serde_json::Value {
        let out = self.cli(&[
            "action",
            "web.navigate",
            "--session",
            session_id,
            "--url",
            url,
        ]);
        // status=0 for success receipts; status=1 for status="error" receipts.
        // Both are valid — we want to inspect the JSON.
        if out.status != 0 && out.status != 1 {
            panic!(
                "loom action web.navigate exited with unexpected status={}: stdout={:?} stderr={:?}",
                out.status, out.stdout, out.stderr
            );
        }
        serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "navigate stdout not JSON: {e}; status={} stdout={:?} cli_stderr={:?}\n--- daemon stderr (tail) ---\n{}",
                out.status,
                out.stdout,
                out.stderr,
                tail_lines(&self.daemon_stderr(), 60)
            )
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct CliOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

// ─── Test serialisation: only one daemon may bind a given socket path ───────
//
// We use a process-wide mutex to serialise tests, which also guarantees that
// each test's daemon-spawn handshake is uncluttered by other tests' stdout.

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn run_with_daemon<F: FnOnce(&Daemon)>(f: F) {
    let _guard = test_lock().lock().unwrap_or_else(|p| p.into_inner());
    let daemon = Daemon::spawn(Sandbox::new());
    f(&daemon);
}

// ─── 200 → success path with status_code populated ──────────────────────────

#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn naverr_04_status_200_through_cli() {
    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let receipt = daemon.navigate(&sid, "http://fake.test/status/200");

        assert_eq!(
            receipt["status"], "success",
            "status must be 'success' for 200 — got receipt: {receipt}"
        );
        assert_eq!(receipt["status_code"], 200u64, "status_code must be 200");
        assert!(receipt["error"].is_null(), "error must be null on success");

        // action_hash + outcome_hash are 64-char lowercase hex SHA-256.
        for field in ["action_hash", "outcome_hash"] {
            let h = receipt[field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} must be a string in {receipt}"));
            assert_eq!(
                h.len(),
                64,
                "{field} must be 64 hex chars (SHA-256), got {} chars: {h}",
                h.len()
            );
            assert!(
                h.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{field} must be lowercase hex only, got {h}"
            );
        }
    });
}

// ─── action_hash byte-equivalent to hashlib.sha256 over canonical bytes ─

/// For `web.navigate`, the daemon canonicalizes the action's
/// payload as the raw UTF-8 URL bytes (see loom-daemon/src/main.rs:451).
/// The guest's `hex_sha256(&a.payload)` must therefore equal
/// `hashlib.sha256(url.encode()).hexdigest()` byte-for-byte.
///
/// This test rebuilds that expected hash in Rust via `sha2::Sha256`
/// (bit-identical to Python's hashlib SHA-256 implementation per FIPS
/// 180-4) and compares against the receipt. Catches regressions where
/// the canonical-bytes selection drifts from URL bytes to e.g. CBOR-
/// encoded payloads.
#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn sha_action_hash_matches_sha256_of_url_for_navigate() {
    use sha2::{Digest, Sha256};

    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let url = "http://fake.test/status/200";
        let receipt = daemon.navigate(&sid, url);

        let expected = format!("{:x}", Sha256::digest(url.as_bytes()));
        let action_hash = receipt["action_hash"]
            .as_str()
            .expect("action_hash must be a string");

        assert_eq!(
            action_hash, expected,
            "action_hash for web.navigate must equal \
             SHA-256(url.as_bytes()) (=hashlib.sha256(url.encode()).hexdigest()). \
             url={url} expected={expected} got={action_hash}"
        );
    });
}

// ─── 404 surfaces typed-error receipt through CLI ───────────────────────────

#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn naverr_01_status_404_through_cli() {
    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let receipt = daemon.navigate(&sid, "http://fake.test/status/404");

        assert_eq!(
            receipt["status"], "error",
            "status must be 'error' for 404 — got receipt: {receipt}"
        );
        assert_eq!(
            receipt["error"]["kind"], "http_status",
            "error.kind must be 'http_status'"
        );
        assert_eq!(
            receipt["error"]["detail"]["status_code"], 404u64,
            "error.detail.status_code must be 404"
        );
        assert_eq!(
            receipt["status_code"], 404u64,
            "top-level status_code mirrors detail.status_code"
        );
    });
}

// ─── 500 surfaces typed-error receipt through CLI ───────────────────────────

#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn naverr_02_status_500_through_cli() {
    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let receipt = daemon.navigate(&sid, "http://fake.test/status/500");

        assert_eq!(receipt["status"], "error", "status must be 'error' for 500");
        assert_eq!(receipt["error"]["kind"], "http_status");
        assert_eq!(receipt["error"]["detail"]["status_code"], 500u64);
        assert_eq!(receipt["status_code"], 500u64);
    });
}

// ─── DNS-fail surfaces typed-error receipt with chromium_error ──────────────

#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn naverr_03_dns_failure_through_cli() {
    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let url = "http://fake.test/error/ERR_NAME_NOT_RESOLVED";
        let receipt = daemon.navigate(&sid, url);

        assert_eq!(
            receipt["status"], "error",
            "status must be 'error' for DNS-fail — got receipt: {receipt}"
        );
        assert_eq!(
            receipt["error"]["kind"], "dns_failure",
            "error.kind must be 'dns_failure'"
        );
        assert_eq!(
            receipt["error"]["detail"]["url"], url,
            "detail.url must match navigate target"
        );
        let chromium_error = receipt["error"]["detail"]["chromium_error"]
            .as_str()
            .expect("detail.chromium_error must be a string");
        assert!(
            chromium_error.contains("ERR_NAME_NOT_RESOLVED"),
            "detail.chromium_error must carry the chromium code (got {chromium_error:?})"
        );
    });
}

// ─── connect-refused classifies separately ──────────────────────────────────

#[test]
#[ignore = "spawns loom-daemon + loom CLI subprocesses; see file header for build commands"]
fn naverr_03_connect_refused_through_cli() {
    run_with_daemon(|daemon| {
        let sid = daemon.create_session();
        let url = "http://fake.test/error/ERR_CONNECTION_REFUSED";
        let receipt = daemon.navigate(&sid, url);

        assert_eq!(
            receipt["status"], "error",
            "connect-refused: status must be 'error'"
        );
        assert_eq!(
            receipt["error"]["kind"], "connect_refused",
            "connect-refused must classify as 'connect_refused'"
        );
        let chromium_error = receipt["error"]["detail"]["chromium_error"]
            .as_str()
            .expect("detail.chromium_error must be a string");
        assert!(
            chromium_error.contains("ERR_CONNECTION_REFUSED"),
            "chromium_error must name the chromium code"
        );
    });
}
