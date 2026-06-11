# ── Stage 1: builder ──────────────────────────────────────────────────────────
FROM rust:1.92-slim-bookworm AS builder

# System deps for wasmtime + OpenTelemetry TLS.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the entire workspace and build. Note: loom-cli ships all four
# binaries (`loom`, `loom-daemon`, `loom-mcp`, `loom-shim-chromium`).
COPY . .

RUN cargo build --release -p loom-cli

# ── Stage 2: production image ─────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# `ca-certificates` + `libssl3` for the daemon's TLS (OTel exporter);
# `tini` as PID 1 (the CLI's daemon-liveness probe deliberately rejects
# PID 1, and Chromium subprocesses need a zombie reaper); `procps` for
# the `kill -0` liveness probe that same CLI path shells out to. The
# remaining packages are the shared libraries the pinned Chromium binary
# loads at runtime — ci.yml's Linux-runner list (`libasound2` is
# bookworm's name for ubuntu-24's `libasound2t64`) plus the libs GitHub
# runners preinstall but bookworm-slim does not. Without them
# `loom postinstall` can download Chromium but never launch it.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    tini \
    procps \
    libnss3 libnspr4 libgbm1 libdrm2 libasound2 \
    libatk1.0-0 libatk-bridge2.0-0 libatspi2.0-0 libcups2 \
    libglib2.0-0 libdbus-1-3 libexpat1 \
    libx11-6 libx11-xcb1 libxcb1 libxcomposite1 libxdamage1 \
    libxext6 libxfixes3 libxrandr2 libxss1 \
    libxkbcommon0 libxkbcommon-x11-0 \
    libpango-1.0-0 libcairo2 \
    fonts-liberation xdg-utils \
    && rm -rf /var/lib/apt/lists/*

# Non-root user for security
RUN groupadd --gid 10001 loom && \
    useradd --uid 10001 --gid loom --shell /bin/false --no-create-home loom

# Data directory
RUN mkdir -p /var/lib/loom && chown loom:loom /var/lib/loom

COPY --from=builder /build/target/release/loom-daemon       /usr/local/bin/loom-daemon
COPY --from=builder /build/target/release/loom              /usr/local/bin/loom
COPY --from=builder /build/target/release/loom-mcp          /usr/local/bin/loom-mcp
COPY --from=builder /build/target/release/loom-shim-chromium /usr/local/bin/loom-shim-chromium

USER loom

# Socket path and data root can be overridden at runtime
ENV LOOM_DATA_ROOT=/var/lib/loom
ENV LOOM_SOCKET_PATH=/var/lib/loom/loom.sock
ENV LOOM_LOG_PATH=/var/lib/loom/daemon.log
ENV LOOM_OTEL_ENABLED=false

# Path harmony with the CLI: the daemon writes hello.token under
# $LOOM_DATA_ROOT/auth, but the `loom` CLI reads it from
# `dirs::data_dir()/loom/auth` = $XDG_DATA_HOME/loom/auth. Point
# XDG_DATA_HOME at /var/lib so both resolve to /var/lib/loom/auth —
# without this, every in-container `loom` invocation fails auth.
ENV XDG_DATA_HOME=/var/lib

# Writable HOME on the data volume so `loom postinstall` (run via
# `docker exec` to fetch Chromium + AOT artifacts) has somewhere to
# write — the `loom` user is created with --no-create-home.
ENV HOME=/var/lib/loom

# No user-namespace sandbox in an unprivileged container — the same
# flags ci.yml sets for its containerized Chromium runs.
ENV LOOM_CHROMIUM_EXTRA_FLAGS="--no-sandbox --disable-dev-shm-usage"

EXPOSE 0

# The image ships no Chromium or AOT artifacts until `loom postinstall`
# is run, so a full `loom doctor` would exit 1 forever (chromium/AOT
# checks) and mark a healthy daemon permanently unhealthy. Scope the
# probe to what the container must provide out of the box: a reachable
# socket and a responsive daemon.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/loom", "doctor", "--daemon-only"]

# tini as PID 1: reaps Chromium zombies, forwards signals, and keeps
# loom-daemon off PID 1 (which the CLI's liveness probe rejects).
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/loom-daemon"]
CMD ["--data-root", "/var/lib/loom", "--socket", "/var/lib/loom/loom.sock"]
