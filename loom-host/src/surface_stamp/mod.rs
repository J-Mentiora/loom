// surface_stamp — the AOT `.cwasm` sidecar format, shared by every reader so
// they cannot disagree.
//
// The sidecar (`<name>.sha256` alongside `<name>.cwasm`) is the COMPOSITE
// install stamp the daemon, `loom postinstall`, and `loom doctor` all key on:
//
//     <source_sha256_hex>      line 1 — SHA-256 of the SOURCE `.wasm` bytes
//     <compat_hash>            line 2 — engine-compat identity string
//                                       (`WasmRuntime::precompile_compatibility_hash`)
//
// # Why two lines (not a content hash of the cwasm)
// The compiled `.cwasm` does NOT byte-match across hosts (macOS vs Linux CI —
// see CLAUDE.md's vendored-wasm gotcha), so a content hash of the cwasm would
// read perpetually "stale". `source_sha256` is over the host-stable SOURCE
// wasm; `compat_hash` is a deterministic IDENTITY STRING (arch + opt-level +
// wasmtime version), never compiled bytes. Neither enters the replay hash chain
// (install + boot path only — no NFR-DET-01 surface).
//
// # Backward compatibility
// Legacy installs wrote a single-line sidecar (source SHA only). `parse` yields
// `compat = None` for those. Consumers treat a missing compat line as:
//   - postinstall (`is_up_to_date`): STALE → recompile (upgrades the stamp).
//   - daemon (`ModuleLibrary::load_one`): skip the early compat check, fall
//     through to `deserialize_file` (the real engine-format backstop) — never a
//     false reject.
//   - doctor: pass (the daemon would still boot a compatible legacy artifact).

/// Render the composite sidecar contents for `source_sha256` (line 1) +
/// `compat_hash` (line 2). Trailing newline so the file is line-tool friendly.
pub fn format_surface_sidecar(source_sha256: &str, compat_hash: &str) -> String {
    format!("{source_sha256}\n{compat_hash}\n")
}

/// Parse a sidecar into `(source_sha256, compat_hash)`. Each line is trimmed;
/// an absent or empty line yields `None`. A legacy single-line sidecar parses
/// as `(Some(sha), None)`.
pub fn parse_surface_sidecar(contents: &str) -> (Option<&str>, Option<&str>) {
    let mut lines = contents.lines();
    let source = lines.next().map(str::trim).filter(|s| !s.is_empty());
    let compat = lines.next().map(str::trim).filter(|s| !s.is_empty());
    (source, compat)
}

/// The SHA-256 (hex) of the `loom_surface_web.wasm` SOURCE bytes THIS binary
/// was built against — the value `ModuleLibrary::load_one` checks the sidecar's
/// first line against. Empty string in dev builds where the wasm artifact was
/// absent at build time (the integrity check is then skipped). Exposed so
/// out-of-process consumers (`loom doctor`) can verify an installed sidecar
/// agrees with this binary WITHOUT loading the daemon.
pub fn embedded_surface_web_sha256() -> &'static str {
    env!("LOOM_SURFACE_WEB_SHA256")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_source_and_compat() {
        let s = format_surface_sidecar("deadbeef", "wh-aarch64-speed_and_size-wt45.0.2");
        let (source, compat) = parse_surface_sidecar(&s);
        assert_eq!(source, Some("deadbeef"));
        assert_eq!(compat, Some("wh-aarch64-speed_and_size-wt45.0.2"));
    }

    #[test]
    fn legacy_single_line_sidecar_has_no_compat() {
        // What every pre-fix install wrote: just the source SHA, no newline.
        let (source, compat) = parse_surface_sidecar("deadbeef");
        assert_eq!(source, Some("deadbeef"));
        assert_eq!(compat, None);
    }

    #[test]
    fn legacy_trailing_newline_sidecar_has_no_compat() {
        // A trailing newline must NOT read as an empty compat line.
        let (source, compat) = parse_surface_sidecar("deadbeef\n");
        assert_eq!(source, Some("deadbeef"));
        assert_eq!(compat, None);
    }

    #[test]
    fn blank_compat_line_is_none() {
        let (source, compat) = parse_surface_sidecar("deadbeef\n   \n");
        assert_eq!(source, Some("deadbeef"));
        assert_eq!(compat, None);
    }
}
