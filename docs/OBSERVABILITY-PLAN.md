# OBSERVABILITY-PLAN — OTEL + eBPF on top of the egress mediator

**Status:** design. Companion to `docs/PRODUCT-THESIS.md` (observability is ~half the
product) and `docs/plans/p7.5-egress-mediator.md` (you can't observe what you don't broker).

> The order matters: **brokering (p7.5) → observability.** The egress mediator is what makes
> model/tool calls *pass through* AgentOS so they can be observed at all. OTEL formats that
> stream; eBPF adds the syscall-level ground truth the flight recorder can't see.

---

## What already exists (the foundation — the hard part is done)

- **Flight recorder:** structured append-only JSONL, 44 event kinds, emitted at every
  meaningful step. Agent/turn structure, token/$ metering, provenance, secret redaction.
- **FUSE `/agents`** introspection + **`agentctl watch`** (dashboard, topology, memory,
  flight-log inspector). The event stream is already *span-shaped*.

What's missing is **format (OTEL)** and **depth (eBPF)** — not instrumentation.

## Part A — OpenTelemetry (close; ~1-2 increments)

**The mapping is natural** (flight events are already span-shaped):
- run = **trace**; each agent = a **span**; each turn = a child span; `inference_request/
  response`, `tool_call/result`, `egress_brokered` = child spans; the `parent_map` (topology)
  gives the trace tree.
- tokens / $ / latency / queue-depth = **metrics**; the other events = **span-events / logs**.
- Use the **GenAI semantic conventions** (`gen_ai.*` for model, prompt/completion tokens,
  etc.) so it lights up in Jaeger/Grafana/Honeycomb out of the box.

**Cleanest design — flight→OTLP sidecar [HARNESS], optional in-core exporter [CORE]:**
- Keep `flight.jsonl` as the tiny canonical in-core record (protects the ≤6 MB core; the
  `opentelemetry`/`opentelemetry-otlp` crates are heavy).
- Ship a **sidecar** (`agentos-otel`, in `agentos:full`) that tails `flight.jsonl` (and the
  egress/audit stream) and emits OTLP. The trace tree reconstructs from flight alone
  (`agent_id` + `turn` + `parent_map`). Zero core weight, no parallel data plane.
- *Optional later:* a cargo-feature-gated in-core OTLP exporter for first-party export, only
  if a deployment needs it and the size budget allows.
- **Trace-context propagation:** when AgentOS hosts a foreign workload (universal tier), inject
  W3C `traceparent` into the brokered calls at the egress mediator so the foreign agent's
  model/tool calls join the same trace — observability without the workload's cooperation.

**Increment:** `obs.1 — flight→OTLP sidecar + GenAI semconv mapping` [HARNESS], plus a thin
`obs.1-core` feature flag if first-party export is wanted. Acceptance: running the demo
produces a coherent OTLP trace (agent → turns → inference/tool spans) + token/$ metrics in a
standard backend, with the foreign-workload spans nested under the same trace.

## Part B — eBPF (substantial; its own phase, genuinely not started)

**Correction to the record:** the roadmap's p3.3 "eBPF/LSM" deliberately used **Landlock +
seccomp, not eBPF** (`docs/SPIKES/p3.3-ebpf-lsm.md`). There is no eBPF code to build on, and
eBPF-for-observability is a new direction.

**What it adds that nothing else can:** the flight recorder records what *agentd chose to
record*; the egress proxy sees brokered calls. Neither sees what a workload **actually did at
the kernel level**. Kernel-level tracing does — real syscalls, file access, network attempts,
CPU/latency — for **universal-tier foreign agents and TLS-pinning workloads** where the proxy
only has connection-level visibility. *Which* tracer (host eBPF, in-guest eBPF, or the gVisor
sink) depends on the isolation floor — see the constraint table above. It is the **observe** complement to the
sandbox's **enforce**, and the ground-truth check that the Landlock/seccomp policy held.

**This closes the two-tier gap:** native tier → full content observability (agentd is the
client); universal tier → connection-level metering (proxy) **+ syscall-level ground truth**.
Together that's a real audit trail even for a foreign framework that pins TLS.

### Critical constraint: the audit mechanism is coupled to the isolation floor

