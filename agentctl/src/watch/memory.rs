use std::path::Path;
use std::fs;

/// One long-term or KB memory entry decoded from a FUSE file.
#[derive(Debug, Clone, Default)]
pub struct MemoryEntry {
    pub key:        String,
    /// Full content text.  Truncation for display happens in views.rs.
    pub content:    String,
    /// Formatted provenance: "turn N | agent <id> | <ts>", or "" on parse failure.
    pub provenance: String,
    /// "log", "scratch", "canon" for KB entries; empty for long-term entries.
    pub class:      String,
}

/// Per-agent memory snapshot (short-term + long-term).
#[derive(Debug, Clone, Default)]
pub struct AgentMemory {
    /// Lines from the short_term FUSE file, sentinel filtered.
    pub short_term:          Vec<String>,
    pub long_term:           Vec<MemoryEntry>,
    /// True when the long_term/ directory has more keys than MAX_DISPLAY_ENTRIES.
    pub long_term_truncated: bool,
}

/// One shared KB segment and its entries.
#[derive(Debug, Clone, Default)]
pub struct KbSegment {
    pub name:      String,
    /// "log", "scratch", "canon" — from the first entry's `class` field.
    pub class:     String,
    pub entries:   Vec<MemoryEntry>,
    /// True when the segment directory has more keys than MAX_DISPLAY_ENTRIES.
    pub truncated: bool,
}

/// Maximum entries fetched per long_term/ or KB segment when no search is active.
/// Bounds FUSE I/O to MAX_DISPLAY_ENTRIES × ~8 KiB per tick.
pub const MAX_DISPLAY_ENTRIES: usize = 20;

/// Maximum entries fetched when a search query is active.
/// Matches the FUSE MAX_DIR_KEYS cap so search covers the full store.
pub const MAX_SEARCH_ENTRIES: usize = 100;

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse a raw FUSE file value (trimmed JSON string) into a `MemoryEntry`.
///
/// On JSON parse failure: content = raw.trim(), provenance = "".
pub fn parse_entry(key: &str, raw: &str) -> MemoryEntry {
    let raw_trimmed = raw.trim();
    let key = sanitize_str(key);

    let v: serde_json::Value = match serde_json::from_str(raw_trimmed) {
        Ok(v) => v,
        Err(_) => {
            return MemoryEntry {
                key,
                content:    sanitize_str(raw_trimmed),
                provenance: String::new(),
                class:      String::new(),
            };
        }
    };

    let content    = v["content"].as_str().unwrap_or(raw_trimmed).to_string();
    let content    = sanitize_str(&content);
    let class      = v["class"].as_str().unwrap_or("").to_string();
    let provenance = format_provenance(&v["provenance"]);

    MemoryEntry { key, content, provenance, class }
}

/// Parse the `short_term` FUSE file content into individual items.
///
/// Splits on newlines, drops empty lines and the `"(empty)"` sentinel.
pub fn parse_short_term(text: &str) -> Vec<String> {
    text.split('\n')
        .filter(|line| !line.is_empty() && *line != "(empty)")
        .map(sanitize_str)
        .collect()
}

// ── Readers ──────────────────────────────────────────────────────────────────

/// Read `/agents/<id>/memory/short_term` and `/agents/<id>/memory/long_term/`.
///
/// Returns `None` if the `memory/` directory does not exist (Phase 5 absent for
/// this agent, or agent has no memory dir).  Returns `Some(empty AgentMemory)`
/// when the dir exists but has no entries.
///
/// When `search_query` is non-empty, fetches up to `MAX_SEARCH_ENTRIES` entries
/// so the substring filter can cover the full store.
pub fn read_agent_memory(agents_dir: &Path, id: &str, search_query: &str) -> Option<AgentMemory> {
    debug_assert!(!id.contains('/') && id != "..", "agent id must be a simple filename");

    let mem_dir = agents_dir.join(id).join("memory");
    if !mem_dir.is_dir() {
        return None;
    }

    let short_term = match fs::read_to_string(mem_dir.join("short_term")) {
        Ok(text) => parse_short_term(&text),
        Err(_)   => vec![],
    };

    let lt_dir = mem_dir.join("long_term");
    let limit  = if search_query.is_empty() { MAX_DISPLAY_ENTRIES } else { MAX_SEARCH_ENTRIES };
    let (long_term, long_term_truncated) = read_entries_from_dir(&lt_dir, limit);

    Some(AgentMemory { short_term, long_term, long_term_truncated })
}

