//! Shared offload-or-inline logic for the observational per-request
//! `network_entries` side-channel.
//!
//! Both the navigate receipt (`host_function_table::host_impl`'s `navigate_execute`)
//! and the `loom.web.network_log` tool (`wasm_host::WasmHost::network_log`) serialize
//! the per-request `network_entries` list and either inline it (small) or offload it
//! to the content store (≥ 64 KB). This module is the single source of truth for that
//! threshold and for the **fail-open** graceful-degrade policy.
//!
//! Determinism (NFR-DET-01): this is the OBSERVATIONAL side-channel — it is NEVER part
//! of the replay hash chain / manifest `ReceiptPayload` (see
//! `loom_shared::navigate_outcome::LoomNetworkEntry` docs and
//! `loom_core::receipt_builder::capture_policy`). A failure here must never fail a
//! navigate, so serialize/put errors drop the list and force `truncated = true` rather
//! than propagating.
//!
//! NOTE: `evaluate_execute`'s return-value offload looks similar but is intentionally
//! NOT routed through here — it uses `>` (a tested boundary) and is fail-CLOSED
//! (propagates put errors). See `specs/2026-06-09-cleanup-network-entries-offload`.

use loom_core::content_store::{ContentRef, ContentStore};
use loom_shared::navigate_outcome::LoomNetworkEntry;

/// ≥ this many serialized bytes → offload to the content store; otherwise inline.
pub(crate) const NETWORK_ENTRIES_INLINE_THRESHOLD: usize = 65_536;

/// How the serialized `network_entries` list was handled.
pub(crate) enum NetworkEntriesPayload {
    /// Below threshold — carries the serialized JSON-array bytes for the caller to
    /// surface inline (as raw bytes or re-parsed into `Vec<serde_json::Value>`).
    Inline(Vec<u8>),
    /// At/above threshold and successfully stored — carries the core `ContentRef`.
    Offloaded(ContentRef),
    /// Serialize or put failed — caller emits an empty list; `truncated` is forced true.
    Dropped,
}

