# AgentOS / agentd Threat Model

This document enumerates the security boundaries, threats, controls, and known
limitations of the `agentd` runtime as of v0.62.0 (cred.3.2). It is the operator
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

**DNS rebinding limitation:** The startup SSRF check (`is_ssrf_blocked`) resolves
`upstream_base` once at boot and pins the result into the `reqwest::Client` via
`ClientBuilder::resolve()` (IP pinning, ar-04, v0.62.0). This prevents the common
DNS rebinding path: the resolved IP is locked for the lifetime of the gateway process.
A zero-TTL or short-TTL DNS entry injected *before* startup (not after) could still
reach an SSRF target, but that requires compromising the operator's DNS infrastructure
before the process starts. `upstream_base` is operator-configured (not MCP-controlled
input). Per-process-lifetime IP pinning is implemented; per-request re-resolution is
not and is not necessary given pinning is in place.

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

The broker forwards only an explicit allow-list of headers from inbound MCP server
requests (`content-type`, `accept`, `accept-language`, `cache-control`,
`x-goog-api-version`). All other headers are dropped before forwarding.
The broker always adds `Authorization` (or `X-Api-Key`) with the real credential,
plus `Content-Length` and `Host` set by reqwest.

Note: `x-goog-user-project` was removed from the allow-list in v0.61.0 (cred.3.1-adv,
F2). Forwarding it would allow a compromised MCP server to redirect API quota and
billing charges to an arbitrary GCP project.

**Gap (pre-cred.3.1):** Prior to v0.61.0, scrubbing was a deny-list of 7 specific
headers. Any header not in the list passed through, enabling header injection attacks.
Fixed in v0.61.0 (ar-08).

### §8.6 Universal-tier agents and credentials

Universal-tier agents (gVisor/runsc subprocesses) currently receive **no** credential
gateway access. They are spawned with an ephemeral `ANTHROPIC_API_KEY` (for inference
only, via `ProxyRegistry`) but do NOT receive `AGENTD_CREDENTIAL_TOKEN` or
`AGENTD_CREDENTIAL_GATEWAY_URL`. Universal-tier MCP servers requiring OAuth or API-key
credentials will receive a 503 from those servers. This is intentional: universal-tier
credential plumbing is deferred to cred.4 or cred.5.

### §8.7 Egress content audit (NOT IMPLEMENTED — ratified de-claim)

Tool output is **not** scanned for credential-shaped tokens before it reaches the flight
log or the agent context. This is an explicit, ratified limitation (S2/S3 de-claims,
cred.3.2):

- **No `SecretRewriter` struct.** Credential-shaped tokens that appear in upstream API
  responses are forwarded as-is to the calling MCP server and logged in `flight.jsonl`.
- **`EgressBrokered` event does not audit content.** The field `content_audited` was
  removed in v0.61.0 (it was hardcoded `true` with no actual scanning). The event now
  accurately records only that the egress was brokered; it makes no content-audit claim.
- **`EvidenceWriter` (p7.5) is the signing mechanism for the *egress proxy* path only.**
  The credential gateway uses `FlightRecorder` only; it does not write to `evidence.jsonl`
  and does not compute body hashes.
- **Tracking.** A real content audit (`SecretRewriter`) scanning tool output for
  credential-shaped tokens is tracked as cred.3-ar-S3 (P2) in TODOS.md. Operators who
  require this control should treat `flight.jsonl` as potentially containing live tokens
  and restrict access accordingly.

---

## 9. Management HTTP API reachability (ux.0b)

The management API (`agentd/src/management.rs`, `:7999` by default) exposes
scheduler snapshot, memory, approvals, credentials, spawn, and inject over
plain HTTP — **it is unauthenticated.** There is no bearer token, session
cookie, or Origin/Host check: any peer that can open a TCP connection to the
bound address can approve/deny pending actions, inject a turn into a running
agent, or spawn a new one.

### §9.1 Loopback-by-default guard

`agentd` refuses to start the management API on a non-loopback address unless
the deployment explicitly opts in:

```
ensure!(bound.ip().is_loopback() || cfg.allow_non_loopback, …)
```

The default config (`bind_addr = "127.0.0.1"`, `allow_non_loopback = false`)
is unaffected by this increment — the guard still refuses `0.0.0.0` for any
config that doesn't set the flag.

