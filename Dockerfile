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
 && echo 'fn main(){}' > agentd/tests/fixtures/sandbox_probe.rs \
 && cargo build --release 2>/dev/null || true

# Now copy the real source and rebuild (only changed crates recompile)
COPY agentd/src     agentd/src
COPY agentctl/src   agentctl/src
COPY surfaces/src   surfaces/src
COPY sandbox/src    sandbox/src
COPY otel/src       otel/src
COPY otel/tests     otel/tests
COPY agentd/tests   agentd/tests

RUN touch agentd/src/main.rs agentd/src/lib.rs agentctl/src/main.rs \
          surfaces/src/lib.rs sandbox/src/lib.rs otel/src/main.rs \
 && cargo build --release --bin agentd --bin agentctl --bin agentos-otel

# ── Stage 2: runtime ─────────────────────────────────────────────────────────
FROM alpine:3.20

RUN apk add --no-cache fuse3 bash jq python3

# Allow non-root users to mount FUSE filesystems
RUN echo "user_allow_other" >> /etc/fuse.conf

COPY --from=builder /src/target/release/agentd       /usr/local/bin/agentd
COPY --from=builder /src/target/release/agentctl    /usr/local/bin/agentctl
COPY --from=builder /src/target/release/agentos-otel /usr/local/bin/agentos-otel

# Default agent config and templates
COPY docker/agent.toml         /etc/agentd/agent.toml
COPY docker/agents.toml        /etc/agentd/agents.toml
COPY docker/weather-agent.toml /etc/agentd/weather-agent.toml
COPY docker/weather_mcp.py     /etc/agentd/weather_mcp.py
COPY templates/                /etc/agentd/templates/

RUN mkdir -p /agents /workspace /run/memory

WORKDIR /workspace

COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
CMD ["shell"]
