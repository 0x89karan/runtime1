//! ux.10 sub-part A — the `[l]` Logs view: a bounded tail of `docker compose logs`.
//!
//! Data model + state machine only; the render lives in `views::render_logs`, the
//! subprocess in `crate::docker`, and the reader thread in `pump::logs_loop`.
//!
//! ```text
//!   docker compose logs --follow  ──► pump::logs_loop (std::thread, bounded blocking read)
//!                                       │  batches while more is already buffered
//!                                       ▼
//!                          AppEvent::LogLines(Vec<LogLine>) ─► step() ─► push_lines()
//!                                                                          │
//!                                              bounded ring (LOG_RING_CAP) ┘
//! ```
//!
//! Scroll model: `scroll` is an index into the *filtered* line list and is only consulted
//! while `follow` is false. In follow mode the viewport is derived (`len - rows`) at render
//! time, so a batch of new lines needs no scroll bookkeeping at all.

use std::collections::VecDeque;

use tui_input::Input;

/// Cap on the in-memory log ring (tail-drop), mirroring `app::EVENT_RING_CAP`. Bounds
/// memory flat regardless of how chatty the compose project is.
pub const LOG_RING_CAP: usize = 2000;

/// Longest rendered log payload. Container output can contain a single enormous line (a
/// serialized blob, a stack trace with no newlines); the view clips rather than building a
/// megabyte-wide span for a terminal that shows ~200 columns.
pub const MAX_LOG_LINE_CHARS: usize = 500;

/// One tailed line. `service` is resolved from compose's line prefix; `ts` is the verbatim
/// RFC3339 stamp from `--timestamps` (formatted at render time, so `[t]` can switch between
/// relative and absolute without re-parsing the stream).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LogLine {
    pub service: Option<String>,
    pub ts:      Option<String>,
    pub text:    String,
    /// An agentctl-generated status line (stream ended, tail failed to start) rather than
    /// container output. Rendered distinctly and never attributed to a service, so an
    /// operator can't mistake our own message for something a container printed.
    pub notice:  bool,
}

impl LogLine {
    pub fn notice(text: impl Into<String>) -> Self {
        Self { service: None, ts: None, text: text.into(), notice: true }
    }
}

/// Parse one raw line of `docker compose logs --timestamps` output.
///
/// Shape: `<container display name>  | <RFC3339 ts> <payload>`. Both halves are optional in
/// practice (compose emits its own un-prefixed progress lines, and a payload can be empty),
/// so every step degrades to "keep the text verbatim" rather than dropping the line.
pub fn parse_compose_line(raw: &str, known: &[String]) -> LogLine {
    // Split on the FIRST `|` only — payloads routinely contain pipes (shell commands,
    // table output), and splitting on the last one would eat the message.
    let (service, rest) = match raw.split_once('|') {
        Some((prefix, rest)) => (resolve_service(prefix.trim(), known), rest.trim_start()),
        None => (None, raw),
    };
    let (ts, text) = split_timestamp(rest);
    LogLine { service, ts, text, notice: false }
}

/// Map compose's container display name back to a declared service name.
///
/// Compose renders the *container* name: `<service>-<index>`, or
/// `<project>-<service>-<index>` depending on compose version and project name. Matching is
/// on `-`-SEGMENT BOUNDARIES, never a bare substring — this repo's own project is the
/// counter-example that proves it: with project `agentos` and services `cos`/`agent`, a
/// substring match on `agentos-cos-1` finds BOTH `cos` and `agent` (inside "agent-os"), and
/// longest-wins then labels every `cos` line as `agent` (caught by /review's Codex pass).
/// Anchoring to a segment boundary makes `agentos-cos` match only `cos`.
fn resolve_service(prefix: &str, known: &[String]) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    if known.iter().any(|s| s == prefix) {
        return Some(prefix.to_string());
    }
    // Drop the replica index once, then look for a declared service that is either the whole
    // remainder or its `-`-delimited tail (i.e. the part after a project prefix). Longest
    // wins so a project declaring both `agent` and `sub-agent` resolves the specific one.
    let candidate = strip_replica_index(prefix);
    // `strip_suffix` + a `-` check, not `ends_with(&format!("-{s}"))`: same segment-anchored
    // semantics without allocating a String per known service per parsed line.
    if let Some(best) = known
        .iter()
        .filter(|s| {
            !s.is_empty()
                && (candidate == s.as_str()
                    || candidate
                        .strip_suffix(s.as_str())
                        .is_some_and(|head| head.ends_with('-')))
        })
        .max_by_key(|s| s.len())
    {
        return Some(best.clone());
    }
    // Declared list unavailable (the `ps --services` probe failed) or an unrecognized name —
    // fall back to the index-stripped prefix, so replicas of one service still group under a
    // single Tab filter entry.
    Some(candidate.to_string())
}

