# Research notes — Pretty TTY Output (loom-cli)

## Search 1: TTY conventions in popular CLIs (kubectl, gh, docker, etc.)

**Query**: `CLI auto-detect TTY stdout pretty json kubectl gh docker conventions 2025 --no-color --quiet`

**Key findings**:
- kubectl explicitly recommends machine-oriented `-o name|json|yaml|jsonpath|go-template` for scripts; default human output is NOT a stable contract ([kubectl conventions](https://kubernetes.io/docs/reference/kubectl/conventions/)).
- `gh` honors `NO_COLOR` (any non-empty value), `CLICOLOR=0` (disable), `CLICOLOR_FORCE!=0` (force), plus its own `GH_FORCE_TTY` to force pretty output when piped ([gh environment](https://cli.github.com/manual/gh_help_environment)).
- `gh pr create` prints just the PR URL on stdout — same at TTY and pipe. Pretty banners/spinners go to stderr ([gh_pr_create](https://cli.github.com/manual/gh_pr_create)).
- `docker ps -q` / `docker images -q` print only IDs on stdout — the canonical "primary identifier under quiet" pattern ([docker container ls](https://docs.docker.com/reference/cli/docker/container/ls/)).

**Convention table**

| Tool    | TTY default | --json flag      | --no-color flag                | --quiet semantic                  | NO_COLOR honored |
|---------|-------------|------------------|--------------------------------|-----------------------------------|------------------|
| kubectl | tabular     | `-o json`        | none (env only)                | none standard                     | yes (modern)     |
| gh      | pretty+ANSI | `--json <fields>`| via `NO_COLOR`/`CLICOLOR=0`    | URL/ID to stdout, banners→stderr  | yes              |
| docker  | tabular     | `--format json`  | via `NO_COLOR`                 | `-q` → bare IDs on stdout         | yes              |
| cargo   | pretty+ANSI | n/a              | `--color=never`                | `-q` suppresses progress only     | yes              |
| jq      | colored     | n/a (is JSON)    | `-M` / `--monochrome-output`   | n/a                               | yes              |
| ripgrep | colored     | `--json`         | `--color=never`                | `-q` exit-code only, no stdout    | yes              |

**Relevance to loom-cli AC-TTY-01..04**: Adopt the gh/docker split — pretty + ANSI when stdout is a TTY; bare machine value (id/url) on stdout when piped or under `--quiet`. Send progress/banners to stderr so they survive `cmd | jq`. Support `--color=auto|always|never` (cargo style) plus `NO_COLOR`/`CLICOLOR_FORCE` env. Add `--json` for explicit machine output that ignores TTY state.

## Search 2: Rust ecosystem (IsTerminal, color crates, NO_COLOR spec)

**Current recommendation as of 2026-05**: use `std::io::IsTerminal` (stdlib, 1.70+) for detection and `anstream` + `anstyle` for output — `anstream` auto-strips ANSI on non-TTY, file, `TERM=dumb`, or `NO_COLOR`, and clap already depends on `anstyle`, so no extra surface ([anstream blog](https://epage.github.io/blog/2023/03/anstream-simplifying-terminal-styling/)).

**Query**: `Rust std::io::IsTerminal 1.70 anstyle owo-colors NO_COLOR spec 2025 best practice`

**Key findings**:
- `atty` is officially deprecated; the README directs users to `std::io::IsTerminal` ([atty repo](https://github.com/softprops/atty)).
- `IsTerminal` gotchas: returns `false` on unknown platforms or invalid FDs; uses MSYS/Cygwin device-name heuristic (`msys-*-pty`, `cygwin-*-pty`); mintty edge cases may slip ([std docs](https://doc.rust-lang.org/std/io/trait.IsTerminal.html)).
- NO_COLOR spec is exact: "present and **not an empty string** (regardless of value)" — `NO_COLOR=""` does NOT disable ([no-color.org](https://no-color.org/)).
- CLICOLOR precedence: `NO_COLOR` > `CLICOLOR_FORCE!=0` (force on, even when piped) > `CLICOLOR=0` (off) > default ([bixense CLICOLOR](https://bixense.com/clicolors/)).
- Sunshowers' Rust CLI guide recommends `owo-colors` over `termcolor` (termcolor targets deprecated Windows Console APIs) ([rust-cli-recommendations](https://rust-cli-recommendations.sunshowers.io/managing-colors-in-rust.html)). For a clap-derive crate, `anstyle`/`anstream` is the lower-friction choice since it's already in the dep tree.
- Basic 16-color (SGR 30–37, 90–97) is universally safe on any modern terminal/Windows 10+; reserve 256-color/truecolor for opt-in features.

**Libraries comparison**

| Name         | Health       | License     | Notes                                                     |
|--------------|--------------|-------------|-----------------------------------------------------------|
| `atty`       | DEPRECATED   | MIT         | README points to `IsTerminal`                             |
| `is-terminal`| maint. mode  | MIT         | Stdlib polyfill; unneeded on 1.70+                        |
| `anstyle`    | active       | MIT/Apache  | Already a clap transitive dep; minimal, no_std-friendly   |
| `anstream`   | active       | MIT/Apache  | Auto-strips on non-TTY/`NO_COLOR`/`TERM=dumb`; ~10× faster strip than alternatives |
| `owo-colors` | active       | MIT         | Ergonomic; MSRV 1.70; respects NO_COLOR/FORCE_COLOR/CI    |
| `colored`    | maintained   | MPL-2.0     | MPL license can be a pain; ergonomics worse than owo      |
| `termcolor`  | maintained   | MIT/Unlic.  | Targets deprecated Windows Console APIs — avoid for new code |
| `nu-ansi-term`| active      | MIT         | Fork of `ansi_term`; fine but redundant with anstyle      |

**Relevance**: Since loom-cli is on Rust 1.92 with clap-derive, use `std::io::IsTerminal` + `anstream::AutoStream` wrapping `io::stdout()`, and emit `anstyle::Style` codes. No new top-level deps. If you'd rather hand-roll ANSI bytes, gate every write behind a single `should_color()` helper that implements the precedence: `--color=never`/`always` > `NO_COLOR` > `CLICOLOR_FORCE` > `CLICOLOR=0` > `stdout.is_terminal()`. Stick to SGR 30–37/90–97 + reset (`\x1b[0m`).

## Search 3: --quiet semantics in 2025

**Query**: `CLI --quiet flag semantics gh kubectl docker prints resource id stdout 2025`

**Key findings**:
- Docker: `-q/--quiet` prints **only the resource ID** on stdout — the dominant convention for ops tooling ([docker container ls](https://docs.docker.com/reference/cli/docker/container/ls/)).
- gh: no global `--quiet`; create-style commands (`gh pr create`, `gh issue create`, `gh run watch`) already emit a single canonical identifier (URL) on stdout regardless of TTY, so a separate `--quiet` is redundant ([gh_pr_create](https://cli.github.com/manual/gh_pr_create)).
- ripgrep: `-q` is "exit-code only, no stdout" — opposite of docker. Be explicit in your help text about which you mean.
- kubectl: no formal `--quiet` (long-running RFE [kubectl#972](https://github.com/kubernetes/kubectl/issues/972)); users fall back to `-o name`.
- Heroku-style "machine-readable on pipe, pretty at TTY": viable but the gotcha is **stderr discipline** — every spinner, banner, header, and progress line MUST go to stderr, otherwise `cmd > out.txt` corrupts the machine output. gh enforces this; older tools (e.g. `npm`) historically did not, which is why their `--json` flag exists.

**Relevance**: Pick the docker semantic for loom-cli `--quiet`: emit the primary identifier (run id / url) on stdout, suppress everything else. Document it explicitly. Always route pretty headers and spinners to stderr so the auto-detect path is safe.

## Synthesis

- **Detection**: use stdlib `std::io::IsTerminal` on `io::stdout()`; do not pull `atty` or `is-terminal`. `IsTerminal` on `stderr` separately — the answer differs and matters.
- **Color gate (one helper)**: precedence `--color=<auto|always|never>` > `NO_COLOR` (present & non-empty) > `CLICOLOR_FORCE!=0` > `CLICOLOR=0` > `stdout.is_terminal() && TERM!=dumb`. Centralize this.
- **Stdout/stderr split is the load-bearing rule**: stdout = the one machine-parseable value (id, url, json); stderr = every spinner, banner, pretty header. This makes auto-detect mode safe under `>`, `|`, and CI.
- **`--quiet` = docker semantic**: print the primary identifier on stdout, nothing else; not the ripgrep "silent + exit code" semantic. Spell it out in help text.
- **Hand-roll is fine, but bound it**: stay in 16-color SGR (30–37, 90–97) with explicit `\x1b[0m` resets; skip 256/truecolor unless gated. If you'd rather not hand-roll, `anstyle` is already a transitive dep via clap and `anstream::AutoStream` gives you free auto-stripping plus `NO_COLOR`/`TERM=dumb` handling.
