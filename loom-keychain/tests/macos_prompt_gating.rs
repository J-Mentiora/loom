//! Prompt-gating tests for the macOS backend (`allow_prompt` enforcement).
//!
//! Unlike `macos_keychain_e2e.rs` these are NOT `#[ignore]`d: they are
//! hermetic. They only observe the process-global Security-framework
//! user-interaction flag (`SecKeychainSetUserInteractionAllowed`) that the
//! backend's constructor/Drop toggles — no keychain item is read or written,
//! so no OS prompt can ever appear and the default `cargo test` stays safe.
//!
//! Everything lives in ONE test fn: the flag is process-global, so two test
//! fns would race under the default parallel test runner.

#![cfg(target_os = "macos")]

use loom_keychain::MacOsKeychain;
use security_framework::os::macos::keychain::SecKeychain;

const TEST_SERVICE: &str = "loom-test";

#[test]
fn allow_prompt_gates_security_framework_ui_for_backend_lifetime() {
    // allow_prompt=true must leave Security-framework UI untouched.
    let permissive = MacOsKeychain::new(TEST_SERVICE, true).expect("new(allow_prompt=true)");
    assert!(permissive.allow_prompt(), "config accessor reflects true");
    assert!(
        SecKeychain::user_interaction_allowed().expect("read interaction flag"),
        "allow_prompt=true must not suppress keychain UI"
    );
    drop(permissive);

    // allow_prompt=false (daemon non-TTY default) holds the suppression lock
    // for the backend's lifetime: would-be prompts now fail fast with
    // errSecInteractionNotAllowed (-25308) → NonInteractivePrompt instead of
    // blocking the daemon behind a GUI dialog.
    let strict = MacOsKeychain::new(TEST_SERVICE, false).expect("new(allow_prompt=false)");
    assert!(!strict.allow_prompt(), "config accessor reflects false");
    assert!(
        !SecKeychain::user_interaction_allowed().expect("read interaction flag"),
        "allow_prompt=false must suppress keychain UI while the backend lives"
    );
    drop(strict);

    // Drop releases the RAII lock and restores interactive behaviour.
    assert!(
        SecKeychain::user_interaction_allowed().expect("read interaction flag"),
        "dropping the backend must re-enable keychain UI"
    );
}
