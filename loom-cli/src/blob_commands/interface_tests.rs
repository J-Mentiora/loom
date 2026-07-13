// D2 output-resolution matrix for `loom blob get` (voice-call-io AC11).

use std::path::PathBuf;

use super::blob_commands::{resolve_output, OutputTarget};
use crate::CliError;

#[test]
fn explicit_file_path_wins_regardless_of_tty() {
    for tty in [true, false] {
        let got = resolve_output(Some(PathBuf::from("out.wav")), tty).unwrap();
        assert_eq!(got, OutputTarget::File(PathBuf::from("out.wav")));
    }
}

#[test]
fn dash_sentinel_forces_stdout_even_on_a_tty() {
    for tty in [true, false] {
        let got = resolve_output(Some(PathBuf::from("-")), tty).unwrap();
        assert_eq!(got, OutputTarget::Stdout);
    }
}

#[test]
fn bare_invocation_on_a_pipe_streams_to_stdout() {
    let got = resolve_output(None, false).unwrap();
    assert_eq!(got, OutputTarget::Stdout);
}

#[test]
fn bare_invocation_on_a_terminal_is_a_usage_refusal() {
    let err = resolve_output(None, true).unwrap_err();
    match err {
        CliError::Usage(msg) => {
            assert!(msg.contains("-o <file>"), "must name the remedy: {msg}");
            assert!(msg.contains("redirect"), "must mention redirect: {msg}");
        }
        other => panic!("expected Usage (exit 2), got {other:?}"),
    }
}
