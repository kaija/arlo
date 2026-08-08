# syntax=docker/dockerfile:1
# Multi-stage Rust builder for arlo --serve.
# BuildKit cache mounts keep the cargo registry and target dir between builds,
# so editing bridge.rs only recompiles agent-cli, not the full dep graph.

FROM rust:1-slim AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first so dependency layer is cached separately from source.
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build with BuildKit cache mounts — cargo registry and target directory
# are preserved between builds via named volumes.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin arlo \
    && cp /app/target/release/arlo /usr/local/bin/arlo

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM debian:stable-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -u 10001 -s /bin/false -M arlo

COPY --from=builder /usr/local/bin/arlo /usr/local/bin/arlo

USER 10001

CMD ["arlo", "--serve", "0.0.0.0:8080"]
