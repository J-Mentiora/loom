# ── Stage 1: builder ──────────────────────────────────────────────────────────
FROM rust:1.82-slim-bookworm AS builder

# System deps for wasmtime (needs libssl for OpenTelemetry TLS)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation separately from application code
COPY Cargo.toml Cargo.lock ./
COPY loom-shared/Cargo.toml    loom-shared/
COPY loom-keychain/Cargo.toml  loom-keychain/
COPY loom-core/Cargo.toml      loom-core/
COPY loom-host/Cargo.toml      loom-host/
COPY loom-rpc/Cargo.toml       loom-rpc/
COPY loom-mcp/Cargo.toml       loom-mcp/
COPY loom-cli/Cargo.toml       loom-cli/
COPY loom-daemon/Cargo.toml    loom-daemon/
COPY loom-surfaces/Cargo.toml  loom-surfaces/
COPY loom-shims/Cargo.toml     loom-shims/

# Create stub lib/main files so cargo can fetch deps without full source
RUN for crate in loom-shared loom-keychain loom-core loom-host loom-rpc \
        loom-mcp loom-cli loom-surfaces loom-shims; do \
      mkdir -p ${crate}/src && echo "// stub" > ${crate}/src/lib.rs; \
    done && \
    mkdir -p loom-daemon/src && printf 'fn main() {}' > loom-daemon/src/main.rs

RUN cargo fetch

# Now copy real source
COPY . .

# Build release binary
RUN cargo build --release -p loom-daemon -p loom-cli

# ── Stage 2: production image ─────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Non-root user for security
RUN groupadd --gid 10001 loom && \
    useradd --uid 10001 --gid loom --shell /bin/false --no-create-home loom

# Data directory
RUN mkdir -p /var/lib/loom && chown loom:loom /var/lib/loom

COPY --from=builder /build/target/release/loom-daemon /usr/local/bin/loom-daemon
COPY --from=builder /build/target/release/loom         /usr/local/bin/loom

USER loom

# Socket path and data root can be overridden at runtime
ENV LOOM_DATA_ROOT=/var/lib/loom
ENV LOOM_SOCKET_PATH=/var/lib/loom/loom.sock
ENV LOOM_LOG_PATH=/var/lib/loom/daemon.log
ENV LOOM_OTEL_ENABLED=false

EXPOSE 0

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/loom", "doctor"]

ENTRYPOINT ["/usr/local/bin/loom-daemon"]
CMD ["--data-root", "/var/lib/loom", "--socket", "/var/lib/loom/loom.sock"]