/// Serialize `entries` and inline (< 64 KB) or offload (≥ 64 KB) to `content_store`.
///
/// Returns the payload plus the **effective** truncated flag: `truncated_in` OR'd with
/// any drop that happened here. Never returns an error — a serialize/put failure is
/// logged (count + session only, no URLs) and degrades to [`NetworkEntriesPayload::Dropped`].
pub(crate) fn offload_or_inline_network_entries(
    content_store: &dyn ContentStore,
    entries: &[LoomNetworkEntry],
    truncated_in: bool,
    session_id: &str,
) -> (NetworkEntriesPayload, bool) {
    let bytes = match serde_json::to_vec(entries) {
        Ok(b) => b,
        Err(e) => {
            // Should not happen (all fields are JSON-trivial). Never silently drop —
            // warn (count + session only, no URLs) and flag truncation.
            tracing::warn!(
                session_id = %session_id,
                entry_count = entries.len(),
                error = %e,
                "network_entries serialization failed; dropping list"
            );
            return (NetworkEntriesPayload::Dropped, true);
        }
    };
    if bytes.len() >= NETWORK_ENTRIES_INLINE_THRESHOLD {
        match content_store.put(&bytes) {
            Ok(cref) => (NetworkEntriesPayload::Offloaded(cref), truncated_in),
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    entry_count = entries.len(),
                    error = %e,
                    "network_entries content-store offload failed; dropping list"
                );
                (NetworkEntriesPayload::Dropped, true)
            }
        }
    } else {
        (NetworkEntriesPayload::Inline(bytes), truncated_in)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_core::content_store::{ContentRef, ContentStore, GcReport};
    use loom_core::mocks::MockContentStore;
    use loom_shared::error_format::{LoomError, LoomErrorCode};
    use std::time::Duration;

    /// A `ContentStore` whose `put` always fails — exercises the fail-open degrade
    /// branch, which has no coverage elsewhere in the workspace (no failing-put mock
    /// existed before this).
    struct FailingContentStore;
    impl ContentStore for FailingContentStore {
        fn put(&self, _bytes: &[u8]) -> Result<ContentRef, LoomError> {
            Err(LoomError::new(
                LoomErrorCode::StoreFullNoEvictable,
                "test: content store is full",
            ))
        }
        fn get(&self, _r: &ContentRef) -> Result<Vec<u8>, LoomError> {
            Err(LoomError::new(LoomErrorCode::StoreNotFound, "test: empty"))
        }
        fn gc(&self, _ttl: Duration) -> Result<GcReport, LoomError> {
            Ok(GcReport {
                blobs_scanned: 0,
                blobs_collected: 0,
                bytes_freed: 0,
            })
        }
    }

    fn entry(url: String) -> LoomNetworkEntry {
        LoomNetworkEntry {
            url,
            method: "GET".to_string(),
            status: 200,
            resource_type: "xhr".to_string(),
            from_cache: false,
            request_id: "req-1".to_string(),
            ts_ms: 0,
        }
    }

    /// Build a single-entry list whose serialized `Vec<LoomNetworkEntry>` JSON is
    /// *exactly* `target` bytes, by padding the (escape-free, ASCII) `url` field.
    fn list_of_serialized_len(target: usize) -> Vec<LoomNetworkEntry> {
        let base = serde_json::to_vec(&vec![entry(String::new())])
            .unwrap()
            .len();
        assert!(target >= base, "target {target} below base {base}");
        vec![entry("a".repeat(target - base))]
    }

    #[test]
    fn inline_below_threshold_preserves_entries_and_truncated() {
        let store = MockContentStore::new();
        let entries = vec![entry("https://example.com/a".to_string())];
        let (payload, truncated) =
            offload_or_inline_network_entries(&*store, &entries, false, "sess");
        match payload {
            NetworkEntriesPayload::Inline(bytes) => {
                let round: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(round.len(), 1);
            }
            _ => panic!("expected Inline below threshold"),
        }
        assert!(!truncated);
    }

    #[test]
    fn inline_passes_through_incoming_truncated_flag() {
        let store = MockContentStore::new();
        let entries = vec![entry("x".to_string())];
        let (_payload, truncated) =
            offload_or_inline_network_entries(&*store, &entries, true, "sess");
        assert!(truncated, "incoming truncated=true must pass through");
    }

    #[test]
    fn offload_at_or_above_threshold_stores_blob() {
        let store = MockContentStore::new();
        let entries = list_of_serialized_len(NETWORK_ENTRIES_INLINE_THRESHOLD);
        let (payload, truncated) =
            offload_or_inline_network_entries(&*store, &entries, false, "sess");
        match payload {
            NetworkEntriesPayload::Offloaded(cref) => {
                assert_eq!(cref.sha256.len(), 64, "sha256 hex");
                assert!(store.get(&cref).is_ok(), "blob must be retrievable");
            }
            _ => panic!("expected Offloaded at threshold"),
        }
        assert!(!truncated, "successful offload does not set truncated");
    }

    #[test]
    fn boundary_is_inclusive_at_threshold() {
        let store = MockContentStore::new();
        // exactly threshold - 1 → inline
        let under = list_of_serialized_len(NETWORK_ENTRIES_INLINE_THRESHOLD - 1);
        let (p_under, _) = offload_or_inline_network_entries(&*store, &under, false, "s");
        assert!(matches!(p_under, NetworkEntriesPayload::Inline(_)));
        // exactly threshold → offload (>= boundary)
        let at = list_of_serialized_len(NETWORK_ENTRIES_INLINE_THRESHOLD);
        let (p_at, _) = offload_or_inline_network_entries(&*store, &at, false, "s");
        assert!(matches!(p_at, NetworkEntriesPayload::Offloaded(_)));
    }

    #[test]
    fn put_failure_degrades_to_dropped_and_truncated() {
        let store = FailingContentStore;
        let entries = list_of_serialized_len(NETWORK_ENTRIES_INLINE_THRESHOLD);
        let (payload, truncated) =
            offload_or_inline_network_entries(&store, &entries, false, "sess");
        assert!(
            matches!(payload, NetworkEntriesPayload::Dropped),
            "put failure must drop the list"
        );
        assert!(truncated, "put failure forces truncated=true");
    }

    /// Regression guard for the `network_log` adapter: re-parsing the helper's
    /// `Inline` bytes via `from_slice::<Vec<Value>>` must yield the exact same
    /// `Vec<serde_json::Value>` the OLD per-entry `to_value().collect()` produced.
    #[test]
    fn inline_bytes_reparse_equals_per_entry_to_value() {
        let store = MockContentStore::new();
        let entries = vec![
            entry("https://example.com/a?q=1".to_string()),
            entry("https://example.com/b".to_string()),
        ];
        let (payload, _) = offload_or_inline_network_entries(&*store, &entries, false, "s");
        let reparsed = match payload {
            NetworkEntriesPayload::Inline(bytes) => {
                serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).unwrap_or_default()
            }
            _ => panic!("expected Inline"),
        };
        // The exact transformation the old wasm_host inline branch performed.
        let per_entry: Vec<serde_json::Value> = entries
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect();
        assert_eq!(reparsed, per_entry);
    }
}
