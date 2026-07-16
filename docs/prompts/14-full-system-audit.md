# AgentOS End-to-End Audit — Technical Direction, Security, Completeness & Future Direction

## Framing

You are conducting a full audit of **AgentOS**, a Linux-based OS where agents are
the primitive instead of applications, and **agentd** is its runtime. Read
`CLAUDE.md`, `docs/DESIGN.md`, `docs/ROADMAP.md`, `docs/STATUS.md`,
`docs/CONVENTIONS.md`, and `THREAT_MODEL.md` first — they are the constitution,
the work queue, the ship log, the style guide, and the security model,
respectively. Treat the "Locked decisions" in CLAUDE.md (cognition is remote,
single-tenant/mutually-trusting agents) as non-negotiable constraints to check
*compliance against* — but see the "Future directions" section below for where
even a locked decision is worth surfacing as an explicit, deliberate choice
rather than silently assumed forever.

This audit has **three distinct jobs**, and they should not blur together:

1. **Tactical**: find bugs, drift, and gaps the test suite and roadmap checklist
   don't catch — architectural inconsistency, security holes, half-finished
   tracks, stale docs.
2. **Structural**: assess whether the system's core abstractions (capability
   model, MCP-as-tool-ABI, sandbox tiers, credential broker, KB layering) are
   actually sound as *load-bearing* primitives, not just "currently passing
   tests." A structural finding is bigger than a bug — it says "this
   abstraction will keep producing bugs like the ones we just found until the
   abstraction itself changes."
3. **Strategic**: given everything above, where should the *product* go next —
   not just which roadmap increment is next, but what's the shape of AgentOS in
   twelve months, what's the actual differentiated bet, and what's a
   distraction. This is explicitly in scope — see "Future directions" below —
   and should be written for a reader making product decisions, not just
   engineering ones.

Do not let (1) crowd out (2) and (3). A list of ten small bugs is easy to
produce and easy to over-index on; the structural and strategic sections are
where this audit earns its keep.

## Scope

- `agentd/` — the runtime (capability system, credential broker, inference
  gateway, tool registry, MCP client, scheduler, flight recorder, management API)
- `agentctl/` — operator CLI (watch TUI, orchestrate REPL, spawn/inject/approve)
- `surfaces/` — FUSE filesystem exposing agent state
- `sandbox/` — kernel-level isolation for MCP subprocesses (gVisor/runsc tiers)
- `distro/` — Buildroot + QEMU boot path (the "real" target environment)
- `docker/` — Mac/Docker dev and CoS deployment path (entrypoint.sh, compose)
- `templates/` — agent template catalogue
- `docs/` — DESIGN, ROADMAP, STATUS, CONVENTIONS, THREAT_MODEL, SPIKES, plans/
- `TODOS.md`, `CHANGELOG.md` — open technical debt and shipped history
- `14-full-system-audit-findings.md` (same directory as this file) — a short
  list of tactical bug patterns confirmed during one recent live debugging
  session. Read it as a *seed* for dimensions 3 and 4 below, not as the audit's
  agenda — it's deliberately kept out of this file so it doesn't crowd out the
  structural and strategic work below.

## Method

1. Read the constitution docs listed above in full before touching code.
2. Cross-reference `docs/ROADMAP.md` against `git log` and `docs/STATUS.md` —
   for every phase marked done, spot-check the actual code, not just the
   checkbox. Flag anything checked off that looks partially delivered.
3. Read `TODOS.md` in full — every open item is a known gap. Don't
   rediscover these from scratch; instead assess whether they're still
   accurately scoped and whether any have gotten more urgent.
4. Grep for `unwrap()`, `expect(`, `unsafe`, `TODO`, `FIXME`, `XXX` across
   `agentd/src` and `sandbox/src` — the "loop never panics on bad input"
   invariant in CLAUDE.md is a hard constraint; verify it holds outside tests.
5. Diff `agentd/cos.agents.toml` against `distro/overlay/etc/agentd/cos.agents.toml`,
   and diff `docker/entrypoint.sh`'s sed-rewrite rules against every literal
   they're supposed to keep in sync. Look broadly for the general pattern this
   is one instance of: hand-duplicated literals across files with no
   compile-time or test-time check that they agree.
