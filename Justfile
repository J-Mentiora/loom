# Loom workspace build helpers.
# Run from the src/ directory (where Cargo.toml lives).
#
# Prerequisites: https://just.systems — `brew install just`

# Generate target/release/meta.json for the binary-size benchmark (AC-BENCH-03).
# The "strategy" field controls which size budget is enforced:
#   "downloaded" — Chromium downloaded on first use; binary budget ≤ 60 MB.
#   "bundled"    — Chromium bundled in the release tarball; binary budget ≤ 150 MB.
# Adjust to match the release configuration before running `loom benchmark`.
gen-meta:
    mkdir -p target/release
    echo '{"strategy":"downloaded"}' > target/release/meta.json
    @echo "Wrote target/release/meta.json (strategy: downloaded)"