/// `cos-1` → `cos`; `qdrant` → `qdrant`; `web-2-1` → `web-2`.
fn strip_replica_index(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((head, idx))
            if !head.is_empty() && !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => name,
    }
}

/// Peel a leading RFC3339 timestamp off the payload, if `--timestamps` produced one.
///
/// The no-whitespace case is not just defensive: a container printing an EMPTY line yields
/// `<prefix> | <stamp>` with no payload at all, and treating that as "no timestamp" rendered
/// the stamp itself as the log text with an empty time column (found by /review's testing
/// specialist).
fn split_timestamp(rest: &str) -> (Option<String>, String) {
    match rest.split_once(char::is_whitespace) {
        Some((head, tail)) if looks_like_timestamp(head) => {
            (Some(head.to_string()), tail.to_string())
        }
        // Whole remainder is a bare stamp → an empty log line, not a line of text.
        None if looks_like_timestamp(rest) => (Some(rest.to_string()), String::new()),
        _ => (None, rest.to_string()),
    }
}

/// Cheap shape test for `2026-07-27T12:34:56.123456789Z` — deliberately not a full parse.
/// A payload that merely *starts* with a word must never be mistaken for a timestamp, and
/// a stamp we misjudge still renders (as text) rather than being dropped.
fn looks_like_timestamp(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() >= 20
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
}

/// Render a stored timestamp: relative age by default (`3s`, `12m`), absolute UTC clock
/// time with `[t]` (D5). `now_unix` is injected so the formatting is pure and testable.
/// UTC, not local: the rest of the codebase renders UTC (`memory::format_unix_secs`) and
/// docker's own stamps are UTC — converting only here would make two adjacent columns
/// disagree.
pub fn format_ts(ts: Option<&str>, absolute: bool, now_unix: i64) -> String {
    let Some(raw) = ts else { return String::new() };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
        // Unparseable stamp: show its first 8 characters rather than lying with an age.
        return raw.chars().take(8).collect();
    };
    if absolute {
        return parsed.naive_utc().format("%H:%M:%S").to_string();
    }
    let age = now_unix.saturating_sub(parsed.timestamp());
    match age {
        a if a < 0    => "now".to_string(),
        a if a < 60   => format!("{a}s"),
        a if a < 3600 => format!("{}m", a / 60),
        a if a < 86400 => format!("{}h", a / 3600),
        a             => format!("{}d", a / 86400),
    }
}

/// Rows of log payload visible at a given terminal height. Single source of truth shared by
/// `views::render_logs` and the key handler (which must know the page size to scroll by a
/// page and to decide when a downward scroll has reached the bottom). Chrome = 1 header row
/// + 1 footer row (`header_footer_layout`) + the content block's 2 border rows.
pub fn logs_viewport_rows(term_rows: u16) -> usize {
    term_rows.saturating_sub(4).max(1) as usize
}

/// State for the Logs view.
#[derive(Debug)]
pub struct LogsState {
    /// True when a Compose project was detected at startup. Gates BOTH the `[l]` binding
    /// and its legend entry — the single source of truth for "is this view reachable".
    pub available:     bool,
    /// Declared services (from `ps --services`), plus any prefix seen on a real line that
    /// the declared list didn't cover. Index `i` here is filter index `i + 1`.
    pub services:      Vec<String>,
    /// Compose project label, rendered in the view's title. The project is resolved from
    /// agentctl's CWD while the data source is resolved separately, so the operator has to be
    /// told WHICH containers these lines came from.
    pub project:       String,
    /// Bounded ring of tailed lines, oldest dropped.
    pub lines:         VecDeque<LogLine>,
    /// 0 = `[All]`; otherwise `services[filter_idx - 1]`.
    pub filter_idx:    usize,
    /// First visible index into the FILTERED list. Only consulted while `!follow`.
    pub scroll:        usize,
    /// Pinned to the newest line (auto-scroll). Any upward scroll clears it; `G`/`End`
    /// re-arms it, as does scrolling back to the bottom.
    pub follow:        bool,
    /// True while the `/` search field owns the keyboard.
    pub search_active: bool,
    /// Search query (`tui_input`, shared with sub-part B's other input sites). Search
    /// HIGHLIGHTS and drives `n`/`N` jumps — it does not filter, so a match keeps its
    /// surrounding context (D8, and the reason logs differ from Memory/Inspector search).
    pub search_query:  Input,
    /// Index into `match_positions()` of the match `n`/`N` last jumped to. `None` = no jump
    /// yet (or the operator moved/refiltered since), in which case the next jump re-enters
    /// from the current viewport.
    pub match_cursor:  Option<usize>,
    /// Lines lost to channel backpressure. Surfaced in the header — a silently truncated
    /// log would read as "nothing happened".
    pub dropped:       usize,
    /// `[t]`: absolute clock time instead of relative age.
    pub absolute_ts:   bool,
}

