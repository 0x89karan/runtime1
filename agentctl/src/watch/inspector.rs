use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// Maximum lines to load from the flight log.
pub const MAX_INSPECTOR_LINES: usize = 500;

/// Last 512 KB of flight.jsonl to scan.
const FLIGHT_TAIL_BYTES: u64 = 512 * 1024;

/// Which events to show in the Inspector view.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum InspectorFilter {
    #[default]
    All,
    Errors,
    Sandbox,
    CapDenied,
    Egress,
}

impl InspectorFilter {
    pub fn label(&self) -> &'static str {
        match self {
            InspectorFilter::All       => "All",
            InspectorFilter::Errors    => "Errors",
            InspectorFilter::Sandbox   => "Sandbox",
            InspectorFilter::CapDenied => "CapDenied",
            InspectorFilter::Egress    => "Egress",
        }
    }

    pub fn next(&self) -> InspectorFilter {
        match self {
            InspectorFilter::All       => InspectorFilter::Errors,
            InspectorFilter::Errors    => InspectorFilter::Sandbox,
            InspectorFilter::Sandbox   => InspectorFilter::CapDenied,
            InspectorFilter::CapDenied => InspectorFilter::Egress,
            InspectorFilter::Egress    => InspectorFilter::All,
        }
    }

    pub fn matches(&self, line: &str, search: &str) -> bool {
        let base = match self {
            InspectorFilter::All       => true,
            InspectorFilter::Errors    => line.contains("\"kind\":\"tool_error\"")
                || line.contains("\"kind\":\"inference_error\"")
                || line.contains("\"kind\":\"agent_failed\""),
            InspectorFilter::Sandbox   => line.contains("\"kind\":\"sandbox_applied\"")
                || line.contains("\"kind\":\"sandbox_skipped\""),
            InspectorFilter::CapDenied => line.contains("\"kind\":\"capability_denied\""),
            InspectorFilter::Egress    => line.contains("\"kind\":\"egress_brokered\"")
                || line.contains("\"kind\":\"egress_denied\"")
                || line.contains("\"kind\":\"action_receipt_emitted\""),
        };
        base && (search.is_empty() || line.contains(search))
    }
}

/// State for the flight-log Inspector view.
#[derive(Debug, Default)]
pub struct InspectorState {
    /// All lines currently displayed (post-filter).
    pub lines:          Vec<String>,
    /// Raw tail lines from the log (pre-filter).
    pub raw_lines:      Vec<String>,
    /// Active filter.
    pub filter:         InspectorFilter,
    /// Current search query.
    pub search_query:   String,
    /// True while user is typing a search query.
    pub search_active:  bool,
    /// Vertical scroll offset.
    pub scroll:         usize,
    /// True once the log has been loaded (load-once model).
    pub loaded:         bool,
    /// Timestamp string of when the log was last loaded.
    pub load_time:      String,
}

