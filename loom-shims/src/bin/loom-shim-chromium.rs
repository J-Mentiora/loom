//! `loom-shim-chromium` — out-of-process Chromium driver shim.
//!
//! Spawned by `loom-host::ShimManager` per session. Reads CBOR
//! `ShimRequest` frames from the inherited socketpair (fd at
//! `LOOM_SHIM_FD`), drives a real Chromium subprocess via CDP, and writes
//! `ShimResponse` frames back. Exits cleanly on `ShimRequest::Shutdown`
//! or on socket EOF.
//!
//! Required env:
//!   - `LOOM_SHIM_FD` — RawFd of the socketpair end this process owns.
//!   - `LOOM_SHIM_CHROMIUM_PATH` — absolute path to the verified Chromium
//!     binary (the daemon resolves and SHA-pins this).
//!
//! Optional env:
//!   - `LOOM_SHIM_USER_DATA_DIR` — defaults to a per-pid temp dir.

use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let fd: RawFd = match std::env::var("LOOM_SHIM_FD")
        .ok()
        .and_then(|s| s.parse::<RawFd>().ok())
    {
        Some(n) => n,
        None => {
            eprintln!("loom-shim-chromium: LOOM_SHIM_FD missing or invalid");
            return ExitCode::from(2);
        }
    };
    let chromium_path = match std::env::var("LOOM_SHIM_CHROMIUM_PATH").map(PathBuf::from) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("loom-shim-chromium: LOOM_SHIM_CHROMIUM_PATH missing");
            return ExitCode::from(2);
        }
    };
    let user_data_dir = std::env::var("LOOM_SHIM_USER_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("loom-chromium-{}", std::process::id()))
        });

    let config = loom_shims::supervisor::SupervisorConfig::new(chromium_path, user_data_dir);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("loom-shim-chromium: tokio runtime build failed: {e}");
            return ExitCode::from(1);
        }
    };

    match rt.block_on(loom_shims::supervisor::run::run(config, fd)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loom-shim-chromium: {e}");
            ExitCode::from(1)
        }
    }
}
