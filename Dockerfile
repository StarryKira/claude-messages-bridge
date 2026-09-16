# syntax=docker/dockerfile:1
ARG RUST_VERSION=1.96.1
ARG NODE_VERSION=24

FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS web-build
WORKDIR /build/web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci
COPY web/ ./
RUN npm run build

FROM rust:${RUST_VERSION}-bookworm AS rust-build
ARG TARGETARCH
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY scripts/credential-store.py scripts/credential-store.py
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=bridge-target-${TARGETARCH},target=/build/target \
    cargo build --locked --release && \
    mkdir /out && cp target/release/claude-messages-bridge /out/

FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS cli-download
ARG TARGETARCH
ARG CLAUDE_CODE_VERSION=2.1.272
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates python3 && \
    rm -rf /var/lib/apt/lists/*
COPY scripts/install-cli.py /install-cli.py
RUN python3 /install-cli.py --version "$CLAUDE_CODE_VERSION" --arch "$TARGETARCH" --output /out/claude

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/StarryKira/claude-messages-bridge" \
      org.opencontainers.image.description="Single-account Claude Code RPC to Anthropic Messages bridge"
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libstdc++6 python3 openssl tini && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --uid 10001 --create-home --shell /bin/bash bridge && \
    install -d -m 0700 -o bridge -g bridge /data
COPY --from=rust-build /out/claude-messages-bridge /usr/local/bin/claude-messages-bridge
COPY --from=cli-download /out/claude /usr/local/bin/claude
COPY --from=web-build /build/web/dist /app/web
COPY scripts/healthcheck.py /app/healthcheck.py
ENV HOME=/home/bridge \
    CLAUDE_CLI_PATH=/usr/local/bin/claude \
    BRIDGE_BIND=0.0.0.0:8787 \
    BRIDGE_CREDENTIAL_DB=/data/credentials.redb \
    BRIDGE_WEB_DIR=/app/web \
    BRIDGE_CLI_BARE=0 \
    DISABLE_AUTOUPDATER=1 \
    RUST_LOG=info
WORKDIR /app
USER 10001:10001
VOLUME ["/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["python3", "/app/healthcheck.py"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/claude-messages-bridge"]
