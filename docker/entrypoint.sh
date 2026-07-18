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
    # Tool-behavior vars: GREP_OPTIONS/POSIXLY_CORRECT change how the boot
    # guards' grep/sed behave — same class as the interpreter vars above.
    case "$_agentos_key" in GREP_OPTIONS|POSIXLY_CORRECT) continue ;; esac
    # Boot-behavior flags must come from the real container environment, never
    # the secrets file: a stray line here could silently disable the path guards
    # or turn a production cos boot into print-and-exit-0 (clean-looking outage).
    case "$_agentos_key" in AGENTOS_SKIP_PATH_GUARDS|DRY_RUN_ONLY) continue ;; esac
    export "${_agentos_key}=${_agentos_val}"
  done < /run/secrets/agentos.env
fi

# ── boot guards (audit.1 / audit86-P1-5) ─────────────────────────────────────
# After the sed rewrite, no relative path may survive in the shipped config: a
# missed rewrite used to boot fine and then fail every affected tool call with
# capability_denied, discoverable only by grepping flight.jsonl inside the
# container (the v0.86.2 bug). These guards turn that into a boot-time failure
# that names the offending line.
#
# Patterns are general (any quoted ./ or ../ in either quote style, plus
# path-bearing keys whose value is not absolute) rather than a copy of the sed
# LHS list — a third hand-maintained literal list would drift in lockstep with
# the sed rules, which is the disease this guards against. POSIX [[:space:]]
# only (\s is a GNU extension; busybox grep silently matches nothing).
#
# Comment lines are DELIBERATELY inside the cos scan: a future comment with a
# quoted ./ path fails loudly at dev time rather than being silently excluded,
# and a ^#-exclusion would false-negative on task = """ content lines that
# legitimately begin with # (e.g. markdown headings in prompts).
#
# Escape hatch: AGENTOS_SKIP_PATH_GUARDS=1 skips these checks — for operators
# whose bind-mounted config legitimately contains a quoted ./ in task prose.
is_truthy() { case "${1:-}" in 1|true|yes) return 0 ;; *) return 1 ;; esac; }

