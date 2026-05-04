//! Pinned Chromium revision constants. One URL + SHA-256 per supported platform.
//!
//! Architecture §6 names **chromium 132** as the pinned revision.
//! Playwright CDN revision **1153** ships Chromium 132.0.6834.57 — the closest
//! available revision to the architecture's 132.0.6834.84 pin. All four platform
//! SHA-256 values were computed from live archive downloads on 2026-04-28:
//!
//! ```sh
//! curl -fsSL <URL> | shasum -a 256
//! ```
//!
//! To re-verify the URLs are still reachable:
//! ```sh
//! LOOM_VERIFY_CHROMIUM_PINS=1 cargo test -p loom-cli verify_url_head
//! ```

/// Chromium version string (Playwright CDN revision 1153).
pub const CHROMIUM_VERSION: &str = "132.0.6834.57";

// ---------- macOS Apple Silicon (aarch64) ----------

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const CHROMIUM_URL: &str =
    "https://playwright.azureedge.net/builds/chromium/1153/chromium-mac-arm64.zip";

/// SHA-256 of chromium-mac-arm64.zip at revision 1153. Verified 2026-04-28.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const CHROMIUM_SHA256: &str =
    "30a05f40e9152ea140ba3f8ccdcd67afcee5bf05adbb1caba98209e21ccc76c0";

// ---------- macOS Intel (x86_64) ----------

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const CHROMIUM_URL: &str =
    "https://playwright.azureedge.net/builds/chromium/1153/chromium-mac.zip";

/// SHA-256 of chromium-mac.zip at revision 1153. Verified 2026-04-28.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const CHROMIUM_SHA256: &str =
    "37f49e41d7d5ca35ccc3f060e8ea29afa82777b137d6d49309b5ed1ddbedd787";

// ---------- Linux x86_64 ----------

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const CHROMIUM_URL: &str =
    "https://playwright.azureedge.net/builds/chromium/1153/chromium-linux.zip";

/// SHA-256 of chromium-linux.zip at revision 1153. Verified 2026-04-28.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const CHROMIUM_SHA256: &str =
    "90f98613e3671ae0d2bb4b6b207c8842b00546dd2ff9debb67cb29bbad61c3ac";

// ---------- Linux ARM64 ----------

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const CHROMIUM_URL: &str =
    "https://playwright.azureedge.net/builds/chromium/1153/chromium-linux-arm64.zip";

/// SHA-256 of chromium-linux-arm64.zip at revision 1153. Verified 2026-04-28.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const CHROMIUM_SHA256: &str =
    "6528ee2cc48f36bcd8f111e856d1fd27e7f960afcf3be94f58aadeeb42067878";

// ---------- Unsupported platform ----------

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
)))]
compile_error!(
    "loom: unsupported platform for chromium_pin constants. \
     Supported: macOS-aarch64, macOS-x86_64, linux-x86_64, linux-aarch64."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!CHROMIUM_VERSION.is_empty());
    }

    #[test]
    fn url_is_non_empty_and_https() {
        assert!(CHROMIUM_URL.starts_with("https://"));
    }

    #[test]
    fn sha256_is_64_hex_chars() {
        assert_eq!(CHROMIUM_SHA256.len(), 64, "SHA-256 must be 64 hex chars");
        assert!(
            CHROMIUM_SHA256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 must be lowercase hex"
        );
    }

    #[test]
    fn version_matches_architecture_pin() {
        // Architecture §6 names chromium 132 as the pinned revision.
        // Playwright CDN revision 1153 ships 132.0.6834.57 — closest available.
        assert_eq!(CHROMIUM_VERSION, "132.0.6834.57");
    }

    #[test]
    fn url_contains_playwright_cdn() {
        // Architecture recommended Playwright CDN as snapshot host.
        assert!(
            CHROMIUM_URL.contains("playwright.azureedge.net"),
            "URL must use Playwright CDN: {CHROMIUM_URL}"
        );
    }

    /// AC-CHRPIN-01: SHA-256 constants must not be placeholder values.
    /// Placeholder pattern: repeating hex pairs like a1b2c3d4e5f6... or
    /// aaaaaa... — real hashes have no such regularity.
    #[test]
    fn sha256_is_not_placeholder() {
        // Known bad placeholders from before this feature.
        let known_placeholders = [
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2",
            "b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3",
            "c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4",
            "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5",
        ];
        assert!(
            !known_placeholders.contains(&CHROMIUM_SHA256),
            "AC-CHRPIN-01: CHROMIUM_SHA256 is still a placeholder value: {CHROMIUM_SHA256}"
        );
    }

    /// AC-CHRPIN-02: URL must return HTTP 200 after redirects.
    /// Gated on LOOM_VERIFY_CHROMIUM_PINS=1 so offline builds are not blocked.
    #[test]
    fn verify_url_head() {
        if std::env::var("LOOM_VERIFY_CHROMIUM_PINS").as_deref() != Ok("1") {
            eprintln!("AC-CHRPIN-02: skipped (set LOOM_VERIFY_CHROMIUM_PINS=1 to run)");
            return;
        }
        let status = std::process::Command::new("curl")
            .args(["-fsSL", "-I", "-o", "/dev/null", "-w", "%{http_code}", CHROMIUM_URL])
            .output()
            .expect("curl not found");
        let code = String::from_utf8_lossy(&status.stdout);
        // curl -fsSL follows redirects; final status is on the last line.
        let final_code = code.trim().lines().last().unwrap_or("").trim();
        assert_eq!(
            final_code, "200",
            "AC-CHRPIN-02: CHROMIUM_URL HEAD returned {final_code}, expected 200: {CHROMIUM_URL}"
        );
    }
}
