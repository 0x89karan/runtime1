use std::path::PathBuf;

use super::reader::{AgentInfo, Snapshot, SysBudget, SysProvider, SysQueue, SysSandbox};

/// Which view is currently displayed.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// Table of all running agents.
    Dashboard,
    /// Expanded detail for the selected agent.
    AgentDetail,
    /// Global system statistics.
    System,
}

/// Full application state, updated on each tick.
pub struct App {
    pub view:        View,
    /// Agent ID of the currently selected row (stable across snapshot refreshes).
    pub selected_id: Option<String>,
    pub agents:      Vec<AgentInfo>,
    pub budget:      Option<SysBudget>,
    pub queue:       Option<SysQueue>,
    pub sandbox:     Option<SysSandbox>,
    pub provider:    Option<SysProvider>,
    pub error:       Option<String>,
}

impl App {
    pub fn new(_agents_dir: PathBuf) -> Self {
        Self {
            view:        View::Dashboard,
            selected_id: None,
            agents:      vec![],
            budget:      None,
            queue:       None,
            sandbox:     None,
            provider:    None,
            error:       None,
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
        self.agents  = snap.agents;
        self.budget  = snap.budget;
        self.queue   = snap.queue;
        self.sandbox = snap.sandbox;
        self.provider = snap.provider;
        self.error   = snap.error;
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
}
