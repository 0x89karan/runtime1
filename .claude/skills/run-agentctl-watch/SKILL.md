---
name: run-agentctl-watch
description: Launch and drive the agentctl watch TUI (ratatui) from an agent — sized pty, keystrokes, readable frame captures, and the fake-docker harness for the [l] Logs view. Use when running, screenshotting, or QA-ing any agentctl watch view.
---

# Run `agentctl watch`

`agentctl watch` is a full-screen ratatui TUI. It cannot be driven by piping into it: it needs a
real pty, and it renders nothing useful without a window size. This skill is the verified path.

Captured from the ux.10-A (Logs view) build, where driving the real binary found a defect that
1 689 passing tests and four rounds of adversarial review had all missed (90% of a log burst
being dropped). **Running it is not optional polish — it is where TUI bugs actually surface.**

## Build

```bash
cargo build -p agentctl            # debug is fine; frames render identically
```

## Run (direct, for humans)

```bash
cd /path/to/agentOS
./target/debug/agentctl watch --url http://127.0.0.1:7999
```

`q` quits from the Dashboard. Without a running `agentd` the agent table shows an HTTP error —
that is expected, and every view still works (the Logs view is independent of the data source).

## Run (for agents) — `driver.py`

`tmux` is not installed on the usual dev machines here, so use the bundled pty driver. Stdlib
Python only; no install step.

```bash
python3 .claude/skills/run-agentctl-watch/driver.py \
  --frames /tmp/frames \
  --step wait:3 --step snap:dashboard \
  --step key:s --step wait:1 --step snap:system \
  --step key:q --step wait:1 \
  --step key:q --step wait:2 \
  -- ./target/debug/agentctl watch --url http://127.0.0.1:7999 --no-plain
```

Each `--step` runs in order: `wait:SECONDS`, `key:LITERAL` (escapes work: `key:\r`, `key:\t`,
`key:\x1b`), `snap:NAME` → `--frames/NAME.txt`, `size:ROWSxCOLS`. Then read the frames:

```bash
head -12 /tmp/frames/dashboard.txt      # header + body
sed -n '$p'  /tmp/frames/dashboard.txt  # footer / key hints
```

Exit status is `0` only if the app quit on its own, so a broken quit path fails the command.
`--no-plain` forces the TUI even when stdout isn't a tty.

### Three things that will waste an hour if you skip them

1. **The pty must have a size.** `driver.py` sets `TIOCSWINSZ`. Without it the pty is 0×0 and
   ratatui draws an EMPTY frame — every assertion then "passes" against a blank screen.
2. **Assert against `snap` output, never raw bytes.** ratatui writes changed cells only, with
   style escapes interleaved mid-line, so `grep`ping raw output splits words and silently
   misses matches. `snap` forces a full repaint (one-column resize) and replays it into a grid.
3. **Use `waitpid(WNOHANG)` to test for exit, not `kill(pid, 0)`.** `kill(pid, 0)` succeeds on a
   zombie, so a clean exit looks like a hang. `driver.py` already does this.

## The `[l]` Logs view: fake-docker harness

The Logs view is gated on `docker compose ps --all --quiet` finding a container, and it tails
`docker compose logs`. To drive it with no daemon, put the bundled fake `docker` first on PATH:

```bash
H=/tmp/fakebin && mkdir -p $H && cp .claude/skills/run-agentctl-watch/fake-docker.sh $H/docker
chmod +x $H/docker
export PATH="$H:$PATH"
export FAKE_DOCKER_PIDFILE=/tmp/tail.pid       # for the orphan assertion below

python3 .claude/skills/run-agentctl-watch/driver.py --frames /tmp/frames \
  --step wait:3 --step key:l   --step wait:3 --step snap:logs \
  --step key:\\t --step wait:1 --step snap:logs-filtered \
  --step key:/ --step key:chief --step key:\\r --step wait:1 --step snap:logs-search \
  --step key:q --step wait:1 --step key:q --step wait:2 \
  -- ./target/debug/agentctl watch --url http://127.0.0.1:7999 --no-plain
```

Search applies WITHIN the active service filter, so the query has to match the filtered
service: after `Tab` selects `cos`, searching `chief` matches and searching `heartbeat`
(an `agent` string) correctly reports `0 matches`. Worth knowing before reading it as a bug.

`FAKE_DOCKER_MODE` selects the scenario:

| Mode | What it exercises |
|---|---|
| `stream` (default) | 3 services, ~3 lines/sec — filters, search, follow, timestamps |
| `flood` | 5 000-line burst — the 2 000-line ring cap and channel backpressure (header must NOT show `⚠ N dropped`) |
| `giant` | one ~12 KB record with no newline — the byte cap, truncation, and resync |
| `empty` | probe exits 0 with no containers — `[l]` must be ABSENT from the legend and inert |
| `fail` | probe exits non-zero — same absent-and-inert expectation |
| `hang` | probe sleeps — the 3 s startup deadline must fire and the TUI must still start |

### Always check for an orphaned tail

`docker compose logs --follow` never ends on its own, and `docker compose` is a CLI **plugin**
(the `docker` process forks `docker-compose`), so teardown has to kill the whole process group.
The fake docker reproduces that fork, which is what makes this assertion real:

```bash
GC=$(cat /tmp/tail.pid); sleep 1
kill -0 "$GC" 2>/dev/null && echo "FAIL: orphaned tail $GC" || echo "PASS: tail reaped"
ps -eo pid,command | grep "chief of staff tick" | grep -v grep || echo "PASS: no stream left"
```

## Key reference

| View | Keys |
|---|---|
| Dashboard | `↑`/`↓` or `k`/`j` select · `Enter` detail · `Tab` chat rail · `r` retarget · `s`ystem · `t`opology · `m`emory · `n`ew · `a`pprovals · `c`reds · `i`nspector · `l`ogs (docker only) · `q` quit |
| Logs | `Tab` service filter · `/` search then `Enter` commit, `Esc` clear · `n`/`N` matches · `↑`/`↓`/`j`/`k`, PageUp/Down scroll · `g`/`G` top/bottom · `t` rel↔abs time · `Esc`/`q` back |
| Memory / Inspector | `Tab` pane or filter · `/` search · `j`/`k` scroll · `Esc`/`q` back |
| Spawn | `Tab` field · `Enter` newline in the task field · `g` preview · `r` spawn · `Esc`/`q` back |

Text fields (chat rail, searches, spawn task, deny reason) capture printable keys, so `q` there
types a `q` — send `Esc` first, or the view will not exit.

## Layout floors worth driving

Small terminals take different code paths, and they have regressed before:

- `size:24x80` — the Dashboard hides the chat rail (needs ≥115 cols / ≥8 rows).
- `size:4x80` then `key:l` — the Logs view must show its "needs at least 6 rows" notice rather
  than an empty bordered box.