6. Run `cargo clippy -- -D warnings` and `cargo test` from `agentd/`, plus
   `make clippy-linux` and `make clippy-aarch64` if Docker is available — CLAUDE.md
   requires these before every commit; confirm CI actually gates on all three,
   not just the default-target clippy pass.
7. Read `git log --oneline -100` and skim `docs/STATUS.md` end to end to build
   a real sense of *velocity and pattern* — which tracks move fast, which stall,
   which get revisited repeatedly (a sign of an unstable abstraction). This
   context matters more for the strategic section than any single commit.

## Audit Dimensions (tactical + structural)

### 1. Architectural integrity vs. locked decisions
Does every subsystem actually respect "cognition is remote, no local weights"
and "single-tenant, mutually-trusting, in-process agents"? Look specifically for
any code that assumes multi-tenant isolation semantics it doesn't actually
enforce (e.g., capability scoping described as "least-privilege between agents"
in CLAUDE.md — is that a real boundary or just documentation?).

### 2. Security & threat model
- Read `THREAT_MODEL.md` and check every stated mitigation against the current
  code, not the code as it was when the threat model was written.
- Management API (`:7999`) is documented as unauthenticated-by-design,
  loopback-only. Verify this is enforced everywhere it's bound (Docker compose
  port mapping, distro/QEMU hostfwd, agentctl `--url` override) and that no
  code path accidentally binds it to `0.0.0.0`.
- Credential broker (`cred.1`–`cred.7`): audit the full chain from OAuth token
  acquisition → `CredentialRegistry` → `CredentialGateway` → MCP sidecar egress.
  Confirm raw secrets never cross into agent-visible state (flight logs, KB,
  FUSE surface) — cred.5-ar-01's secret-redaction fix suggests this leaked once
  already; check for siblings of that bug.
