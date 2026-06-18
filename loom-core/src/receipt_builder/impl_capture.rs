// impl_capture — apply_capture_profile impl for ReceiptPayload.
//
// Decision logic (which field is kept under which profile) lives in
// `super::capture_policy::keep_field`; this file only knows HOW to
// apply a "drop" decision (nulling blob ref vs downgrading to hash vs
// clearing a Vec). Both this file and the wire-side enforcer in
// `loom-rpc::host_service_adapter::wire_capture` consult the same
// `keep_field` table — the exhaustiveness test in `capture_policy`
// guards drift.
//
// Minimal capture:
// - Strip all blob refs: `*_blob_ref` → None
// - Downgrade navigate blob-after refs → hash-only fields
// - Strip network event body refs (keep hash/metadata)
// - Empty console_lines
//
// Full capture:
// - No-op. Before-refs must already be set by the caller on the struct.
//
// Default: no-op.

use super::capture_policy::{keep_field, CaptureField, CaptureScope};
use super::receipt_builder::{CaptureProfile, ReceiptPayload};

impl ReceiptPayload {
    /// Apply capture-profile post-processing.
    ///
    /// - `Full`: no-op. The caller sets `dom_before_blob_ref` and
    ///   `screenshot_before_blob_ref` directly on the struct before calling Ship.
    /// - `Minimal`: strips per `keep_field(Manifest, Minimal, _)`. Two
    ///   fields require special "downgrade" rather than plain strip
    ///   (`dom_after_blob_ref` → hash; `screenshot_after_blob_ref` →
    ///   hash); per-event `response_body_ref` is mutated in place
    ///   without dropping the event itself.
    /// - `Default`: no-op.
    pub fn apply_capture_profile(&mut self, profile: CaptureProfile) {
        // Manifest+Default and Manifest+Full are no-ops on the struct
        // (the caller pre-populates Full's `*BeforeBlobRef` fields).
        if profile != CaptureProfile::Minimal {
            return;
        }
        let scope = CaptureScope::Manifest;

        // dom_after_blob_ref → dom_after_hash downgrade.
        if !keep_field(scope, profile, CaptureField::DomAfterBlobRef) {
            if let Some(ref r) = self.dom_after_blob_ref {
                self.dom_after_hash = Some(r.sha256.clone());
            }
            self.dom_after_blob_ref = None;
        }

        // screenshot_after_blob_ref → screenshot_after_hash downgrade.
        if !keep_field(scope, profile, CaptureField::ScreenshotAfterBlobRef) {
            if let Some(ref r) = self.screenshot_after_blob_ref {
                self.screenshot_after_hash = Some(r.sha256.clone());
            }
            self.screenshot_after_blob_ref = None;
        }

        // screencast_after_blob_ref → screencast_after_hash downgrade.
        if !keep_field(scope, profile, CaptureField::ScreencastAfterBlobRef) {
            if let Some(ref r) = self.screencast_after_blob_ref {
                self.screencast_after_hash = Some(r.sha256.clone());
            }
            self.screencast_after_blob_ref = None;
        }

        // Before-refs: plain strip.
        if !keep_field(scope, profile, CaptureField::DomBeforeBlobRef) {
            self.dom_before_blob_ref = None;
        }
        if !keep_field(scope, profile, CaptureField::ScreenshotBeforeBlobRef) {
            self.screenshot_before_blob_ref = None;
        }

        // Per-event body-ref strip; events themselves stay (counts +
        // metadata still meaningful under Minimal).
        if !keep_field(scope, profile, CaptureField::NetworkEventBodyRef) {
            for event in &mut self.network_events {
                event.response_body_ref = None;
            }
        }

        // Console lines: empty (not None — the field is `Vec`, not
        // `Option<Vec>`; absence is signalled by emptiness).
        if !keep_field(scope, profile, CaptureField::ConsoleLines) {
            self.console_lines.clear();
        }
    }
}
