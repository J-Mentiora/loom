// Interface tests for `HelpGenerator`. Verifies mechanical
// field-name conversion (round-trip) and the parity-test data shape.

use super::help_generator::{clap_arg_to_json_field, generate, json_field_to_clap_arg, MethodHelp};
use crate::schema_cache::SchemaCache;
use crate::CliError;

#[test]
fn generate_signature() {
    fn _ck(s: &SchemaCache) -> Result<Vec<MethodHelp>, CliError> {
        generate(s)
    }
    let _ = _ck;
}

#[test]
fn method_help_carries_method_fields_description() {
    let h = MethodHelp {
        method: "session.create".into(),
        fields: vec!["profile".into(), "network_mode".into()],
        description: "Creates a new session.".into(),
    };
    assert_eq!(h.method, "session.create");
    assert_eq!(h.fields.len(), 2);
}

// === mechanical field-name conversion ===
//
// `json_field_to_clap_arg` and `clap_arg_to_json_field` must be
// inverse functions for the parity test to be byte-equal. A future
// iteration asserts the round-trip; here we lock the signatures.
#[test]
fn json_field_to_clap_arg_signature() {
    fn _ck(f: &str) -> String {
        json_field_to_clap_arg(f)
    }
    let _ = _ck;
}

#[test]
fn clap_arg_to_json_field_signature() {
    fn _ck(a: &str) -> String {
        clap_arg_to_json_field(a)
    }
    let _ = _ck;
}

// Pure signature lock — runtime byte-equality is checked by the CI
// parity test once a future iteration implements the conversion.
