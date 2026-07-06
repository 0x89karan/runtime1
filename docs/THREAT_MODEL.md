# AgentOS / agentd Threat Model

This document enumerates the security boundaries, threats, controls, and known
limitations of the `agentd` runtime as of v0.25.0 (Phase 5.8). It is the operator
reference for understanding what the sandbox stops, what it doesn't, and why.

---

## Scope and assumptions

**Threat actors considered:**

- **Malicious or buggy MCP server** — a tool server that tries to exfiltrate
  secrets, spawn child processes, or reach the network beyond its declared
  capabilities.
- **Prompt-injection from tool output** — LLM output crafted to leak sensitive
  context via flight.jsonl or checkpoint.json.
- **Resource exhaustion** — an agent (or its task) deliberately or accidentally
  driving unbounded token/API cost.
- **Supply chain** — a compromised Rust dependency reaching the runtime.

**Not in scope:**

- Attacks requiring physical access to the host.
- Multi-tenant isolation — AgentOS is single-tenant by design. If an attacker
  has a shell, they have everything.
- Post-compromise forensics — flight.jsonl is not tamper-evident.

---

## 1. Secret handling

### 1.1 `ANTHROPIC_API_KEY`

| Property | Status |
|---|---|
| Read from | Environment variable (`std::env::var`) at startup only |
| Stored as | `String` field on `AnthropicGateway` |
| `Debug` impl | Absent — key cannot appear in `{:?}` output |
| Logged | Never — flight recorder never touches the struct |
| In config | Never — TOML spec forbids secrets (see CONVENTIONS.md) |
| In error messages | Never — `bail!("Anthropic API {status}: {msg}")` does not include the key |

**Control:** The key lives only in the `AnthropicGateway` struct for the lifetime
of the process. It is transmitted exclusively via the `x-api-key` HTTP header to
`api.anthropic.com` over TLS (rustls, no system CA store required).

**Known gap:** No key rotation support. If the key leaks, it must be revoked at
the Anthropic console and a new one set in the environment.

### 1.2 Other secrets

There are no other credentials in the current codebase. MCP servers that need
their own credentials receive them via their own environment (configured at the OS
level, not by agentd).

### 1.3 MCP subprocess environment isolation

