//! Hermetic end-to-end test for the v0.9.4 W6 keychain CLI surface
//! (W7.2 / plan amendment A-W7.1).
//!
//! Spawns a real `loom-daemon` subprocess with
//! `LOOM_KEYCHAIN_BACKEND=in_memory` so the test doesn't touch the OS
//! keychain. Exercises the new `loom vault add --label X --from-stdin`,
//! `vault list-labels`, and `vault delete` subcommands, then restarts
//! the daemon to confirm `InMemoryKeychain` is genuinely process-local
//! (post-restart `list-labels` must be empty — proves the daemon is
//! talking to a real backend, not a stub).
//!
//! The G1 grep test (PROMPT canary-substring check) scans every file
//! under the daemon's data root after `vault add` and asserts the
//! canary bytes never appear — the SecretAuditPayload carries
//! `size_bucket`, not the raw bytes, so the canary must be absent from
//! manifests, WAL fragments, or any other persisted artefact.
//!
//! Gated `#[ignore]` because the test requires the `loom` + `loom-daemon`
//! binaries to be built ahead of time. Run via:
//!
//!     cargo build --release --bin loom --bin loom-daemon
//!     cargo test -p loom-cli --test keychain_e2e_hermetic -- --ignored

#![allow(clippy::expect_fun_call)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// ── Paths ──────────────────────────────────────────────────────────────

fn target_bin_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current_exe");
    test_exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target/debug")
        .to_path_buf()
}

struct Bins {
    loom_bin: PathBuf,
    daemon_bin: PathBuf,
}

fn bins() -> &'static Bins {
    static B: OnceLock<Bins> = OnceLock::new();
    B.get_or_init(|| {
        let dir = target_bin_dir();
        let loom_bin = dir.join("loom");
        let daemon_bin = dir.join("loom-daemon");
        for (name, p) in [("loom", &loom_bin), ("loom-daemon", &daemon_bin)] {
            if !p.exists() {
                panic!(
                    "{name} binary not built at {}; build with `cargo build \
                     --release --bin loom --bin loom-daemon`",
                    p.display()
                );
            }
        }
        Bins {
            loom_bin,
            daemon_bin,
        }
    })
}

fn data_root(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/loom")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local/share/loom")
    }
}

fn socket_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Caches/loom/loom.sock")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".runtime/loom.sock")
    }
}

fn socket_parent(home: &Path) -> PathBuf {
    socket_path(home).parent().unwrap().to_path_buf()
}

// ── Daemon fixture ─────────────────────────────────────────────────────

struct Daemon {
    child: Child,
    home: PathBuf,
}

impl Daemon {
    fn spawn(home: &Path) -> Self {
        std::fs::create_dir_all(socket_parent(home)).expect("mkdir socket parent");

        let mut child = Command::new(&bins().daemon_bin)
            .args(["--socket", &socket_path(home).display().to_string()])
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", socket_parent(home))
            // A-W4.3 / A-W7.1 — hermetic backend.
            .env("LOOM_KEYCHAIN_BACKEND", "in_memory")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::piped())
            .stderr(
                std::fs::File::create(home.join("daemon.stderr"))
                    .map(Stdio::from)
                    .expect("create daemon.stderr"),
            )
            .spawn()
            .expect("spawn loom-daemon");

