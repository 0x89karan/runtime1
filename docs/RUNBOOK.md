# RUNBOOK — Operating AgentOS

> ⚠️ **PARTIALLY STALE** — This document was last fully updated at v0.20.0. The
> codebase is now at **v0.62.0** (cred.3.2). §11 (Credentials) has been updated to
> reflect the current `agentctl auth google` + secrets-file + credential broker flow.
> Other sections may still reference `cargo run -- agentd`, old env-var flows, or paths
> that no longer apply. When in doubt, consult `CHANGELOG.md` and `docs/plans/`.

> The single source of operational truth. Every command is runnable as written.
> Design in `DESIGN.md`; threats in `THREAT_MODEL.md` (referenced by §number);
> how to extend in `CONVENTIONS.md`.

---

## 1. Scope

This document covers **operating** AgentOS: deploying it (dev box and QEMU image),
configuring agents, hooking up model keys and MCP servers, running agents, day-2
operations (observability, budgets, backups, upgrades), troubleshooting, and security
operations. It does **not** cover *why* the system is shaped this way (`DESIGN.md`),
the *threat model* (`THREAT_MODEL.md` — cited here by section), or *how to extend the
code* (`CONVENTIONS.md`). Where a feature is planned but unbuilt, it is marked
***(lands in pX.Y)***.

---

## 2. Deployment modes

Three modes are real today. Cognition is always remote — every mode needs a reachable
`ANTHROPIC_API_KEY`.

### 2a. Dev mode (normal Linux dev box)
The default during development. `agentd` runs as an ordinary binary.

**Host requirements:**
| Need | Why | Check |
|---|---|---|
| Rust stable | build | `cargo --version` |
| Linux **5.13+** | Landlock V1 (FS confinement) | `uname -r` |
| Linux **6.7+** | Landlock V4 (TCP-port confinement, p4.6). Below this, a `Net{ports}` server **falls back to deny-all network** with a warning (§8) | `uname -r` |
| `fusermount3` | the `/agents` FUSE mount | `which fusermount3` (or run `--no-fuse`) |
| `runsc` on PATH | only if any server uses `isolation = "gvisor"` | `which runsc` |
| `ANTHROPIC_API_KEY` | remote cognition | `echo ${ANTHROPIC_API_KEY:+set}` |

**First run:**
```bash
cd agentd
export ANTHROPIC_API_KEY=sk-ant-...
cargo run -- agent.toml            # single agent (the scout demo)
tail -f flight.jsonl               # watch it think (separate shell)
```
**It's working when** `flight.jsonl` shows, in order:
`agent_spawned` → `perceive` → `inference_request` → `inference_response`
→ (`tool_call`/`tool_result`/`observe` cycles) → `agent_completed`. The final answer
prints to **stdout**; diagnostics go to **stderr**; structured activity to
`flight.jsonl`.

**Clean shutdown:** `Ctrl-C` (SIGINT) or `kill -TERM <pid>`. The scheduler records
`system_shutdown_requested`, checkpoints in-flight agents (`agent_checkpointed`), drains
the deferred queue (each emits `agent_admission_denied {reason:"shutdown"}`), and exits.

### 2b. QEMU image mode (the distro — `agentd` is the userspace)
A Buildroot musl rootfs that boots straight into `agentd`. This is "AgentOS as an OS."

**Prereqs & build:**
```bash
cd distro
make prereqs                       # one-time: checks host tools (qemu, jq, etc.)
make build                         # builds bzImage + rootfs.cpio.gz (slow first time)
```
**Provide the key** (host side; the init mounts it via virtio-9p as `secrets0` →
`/run/secrets`):
```bash
mkdir -p ~/.agentos-secrets
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > ~/.agentos-secrets/agentos.env
chmod 600 ~/.agentos-secrets/agentos.env       # never world-readable
```
**Boot & read output:**
```bash
make run                           # boots QEMU; agentd runs the demo agent
# flight log lands in the output0 9p mount on the host:
cat distro/build/output/images/run/flight.jsonl
```
`make test` runs a bounded boot (120 s) and asserts an `agent_completed` (or
`budget_exceeded`) event appears in the flight log — the smoke test.

**Mounts:** `secrets0` → `/run/secrets` (read the key from `agentos.env`), `output0`
→ `/run/output` (flight logs, checkpoints), `memory0` → `/run/memory` (durable store —
persists across container respawns; host path `~/.agentos-memory/`). DNS resolves via
QEMU SLIRP (10.0.2.3).
**Clean shutdown:** the agent completing exits `agentd`; the kernel then halts cleanly
(`-no-reboot`). For a long-running agent, the SIGTERM drain (§2a) applies.

### 2c. Hybrid (dev `agentd` → remote MCP servers)
Uncommon: `agentd` on a dev box, MCP servers reached over the network rather than
spawned as local stdio children. **Operationally:** today's MCP client is **stdio-only**
(spawned subprocess), so "remote MCP" means an MCP server you spawn locally that itself
proxies outbound — i.e. it needs a `Net` capability and the network-sandbox caveats of
§8 apply (the proxy's egress is what you must reason about, not `agentd`'s). A native
HTTP/SSE MCP transport *(lands with the Layer-2 KB increment — see ROADMAP "Beyond")*
will make true remote MCP first-class; until then, treat any network-reaching server as
a `Net{...}`-capable local subprocess and sandbox it accordingly.

---

## 3. Configuration reference

Authoritative. An agent is a TOML spec; secrets are **never** in it (§3, THREAT_MODEL
§1).