**Gap:** `allow_non_loopback` is an unscoped bypass, not a Docker-bridge-only
one — the guard is a plain `||`, so once the flag is true, the code itself
does not distinguish a Docker bridge address from a real LAN or public NIC.
The mitigation for that distinction lives entirely in operator judgment (only
pair the flag with `bind_addr`s that are actually container-internal), not in
the mechanism. Copying the exact `agentd/cos.agents.toml` pattern
(`bind_addr = "0.0.0.0"`, `allow_non_loopback = true`) onto a bare-metal host
or a cloud VM without Docker's NAT boundary exposes the full unauthenticated
control plane (spawn/inject/approve/deny/credentials) to whatever network
that interface actually reaches. Narrowing the guard (e.g. restricting the
opt-in to RFC 1918/link-local ranges) was considered out of scope for this
increment's Option A (gated override, smallest change); tracked as a
follow-up in TODOS.md.

### §9.2 The Docker-bridge exposure this increment accepts

Docker's `-p`/`ports:` host-port mapping cannot reach a process bound to
`127.0.0.1` *inside* the container — the mapping targets the container's
network namespace, not its loopback interface. To make `agentctl watch --url
http://localhost:7999` work from the Mac host against the `cos` container,
`agentd/cos.agents.toml` and `distro/overlay/etc/agentd/cos.agents.toml` now
set `bind_addr = "0.0.0.0"` + `allow_non_loopback = true` — an explicit,
per-deployment opt-in, not a change to agentd's default.

Binding `0.0.0.0` inside the container exposes the unauthenticated API to
**every peer on the same Compose network** the container is attached to, not
just the host loopback interface that `docker-compose.yml` publishes to
(`127.0.0.1:7999:7999` — never bare `7999:7999`).

**Finding (ux.0b ship-stage adversarial review) and fix, same PR:** an
earlier draft of this section claimed the bridge "has no other services on
it besides operator-controlled sidecars" — that was **false**.
`docker-compose.yml` originally had no `networks:` stanza, so Compose put
every service it defines — `cos`, `agent`, and (under the `semantic`
profile) `qdrant`/`semantic-kb-mcp` — on the same default bridge network.
`agent` is not an operator-controlled sidecar: it is the documented,
ordinary way to run an arbitrary template (`docker compose run --rm agent`,
`docs/DEPLOYMENT.md`), including templates with live `http_fetch`/`web_search`
capabilities that process untrusted web content. A prompt-injected or
otherwise misbehaving `agent` container could reach `cos:7999` on the bridge
with no host-network exposure required at all — just the project's own two
documented commands (`docker compose up cos` + `docker compose run --rm
agent`) run together, an ordinary dogfooding pattern, not an
attacker-controlled scenario. Three independent reviewers (Claude structured
+ adversarial, Codex adversarial, and an independent outside-voice pass) all
converged on this being reachable via the default quickstart with no
misconfiguration required, which raised it above the bar for "defer to a
follow-up" — **`docker-compose.yml` now defines separate `cos-net` /
`agent-net` networks**: `cos` is alone on `cos-net`; `agent`, `qdrant`, and
`semantic-kb-mcp` share `agent-net`. `agent` can no longer reach `cos:7999`
on the Compose bridge. Verified via `docker compose config` showing each
service's resolved network membership.

**Remaining gap:** an operator who attaches an untrusted or third-party
container directly to `cos-net` still gives that container the same
unauthenticated control (spawn, inject, approve, deny) — that is a
deployment-hygiene requirement this fix does not (and cannot) enforce.
`allow_non_loopback` also remains an unscoped bypass rather than one limited
to Docker-internal ranges specifically — tracked as `ux.0b-ar-02` (P3) in
TODOS.md, a design decision (IP-range scoping) deliberately left out of this
increment's Option-A ("smallest change") scope.

The QEMU deployment has always set `bind_addr = "0.0.0.0"` (so `hostfwd` can
reach it) but — until this increment — the guard silently refused to start
the management API there at all; `allow_non_loopback = true` in the overlay
config fixes that pre-existing conflict rather than expanding QEMU's
exposure (QEMU's hostfwd already scopes `:7999` to the host's loopback).

### §9.3 Follow-up: per-session auth (deferred to ux.5)

Auth (a per-session bearer token) is the only option that actually closes the
Docker-bridge exposure in §9.2. It was considered and deferred (Option C in
`docs/plans/ux.0b-host-loopback-reachability.md`) because the cockpit's only
*first-party* consumer today is `agentctl` on a Mac/Linux host the operator
controls.

