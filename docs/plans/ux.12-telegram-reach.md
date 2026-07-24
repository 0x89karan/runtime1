# ux.12 — Telegram reach (two-way sidecar: digest + approve/deny)

**Increment:** ux.12 (UX cockpit reshape, increment 3 of 4 — pulled ahead of ux.13 per the
autoplan sequencing decision: reach-before-verbs serves the absent-operator thesis).
**Branch:** `ux.12-telegram-reach`
**Design of record:** `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md`
(APPROVED — premises P1–P5 AGREED; remote-inject already challenged-and-dropped by Codex).
Reshape plan-of-record `docs/plans/ux.11-trust-after-absence.md`.

## Problem

The lived friction is a **blocked approval while you're away**: the always-on CoS parks on a
`request_approval` (e.g. an egress send), and today the only way to unblock is the TUI at your
machine. ux.12 closes the loop from your phone: deliver the morning brief + push pending
approvals to Telegram, and accept **approve/deny** replies. No inject (the lived friction was a
blocked approval, not phone-steering — cut on cross-model challenge). Degrades safe: sidecar or
Telegram down ⇒ nothing blocks, approvals stay pending in the TUI/chat rail exactly as today,
undelivered digests are dropped (the TUI brief is canonical).

## Architecture (D1 — CORRECTED by Eng dual-voice)

**The earlier "separate service can't reach :7999" rationale was FACTUALLY WRONG.** The CoS
config binds `bind_addr = "0.0.0.0"` + `allow_non_loopback = true` (cos.agents.toml:112), so
`:7999` is on cos's *bridge* interface, and `semantic-kb-mcp` (on `cos-net`) already reaches
`cos:7999` today. Reachability is therefore NOT the reason to choose the sidecar shape.