- Sandbox isolation tiers (`sandbox/src/lib.rs`, ma.4's `classify_tier()`): audit
  whether the tier classification is *complete* — ma.4 found a HIGH-severity
  misclassification bug (runsc-only reported as "none"). Are there other gaps
  between what a tier *claims* to isolate and what it *actually* isolates?
- MCP subprocess sandboxing (h7.1's `PASSENV_BLOCKLIST`, LD_PRELOAD stripping,
  SSRF blocking): is this enforced for every MCP server type, including
  Streamable HTTP/SSE (p7.1) and OAuth sidecars (h7.2), or only the original
  stdio Python servers?
- Supply chain: run `cargo audit` (the `audit` CI job) and check for any
  suppressed/ignored advisories. Check Docker base image pinning.
- Two-tier capability confusion: the *MCP server's own* `capabilities` field is
  checked independently from the *owning agent's* capabilities list. Audit
  every capability-consuming code path for the same ambiguity — is it possible
  to grant something at the wrong tier and have it silently do nothing, rather
  than fail loudly?
- Given this system is designed to hold real personal credentials (Gmail OAuth
  today, more providers implied by the roadmap) for a single individual: is
  there a credible path to third-party security review before this is used by
  anyone beyond the project's own operator? What would that review need to see?

### 3. Capability & credential system soundness (structural)
This is the system's core security primitive (`agentd/src/capability.rs`).
Treat repeated bugs in this area as a signal about the *abstraction*, not just
about individual call sites:
- Is `satisfies()`'s "absolute paths assumed, relative fails closed" behavior
  documented at every capability-declaration site, or only in a doc comment?
  Should the type system make relative capability grants unrepresentable
  instead of a runtime footgun?
- Are there compile-time or test-time checks that a capability declared in one
  file (e.g., a TOML task prompt) matches the enforced grant, or is this only
  discoverable by live flight-log inspection? Recommend a structural fix — a
  test matrix that generates the *actual runtime config* (including any
  entrypoint-script rewrites) and asserts self-consistency, rather than only
  testing the source file in isolation.
- Step back and ask: is a two-tier (server-level + agent-level) capability
  model with silent-deny-by-default the right design, or does the repeated
  confusion between the tiers suggest the model itself needs to change (e.g.,
  a single merged capability set per agent invocation, computed once and
  logged, rather than two lists checked independently at different points)?

### 4. Test coverage & CI reality-check
1327+ tests is a number, not a guarantee. Specifically probe:
- Is there any test coverage for `docker/entrypoint.sh`'s sed-rewrite pipeline
  itself? Shell scripts are usually the least-tested part of a Rust-heavy repo
  — quantify that gap and recommend a testing strategy for it (even a simple
  "generate + assert" harness beats nothing).
- How much of the credential broker, OAuth flow, and MCP sidecar code is
  covered by integration tests vs. unit tests with mocked boundaries? Mocked
  boundaries are exactly where cross-file config bugs hide.
- Does CI ever actually boot the Docker `cos` image and exercise a live cycle,
  or does it stop at `cargo test` + `cargo clippy`? If not, that's a structural
  gap — recommend a CI job (even a cheap one) that boots the image and asserts
  on `flight.jsonl` shape, not just that the binary compiles.

### 5. Deployment & runtime parity (Docker vs. QEMU/distro)
Two parallel deployment paths (`docker/` and `distro/overlay/`) exist and must
be hand-kept in sync. Assess whether this dual-maintenance burden is
sustainable as the system grows, or whether one path should become the single
source of truth with the other generated/derived from it. This feeds directly
into the "distro vs. Docker priority" strategic question below — don't treat it
as purely a maintenance-cost question, treat it as a distribution-strategy one.

### 6. Roadmap completeness — phase-by-phase gap audit
Cross-reference `docs/ROADMAP.md` checkboxes against `docs/STATUS.md` and
`TODOS.md`. Specifically flag:
- **Phase 10** (per project history: "half-delivered — flagship never migrated
  to broker") — confirm current status; resolved by cred.6, or still open?
- Every open `-ar-NN` (adversarial-review) item across TODOS.md — are any of
  these now higher-priority than their assigned P-level suggests?
- `cos-dev-02` (Curator inherits unneeded Gmail access — spawn capability
  inheritance is all-or-nothing) — a real least-privilege gap; assess severity
  now that the credential broker is fully live.
- Personal track (`personal.1` — gbrain as Layer 3 operator KB) — is this still
  the intended next step after h8.1, or has priority shifted?

### 7. Skills subsystem (Phase 11) — deep dive
Per the project's design notes, skills are meant to be "the missing procedural
layer," governed by the sandbox, using something like Anthropic's SKILL.md
format. This track is planned but **not yet built**. Audit:
- Does the *current* architecture (capability system, MCP-as-tool-ABI, sandbox
  isolation tiers, template catalogue) actually have a clean extension point
  for skills, or will skills require retrofitting any of those subsystems?
  Be concrete: trace exactly where a skill invocation would enter the system
  (agent step loop? a new native tool? a new MCP server type?) and what
  capability it would need to declare.
- Tension check: skills-as-procedural-knowledge vs. this system's "cognition is
  remote, no local inference" constitution — does a skill format designed for
  a different runtime (Claude Code, an interactive single-user coding
  assistant) translate cleanly to an always-on, multi-agent, capability-scoped
  runtime, or does it need real adaptation? Name specific incompatibilities —
  e.g., Claude Code skills assume interactive tool-permission prompts;
  AgentOS assumes pre-declared static capability grants — how would a skill
  request *new* capabilities at runtime, if at all? Does that require an
  approval-flow integration (`request_approval`) that doesn't exist for skills
  today?
- Discoverability and mental model: should skills live in `templates/`
  alongside existing agent templates, or as a new top-level catalogue? See the
  strategic question below on whether templates and skills are converging into
  one primitive.
- Security: a skill is essentially injected procedural instructions — what
  stops a malicious or buggy skill from being a privilege-escalation vector,
  given the single-tenant/mutually-trusting model? Is "mutually trusting
  between agents" still safe once skills can be authored by third parties and
  shared, or does that assumption break the moment skills become shareable?
- Deliver a concrete gap list: what's designed, what's decided, what's
  genuinely unknown, and a suggested build order (increments, in the roadmap's
  style) to close Phase 11 out. This should be usable as direct input to
  `/plan-eng-review` for the first skills increment.

### 8. Documentation drift
For each of `docs/DESIGN.md`, `docs/ROADMAP.md`, `docs/CONVENTIONS.md`,
`THREAT_MODEL.md`, and `README.md`: find at least one concrete instance (if any
exist) where the doc describes behavior the code no longer has, or omits
behavior the code has gained. Cite file:line pairs, not vague impressions.

### 9. Operational maturity
- Observability (`obs.1`–`obs.3`, OTel sidecar): is span/token accounting
  actually wired into every inference call path, including the newer streaming
  and multi-agent orchestration paths (orch.1/orch.2)?
- Cost/budget guardrails: CLAUDE.md states "cognition is metered... always
  accounted and bounded." Audit whether every code path that calls the
  inference gateway actually passes through budget enforcement, or whether any
  newer path bypasses it (e.g., check whether an orchestrator's configured
  `token_budget` is a real enforced cap or effectively unbounded in practice).
- Multi-arch (`ma.1`–`ma.4`) and version/publish pipeline: does the
  tag-to-publish pipeline have any integrity check that the tagged commit
  actually matches what gets published, or could a tag silently point at a
  stale commit? Recommend a guard if none exists.

### 10. Ecosystem & extensibility positioning (structural + strategic)
- MCP is the tool ABI by design. Is AgentOS positioned to consume the *broader*
  MCP ecosystem (third-party MCP servers, not just the project's own
  hand-written ones) with low friction, or does the capability/sandbox model
  impose enough integration overhead per server that it discourages adoption
  of the wider ecosystem? Walk through what it would actually take to add a
  brand-new third-party MCP server today, end to end, and assess the friction
  honestly.
- Is the embedding/semantic-search provider (OpenAI, per h8.1/memory-routing)
  behind the same kind of pluggable abstraction as chat inference
  (`InferenceGateway`), or is it a hardcoded dependency? If hardcoded, that's
  an inconsistency with the project's own "cognition is remote but not
  vendor-locked" spirit — worth naming explicitly even though embeddings
  weren't covered by the original "cognition is remote" locked decision.

### 11. Reliability & operations at always-on, 24/7 scale
CoS-style agents are meant to run indefinitely on cron triggers, not as
one-shot tasks. Audit whether the system is actually built for that:
- Checkpoint/crash recovery: if `agentd` crashes or the host restarts mid-cycle,
  does an always-on agent resume cleanly, or does state get lost/duplicated?
- Rolling upgrades: can `agentd` be updated to a new version without losing an
  in-flight agent's state, or does every version bump require a fresh
  container/boot?
- Multi-day/multi-week stability: any evidence of memory growth, KB bloat,
  flight-log disk growth, or token-budget exhaustion over long-running
  operation? Is there a retention/eviction story for `flight.jsonl` itself
  (it's append-only — does it grow unbounded)?

## Future directions for the product (strategic — options, not decisions)

This section is explicitly requested: don't just find gaps in the current
plan, propose where the *product* could go. Frame each as an option with
tradeoffs, not a recommendation to silently implement — anything that would
touch a **locked decision** (remote-only cognition, single-tenant) must be
flagged as requiring deliberate operator sign-off to relitigate, never treated
as a default. For each direction, give: what it is, why it might matter, what
it would cost to build, what it's in tension with, and a rough sizing (small
increment vs. multi-phase bet).

1. **Skills as a shareable ecosystem, not just a local procedural layer.**
   Once Phase 11 lands, the natural next question is whether skills stay
   private-per-install or become shareable/composable across installs — a
   "skills marketplace" direction. This raises real questions this audit
   should surface, not answer: trust and signing model for third-party skills,
   versioning, and whether "mutually trusting agents" still holds once a skill
   author isn't the operator. Tension: directly interacts with the
   single-tenant locked decision's spirit (trust boundary was designed around
   *agents*, not *skill authors*).
2. **Templates and skills convergence.** `templates/` (agent personas) and the
   planned skills catalogue (procedural knowledge) may be converging into one
   underlying primitive with two names. Worth a deliberate design decision
   before Phase 11 ships, rather than discovering the overlap after the fact.
3. **Cognition-provider neutrality extended past chat inference.** The
   `InferenceGateway` trait abstracts chat models; the embedding/semantic-KB
   path (h8.1+) does not appear to have the same treatment. If "no vendor
   lock-in for cognition" is a real product value, extend it consistently or
   explicitly scope it to chat-only and say so in `docs/DESIGN.md`.
4. **Notification/approval UX beyond the TUI.** `agentctl watch` is a terminal
   app; `request_approval` exists but requires an operator watching a
   terminal. Given CoS already touches Gmail, a natural extension is
   proactive notification through channels the system already has credentials
   for (email, and eventually push/Slack) rather than requiring a
   continuously-open TUI session. This is a UX/distribution bet, not a small
   feature — assess whether it changes the "device is a thin agent host"
   framing (does the *notification* need to live off-device, e.g., a
   companion mobile surface, or can it stay strictly local?).
5. **Cost/budget transparency as a first-class surface**, not just an internal
   guard. `ux.8` (budgets) is already queued per the roadmap — assess whether
   it should be elevated given that an always-on agent spending real API
   dollars with no visible running-cost dashboard is a trust problem for any
   operator who isn't also the engineer who built the system.
6. **Distro (Buildroot/QEMU, PID-1 boot) vs. Docker-first distribution
   priority.** The bare-metal PID-1 vision is the project's original thesis
   (`docs/DESIGN.md`), but the CoS flagship has been built, tested, and
   iterated almost entirely through Docker this cycle. Is bare-metal boot
   still the differentiated long-term bet worth continued investment, or has
   Docker-first proven itself as the more practical near-term distribution
   channel — and if so, should the roadmap's relative investment shift
   explicitly, rather than let it happen by default because Docker is where
   the debugging pressure naturally goes?
7. **Formal security review as a distribution gate.** Once this holds real
   personal credentials for anyone beyond the project's own operator, what's
   the credible path to a third-party security review or a documented
   hardening milestone before wider distribution? Treat this as a real
   go/no-go gate to name explicitly, not an implicit someday.
8. **Multi-device operation for a single individual.** The device is
   documented as "a thin agent host" (singular). As this becomes a daily
   driver, does one operator's AgentOS identity need to span multiple physical
   devices (e.g., a home server plus a phone/laptop surface) with continuity
   of agent state? This is explicitly in tension with the current
   single-device framing — surface it as a question worth a deliberate answer,
   not something to silently build toward or silently rule out.

## Deliverable format

Produce a plan document in the style of `docs/plans/*.md` (see
`docs/plans/cred.3.1-hardening.md` or `docs/plans/h8.1-layer2-semantic-memory.md`
for the house style: numbered decisions, explicit build scope, test plan).
Structure the output as:

1. **Executive summary** — 5-10 bullets, most important findings first,
   security findings flagged separately from architecture/completeness findings.
2. **Findings table** — one row per tactical/structural finding: severity
   (P0-P3), area (from dimensions 1-11 above), file:line evidence, one-line fix
   or investigation needed.
3. **Skills subsystem gap analysis** — its own section per dimension 7 above,
   detailed enough to hand directly to `/plan-eng-review` as the seed for the
   first Phase 11 increment.
4. **Recommended build order** — a roadmap-style increment list (matching
   `docs/ROADMAP.md`'s existing format: id, dependencies, acceptance criteria)
   for closing the highest-priority tactical/structural gaps, with skills work
   sequenced explicitly relative to other open tracks (cos-dev, personal.1,
   orch.3+, ma.5+).
5. **Future directions** — one write-up per option in that section above:
   what it is, cost/tension/sizing, and — for anything touching a locked
   decision — an explicit call-out that this requires deliberate operator
   sign-off, framed as a question to decide, not a plan to execute.
6. **Open questions for the operator** — anything else that's a genuine
   judgment call (not a bug, not a strategic bet), framed as a decision to
   make, not a task to do.
