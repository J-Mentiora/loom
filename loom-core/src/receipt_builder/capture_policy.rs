// capture_policy — single source of truth for `--capture-policy` field
// inclusion across the manifest receipt and the wire receipt.
//
// # Why one decision table
//
// Two existing transforms (`ReceiptPayload::apply_capture_profile` for
// the canonical-JSON manifest path; the new
// `Receipt::apply_capture_profile_to_wire` in loom-rpc) operate on
// different structs but enforce overlapping rules. Without a shared
// source of truth the two enforcers drift — manifest could keep a
// field that wire strips, or vice versa, with no compile-time guard.
// `keep_field` here is the only place a (scope, profile, field)
// decision is made; both enforcers consult it field-by-field.
//
// The exhaustiveness test at the bottom ensures every `(CaptureScope,
// CaptureProfile, CaptureField)` triple has an explicit answer — no
// `_ => true` fallback. Adding a new wire field forces a new arm.

use super::receipt_builder::CaptureProfile;

/// Where this enforcer runs. Manifest path keeps tier-1 hashes on
/// Minimal (Minimal strips blobs but keeps `*_hash` references —
/// see `apply_capture_profile`); the brief makes the wire-path
/// Minimal stricter ("only `url`, `status_code`, `error`").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureScope {
    Manifest,
    Wire,
}

/// One enum variant per receipt field that could be capture-policy
/// gated. Identity fields (`action_id`, `session_id`, `status`,
/// `timing_ticks`) are unconditional and not enumerated — they appear
/// on every receipt regardless of profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureField {
    // Wire-receipt fields
    Url,
    FinalUrl,
    Title,
    StatusCode,
    DomSnapshotHash,
    ScreenshotAfterHash,
    ScreencastAfterHash,
    ConsoleCount,
    ConsoleLines,
    NetworkCount,
    NetworkSummary,
    SideEffects,
    /// Observational per-request `network_entries` (+ blob_ref + truncated).
    /// Wire-only; never on the manifest `ReceiptPayload`.
    NetworkEntries,
    ActionHash,
    OutcomeHash,
    EmittedAtMs,
    ErrorDetail,
    // Manifest-receipt fields (additional, not on wire)
    DomBeforeBlobRef,
    ScreenshotBeforeBlobRef,
    DomAfterBlobRef,
    ScreenshotAfterBlobRef,
    ScreencastAfterBlobRef,
    NetworkEvents,
    NetworkEventBodyRef,
}