impl InspectorState {
    /// Load (or reload) the flight log. Resets scroll and sets `loaded = true`.
    pub fn load(&mut self, log_path: Option<&Path>) {
        self.raw_lines = read_flight_tail(log_path);
        self.loaded    = true;
        self.scroll    = 0;
        // Use a simple formatted timestamp placeholder (no std::time in no_std, but
        // we're in normal std — use SystemTime).
        self.load_time = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let h = (secs % 86400) / 3600;
            let m = (secs % 3600)  / 60;
            let s =  secs % 60;
            format!("{h:02}:{m:02}:{s:02} UTC")
        };
        self.rebuild_view();
    }

    /// Rebuild `lines` from `raw_lines` applying the current filter + search.
    pub fn rebuild_view(&mut self) {
        self.lines = self.raw_lines
            .iter()
            .filter(|l| self.filter.matches(l, &self.search_query))
            .cloned()
            .collect();
        // Cap scroll to new line count.
        let max = self.lines.len().saturating_sub(1);
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

/// Read the last `FLIGHT_TAIL_BYTES` of the flight log and return up to
/// `MAX_INSPECTOR_LINES` lines (most recent first, then limited).
fn read_flight_tail(log_path: Option<&Path>) -> Vec<String> {
    let path = log_path.unwrap_or_else(|| Path::new("flight.jsonl"));
    let mut file = match std::fs::File::open(path) {
        Ok(f)  => f,
        Err(_) => return vec![],
    };

    let file_size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start     = file_size.saturating_sub(FLIGHT_TAIL_BYTES);
    let _ = file.seek(SeekFrom::Start(start));

    let mut raw = Vec::new();
    let _ = file.read_to_end(&mut raw);
    let buf = String::from_utf8_lossy(&raw).into_owned();

    let buf_start = if start > 0 {
        buf.find('\n').map(|i| i + 1).unwrap_or(0)
    } else {
        0
    };

    let all: Vec<&str> = buf[buf_start..]
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    all.iter()
        .rev()
        .take(MAX_INSPECTOR_LINES)
        .rev()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_filter_all_matches_any_line() {
        let f = InspectorFilter::All;
        assert!(f.matches(r#"{"kind":"some_event"}"#, ""));
        assert!(f.matches(r#"{"kind":"tool_error"}"#, ""));
    }

    #[test]
    fn inspector_filter_errors_matches_error_events() {
        let f = InspectorFilter::Errors;
        assert!(f.matches(r#"{"kind":"tool_error","agent":"a"}"#, ""));
        assert!(f.matches(r#"{"kind":"inference_error"}"#, ""));
        assert!(f.matches(r#"{"kind":"agent_failed"}"#, ""));
        assert!(!f.matches(r#"{"kind":"tool_result"}"#, ""));
    }

    #[test]
    fn inspector_filter_sandbox_matches_sandbox_events() {
        let f = InspectorFilter::Sandbox;
        assert!(f.matches(r#"{"kind":"sandbox_applied"}"#, ""));
        assert!(f.matches(r#"{"kind":"sandbox_skipped"}"#, ""));
        assert!(!f.matches(r#"{"kind":"tool_error"}"#, ""));
    }

    #[test]
    fn inspector_filter_cap_denied_matches_only_cap_denied() {
        let f = InspectorFilter::CapDenied;
        assert!(f.matches(r#"{"kind":"capability_denied"}"#, ""));
        assert!(!f.matches(r#"{"kind":"tool_error"}"#, ""));
    }

    #[test]
    fn inspector_filter_search_further_narrows() {
        let f = InspectorFilter::All;
        let line = r#"{"kind":"tool_result","agent":"scout"}"#;
        assert!(f.matches(line, "scout"));
        assert!(!f.matches(line, "coordinator"));
    }

    #[test]
    fn inspector_filter_egress_matches_egress_events() {
        let f = InspectorFilter::Egress;
        assert!(f.matches(r#"{"kind":"egress_brokered","agent":"scout"}"#, ""));
        assert!(f.matches(r#"{"kind":"egress_denied","agent":"scout"}"#, ""));
        assert!(f.matches(r#"{"kind":"action_receipt_emitted","agent":"scout"}"#, ""));
        assert!(!f.matches(r#"{"kind":"capability_denied"}"#, ""));
        assert!(!f.matches(r#"{"kind":"tool_error"}"#, ""));
    }

    #[test]
    fn inspector_filter_cycles_all_to_errors() {
        assert_eq!(InspectorFilter::All.next(), InspectorFilter::Errors);
    }

    #[test]
    fn inspector_filter_cycles_cap_denied_to_egress() {
        assert_eq!(InspectorFilter::CapDenied.next(), InspectorFilter::Egress);
    }

    #[test]
    fn inspector_filter_cycles_egress_back_to_all() {
        assert_eq!(InspectorFilter::Egress.next(), InspectorFilter::All);
    }

    #[test]
    fn inspector_filter_labels_are_nonempty() {
        assert!(!InspectorFilter::All.label().is_empty());
        assert!(!InspectorFilter::Errors.label().is_empty());
        assert!(!InspectorFilter::Sandbox.label().is_empty());
        assert!(!InspectorFilter::CapDenied.label().is_empty());
        assert!(!InspectorFilter::Egress.label().is_empty());
    }

    #[test]
    fn inspector_state_default_not_loaded() {
        let s = InspectorState::default();
        assert!(!s.loaded);
        assert!(s.lines.is_empty());
        assert!(s.raw_lines.is_empty());
        assert_eq!(s.filter, InspectorFilter::All);
    }

    #[test]
    fn inspector_state_load_none_path_produces_empty_lines() {
        let mut s = InspectorState::default();
        s.load(Some(Path::new("/nonexistent/flight.jsonl")));
        assert!(s.loaded, "loaded must be true even when file is absent");
        assert!(s.lines.is_empty(), "lines must be empty when file absent");
    }

    #[test]
    fn inspector_state_load_sets_loaded_flag() {
        let mut s = InspectorState::default();
        s.load(None); // tries flight.jsonl in CWD, likely missing — that's OK
        assert!(s.loaded);
        assert!(!s.load_time.is_empty(), "load_time must be set");
    }

    #[test]
    fn inspector_state_rebuild_applies_filter() {
        let mut s = InspectorState {
            raw_lines: vec![
                r#"{"kind":"tool_error"}"#.to_string(),
                r#"{"kind":"tool_result"}"#.to_string(),
            ],
            filter: InspectorFilter::Errors,
            ..Default::default()
        };
        s.rebuild_view();
        assert_eq!(s.lines.len(), 1);
        assert!(s.lines[0].contains("tool_error"));
    }

    #[test]
    fn inspector_state_rebuild_caps_scroll() {
        let mut s = InspectorState {
            raw_lines: vec![r#"{"kind":"tool_error"}"#.to_string()],
            scroll: 100,
            filter: InspectorFilter::Errors,
            ..Default::default()
        };
        s.rebuild_view();
        assert_eq!(s.scroll, 0, "scroll must be capped to line count after rebuild");
    }

    #[test]
    fn inspector_max_lines_constant_is_five_hundred() {
        assert_eq!(MAX_INSPECTOR_LINES, 500);
    }

    #[test]
    fn inspector_state_load_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("flight.jsonl");
        let content = (0..10)
            .map(|i| format!("{{\"kind\":\"tool_result\",\"seq\":{i}}}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, content).unwrap();

        let mut s = InspectorState::default();
        s.load(Some(&path));
        assert!(s.loaded);
        assert_eq!(s.lines.len(), 10, "all lines within limit must be loaded");
    }
}
