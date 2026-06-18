use std::path::PathBuf;

use super::memory::{read_agent_memory, read_kb_segments, AgentMemory, KbSegment};
use super::reader::{AgentInfo, Snapshot, SysBudget, SysProvider, SysQueue, SysSandbox};
use super::topology::{build_graph, TopologyGraph};

/// Which view is currently displayed.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// Table of all running agents.
    Dashboard,
    /// Expanded detail for the selected agent.
    AgentDetail,
    /// Global system statistics.
    System,
    /// Multi-agent spawn tree and message graph.
    Topology,
    /// Browse per-agent and shared KB memory stores.
    Memory,
}

/// Which pane is active in the Memory view (true-tab model).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum MemoryPane {
    #[default]
    ShortTerm,
    LongTerm,
    Kb,
}

/// Why the Memory view shows a degraded state.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryAbsence {
    /// `/agents/kb/` directory absent — Phase 5 not compiled / deployed.
    Subsystem,
    /// `/agents/kb/` present but empty — Phase 5 present, no KB data yet.
    Empty,
}

/// UI state for the Memory view.
#[derive(Debug, Default)]
pub struct MemoryPaneState {
    /// Per-agent memory for the currently selected agent.  None when no agent is
    /// selected or the agent has no memory dir.
    pub agent_memory:      Option<AgentMemory>,
    /// All shared KB segments.
    pub kb_segments:       Vec<KbSegment>,
    /// Current search query (empty = no filter).
    pub search_query:      String,
    /// True while the user is typing into the search box.
    pub search_active:     bool,
    /// Per-pane scroll offsets — only the active pane is rendered (true-tab).
    pub short_term_scroll: usize,
    pub long_term_scroll:  usize,
    pub kb_scroll:         usize,
    /// Which pane is currently visible.
    pub pane:              MemoryPane,
    /// None = subsystem present (or not yet checked).
    pub absence:           Option<MemoryAbsence>,
}

impl MemoryPaneState {
    /// Return a mutable reference to the scroll counter for the active pane.
    pub fn active_scroll_mut(&mut self) -> &mut usize {
        match self.pane {
            MemoryPane::ShortTerm => &mut self.short_term_scroll,
            MemoryPane::LongTerm  => &mut self.long_term_scroll,
            MemoryPane::Kb        => &mut self.kb_scroll,
        }
    }
}

/// Full application state, updated on each tick.
pub struct App {
    pub view:            View,
    /// Agent ID of the currently selected row (stable across snapshot refreshes).
    pub selected_id:     Option<String>,
    pub agents:          Vec<AgentInfo>,
    pub budget:          Option<SysBudget>,
    pub queue:           Option<SysQueue>,
    pub sandbox:         Option<SysSandbox>,
    pub provider:        Option<SysProvider>,
    pub error:           Option<String>,
    /// Topology graph, rebuilt on every tick.
    pub topology:        TopologyGraph,
    /// Vertical scroll offset for the Topology view.
    pub topology_scroll: usize,
    /// Optional path to flight.jsonl for message edge data.
    pub log_path:        Option<PathBuf>,
    /// FUSE mount point — needed by memory readers in apply_snapshot.
    pub agents_dir:      PathBuf,
    /// UI state for the Memory view.
    pub memory_view:     MemoryPaneState,
}