MCP server subprocesses are spawned with `env_clear()`: the parent process's full
environment, including `ANTHROPIC_API_KEY`, is **not** inherited. Each subprocess
receives only a vetted allowlist (`PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `TMPDIR`)
plus any explicit `env` key-value pairs declared in the server's `[[tools.mcp_servers]]`
config entry. This containment is independent of Landlock/seccomp (which operate on
filesystem/syscall access) and applies even to unsandboxed servers.

---

## 2. Flight recorder (flight.jsonl)

`flight.jsonl` is an append-only JSONL file written to CWD. It is a structured
activity log — not a secret store — but it may contain excerpts from agent tasks,
tool inputs, and tool results.

### 2.1 What was logged (pre-p4.3, now fixed)

Before this increment, `ToolCall` events recorded the **full, untruncated** tool
input as `input`. A `write_file` call with a large file body, or any tool called
with an argument containing a secret, would land verbatim in the log.

Similarly, `AgentSpawned` events recorded the **full task string**, and `ToolResult`
error events recorded the **full error message** as `error` — which could echo
back the tool input that caused the failure. Both could contain sensitive context.

### 2.2 What is logged now (post-p4.3)

All event fields that carry user-supplied or tool-supplied text are now truncated
to a 200-character preview:

| Event | Field | Before | After |
|---|---|---|---|
| `agent_spawned` | `task` (full) | verbatim | `task_preview` (≤200 chars + `…`) |
| `tool_call` | `input` (full JSON) | verbatim | `input_preview` (≤200 chars + `…`) |
| `tool_result` | `preview` (success path) | ≤200 chars ✓ | unchanged |
| `tool_result` | `error` (error path) | verbatim | ≤200 chars (fixed in p4.3) |
| `perceive` | `preview` | ≤200 chars ✓ | unchanged |
| `inference_response` | `preview` | ≤200 chars ✓ | unchanged |
| `agent_completed` | `answer_preview` | ≤200 chars ✓ | unchanged |

**Remaining limitation:** 200-char truncation does not prevent short secrets
(e.g. a 32-char API key) from appearing if passed as a tool argument. The correct
defence for high-sensitivity deployments is to never pass secrets as tool inputs
— use environment variables or file-based credentials instead. This is a documented
operational constraint, not a future code fix.

### 2.3 File permissions

`flight.jsonl` is created with the process's umask (typically `0644`). On a
single-tenant AgentOS installation the file is world-readable only if the umask
allows it. Operators running agentd with sensitive tasks should set `umask 0077`
or restrict the CWD.

---

## 3. Checkpoint (checkpoint.json)

`checkpoint.json` contains the **full, unredacted conversation history** of each
agent: every message exchanged with the model, including tool calls and results.
This is a structural requirement — checkpoint/restore needs complete fidelity to
resume a run correctly.

### 3.1 Risk window

The checkpoint file exists **only while a run is active or after a crash**. On
successful completion or restore, `CheckpointStore::remove()` deletes it via
`std::fs::remove_file`. The risk window is:

1. **During a run** — the file is on disk for the entire run duration.
2. **After a crash** — the file persists until the next startup, which renames a
   corrupt checkpoint to `.corrupt` and starts fresh (no silent overwrite).

### 3.2 Mitigations in place

- File is written atomically (tmp → rename) — no partial writes readable by
  concurrent processes.
- File is deleted immediately after successful restore.
- Tmp file is created with mode 0600 (`O_CREAT | 0600` via `OpenOptions::mode()`);
  `rename(2)` preserves those permissions on the final `checkpoint.json`. On Unix
  the file is never world-readable regardless of the process umask.

### 3.3 Known gap (tracked in TODOS.md)

`checkpoint.json` has no encryption. Mode restriction (0600) was added in p4.4,
which prevents world-readable leaks on shared filesystems. The remaining gap is
at-rest encryption: a future increment should encrypt the checkpoint with a key
derived from the agent's identity so that even a user with read access to the file
cannot recover the conversation history.

**p5.3.5 note — memory volume durability widens the at-rest window.** `memory.redb`
(Tiers 3/4) now lives on a *persistent, detachable* host volume (`~/.agentos-memory/`)
rather than the ephemeral output mount. Unlike `checkpoint.json` (deleted on success),
this file accumulates long-term agent memory across many runs. It has the same mode-0600
protection but is never automatically cleaned — the at-rest encryption gap applies with
a larger window. **Operator action (same as §3.3 above):** keep `~/.agentos-memory/`
on a LUKS-encrypted or `0700` directory on shared hosts until in-process encryption
ships.

---

## 4. Budget-exhaustion DoS

An agent (or a malicious task) that generates unbounded token usage would run up
API costs without limit.

### 4.1 Guards in place

| Guard | Mechanism |
|---|---|
| Per-agent token budget | `token_budget: u64` in `AgentConfig` (default 100,000 tokens). Agent emits `budget_exceeded` and stops. |
| Global token budget | `global_token_budget: u64` in `SchedulerConfig` (0 = unlimited). Scheduler emits `agent_admission_denied` and rejects new inferences once exceeded. |
| Max turns | `max_turns: u32` per agent (default 20). Hard cap on the number of inference calls regardless of token count. |
| Spawn depth | `max_spawn_depth: u32` in `SchedulerConfig` (default 4). Prevents recursive agent spawning from amplifying cost exponentially. |

### 4.2 Threat: per-agent budget bypass via spawn

A parent agent at budget limit can still spawn a child with a fresh budget. The
global token budget (`global_token_budget`) is the backstop. Without a global
budget, a tree of spawned agents could together exceed any intended spend limit.

**Recommendation:** always set `global_token_budget` in production deployments.

### 4.3 Threat: MCP response flooding

A malicious MCP server can return arbitrarily large responses. The transport cap
is `MAX_RESPONSE_BYTES = 4 MiB` in `agentd/src/tools/mcp.rs`. Responses exceeding
this are rejected as errors, not injected into the agent's context.

---

## 5. Supply chain

### 5.1 Dependency footprint

`agentd` has 15 direct dependencies (see `Cargo.toml`). All are well-maintained
crates with active maintainers:

```
anyhow, async-trait, chrono, futures, libc, nix, reqwest (rustls-tls),
serde, serde_json, surfaces (local), sandbox (local), tokio, toml,
tracing, tracing-subscriber
```

No deprecated, unmaintained, or single-maintainer crates with a history of
security incidents.

### 5.2 CVE scanning

**Known gap:** `cargo audit` is not installed and CVE scanning is not part of CI.
This means a known-vulnerable transitive dependency would not be caught
automatically. Mitigations:

- `cargo update` is run before each release to pull patch-level fixes.
- Dependabot or a scheduled `cargo audit` job should be added to CI in a future
  increment.

### 5.3 Binary verification

The release binary is built via `cross` against `x86_64-unknown-linux-musl` and
linked statically. No dynamic library loading at runtime means no LD_PRELOAD or
library substitution attacks.

### 5.4 TLS

`reqwest` is configured with `rustls-tls` (no system OpenSSL). Certificate
verification uses the bundled `webpki-roots` trust store. This removes the
system CA store as an attack surface.

---

## 6. Sandbox coverage and known bypass vectors

The sandbox is applied to MCP server subprocesses only. `agentd` itself runs
unsandboxed (it is the runtime).

### 6.1 What the sandbox enforces

| Mechanism | What it stops |
|---|---|
| **Landlock LSM** (`AllowFsRead`/`AllowFsWrite`) | Filesystem access outside declared path prefixes |
| **seccomp-bpf** (`DenySpawn`) | `fork(2)` and `vfork(2)` on x86_64 |
| **Network namespace** (`IsolateNetwork`) | All outbound/inbound network by default; disabled if `Net` capability is declared |
| **Mount namespace** (`IsolateMount`) | Mount/unmount operations |
| **gVisor** (`isolation = "gvisor"`) | Full syscall interception via Sentry; recommended for adversarial workloads |

### 6.2 Known bypass vectors (not yet fixed)

These are documented in full here and tracked in TODOS.md. Publishing them is
intentional — operators need to know the exact limits of the sandbox they are
running.

**BP-1: `clone(56)` / `clone3(435)` bypass for `DenySpawn`**
— Severity: Medium (namespace-only path)

The seccomp filter blocks `fork(57)` and `vfork(58)` but not `clone(56)` or
`clone3(435)`. A sandboxed MCP server on x86_64 can still call
`clone(SIGCHLD, ...)` to spawn a child process. Classic BPF cannot inspect
`clone` flags to distinguish thread-create from process-create without
`SECCOMP_DATA_ARGS`. The namespace-only sandbox path (`IsolateNetwork` +
`DenySpawn`) is therefore bypassable for spawn prevention.

**Mitigation:** Use `isolation = "gvisor"` for MCP servers where spawn prevention
is a hard requirement. gVisor's Sentry intercepts `clone3` correctly.

**BP-2: `DenySpawn` is a no-op on aarch64**
— Severity: Low (development environments; production target is x86_64)

The seccomp fork/vfork filter is gated under `#[cfg(target_arch = "x86_64")]`.
On aarch64, `DenySpawn` emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }`
instead of installing a filter. The flight log correctly reflects this — unlike
earlier versions where `SandboxApplied` fired falsely.

**BP-3: `CLONE_NEWPID` does not isolate the MCP server itself**
— Severity: Low (PID visibility only)

`unshare(CLONE_NEWPID)` in `pre_exec` creates a new PID namespace for the MCP
server's future children, but the server process itself remains in the parent PID
namespace. A second fork before exec is needed to put the server in the new
namespace. Deferred to a future increment.

**BP-4: Landlock degrades silently on kernels < 5.13**
— Severity: Low (QEMU target runs 6.x)

On kernels without `CONFIG_SECURITY_LANDLOCK`, Landlock compiles to a BestEffort
no-op. The `EnforcementStatus` struct (added in p4.1) reports `landlock: false` in
the `SandboxApplied` flight event so operators can detect degraded enforcement.

**BP-4a: `Net{ports}` with Landlock ABI < V4 (kernel < 6.7)**

TCP port enforcement (`AllowNetConnect`) requires Landlock ABI V4 (Linux ≥ 6.7).
On kernels < 6.7, if an MCP server declares `Net{ports:[...]}`, agentd falls back
to `IsolateNetwork` (deny-all network), emits a `tracing::warn!`, and logs
`landlock_net: false` in `SandboxApplied`. This is intentionally conservative:
deny-all is safer than silently granting unrestricted network access when
per-port enforcement is unavailable.

**BP-5: MCP server without `capabilities` runs fully unsandboxed**
— Severity: Medium (mitigated by `mcp_require_capabilities = true`)

`capabilities = None` (the default, for backward compatibility) bypasses all kernel
enforcement. Set `mcp_require_capabilities = true` in `[tools]` to make sandboxing
mandatory — startup will fail if any server omits `capabilities`.

**BP-6: Landlock path traversal via symlinks**
— Severity: Low (path-prefix bypass only)

Landlock path-beneath rules are applied at path open time by the kernel, which
follows symlinks. A symlink inside a granted prefix can point outside it. The
capability prefix check in `normalize_path` does not resolve symlinks (it uses
`Path::components()` with no filesystem access). This is a defence-in-depth gap:
Landlock itself enforces at the kernel level and does resolve symlinks, so the
capability check is the weaker of the two layers.

---

## 7. Memory substrate (p5.1+)

Phase 5 added a persistent key-value store (`memory.redb`) shared across agent runs.
This section enumerates the new threat surface it introduces.

### 7.1 Shared-KB cross-agent information flow (poisoning / integrity)

Agents write to shared KB segments via `kb_put`. The runtime enforces mutability
class invariants before every write:

| Class | Write semantics | Agent-write allowed? |
|---|---|---|
| `canon` | Operator-seeded at startup via `[[memory.segments]]` | **No** — runtime returns an error |
| `log` | Append-only; auto-generated monotonic key per write | Yes — entry is immutable once written |
| `scratch` | Last-writer-wins; version counter incremented atomically | Yes — any agent with `KbWrite` can overwrite |

**Provenance is stamped by the runtime** (from `ToolContext`) — not from tool
input — so agents cannot forge the `agent_id`, `turn`, `task_fp`, or `ts` fields
stored with each entry.

**Remaining gap:** Scratch segments allow any `KbWrite`-holding agent to clobber
another agent's entry. This is by design (LWW semantics) but means a misbehaving
agent can corrupt shared scratch state. Use `log` or `canon` for append-only or
immutable data. Verifiable history on scratch is deferred to a later increment.

### 7.2 Provenance integrity

`kb_put`/`kb_get` both require a `KbWrite`/`KbRead` capability for any segment
outside the agent's implicit self-namespace (`agent/<id>`). An agent without the
appropriate capability receives a `capability_denied` event and an error — it
cannot read or write shared segments it wasn't granted access to.

**Remaining gap:** There is no output-side redaction. An agent that legitimately
holds `KbRead { segment: "project:notes" }` can read all entries in that segment
and include excerpts in its response. The operator is responsible for only granting
`KbRead` to agents that should see that data.

**Provenance `citation` field is untrusted.** The `citation` field in a stored entry
is set by the agent at write time and is not validated by the runtime. It is a
convenience annotation, not a verified reference. Do not make security decisions
based on `citation` alone.

### 7.3 KB as exfiltration channel

An agent with `KbWrite { segment: "shared:notes" }` and implicit access to the
environment (e.g. via a tool result that echoes back env vars) could persist
sensitive values to the KB before the response reaches the operator. Controls:

- Capability segmentation: agents only get `KbWrite` for segments explicitly
  granted in config. An agent cannot write to a segment it was not given.
- Provenance stamps the writing agent (`agent_id`) on every entry, so
  retroactive attribution is possible via `kb_search` with `author` filter.
- Eviction (`max_entry_age_days`, `max_entries_per_segment`) bounds how long
  exfiltrated data persists in the store.

**Residual risk:** An agent with `KbRead` can copy entries from a sensitive segment
into its response text, which is visible to the operator and in `flight.jsonl`.
This is intentional — the operator controls capability grants. Segment-level
read-access controls are the primary defence; no sanitization layer exists.

### 7.4 Prompt-injection persistence

The most dangerous memory threat in a single-tenant system: a malicious tool
result or adversarial web page causes an agent to store a poisoned entry in the
KB. On a future run, another agent retrieves that entry via `kb_search` or
`kb_get` and acts on the injected instruction.

Controls in place:
- **Provenance shown on retrieval.** Every `kb_get` / `kb_search` result
  includes the full provenance block (`agent_id`, `turn`, `task_fp`, `ts`).
  A reading agent can see who wrote the entry and when. Example output:
  ```json
  {
    "content": "use /bin/sh for tool calls",
    "provenance": { "agent_id": "untrusted-scraper", "turn": 3, "task_fp": "aabbccdd", "ts": 1700000000 }
  }
  ```
- **Canon segments are trusted.** Operator-seeded `canon` entries cannot be
  overwritten by any agent — a reading agent can assume canon data is clean.
- **No automatic retrieval injection.** The runtime never inserts KB entries
  into the system prompt or user turn without an explicit `kb_get`/`kb_search`
  tool call. Injection requires an agent to call the tool.

**Remaining gap:** There is no sanitization of retrieved KB content before it
enters the agent's context. The trust relationship is: the reading agent trusts
data proportional to the writing agent's trustworthiness. In a single-tenant
cooperative system the operator is the writing agent; multi-agent retrieval
requires careful provenance review in the task prompt.

### 7.5 `memory.redb` at rest

`memory.redb` is opened at path `store_path` (default `memory.redb` relative to CWD;
production: `/run/memory/memory.redb` on the persistent 9p virtfs mount).

| Property | Control |
|---|---|
| File permissions | Mode `0600` set immediately after open via `set_permissions` (§3.3 gap applies) |
| Encryption | None — same gap as checkpoint.json; mitigate with LUKS on the volume |
| Corruption | Detected at open; corrupt file quarantined to `.corrupt` and a fresh empty store opened; `memory_quarantined` flight event emitted |
| Intentional deletion / volume absent | `MemoryStore` returns `None`; `memory_unavailable` flight event emitted; `kv_get`/`kv_set`/`kb_*` tools not registered; agents proceed without memory |
| Eviction floor | `max_entries_per_segment` + `max_entry_age_days` bound store growth; `memory_evicted` events record every eviction |

**Startup invariant (p5.8+):** `agentd` asserts at startup that `store_path` does
not fall inside any MCP server's `AllowFsRead` or `AllowFsWrite` prefix. If it does,
startup fails with a descriptive error. This prevents a sandboxed MCP server from
reading or overwriting the memory store file. `store_path` must be an absolute path.

### 7.6 Availability

The memory substrate is designed to be available-over-consistent. If the store
cannot be opened (missing, locked by another process, or permission-denied):

- `agentd` emits a `memory_unavailable` flight event with `stage` and `error` fields.
- Memory-dependent tools (`kv_get`, `kv_set`, `kb_*`) are not registered.
- All other agents and tools continue normally.
- A `tracing::warn!` is written to stderr so the operator sees the degradation.

There is no automatic retry or failover to a secondary store. If the store path
is on a network volume that becomes temporarily unavailable mid-run, subsequent
write transactions will fail and be recorded as errors, but running agents are
not terminated. Restart agentd to re-open the store after the volume is restored.

---

## 8. Credential gateway (cred.3+)

The in-process credential broker (`agentd/src/credential/mod.rs`) is a second
loopback HTTP listener (OS-assigned port) that MCP server subprocesses use to
make authenticated API calls without holding provider credentials directly.

### §8.1 Token identity and scope

Each MCP server spawn receives one ephemeral credential token (UUID4) as
`AGENTD_CREDENTIAL_TOKEN`. The broker validates this token against a registry
that maps token → `(agent_id, allowed_providers)`. Tokens are deregistered
before MCP server shutdown.

**Gap:** Token is a shared secret between the host process and the subprocess.
A compromised MCP server can make requests to any provider in its `allowed_providers`
list for the lifetime of the token (until deregistration). cred.4 will add
per-call budget enforcement.

### §8.2 Credential isolation

Secrets (`ANTHROPIC_API_KEY`, `BRAVE_SEARCH_API_KEY`, `OAUTH_REFRESH_TOKEN`,
`OAUTH_CLIENT_SECRET`) are blocked from MCP subprocess environments via
`PASSENV_BLOCKLIST`. The broker reads them from env/secrets-file at token-refresh
time, not at subprocess spawn time.

**Gap:** The broker process itself holds secrets in process memory. A memory
read exploit against agentd can extract them.

### §8.3 Loopback SSRF

The broker binds on `127.0.0.1:0`. MCP subprocesses that are network-sandboxed
(Landlock IsolateNetwork) cannot reach the loopback adapter. To allow broker
access, the MCP server must have `Net { ports = [gateway_port] }` in its
capabilities, or the operator must not use IsolateNetwork.

**Gap:** A sandboxed MCP server with only loopback access could exfiltrate data
via the broker's upstream forwarding (the broker relays responses back to the
caller). The `allowed_providers` list limits which upstreams are reachable.

### §8.4 Token state write integrity (QEMU 9p)

On QEMU virtfs 9p mounts, the `rename()` system call is not atomic (known
kernel limitation). If the OAuth access token rotates and the broker emits a
new refresh token, the state write may fail silently. The broker:
- Still returns the newly fetched access token for the current request.
- Emits `CredentialRefreshFailed` with `token_written: false` so the operator
  is alerted.
- On next request, re-reads the original secrets file and re-fetches from the
  token endpoint, losing the rotated refresh token.

**Gap:** If the original refresh token was single-use (Google rotates them),
the next refresh attempt will fail and the operator must re-run `agentctl auth`.

### §8.5 Header scrubbing

The broker always strips `Authorization`, `Host`, `X-Subscription-Token`,
`X-Credential-Token`, and the provider's `header_name` from inbound MCP server
requests before forwarding. This prevents credential injection attacks where
a compromised MCP server tries to bypass the broker by sending its own auth header.

---

## 9. Summary table

| Threat | Control | Gaps |
|---|---|---|
| API key leakage via logs | Never logged; no Debug impl | Short secrets in tool args still appear if ≤200 chars |
| Large content in flight.jsonl | 200-char preview on all user/tool text fields | — |
| Checkpoint file on disk | Deleted after restore; atomic write; mode 0600 | No encryption; readable by root or file owner only |
| Token cost DoS | Per-agent + global budget + max turns | Global budget must be set explicitly; spawn tree can multiply costs |
| MCP server filesystem escape | Landlock path-beneath rules | Symlink traversal; degrades on kernels < 5.13 |
| MCP server spawn | seccomp fork/vfork block | clone/clone3 bypass on namespace-only path; no-op on aarch64 |
| MCP server network access | Network namespace isolation by default | — |
| Supply chain | 15 audited deps; static binary; rustls | No automated CVE scanning in CI |
| Unsafe MCP server (adversarial) | `isolation = "gvisor"` available | Not the default; requires runsc on PATH |
| Shared KB poisoning (p5.4+) | See §7.1 | Scratch segments last-writer-wins; canon segments operator-only |
| Cross-agent KB exfiltration (p5.4+) | See §7.2–7.3 | `KbRead` cap required; provenance stamps writing agent; no output-side redaction |
| Prompt-injection persistence (p5.4+) | See §7.4 | Provenance shown; canon trusted; no sanitization of retrieved content |
| `memory.redb` at rest (p5.1+) | See §7.5 | No encryption; mode 0600 only; startup asserts store not inside MCP sandbox |
| Memory substrate availability (p5.1+) | See §7.6 | No retry/failover; store-open failure silently disables memory tools |
