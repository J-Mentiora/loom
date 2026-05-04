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

# AC-DIST-01: install all four loom binaries from git in one shot.
# Requires `rustup target add wasm32-wasip2` first (loom-cli's build.rs
# recursively builds the wasm32-wasip2 cdylib in release mode).
install-loom:
    cargo install --git https://github.com/J-Mentiora/loom loom-cli
    cargo install --git https://github.com/J-Mentiora/loom loom-daemon
    cargo install --git https://github.com/J-Mentiora/loom loom-mcp
    cargo install --git https://github.com/J-Mentiora/loom loom-shims --bin loom-shim-chromium