/// Decide whether `field` should be present on a receipt under the
/// given (scope, profile). `true` = include; `false` = strip / set to
/// None / set to empty.
pub fn keep_field(scope: CaptureScope, profile: CaptureProfile, field: CaptureField) -> bool {
    use CaptureField as F;
    use CaptureProfile as P;
    use CaptureScope as S;

    match (scope, profile, field) {
        // ───────────── Wire scope ──────────────────────────────────────
        // Wire + Default: full tier-2 (everything that's set stays set).
        (S::Wire, P::Default, _) => true,

        // Wire + Full: same as Default today; future `dom_full_text`
        // toggle lands here without touching the wire `Receipt` struct.
        (S::Wire, P::Full, _) => true,

        // Wire + Fingerprint: Default field-keeping (keep everything that's set).
        // The interaction `dom_after_hash` is presence-gated at the host (only
        // produced under this tier), so it simply rides through here; no per-field
        // strip arm is needed (a non-fingerprint session never carries it).
        (S::Wire, P::Fingerprint, _) => true,

        // Wire + Minimal (brief): keep only `url`, `status_code`,
        // `error.detail`. Strip everything else. Identity fields
        // (action_id/session_id/status/timing_ticks) are unconditional
        // and not enumerated.
        (S::Wire, P::Minimal, F::Url) => true,
        (S::Wire, P::Minimal, F::StatusCode) => true,
        (S::Wire, P::Minimal, F::ErrorDetail) => true,
        (S::Wire, P::Minimal, F::FinalUrl) => false,
        (S::Wire, P::Minimal, F::Title) => false,
        (S::Wire, P::Minimal, F::DomSnapshotHash) => false,
        (S::Wire, P::Minimal, F::ScreenshotAfterHash) => false,
        (S::Wire, P::Minimal, F::ScreencastAfterHash) => false,
        (S::Wire, P::Minimal, F::ConsoleCount) => false,
        (S::Wire, P::Minimal, F::ConsoleLines) => false,
        (S::Wire, P::Minimal, F::NetworkCount) => false,
        (S::Wire, P::Minimal, F::NetworkSummary) => false,
        (S::Wire, P::Minimal, F::SideEffects) => false,
        (S::Wire, P::Minimal, F::NetworkEntries) => false,
        (S::Wire, P::Minimal, F::ActionHash) => false,
        (S::Wire, P::Minimal, F::OutcomeHash) => false,
        (S::Wire, P::Minimal, F::EmittedAtMs) => false,
        // Manifest-only fields under Wire scope are inapplicable; treat
        // as "do not include" defensively (they shouldn't exist on the
        // wire `Receipt` struct anyway, so no enforcer ever queries
        // them in Wire scope — but the exhaustiveness test below still
        // requires an answer).
        (S::Wire, P::Minimal, F::DomBeforeBlobRef) => false,
        (S::Wire, P::Minimal, F::ScreenshotBeforeBlobRef) => false,
        (S::Wire, P::Minimal, F::DomAfterBlobRef) => false,
        (S::Wire, P::Minimal, F::ScreenshotAfterBlobRef) => false,
        (S::Wire, P::Minimal, F::ScreencastAfterBlobRef) => false,
        (S::Wire, P::Minimal, F::NetworkEvents) => false,
        (S::Wire, P::Minimal, F::NetworkEventBodyRef) => false,

        // ───────────── Manifest scope ──────────────────────────────────
        // Manifest + Default: keep tier-assigned fields. `*BeforeBlobRef`
        // is the Full-only override; not present in Default.
        (S::Manifest, P::Default, F::DomBeforeBlobRef) => false,
        (S::Manifest, P::Default, F::ScreenshotBeforeBlobRef) => false,
        (S::Manifest, P::Default, _) => true,

        // Manifest + Full: keep everything including
        // before-refs. The caller is responsible for populating
        // `*BeforeBlobRef` on the struct before invoking the enforcer.
        (S::Manifest, P::Full, _) => true,

        // Manifest + Fingerprint: identical to Default (tier-assigned fields,
        // no Full-only before-refs). The interaction `dom_after_hash` rides the
        // interaction receipt's default canonical path (not a ReceiptPayload
        // field), so it is not gated here.
        (S::Manifest, P::Fingerprint, F::DomBeforeBlobRef) => false,
        (S::Manifest, P::Fingerprint, F::ScreenshotBeforeBlobRef) => false,
        (S::Manifest, P::Fingerprint, _) => true,

        // Manifest + Minimal: hash-only. Strip blob
        // refs; downgrade to hash-only navigate fields; empty
        // console_lines; strip network event body refs (keep
        // hash/metadata).
        (S::Manifest, P::Minimal, F::DomBeforeBlobRef) => false,
        (S::Manifest, P::Minimal, F::ScreenshotBeforeBlobRef) => false,
        (S::Manifest, P::Minimal, F::DomAfterBlobRef) => false,
        (S::Manifest, P::Minimal, F::ScreenshotAfterBlobRef) => false,
        // Screencast: Minimal strips the blob_ref (downgrade to hash), keeps the hash.
        (S::Manifest, P::Minimal, F::ScreencastAfterBlobRef) => false,
        (S::Manifest, P::Minimal, F::ConsoleLines) => false,
        (S::Manifest, P::Minimal, F::NetworkEventBodyRef) => false,
        // Hash-only payloads + per-event metadata (status, url,
        // method, timing) survive Minimal — only the *bodies* are
        // stripped, per Minimal wording "no `network_events[]`
        // bodies, only counts" interpreted as "keep counts + metadata,
        // strip body refs".
        (S::Manifest, P::Minimal, F::Url) => true,
        (S::Manifest, P::Minimal, F::FinalUrl) => true,
        (S::Manifest, P::Minimal, F::Title) => true,
        (S::Manifest, P::Minimal, F::StatusCode) => true,
        (S::Manifest, P::Minimal, F::DomSnapshotHash) => true,
        (S::Manifest, P::Minimal, F::ScreenshotAfterHash) => true,
        (S::Manifest, P::Minimal, F::ScreencastAfterHash) => true,
        (S::Manifest, P::Minimal, F::ConsoleCount) => true,
        (S::Manifest, P::Minimal, F::NetworkCount) => true,
        (S::Manifest, P::Minimal, F::NetworkSummary) => true,
        // network_entries is wire-only — never on the manifest ReceiptPayload.
        // No manifest enforcer queries it; answer defensively (irrelevant).
        (S::Manifest, P::Minimal, F::NetworkEntries) => false,
        (S::Manifest, P::Minimal, F::NetworkEvents) => true,
        (S::Manifest, P::Minimal, F::SideEffects) => true,
        (S::Manifest, P::Minimal, F::ActionHash) => true,
        (S::Manifest, P::Minimal, F::OutcomeHash) => true,
        (S::Manifest, P::Minimal, F::EmittedAtMs) => true,
        (S::Manifest, P::Minimal, F::ErrorDetail) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (scope, profile, field) triple has an explicit answer.
    /// If a future field lands without a `keep_field` arm, this test
    /// fails to compile (non-exhaustive match) — there's no `_ =>
    /// true` fallback to silently include things.
    #[test]
    fn keep_field_decision_table_is_exhaustive() {
        let scopes = [CaptureScope::Manifest, CaptureScope::Wire];
        let profiles = [
            CaptureProfile::Default,
            CaptureProfile::Full,
            CaptureProfile::Minimal,
            CaptureProfile::Fingerprint,
        ];
        let fields = [
            CaptureField::Url,
            CaptureField::FinalUrl,
            CaptureField::Title,
            CaptureField::StatusCode,
            CaptureField::DomSnapshotHash,
            CaptureField::ScreenshotAfterHash,
            CaptureField::ScreencastAfterHash,
            CaptureField::ConsoleCount,
            CaptureField::ConsoleLines,
            CaptureField::NetworkCount,
            CaptureField::NetworkSummary,
            CaptureField::SideEffects,
            CaptureField::NetworkEntries,
            CaptureField::ActionHash,
            CaptureField::OutcomeHash,
            CaptureField::EmittedAtMs,
            CaptureField::ErrorDetail,
            CaptureField::DomBeforeBlobRef,
            CaptureField::ScreenshotBeforeBlobRef,
            CaptureField::DomAfterBlobRef,
            CaptureField::ScreenshotAfterBlobRef,
            CaptureField::ScreencastAfterBlobRef,
            CaptureField::NetworkEvents,
            CaptureField::NetworkEventBodyRef,
        ];
        for &scope in &scopes {
            for &profile in &profiles {
                for &field in &fields {
                    // Just calling it asserts the match is exhaustive;
                    // the boolean we get back is asserted in the
                    // semantic tests below.
                    let _ = keep_field(scope, profile, field);
                }
            }
        }
    }

    /// Wire + Minimal: only Url, StatusCode, ErrorDetail survive.
    /// (Brief specifies "url, status_code, error".)
    #[test]
    fn wire_minimal_keeps_only_brief_listed_fields() {
        for &field in &[
            CaptureField::Url,
            CaptureField::StatusCode,
            CaptureField::ErrorDetail,
        ] {
            assert!(
                keep_field(CaptureScope::Wire, CaptureProfile::Minimal, field),
                "Wire+Minimal should KEEP {field:?}"
            );
        }
        for &field in &[
            CaptureField::FinalUrl,
            CaptureField::Title,
            CaptureField::DomSnapshotHash,
            CaptureField::ScreenshotAfterHash,
            CaptureField::ConsoleCount,
            CaptureField::ConsoleLines,
            CaptureField::NetworkCount,
            CaptureField::NetworkSummary,
            CaptureField::SideEffects,
            CaptureField::NetworkEntries,
            CaptureField::ActionHash,
            CaptureField::OutcomeHash,
            CaptureField::EmittedAtMs,
        ] {
            assert!(
                !keep_field(CaptureScope::Wire, CaptureProfile::Minimal, field),
                "Wire+Minimal should STRIP {field:?}"
            );
        }
    }

    /// Wire + Default: everything stays. Wire + Full: same today.
    #[test]
    fn wire_default_and_full_keep_everything() {
        let wire_fields = [
            CaptureField::Url,
            CaptureField::FinalUrl,
            CaptureField::Title,
            CaptureField::StatusCode,
            CaptureField::DomSnapshotHash,
            CaptureField::ScreenshotAfterHash,
            CaptureField::ConsoleCount,
            CaptureField::ConsoleLines,
            CaptureField::NetworkCount,
            CaptureField::NetworkSummary,
            CaptureField::SideEffects,
            CaptureField::NetworkEntries,
            CaptureField::ActionHash,
            CaptureField::OutcomeHash,
            CaptureField::EmittedAtMs,
            CaptureField::ErrorDetail,
        ];
        for &profile in &[CaptureProfile::Default, CaptureProfile::Full] {
            for &field in &wire_fields {
                assert!(
                    keep_field(CaptureScope::Wire, profile, field),
                    "Wire+{profile:?} should KEEP {field:?}"
                );
            }
        }
    }

    /// Manifest + Minimal preserves the existing
    /// `apply_capture_profile` behavior: blob refs stripped,
    /// console_lines emptied, body refs dropped, but hash-only
    /// payloads + counts + per-event metadata survive.
    #[test]
    fn manifest_minimal_matches_legacy_apply_capture_profile_semantics() {
        // Stripped:
        for &field in &[
            CaptureField::DomBeforeBlobRef,
            CaptureField::ScreenshotBeforeBlobRef,
            CaptureField::DomAfterBlobRef,
            CaptureField::ScreenshotAfterBlobRef,
            CaptureField::ConsoleLines,
            CaptureField::NetworkEventBodyRef,
        ] {
            assert!(
                !keep_field(CaptureScope::Manifest, CaptureProfile::Minimal, field),
                "Manifest+Minimal should STRIP {field:?}"
            );
        }
        // Kept:
        for &field in &[
            CaptureField::DomSnapshotHash,
            CaptureField::ScreenshotAfterHash,
            CaptureField::NetworkEvents,
            CaptureField::ConsoleCount,
            CaptureField::NetworkCount,
        ] {
            assert!(
                keep_field(CaptureScope::Manifest, CaptureProfile::Minimal, field),
                "Manifest+Minimal should KEEP {field:?}"
            );
        }
    }
}