**Correction (found during adversarial review — this browser risk is already
live, not a future one):** the moment `docker-compose.yml` publishes
`127.0.0.1:7999:7999`, *any* webpage open in the operator's own browser on
that host — not just a future ux.5 cockpit page — can already reach
`http://localhost:7999` the same way any localhost dev server is reachable
from browser JavaScript. The management API has no Host/Origin check and no
CORS policy, and several routes (e.g. `POST /api/v1/spawn`) accept a JSON
body; a request sent with `Content-Type: text/plain` is a CORS-simple
request that the browser will send without a preflight, so the server-side
handler executes even though the page can't read the response — a classic
localhost CSRF pattern. This is a real, present-day gap this increment
introduces (previously the port wasn't published to the host at all), not
merely a future one. Ranked P1 for the ux.5 build (that increment must not
ship a browser-facing cockpit without closing this), but not blocking ux.0b
itself: the plan's Option-A decision explicitly accepted the "no auth yet"
tradeoff for the `agentctl`-only consumer model, and adding auth now is a
scope expansion the /plan-eng-review gate didn't approve. **Revisit at
ux.5** — that increment adds a *new*, browser-native consumer and so is the
right place to add both a bearer token and an Origin/Host allowlist; it is
not the point at which this CSRF exposure first appears.

### §9.4 The management API is now on by default for the primary entrypoint (ux.9)

`docker/cockpit.toml` sets `[management] enabled = true`, and the Dockerfile's
`CMD` now boots that config unconditionally for a bare `docker run` with no
arguments — the zero-arg default entrypoint, not just the opt-in `orchestrate`
mode or the network-segmented `cos`/`agent` compose services from §9.2. The
bind address and loopback guard are unchanged (`bind_addr = "127.0.0.1"` by
default, per §9.1 — `docker/cockpit.toml` does not override it), so this is
**not a new vulnerability class**, but the likelihood of an operator having
the unauthenticated control plane live without realizing it is materially
higher now that it is the default rather than something they opted into.
`agentctl watch` (the cockpit's own client) runs inside the *same* container,
connecting over loopback with no Docker NAT boundary to cross — the §9.2
Docker-bridge-publish exposure only applies if an operator additionally
publishes `7999` to the host (cockpit mode does not do this by default; no
`docker-compose.yml` service exists for it). Flagged by the `/review`
security specialist (2026-07-12); this section is the fix — a documentation
gap, not a code change.

---

## 9.5 Injected-trigger lateral movement & brief integrity (cap.2b)

The CoS pipeline ingests attacker-authored content by design: the daily brief is built from the
operator's Gmail, so a crafted email is a normal input, not an edge case. Single-tenant does not
make prompt injection irrelevant — it changes the *victim model* (the attacker is anyone who can
get text into the inbox), so reducing the authority co-located with untrusted-data ingestion is
least-privilege between agents (in scope), not multi-tenant isolation (out of scope).

**Closed (cap.2b — the machine-credential path).** The node exposed to the schedule (the CoS
orchestrator) is de-privileged to a summary-free cron TRIGGER holding only `{Mcp{cron_trigger},
RunJob}`. Gmail lives on the `cos-inbox` sealed job; brief authoring on the KB-only `cos-curator`
job. Sealed-job caps + task templates are owned by config, not the (injectable) trigger, and the
trigger receives only an agentd-authored completion signal — never a job's output (`AwaitingParent.
deliver_content=false`). So an injected trigger can neither mint caps, nor supply task text, nor
read email-derived content back into a spawn-capable context. **No child obtains live Gmail via
injection, and no untrusted-data-reading node holds spawn or credential authority.**

**Residual (NOT closed — capability envelopes bound actions, not intent).** The curator reads the
email-derived summary and authors the operator-facing brief. An injected curator can therefore write
a *misleading* brief (e.g. "URGENT: wire funds to X") — a social-engineering channel that no
capability system closes, because writing the brief is the curator's legitimate job. Its blast radius
is bounded to *integrity/manipulation* (a bad brief, poisoned KB), NOT credential exfiltration or
RCE: Gmail is read-only (no send scope), `oauth_call_api` is provider-host-pinned + SSRF/IP-hardened
(§8.3), and single-tenant means a stolen token has no lateral value. Mitigation is detective, not
preventive: the flight recorder, signed egress receipts (§8), and the operator's own judgment reading
a brief they know is machine-assembled. **North star (not built):** data-taint — a node that has read
untrusted data may not exercise irreversible authority — which would generalize this to future ingest
flows (Telegram, webhooks) without hand-decomposing each pipeline.

---

## 9.6 Telegram reach: remote approve/deny writer (ux.12)

ux.12 adds a two-way Telegram sidecar (`docker/telegram_mcp.py`, a no-tools stdio MCP server
inside the `cos` container) that delivers the brief, pushes pending approvals, and relays
allowlisted approve/deny replies to `POST /api/v1/approvals/*/{approve,deny}`. This turns an
**untrusted-input bridge** (arbitrary Telegram messages) into a writer on the approval control
plane, so the controls below are load-bearing, not cosmetic.

**Route-scoped approval secret (the actual boundary).** The management API is otherwise
unauthenticated (§9.3) and approval ids are sequential/guessable (`act_{seq}`), so a chat-ID
allowlist alone protects nothing at the API — any peer that reaches `:7999` could approve by
enumeration. ux.12 therefore gates the approve/deny routes with a shared secret: when
`AGENTOS_APPROVAL_SECRET` is set, those routes require a constant-time-matched `X-Approval-Token`
header (`management.rs` `approval_token_ok`). The sidecar and `agentctl` both send it; the secret
is env-only (secrets invariant) and reaches only the sidecar via its own `passenv` (it is NOT in
`PASSENV_BLOCKLIST` because the sidecar needs it; the passenv opt-in model keeps it off every
other MCP server). This is route-scoped; full API auth remains ux.5. When the secret is unset the
routes stay open (pre-ux.12 behavior) — set it whenever Telegram is enabled.

**Pre-existing exposure this makes concrete (§9.2).** Because `cos` binds `0.0.0.0`
(`allow_non_loopback = true`, for Docker hostfwd), `semantic-kb-mcp` on `cos-net` was *already*
an unauthenticated, approve-capable peer of `:7999` before ux.12. The approval secret closes the
approve/deny routes for all such peers, not just Telegram; the broader `0.0.0.0` bind is unchanged.

**Chat-ID authZ + token secrecy.** Only `message` updates from `from.id == TELEGRAM_CHAT_ID` in a
`private` chat are honored (a group would leak approval/args content). `from.id` is unforgeable via
the Bot API, so the bot token is the crown jewel: env-only, never logged. Token compromise =
read approval `args_json` + brief text + bridge DoS, but NOT approval injection (cannot forge
`from.id`, and cannot mint an `X-Approval-Token` without the separate approval secret).

**Relay-only + re-verify.** The sidecar never synthesizes an approval: before POSTing it re-GETs
`/api/v1/approvals`, confirms the id is still pending, and confirms its `args_json` hashes to what
was delivered to Telegram — refusing otherwise. This closes a cross-generation id collision (a
deleted `checkpoint.json` resets `approval_seq`, so a redelivered "approve act_3" could hit a
different freshly-minted `act_3`) and enforces "the human approved what they actually saw".

**New egress sink (§8.7).** Approval `args_json` and brief text (email-derived) are sent to
`api.telegram.org`, a third party, and egress content is not audited. The sidecar sends a
length-capped args preview (the first 500 chars, not the full payload) with a "view full in TUI"
pointer, but this is a real confidentiality sink the operator opts into by enabling Telegram. `Net{hosts}`
is advisory (only `ports` is kernel-enforced, sandbox); the sidecar's `[443, 7999]` grant means a
compromised sidecar could reach any host on 443 plus loopback:7999 — the approval secret and
chat-ID allowlist are the compensating controls.

**Degrade-safe.** `request_approval` parks based on `[management].enabled`, independent of the
sidecar, so a sidecar/Telegram outage never auto-approves or blocks — approvals stay pending in the
TUI (canonical); undelivered digests are dropped (durable in runs.redb). The sidecar fails closed on
its own POST errors (never marks an approval resolved on a network failure).

---

## 10. Summary table

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
| Credential SSRF (cred.3+) | See §8.3 | DNS-checked at startup + IP pinned for process lifetime (v0.62.0); link-local/private/IMDS ranges blocked |
| Credential header injection (cred.3+) | See §8.5 | Allow-list enforced (v0.61.0+); x-goog-user-project blocked (billing injection) |
| Universal-tier credential access | See §8.6 | Not implemented; deferred to cred.4/5 |
| Egress content scan | See §8.7 | NOT IMPLEMENTED; no credential-shaped token scanning in tool output |
| Management API unauthenticated access (ux.0b+) | See §9 | Loopback guard defaults on; `cos`/`agent` network-segmented (ux.0b-ar-01, fixed); `allow_non_loopback` opt-in still unscoped rather than Docker-bridge-limited (ux.0b-ar-02, open); no auth until ux.5 |
| Injected trigger grants live Gmail to a child (cap.2b) | See §9.5 | CLOSED: de-privileged cron trigger + sealed `run_job` (config-owned caps/task, completion-only delivery); no injection path to live credentials |
| Injected curator writes a misleading brief (cap.2b) | See §9.5 | OPEN by design: brief authoring is the curator's job; social-engineering channel, detective controls only (flight log, receipts, operator judgment); north star = data-taint |
