// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/HelpGenerator/interface_tests.rs` instead.
// Interface tests for `HelpGenerator`. Verifies IC-CLI-09 mechanical
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

// === IC-CLI-09: mechanical field-name conversion ===
//
// `json_field_to_clap_arg` and `clap_arg_to_json_field` must be
// inverse functions for the parity test to be byte-equal. Phase 5.4
// asserts the round-trip; here we lock the signatures.
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
// parity test once Phase 5.4 implements the conversion.
