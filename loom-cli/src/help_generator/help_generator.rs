// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/HelpGenerator/interfaces.rs` instead.
// HelpGenerator — derives `--help` text from clap definitions +
// `SchemaCache` field descriptions.
//
// # Contract semantics
// - **clap arg names** are mechanically
//   derived from JSON-Schema field names so
//   `loom session create --help` mirrors `session.create` field names
//   exactly. CI parity test
//   (`tools/test-help-schema-parity`) asserts byte-equal name match
//   between clap derive output and `rpc.schemas` field names.
// - **No hand-curated descriptions.** Description text is taken from
//   the schema's `description` field; clap doc comments are
//   forbidden in handler modules (clippy lint).

use crate::schema_cache::SchemaCache;
use crate::CliError;

/// Derived help fragment for one method. Used by the CI parity test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodHelp {
    pub method: String,
    /// Field names in JSON-Schema order. clap arg names MUST match
    /// (kebab-case mechanical conversion of snake_case).
    pub fields: Vec<String>,
    pub description: String,
}

/// Generates the help-text bundle from the schema cache. Pure function.
pub fn generate(_schemas: &SchemaCache) -> Result<Vec<MethodHelp>, CliError> {
    // Full schema-driven generation is a future CI parity task.
    Ok(vec![])
}

/// Mechanical conversion: snake_case JSON-Schema field name →
/// kebab-case clap arg name. Used by the CI parity test
/// (`tools/test-help-schema-parity`).
pub fn json_field_to_clap_arg(field: &str) -> String {
    field.replace('_', "-")
}

/// Inverse of `json_field_to_clap_arg` — kebab-case clap arg →
/// snake_case JSON field. Both directions are pure.
pub fn clap_arg_to_json_field(arg: &str) -> String {
    arg.replace('-', "_")
}
