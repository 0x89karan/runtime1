# syntax=docker/dockerfile:1
# ── Stage 1: build ───────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev fuse-dev pkgconfig

WORKDIR /src

# Cache dependencies before copying source
COPY Cargo.toml Cargo.lock ./
COPY agentd/Cargo.toml   agentd/Cargo.toml
COPY agentctl/Cargo.toml agentctl/Cargo.toml
COPY surfaces/Cargo.toml surfaces/Cargo.toml
COPY sandbox/Cargo.toml  sandbox/Cargo.toml
COPY otel/Cargo.toml     otel/Cargo.toml

# Stub out all lib/main entry points so cargo can resolve the dep graph
RUN mkdir -p agentd/src agentctl/src surfaces/src sandbox/src otel/src \
 && echo 'fn main(){}' > agentd/src/main.rs \
 && echo 'pub fn stub(){}' > agentd/src/lib.rs \
 && echo 'fn main(){}' > agentctl/src/main.rs \
 && echo 'pub fn stub(){}' > surfaces/src/lib.rs \
 && echo 'pub fn stub(){}' > sandbox/src/lib.rs \
 && echo 'fn main(){}' > otel/src/main.rs \
 # echo-mcp and sandbox-probe fixture binaries
 && mkdir -p agentd/tests/fixtures \
 && echo 'fn main(){}' > agentd/tests/fixtures/echo_mcp.rs \
 && echo 'fn main(){}' > agentd/tests/fixtures/sandbox_probe.rs
# /usr/local/cargo/registry cache: speeds up local `make dev-image` builds by
# persisting crate downloads across rebuilds. In CI, the GHA layer cache
# (cache-from: type=gha) serves the equivalent purpose.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release 2>/dev/null || true

# Now copy the real source and rebuild (only changed crates recompile)
COPY agentd/src     agentd/src
COPY agentctl/src   agentctl/src
COPY surfaces/src   surfaces/src
COPY sandbox/src    sandbox/src
COPY otel/src       otel/src
COPY otel/tests     otel/tests
COPY agentd/tests   agentd/tests

# NOTE: do NOT cache /src/target here — cache mounts are not committed to image
# layers, which would break the COPY --from=builder step in stage 2.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    touch agentd/src/main.rs agentd/src/lib.rs agentctl/src/main.rs \
          surfaces/src/lib.rs sandbox/src/lib.rs otel/src/main.rs \
 && cargo build --release --bin agentd --bin agentctl --bin agentos-otel

# ── Stage 2a: runtime-core — Rust binaries only ──────────────────────────────
# Use this tier for custom MCP setups or HTTP-only MCP endpoints.
# Build: docker build --target runtime-core
# Pull:  docker pull ghcr.io/0x89karan/runtime1:core
FROM alpine:3.20 AS runtime-core

RUN apk add --no-cache fuse3 bash jq curl

# Allow non-root users to mount FUSE filesystems
RUN echo "user_allow_other" >> /etc/fuse.conf

COPY --from=builder /src/target/release/agentd       /usr/local/bin/agentd
COPY --from=builder /src/target/release/agentctl     /usr/local/bin/agentctl
COPY --from=builder /src/target/release/agentos-otel /usr/local/bin/agentos-otel

# Base agent configs and full template catalogue
COPY docker/agent.toml      /etc/agentd/agent.toml
COPY docker/agents.toml     /etc/agentd/agents.toml
COPY docker/cockpit.toml    /etc/agentd/cockpit.toml
COPY agentd/cos.agents.toml /etc/agentd/cos.agents.toml
COPY templates/             /etc/agentd/templates/

RUN mkdir -p /agents /workspace /run/memory /data

WORKDIR /workspace

COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
CMD ["cockpit"]

# ── Stage 2b: runtime-full — adds Python MCP harness ─────────────────────────
# Extends core with all standard MCP servers (h7.1–h7.3, h8.1) and OAuth sidecar.
# Build: docker build (no --target, or --target runtime-full)
# Pull:  docker pull ghcr.io/0x89karan/runtime1:full  (also :latest)
FROM runtime-core AS runtime-full

RUN apk add --no-cache python3

# Standard MCP servers and supplemental agent configs
COPY docker/*.py               /etc/agentd/
COPY docker/weather-agent.toml /etc/agentd/weather-agent.toml
