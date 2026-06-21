#!/usr/bin/env bash
set -e

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

  *)
    exec "$@"
    ;;

esac