impl Default for LogsState {
    fn default() -> Self {
        Self {
            available:     false,
            services:      Vec::new(),
            project:       String::new(),
            lines:         VecDeque::new(),
            filter_idx:    0,
            scroll:        0,
            // Follow by default: a log view that opens anywhere but the newest line is
            // just a worse `docker compose logs`.
            follow:        true,
            search_active: false,
            search_query:  Input::default(),
            match_cursor:  None,
            dropped:       0,
            absolute_ts:   false,
        }
    }
}

/// Does `line` pass the active service filter? Free function (not a method) so
/// `push_lines` can consult it while holding a mutable borrow of the ring.
fn passes(line: &LogLine, active: Option<&str>) -> bool {
    match active {
        // Notices are agentctl's own status lines: always visible, so "the stream died"
        // can never be hidden behind a service filter.
        None => true,
        Some(_) if line.notice => true,
        Some(svc) => line.service.as_deref() == Some(svc),
    }
}

impl LogsState {
    /// Mark the view reachable and seed the service filter + project label (called once at
    /// startup with the detected `DockerContext`).
    pub fn enable(&mut self, services: Vec<String>, project: String) {
        self.available = true;
        self.services  = services;
        self.project   = project;
    }

    /// Fold one batch from the reader thread into the ring.
    pub fn push_lines(&mut self, batch: Vec<LogLine>) {
        let active = self.active_filter().map(str::to_string);
        for line in batch {
            // Register a service the `ps --services` probe didn't report (older compose
            // formats, a container started after startup) so it still gets a Tab entry.
            if let Some(svc) = line.service.as_deref() {
                if !svc.is_empty() && !self.services.iter().any(|s| s == svc) {
                    self.services.push(svc.to_string());
                }
            }
            self.lines.push_back(line);
        }
        let overflow = self.lines.len().saturating_sub(LOG_RING_CAP);
        if overflow == 0 {
            return;
        }
        let mut evicted_visible = 0usize;
        for _ in 0..overflow {
            if let Some(old) = self.lines.pop_front() {
                if passes(&old, active.as_deref()) {
                    evicted_visible += 1;
                }
            }
        }
        // A paused viewport is anchored by index, and ring eviction shifts every index
        // down. Without this compensation the operator's reading position would drift
        // toward newer output on its own while they are trying to read older output.
        if !self.follow {
            self.scroll = self.scroll.saturating_sub(evicted_visible);
        }
        // `match_cursor` is an index into the same renumbered filtered list, so eviction makes
        // it point at a different match. Drop it and let the next n/N re-enter from the
        // viewport (found by /review's maintainability pass — scroll was compensated, this
        // sibling index was silently left stale).
        if evicted_visible > 0 {
            self.match_cursor = None;
        }
    }

    /// Record `n` log lines dropped to channel backpressure.
    pub fn note_dropped(&mut self, n: usize) {
        self.dropped = self.dropped.saturating_add(n);
    }

    /// Active service filter, or `None` for `[All]`.
    pub fn active_filter(&self) -> Option<&str> {
        if self.filter_idx == 0 {
            None
        } else {
            self.services.get(self.filter_idx - 1).map(String::as_str)
        }
    }

    /// `[All]` plus one entry per known service — the `Tab` cycle, in display order.
    pub fn filter_labels(&self) -> Vec<&str> {
        let mut out = vec!["All"];
        out.extend(self.services.iter().map(String::as_str));
        out
    }

    /// `Tab`: next service (wrapping through `[All]`). Re-arms follow — the visible line
    /// set just changed wholesale, and a stale index into the previous filter's list is
    /// meaningless.
    pub fn cycle_filter(&mut self) {
        let n = self.services.len() + 1;
        self.filter_idx = (self.filter_idx + 1) % n;
        self.scroll = 0;
        self.follow = true;
        // Match positions are indices into the FILTERED list, so they all just changed
        // meaning — re-enter from the viewport on the next n/N.
        self.match_cursor = None;
    }