/// Read `/agents/kb/<segment>/<key>` for every segment under `/agents/kb/`.
///
/// Returns an empty `Vec` when `/agents/kb/` does not exist.
/// When `search_query` is non-empty, fetches up to `MAX_SEARCH_ENTRIES` per segment.
pub fn read_kb_segments(agents_dir: &Path, search_query: &str) -> Vec<KbSegment> {
    let kb_dir = agents_dir.join("kb");
    if !kb_dir.is_dir() {
        return vec![];
    }

    let limit = if search_query.is_empty() { MAX_DISPLAY_ENTRIES } else { MAX_SEARCH_ENTRIES };

    let mut dir_entries: Vec<_> = match fs::read_dir(&kb_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return vec![],
    };
    dir_entries.sort_by_key(|e| e.file_name());

    let mut segments = vec![];
    for entry in dir_entries {
        let seg_path = entry.path();
        if !seg_path.is_dir() {
            continue;
        }
        let seg_name = entry.file_name().to_string_lossy().to_string();
        if seg_name.starts_with('.') {
            continue;
        }
        let (entries, truncated) = read_entries_from_dir(&seg_path, limit);
        let class = entries.first().map(|e| e.class.clone()).unwrap_or_default();
        segments.push(KbSegment { name: seg_name, class, entries, truncated });
    }
    segments
}

/// Read at most `limit` file entries from `dir`, sorted lexicographically by key.
///
/// Unreadable files are silently skipped.  Returns `(entries, truncated)` where
/// `truncated` is true when the directory contained more than `limit` files.
fn read_entries_from_dir(dir: &Path, limit: usize) -> (Vec<MemoryEntry>, bool) {
    if !dir.is_dir() {
        return (vec![], false);
    }
    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return (vec![], false),
    };
    entries.sort_by_key(|e| e.file_name());

    let total     = entries.len();
    let truncated = total > limit;

    let result = entries
        .into_iter()
        .take(limit)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let key = entry.file_name().to_string_lossy().to_string();
            match fs::read_to_string(&path) {
                Ok(raw) => Some(parse_entry(&key, &raw)),
                Err(_)  => None,
            }
        })
        .collect();

    (result, truncated)
}

// ── Filters ──────────────────────────────────────────────────────────────────

/// Case-insensitive substring filter on `key`, full `content`, and `provenance`.
///
/// An empty query returns all entries without cloning.
pub fn filter_entries<'a>(entries: &'a [MemoryEntry], query: &str) -> Vec<&'a MemoryEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }
    let q = query.to_lowercase();
    entries
        .iter()
        .filter(|e| {
            e.key.to_lowercase().contains(&q)
                || e.content.to_lowercase().contains(&q)
                || e.provenance.to_lowercase().contains(&q)
        })
        .collect()
}

/// Case-insensitive substring filter for short-term preview strings.
///
/// An empty query returns all items without cloning.
pub fn filter_short_term<'a>(items: &'a [String], query: &str) -> Vec<&'a String> {
    if query.is_empty() {
        return items.iter().collect();
    }
    let q = query.to_lowercase();
    items.iter().filter(|s| s.to_lowercase().contains(&q)).collect()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Format a provenance JSON object as a human-readable string.
fn format_provenance(prov: &serde_json::Value) -> String {
    if prov.is_null() || !prov.is_object() {
        return String::new();
    }
    let mut parts = Vec::new();
    if let Some(turn) = prov["turn"].as_u64() {
        parts.push(format!("turn {turn}"));
    }
    if let Some(agent) = prov["agent_id"].as_str() {
        if !agent.is_empty() {
            parts.push(format!("agent {agent}"));
        }
    }
    // ts: nanosecond u64 (long-term) or RFC3339 string (KB)
    if let Some(ts_ns) = prov["ts"].as_u64() {
        parts.push(format_unix_secs(ts_ns / 1_000_000_000));
    } else if let Some(ts_str) = prov["ts"].as_str() {
        parts.push(strip_subsecond(ts_str));
    }
    parts.join(" | ")
}

