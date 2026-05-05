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

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
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

EXPOSE 0

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/loom", "doctor"]

ENTRYPOINT ["/usr/local/bin/loom-daemon"]
CMD ["--data-root", "/var/lib/loom", "--socket", "/var/lib/loom/loom.sock"]
