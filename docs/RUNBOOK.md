# RUNBOOK — Operating AgentOS

> The single source of operational truth. Every command is runnable as written.
> **Current state:** `main` is **v0.20.0** — Phases 0–4 complete (p4.7 hardening
> landed all P0/P1 audit fixes) + **p5.1** (memory storage primitive) + **p5.2**
> (per-agent short-term + paging) + **p5.3** (per-agent long-term memory) + **p5.3.5**
> (detachable memory volume) shipped. Remaining Phase 5 and all of Phase 6 are
> *designed but not built* — marked ***(lands in pX.Y)*** throughout. Design lives
> in `DESIGN.md`; threats in `THREAT_MODEL.md` (referenced by §number, not restated);
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
```bash
ls /agents/                        # one dir per running agent (by id)
cat /agents/scout/status           # running | deferred | awaiting_child | done | failed
cat /agents/scout/context_size     # working-context token count
cat /agents/scout/budget           # spend vs token_budget
cat /agents/scout/flight           # this agent's flight tail
fusermount3 -u /agents             # manual unmount if a crash left it mounted
```

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
0600). **Inspect:** via `kv_get` from an agent, or `/agents/<id>/memory/` *(lands in
p5.7)*. Per-agent tiers, the shared KB, lexical search, and eviction land across
p5.2–p5.8 (`PHASE-5-PLAN.md`).

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

## 9. The phase ahead (Phase 5 / Phase 6 preview)

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
- *(lands in p5.7):* `/agents/<id>/memory/` + `/agents/kb/<segment>/` FUSE — read-only
  memory inspection without `jq`.
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