**Host eBPF is blind inside gVisor.** gVisor's Sentry handles the sandboxed app's syscalls in
*user space* — they never reach the host kernel, so host-attached eBPF probes never fire. So
"syscall-level ground truth" is delivered by a *different mechanism depending on the isolation
floor* (see `docs/PRODUCT-THESIS.md`):

| Isolation floor | Syscall-level observability mechanism |
|---|---|
| Native / capability-only (no separate kernel) | host eBPF on the child PID |
| **Firecracker microVM** (own guest kernel) | **eBPF inside the guest** (host eBPF can't see in) |
| **gVisor** (user-space kernel) | **gVisor remote sink** (`seccheck.Sink`: Sentry serializes syscalls to protobuf over a Unix domain socket; agentd runs a listener) — eBPF does NOT work |

This is the correction the deep-research pass surfaced: **picking the isolation floor and
picking the observability mechanism is one decision, not two.** Phase 9 below is therefore
conditional on the floor, not "eBPF everywhere."

**And it varies by arch + kernel, not just by floor.** seccomp/Landlock/eBPF availability differs
across x86_64 vs. aarch64 and across kernel versions (we already carry an aarch64 `DenySpawn`
no-op). On thin/stock ARM kernels (a Pi, an edge box) the floor degrades to the capability layer,
so both isolation *and* syscall-observability drop to a lower tier. Multi-arch reach therefore
requires **stating the active isolation/observability tier per device** (planned as `ma.4` in
`docs/DEPLOYMENT-TOPOLOGY.md`) — breadth must not silently outrun the trust/audit guarantees.

**The lift (why it's a phase, not an increment):**
- `aya` (pure-Rust eBPF, ethos fit) or libbpf; kernel **BTF/CO-RE** + `CONFIG_BPF*`; elevated
  privilege (`CAP_BPF`/`CAP_SYS_ADMIN` — a deliberate trust boundary: the observer outranks the
  agents, like a kernel watching processes); Linux-gated; binary/runtime weight (tension with
  super-light); kernel-version floor (CO-RE ≥ ~5.x). On the appliance you control the kernel,
  so this is tractable; "run anywhere" degrades.
- A new surface to expose probes: feed kernel events into the flight/OTEL stream as
  kernel-level span-events, and/or a `/agents/<id>/syscalls` read surface.

**Phase (≈3-5 increments), conditional on the isolation floor:** `ebpf.1` aya scaffold +
capability + kernel-config + a single syscall-trace probe per child PID (native/Firecracker-
guest path) → `ebpf.2` network/file probes → `ebpf.3` perf/latency → `ebpf.4` surface + OTEL
integration (kernel span-events) → `ebpf.5` policy-violation detection. **`sink.1` — gVisor
remote-sink listener:** a `seccheck.Sink` UDS listener that ingests + decodes the Sentry's
protobuf syscall stream, for workloads isolated with gVisor (where eBPF is blind). Build
whichever path matches the chosen floor first; don't assume eBPF-everywhere. Likely its own
roadmap phase ("Phase 9 — kernel observability").

## Core vs. Harness split

- **Core:** the flight recorder (canonical, tiny) stays in core; the eBPF subsystem is core (or
  a privileged sidecar) because it needs elevated privilege and ships with the appliance image;
  the trace-context injection lives at the egress mediator (core).
- **Harness:** the OTLP exporter sidecar (`agentos-otel`) ships in `agentos:full`. Keeps the
  heavy OTEL deps out of `agentd`.

## Honest non-goals

- OTEL does not add observability AgentOS lacks; it *exports* what's already recorded in a
  standard format. The value is interop (existing backends), not new signal.
- eBPF observes; it does not enforce (Landlock/seccomp enforce). It's the audit/ground-truth
  layer, including catching enforcement gaps.
- Neither closes the covert-channel exfil via allowed model/tool channels (see p7.5 non-goals).
  Observability makes it *accountable and detectable*, not impossible.

## Sequencing

1. `p7.5` egress mediator (prerequisite — brokering enables observation).
2. `obs.1` flight→OTLP sidecar + GenAI semconv (cheap, high interop value). ← your original ask.
3. `ebpf.1`+ kernel-observability phase (the syscall-level ground truth; closes the universal-
   tier audit gap). Bigger; schedule as its own phase.
