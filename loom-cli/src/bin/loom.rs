//! `loom` binary entrypoint. Thin shim over `loom_cli::cli_main::main`.
fn main() {
    loom_cli::cli_main::main();
}