### `agent.toml` / `agents.toml`
```toml
# Single-agent form: [agent]. Multi-agent form: repeat [[agents]]. Both accepted.
[agent]
id           = "scout"             # required, unique
task         = "…"                 # the goal; empty → read from stdin (single form only)
name         = "Scout"             # optional; advertised in the AgentCard (p1.6)
description  = "Read-only researcher"   # optional; AgentCard
skills       = ["research"]        # optional; AgentCard
priority     = 0                   # u32, default 0; higher runs first under contention
token_budget = 100_000             # cumulative input+output ceiling; default 100_000
max_turns    = 20                  # inference-turn cap; default 20
capabilities = [ … ]               # see Capability vocabulary below; OMITTED = unrestricted

[model]
provider   = "anthropic"           # only "anthropic" today
model      = "claude-sonnet-4-6"   # model id sent to the provider
max_tokens = 4096                  # max tokens per inference response; default 4096

[tools]
native                   = ["all"] # built-ins; "all" OR explicit list. NOTE: kv_get/kv_set
                                   #   are NOT in "all" — list them explicitly (p5.1).
mcp_require_capabilities = false   # default false; SET true IN PRODUCTION (see §8)

[[tools.mcp_servers]]              # repeat per server
name         = "filesystem"
command      = "npx"
args         = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
capabilities = [ { FsRead = { prefix = "/workspace" } } ]   # → the sandbox profile
isolation    = "none"              # "none" (default) | "gvisor"

[scheduler]                        # multi-agent knobs; all optional
global_token_budget       = 0      # 0 = unlimited; otherwise a hard cross-agent ceiling
max_concurrent_inferences = 0      # 0 = unlimited; else the in-flight cap
max_spawn_depth           = 4      # recursive-spawn cap; default 4
checkpoint_interval_turns = 1      # auto-checkpoint cadence; 0 = SIGTERM-only

[memory]                           # durable key/value store (p5.1+)
store_path = "memory.redb"         # relative to CWD or absolute; default "memory.redb"
enabled    = true                  # false → store not opened, kv_get/kv_set not registered

# [egress]                         # HTTP forwarding proxy (p7.5b+); omit section to disable
# proxy_addr = "127.0.0.1:9100"   # host:port to bind; OS picks a free port with ":0"

log_path = "flight.jsonl"          # top-level; default "flight.jsonl" (p4.5)
```

**Capability vocabulary** (deny-by-default once `capabilities` is present;
`Some([])` = deny all; field absent = unrestricted):
| Capability | Grants | Notes |
|---|---|---|
| `FsRead { prefix }` | read under a path prefix | normalized, `..`-safe |
| `FsWrite { prefix }` | read+write under a prefix | |
| `Net { hosts, ports }` | outbound network | `ports` → Landlock V4 TCP confinement (p4.6); see §8 for the pre-6.7 fallback |
| `Mcp { server, tools }` | use named MCP tools | empty `tools` = all tools on that server |
| `Spawn` | spawn sub-agents | required for `spawn_agent` |
| `KbRead { segment }` | read a memory segment | **p5.1+** (prefix match like FsRead) |
| `KbWrite { segment }` | write a memory segment | **p5.1+** |

TOML capability syntax: `capabilities = [ { FsRead = { prefix = "/workspace" } }, { Net = { hosts = ["api.x.com"], ports = [443] } }, { KbWrite = { segment = "agent/scout" } } ]`.

### Environment variables
| Var | Required | Read where |
|---|---|---|
| `ANTHROPIC_API_KEY` | **yes** | `AnthropicGateway::from_env` at startup |
| `ANTHROPIC_BASE_URL` | no | gateway base-URL override (proxies/gateways) |
| `RUST_LOG` | no | `tracing` stderr verbosity (`info` default; `debug` for detail) |
| `AGENTOS_NO_FUSE` | no | any truthy value skips the FUSE mount (same as `--no-fuse`) |

### CLI flags & precedence
- `cargo run -- <config.toml>` — positional config path (default `agent.toml`).
- `--probe "hello"` — one-shot gateway sanity check (no agent loop).
- `--no-fuse` (p4.4) — skip the `/agents` mount.
- `--log-path PATH` (p4.5) — flight-log destination.
- **Flight-log path precedence:** `--log-path` > TOML `log_path` > default `flight.jsonl`.
- **FUSE-off precedence:** `--no-fuse` OR `AGENTOS_NO_FUSE` truthy → skipped.

### Filesystem layout
| Artifact | Dev mode | QEMU mode |
|---|---|---|
| flight log | `./flight.jsonl` (or `--log-path`) | `/run/output/flight.jsonl` → host `distro/build/output/images/run/` |
| checkpoint | `./checkpoint.json` (mode 0600) | `/run/output/checkpoint.json` |
| memory store (p5.1+) | `./memory.redb` (mode 0600) | `/run/memory/memory.redb` (detachable `memory0` volume — `~/.agentos-memory/`) |
| FUSE mount | `/agents/` (dev only; `--no-fuse` to skip) | not mounted in the minimal boot |

### Secrets handling
- **Dev:** export `ANTHROPIC_API_KEY` in the shell, or use `direnv`/an env file you
  source — never commit it. Never put it in `agent.toml` (THREAT_MODEL §1.1).
