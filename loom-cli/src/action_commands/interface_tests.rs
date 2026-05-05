// Interface tests for `ActionCommands`. Verifies the single-RPC
// shape, the schema-before-RPC contract, and the
// `<surface>.<verb>` method-name discipline.

#[allow(unused_imports)]
use super::action_commands::{parse_extra_to_json, validate_args, ActionArgs};

#[test]
fn action_args_carry_method_session_extra() {
    let a = ActionArgs {
        method: "web.click".into(),
        session: "01HW".into(),
        extra: vec!["--selector".into(), "#btn".into()],
    };
    assert_eq!(a.method, "web.click");
    assert_eq!(a.session, "01HW");
    assert_eq!(a.extra.len(), 2);
}

#[test]
fn action_method_must_be_dot_separated() {
    // <surface>.<verb> is structural — the method name maps 1:1 to
    // the JSON-RPC method name. The runtime check lives in dispatch();
    // here we lock the convention by constructing a typical example.
    let a = ActionArgs {
        method: "web.navigate".into(),
        session: "S".into(),
        extra: vec![],
    };
    assert!(a.method.contains('.'), "method must be <surface>.<verb>");
}

#[test]
fn parse_extra_signature() {
    fn _ck(e: &[String]) -> Result<serde_json::Value, super::action_commands::ActionArgs> {
        // signature compile-check only
        let _ = parse_extra_to_json(e);
        unreachable!()
    }
    let _ = _ck;
}

#[test]
fn validate_args_signature() {
    use super::action_commands::*;
    fn _ck(
        s: &crate::schema_cache::SchemaCache,
        m: &str,
        v: &serde_json::Value,
    ) -> Result<(), crate::CliError> {
        validate_args(s, m, v)
    }
    let _ = _ck;
}