impl App {
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            view:            View::Dashboard,
            selected_id:     None,
            agents:          vec![],
            budget:          None,
            queue:           None,
            sandbox:         None,
            provider:        None,
            error:           None,
            topology:        TopologyGraph::default(),
            topology_scroll: 0,
            log_path:        None,
            agents_dir,
            memory_view:     MemoryPaneState::default(),
        }
    }

    pub fn apply_snapshot(&mut self, snap: Snapshot) {
        // Preserve selected_id stability: if the selected agent is still present,
        // keep it selected; otherwise clear the selection.
        if let Some(ref id) = self.selected_id {
            if !snap.agents.iter().any(|a| &a.id == id) {
                self.selected_id = None;
                // If the agent we were inspecting is gone, go back to the
                // dashboard so the user isn't left in a stale AgentDetail view
                // where 'q' would require two presses to quit.
                if self.view == View::AgentDetail {
                    self.view = View::Dashboard;
                }
            }
        }
        // Auto-select first agent on first load.
        if self.selected_id.is_none() {
            self.selected_id = snap.agents.first().map(|a| a.id.clone());
        }
        self.agents   = snap.agents;
        self.budget   = snap.budget;
        self.queue    = snap.queue;
        self.sandbox  = snap.sandbox;
        self.provider = snap.provider;
        self.error    = snap.error;
        // Parse flight.jsonl for message edges only while the Topology view is
        // active — reading up to 512 KB on every tick in other views causes stutter.
        let log = if self.view == View::Topology { self.log_path.as_deref() } else { None };
        self.topology = build_graph(&self.agents, log);

        // Read memory only while the Memory view is active to avoid FUSE I/O on
        // every tick when the user is not looking at memory data.
        if self.view == View::Memory {
            let q = self.memory_view.search_query.clone();
            self.memory_view.agent_memory = self.selected_id
                .as_deref()
                .and_then(|id| read_agent_memory(&self.agents_dir, id, &q));
            self.memory_view.kb_segments = read_kb_segments(&self.agents_dir, &q);
            let kb_dir = self.agents_dir.join("kb");
            self.memory_view.absence = if !kb_dir.is_dir() {
                Some(MemoryAbsence::Subsystem)
            } else if self.memory_view.kb_segments.is_empty() {
                Some(MemoryAbsence::Empty)
            } else {
                None
            };
        }
    }

    /// Index of the selected agent in the current list, or None.
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id.as_ref()?;
        self.agents.iter().position(|a| &a.id == id)
    }

    /// Select the agent at the given index.
    pub fn select_index(&mut self, idx: usize) {
        self.selected_id = self.agents.get(idx).map(|a| a.id.clone());
    }

    /// Move selection up one row (wraps).
    pub fn select_prev(&mut self) {
        if self.agents.is_empty() { return; }
        let idx = self.selected_index().unwrap_or(0);
        let next = if idx == 0 { self.agents.len() - 1 } else { idx - 1 };
        self.select_index(next);
    }

    /// Move selection down one row (wraps).
    pub fn select_next(&mut self) {
        if self.agents.is_empty() { return; }
        let idx = self.selected_index().unwrap_or(0);
        let next = (idx + 1) % self.agents.len();
        self.select_index(next);
    }

    /// The currently selected agent, if any.
    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        let id = self.selected_id.as_ref()?;
        self.agents.iter().find(|a| &a.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::reader::{AgentInfo, BudgetKind, Snapshot};

    fn make_agent(id: &str) -> AgentInfo {
        AgentInfo {
            id:             id.to_string(),
            status:         "running".to_string(),
            context_tokens: 0,
            budget:         BudgetKind::Unlimited,
            tools:          vec![],
            parent_id:      None,
        }
    }

    fn make_snapshot(ids: &[&str]) -> Snapshot {
        Snapshot {
            agents:   ids.iter().map(|id| make_agent(id)).collect(),
            budget:   None,
            queue:    None,
            sandbox:  None,
            provider: None,
            error:    None,
        }
    }

    // ── App::new ─────────────────────────────────────────────────────────────

    #[test]
    fn app_new_starts_on_dashboard_with_no_selection() {
        let app = App::new(PathBuf::from("/agents"));
        assert_eq!(app.view, View::Dashboard);
        assert!(app.selected_id.is_none());
        assert!(app.agents.is_empty());
    }

    // ── apply_snapshot: auto-select ──────────────────────────────────────────

    #[test]
    fn apply_snapshot_autoselects_first_agent_on_first_load() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["beta", "alpha"]));
        // agents are NOT re-sorted by apply_snapshot; first in list is selected.
        assert_eq!(app.selected_id.as_deref(), Some("beta"));
    }

    #[test]
    fn apply_snapshot_empty_list_leaves_selected_id_none() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&[]));
        assert!(app.selected_id.is_none());
    }

    // ── apply_snapshot: selection stability ─────────────────────────────────

    #[test]
    fn apply_snapshot_preserves_selection_when_agent_still_present() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b"]));
        app.selected_id = Some("b".to_string());

        app.apply_snapshot(make_snapshot(&["a", "b"]));
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn apply_snapshot_clears_selection_when_agent_disappears() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b"]));
        app.selected_id = Some("b".to_string());

        // "b" disappears on next snapshot.
        app.apply_snapshot(make_snapshot(&["a"]));
        // selection was cleared → auto-select first agent "a".
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn apply_snapshot_does_not_change_selection_to_first_when_already_selected() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        app.selected_id = Some("c".to_string());

        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        assert_eq!(app.selected_id.as_deref(), Some("c"),
            "selection must not be reset to first when current selection is still present");
    }

    #[test]
    fn apply_snapshot_exits_agent_detail_when_viewed_agent_disappears() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b"]));
        app.selected_id = Some("b".to_string());
        app.view = View::AgentDetail;

        // Agent "b" disappears — view must auto-reset to Dashboard.
        app.apply_snapshot(make_snapshot(&["a"]));
        assert_eq!(app.view, View::Dashboard,
            "AgentDetail view must revert to Dashboard when the selected agent disappears");
    }

    // ── apply_snapshot: system fields propagated ─────────────────────────────

    #[test]
    fn apply_snapshot_propagates_error_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        let mut snap = make_snapshot(&[]);
        snap.error = Some("read error".to_string());
        app.apply_snapshot(snap);
        assert_eq!(app.error.as_deref(), Some("read error"));
    }

    // ── selected_index ───────────────────────────────────────────────────────

    #[test]
    fn selected_index_returns_none_when_no_selection() {
        let app = App::new(PathBuf::from("/agents"));
        assert!(app.selected_index().is_none());
    }

    #[test]
    fn selected_index_returns_correct_position() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        app.selected_id = Some("c".to_string());
        assert_eq!(app.selected_index(), Some(2));
    }

    // ── select_prev / select_next ────────────────────────────────────────────

    #[test]
    fn select_prev_on_empty_list_is_noop() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.select_prev(); // must not panic
        assert!(app.selected_id.is_none());
    }

    #[test]
    fn select_next_on_empty_list_is_noop() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.select_next(); // must not panic
        assert!(app.selected_id.is_none());
    }

    #[test]
    fn select_next_advances_selection() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        // auto-selected "a" (index 0)
        app.select_next();
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn select_next_wraps_at_end() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        app.selected_id = Some("c".to_string()); // last agent
        app.select_next();
        assert_eq!(app.selected_id.as_deref(), Some("a"),
            "select_next must wrap from last to first");
    }

    #[test]
    fn select_prev_decrements_selection() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        app.selected_id = Some("b".to_string());
        app.select_prev();
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn select_prev_wraps_at_start() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b", "c"]));
        app.selected_id = Some("a".to_string()); // first agent
        app.select_prev();
        assert_eq!(app.selected_id.as_deref(), Some("c"),
            "select_prev must wrap from first to last");
    }

    // ── selected_agent ───────────────────────────────────────────────────────

    #[test]
    fn selected_agent_returns_none_with_no_selection() {
        let app = App::new(PathBuf::from("/agents"));
        assert!(app.selected_agent().is_none());
    }

    #[test]
    fn selected_agent_returns_correct_agent() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a", "b"]));
        app.selected_id = Some("b".to_string());
        let agent = app.selected_agent().unwrap();
        assert_eq!(agent.id, "b");
    }

    #[test]
    fn selected_agent_returns_none_when_id_not_in_list() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(&["a"]));
        app.selected_id = Some("z".to_string()); // stale id
        assert!(app.selected_agent().is_none());
    }

    // ── Memory view: agents_dir stored ───────────────────────────────────────

    #[test]
    fn app_agents_dir_stored_in_new() {
        let app = App::new(PathBuf::from("/test/agents"));
        assert_eq!(app.agents_dir, PathBuf::from("/test/agents"),
            "agents_dir must be stored (not discarded with _ prefix)");
    }

    // ── Memory view: absence detection ───────────────────────────────────────

    #[test]
    fn app_view_memory_absent_subsystem_when_kb_dir_missing() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(d.path().to_path_buf());
        app.view = View::Memory;
        // No kb/ dir in tmpdir → Phase 5 absent.
        app.apply_snapshot(make_snapshot(&[]));
        assert_eq!(app.memory_view.absence, Some(MemoryAbsence::Subsystem));
    }

    #[test]
    fn app_view_memory_absent_empty_when_kb_dir_exists_but_no_segs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("kb")).unwrap();
        let mut app = App::new(d.path().to_path_buf());
        app.view = View::Memory;
        app.apply_snapshot(make_snapshot(&[]));
        assert_eq!(app.memory_view.absence, Some(MemoryAbsence::Empty));
    }

    #[test]
    fn app_view_memory_absence_none_when_kb_has_segments() {
        let d = tempfile::tempdir().unwrap();
        let seg = d.path().join("kb").join("project");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join("k1"), r#"{"content":"x","class":"log","provenance":{}}"#).unwrap();
        let mut app = App::new(d.path().to_path_buf());
        app.view = View::Memory;
        app.apply_snapshot(make_snapshot(&[]));
        assert_eq!(app.memory_view.absence, None);
    }

    // ── Memory view: MemoryPaneState scroll helpers ───────────────────────────

    #[test]
    fn active_scroll_mut_returns_correct_field_per_pane() {
        let mut state = MemoryPaneState {
            short_term_scroll: 1,
            long_term_scroll: 2,
            kb_scroll: 3,
            ..Default::default()
        };

        state.pane = MemoryPane::ShortTerm;
        assert_eq!(*state.active_scroll_mut(), 1);
        state.pane = MemoryPane::LongTerm;
        assert_eq!(*state.active_scroll_mut(), 2);
        state.pane = MemoryPane::Kb;
        assert_eq!(*state.active_scroll_mut(), 3);
    }

    #[test]
    fn memory_pane_per_pane_scroll_preserved_across_tab() {
        let mut state = MemoryPaneState::default();
        // Set different scroll values per pane.
        *state.active_scroll_mut() = 5; // pane starts at ShortTerm (the default)
        state.pane = MemoryPane::LongTerm;
        *state.active_scroll_mut() = 10;

        // Switch back to ShortTerm — value must be preserved.
        state.pane = MemoryPane::ShortTerm;
        assert_eq!(*state.active_scroll_mut(), 5,
            "ShortTerm scroll must survive pane switch to LongTerm and back");
    }

    #[test]
    fn memory_pane_tab_cycles_shortterm_longterm_kb_repeat() {
        let mut pane = MemoryPane::ShortTerm;
        pane = match pane { MemoryPane::ShortTerm => MemoryPane::LongTerm,
                             MemoryPane::LongTerm  => MemoryPane::Kb,
                             MemoryPane::Kb        => MemoryPane::ShortTerm };
        assert_eq!(pane, MemoryPane::LongTerm);
        pane = match pane { MemoryPane::ShortTerm => MemoryPane::LongTerm,
                             MemoryPane::LongTerm  => MemoryPane::Kb,
                             MemoryPane::Kb        => MemoryPane::ShortTerm };
        assert_eq!(pane, MemoryPane::Kb);
        pane = match pane { MemoryPane::ShortTerm => MemoryPane::LongTerm,
                             MemoryPane::LongTerm  => MemoryPane::Kb,
                             MemoryPane::Kb        => MemoryPane::ShortTerm };
        assert_eq!(pane, MemoryPane::ShortTerm, "cycle must wrap back to ShortTerm");
    }

    #[test]
    fn memory_search_active_toggled_by_slash() {
        let mut state = MemoryPaneState::default();
        assert!(!state.search_active);
        state.search_active = true;
        assert!(state.search_active);
    }

    #[test]
    fn memory_search_query_cleared_on_esc() {
        let mut state = MemoryPaneState::default();
        state.search_query.push_str("arch");
        state.search_active = true;
        state.search_active = false;
        state.search_query.clear();
        assert!(state.search_query.is_empty());
        assert!(!state.search_active);
    }

    #[test]
    fn memory_view_stays_active_when_agent_disappears_kb_still_shows() {
        let d = tempfile::tempdir().unwrap();
        let seg = d.path().join("kb").join("project");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join("k1"), r#"{"content":"x","class":"log","provenance":{}}"#).unwrap();

        let mut app = App::new(d.path().to_path_buf());
        app.apply_snapshot(make_snapshot(&["a"]));
        app.view = View::Memory;

        // Agent "a" disappears — view must stay Memory (KB is still browsable).
        app.apply_snapshot(make_snapshot(&[]));
        assert_eq!(app.view, View::Memory,
            "Memory view must stay active when selected agent disappears — KB still browsable");
        assert!(!app.memory_view.kb_segments.is_empty(),
            "KB segments must remain loaded even with no selected agent");
    }
}
