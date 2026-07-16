#!/usr/bin/env bash
set -e
set -o pipefail

# ── helpers ──────────────────────────────────────────────────────────────────

# Source secrets file before any key check — this is the preferred credential path.
# Parse KEY=value pairs directly (no shell sourcing) to prevent code execution from
# a malformed or externally-controlled secrets file. File wins over compose env vars
# (intentional: the secrets file is the authoritative source).
if [ -f /run/secrets/agentos.env ]; then
  while IFS= read -r _agentos_line || [ -n "$_agentos_line" ]; do
    case "$_agentos_line" in ''|'#'*) continue ;; esac
    _agentos_key="${_agentos_line%%=*}"
    _agentos_val="${_agentos_line#*=}"
    case "$_agentos_key" in *[!A-Za-z0-9_]*|''|[0-9]*) continue ;; esac
    case "$_agentos_key" in BASH_ENV|LD_PRELOAD|LD_LIBRARY_PATH|PATH|IFS|ENV|PS4|CDPATH) continue ;; esac
    case "$_agentos_key" in PYTHONSTARTUP|PYTHONPATH|PYTHONUSERSITE|PYTHONINSPECT|RUBYOPT|NODE_OPTIONS) continue ;; esac
    export "${_agentos_key}=${_agentos_val}"
  done < /run/secrets/agentos.env
fi

