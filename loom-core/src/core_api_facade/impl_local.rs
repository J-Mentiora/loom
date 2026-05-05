// list_sessions_info_from_dir — disk scan for session listing.
//
// Called by CoreServiceAdapter via CoreFacadeBridge::list_sessions_info.
// Walks sessions_root and reads each session's manifest.wal to determine
// status. Crashed sessions (last entry = RuntimeCrash) and properly closed
// sessions (last entry = SessionTerminal reason=close) are both surfaced.

use crate::error::LoomError;
use crate::manifest_writer::ManifestEntry;

/// Scan `sessions_root` and return `(session_id, status, created_at_ms)` for
/// every session directory that contains a `manifest.wal`.
///
/// Status mapping:
/// - Last WAL entry is `RuntimeCrash`              → `"crashed"`
/// - Last WAL entry is `SessionTerminal { reason="close" }`     → `"closed"`
/// - Last WAL entry is `SessionTerminal { reason=other }`       → `"aborted:<reason>"`
///   (bridge layer splits this back into status+reason
///   for the `SessionInfo` wire shape)
/// - No terminal entry                              → `"active"`
pub fn list_sessions_info_from_dir(
    sessions_root: &std::path::Path,
) -> Result<Vec<(String, String, u64)>, LoomError> {
    let mut result = Vec::new();

    let dir = match std::fs::read_dir(sessions_root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(LoomError::from(e)),
    };

    for entry in dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let session_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let wal_path = path.join("manifest.wal");
        let content = match std::fs::read_to_string(&wal_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut created_at_ms: u64 = 0;
        let mut status = "active".to_string();

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            match &entry {
                ManifestEntry::Header { started_at_ms, .. } => {
                    created_at_ms = *started_at_ms;
                }
                ManifestEntry::SessionTerminal { reason, .. } => {
                    // Map the WAL reason to the external-facing status string.
                    // The bridge layer pulls "aborted:<x>" apart so the wire
                    // SessionInfo carries status="aborted" and reason="x".
                    status = match reason.as_str() {
                        "close" => "closed".to_string(),
                        // Heuristic: anything that isn't "close" or known
                        // lifecycle-only markers ("replay_complete") is an
                        // operator-initiated abort. Encode the reason in
                        // the status string so the (status, created_at_ms)
                        // tuple shape doesn't need to widen.
                        "replay_complete" => "replay_complete".to_string(),
                        other => format!("aborted:{other}"),
                    };
                }
                ManifestEntry::RuntimeCrash { .. } => {
                    status = "crashed".to_string();
                }
                _ => {}
            }
        }

        result.push((session_id, status, created_at_ms));
    }

    // Stable order: most-recent-first by created_at_ms (Header's
    // started_at_ms), tiebreak by session_id. Without this the listing
    // reflects filesystem read_dir order which on macOS HFS/APFS is
    // hash-based and varies across boots, making `loom session list`
    // non-deterministic and a poor citizen for scripts that compare
    // adjacent invocations.
    result.sort_by(|(a_id, _, a_ts), (b_id, _, b_ts)| b_ts.cmp(a_ts).then_with(|| a_id.cmp(b_id)));

    Ok(result)
}
