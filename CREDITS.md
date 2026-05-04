# Credits

Loom was built inside Mentiora's
[code-pipeline](https://github.com/WhoIsJohannes/code-pipeline) project,
a genetic-algorithm-driven software-generation harness. Its initial
design was produced by the pipeline's GA exploration phase; subsequent
hardening (security, determinism, MCP integration, crash detection,
GC reference protection, runtime correctness) happened in 23 rounds
of iterative testing + fix.

This repository is a clean extraction at v1.0.0 — the original
commit history is preserved in the source pipeline at
`projects/loom/`. The code-pipeline repository now consumes loom
as a regular Cargo dependency.

## Extraction provenance

- **Source repository:** https://github.com/WhoIsJohannes/code-pipeline
- **Source path:** `projects/loom/src/`
- **Extracted at:** 2026-05-04
- **Source commit at extraction:** see git log of the source repo
  for the commit dated 2026-05-04 that touches `projects/loom/`.

## Contributors

The pipeline's GA generated, scored, and hybridized candidate designs;
Claude (Sonnet 4.6, Opus 4.7) authored the implementation under
human review at every phase gate. Human stewardship by Johannes
Rummel + the Mentiora team.