/// Strip sub-second precision from an RFC3339 timestamp.
fn strip_subsecond(ts: &str) -> String {
    if let Some(dot) = ts.find('.') {
        if ts[dot..].contains('Z') {
            return format!("{}Z", &ts[..dot]);
        }
    }
    ts.to_string()
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` using chrono.
fn format_unix_secs(secs: u64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_opt(secs as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("{secs}s"))
}

/// Strip ASCII control characters (< 0x20, except tab) from a string.
fn sanitize_str(s: &str) -> String {
    s.chars().filter(|&c| c >= ' ' || c == '\t').collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmpdir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_file(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    fn make_lt_entry(content: &str, ts_ns: u64) -> String {
        format!(
            r#"{{"content":"{content}","provenance":{{"agent_id":"a1","turn":1,"ts":{ts_ns},"task_fp":"0x1"}}}}"#
        )
    }

    fn make_kb_entry(content: &str, class: &str, ts: &str) -> String {
        format!(
            r#"{{"content":"{content}","class":"{class}","version":1,"provenance":{{"agent_id":"a1","turn":1,"ts":"{ts}","task_fp":"0x1"}}}}"#
        )
    }

    // ── parse_entry ──────────────────────────────────────────────────────────

    #[test]
    fn parse_entry_invalid_json_returns_raw_verbatim() {
        let e = parse_entry("key1", "not json at all");
        assert_eq!(e.key, "key1");
        assert_eq!(e.content, "not json at all");
        assert_eq!(e.provenance, "");
    }

    #[test]
    fn parse_entry_strips_control_chars() {
        // \x1b is an escape — in the FUSE file it is the literal char
        let raw_with_esc = "{\"content\":\"hello\x1bworld\"}";
        let e = parse_entry("k", raw_with_esc);
        assert!(!e.content.contains('\x1b'), "ESC must be stripped");
    }

    #[test]
    fn parse_entry_parses_content_field() {
        let raw = r#"{"content":"hello world","provenance":{"agent_id":"","turn":0,"ts":0}}"#;
        let e = parse_entry("k", raw);
        assert_eq!(e.content, "hello world");
    }

    // ── parse_short_term ─────────────────────────────────────────────────────

    #[test]
    fn parse_short_term_empty_sentinel_returns_empty() {
        let items = parse_short_term("(empty)\n");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_short_term_parses_lines() {
        let items = parse_short_term("line one\nline two\n");
        assert_eq!(items, vec!["line one", "line two"]);
    }

    #[test]
    fn parse_short_term_skips_empty_lines() {
        let items = parse_short_term("a\n\nb\n");
        assert_eq!(items, vec!["a", "b"]);
    }

    // ── filter_entries ───────────────────────────────────────────────────────

    fn make_entry(key: &str, content: &str, prov: &str) -> MemoryEntry {
        MemoryEntry {
            key:        key.to_string(),
            content:    content.to_string(),
            provenance: prov.to_string(),
            class:      String::new(),
        }
    }

    #[test]
    fn filter_entries_empty_query_returns_all() {
        let entries = vec![make_entry("k1", "alpha", ""), make_entry("k2", "beta", "")];
        let r = filter_entries(&entries, "");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn filter_entries_case_insensitive_key_match() {
        let entries = vec![make_entry("MyKey", "content", ""), make_entry("other", "stuff", "")];
        let r = filter_entries(&entries, "mykey");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key, "MyKey");
    }

    #[test]
    fn filter_entries_content_match() {
        let entries = vec![make_entry("k", "the quick brown fox", "")];
        let r = filter_entries(&entries, "QUICK");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn filter_entries_provenance_match() {
        let entries = vec![make_entry("k", "stuff", "turn 5 | agent scout-0 | 2026-01-01Z")];
        let r = filter_entries(&entries, "scout-0");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn filter_entries_match_after_char_200() {
        let long_content = "a".repeat(300) + "NEEDLE";
        let entries = vec![make_entry("k", &long_content, "")];
        let r = filter_entries(&entries, "needle");
        assert_eq!(r.len(), 1, "filter must search full content, not just first 200 chars");
    }

    // ── filter_short_term ────────────────────────────────────────────────────

    #[test]
    fn filter_short_term_empty_query_returns_all() {
        let items: Vec<String> = vec!["foo".into(), "bar".into()];
        let r = filter_short_term(&items, "");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn filter_short_term_case_insensitive_match() {
        let items: Vec<String> = vec!["Hello World".into(), "goodbye".into()];
        let r = filter_short_term(&items, "HELLO");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], "Hello World");
    }

    #[test]
    fn filter_short_term_no_match_returns_empty() {
        let items: Vec<String> = vec!["alpha".into(), "beta".into()];
        let r = filter_short_term(&items, "gamma");
        assert!(r.is_empty());
    }

    // ── read_agent_memory ────────────────────────────────────────────────────

    #[test]
    fn read_agent_memory_returns_none_when_dir_missing() {
        let d = tmpdir();
        let result = read_agent_memory(d.path(), "nonexistent-agent", "");
        assert!(result.is_none());
    }

    #[test]
    fn read_agent_memory_parses_short_term_lines() {
        let d = tmpdir();
        let mem_dir = d.path().join("agent-1").join("memory");
        fs::create_dir_all(&mem_dir).unwrap();
        write_file(&mem_dir, "short_term", "fact one\nfact two\n");
        let result = read_agent_memory(d.path(), "agent-1", "").unwrap();
        assert_eq!(result.short_term, vec!["fact one", "fact two"]);
    }

    #[test]
    fn read_agent_memory_handles_empty_sentinel() {
        let d = tmpdir();
        let mem_dir = d.path().join("agent-1").join("memory");
        fs::create_dir_all(&mem_dir).unwrap();
        write_file(&mem_dir, "short_term", "(empty)\n");
        let result = read_agent_memory(d.path(), "agent-1", "").unwrap();
        assert!(result.short_term.is_empty());
    }

    #[test]
    fn read_agent_memory_parses_long_term_json_entry_content_field() {
        let d = tmpdir();
        let lt_dir = d.path().join("a").join("memory").join("long_term");
        fs::create_dir_all(&lt_dir).unwrap();
        write_file(&d.path().join("a").join("memory"), "short_term", "(empty)\n");
        write_file(&lt_dir, "key-abc", &make_lt_entry("my note", 1_000_000_000_000_000_000));
        let result = read_agent_memory(d.path(), "a", "").unwrap();
        assert_eq!(result.long_term.len(), 1);
        assert_eq!(result.long_term[0].key, "key-abc");
        assert_eq!(result.long_term[0].content, "my note");
    }

    #[test]
    fn read_agent_memory_handles_non_json_entry_verbatim() {
        let d = tmpdir();
        let lt_dir = d.path().join("a").join("memory").join("long_term");
        fs::create_dir_all(&lt_dir).unwrap();
        write_file(&d.path().join("a").join("memory"), "short_term", "(empty)\n");
        write_file(&lt_dir, "raw-key", "not json content\n");
        let result = read_agent_memory(d.path(), "a", "").unwrap();
        assert_eq!(result.long_term[0].content, "not json content");
    }

    #[test]
    fn read_agent_memory_long_term_dir_missing_short_term_present() {
        let d = tmpdir();
        let mem_dir = d.path().join("a").join("memory");
        fs::create_dir_all(&mem_dir).unwrap();
        write_file(&mem_dir, "short_term", "a note\n");
        // no long_term/ directory
        let result = read_agent_memory(d.path(), "a", "").unwrap();
        assert_eq!(result.short_term, vec!["a note"]);
        assert!(result.long_term.is_empty());
    }

    #[test]
    fn read_agent_memory_handles_read_error_gracefully() {
        // Unreadable entries are skipped — tested via non-existent path (no-op).
        let d = tmpdir();
        let lt_dir = d.path().join("a").join("memory").join("long_term");
        fs::create_dir_all(&lt_dir).unwrap();
        write_file(&d.path().join("a").join("memory"), "short_term", "(empty)\n");
        // write a valid entry alongside (reader should not panic)
        write_file(&lt_dir, "good-key", &make_lt_entry("ok", 0));
        let result = read_agent_memory(d.path(), "a", "").unwrap();
        assert_eq!(result.long_term.len(), 1);
    }

    #[test]
    fn read_agent_memory_truncates_at_max_display_entries() {
        let d = tmpdir();
        let lt_dir = d.path().join("a").join("memory").join("long_term");
        fs::create_dir_all(&lt_dir).unwrap();
        write_file(&d.path().join("a").join("memory"), "short_term", "(empty)\n");
        for i in 0..(MAX_DISPLAY_ENTRIES + 1) {
            write_file(&lt_dir, &format!("key-{i:04}"), &make_lt_entry("x", 0));
        }
        let result = read_agent_memory(d.path(), "a", "").unwrap();
        assert_eq!(result.long_term.len(), MAX_DISPLAY_ENTRIES);
        assert!(result.long_term_truncated);
    }

    #[test]
    fn read_agent_memory_fetches_100_when_search_query_set() {
        let d = tmpdir();
        let lt_dir = d.path().join("a").join("memory").join("long_term");
        fs::create_dir_all(&lt_dir).unwrap();
        write_file(&d.path().join("a").join("memory"), "short_term", "(empty)\n");
        // write MAX_DISPLAY_ENTRIES+5 entries — search should fetch more than display limit
        for i in 0..(MAX_DISPLAY_ENTRIES + 5) {
            write_file(&lt_dir, &format!("key-{i:04}"), &make_lt_entry("needle", 0));
        }
        let result = read_agent_memory(d.path(), "a", "needle").unwrap();
        assert_eq!(result.long_term.len(), MAX_DISPLAY_ENTRIES + 5,
            "search mode should fetch all entries up to MAX_SEARCH_ENTRIES");
        assert!(!result.long_term_truncated, "no truncation when below MAX_SEARCH_ENTRIES");
    }

    // ── read_kb_segments ─────────────────────────────────────────────────────

    #[test]
    fn read_kb_segments_returns_empty_when_kb_missing() {
        let d = tmpdir();
        let segs = read_kb_segments(d.path(), "");
        assert!(segs.is_empty());
    }

    #[test]
    fn read_kb_segments_parses_entry_with_provenance() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("project");
        fs::create_dir_all(&seg_dir).unwrap();
        write_file(&seg_dir, "doc-1", &make_kb_entry("KB note", "log", "2026-01-01T00:00:00Z"));
        let segs = read_kb_segments(d.path(), "");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].name, "project");
        assert_eq!(segs[0].entries[0].content, "KB note");
        assert_eq!(segs[0].class, "log");
    }

    #[test]
    fn read_kb_segments_handles_rfc3339_ts_in_kb_entry() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("seg");
        fs::create_dir_all(&seg_dir).unwrap();
        write_file(&seg_dir, "k1", &make_kb_entry("data", "scratch", "2026-06-18T03:00:00Z"));
        let segs = read_kb_segments(d.path(), "");
        assert!(segs[0].entries[0].provenance.contains("2026-06-18T03:00:00Z"),
            "RFC3339 ts must appear in provenance");
    }

    #[test]
    fn read_kb_segments_handles_nanosecond_u64_ts() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("seg");
        fs::create_dir_all(&seg_dir).unwrap();
        // 1_000_000_000 ns = 1s since epoch → "1970-01-01T00:00:01Z"
        let raw = r#"{"content":"x","class":"log","provenance":{"ts":1000000000,"agent_id":"a","turn":1}}"#;
        write_file(&seg_dir, "k1", raw);
        let segs = read_kb_segments(d.path(), "");
        assert!(segs[0].entries[0].provenance.contains("1970-01-01T00:00:01Z"),
            "nanosecond u64 ts must be formatted as RFC3339");
    }

    #[test]
    fn read_kb_segments_truncates_at_max_display_entries() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("s");
        fs::create_dir_all(&seg_dir).unwrap();
        for i in 0..(MAX_DISPLAY_ENTRIES + 1) {
            write_file(&seg_dir, &format!("k-{i:04}"), &make_kb_entry("v", "log", "2026-01-01T00:00:00Z"));
        }
        let segs = read_kb_segments(d.path(), "");
        assert_eq!(segs[0].entries.len(), MAX_DISPLAY_ENTRIES);
        assert!(segs[0].truncated);
    }

    #[test]
    fn read_kb_segments_shows_truncation_sentinel() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("seg");
        fs::create_dir_all(&seg_dir).unwrap();
        for i in 0..(MAX_DISPLAY_ENTRIES + 3) {
            write_file(&seg_dir, &format!("k{i:04}"), &make_kb_entry("v", "scratch", "2026-01-01T00:00:00Z"));
        }
        let segs = read_kb_segments(d.path(), "");
        assert!(segs[0].truncated, "truncated flag must be set when entries exceed limit");
    }

    #[test]
    fn read_kb_segments_fetches_100_when_search_query_set() {
        let d = tmpdir();
        let seg_dir = d.path().join("kb").join("seg");
        fs::create_dir_all(&seg_dir).unwrap();
        for i in 0..(MAX_DISPLAY_ENTRIES + 5) {
            write_file(&seg_dir, &format!("k{i:04}"), &make_kb_entry("needle", "log", "2026-01-01T00:00:00Z"));
        }
        let segs = read_kb_segments(d.path(), "needle");
        assert_eq!(segs[0].entries.len(), MAX_DISPLAY_ENTRIES + 5,
            "search mode should fetch all entries up to MAX_SEARCH_ENTRIES");
    }

    // ── format_provenance edge cases ─────────────────────────────────────────

    #[test]
    fn format_provenance_null_returns_empty() {
        assert!(format_provenance(&serde_json::Value::Null).is_empty(),
            "null provenance must return empty string");
    }

    #[test]
    fn format_provenance_non_object_returns_empty() {
        assert!(format_provenance(&serde_json::json!(42)).is_empty(),
            "non-object provenance must return empty string");
    }

    #[test]
    fn format_provenance_empty_agent_id_is_omitted() {
        let prov = serde_json::json!({"turn": 2, "agent_id": "", "ts": 0});
        let result = format_provenance(&prov);
        assert!(result.contains("turn 2"), "turn must appear in result");
        assert!(!result.contains("agent"), "empty agent_id must be omitted");
    }

    #[test]
    fn format_provenance_absent_ts_omits_timestamp() {
        let prov = serde_json::json!({"turn": 3, "agent_id": "scout"});
        let result = format_provenance(&prov);
        assert!(result.contains("turn 3"), "turn must appear");
        assert!(result.contains("agent scout"), "agent must appear");
        assert!(!result.contains("1970"), "absent ts must not produce a fallback timestamp");
    }

    // ── strip_subsecond edge cases ────────────────────────────────────────────

    #[test]
    fn strip_subsecond_removes_fractional_seconds() {
        assert_eq!(
            strip_subsecond("2026-06-01T12:00:00.123456Z"),
            "2026-06-01T12:00:00Z",
        );
    }

    #[test]
    fn strip_subsecond_passthrough_when_no_dot() {
        let ts = "2026-06-01T12:00:00Z";
        assert_eq!(strip_subsecond(ts), ts, "timestamp without dot must pass through unchanged");
    }

    // ── format_unix_secs edge cases ───────────────────────────────────────────

    #[test]
    fn format_unix_secs_out_of_range_returns_fallback() {
        // i64::MAX / 1 overflows i64 when passed as u64 — chrono returns None → fallback.
        let big: u64 = i64::MAX as u64 + 1;
        let result = format_unix_secs(big);
        assert!(!result.is_empty(), "out-of-range epoch must not produce empty string");
        // The fallback branch produces "{secs}s".
        assert!(result.ends_with('s'), "out-of-range epoch must end with 's' fallback; got: {result}");
    }

    // ── read_kb_segments filesystem edge cases ────────────────────────────────

    #[test]
    fn read_kb_segments_non_dir_file_in_kb_is_skipped() {
        let d = tmpdir();
        let kb_dir = d.path().join("kb");
        fs::create_dir_all(&kb_dir).unwrap();
        write_file(&kb_dir, "not-a-segment", "some random content");
        let segs = read_kb_segments(d.path(), "");
        assert!(segs.is_empty(), "regular files in kb/ must be skipped (only dirs are segments)");
    }

    #[test]
    fn read_kb_segments_dot_prefixed_segment_is_skipped() {
        let d = tmpdir();
        let hidden = d.path().join("kb").join(".hidden-seg");
        fs::create_dir_all(&hidden).unwrap();
        write_file(&hidden, "k1", &make_kb_entry("secret", "scratch", "2026-01-01T00:00:00Z"));
        let segs = read_kb_segments(d.path(), "");
        assert!(segs.is_empty(), "dot-prefixed segment dirs must be skipped");
    }

    // ── read_entries_from_dir: subdir entry skipped ───────────────────────────

    #[test]
    fn read_entries_from_dir_subdir_is_skipped() {
        let d = tmpdir();
        let lt_dir = d.path().join("long_term");
        let sub    = lt_dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        write_file(&lt_dir, "real-key", r#"{"content":"value","provenance":{}}"#);
        let (entries, _) = read_entries_from_dir(&lt_dir, 100);
        assert_eq!(entries.len(), 1, "subdir must be skipped; only 1 file entry expected");
        assert_eq!(entries[0].key, "real-key");
    }
}
