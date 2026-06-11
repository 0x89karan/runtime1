# AgentOS / agentd Threat Model

This document enumerates the security boundaries, threats, controls, and known
limitations of the `agentd` runtime as of Phase 4.3. It is the operator reference
for understanding what the sandbox stops, what it doesn't, and why.

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

---

## 2. Flight recorder (flight.jsonl)

`flight.jsonl` is an append-only JSONL file written to CWD. It is a structured
activity log — not a secret store — but it may contain excerpts from agent tasks,
tool inputs, and tool results.

### 2.1 What was logged (pre-p4.3, now fixed)

Before this increment, `ToolCall` events recorded the **full, untruncated** tool
input as `input`. A `write_file` call with a large file body, or any tool called
with an argument containing a secret, would land verbatim in the log.

Similarly, `AgentSpawned` events recorded the **full task string**, which could
contain sensitive context passed at invocation time.

### 2.2 What is logged now (post-p4.3)

All event fields that carry user-supplied or tool-supplied text are now truncated
to a 200-character preview:

| Event | Field | Before | After |
|---|---|---|---|
| `agent_spawned` | `task` (full) | verbatim | `task_preview` (≤200 chars + `…`) |
| `tool_call` | `input` (full JSON) | verbatim | `input_preview` (≤200 chars + `…`) |
| `tool_result` | `preview` | ≤200 chars ✓ | unchanged |
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

### 3.3 Known gap (tracked in TODOS.md)

`checkpoint.json` has no encryption and is written with the default umask. On a
shared filesystem this is a data-leakage risk. A future increment should either:
(a) set `O_CREAT | 0600` permissions on the checkpoint file, or
(b) encrypt the checkpoint at rest with a key derived from the agent's identity.

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

## 7. Summary table

| Threat | Control | Gaps |
|---|---|---|
| API key leakage via logs | Never logged; no Debug impl | Short secrets in tool args still appear if ≤200 chars |
| Large content in flight.jsonl | 200-char preview on all user/tool text fields | — |
| Checkpoint file on disk | Deleted after restore; atomic write | No encryption; world-readable without umask restriction |
| Token cost DoS | Per-agent + global budget + max turns | Global budget must be set explicitly; spawn tree can multiply costs |
| MCP server filesystem escape | Landlock path-beneath rules | Symlink traversal; degrades on kernels < 5.13 |
| MCP server spawn | seccomp fork/vfork block | clone/clone3 bypass on namespace-only path; no-op on aarch64 |
| MCP server network access | Network namespace isolation by default | — |
| Supply chain | 15 audited deps; static binary; rustls | No automated CVE scanning in CI |
| Unsafe MCP server (adversarial) | `isolation = "gvisor"` available | Not the default; requires runsc on PATH |