    /// Indices into `lines` that pass the active service filter, oldest first.
    pub fn visible_indices(&self) -> Vec<usize> {
        let active = self.active_filter();
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| passes(l, active))
            .map(|(i, _)| i)
            .collect()
    }

    fn filtered_len(&self) -> usize {
        let active = self.active_filter();
        self.lines.iter().filter(|l| passes(l, active)).count()
    }

    /// Does this line match the current search query? Empty query matches nothing (so
    /// nothing is highlighted and `n`/`N` are no-ops). Case-insensitive, and the service
    /// name is searchable too — `/cos` is the obvious way to find one service's lines.
    ///
    /// Allocation-free (`contains_ignore_case`, not `to_lowercase()`): this runs over every
    /// line in the ring on every frame that has an active query, and two `String`s per line
    /// × 2 000 lines × up to 33 fps is a lot of garbage for a substring test.
    ///
    /// Searches only the payload prefix that is actually RENDERED (`MAX_LOG_LINE_CHARS`).
    /// Two reasons: a match the operator can never see is not a useful match, and it bounds
    /// the naive window scan — /review's performance pass measured 386 ms for one full-ring
    /// pass with a long query against repetitive 4 KB lines (base64, hex, `----` padding).
    pub fn matches_search(&self, line: &LogLine) -> bool {
        let q = self.search_query.value();
        if q.is_empty() {
            return false;
        }
        let visible_end = line
            .text
            .char_indices()
            .nth(MAX_LOG_LINE_CHARS)
            .map_or(line.text.len(), |(i, _)| i);
        contains_ignore_case(&line.text[..visible_end], q)
            || line.service.as_deref().is_some_and(|s| contains_ignore_case(s, q))
    }

    /// Positions (within the FILTERED list) of lines matching the search query.
    pub fn match_positions(&self) -> Vec<usize> {
        self.visible_indices()
            .iter()
            .enumerate()
            .filter(|(_, &ring_idx)| {
                self.lines.get(ring_idx).is_some_and(|l| self.matches_search(l))
            })
            .map(|(pos, _)| pos)
            .collect()
    }

    /// First visible filtered index for a viewport of `rows`. In follow mode this is
    /// derived from the current length (so new lines need no bookkeeping); otherwise it is
    /// the stored offset, clamped so the viewport can never scroll past the end.
    pub fn effective_scroll(&self, rows: usize) -> usize {
        self.effective_scroll_for_len(self.filtered_len(), rows)
    }

    /// `effective_scroll` for a caller that has ALREADY counted the filtered lines (the
    /// renderer holds `visible_indices()`), so the ring isn't walked twice per frame.
    pub fn effective_scroll_for_len(&self, len: usize, rows: usize) -> usize {
        let max = len.saturating_sub(rows);
        if self.follow {
            max
        } else {
            self.scroll.min(max)
        }
    }

    /// Scroll by `delta` lines. Any upward movement pauses follow; reaching the bottom
    /// re-arms it (same idiom as the converse rail's `End`).
    pub fn scroll_by(&mut self, rows: usize, delta: isize) {
        let from = self.effective_scroll(rows);
        let max  = self.filtered_len().saturating_sub(rows);
        let next = if delta < 0 {
            from.saturating_sub(delta.unsigned_abs())
        } else {
            from.saturating_add(delta as usize).min(max)
        };
        self.scroll = next;
        self.follow = next >= max;
        // A manual move means the next n/N should continue from where the operator is
        // looking, not from the last jump.
        self.match_cursor = None;
    }

    /// `g`/`Home`: oldest retained line, follow paused.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow = false;
        self.match_cursor = None;
    }

    /// `G`/`End`: newest line, follow re-armed.
    pub fn scroll_to_bottom(&mut self) {
        self.follow = true;
        self.scroll = 0;
        self.match_cursor = None;
    }

    /// `n`: next search match (wrapping).
    ///
    /// Stepping is by MATCH INDEX (`match_cursor`), not by comparing against the viewport
    /// offset. The offset-based version deadlocked on the last page: a match inside the final
    /// viewport clamps `scroll` to `max`, so "first match after scroll" kept resolving to the
    /// same match forever and `n` never wrapped (found by /review's Codex pass).
    pub fn next_match(&mut self, rows: usize) {
        let positions = self.match_positions();
        if positions.is_empty() {
            return;
        }
        let idx = match self.match_cursor {
            Some(c) => (c + 1) % positions.len(),
            // Cold start (or after a manual scroll / filter change): enter at the first match
            // at or below what the operator is currently looking at.
            None => positions
                .iter()
                .position(|&p| p >= self.effective_scroll(rows))
                .unwrap_or(0),
        };
        self.match_cursor = Some(idx);
        self.jump_to(rows, positions[idx]);
    }

    /// `N`: previous search match (wrapping). Mirror of `next_match`.
    pub fn prev_match(&mut self, rows: usize) {
        let positions = self.match_positions();
        if positions.is_empty() {
            return;
        }
        let idx = match self.match_cursor {
            Some(c) => (c + positions.len() - 1) % positions.len(),
            None => positions
                .iter()
                .rposition(|&p| p <= self.effective_scroll(rows))
                .unwrap_or(positions.len() - 1),
        };
        self.match_cursor = Some(idx);
        self.jump_to(rows, positions[idx]);
    }

    /// Park the viewport at filtered index `pos`. Always pauses follow unless the jump
    /// landed at the bottom anyway — a match the operator jumped to must stay on screen
    /// instead of being scrolled away by the next arriving batch.
    fn jump_to(&mut self, rows: usize, pos: usize) {
        let max = self.filtered_len().saturating_sub(rows);
        self.scroll = pos.min(max);
        self.follow = self.scroll >= max;
    }
}

