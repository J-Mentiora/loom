//! `loom-mcp` binary entrypoint — produces `target/release/loom-mcp` so the
//! `loom mcp serve` CLI subcommand can delegate to it via subprocess
//! (AC-PROTO-03.1, SR-CLI-02).
//!
//! Thin clap wrapper around `loom_mcp::mcp_main::run`. This file lives in
//! `loom-cli/` (not `loom-mcp/`) so cargo-dist can bundle all 4 loom
//! binaries into one Cargo package and ship them in one tarball
//! (AC-DIST-01).

use clap::Parser;
use loom_mcp::mcp_main::{run, ServeArgs};

#[derive(Parser, Debug)]
#[command(name = "loom-mcp", about = "Loom MCP server (stdio transport)")]
enum Cli {
    /// Run the MCP server on stdio. Stays running until stdin closes
    /// or SIGINT/SIGTERM arrives.
    Serve(ServeArgs),
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("multi-thread tokio runtime must build");

    let exit_code = runtime.block_on(async {
        match Cli::parse() {
            Cli::Serve(args) => match run(args).await {
                Ok(()) => 0,
                Err(e) => {
                    // Pass-through error to stderr; exit non-zero so the
                    // parent (loom-cli mcp_delegate) propagates a useful
                    // exit code. AC-CLIEXIT-* downstream of this binary
                    // is the parent's responsibility.
                    eprintln!("loom-mcp serve failed: {e}");
                    1
                }
            },
        }
    });
    std::process::exit(exit_code);
}
