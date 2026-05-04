// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/OutputFormatter/interfaces.rs` instead.
// OutputFormatter — SOLE writer to stdout.
//
// # Contract semantics
// - **IC-CLI-01 / AC-CLI-04.1.** Default path is `serde_jcs::to_string`
//   (RFC 8785 canonical JSON). Exactly one canonical-JSON object per
//   command. No ANSI color, no headers, no prose.
// - **IC-CLI-02 / AC-CLI-04.2.** `--pretty` delegates to
//   `PrettyRenderer`; default path never touches the renderer.
// - **SR-CLI-03.** Receipt fields flow verbatim. No field rewriting,
//   no field stripping, no prose augmentation. Clippy lint forbids
//   `Receipt::redact` calls in any handler module.
// - **Hard binding 3 (no floats).** `serde_jcs` rejects f32/f64;
//   clippy lint forbids float literals in any loom-cli module.

use crate::pretty_renderer::PrettyRenderer;
use crate::CliError;

/// Output destination — stdout for the default path; abstracted for
/// integration tests.
pub trait OutputSink: Send + Sync {
    /// Writes the bytes verbatim. No newline appended; the caller is
    /// responsible for the trailing `\n`.
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()>;
}

/// `OutputFormatter` carries the pretty-renderer reference and the
/// chosen sink. Stateless transformation otherwise.
pub struct OutputFormatter<'a, S: OutputSink> {
    pub(crate) sink: &'a mut S,
    pub(crate) renderer: Option<&'a PrettyRenderer<'a>>,
}

impl<'a, S: OutputSink> OutputFormatter<'a, S> {
    /// Construct with the default canonical-JSON path (no renderer).
    pub fn new(sink: &'a mut S) -> Self {
        Self {
            sink,
            renderer: None,
        }
    }

    /// Construct with `--pretty` enabled — the renderer is required.
    pub fn with_pretty(sink: &'a mut S, renderer: &'a PrettyRenderer<'a>) -> Self {
        Self {
            sink,
            renderer: Some(renderer),
        }
    }

    /// Write a receipt to stdout. The default path serialises `value`
    /// via `serde_jcs::to_string`, then writes a single trailing `\n`.
    /// The pretty path delegates to `PrettyRenderer::render(method, value)`.
    pub fn write(&mut self, method: &str, value: &serde_json::Value) -> Result<(), CliError> {
        let bytes = if let Some(renderer) = self.renderer {
            let rendered = renderer.render(method, value)?;
            format!("{}\n", rendered).into_bytes()
        } else {
            let s = Self::canonical_json(value)?;
            format!("{}\n", s).into_bytes()
        };
        self.sink
            .write_all(&bytes)
            .map_err(|e| CliError::Internal(format!("stdout write error: {}", e)))
    }

    /// Canonical-JSON helper used by tests + by the pretty path's
    /// fallback when a method has no schema in `SchemaCache`.
    pub fn canonical_json(value: &serde_json::Value) -> Result<String, CliError> {
        serde_jcs::to_string(value)
            .map_err(|e| CliError::Internal(format!("canonical JSON serialisation failed: {}", e)))
    }
}

/// Render `value` for direct stdout use by handlers (AC-CLI-04.1 / AC-CLI-04.2).
///
/// `pretty=false` (default) yields RFC 8785 canonical JSON on a single line.
/// `pretty=true` yields multi-line indented JSON via `serde_json::to_string_pretty`.
///
/// Keeps the canonical-vs-pretty decision in one place so handlers don't
/// re-derive it (and don't accidentally call `to_string_pretty` directly,
/// which silently broke the default for every subcommand).
pub fn format_output(value: &serde_json::Value, pretty: bool) -> Result<String, CliError> {
    if pretty {
        serde_json::to_string_pretty(value)
            .map_err(|e| CliError::Internal(format!("pretty JSON serialisation failed: {}", e)))
    } else {
        serde_jcs::to_string(value)
            .map_err(|e| CliError::Internal(format!("canonical JSON serialisation failed: {}", e)))
    }
}

/// Default stdout sink. Wraps `std::io::stdout().lock()` to ensure
/// atomic single-line writes.
pub struct StdoutSink;

impl OutputSink for StdoutSink {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        std::io::stdout().lock().write_all(bytes)
    }
}
