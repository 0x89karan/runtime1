# cos-polish — make the Chief of Staff usable end-to-end

**Increment:** cos-polish (harness / backend + config; not the cockpit track)
**Status:** Planned — bugs surfaced in a live Mac+Docker dogfood on **v0.73.2** (2026-07-11). Not started.
**Why now:** auth works (v0.73.2) and the CoS produces a real, well-prioritized daily brief — but a batch
of retrieval / persistence / tuning papercuts make it *look* broken when it isn't. These are small
config/backend fixes; landing them makes the CoS pleasant to use *today*, independent of the cockpit.

## Evidence (from the 2026-07-11 run)
The CoS read Gmail (33 `oauth_call_api` → 200), extracted 5+ high-urgency items (ADP onboarding, ACH/wire,
variable-comp invoice, GoDaddy payment, Entra security), and stored a brief. But: `~/.agentos-output/` is
empty, both the memory pane and a fresh orchestrate agent reported the KB "empty," and two inbox agents
blew their token budget. Ground truth via FUSE: the data **is** there —
`/agents/kb/ops:briefs/0000000000000001` (6.4 KB brief) and `/agents/kb/ops:entities/inbox-2026-07-11`
(7.3 KB). The problems are persistence/retrieval/tuning, not data loss.

## The 9 items (each ships with a test that fails without the fix)

1. **Brief never written to a file — `write_file` capability-denied.**
   `write_file` returned `capability denied: tool 'write_file' requires FsWrite { prefix: "./output/brief-2026-07-11.md" }`.
   The `cos-orchestrator` has `KbRead`/`KbWrite` for `ops:briefs` but **no `FsWrite`** for the output dir, so
   agentd's capability gate blocks the file write (this is a capability check, independent of Landlock).
   **Fix:** add `{ FsWrite = { prefix = "./output" } }` (or `/data/output`) to the `cos-orchestrator`
   capabilities in `agentd/cos.agents.toml` + `distro/overlay/etc/agentd/cos.agents.toml`.
   **Test:** the orchestrator's `write_file` to `./output/brief-*.md` succeeds; the file appears on the mount.

2. **Brief lands as JSON, not readable markdown.** The KB copy stores the brief as a JSON blob (structured).
   Once #1 lands, the orchestrator should write a **markdown** brief to the file and keep the structured
   copy in the KB. **Fix:** brief-writing step emits markdown to the file. **Test:** the file is markdown a
   human can read; the KB entry keeps the structured form.

3. **Memory pane (`agentctl watch [m]`) shows nothing for `ops:*` segments.** FUSE `/agents/kb/` lists
   `ops:briefs` + `ops:entities` with entries, but the pane renders empty — almost certainly the colon in
   the segment name tripping the KB reader. **Fix:** the memory reader/pane handles colon-named segments.
   **Test:** with `ops:briefs`/`ops:entities` populated, the pane lists both segments and their keys.

4. **Agents `kb_search` the wrong scope.** The orchestrate/CoS agents searched segments `canon`/`log`/`scratch`
   — those are storage **classes**, not the **segment** names (`ops:briefs`/`ops:entities`) — so they found
   nothing. The CoS itself also wrote inconsistently (a mix of `ops:briefs` and bare `canon/log/scratch`),
   fragmenting the KB. **Fix:** give agents the configured segment names (prompt or a `kb_list_segments`
   helper) and enforce a single segment convention. **Test:** a `kb_search` from an orchestrate agent finds
   the brief the CoS wrote.

5. **Inbox token budget too small.** Children have `token_budget = 500_000`; the inbox agents spent
   **~820 k** (two `budget_exceeded` events) reading 50 messages — each `oauth_call_api` returns a full body
   and every turn re-sends the growing context. **Fix:** raise the child budget **and/or** lighten the read
   strategy (fetch snippets/metadata, cap message count, summarize incrementally). *(The durable fix is the
   read strategy — see the `memory-routing` plan: store emails once, work from summaries + semantic search.)*
   **Test:** a 50-message inbox run completes under a sane budget without `budget_exceeded`.

6. **Orchestrate REPL swallows the answer on a race.** For a valid inject, both `orchestrator_turn_complete`
   (with the answer) and `agent_completed` fired; the drain loop checked `agent_completed` first and errored
   *"agent exited without completing orchestrated turn"* — even though the answer existed. **Fix:** in
   `agentctl/src/orchestrate.rs`, prefer `orchestrator_turn_complete` and print the answer. **Test:** an
   inject that both completes and terminates still prints the answer, not the error.

7. **`orchestrator.template.toml max_turns = 200` too low.** A chat agent exhausts 200 turns; turn-limit
   truncation orphaned a `tool_use`, producing `Anthropic API 400: tool_use ids were found without
   tool_result` that corrupts the agent. **Fix:** raise `max_turns` for the orchestrator template **and**
   guard against truncating mid-tool-call (never cut a `tool_use` from its `tool_result`). **Test:** a long
   orchestrate session doesn't emit the 400; truncation keeps `tool_use`/`tool_result` paired.

8. **Orchestrator hit one `inference_error`** (`agent_failed cos-orchestrator`, recovered on restart).
   Likely #5/#7-related (large accumulated context / a transient at high token counts). **Fix:** confirm it's
   resolved by #5/#7; if not, add a bounded retry on transient inference errors so one blip doesn't fail the
   orchestrator. **Test:** a simulated transient inference error → the orchestrator retries, doesn't die.

9. **Google OAuth app in Testing mode → Gmail auth dies every 7 days.** The setup steers operators to add
   themselves as a "Test user" (`MCP_SERVERS.md:119`), which is Google's Testing mode — refresh tokens for
   an unverified `gmail.readonly` app expire after **7 days**. So even with everything else perfect, the CoS
   silently loses auth weekly and the operator re-runs the flow. This is the **highest-frequency real
   failure** and the cheapest to kill; it is orthogonal to broker-vs-file mode. **Fix:** publish the OAuth
   app to **Production** (confirm the 7-day clock is actually gone for an unverified `gmail.readonly` app —
   verify, don't assume); update `agentctl auth google` output + `DEPLOYMENT.md`; **fix `MCP_SERVERS.md:119`**
   (which steers operators *into* the trap); add an `invalid_grant` row to the error table. **Test:** the
   run guide's auth section documents Production publishing; the `MCP_SERVERS.md` Test-user step is gone.
   *(Pulled forward per the 2026-07-11 CEO review as a do-now cheap win; it was the "prevention" half of the
   old cred.6 plan.)*

## Non-goals
- The cockpit chat/observe/budget UI (that's Track UX: ux.1/ux.2/ux.8).
- Moving raw emails to the semantic KB (that's the `memory-routing` plan — related, but separate).

## Done
On Mac+Docker: the CoS reads Gmail, writes a **readable markdown brief to `~/.agentos-output/brief-*.md`**,
the KB is findable from the memory pane and from `kb_search`, the inbox run completes without
`budget_exceeded`, and the interactive orchestrator answers cleanly (no swallowed answers, no 400s). Every
fix has a test that fails without it.
