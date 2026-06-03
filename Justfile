# Loom workspace build helpers. Run from the repo root.
#
# Prerequisites: https://just.systems — `brew install just`

# Generate target/release/meta.json for the binary-size benchmark.
# The "strategy" field controls which size budget is enforced:
#   "downloaded" — Chromium downloaded on first use; binary budget ≤ 60 MB.
#   "bundled"    — Chromium bundled in the release tarball; binary budget ≤ 150 MB.
# Adjust to match the release configuration before running `loom benchmark`.
gen-meta:
    mkdir -p target/release
    echo '{"strategy":"downloaded"}' > target/release/meta.json
    @echo "Wrote target/release/meta.json (strategy: downloaded)"

# Regenerate the committed loom_surface_web.wasm artifact.
# Run after editing anything under loom-surface-web/ — CI's
# vendored-wasm-check job will fail PRs where this is stale.
vendor-wasm:
    cargo build -p loom-surface-web \
        --target wasm32-wasip2 \
        --profile=wasm-guest \
        --target-dir target/wasm-guest
    cp target/wasm-guest/wasm32-wasip2/wasm-guest/loom_surface_web.wasm \
       loom-cli/vendor/loom_surface_web.wasm
    @echo "✓ regenerated loom-cli/vendor/loom_surface_web.wasm — commit if changed"

# Regenerate docs/actions.md and the man-page family from the action
# registry. CI fails on any stale committed `docs/` against the registry,
# so run this whenever you touch loom-rpc/src/action_registry/.
gen-docs:
    cargo run --example gen-docs -p loom-cli

# Re-render the README demo GIFs from the vhs tapes in scripts/.
# Requires: vhs + ttyd + ffmpeg (brew install vhs); loom on PATH with
# `loom doctor` green (Chromium present); jq; outbound network. The tapes
# run the real loom binary, so this also re-verifies the demo still works.
record-demo:
    vhs scripts/session-diff-demo.tape
    vhs scripts/session-diff-divergence.tape
    @echo "✓ rendered docs/assets/session-diff-demo.gif + session-diff-divergence.gif"