guard_no_relative_paths() {
  _guard_file="$1"    # rewritten config to check
  _guard_primary="$2" # main ERE (cos: whole-file quoted ./; agent: args-line-scoped)
  _guard_extra="$3"   # additional ERE, may be empty
  _guard_repro="$4"   # mode-specific dry-run repro command for the error text
  if is_truthy "${AGENTOS_SKIP_PATH_GUARDS:-}"; then
    echo "WARNING: AGENTOS_SKIP_PATH_GUARDS set — skipping boot path guards" >&2
    return 0
  fi
  # grep exit 0/1 = match/no-match, both fine; exit >=2 (unreadable file, bad
  # pattern, hostile GREP_OPTIONS) must FAIL the boot — a guard that errors must
  # not silently pass. $? of a command substitution is grep's rc in THIS shell,
  # so the exit below terminates the script (an exit inside the $() would not).
  set +e
  _guard_hits=$(grep -nE "$_guard_primary" "$_guard_file")
  _guard_rc=$?
  set -e
  if [ "$_guard_rc" -gt 1 ]; then
    echo "ERROR: boot guard grep failed (rc=$_guard_rc) on $_guard_file — refusing to boot unverified" >&2
    exit 1
  fi
  if [ -n "$_guard_extra" ]; then
    set +e
    _guard_extra_hits=$(grep -nE "$_guard_extra" "$_guard_file")
    _guard_rc=$?
    set -e
    if [ "$_guard_rc" -gt 1 ]; then
      echo "ERROR: boot guard grep failed (rc=$_guard_rc) on $_guard_file — refusing to boot unverified" >&2
      exit 1
    fi
    if [ -n "$_guard_extra_hits" ]; then
      _guard_hits="${_guard_hits}${_guard_hits:+
}${_guard_extra_hits}"
    fi
  fi
  if [ -n "$_guard_hits" ]; then
    # CI's docker-smoke negative control greps this exact message (ci.yml) —
    # rewording it must update that assertion in the same change.
    echo "ERROR: relative path survived the boot rewrite in $_guard_file:" >&2
    echo "$_guard_hits" | head -5 >&2
    echo "" >&2
    echo "  If this is a bind-mounted custom config: use absolute paths in" >&2
    echo "  container configs (relative paths resolve against the container" >&2
    echo "  CWD, not your host checkout)." >&2
    echo "  If this is the baked config: a sed rule in docker/entrypoint.sh" >&2
    echo "  drifted from the source TOML — add or fix the rewrite rule." >&2
    echo "  Reproduce with the dry-run path:  $_guard_repro" >&2
    echo "  Override (accept relative paths): AGENTOS_SKIP_PATH_GUARDS=1" >&2
    exit 1
  fi
}

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

    # DRY_RUN_ONLY=1|true|yes: run the config rewrite + boot guards, print the
    # rendered config, exit 0. Bypasses ALL credential preflights (check_api_key
    # included) — a dry run verifies the rewrite, not credentials, and must be
    # runnable in CI and by operators with zero secrets:
    #   docker run --rm -e DRY_RUN_ONLY=1 <image> cos
    _COS_DRY_RUN=""
    if is_truthy "${DRY_RUN_ONLY:-}"; then _COS_DRY_RUN=1; fi

    if [ -z "$_COS_DRY_RUN" ]; then
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
    fi  # end of credential preflights (skipped under DRY_RUN_ONLY)

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
    # Gated behind the same escape hatch as the general guards: a bind-mounted
    # custom config won't carry this baked-config literal, and the skip flag
    # means "my config intentionally differs" (audit.1 /review, F5).
    if ! is_truthy "${AGENTOS_SKIP_PATH_GUARDS:-}"; then
      grep -q "write_file(path='/data/output/" /data/cos.agents.toml || {
        echo "ERROR: cos.agents.toml path rewrite failed — write_file prompt path" >&2
        echo "       doesn't match the rewritten FsWrite grant. Check the sed" >&2
        echo "       patterns in docker/entrypoint.sh still match the source TOML." >&2
        echo "       Override (custom config): AGENTOS_SKIP_PATH_GUARDS=1" >&2
        exit 1
      }
    fi
    # General negative assertion (audit.1): no quoted relative path — in either
    # quote style — and no path-bearing key with a non-absolute value may survive
    # the rewrite. Covers future relative paths no sed rule knows about yet.
    guard_no_relative_paths /data/cos.agents.toml \
      "[\"']\.\.?/" \
      "^[[:space:]]*[a-z_]*_(path|dir)[[:space:]]*=[[:space:]]*[\"'][^/]" \
      "docker run --rm -e DRY_RUN_ONLY=1 <image> cos"
    if [ -n "$_COS_DRY_RUN" ]; then
      cat /data/cos.agents.toml
      exit 0
    fi
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
    # Sed is anchored to lines STARTING with `args =` (a bare /args/ substring
    # address would also rewrite task text that merely mentions "args" — real
    # corruption, reproduced in review). Multi-line args arrays are not rewritten
    # by these rules; the boot guard below catches them loudly instead.
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
      -e '/^args[[:space:]]*=/s|"\.\./docker/|"/etc/agentd/|g' \
      -e '/^args[[:space:]]*=/s|"/usr/lib/agentos/docker/|"/etc/agentd/|g' \
      > /data/agent.toml
    [ -s /data/agent.toml ] || {
      echo "ERROR: rendered config is empty for template '$TEMPLATE'" >&2
      exit 1
    }
    # General negative assertion (audit.1): the dot-slash scan is anchored to
    # `args =` lines and to line-START quoted paths (multi-line TOML arrays put
    # each element on its own line). Deliberately NOT whole-file: AGENT_TASK is
    # user input, and a task quoting "../docker/x" MID-line must not brick the
    # boot (reproduced in review — toml renders such tasks as single lines where
    # the path never sits at line start).
    guard_no_relative_paths /data/agent.toml \
      "^args[[:space:]]*=.*[\"']\.\.?/" \
      "^[[:space:]]*[\"']\.\.?/|^[[:space:]]*\"/usr/lib/agentos/docker/" \
      "docker compose run --rm -e DRY_RUN_ONLY=1 -e ANTHROPIC_API_KEY=x -e AGENT_TASK=x -e TEMPLATE_NAME='$TEMPLATE' agent"
    # Remove any stale checkpoint — each 'docker compose run' is a fresh invocation.
    # Without this, switching TEMPLATE_NAME on a reused volume would restore the wrong agent.
    rm -f /data/checkpoint.json
    # DRY_RUN_ONLY=1|true|yes: print rendered config and exit (smoke-test path rewriting)
    if is_truthy "${DRY_RUN_ONLY:-}"; then
      cat /data/agent.toml
      exit 0
    fi
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