- **QEMU:** `~/.agentos-secrets/agentos.env` (mode 0600), mounted as `secrets0`.
- **Never in flight logs:** tool args/results are truncated to 200 chars
  (`PREVIEW_CHARS`, THREAT_MODEL §2.2) — but a ≤200-char secret passed as a tool arg
  still appears. **Don't pass secrets as tool inputs**; give MCP servers their own env
  (see §4 — and note p4.7 now filters `agentd`'s env out of MCP children).

---

## 4. Hooking up dependencies

### Model providers
**Anthropic (only provider today).**
```bash
export ANTHROPIC_API_KEY=sk-ant-...
# behind a proxy / gateway:
export ANTHROPIC_BASE_URL=https://your-gateway.example.com
```
Verify the key/route without running an agent:
```bash
cargo run -- --probe "say hello"          # prints the model's text reply
```
QEMU: put both vars in `~/.agentos-secrets/agentos.env`. **Adding a new provider** is a
code change (a new `impl InferenceGateway`, CONVENTIONS "Add an inference backend");
operator-side it's then `provider = "<name>"` in `[model]` + that provider's key in env.

### MCP servers
Each server is a stdio subprocess `agentd` spawns. **Always declare `capabilities`** so
the kernel sandbox is actually applied — a server with no `capabilities` runs
unsandboxed (THREAT_MODEL §6.2 BP-5). Note (p4.7): the child's environment is **filtered**
(`agentd`'s `ANTHROPIC_API_KEY` is *not* inherited) — give a server its own creds via
its config/args, not via `agentd`'s env.

**Filesystem** (path-scoped read/write):
```toml
[[tools.mcp_servers]]
name = "fs"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
capabilities = [ { FsRead = { prefix = "/workspace" } }, { FsWrite = { prefix = "/workspace" } } ]
```
Exposes read/write/list tools. Sandbox: Landlock FS confinement to `/workspace`,
seccomp fork/vfork block, net + mount namespaces. Failure mode: if `/workspace` doesn't
exist at spawn, the Landlock path-open fails → spawn error.

**Git:**
```toml
[[tools.mcp_servers]]
name = "git"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-git", "--repository", "/workspace/repo"]
capabilities = [ { FsRead = { prefix = "/workspace/repo" } }, { FsWrite = { prefix = "/workspace/repo" } } ]
```

**SQLite:**
```toml
[[tools.mcp_servers]]
name = "sqlite"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "--db-path", "/workspace/data.db"]
capabilities = [ { FsRead = { prefix = "/workspace" } }, { FsWrite = { prefix = "/workspace" } } ]
```

**Brave search** (needs network — read §8 first):
```toml
[[tools.mcp_servers]]
name = "brave"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
capabilities = [ { Net = { hosts = ["api.search.brave.com"], ports = [443] } } ]
# the server reads BRAVE_API_KEY from ITS env — set it where you launch agentd
```
`Net { ports = [443] }` → Landlock V4 confines outbound to TCP 443 **on kernel ≥ 6.7**.
On **< 6.7** it falls back to a deny-all network namespace with a `tracing::warn!` (p4.7
fix for AUDIT F-002 / THREAT_MODEL §6.2 BP-4) — meaning **the search server will have no
network on an old kernel**. Confirm via the `sandbox_applied` event's `landlock_net`
field (§6) before relying on outbound.

**Custom server (stub) — declare the binary path + its capability:**
```toml
[[tools.mcp_servers]]
name = "my-tool"
command = "/usr/local/bin/my-mcp-server"   # your binary; speaks MCP over stdio
args = []
capabilities = [ { FsRead = { prefix = "/srv/data" } } ]
```
A minimal server must answer the handshake (`initialize` → `notifications/initialized`
→ `tools/list`) and `tools/call`; if it doesn't, it spawns but exposes **no tools** (§7).

### gVisor (`isolation = "gvisor"`)
Use for **untrusted** MCP servers — it closes the namespace-only gaps BP-1…BP-3
(`clone3` spawn bypass, PID-ns) that the default path leaves open (THREAT_MODEL §6.2).
```bash
# install runsc (gVisor) and put it on PATH; then:
[[tools.mcp_servers]]
isolation = "gvisor"               # wraps the command in `runsc do`
```
`agentd` fails fast at startup if `isolation = "gvisor"` but `runsc` isn't on PATH.
`runsc do` is **experimental** (TODOS) — fine for read/network-isolated workloads;
verify FS-write servers behave before trusting them in production.

### The FUSE surface (`/agents/<id>/`)
Read-only. Mounted in dev mode unless `--no-fuse`/`AGENTOS_NO_FUSE`.

**Requires** `fusermount3` on PATH and `agentd` started without `--no-fuse`.
The `memory/` and `kb/` subtrees only appear when `[memory] enabled = true` is set.
```bash
ls /agents/                        # one dir per running agent (by id)
cat /agents/scout/status           # running | deferred | awaiting_child | done | failed
cat /agents/scout/context_size     # working-context token count
cat /agents/scout/budget           # spend vs token_budget
cat /agents/scout/flight           # this agent's flight events (JSONL — pipe through jq .)
fusermount3 -u /agents             # manual unmount if a crash left it mounted
```

**Memory surface (p5.7+)** — visible once `[memory] enabled = true`:
```bash
# Confirm the FUSE memory surface is active
grep '"kind":"fuse_mounted"' flight.jsonl | tail -1

# Inspect short-term context (up to 20 previews, one per line)
cat /agents/scout/memory/short_term
# Example output:
#   t0 user: Research the competitor landscape for AI agent runtimes
#   t1 assistant: I'll start by searching for existing frameworks...

# List long-term memory keys (nanosecond timestamp filenames; up to 100 shown)
ls /agents/scout/memory/long_term/
# 1749123456789012345  1749234567890123456

# Read a long-term memory entry (raw JSON with value + provenance)
cat /agents/scout/memory/long_term/1749123456789012345 | jq -r '.value'

# Scan all long-term entries for a keyword
grep -r "keyword" /agents/scout/memory/long_term/ 2>/dev/null | head

# Browse shared KB segments
ls /agents/kb/                     # one dir per segment (canon, scratch, etc.)
cat /agents/kb/canon/my-key        # raw JSON entry
ls /agents/kb/scratch/ | head -10  # up to 100 keys per segment

# jq one-liner to list all long-term entries with content previews
for f in /agents/scout/memory/long_term/*; do
  printf "=== %s ===\n" "$(basename "$f")"
  cat "$f" | jq -r '.value' 2>/dev/null | head -c 120
  echo
done
```

> **Notes:**
> - `long_term/` shows at most 100 keys; if the agent has stored more, the rest are
>   in the store but not listed (no truncation marker — check `mem_recall` or the redb file).
> - `kb/` only shows segments without the `agent/` namespace prefix (shared KB only;
>   per-agent long-term memory is under each agent's `long_term/` directory, not here).
> - `watch -n 1 cat /agents/scout/memory/short_term` works but avoid polling `ls /agents/kb/`
>   in a tight loop — every `ls kb/` triggers an O(n) namespace scan (p5.8 will add an index).

### The memory store *(p5.1 landed; tiers land in p5.2+)*
p5.1 ships a durable key/value store and two tools. **Point at it / enable it:**
```toml
[memory]
store_path = "/run/output/memory.redb"   # absolute for QEMU; default "memory.redb" (CWD)
enabled    = true
[tools]
native = ["read_file", "kv_get", "kv_set"]   # kv_* are NOT in "all" — list explicitly
```
Grant access with `KbRead`/`KbWrite` capabilities (deny-by-default). **Back it up** by
copying the single `memory.redb` file when `agentd` is stopped (it's a redb file, mode
0600). **Inspect:** via `kv_get` from an agent, or `/agents/<id>/memory/` (p5.7, landed).
Per-agent tiers, the shared KB, lexical search, and eviction land across p5.2–p5.8
(`PHASE-5-PLAN.md`).

### The interface *(lands in p6.x)*
A `ratatui` TUI shipped as a **separate `agentctl` binary** (read-only over `/agents`
+ `flight.jsonl`; `agentctl spawn <template>` to launch from a catalogue). It is an
*optional* second binary in the QEMU rootfs — the minimal boot stays `agentd`-only.
Full design + the seven views: `INTERFACE.md`.

---

## 5. Running agents (walkthroughs)

### 5a. Single-agent scout (works today)
```bash
cd agentd && export ANTHROPIC_API_KEY=sk-ant-...
cargo run -- agent.toml
# success: stdout shows the answer; flight.jsonl ends with agent_completed
jq -r 'select(.kind=="agent_completed") | .data.answer_preview' flight.jsonl
```

### 5b. Multi-agent + bus + spawn (works today)
Use an `agents.toml` with two `[[agents]]`, one holding `capabilities = [ { Spawn = {} } ]`
and tasked to spawn the other. Watch the cross-agent exchange:
```bash
cargo run -- agents.toml
jq -c 'select(.kind|test("agent_card_registered|message_sent|message_received|agent_spawned|agent_child_result_delivered"))' flight.jsonl
```
Success = a child `agent_spawned` with a `parent_id`, then `agent_child_result_delivered`.

### 5c. Capability denial (works today)
Give a read-only agent (`capabilities = [ { FsRead = { prefix = "/workspace" } } ]`) a
write task. Observe:
```bash
jq -c 'select(.kind=="capability_denied")' flight.jsonl
# → {"tool":"write_file","required":"FsWrite",...}
```
Read it as "the agent tried `write_file`; it lacked `FsWrite`; the registry blocked it
and returned an `is_error` tool result." Fix = add the needed capability (scoped tightly).

### 5d. Sandbox enforcement (works today)
Attach an MCP server with `capabilities = [...]` and inspect what was actually enforced:
```bash
jq -c 'select(.kind=="sandbox_applied") | {server:.data.server, enforced:.data.enforced}' flight.jsonl
```
Then set `mcp_require_capabilities = true` and add a server with **no** `capabilities` →
`agentd` **fails at startup** naming the offending server (THREAT_MODEL §6.2 BP-5).

### 5e. QEMU image (works today)
```bash
cd distro && make build
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > ~/.agentos-secrets/agentos.env && chmod 600 ~/.agentos-secrets/agentos.env
make run
jq -c 'select(.kind=="agent_completed")' build/output/images/run/flight.jsonl
```

### 5f. Checkpoint / restore (works today)
```bash
cargo run -- agents.toml &            # a long multi-turn run
kill -TERM %1                         # SIGTERM → system_shutdown_requested + agent_checkpointed
ls -l checkpoint.json                 # mode 0600, present
cargo run -- agents.toml              # restart → agent_restored, resumes from saved turn
jq -c 'select(.kind|test("agent_checkpointed|agent_restored"))' flight.jsonl
```

### 5g. Memory kv round-trip *(p5.1)*
Configure `[memory] enabled = true`, `native = [..., "kv_set", "kv_get"]`, grant
`KbWrite`/`KbRead { segment = "agent/<id>" }`, and task the agent to set then get a key:
```bash
jq -c 'select(.kind|test("memory_write|memory_read"))' flight.jsonl
# memory_write {agent, bytes}  then  memory_read {agent, found:true}
```

### Catalogue template walkthroughs *(land in p6.7)*
The seven starter templates (scout, librarian, journaler, code-aware, watcher,
coordinator, memory-custodian) and their `agentctl spawn` flows are specified in
`INTERFACE.md` §6 and ship with p6.7.

---

## 6. Day-2 operations

### Observability — `jq` recipes against `flight.jsonl`
```bash
# What did agent X do today (everything tagged to it):
jq -c 'select(.agent=="scout")' flight.jsonl

# Where are tokens going (per-agent total of the running total):
jq -r 'select(.kind=="inference_response") | "\(.agent) \(.data.total_tokens)"' flight.jsonl | sort -k2 -n

# Which capability denials fired:
jq -c 'select(.kind=="capability_denied") | {agent, tool:.data.tool, need:.data.required}' flight.jsonl

# Deferred vs admission-denied (contention vs terminal budget exhaustion):
jq -c 'select(.kind|test("agent_deferred|agent_admission_denied")) | {kind, agent, reason:.data.reason}' flight.jsonl

# Did sandbox enforcement degrade on this kernel (net not enforced)?
jq -c 'select(.kind=="sandbox_applied" and .data.enforced.landlock_net==false) | {server:.data.server}' flight.jsonl
jq -c 'select(.kind=="sandbox_skipped") | {server:.data.server, reason:.data.reason}' flight.jsonl

# Memory activity (p5.1):
jq -c 'select(.kind|test("memory_"))' flight.jsonl
```

### Budget management
- **Per-agent** `token_budget` (default 100k) → the agent emits `budget_exceeded` and
  stops. **Global** `[scheduler] global_token_budget` (0 = unlimited) → a hard
  cross-agent ceiling; once hit, new inferences are `agent_admission_denied`. **Always
  set a global budget in production** — a spawn tree can multiply per-agent budgets
  (THREAT_MODEL §4.2).
- **`priority`** (u32) orders the deferred queue under a `max_concurrent_inferences`
  cap — higher first, ties broken by arrival.
- **`agent_deferred`** = waiting for an inference slot (recoverable). **`agent_admission_denied`**
  = terminal (global budget exhausted or shutdown). Different events, different meaning.

### Updates
Rebuild `agentd`, drain in-flight agents with SIGTERM (they checkpoint), swap the
binary, restart (agents restore from `checkpoint.json`). Keep `checkpoint_interval_turns`
≥ 1 in production so an unclean kill loses at most one turn.

### Backups
- **`checkpoint.json`** — contains full conversation history; **sensitive** (treat like
  your home dir; THREAT_MODEL §3). Ephemeral (deleted on success), so back it up only if
  you need crash forensics.
- **`memory.redb`** (p5.1+) — durable; back it up with `agentd` stopped (mode 0600).
  In QEMU mode this lives at `~/.agentos-memory/memory.redb` (the detachable volume,
  p5.3.5). **Do not `make clean` this directory** — clean only deletes `output/`, not the
  memory volume.
- **Flight logs grow unbounded** — rotate (see §7). Put `--log-path` on a volume with
  room.

### Image upgrades (QEMU)
Replace **`bzImage` *and* `rootfs.cpio.gz` together** (`make build` produces both). The
kernel config (Landlock/seccomp/FUSE/9p) and the `agentd` binary in the rootfs are a
matched pair; a mismatched kernel can silently drop a sandbox mechanism the binary
assumes.

---

## 7. Troubleshooting

| Symptom | Likely cause | Confirm by | Fix |
|---|---|---|---|
| `agentd` won't start | bad TOML / missing `id` / missing key | stderr single-line error | fix config; `export ANTHROPIC_API_KEY` |
| won't start, mentions a server name | `mcp_require_capabilities=true` + a server with no/empty-effective `capabilities` | the startup `bail!` names it | add `capabilities`, or set the flag false |
| won't start, FUSE error | `fusermount3` missing / no `/dev/fuse` | stderr "FUSE mount failed" | install `fuse3`, or run `--no-fuse` |
| agent stuck `awaiting_inference` | provider down / 429 / bad key / wrong `ANTHROPIC_BASE_URL` / TLS | `jq 'select(.kind=="error" and .data.stage=="inference")' flight.jsonl` | fix key/URL; `--probe` to isolate; back off on 429 |
| agent stuck `awaiting_tool` | MCP server crashed / capability denied / sandbox blocked a syscall | server **stderr** (inherited, visible); `sandbox_applied` vs `sandbox_skipped` | restart server; widen capability; or it's correctly denied |
| `sandbox_applied` has `enforced.landlock=false` | kernel < 5.13 or no `CONFIG_SECURITY_LANDLOCK` | the event payload | upgrade kernel / rebuild with Landlock (THREAT_MODEL §6.2 BP-4) |
| `enforced.seccomp=false` / `spawn_enforcement:"none"` | non-x86_64 (`sandbox_skipped reason=deny-spawn-unsupported-arch`) | `sandbox_skipped` event | use x86_64, or `isolation="gvisor"` for hard spawn-deny |
| `landlock_net:false` after configuring `Net{ports}` | kernel < 6.7 → **deny-all fallback** (p4.7) | the startup `warn!` + `sandbox_applied` | the server now has **no** network on this kernel — upgrade to ≥6.7 for port-level egress, or drop the `Net` cap if it doesn't need network |
| MCP server spawns but **no tools** | handshake failed / protocol mismatch / no `tools/list` | server stderr; no `tools_registered` entry for it | fix the server; check `protocolVersion` |
| `capability_denied` storm | capability config too narrow for the task | `jq 'select(.kind=="capability_denied")'` | widen the specific capability (tightly scoped) |
| flight log fills disk | unbounded append | `du -h flight.jsonl` | rotate (`logrotate` on `*.jsonl`, or restart with a fresh `--log-path`); place on its own volume |
| `checkpoint.json.corrupt` on startup | prior unclean write; quarantined, started fresh (p4.7 robust path) | startup `error!` "checkpoint corrupt"; the `.corrupt` file | inspect `.corrupt` for forensics; the agent reran fresh — expected |
| `memory.redb.corrupt` on startup *(p5.1)* | corrupt store quarantined, fresh opened | `memory_quarantined` event | inspect `.corrupt`; restore from backup if needed |
| store unavailable, `kv_*` tools missing *(p5.1)* | store open/txn failed (perms, disk) | `memory_unavailable {stage,hint,error}` | fix path/perms; `agentd` proceeds without memory (best-effort) |
| QEMU boot hangs | key not in `secrets/` / 9p unmounted / kernel gap | `make run` console; the `init` ERROR lines | create `~/.agentos-secrets/agentos.env`; check `-virtfs` tags |

The failure signature is always **an event kind + key `data` fields** in the flight log —
grep for it before guessing.

---

## 8. Security operations

Threat-model findings as operator actions (canonical detail: `THREAT_MODEL.md §N`).

- **Key rotation (TM §1.1).** No in-process rotation: revoke at the Anthropic console →
  replace `ANTHROPIC_API_KEY` in env / `agentos.env` → restart `agentd`. Drain first
  (SIGTERM) so in-flight agents checkpoint.
- **MCP server trust (TM §6).** Give *every* server an explicit `capabilities = [...]`
  and set `mcp_require_capabilities = true` so a forgotten one fails startup rather than
  running unsandboxed (BP-5). Untrusted-source servers → `isolation = "gvisor"`
  (closes the namespace-only BP-1…BP-3 gaps). p4.7 fixed the env leak: `agentd`'s
  secrets are no longer inherited by MCP children — give each server only the env it
  needs.
- **Checkpoint posture (TM §3.3).** Mode 0600 is automatic; **at-rest encryption is the
  open gap.** For long-lived checkpoints on shared hosts, keep CWD on a LUKS-encrypted
  or `0700` volume until in-process encryption ships. Same posture for `memory.redb`.
- **Egress posture (TM §6).** What leaves the box: HTTPS to `api.anthropic.com`, plus
  whatever `Net`-capable MCP servers reach. With Landlock V4 (kernel ≥ 6.7) outbound is
  port-confined per the `Net{ports}` grant; below 6.7 a `Net` server is **fully network-
  isolated** (deny-all, p4.7). Confirm enforcement, don't assume:
  `jq 'select(.kind=="sandbox_applied") | .data.enforced' flight.jsonl`. For a live
  check, run the server's namespace under `ip netns`/`ss` audit.
- **Supply chain (TM §5).** `cargo audit` runs in CI (added in p4.7); static musl binary,
  rustls — no system OpenSSL / dynamic loading.
- **Audit ("what did agent X do yesterday?").** The flight log is the record:
  `jq -c 'select(.agent=="X")' flight.jsonl` (or filter by date range on `.ts`). It is
  append-only and not tamper-evident (TM scope) — protect the file, don't rely on it as
  a security control.

---

## 9. HTTP egress proxy (p7.5b+)

`agentd` can run a lightweight HTTP forwarding proxy so workloads talk to
`api.anthropic.com` through a single audited gateway — not directly.

### Enabling the proxy

Add to `agent.toml` (or `agents.toml`):
```toml
[egress]
proxy_addr = "127.0.0.1:9100"   # ":0" → OS assigns a free port
```

On startup, `agentd` binds the proxy, captures the bound port, and prints:
```
INFO agentd::egress: egress proxy started addr=127.0.0.1:9100
```
If the bind fails, `agentd` exits immediately (fail-closed). Omit the `[egress]`
section entirely to disable the proxy (default).

### Discovering the bound port

After the proxy starts, the bound address is exposed in the FUSE filesystem:
```bash
cat /agents/system/egress_addr     # → "http://127.0.0.1:9100"
```
Returns `"not configured"` when the proxy is disabled.

### Wiring a workload

Each workload that should route through the proxy must be registered with a unique
ephemeral key (generated by `agentd`). The workload sets:
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:9100
export ANTHROPIC_API_KEY=sk-ant-WORKLOAD-<agent_id>-<random>   # issued by agentd
```
The real `ANTHROPIC_API_KEY` never leaves the proxy — child processes see only the
ephemeral key. The proxy strips the ephemeral key, injects the real key, and forwards
the request to `https://api.anthropic.com/v1/messages`.

Requests to any path other than `/v1/messages` receive `404`. Requests with
`Accept: text/event-stream` receive `501` (streaming deferred to p7.5c).

### Streaming limitation

The proxy in p7.5b buffers the full upstream response (max 8 MB). SSE/streaming
inference through the proxy is not yet supported; attempts return:
```json
{"type":"error","error":{"type":"streaming_not_supported",
  "message":"SSE streaming is not supported by this proxy version",
  "detail":"streaming_not_supported"}}
```
Native streaming (`streaming = true` in `[model]`) still works — it calls
`api.anthropic.com` directly via `AnthropicGateway`, bypassing the proxy.

### Verifying proxy started

```bash
# 1. Check the FUSE file
cat /agents/system/egress_addr

# 2. Scan the flight log for the startup event
jq -c 'select(.kind=="egress_proxy_started")' flight.jsonl

# 3. Confirm the port is listening
ss -tlnp | grep 9100          # Linux; use lsof -i :9100 on macOS
```

---

## 11. Chief of Staff — Daily Operating Brief (cos.1)

An always-on agent that reads Gmail every day and writes a structured Operating Brief.
Autonomy L0 (read-only). All five trust properties are live and verifiable.

### 11.1 Overview

| Property | How it works |
|---|---|
| Wake on schedule | `cron_mcp.py` `wait_for_trigger` (daily 08:00 UTC default) |
| Read Gmail without holding the token | `oauth_mcp.py` sidecar — token lives in the process, never in the agent context |
| Multi-agent | Orchestrator spawns Inbox + Curator each cycle via `spawn_agent` |
| Brief persistence | KB `ops:briefs` (log) + `ops:entities` (scratch) |
| Tamper-evident | Ed25519-signed receipt chain in `evidence.jsonl` |

**Cost:** ~50 k tokens/day × $3/1M = ~$0.15/day (Sonnet for both orchestrator and inbox).

### 11.2 Prerequisites

```bash
# Check required tools
cargo --version           # Rust stable
python3 --version         # Python 3.8+
which fusermount3          # for agentctl watch (or run agentd with --no-fuse)
```

Google Cloud Console setup (one-time):
1. Create/select a project → **APIs & Services → Library** → enable **Gmail API**.
2. **OAuth consent screen** → External → add your email as a Test user.
3. **Credentials → Create → OAuth client ID** → Application type: **Desktop app**.
4. Copy the **Client ID** and **Client Secret**.

### 11.3 Credentials (cred.3.2 — current flow)

**One-time setup on the Mac host:**

```bash
# 1. Create the secrets directory (one time only)
mkdir -p ~/.agentos-secrets

# 2. Write your Anthropic key into the secrets file
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > ~/.agentos-secrets/agentos.env
chmod 600 ~/.agentos-secrets/agentos.env

# 3. Provision Google credentials (opens a browser for OAuth consent)
agentctl auth google \
  --client-id YOUR_CLIENT_ID \
  --client-secret YOUR_CLIENT_SECRET
# Writes ~/.agentos-secrets/google.json (mode 0600)
```

`docker-compose.yml` mounts `~/.agentos-secrets` as `/run/secrets:ro` in both the `cos`
and `agent` services. The entrypoint sources `agentos.env` automatically before any key
checks — no shell exports needed.

**Schedule (optional):**
```bash
# Add to docker-compose.yml agent or cos environment block, or export in your shell:
TRIGGER_INTERVAL="every 2m"   # cos default; accepts cron expressions too
```

### 11.4 First run (Docker — recommended)

```bash
# Start the CoS (runs continuously; Ctrl-C to stop)
docker compose up cos

# Watch in a second terminal:
docker compose exec cos agentctl watch --url http://localhost:7999
# or, from the host (if management API exposed):
agentctl watch --url http://localhost:7999
```

The Inbox agent handles the first-run Gmail authorization automatically using the
refresh token written by `agentctl auth google`. No browser dance required on
subsequent runs — the one-time browser step is §11.3 step 3 (`agentctl auth google`).

### 11.5 Subsequent runs

```bash
# CoS resumes from checkpoint automatically on restart:
docker compose up cos

# Named volume cos-data holds checkpoint, memory, evidence — survives --rm.
# To reset state completely (rare):
docker compose down -v   # WARNING: deletes cos-data volume + all KB
```

The orchestrator parks on `wait_for_trigger` between brief cycles. Ctrl-C checkpoints
state; restart picks up from where it left off.

### 11.6 Where the brief lands

**Dev mode (cargo run):**
```bash
ls ./output/brief-*.md          # written by the orchestrator
cat ./output/brief-$(date +%Y-%m-%d).md
```

**QEMU/distro mode:**
The `output0` 9p mount at `/run/output` is readable from the host at `distro/build/output/images/run/`.

### 11.7 Verifying the trust story

```bash
# 1. Agent never holds the Gmail token (no ya29.* in any flight event):
grep -E "ya29\.|access_token|refresh_token" flight.jsonl && echo "FAIL" || echo "PASS"

# 2. Egress confined (any off-domain attempt logged as denied):
jq 'select(.kind=="egress_rejected")' flight.jsonl

# 3. Signed receipt chain (exit 0 = chain intact):
cargo run --bin agentctl -- verify evidence.jsonl

# 4. Cost bounded (total tokens per run):
jq -r 'select(.kind=="inference_response") | .data | (.input_tokens + .output_tokens)' flight.jsonl \
  | paste -sd+ | bc

# 5. Approval gate (L1 mode only — confirms no send without Approve):
jq 'select(.kind=="REQUEST_APPROVAL" or .kind=="APPROVAL_GRANTED" or .kind=="APPROVAL_DENIED")' flight.jsonl
```

### 11.8 Monitoring with agentctl watch

```bash
# From inside the container:
docker compose exec cos agentctl watch --agents-dir /agents

# From the host via management API (p7.7+) — requires port forwarding in docker-compose.yml:
#   ports: ["7999:7999"]   (under the 'cos' service; not enabled by default)
agentctl watch --url http://localhost:7999
```

Key views:
- **Dashboard** (default): shows orchestrator + spawned inbox/curator agents, budget, status.
- **Topology** (`[t]`): spawn tree — orchestrator → inbox-YYYY-MM-DD + curator-YYYY-MM-DD.
- **Approvals** (`[a]`): approve/deny pending requests (OAuth URL on first run; L1 send drafts).
- **Inspector** (`[i]`): flight log with filter for Sandbox/CapDenied events.
- **Memory** (`[m]`): browse `ops:briefs` and `ops:entities` KB content.

### 11.9 Known limits and operational notes

**max_turns = 200,000** — cron polling at 25 s/turn burns ~3,456 turns/day just waiting.
The default (20) would kill the orchestrator in under 8 minutes before the first brief.
`cos.agents.toml` sets 200,000 (≈58 days of continuous polling).

**token_budget = 5,000,000,000** — `tokens_spent` persists in checkpoint and never resets.
5B tokens ≈ 50k/day × 365 days × 274× margin. When you see `budget_exceeded`:
```bash
rm agentd/checkpoint.json   # clears the accumulated spend counter
# then restart agentd normally — KB and brief files are preserved
```

**Child ID collision** — the orchestrator spawns `inbox-YYYY-MM-DD` and `curator-YYYY-MM-DD`
on each cycle. These date-stamped IDs are required; a static ID like `inbox-agent` would
collide on the second cron cycle (terminated children stay in the scheduler's outcome map).

**cron_mcp restarts** — on agentd restart, `cron_mcp.py` recomputes the next fire time.
Events that would have fired during downtime are not replayed.

### 11.10 Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| No brief after 24h; `max_turns_reached` in flight.jsonl | max_turns too low | Check cos.agents.toml has `max_turns = 200_000` |
| `ERROR: ANTHROPIC_API_KEY is not set` | Key not in agentos.env or shell env | Run `printf 'ANTHROPIC_API_KEY=sk-ant-...\n' > ~/.agentos-secrets/agentos.env` |
| `ERROR: Google credentials not provisioned` | google.json missing | Run `agentctl auth google --client-id ... --client-secret ...` |
| `{"error":"host_not_allowed"}` from Gmail | MCP server passenv issue | Check `docker compose exec cos env | grep OAUTH` |
| OAuth URL never appears | agentctl watch not running | `docker compose exec cos agentctl watch --agents-dir /agents`, press `[a]` |
| Orchestrator dies after one brief | Task prompt missing re-trigger instruction | Check orchestrator task ends with explicit `wait_for_trigger` loop |
| Second cron cycle spawns no children | Child ID collision | Verify orchestrator uses date-stamped child IDs |
| `budget_exceeded` in flight.jsonl | Lifetime token budget exhausted | `docker compose exec cos rm /data/checkpoint.json` and restart |
| `agent_admission_denied` in flight.jsonl | Child ID collision guard fired | Child ID is already in outcomes map; confirm date-stamping |

### 11.11 Credential broker operations (cred.3.2+)

The credential broker (`CredentialGateway`) runs as a loopback HTTP server inside `agentd`.
It resolves upstream API credentials so MCP servers never hold them directly.

**How the broker starts:**
1. `agentd` reads `[credential_gateway.providers.*]` from the TOML config.
2. For each provider, it resolves `upstream_base` via DNS and pins the result
   using `reqwest::ClientBuilder::resolve()` (IP pinning — DNS rebinding blocked).
3. It rejects `upstream_base` values that resolve to loopback, private, or
   link-local addresses (SSRF guard).
4. The broker binds on `127.0.0.1:0`; the OS-assigned port is injected into each
   MCP server's environment as `AGENTD_CREDENTIAL_GATEWAY_URL`.

**Verifying the broker is running:**
```bash
# Flight event emitted at startup:
jq 'select(.kind=="credential_gateway_started")' flight.jsonl

# Each MCP server token registration:
jq 'select(.kind=="credential_token_registered")' flight.jsonl
```

**OAuth token cache on disk:**
The broker writes the OAuth token state to the provider's secrets file path (same
`~/.agentos-secrets/google.json` written by `agentctl auth google`). It does NOT
hold tokens in memory between process restarts — on startup it re-reads the cache
from disk and checks token expiry before use.

**Token refresh failures:**
```bash
# Watch for refresh failures (emit on every failed refresh):
jq 'select(.kind=="credential_refresh_failed")' flight.jsonl
# If you see token_written: false — secrets file is on a 9p mount with non-atomic
# rename (QEMU). A subsequent request triggers a re-fetch; if the refresh token
# rotated (single-use), run: agentctl auth google --client-id ... --client-secret ...
```

**Security: what the broker does NOT do:**
- Tool output is **not** scanned for credential-shaped tokens (no `SecretRewriter`).
  `flight.jsonl` may contain live tokens that appear in API responses. Restrict
  flight log access accordingly.
- The `EgressBrokered` event records routing only; it does NOT indicate content was audited.
  See §8.7 in `THREAT_MODEL.md`.

**Adding a new provider:**
```toml
[credential_gateway.providers.my-api]
type         = "api-key-header"
upstream_base = "https://api.example.com"
header_name  = "Authorization"
secret_env   = "MY_API_SECRET"   # read from environment at startup
```

Then grant the `credential:custom` capability to any agent that should use it.

---

## 10. The phase ahead (Phase 5 / Phase 6 preview)

What's landed vs coming, with operational implications. Design: `DESIGN-memory.md`,
`PHASE-5-PLAN.md`, `INTERFACE.md`.

**Phase 5 — memory.**
- *p5.1 (landed, v0.18.0):* durable kv store (`memory.redb`), `kv_get`/`kv_set` tools,
  `KbRead`/`KbWrite` caps, events `memory_read` / `memory_write` / `memory_unavailable`
  / `memory_quarantined`. Ops: back up `memory.redb`; grant KB caps deny-by-default.
- *p5.2 (landed, v0.19.0):* per-agent short-term + token-budget paging; `checkpoint.json`
  format bumps to v2 (a v1 checkpoint still loads).
- *p5.3 (landed, v0.20.0):* per-agent long-term memory (`mem_remember`/`mem_recall`) persisting
  across runs; don't delete `memory.redb` on success, unlike `checkpoint.json`.
- *p5.3.5 (landed):* detachable memory volume — `memory0` 9p mount (`~/.agentos-memory/`
  → `/run/memory`); store survives container respawn; `make run/test` wired automatically.
- *(lands in p5.4):* shared KB segments with provenance — new backup surface; segment
  capabilities become a real authorization boundary.
- *(lands in p5.5–p5.6):* `kb_search` (lexical) + eviction/age floors (a growth knob to
  tune).
- *p5.7 (landed, v0.24.0):* `/agents/<id>/memory/` + `/agents/kb/<segment>/` FUSE — read-only
  memory inspection (see §4 "Memory surface" walkthrough).
- **Semantic search** is *not* in the embedded store: it arrives only by attaching an
  **external hybrid KB as an MCP server** (Layer 2), embeddings from a remote API
  (Voyage/Cohere/OpenAI) — operationally just another `Net`-capable, sandboxed MCP
  server. See ROADMAP "Beyond".

**Phase 6 — interface** *(lands in p6.x)*. A `ratatui` TUI as a separate **`agentctl`**
binary (not in `agentd`; the 6 MB CI guard is untouched). Runs on the QEMU serial console
or over SSH; read-only over `/agents` + `flight.jsonl`; spawning agents from a template
catalogue. It is an optional addition to the QEMU image — headless deployments omit it.
The Watcher (daemon-shaped) template needs an event-trigger surface that lands *after*
Phase 6.
