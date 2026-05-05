//! `loom-daemon` binary entrypoint.
//!
//! Thin shim — all real logic lives in `loom_daemon::run()`. This file
//! exists in `loom-cli/` (not `loom-daemon/`) so cargo-dist can bundle
//! all 4 loom binaries (loom, loom-daemon, loom-mcp, loom-shim-chromium)
//! into one Cargo package and ship them in one tarball / one brew formula.

fn main() -> anyhow::Result<()> {
    loom_daemon::run()
}
