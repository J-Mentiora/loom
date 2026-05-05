// PrettyRenderer — schema-driven tabular renderer for `--pretty`.
//
// # Contract semantics
// - Reads the response schema from
//   `SchemaCache` (populated from `rpc.schemas` once at startup) and
//   derives the column list mechanically from JSON-Schema field
//   order. No hand-rolled per-method tables. Clippy lint forbids
//   hand-rolled `Vec<&str>` column lists in handler modules.
// - **NO_COLOR honoured.** The renderer respects the `NO_COLOR`
//   environment variable and emits plain ASCII when set.
// - **TERM=dumb honoured.** Same fallback path.
// - **Fallback.** If the method has no schema in `SchemaCache`, the
//   renderer falls back to `serde_jcs::to_string` and emits a single
//   stderr warning (`PrettyRenderer: no schema for <method>;
//   degraded to canonical JSON`).

use crate::schema_cache::SchemaCache;
use crate::CliError;

/// Renderer instance. Borrows the `SchemaCache` for the lifetime of
/// the CLI invocation.
pub struct PrettyRenderer<'a> {
    pub(crate) schemas: &'a SchemaCache,
    pub(crate) color_enabled: bool,
}

impl<'a> PrettyRenderer<'a> {
    /// Construct from a `SchemaCache` reference. Determines color
    /// enablement from `NO_COLOR` / `TERM=dumb` at construction time.
    pub fn new(schemas: &'a SchemaCache) -> Self {
        Self::with_color(schemas, detect_color_enabled())
    }

    /// Force color enablement (used by tests; production goes through
    /// `new`).
    pub fn with_color(schemas: &'a SchemaCache, color_enabled: bool) -> Self {
        Self {
            schemas,
            color_enabled,
        }
    }

    /// Render a value as a tabular string. Looks up the response
    /// schema for `method` in `SchemaCache`; derives columns from the
    /// schema's `properties` order. Falls back to canonical JSON +
    /// stderr warning if the schema is missing.
    pub fn render(&self, method: &str, value: &serde_json::Value) -> Result<String, CliError> {
        let schema = self.schemas.response_schema(method);
        let columns = match schema {
            Some(s) => Self::columns_from_schema(s),
            None => {
                eprintln!(
                    "PrettyRenderer: no schema for {}; degraded to canonical JSON",
                    method
                );
                return serde_jcs::to_string(value).map_err(|e| {
                    CliError::Internal(format!("canonical JSON serialisation failed: {}", e))
                });
            }
        };

        if columns.is_empty() {
            return serde_jcs::to_string(value).map_err(|e| {
                CliError::Internal(format!("canonical JSON serialisation failed: {}", e))
            });
        }

        let mut lines: Vec<String> = Vec::new();
        // color_enabled controls ANSI output (NO_COLOR / TERM=dumb compliance).
        // Bold header reserved for future enhancement; suppressed when !color_enabled.
        let _ = self.color_enabled;
        // One key-value pair per line: schema-derived column order.
        for col in &columns {
            let v = value.get(col).unwrap_or(&serde_json::Value::Null);
            let v_str = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "null".to_string(),
                other => other.to_string(),
            };
            lines.push(format!("{}: {}", col, v_str));
        }
        Ok(lines.join("\n"))
    }

    /// Pure helper: derive column names from a JSON-Schema object.
    /// Returns an empty `Vec` if the schema has no `properties` field.
    pub fn columns_from_schema(schema: &serde_json::Value) -> Vec<String> {
        schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default()
    }
}

/// Reads `NO_COLOR` and `TERM` to determine color enablement.
/// Public so `interface_tests` can lock the env-var contract.
///
/// Per the no-color.org spec: NO_COLOR disables color only when set to a
/// **non-empty** value. The previous implementation's `is_ok()` check
/// treated `NO_COLOR=""` as a disable signal, contrary to spec.
pub fn detect_color_enabled() -> bool {
    if std::env::var("NO_COLOR")
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("TERM").ok().as_deref() == Some("dumb") {
        return false;
    }
    true
}