        let stdout = child.stdout.take().expect("daemon stdout piped");
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut hello_line = String::new();
        loop {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let stderr_dump =
                    std::fs::read_to_string(home.join("daemon.stderr")).unwrap_or_default();
                panic!("loom-daemon did not print HELLO_TOKEN within 15s\n--- daemon stderr ---\n{stderr_dump}");
            }
            hello_line.clear();
            match reader.read_line(&mut hello_line) {
                Ok(0) => {
                    let _ = child.kill();
                    let stderr_dump =
                        std::fs::read_to_string(home.join("daemon.stderr")).unwrap_or_default();
                    panic!("loom-daemon stdout EOF before HELLO_TOKEN\n--- daemon stderr ---\n{stderr_dump}");
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

        Daemon {
            child,
            home: home.to_path_buf(),
        }
    }

    fn cli(&self, args: &[&str], stdin_bytes: Option<&[u8]>) -> CliOutput {
        let mut cmd = Command::new(&bins().loom_bin);
        cmd.args(args)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("XDG_RUNTIME_DIR", socket_parent(&self.home))
            .env("RUST_LOG", "warn");

        if stdin_bytes.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("spawn loom CLI");
        if let Some(bytes) = stdin_bytes {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(bytes)
                .expect("write stdin");
        }
        let output = child.wait_with_output().expect("wait_with_output");
        CliOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn shutdown(mut self) {
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

// ── Helpers ────────────────────────────────────────────────────────────

fn unique_canary() -> String {
    // Combine PID + wall-clock nanos for a per-test-invocation suffix
    // without pulling rand into loom-cli's dev-deps.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("LOOM_TEST_CANARY_v094_{pid:08x}_{nanos:08x}")
}

/// Recursive byte-level grep — returns the first file path containing
/// `needle`, or `None` when clean. Skips the socket / pid files (binary
/// metadata that can never carry secret bytes by construction).
fn scan_for_canary(root: &Path, needle: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, needle: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(hit) = walk(&p, needle) {
                    return Some(hit);
                }
                continue;
            }
            // Skip the unix socket (metadata, not bytes) and the pid file.
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if fname == "loom.sock" || fname == "daemon.pid" {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&p) {
                if memmem(&bytes, needle.as_bytes()) {
                    return Some(p);
                }
            }
        }
        None
    }
    walk(root, needle)
}

fn memmem(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}

// ── The hermetic test ─────────────────────────────────────────────────

#[test]
#[ignore]
fn vault_add_list_delete_round_trip_with_in_memory_backend() {
    let home = tempfile::tempdir().expect("tempdir");
    let canary = unique_canary();

    let daemon = Daemon::spawn(home.path());

    // 1. `vault add --label test-grant --from-stdin` writes the canary
    //    secret. The daemon hex-encodes through the wire and stores in
    //    the in-memory keychain.
    let add = daemon.cli(
        &[
            "vault", "add",
            "--label", "test-grant",
            "--from-stdin",
        ],
        Some(canary.as_bytes()),
    );
    assert_eq!(
        add.status, 0,
        "vault add: status={} stdout={:?} stderr={:?}",
        add.status, add.stdout, add.stderr
    );

    // 2. `vault list-labels` includes test-grant.
    let list = daemon.cli(&["vault", "list-labels", "--json"], None);
    assert_eq!(list.status, 0, "vault list-labels failed: {list:?}");
    assert!(
        list.stdout.contains("test-grant"),
        "list-labels output missing test-grant: {}",
        list.stdout
    );

    // 3. G1 invariant: the canary substring must NEVER appear in any
    //    persisted file under the daemon's data root. The audit payload
    //    carries `size_bucket = small`, not the raw bytes, so a literal
    //    byte-grep is sound.
    let root = data_root(home.path());
    let hit = scan_for_canary(&root, &canary);
    assert!(
        hit.is_none(),
        "G1 violation: canary {canary:?} found in {:?}",
        hit
    );

    // 4. Restart the daemon (still LOOM_KEYCHAIN_BACKEND=in_memory).
    //    Post-restart, `InMemoryKeychain` is a fresh empty map — proves
    //    the daemon is actually wired to a backend (a stub would have
    //    behaved the same before and after).
    daemon.shutdown();
    let daemon = Daemon::spawn(home.path());
    let list2 = daemon.cli(&["vault", "list-labels", "--json"], None);
    assert_eq!(list2.status, 0, "post-restart list-labels failed: {list2:?}");
    assert!(
        list2.stdout.contains("\"count\":0") || list2.stdout.contains("\"count\": 0"),
        "post-restart list-labels expected count=0, got: {}",
        list2.stdout
    );

    // 5. `vault delete <unknown-label>` is idempotent (cascade=0, exit 0).
    let del = daemon.cli(&["vault", "delete", "test-grant"], None);
    assert_eq!(
        del.status, 0,
        "delete of unknown label must be idempotent: {del:?}"
    );

    daemon.shutdown();
}
