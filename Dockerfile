# ============================================================================
# Stage 1: Rust Backend & WASM Frontend Builder
# ============================================================================
FROM rust:1.84-bookworm AS builder

WORKDIR /usr/src/pyra_ocean

# Install WASM target & Trunk for Leptos CSR build
RUN rustup target add wasm32-unknown-unknown && \
    cargo install trunk --locked

# Copy workspace manifests and source code
COPY Cargo.toml ./
COPY shared ./shared
COPY backend ./backend
COPY frontend ./frontend
COPY migrations ./migrations

# Compile the backend binary in release mode
RUN cargo build --release --package pyra_ocean_backend

# ============================================================================
# Stage 2: Minimal Hardened Debian Runtime
# ============================================================================
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 curl && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/src/pyra_ocean/target/release/pyra_ocean_backend /app/pyra_ocean_backend
COPY --from=builder /usr/src/pyra_ocean/migrations /app/migrations
COPY --from=builder /usr/src/pyra_ocean/frontend/public/assets /app/frontend/public/assets

# Create persistent attachment volume directory for `Files` property cells
RUN mkdir -p /data/pyraocean_uploads

ENV APP_HOST=0.0.0.0 \
    APP_PORT=8080 \
    STORAGE_DIR=/data/pyraocean_uploads \
    RUST_LOG=pyra_ocean_backend=info,tower_http=info

EXPOSE 8080

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/api/health || exit 1

ENTRYPOINT ["/app/pyra_ocean_backend"]
