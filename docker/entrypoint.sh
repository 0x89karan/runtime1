#!/usr/bin/env bash
set -e
set -o pipefail

# ── helpers ──────────────────────────────────────────────────────────────────

check_api_key() {
  if [ -z "$ANTHROPIC_API_KEY" ]; then
    echo ""
    echo "  ERROR: ANTHROPIC_API_KEY is not set."
    echo "  Pass it with:  docker run -e ANTHROPIC_API_KEY=sk-ant-... ..."
    echo ""
    exit 1
  fi
}

print_banner() {
  echo ""
  echo "  ┌─────────────────────────────────────────────────────┐"
  echo "  │               AgentOS  •  Docker Shell               │"
  echo "  └─────────────────────────────────────────────────────┘"
  echo ""
  echo "  Binaries:   agentd   agentctl"
  echo "  Configs:    /etc/agentd/agent.toml    (single agent)"
  echo "              /etc/agentd/agents.toml   (multi-agent)"
  echo "  FUSE mount: /agents  (live while agentd is running)"
  echo "  Workspace:  /workspace  (mount your files here)"
  echo ""
  echo "  Quick start:"
  echo "    agentd /etc/agentd/agent.toml &      # run scout in background"
  echo "    ls /agents/                          # FUSE control plane"
  echo "    cat /agents/scout/status             # live status"
  echo "    agentctl watch --agents-dir /agents  # TUI dashboard"
  echo "    tail -f /workspace/flight.jsonl      # raw event stream"
  echo ""
}

# ── modes ────────────────────────────────────────────────────────────────────

case "${1:-shell}" in

  shell)
    check_api_key
    print_banner
    exec bash
    ;;

  run)
    # agentd <config> — run a single agent to completion, then exit
    check_api_key
    CONFIG="${2:-/etc/agentd/agent.toml}"
    echo "Running: agentd $CONFIG"
    exec agentd "$CONFIG"
    ;;

  demo)
    # Start scout in background, drop into shell so you can explore /agents
    check_api_key
    print_banner
    echo "  Starting scout agent in background..."
    # Run from /workspace so flight.jsonl lands there; logs go to stderr (visible in terminal)
    cd /workspace && agentd /etc/agentd/agent.toml &
    AGENTD_PID=$!
    echo "  agentd PID $AGENTD_PID"
    echo ""
    echo "  Waiting for /agents FUSE mount..."
    # Wait up to 5s for the FUSE mount to appear
    for i in $(seq 1 10); do
      mountpoint -q /agents 2>/dev/null && break
      sleep 0.5
    done
    if mountpoint -q /agents 2>/dev/null; then
      echo "  /agents is mounted. Agents:"
      ls /agents/ 2>/dev/null
    else
      echo "  WARNING: /agents did not mount (check logs above for errors)"
    fi
    echo ""
    exec bash
    ;;

  cos)
    # Chief of Staff — fully self-contained, no repo mount needed.
    # Runtime state (checkpoint, memory, briefs) goes to /data (named volume).
    check_api_key
    mkdir -p /data /data/output
    # Patch the baked config: rewrite dev-mode relative paths to absolute paths.
    sed \
      -e 's|"\.\./docker/|"/etc/agentd/|g' \
      -e 's|store_path = "memory\.redb"|store_path = "/data/memory.redb"|' \
      -e 's|evidence_path = "evidence\.jsonl"|evidence_path = "/data/evidence.jsonl"|' \
      -e 's|key_path      = "egress-key\.pkcs8"|key_path      = "/data/egress-key.pkcs8"|' \
      -e 's|prefix = "\./output"|prefix = "/data/output"|' \
      /etc/agentd/cos.agents.toml > /data/cos.agents.toml
    cd /data
    exec agentd /data/cos.agents.toml
    ;;

  agent)
    # Generic single-agent mode: lower any template to TOML, rewrite paths, run agentd.
    # Usage: TEMPLATE_NAME=scout AGENT_TASK="..." docker compose run --rm agent
    check_api_key
    TEMPLATE="${TEMPLATE_NAME:?TEMPLATE_NAME must be set (e.g. TEMPLATE_NAME=scout)}"
    TASK="${AGENT_TASK:?AGENT_TASK must be set (e.g. AGENT_TASK=\"Summarize my last 5 emails\")}"
    case "$TEMPLATE" in
      code-aware|langchain-worker)
        echo "ERROR: $TEMPLATE requires runsc (gVisor) and is not supported in the standard Docker image." >&2
        exit 1
        ;;
      journaler|memory-custodian)
        echo "ERROR: $TEMPLATE requires a persistent /run/memory volume (Phase-5 memory store)." >&2
        echo "  Run it in the QEMU-based environment where /run/memory is a persistent 9p mount." >&2
        exit 1
        ;;
    esac
    mkdir -p /data
    # Warn when OAuth callback port is configured — port forwarding requires --service-ports
    if [ -n "${OAUTH_CALLBACK_PORT:-}" ]; then
      echo ""
      echo "  IMPORTANT: OAuth mode — port $OAUTH_CALLBACK_PORT must be forwarded to receive the callback."
      echo "  Run with: docker compose run --service-ports --rm agent"
      echo ""
    fi
    # Lower template → TOML, rewrite MCP script paths to /etc/agentd/ (Docker path layout).
    # Sed is scoped to 'args' lines to avoid rewriting task text that happens to match.
    # Both path conventions used across the catalogue are handled:
    #   ../docker/        — dev-mode relative path (scout, librarian, google-agent, …)
    #   /usr/lib/agentos/docker/ — installed absolute path (cron-agent, watcher, webhook-agent)
    _raw=$(agentctl spawn "$TEMPLATE" --task "$TASK" --dry-run) || {
      echo "ERROR: agentctl spawn failed for template '$TEMPLATE'" >&2
      echo "" >&2
      echo "Valid templates:" >&2
      agentctl list-templates 2>/dev/null | awk 'NR>1 {print "  " $1}' >&2 || true
      exit 1
    }
    printf '%s\n' "$_raw" | sed \
      -e '/args/s|"\.\./docker/|"/etc/agentd/|g' \
      -e '/args/s|"/usr/lib/agentos/docker/|"/etc/agentd/|g' \
      > /data/agent.toml
    [ -s /data/agent.toml ] || {
      echo "ERROR: rendered config is empty for template '$TEMPLATE'" >&2
      exit 1
    }
    # Remove any stale checkpoint — each 'docker compose run' is a fresh invocation.
    # Without this, switching TEMPLATE_NAME on a reused volume would restore the wrong agent.
    rm -f /data/checkpoint.json
    # DRY_RUN_ONLY=1|true|yes: print rendered config and exit (smoke-test path rewriting)
    case "${DRY_RUN_ONLY:-}" in 1|true|yes)
      cat /data/agent.toml
      exit 0
      ;;
    esac
    cd /data
    exec agentd /data/agent.toml
    ;;

  *)
    exec "$@"
    ;;

esac
