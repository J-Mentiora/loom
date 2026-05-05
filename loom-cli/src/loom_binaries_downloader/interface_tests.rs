// LoomBinariesDownloader interface tests — unit-level coverage of the
// constants, helpers, and target-triple resolution. Network-touching paths
// (`ensure()`) are covered end-to-end by
// loom-cli/tests/integration_loom_binaries_postinstall.rs.

use super::loom_binaries_downloader as lbd;

#[test]
fn aux_binary_names_are_the_three_siblings() {
    let names = lbd::AUX_BINARY_NAMES;
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"loom-daemon"));
    assert!(names.contains(&"loom-mcp"));
    assert!(names.contains(&"loom-shim-chromium"));
    // Crucially: must NOT include "loom" — that's the binary cargo install
    // already produced; pulling it from the release would self-overwrite.
    assert!(!names.contains(&"loom"));
}

#[test]
fn host_target_triple_is_one_of_supported() {
    let triple = lbd::host_target_triple();
    let supported = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        // Tests may run on Windows or unusual targets in CI matrix
        // expansions; the function returns this sentinel for those:
        "unsupported-target",
    ];
    assert!(
        supported.contains(&triple),
        "host_target_triple() returned {triple}, expected one of {supported:?}"
    );
}

#[test]
fn default_install_dir_resolves_to_loom_bin_under_data_local() {
    let dir = lbd::default_install_dir().expect("data_local_dir resolves in tests");
    let suffix: Vec<_> = dir
        .iter()
        .rev()
        .take(2)
        .map(|s| s.to_str().unwrap_or(""))
        .collect();
    assert_eq!(suffix[0], "bin");
    assert_eq!(suffix[1], "loom");
}