check_api_key() {
  if [ -z "$ANTHROPIC_API_KEY" ]; then
    echo ""
    echo "  ERROR: ANTHROPIC_API_KEY is not set."
    echo ""
    echo "  Option 1 — secrets file (recommended):"
    echo "    mkdir -p ~/.agentos-secrets"
    echo "    printf 'ANTHROPIC_API_KEY=sk-ant-...\\n' > ~/.agentos-secrets/agentos.env"
    echo "    chmod 600 ~/.agentos-secrets/agentos.env"
    echo ""
    echo "  Option 2 — environment variable:"
    echo "    docker run -e ANTHROPIC_API_KEY=sk-ant-... ..."
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

    # Secrets preflight: google.json must be provisioned before the CoS starts.
    # Run once on the Mac host: agentctl auth google --client-id ... --client-secret ...
    # Then mount with: -v ~/.agentos-secrets:/run/secrets:ro (already in compose).
    # Unlike the 'agent' mode, 'cos' intentionally has no OAUTH_* env-var fallback —
    # the multi-agent setup requires the persistent token file, not one-shot env vars.
    if [ ! -s /run/secrets/google.json ]; then
      echo ""
      echo "  ERROR: Google credentials not provisioned."
      echo ""
      echo "  Run on your Mac (once):"
      echo "    agentctl auth google \\"
      echo "      --client-id YOUR_CLIENT_ID \\"
      echo "      --client-secret YOUR_CLIENT_SECRET"
      echo ""
      echo "  Then restart:"
      echo "    docker compose restart cos"
      echo ""
      exit 1
    fi

    # Semantic KB (memory-routing): email bodies are indexed via OpenAI text-embedding-3-small.
    if [ -z "${OPENAI_API_KEY:-}" ]; then
      echo ""
      echo "  ERROR: OPENAI_API_KEY is not set."
      echo ""
      echo "  The CoS semantic KB requires an OpenAI API key for email embeddings."
      echo "  Add it to your environment:"
      echo "    export OPENAI_API_KEY=sk-..."
      echo ""
      echo "  Or pass it to docker compose:"
      echo "    OPENAI_API_KEY=sk-... docker compose up cos"
      echo ""
      exit 1
    fi

    mkdir -p /data /data/output
    # Patch the baked config: rewrite dev-mode relative paths to absolute paths.
    sed \
      -e 's|"\.\./docker/|"/etc/agentd/|g' \
      -e 's|store_path = "memory\.redb"|store_path = "/data/memory.redb"|' \
      -e 's|evidence_path = "evidence\.jsonl"|evidence_path = "/data/evidence.jsonl"|' \
      -e 's|key_path      = "egress-key\.pkcs8"|key_path      = "/data/egress-key.pkcs8"|' \
      -e 's|prefix = "\./output"|prefix = "/data/output"|' \
      -e "s|write_file(path='\\./output/|write_file(path='/data/output/|" \
      /etc/agentd/cos.agents.toml > /data/cos.agents.toml
    # Fail fast if the FsWrite grant and the write_file prompt instruction
    # ever desync again (the exact bug this rewrite fixes): a silent mismatch
    # here means every write_file call gets denied with capability_denied,
    # discoverable only by reading flight.jsonl inside a running container.
    grep -q "write_file(path='/data/output/" /data/cos.agents.toml || {
      echo "ERROR: cos.agents.toml path rewrite failed — write_file prompt path" >&2
      echo "       doesn't match the rewritten FsWrite grant. Check the sed" >&2
      echo "       patterns in docker/entrypoint.sh still match the source TOML." >&2
      exit 1
    }
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
      google-agent)
        # TODO cred.3: replace with broker startup validation (generic preflight from gated_requires)
        # Fail only when BOTH the secrets file is absent AND the env-var fallback is incomplete.
        # Users who set OAUTH_CLIENT_ID + OAUTH_CLIENT_SECRET + OAUTH_REFRESH_TOKEN still work.
        if [ ! -s /run/secrets/google.json ] && {
          [ -z "${OAUTH_CLIENT_ID:-}" ] ||
          [ -z "${OAUTH_CLIENT_SECRET:-}" ] ||
          [ -z "${OAUTH_REFRESH_TOKEN:-}" ]
        }; then
          if [ ! -d /run/secrets ]; then
            echo ""
            echo "  ERROR: Secrets volume not mounted."
            echo ""
            echo "  The docker-compose.yml agent service mounts ~/.agentos-secrets."
            echo "  If running with 'docker run', add: -v ~/.agentos-secrets:/run/secrets:ro"
            echo ""
          else
            echo ""
            echo "  ERROR: Google credentials not provisioned."
            echo ""
            echo "  Run on your Mac (once):"
            echo "    agentctl auth google \\"
            echo "      --client-id YOUR_CLIENT_ID \\"
            echo "      --client-secret YOUR_CLIENT_SECRET"
            echo ""
            echo "  Then re-run:"
            echo "    TEMPLATE_NAME=google-agent AGENT_TASK=\"your task\" docker compose run --rm agent"
            echo ""
          fi
          exit 1
        fi
        ;;
    esac
    mkdir -p /data
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

  orchestrate)
    # Start agentd with management API enabled (if not already running),
    # then launch agentctl orchestrate pointed at the management port.
    export AGENTD_MANAGEMENT_ENABLED=true
    export AGENTD_MANAGEMENT_PORT="${AGENTD_MANAGEMENT_PORT:-7999}"
    _MGMT_URL="http://127.0.0.1:${AGENTD_MANAGEMENT_PORT}"
    if curl -sf "${_MGMT_URL}/healthz" >/dev/null 2>&1; then
      # agentd already running — inject into it.
      exec agentctl orchestrate --url "${_MGMT_URL}" "$@"
    else
      # Cold-start: launch agentd in background, wait for healthz, then attach.
      agentd /etc/agentd/agents.toml &
      AGENTD_PID=$!
      # Forward SIGTERM/SIGINT to agentd so graceful checkpoint fires on docker stop.
      trap "kill $AGENTD_PID 2>/dev/null; wait $AGENTD_PID 2>/dev/null; exit 0" TERM INT
      timeout 15 sh -c "until curl -sf '${_MGMT_URL}/healthz' >/dev/null 2>&1; do sleep 0.5; done" || {
        echo "ERROR: agentd did not start within 15 seconds" >&2
        kill $AGENTD_PID 2>/dev/null || true
        exit 1
      }
      agentctl orchestrate --url "${_MGMT_URL}" "$@"
      kill $AGENTD_PID 2>/dev/null || true
      wait $AGENTD_PID 2>/dev/null || true
    fi
    ;;

  cockpit)
    # Zero-arg default (Dockerfile CMD). Cold-starts agentd with a minimal,
    # agent-free config (docker/cockpit.toml — opening the cockpit shows the
    # empty system state, it doesn't spend API tokens on demo work automatically)
    # and attaches agentctl watch. FUSE is used opportunistically when the
    # container is privileged; agentctl watch's detect_source() already prefers
    # FUSE and falls back to the management API over HTTP on its own, so no
    # --privileged requirement is enforced here — the readiness wait below
    # accepts either surface coming up.
    check_api_key
    shift

    if [ ! -t 1 ]; then
      echo "ERROR: cockpit mode requires an interactive terminal (docker run -it ...)." >&2
      echo "  For a one-shot, non-interactive agent run instead, use:" >&2
      echo "    docker run ... run <config.toml>" >&2
      exit 1
    fi

    export AGENTD_MANAGEMENT_ENABLED=true
    export AGENTD_MANAGEMENT_PORT="${AGENTD_MANAGEMENT_PORT:-7999}"
    _MGMT_URL="http://127.0.0.1:${AGENTD_MANAGEMENT_PORT}"
    # agentctl's HTTP fallback (when FUSE is absent) is hard-coded to port 7999.
    # Only override it with an explicit URL when the port was customized away
    # from the default — doing this unconditionally would make agentctl always
    # skip FUSE (an explicit --url/AGENTCTL_URL bypasses FUSE detection entirely),
    # regressing the privileged-container path for the common (default-port) case.
    if [ "$AGENTD_MANAGEMENT_PORT" != "7999" ]; then
      export AGENTCTL_URL="${_MGMT_URL}"
    fi

    # Register the trap BEFORE backgrounding agentd: if it were backgrounded first,
    # a signal landing in that narrow gap would kill this untrapped script outright
    # (bash does not forward signals to background jobs by default), orphaning
    # agentd instead of giving it a chance at its own graceful SIGTERM checkpoint.
    # Late-bound (single-quoted) so $AGENTD_PID/$WATCH_PID — not yet set here — are
    # re-read at signal-delivery time, once each is actually assigned.
    #
    # CRITICAL: must `wait` on BOTH pids, not just $AGENTD_PID, before `exit 0`.
    # This script is the container's PID 1 — the instant it exits, Docker tears
    # down every other process in the container's PID namespace. agentctl watch's
    # own graceful shutdown (its SIGTERM handler sets a flag; the render loop
    # notices on its next ~30ms tick and restores the terminal: disables raw mode
    # + leaves the alternate screen) needs a moment to actually run. Without
    # waiting for it here first, `exit 0` races it and usually wins, so the
    # container disappears before agentctl's terminal-restore write lands —
    # leaving the operator's real terminal stuck in raw mode + the alternate
    # screen (verified: reproducible without this `wait`, absent with it).
    trap 'kill "$AGENTD_PID" "$WATCH_PID" 2>/dev/null; wait "$WATCH_PID" 2>/dev/null; wait "$AGENTD_PID" 2>/dev/null; exit 0' TERM INT

    # Run from /data, NOT /workspace: /workspace is a bind mount for the operator's
    # own files ("mount your files here" in print_banner), and agentd writes its
    # own runtime state (checkpoint.json, flight.jsonl) into its CWD. Running there
    # would (a) contaminate the mounted directory and (b) silently restore agents
    # from a stale checkpoint.json left by a prior demo/run/cockpit session on the
    # same mount — resurrecting spawned agents and spending tokens despite
    # cockpit.toml's zero-agent config. `cos)`/`agent)` modes already avoid this by
    # using /data instead of /workspace; `rm -f checkpoint.json` matches `agent)`
    # mode's same "each launch starts fresh" rationale.
    mkdir -p /data
    rm -f /data/checkpoint.json
    cd /data && agentd /etc/agentd/cockpit.toml &
    AGENTD_PID=$!

    _ready=""
    for _i in $(seq 1 30); do
      kill -0 "$AGENTD_PID" 2>/dev/null || {
        echo "ERROR: agentd exited unexpectedly during startup — see stderr above" >&2
        exit 1
      }
      if [ -e /agents/system ] || curl -sf "${_MGMT_URL}/healthz" >/dev/null 2>&1; then
        _ready=1
        break
      fi
      sleep 0.5
    done
    if [ -z "$_ready" ]; then
      echo "ERROR: agentd started but is not responding on either the FUSE surface or the management API after 15s — check agentd's stderr above" >&2
      kill "$AGENTD_PID" 2>/dev/null || true
      exit 1
    fi

    # Non-exec'd (preserves the trap above) and backgrounded only to capture its
    # PID for cleanup; `wait` still blocks the script exactly like a foreground run.
    set +e
    agentctl watch "$@" &
    WATCH_PID=$!
    wait "$WATCH_PID"
    rc=$?
    set -e
    # Disarm the trap now: $WATCH_PID has already been wait-reaped (its PID number
    # is free for kernel reuse), and a signal landing during the next few cleanup
    # lines would otherwise re-fire the trap's `exit 0`, discarding a real nonzero
    # `$rc` from agentctl watch and reporting success to Docker regardless.
    trap - TERM INT

    kill "$AGENTD_PID" 2>/dev/null || true
    wait "$AGENTD_PID" 2>/dev/null || true
    exit "$rc"
    ;;

  *)
    exec "$@"
    ;;

esac