**Decision D1 — the sidecar is a no-tools stdio MCP server spawned by agentd** (cron/oauth model,
`[[tools.mcp_servers]]`), chosen because it **inherits the security + build plumbing** an
entrypoint-launched process would not: the caps→Landlock port allowlist (sandbox), `PASSENV`
handling (mcp.rs:42), and the CI `sidecar-tests` contract. Eng verified the shell is sound: an
empty `tools/list` registers cleanly (mcp.rs:528), MCP servers are spawned eagerly + unconditionally
at startup (main.rs:430/557 — a bridge with no agent `Mcp` grant still spawns and stays alive),
`kill_on_drop` holds it for the scheduler's lifetime. **Rider:** start the background bridge thread
only AFTER the `initialize`/`tools/list` handshake responds (30s `MCP_TIMEOUT`, mcp.rs:23) — never
block the handshake on a Telegram `getUpdates`. The thread (webhook_mcp.py pattern) long-polls
Telegram `getUpdates`, **polls `GET /api/v1/approvals`+`/api/v1/brief` as the baseline** (SSE
`GET /api/v1/events` optional for latency — it can drop events under `Lagged`, management.rs:349,
so it can't be the only source), and POSTs approve/deny on an allowlisted reply.

**Pre-existing hole this surfaces (THREAT_MODEL must own it):** because cos binds `0.0.0.0`,
`semantic-kb-mcp` is *already* an unauthenticated, approve-capable peer of the `:7999` API — ux.12
is not the first such node. The route-scoped approval secret below partially closes this for the
approve/deny routes; fully re-scoping the `0.0.0.0` bind is out of scope (it exists for Docker
hostfwd reachability).

## Security surface (the load-bearing review)

Approving from Telegram is a trust boundary. `request_approval` is a **cooperative self-park**
(agent/mod.rs:874); "approve" hands the parked `args` back as the tool result and re-steps the
agent — it does NOT execute a tool, it **unblocks the agent to perform its self-declared next
action with its OWN capabilities** (scheduler.rs:2297). So the human is trusting the approval's
description. Constraints for v1:

- **Route-scoped approval secret — IN SCOPE (the load-bearing fix; both voices).** The
  `:7999` approve/deny API is unauthenticated and ids are guessable `act_{seq}` (scheduler.rs:1760),
  so **the chat-ID allowlist is NOT the trust boundary** — any cos-net peer / co-resident process /
  compromised sidecar can approve arbitrary ids by enumeration. "Relay-only binding" lives only in
  the sidecar's memory and enforces nothing (the earlier plan framing had this backwards). Fix:
  require an `X-Approval-Token` header on `POST /api/v1/approvals/*/{approve,deny}` (management.rs:161/200),
  `hmac`-compare against `AGENTOS_APPROVAL_SECRET` delivered by env to BOTH agentd and the sidecar;
  add the secret to `PASSENV_BLOCKLIST` semantics so no *other* MCP server receives it. Route-scoped
  → ux.5's full-auth story still lands later. This is the actual control; the chat-ID allowlist is
  the second factor (who may drive the bot), not the boundary.
- **Chat-ID allowlist (correct check):** honor only `message` updates (ignore `edited_message`,
  `channel_post`, `callback_query`, `my_chat_member`); accept iff `from.id == TELEGRAM_CHAT_ID`
  **AND `message.chat.type == "private"`** — else an operator-containing group would leak
  approval/args into the group. `from.id` is unforgeable via the Bot API, so this is sound authZ
  *provided the bot token stays secret*. The bot token (`TELEGRAM_BOT_TOKEN`) is the crown jewel:
  env-only, never logged; compromise = read approval `args_json` + brief text + bridge DoS, but NOT
  approval injection (can't forge `from.id`).
- **Relay-only + re-verify before approve (closes cross-gen id collision).** The sidecar POSTs an
  approve/deny only for an id it delivered and got an allowlisted reply for. **Before POSTing it
  re-`GET /api/v1/approvals` and confirms the id is still pending AND its `args_json` matches what
  was delivered to Telegram** — refuse otherwise. This closes the collision where a deleted
  `checkpoint.json` resets `approval_seq` to 0 and a redelivered "approve act_3" would hit a
  different, freshly-minted `act_3`; it also enforces "the human approved what they actually saw".
- **Surface real args, capped.** The `approval_requested` event carries only `kind/risk/summary`
  (scheduler.rs:1765) — the sidecar MUST fetch `GET /api/v1/approvals` for `args_json`
  (snapshot.rs:127). Send `kind/risk/summary` + a **length-capped, field-filtered** args preview +
  "view full in TUI" — not a full `args_json`/brief dump (Telegram is a third-party egress sink).
- **`update_id` dedup + durable offset.** Track `update_id`, de-dup, and **persist the offset AND
  the delivered-id⇄message bindings to a durable path** — `cos-data:/data` (Docker) / `/run/memory`
  (QEMU), NOT a `~/`-relative file (cos sets no `HOME`, so that lands on the ephemeral FS and dies
  on restart → `getUpdates` redelivers). A same-id replay after resolution 404s (management.rs:178,
  safe); the re-verify check above backstops the cross-generation case.
- **Degrade-safe (verified):** CoS has `[management].enabled` so `has_control == true` →
  `request_approval` always *parks*, never the reject path — independent of the sidecar. Sidecar
  down ⇒ approval stays pending in the TUI (no auto-approve, no block); undelivered digest is a safe
  drop (brief is durable in runs.redb). The sidecar must **fail-closed on its own POST errors** —
  never synthesize an approve, never retry in a way that double-fires.
- **THREAT_MODEL extensions:** `Net{hosts}` is advisory (only `ports` kernel-enforced,
  sandbox/lib.rs:44) — a `[443,7999]` sidecar can reach any host on 443 + loopback:7999; name
  Telegram as a new **egress sink** for approval/brief content (§8.7 unaudited egress); record the
  pre-existing `0.0.0.0`/semantic-kb reachability (§9.2) and that the approval secret is the
  compensating control on the approve/deny routes.

## Scope

- **Route-scoped approval auth (Rust, in agentd):** `X-Approval-Token` header required on
  `POST /api/v1/approvals/*/{approve,deny}` (management.rs:161/200), `hmac::compare_digest` vs
  `AGENTOS_APPROVAL_SECRET` (env; absent ⇒ routes reject with 401 so the control can't be silently
  off — or, to stay backward-compatible with the TUI/CLI, absent-secret ⇒ header not required but
  log a warn; decide at build, default fail-closed). Add the secret to `PASSENV_BLOCKLIST` so other
  MCP servers can't receive it; agentctl's approve/deny + the TUI must send the header too.
- New `docker/telegram_mcp.py` (stdlib-only): stdio JSON-RPC shell + background bridge thread;
  `--test` self-test that mocks BOTH `api.telegram.org` and the `:7999` POST (patch
  `urllib.request.urlopen`, oauth_mcp.py model), needs no token, prints `self-test PASSED` to
  stderr (ci.1 sidecar-tests contract, CONVENTIONS.md:175).
- Bridge behavior: send brief on `brief_written` (events.rs:65); push notification on
  `approval_requested` (events.rs:116); on allowlisted approve/deny reply → POST the approvals
  API; edit/annotate the Telegram message when an approval is resolved elsewhere
  (`approval_http_approved`/`approval_granted`, events.rs:119/161) so the phone reflects reality.
- Wiring: `[[tools.mcp_servers]]` entry (stdio) in both `agentd/cos.agents.toml` and
  `distro/overlay/etc/agentd/cos.agents.toml` with `passenv=["TELEGRAM_BOT_TOKEN",
  "TELEGRAM_CHAT_ID", ...]` and `capabilities=[{Net={hosts=["api.telegram.org"],ports=[443,7999]}}]`.
- Compose: deliver `TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` into the cos service env / secrets file
  (docker-compose.yml:30, entrypoint.sh:11). No new compose service.
- Docs: THREAT_MODEL extension (above), DEPLOYMENT/cos-guide (bot setup + env), ROADMAP checkbox.

## Explicitly OUT
- Remote inject (design doc P4). Remote spawn/cancel/setcaps (that's ux.13's surface; not exposed
  to Telegram in v1). A user/auth system (single chat-ID allowlist only). Egress content audit
  (§8.7, pre-existing gap — noted, not built).

## Decisions
- **D-ENG — sidecar shell — RESOLVED: no-tools MCP stdio server** (agentd-spawned). Chosen for the
  sandbox + PASSENV + CI contract it inherits (not reachability — D1 corrected). Thread starts after
  the handshake.
- **D-ENG — event source — RESOLVED: poll baseline** (`GET /api/v1/approvals`+`/brief`), SSE
  optional for latency only (SSE can drop under `Lagged` → can't be the sole source).
- **D-SEC — approval auth — RESOLVED: route-scoped `X-Approval-Token` shared secret, IN SCOPE**
  (both Eng voices; the chat-ID allowlist alone does not protect the unauthenticated `:7999`
  control boundary). Small deviation from "API auth deferred to ux.5" — justified because ux.12
  materially widens the surface (untrusted Telegram input → approval writer); route-scoped so ux.5
  still owns the full story.
- **D-CEO — digest when CoS is mid-task — RESOLVED: deliver immediately** (bridge thread is
  independent of the agent loop; brief is durable in runs.redb; sidecar just reads and sends).

## Acceptance criteria
- `cargo build/clippy/test --workspace` clean; `docker/telegram_mcp.py --test` exits 0 + prints
  `self-test PASSED` to stderr (offline, mocks Telegram + `:7999`, no token/chat-id required; poll
  thread NOT started under `--test`).
- Approve/deny routes reject without a valid `X-Approval-Token` (401); agentctl + TUI send it; the
  sidecar sends it. Rust test: approve without/with wrong token → rejected; with right token → OK.
- Brief delivered to Telegram on publish; pending approval pushed with a capped args preview
  (fetched from `GET /api/v1/approvals`, not the event).
- Allowlisted `from.id` in a **private** chat resolves the approval via the API; non-allowlisted
  `from.id` OR non-private chat → ignored (no POST); replayed `update_id` → one approve; a resolved
  id → re-verify fails / 404, no double-resolve.
- Sidecar down ⇒ approvals stay pending (no auto-approve, no block); undelivered digest dropped;
  offset + delivered-bindings persist across a checkpoint restart (durable path).
- THREAT_MODEL (egress sink, `Net{hosts}` advisory, pre-existing 0.0.0.0 reachability, approval
  secret as compensating control) + deployment/cos-guide (bot setup + env) updated same PR.

## Test plan (Eng-expanded)
- **Self-test** (offline, mocks `urlopen` for both `api.telegram.org` and `127.0.0.1:7999`,
  dispatch on URL; no env; marker + rc==0).
- **Approval secret (Rust):** approve/deny without token → 401; wrong token → 401; correct →
  200; agentctl path sends the header.
- **Allowlist:** non-allowlisted `from.id` → ignored; allowlisted but `chat.type != private` →
  ignored (group-leak guard); allowlisted private → one POST.
- **Relay + re-verify:** sidecar never POSTs an id it didn't deliver; args-mismatch on re-GET →
  refuse; deleted-checkpoint cross-gen collision (seq resets, redelivered id) → refuse.
- **Dedup:** same `update_id` twice → one approve.
- **Degrade:** `:7999` unreachable → retry/backoff, never synthesizes an approval, fail-closed on
  own POST error; Telegram unreachable → digest dropped, no crash; SSE `Lagged` → poll reconciles.
- **args surfacing:** pushed message carries kind/risk + capped args preview, not full dump.

## Decision Audit Trail

| # | Phase | Decision | Class | Principle | Rationale |
|---|-------|----------|-------|-----------|-----------|
| 1 | Eng | D1 rationale corrected (0.0.0.0 bridge, not loopback) | Mechanical | P5 | Both voices verified cos binds 0.0.0.0+allow_non_loopback |
| 2 | Eng | Keep no-tools MCP stdio shell (for sandbox+PASSENV+CI, not reachability) | Mechanical | P4 | Empty tools/list registers; eager spawn; kill_on_drop lifetime |
| 3 | Eng | Route-scoped X-Approval-Token secret — IN SCOPE | Taste→adopted | P1 | Both: chat-ID allowlist doesn't protect unauth :7999; relay-only is not a control |
| 4 | Eng | Chat check = from.id==allowlist AND chat.type==private, message-only | Mechanical | P5 | Group-leak guard; from.id unforgeable via Bot API |
| 5 | Eng | Re-GET + args-match before approve | Mechanical | P1 | Closes cross-gen id collision on deleted checkpoint + "approved what you saw" |
| 6 | Eng | Durable offset/binding path (cos-data / /run/memory) | Mechanical | P1 | cos sets no HOME → ~/ file ephemeral → redelivery |
| 7 | Eng | Send capped args preview, not full dump | Mechanical | P5 | Telegram = new egress sink (§8.7); brief is email-derived |
| 8 | Eng | Event source = poll baseline, SSE optional | Mechanical | P5 | SSE drops under Lagged; can't be sole source |
| 9 | CEO | Digest mid-task = deliver immediately | Mechanical | P3 | Bridge thread independent; brief durable in runs.redb |
| 10 | Eng | Degrade-safe confirmed; sidecar fail-closed on own POST errors | Mechanical | P1 | has_control independent of sidecar; request_approval always parks |
