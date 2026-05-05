---
name: Bug report
about: Something in loom is broken or behaves unexpectedly
labels: bug
---

## What happened

<!-- Describe the actual behavior you observed. -->

## What you expected

<!-- Describe what you thought should happen instead. -->

## Steps to reproduce

<!-- Minimal commands or code that reproduce the bug. The more self-contained, the faster we can fix it. -->

```bash
loom session create --profile standard
loom action web.navigate -- --url https://example.com
# ...
```

## Environment

- **loom version**: <!-- output of `loom --version` -->
- **OS**: <!-- macOS 14, Ubuntu 24.04, etc. -->
- **Architecture**: <!-- arm64 / x86_64 -->
- **Install method**: <!-- brew / cargo install / installer script / source build -->

## Logs and receipt JSON

<!-- If applicable: relevant lines from `loom serve`'s stderr, the JSON receipt
from the failing action, or the output of `loom session inspect <id>`.
Paste in fenced blocks, redact anything sensitive. -->

```text
```
