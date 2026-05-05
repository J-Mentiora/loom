// Reject mutually-exclusive output flags before they reach the resolver.
//
// Per D-7: `--json` and `--pretty` together → exit 2 Usage.
// Per D-31: `--color X` and `--no-color` together → exit 2 Usage.
//
// Both checks examine raw flag presence (not resolved values) so the
// usage error fires even when both flags would reduce to the same value.

use super::command_router::Cli;
use crate::CliError;

/// Reject conflicting output-mode flags. Caller is expected to invoke
/// this immediately after `Cli::try_parse_from` and before constructing
/// the resolved `CliConfig`.
pub fn validate_flags(cli: &Cli) -> Result<(), CliError> {
    if cli.json && cli.pretty {
        return Err(CliError::Usage(
            "--json and --pretty are mutually exclusive".into(),
        ));
    }
    if cli.color.is_some() && cli.no_color {
        return Err(CliError::Usage(
            "--color and --no-color are mutually exclusive".into(),
        ));
    }
    Ok(())
}