/// Case-insensitive substring test with no allocation. ASCII-case-folds each byte window;
/// non-ASCII bytes compare exactly (so a match on non-ASCII text is case-SENSITIVE — the
/// honest tradeoff for keeping this off the render hot path, and log payloads searched by an
/// operator are overwhelmingly ASCII identifiers, ids, and error strings).
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len())
        .any(|w| w.iter().zip(n).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

/// Clip a payload to `MAX_LOG_LINE_CHARS` on a char boundary, flagging the cut so a
/// truncated line never masquerades as a complete one.
pub fn clip_payload(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_LOG_LINE_CHARS).collect();
    if out.chars().count() < text.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec!["cos".to_string(), "agent".to_string(), "qdrant".to_string()]
    }

    fn line(service: &str, text: &str) -> LogLine {
        LogLine {
            service: Some(service.to_string()),
            ts:      None,
            text:    text.to_string(),
            notice:  false,
        }
    }

    #[test]
    fn parses_service_timestamp_and_payload() {
        let l = parse_compose_line(
            "cos-1  | 2026-07-27T12:34:56.123456789Z starting chief of staff",
            &known(),
        );
        assert_eq!(l.service.as_deref(), Some("cos"));
        assert_eq!(l.ts.as_deref(), Some("2026-07-27T12:34:56.123456789Z"));
        assert_eq!(l.text, "starting chief of staff");
        assert!(!l.notice);
    }

    #[test]
    fn parses_project_prefixed_container_name_to_longest_known_service() {
        // Older compose / custom project names render `<project>-<service>-<n>`.
        let l = parse_compose_line("agentos-qdrant-1 | 2026-07-27T00:00:00Z ready", &known());
        assert_eq!(l.service.as_deref(), Some("qdrant"));
    }

    #[test]
    fn longest_match_wins_so_agent_does_not_shadow_agentd() {
        let svcs = vec!["agent".to_string(), "agentd".to_string()];
        let l = parse_compose_line("agentd-1 | 2026-07-27T00:00:00Z up", &svcs);
        assert_eq!(l.service.as_deref(), Some("agentd"));
    }

    /// Regression for the substring-match bug /review's Codex pass caught, using THIS repo's
    /// real compose project (`agentos`) and services: a bare `prefix.contains(service)` finds
    /// `agent` inside "ag-ent-os" and mislabels every `cos` line as `agent`.
    #[test]
    fn project_prefix_containing_a_service_name_does_not_steal_attribution() {
        let svcs = vec!["cos".to_string(), "agent".to_string(), "qdrant".to_string()];
        assert_eq!(
            parse_compose_line("agentos-cos-1  | 2026-07-27T00:00:00Z hi", &svcs).service.as_deref(),
            Some("cos"),
        );
        assert_eq!(
            parse_compose_line("agentos-agent-1  | 2026-07-27T00:00:00Z hi", &svcs)
                .service
                .as_deref(),
            Some("agent"),
        );
        assert_eq!(
            parse_compose_line("agentos-qdrant-1  | 2026-07-27T00:00:00Z hi", &svcs)
                .service
                .as_deref(),
            Some("qdrant"),
        );
    }

    #[test]
    fn hyphenated_service_names_resolve_on_segment_boundaries() {
        let svcs = vec!["agent".to_string(), "sub-agent".to_string()];
        assert_eq!(
            parse_compose_line("proj-sub-agent-1 | 2026-07-27T00:00:00Z x", &svcs)
                .service
                .as_deref(),
            Some("sub-agent"),
        );
        assert_eq!(
            parse_compose_line("proj-agent-1 | 2026-07-27T00:00:00Z x", &svcs).service.as_deref(),
            Some("agent"),
        );
    }

    #[test]
    fn unknown_service_falls_back_to_prefix_without_replica_index() {
        let l = parse_compose_line("mystery-3 | 2026-07-27T00:00:00Z hello", &[]);
        assert_eq!(l.service.as_deref(), Some("mystery"));
    }

    #[test]
    fn line_without_prefix_keeps_full_text_and_has_no_service() {
        let l = parse_compose_line("no prefix here", &known());
        assert_eq!(l.service, None);
        assert_eq!(l.ts, None);
        assert_eq!(l.text, "no prefix here");
    }

    #[test]
    fn only_the_first_pipe_splits_the_prefix() {
        let l = parse_compose_line(
            "cos-1  | 2026-07-27T00:00:00Z ps aux | grep agentd",
            &known(),
        );
        assert_eq!(l.service.as_deref(), Some("cos"));
        assert_eq!(l.text, "ps aux | grep agentd");
    }

    #[test]
    fn payload_that_is_not_a_timestamp_is_kept_whole() {
        let l = parse_compose_line("cos-1  | hello world", &known());
        assert_eq!(l.ts, None);
        assert_eq!(l.text, "hello world");
    }

    #[test]
    fn ring_is_bounded_and_drops_oldest() {
        let mut st = LogsState::default();
        let batch: Vec<LogLine> =
            (0..LOG_RING_CAP + 50).map(|i| line("cos", &format!("line {i}"))).collect();
        st.push_lines(batch);
        assert_eq!(st.lines.len(), LOG_RING_CAP);
        assert_eq!(st.lines.front().unwrap().text, "line 50");
        assert_eq!(st.lines.back().unwrap().text, format!("line {}", LOG_RING_CAP + 49));
    }

    #[test]
    fn paused_scroll_is_compensated_when_the_ring_evicts() {
        let mut st = LogsState::default();
        st.push_lines((0..LOG_RING_CAP).map(|i| line("cos", &format!("l{i}"))).collect());
        st.follow = false;
        st.scroll = 100;
        st.push_lines((0..10).map(|i| line("cos", &format!("new{i}"))).collect());
        // 10 lines evicted from the front → the same content stays under the viewport.
        assert_eq!(st.scroll, 90);
    }

    #[test]
    fn push_lines_registers_services_absent_from_the_declared_list() {
        let mut st = LogsState::default();
        st.enable(vec!["cos".to_string()], "test-project".to_string());
        st.push_lines(vec![line("cos", "a"), line("late-joiner", "b")]);
        assert_eq!(st.filter_labels(), vec!["All", "cos", "late-joiner"]);
    }

    #[test]
    fn tab_cycles_all_then_each_service_and_wraps() {
        let mut st = LogsState::default();
        st.enable(vec!["cos".to_string(), "agent".to_string()], "test-project".to_string());
        assert_eq!(st.active_filter(), None);
        st.cycle_filter();
        assert_eq!(st.active_filter(), Some("cos"));
        st.cycle_filter();
        assert_eq!(st.active_filter(), Some("agent"));
        st.cycle_filter();
        assert_eq!(st.active_filter(), None);
    }

    #[test]
    fn service_filter_selects_only_that_services_lines_plus_notices() {
        let mut st = LogsState::default();
        st.enable(vec!["cos".to_string(), "agent".to_string()], "test-project".to_string());
        st.push_lines(vec![
            line("cos", "a"),
            line("agent", "b"),
            line("cos", "c"),
            LogLine::notice("stream ended"),
        ]);
        assert_eq!(st.filtered_len(), 4);
        st.cycle_filter(); // → cos
        assert_eq!(st.active_filter(), Some("cos"));
        assert_eq!(st.filtered_len(), 3); // 2 cos lines + the notice
    }

    #[test]
    fn follow_mode_pins_the_viewport_to_the_newest_lines() {
        let mut st = LogsState::default();
        st.push_lines((0..100).map(|i| line("cos", &format!("l{i}"))).collect());
        assert!(st.follow);
        assert_eq!(st.effective_scroll(10), 90);
        st.push_lines((0..5).map(|i| line("cos", &format!("n{i}"))).collect());
        assert_eq!(st.effective_scroll(10), 95);
    }

    #[test]
    fn scrolling_up_pauses_follow_and_back_down_re_arms_it() {
        let mut st = LogsState::default();
        st.push_lines((0..100).map(|i| line("cos", &format!("l{i}"))).collect());
        st.scroll_by(10, -1);
        assert!(!st.follow);
        assert_eq!(st.effective_scroll(10), 89);
        st.scroll_by(10, 1);
        assert!(st.follow);
        assert_eq!(st.effective_scroll(10), 90);
    }

    #[test]
    fn page_scroll_and_top_bottom_jumps() {
        let mut st = LogsState::default();
        st.push_lines((0..100).map(|i| line("cos", &format!("l{i}"))).collect());
        st.scroll_by(10, -10);
        assert_eq!(st.effective_scroll(10), 80);
        st.scroll_to_top();
        assert!(!st.follow);
        assert_eq!(st.effective_scroll(10), 0);
        st.scroll_to_bottom();
        assert!(st.follow);
        assert_eq!(st.effective_scroll(10), 90);
    }

    #[test]
    fn viewport_never_scrolls_past_the_end_even_with_fewer_lines_than_rows() {
        let mut st = LogsState::default();
        st.push_lines(vec![line("cos", "only")]);
        assert_eq!(st.effective_scroll(30), 0);
        st.scroll_by(30, 5);
        assert_eq!(st.effective_scroll(30), 0);
    }

    #[test]
    fn search_matches_payload_and_service_case_insensitively() {
        let mut st =
            LogsState { search_query: Input::new("ERROR".to_string()), ..Default::default() };
        assert!(st.matches_search(&line("cos", "an error occurred")));
        assert!(!st.matches_search(&line("cos", "all good")));
        st.search_query = Input::new("qdr".to_string());
        assert!(st.matches_search(&line("qdrant", "up")));
    }

    #[test]
    fn empty_search_matches_nothing_and_match_nav_is_a_noop() {
        let mut st = LogsState::default();
        st.push_lines((0..50).map(|i| line("cos", &format!("l{i}"))).collect());
        assert!(st.match_positions().is_empty());
        let before = st.effective_scroll(10);
        st.next_match(10);
        assert_eq!(st.effective_scroll(10), before);
    }

    /// Regression: a match inside the LAST viewport clamps `scroll` to `max`, so the old
    /// offset-derived "next match after scroll" resolved to the same match forever and `n`
    /// never wrapped back to the earlier one (found by /review's Codex pass).
    #[test]
    fn n_wraps_even_when_a_match_sits_in_the_final_viewport() {
        let mut st = LogsState::default();
        let mut batch: Vec<LogLine> = (0..50).map(|i| line("cos", &format!("l{i}"))).collect();
        batch[5]  = line("cos", "needle one");
        batch[48] = line("cos", "needle two");
        st.push_lines(batch);
        st.search_query = Input::new("needle".to_string());
        assert_eq!(st.match_positions(), vec![5, 48]);
        // rows=10 → max scroll is 40, so match 48 can never BE the viewport top: it is shown
        // by clamping to 40. The old offset-derived stepping read that clamp back as "still
        // at 40", re-found 48, and pinned the view at 40 forever.
        let seq: Vec<usize> = (0..4)
            .map(|_| {
                st.next_match(10);
                st.effective_scroll(10)
            })
            .collect();
        assert_eq!(
            seq,
            vec![40, 5, 40, 5],
            "n must cycle between the two matches (40 = the clamped view of match 48)"
        );
    }

    #[test]
    fn case_insensitive_contains_is_allocation_free_and_correct() {
        assert!(contains_ignore_case("An ERROR occurred", "error"));
        assert!(contains_ignore_case("abc", ""));
        assert!(!contains_ignore_case("abc", "abcd"));
        assert!(contains_ignore_case("aXbXc", "xb"));
        assert!(!contains_ignore_case("", "x"));
    }

    #[test]
    fn n_and_shift_n_jump_between_matches_and_pause_follow() {
        let mut st = LogsState::default();
        let mut batch: Vec<LogLine> =
            (0..50).map(|i| line("cos", &format!("l{i}"))).collect();
        batch[5]  = line("cos", "boom needle");
        batch[30] = line("cos", "another needle");
        st.push_lines(batch);
        st.search_query = Input::new("needle".to_string());
        assert_eq!(st.match_positions(), vec![5, 30]);
        st.scroll_to_top();
        st.next_match(10);
        assert_eq!(st.effective_scroll(10), 5);
        assert!(!st.follow);
        st.next_match(10);
        assert_eq!(st.effective_scroll(10), 30);
        st.prev_match(10);
        assert_eq!(st.effective_scroll(10), 5);
        // Wraps backwards from the first match to the last.
        st.prev_match(10);
        assert_eq!(st.effective_scroll(10), 30);
    }

    /// `cycle_filter` makes three state changes; only the filter index was ever asserted, so
    /// deleting the other two left every test green while Tab stranded a paused viewport on an
    /// index from the previous filter's list (/review's testing pass — negative-control gap).
    #[test]
    fn cycle_filter_rearms_follow_and_drops_the_stale_scroll_and_match_cursor() {
        let mut st = LogsState::default();
        st.enable(vec!["cos".to_string(), "agent".to_string()], "p".to_string());
        let mut batch: Vec<LogLine> = (0..100).map(|i| line("cos", &format!("l{i}"))).collect();
        batch[5] = line("cos", "needle");
        st.push_lines(batch);
        st.search_query = Input::new("needle".to_string());
        st.scroll_to_top();
        st.next_match(10);
        // Pre-state must be dirty or this proves nothing.
        assert!(!st.follow);
        assert_eq!(st.match_cursor, Some(0));
        assert_eq!(st.scroll, 5);

        st.cycle_filter();
        assert!(st.follow, "Tab re-arms follow");
        assert_eq!(st.scroll, 0, "an index into the previous filter's list is meaningless");
        assert_eq!(st.match_cursor, None, "match positions changed meaning");
    }

    /// A manual move must invalidate the n/N cursor so the next jump continues from where the
    /// operator is looking, not from the last jump.
    #[test]
    fn a_manual_scroll_reenters_n_from_the_viewport_not_from_the_last_jump() {
        let mut st = LogsState::default();
        let mut batch: Vec<LogLine> = (0..60).map(|i| line("cos", &format!("l{i}"))).collect();
        batch[5]  = line("cos", "needle a");
        batch[25] = line("cos", "needle b");
        batch[45] = line("cos", "needle c");
        st.push_lines(batch);
        st.search_query = Input::new("needle".to_string());
        st.scroll_to_top();
        st.next_match(10);
        assert_eq!(st.effective_scroll(10), 5);
        assert_eq!(st.match_cursor, Some(0));

        st.scroll_by(10, 30); // operator scrolls past match b by hand
        assert_eq!(st.match_cursor, None, "a manual move invalidates the cursor");
        st.next_match(10);
        assert_eq!(
            st.effective_scroll(10),
            45,
            "n re-enters from the viewport (match c), not cursor+1 (match b)"
        );
    }

    /// Ring eviction renumbers the filtered list, so a cursor into it is stale — keeping it
    /// made `n` skip the match that just scrolled into view (/review's red-team + maintainability
    /// passes, independently).
    #[test]
    fn ring_eviction_invalidates_the_match_cursor() {
        let mut st = LogsState::default();
        let mut batch: Vec<LogLine> =
            (0..LOG_RING_CAP).map(|i| line("cos", &format!("l{i}"))).collect();
        batch[100] = line("cos", "needle");
        st.push_lines(batch);
        st.search_query = Input::new("needle".to_string());
        st.scroll_to_top();
        st.next_match(10);
        assert_eq!(st.match_cursor, Some(0));
        st.push_lines(vec![line("cos", "fresh")]); // forces one eviction
        assert_eq!(st.match_cursor, None);
    }

    #[test]
    fn a_bare_timestamp_with_no_payload_is_an_empty_line_not_a_line_of_text() {
        let k = known();
        let l = parse_compose_line("cos-1  | 2026-07-27T00:00:00.000000000Z", &k);
        assert_eq!(l.ts.as_deref(), Some("2026-07-27T00:00:00.000000000Z"));
        assert_eq!(l.text, "", "the stamp must not be rendered as the payload");
        let l = parse_compose_line("cos-1  | 2026-07-27T00:00:00.000000000Z ", &k);
        assert_eq!(l.ts.as_deref(), Some("2026-07-27T00:00:00.000000000Z"));
        assert_eq!(l.text, "");
    }

    #[test]
    fn degenerate_compose_lines_parse_predictably() {
        let k = known();
        // Prefix only.
        let l = parse_compose_line("cos-1  |", &k);
        assert_eq!((l.service.as_deref(), l.ts, l.text.as_str()), (Some("cos"), None, ""));
        // Empty prefix → no service attribution at all.
        assert_eq!(parse_compose_line("| orphan", &k).service, None);
        // A CR left by CRLF output is stripped by the reader before parsing.
        let l = parse_compose_line("cos-1  | 2026-07-27T00:00:00Z hi\r".trim_end_matches(['\r', '\n']), &k);
        assert_eq!(l.text, "hi");
        // Multibyte payloads clip on a char boundary, never mid-char.
        let emoji = "🚀".repeat(MAX_LOG_LINE_CHARS + 5);
        assert_eq!(clip_payload(&emoji).chars().count(), MAX_LOG_LINE_CHARS + 1);
    }

    #[test]
    fn search_only_matches_the_part_of_the_payload_that_is_rendered() {
        let st =
            LogsState { search_query: Input::new("needle".to_string()), ..Default::default() };
        let hidden = format!("{}needle", "x".repeat(MAX_LOG_LINE_CHARS + 10));
        assert!(
            !st.matches_search(&line("cos", &hidden)),
            "a match past the render clip is invisible, so it must not count"
        );
        let visible = format!("needle{}", "x".repeat(MAX_LOG_LINE_CHARS));
        assert!(st.matches_search(&line("cos", &visible)));
    }

    #[test]
    fn dropped_lines_accumulate() {
        let mut st = LogsState::default();
        st.note_dropped(7);
        st.note_dropped(3);
        assert_eq!(st.dropped, 10);
    }

    #[test]
    fn relative_and_absolute_timestamp_formatting() {
        let ts = "2026-07-27T12:00:00.000000000Z";
        let now = chrono::DateTime::parse_from_rfc3339(ts).unwrap().timestamp();
        assert_eq!(format_ts(Some(ts), false, now + 3), "3s");
        assert_eq!(format_ts(Some(ts), false, now + 125), "2m");
        assert_eq!(format_ts(Some(ts), false, now + 7200), "2h");
        assert_eq!(format_ts(Some(ts), false, now + 200_000), "2d");
        assert_eq!(format_ts(Some(ts), true, now), "12:00:00");
        assert_eq!(format_ts(None, false, now), "");
        // Unparseable stamps degrade to a prefix, never to a fabricated age.
        assert_eq!(format_ts(Some("not-a-stamp-at-all"), false, now), "not-a-st");
    }

    #[test]
    fn viewport_rows_accounts_for_header_footer_and_borders() {
        assert_eq!(logs_viewport_rows(40), 36);
        assert_eq!(logs_viewport_rows(4), 1);
        assert_eq!(logs_viewport_rows(0), 1);
    }

    #[test]
    fn long_payloads_are_clipped_with_an_ellipsis() {
        let long = "x".repeat(MAX_LOG_LINE_CHARS + 20);
        let out  = clip_payload(&long);
        assert_eq!(out.chars().count(), MAX_LOG_LINE_CHARS + 1);
        assert!(out.ends_with('…'));
        assert_eq!(clip_payload("short"), "short");
    }
}
