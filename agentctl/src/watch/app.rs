use std::path::PathBuf;

use agentd::capability::Capability;

use super::approvals::ApprovalsViewState;
use super::inspector::InspectorState;
use super::memory::{read_agent_memory, read_kb_segments, AgentMemory, KbSegment};
use super::reader::{self, AgentInfo, PendingAction, Snapshot, SysBudget, SysProvider, SysQueue, SysSandbox};
use super::spawn::{load_spawn_templates, SpawnTemplate};
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
    /// Form to pick a template, fill a task, toggle capabilities, and spawn an agent.
    Spawn,
    /// Read-only flight-log inspector with filter cycling and search.
    Inspector,
    /// Browse and resolve pending operator approval requests.
    Approvals,
}

// ── Spawn view ───────────────────────────────────────────────────────────────

/// Which form field has keyboard focus in the Spawn view.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SpawnFocus {
    #[default]
    TemplatePicker,
    TaskField,
    CapToggles,
    ActionGenerate,
    ActionSpawn,
}

/// Pending exec action: set by `handle_spawn_key`, consumed by `run_tui`.
///
/// `run_tui` restores the terminal and then executes agentd when it finds this.
#[derive(Debug, Clone)]
pub struct PendingSpawn {
    /// Name of the resolved template (used to construct the output filename).
    pub template_name:  String,
    /// Task string to inject.
    pub task:           String,
    /// Capabilities to add beyond the template baseline.
    pub extra_caps:     Vec<Capability>,
    /// Suggested caps the user explicitly unchecked — stripped from the template
    /// baseline so the toggle semantics are "grant / revoke", not just "add".
    pub disabled_caps:  Vec<Capability>,
}

/// UI state for the Spawn view.
#[derive(Debug, Default)]
pub struct SpawnViewState {
    /// Loaded templates; empty until the view is first entered.
    pub templates:      Vec<SpawnTemplate>,
    /// `true` once `load_spawn_templates()` has been called at least once.
    pub loaded:         bool,
    /// Load error message, if any.
    pub load_error:     Option<String>,
    /// Index of the currently selected template.
    pub template_idx:   usize,
    /// Which form section has keyboard focus.
    pub focus:          SpawnFocus,
    /// Index of the highlighted capability toggle (when focus = CapToggles).
    pub cap_idx:        usize,
    /// Task text entered by the operator.
    pub task_input:     String,
    /// Per-capability toggle state: (cap, display_string, enabled).
    pub cap_toggles:    Vec<(Capability, String, bool)>,
    /// Generated `agent.toml` preview text (set by Generate action).
    pub preview:        Option<String>,
    /// Feedback message shown in the footer (error or success).
    pub result_msg:     Option<String>,
    /// Pending exec action.  When set, `run_tui` cleans up the terminal and
    /// execs agentd.
    pub pending_exec:   Option<PendingSpawn>,
}

impl SpawnViewState {
    /// Load templates and build the initial cap-toggle list for the first template.
    pub fn load(&mut self) {
        if self.loaded { return; }
        self.loaded = true;
        let (templates, err) = load_spawn_templates();
        self.load_error = err;
        self.templates  = templates;
        self.template_idx = 0;
        self.rebuild_cap_toggles();
        self.prefill_task_if_empty();
    }

    /// Pre-fill `task_input` with the first `sample_tasks` entry of the selected template,
    /// but only when the field is currently empty (never overwrites user-typed text).
    pub fn prefill_task_if_empty(&mut self) {
        if self.task_input.is_empty() {
            if let Some(sample) = self
                .templates
                .get(self.template_idx)
                .and_then(|t| t.sample_tasks.first())
            {
                self.task_input = sample.clone();
            }
        }
    }

    /// Rebuild `cap_toggles` from the currently selected template's `suggested_caps`.
    pub fn rebuild_cap_toggles(&mut self) {
        let caps = self
            .templates
            .get(self.template_idx)
            .map(|t| t.suggested_caps.clone())
            .unwrap_or_default();
        self.cap_toggles = caps
            .into_iter()
            .map(|cap| {
                let label = super::spawn::display_cap(&cap);
                (cap, label, true) // all pre-checked
            })
            .collect();
        self.cap_idx = 0;
    }

    /// The currently selected template, if any.
    pub fn selected_template(&self) -> Option<&SpawnTemplate> {
        self.templates.get(self.template_idx)
    }

    /// Reset all mutable form state after a template selection change.
    ///
    /// Only replaces `task_input` if it is empty or still holds `prev_sample`
    /// (the previous template's prefill). User-typed text is never discarded.
    fn reset_after_template_change(&mut self, prev_sample: Option<&str>) {
        self.rebuild_cap_toggles();
        self.preview    = None;
        self.result_msg = None;
        let is_unchanged = self.task_input.is_empty()
            || prev_sample.is_some_and(|s| self.task_input == s);
        if is_unchanged {
            self.task_input.clear();
            self.prefill_task_if_empty();
        }
    }

    /// Move template selection up (saturating).
    pub fn select_template_prev(&mut self) {
        if self.template_idx > 0 {
            let prev_sample = self.templates.get(self.template_idx)
                .and_then(|t| t.sample_tasks.first())
                .cloned();
            self.template_idx -= 1;
            self.reset_after_template_change(prev_sample.as_deref());
        }
    }

    /// Move template selection down (saturating).
    pub fn select_template_next(&mut self) {
        if !self.templates.is_empty()
            && self.template_idx < self.templates.len() - 1
        {
            let prev_sample = self.templates.get(self.template_idx)
                .and_then(|t| t.sample_tasks.first())
                .cloned();
            self.template_idx += 1;
            self.reset_after_template_change(prev_sample.as_deref());
        }
    }

    /// Toggle the cap at `cap_idx` (or at a specific index).
    pub fn toggle_cap_at(&mut self, idx: usize) {
        if let Some((_, _, enabled)) = self.cap_toggles.get_mut(idx) {
            *enabled = !*enabled;
        }
    }

    /// Move cap selection up.
    pub fn cap_prev(&mut self) {
        if self.cap_idx > 0 { self.cap_idx -= 1; }
    }

    /// Move cap selection down.
    pub fn cap_next(&mut self) {
        if !self.cap_toggles.is_empty()
            && self.cap_idx < self.cap_toggles.len() - 1
        {
            self.cap_idx += 1;
        }
    }

    /// Collect the caps that are currently enabled.
    pub fn enabled_caps(&self) -> Vec<Capability> {
        self.cap_toggles
            .iter()
            .filter(|(_, _, on)| *on)
            .map(|(cap, _, _)| cap.clone())
            .collect()
    }

    /// Collect the suggested caps that the user explicitly unchecked.
    pub fn disabled_caps(&self) -> Vec<Capability> {
        self.cap_toggles
            .iter()
            .filter(|(_, _, on)| !*on)
            .map(|(cap, _, _)| cap.clone())
            .collect()
    }

    /// Cycle focus forward through the form sections.
    pub fn focus_next(&mut self) {
        self.focus = match self.focus {
            SpawnFocus::TemplatePicker => SpawnFocus::TaskField,
            SpawnFocus::TaskField      => {
                if self.cap_toggles.is_empty() {
                    SpawnFocus::ActionGenerate
                } else {
                    SpawnFocus::CapToggles
                }
            }
            SpawnFocus::CapToggles     => SpawnFocus::ActionGenerate,
            SpawnFocus::ActionGenerate => SpawnFocus::ActionSpawn,
            SpawnFocus::ActionSpawn    => SpawnFocus::TemplatePicker,
        };
    }

    /// Generate a preview from the current form state.
    ///
    /// When `/agents/control` is present (live agentd running), the preview is
    /// the JSON payload that will be injected. Otherwise it falls back to the
    /// full `agent.toml` TOML that will be exec'd.
    pub fn do_generate(&mut self, agents_dir: Option<&std::path::Path>) {
        let Some(template) = self.selected_template() else {
            self.result_msg = Some("No template selected.".to_string());
            return;
        };
        let task = if self.task_input.is_empty() {
            None
        } else {
            Some(self.task_input.as_str())
        };
        // Re-resolve the full template config to call `to_agent_config`.
        let resolver = crate::build_resolver(None, None);
        match resolver.resolve(&template.name) {
            Err(e) => {
                self.result_msg = Some(format!("resolve error: {e:#}"));
            }
            Ok((cfg, _)) => {
                let extra    = self.enabled_caps();
                let disabled = self.disabled_caps();
                match cfg.to_agent_config(task, extra) {
                    Err(e) => {
                        self.result_msg = Some(format!("config error: {e:#}"));
                    }
                    Ok(mut config) => {
                        // Strip caps the user explicitly disabled from the template
                        // baseline so the preview matches what execute_pending_spawn
                        // will actually exec.
                        if let Some(agent) = config.agent.as_mut() {
                            if let Some(caps) = agent.capabilities.as_mut() {
                                caps.retain(|c| !disabled.contains(c));
                            }
                        }
                        let use_control = agents_dir
                            .map(|d| d.join("control").exists())
                            .unwrap_or(false);
                        if use_control {
                            // JSON preview matches the OperatorSpawnRequest payload
                            // that execute_pending_spawn writes to /agents/control.
                            let agent_id = config.agent.as_ref()
                                .map(|a| a.id.clone())
                                .unwrap_or_else(|| "operator".to_string());
                            let capabilities = config.agent.as_ref()
                                .and_then(|a| a.capabilities.clone());
                            let payload = serde_json::json!({
                                "task":         self.task_input,
                                "id":           agent_id,
                                "capabilities": capabilities,
                            });
                            match serde_json::to_string_pretty(&payload) {
                                Err(e) => {
                                    self.result_msg = Some(format!("json error: {e:#}"));
                                }
                                Ok(json_str) => {
                                    self.preview    = Some(json_str);
                                    self.result_msg = Some(
                                        "JSON preview (live inject). Press [r] to send.".to_string()
                                    );
                                }
                            }
                        } else {
                            match toml::to_string_pretty(&config) {
                                Err(e) => {
                                    self.result_msg = Some(format!("toml error: {e:#}"));
                                }
                                Ok(toml_str) => {
                                    self.preview    = Some(toml_str);
                                    self.result_msg = Some(
                                        "TOML preview (exec fallback). Press [r] to spawn.".to_string()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Queue a spawn action for `run_tui` to execute (mode a: generate + exec).
    pub fn do_spawn(&mut self) {
        let template_name = match self.selected_template() {
            Some(t) => t.name.clone(),
            None    => { self.result_msg = Some("No template selected.".to_string()); return; }
        };
        if self.task_input.is_empty() {
            // Check if template has a default task; if not, force Generate first.
            let resolver = crate::build_resolver(None, None);
            let has_default = resolver
                .resolve(&template_name)
                .ok()
                .and_then(|(cfg, _)| cfg.agent.map(|a| !a.task.is_empty()))
                .unwrap_or(false);
            if !has_default {
                self.result_msg = Some("Task required — fill in the task field and press [g] to generate first.".to_string());
                return;
            }
        }
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            self.result_msg = Some("ANTHROPIC_API_KEY is not set — required by agentd.".to_string());
            return;
        }
        let extra    = self.enabled_caps();
        let disabled = self.disabled_caps();
        self.result_msg  = Some("Spawning agentd — TUI will exit...".to_string());
        self.pending_exec = Some(PendingSpawn {
            template_name,
            task:          self.task_input.clone(),
            extra_caps:    extra,
            disabled_caps: disabled,
        });
    }
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
    /// UI state for the Spawn view.
    pub spawn_view:      SpawnViewState,
    /// UI state for the Inspector view.
    pub inspector_view:  InspectorState,
    /// Current approval queue (refreshed every tick from /agents/approvals).
    pub approvals_items: Vec<PendingAction>,
    /// UI state for the Approvals view.
    pub approvals_view:  ApprovalsViewState,
    /// Shown as a green banner on the Dashboard after a successful live injection
    /// via /agents/control; cleared on the next keypress.
    pub spawn_banner:    Option<String>,
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
            spawn_view:      SpawnViewState::default(),
            inspector_view:  InspectorState::default(),
            approvals_items: vec![],
            approvals_view:  ApprovalsViewState::default(),
            spawn_banner:    None,
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

        // Count egress events per agent only when AgentDetail is open; egress
        // events are sparse so the scan is cheap, but we still avoid it on every
        // tick in other views.
        if self.view == View::AgentDetail {
            if let Some(lp) = self.log_path.as_deref() {
                let counts = reader::count_egress_by_agent(lp);
                for agent in &mut self.agents {
                    if let Some(&(b, d)) = counts.get(&agent.id) {
                        agent.egress_brokered = b;
                        agent.egress_denied   = d;
                    }
                }
            }
        }

        // Load templates once when the Spawn view is first entered.
        if self.view == View::Spawn {
            self.spawn_view.load();
        }

        // Load inspector lines once on first entry to this view; [r] triggers reload.
        if self.view == View::Inspector && !self.inspector_view.loaded {
            self.inspector_view.load(self.log_path.as_deref());
        }

        // Clamp selection to stay in-bounds if items were resolved since last tick.
        // (Approval items are refreshed by run_tui/run_plain via update_approvals().)
        if !self.approvals_items.is_empty()
            && self.approvals_view.selected_idx >= self.approvals_items.len()
        {
            self.approvals_view.selected_idx = self.approvals_items.len() - 1;
        }

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

    /// Update the pending approvals list. Called every tick from run_tui/run_plain
    /// via `source.load_approvals()`, replacing the old FUSE-direct call in apply_snapshot.
    pub fn update_approvals(&mut self, items: Vec<PendingAction>) {
        self.approvals_items = items;
        if !self.approvals_items.is_empty()
            && self.approvals_view.selected_idx >= self.approvals_items.len()
        {
            self.approvals_view.selected_idx = self.approvals_items.len() - 1;
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

    use crate::ENV_MUTEX;

    fn make_agent(id: &str) -> AgentInfo {
        AgentInfo {
            id:              id.to_string(),
            status:          "running".to_string(),
            status_detail:   None,
            context_tokens:  0,
            budget:          BudgetKind::Unlimited,
            tools:           vec![],
            parent_id:       None,
            sandbox:         None,
            egress_brokered: 0,
            egress_denied:   0,
            tier:            "native".to_string(),
            isolation:       String::new(),
            pid:             0,
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

    // ── Spawn view unit tests ─────────────────────────────────────────────────

    #[test]
    fn spawn_view_initial_state_has_no_preview() {
        let state = SpawnViewState::default();
        assert!(state.preview.is_none(), "preview must start as None");
        assert!(!state.loaded, "loaded must start false");
        assert!(state.templates.is_empty());
        assert!(state.pending_exec.is_none());
    }

    #[test]
    fn spawn_view_focus_cycles_through_all_states_with_caps() {
        // Inject a cap so CapToggles is reachable.
        let mut state = SpawnViewState {
            cap_toggles: vec![(
                agentd::capability::Capability::Spawn,
                "Spawn".to_string(),
                true,
            )],
            ..Default::default()
        };
        assert_eq!(state.focus, SpawnFocus::TemplatePicker);
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::TaskField);
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::CapToggles);
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::ActionGenerate);
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::ActionSpawn);
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::TemplatePicker, "must wrap back");
    }

    #[test]
    fn spawn_view_focus_skips_cap_toggles_when_empty() {
        // No cap_toggles — CapToggles must be skipped.
        let mut state = SpawnViewState {
            focus: SpawnFocus::TaskField,
            ..Default::default()
        };
        state.focus_next();
        assert_eq!(state.focus, SpawnFocus::ActionGenerate,
            "CapToggles must be skipped when cap_toggles is empty");
    }

    #[test]
    fn spawn_view_select_template_prev_does_not_underflow() {
        let mut state = SpawnViewState {
            template_idx: 0,
            ..Default::default()
        };
        state.select_template_prev();
        assert_eq!(state.template_idx, 0, "must saturate at 0");
    }

    #[test]
    fn spawn_view_select_template_next_saturates_at_end() {
        let mut state = SpawnViewState::default();
        // No templates loaded — next must be a no-op.
        state.select_template_next();
        assert_eq!(state.template_idx, 0, "must not advance past end");
    }

    #[test]
    fn spawn_view_toggle_cap_flips_enabled_state() {
        let mut state = SpawnViewState {
            cap_toggles: vec![(
                agentd::capability::Capability::Spawn,
                "Spawn".to_string(),
                true,
            )],
            ..Default::default()
        };
        state.toggle_cap_at(0);
        assert!(!state.cap_toggles[0].2, "must be disabled after toggle");
        state.toggle_cap_at(0);
        assert!(state.cap_toggles[0].2, "must be re-enabled after second toggle");
    }

    #[test]
    fn spawn_view_enabled_caps_returns_only_enabled() {
        let state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
                (agentd::capability::Capability::FsRead { prefix: "/".into() },
                 "FsRead {/}".to_string(), false),
            ],
            ..Default::default()
        };
        let enabled = state.enabled_caps();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0], agentd::capability::Capability::Spawn);
    }

    #[test]
    fn spawn_view_cap_idx_clamps_within_toggles() {
        let mut state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
            ],
            ..Default::default()
        };
        // prev at 0 must not underflow
        state.cap_prev();
        assert_eq!(state.cap_idx, 0);
        // next at last must not overflow
        state.cap_next();
        assert_eq!(state.cap_idx, 0, "must saturate at last index");
    }

    #[test]
    fn app_new_has_spawn_view_default_state() {
        let d = tempfile::tempdir().unwrap();
        let app = App::new(d.path().to_path_buf());
        assert!(!app.spawn_view.loaded);
        assert!(app.spawn_view.preview.is_none());
        assert!(app.spawn_view.templates.is_empty());
        assert_eq!(app.spawn_view.focus, SpawnFocus::TemplatePicker);
    }

    fn make_spawn_template(name: &str) -> super::SpawnTemplate {
        use agentd::template::TemplateSource;
        super::SpawnTemplate {
            name:           name.to_string(),
            source:         TemplateSource::Repo,
            description:    String::new(),
            showcases:      String::new(),
            suggested_caps: vec![],
            sample_tasks:   vec![],
        }
    }

    #[test]
    fn spawn_view_select_template_prev_decrements_when_idx_greater_than_zero() {
        let mut state = SpawnViewState {
            templates: vec![make_spawn_template("a"), make_spawn_template("b")],
            template_idx: 1,
            ..Default::default()
        };
        state.select_template_prev();
        assert_eq!(state.template_idx, 0, "prev must decrement when idx > 0");
    }

    #[test]
    fn spawn_view_select_template_next_increments_when_not_at_end() {
        let mut state = SpawnViewState {
            templates: vec![make_spawn_template("a"), make_spawn_template("b")],
            template_idx: 0,
            ..Default::default()
        };
        state.select_template_next();
        assert_eq!(state.template_idx, 1, "next must increment when idx < len-1");
    }

    fn make_spawn_template_with_tasks(name: &str, tasks: Vec<String>) -> super::SpawnTemplate {
        use agentd::template::TemplateSource;
        super::SpawnTemplate {
            name:           name.to_string(),
            source:         TemplateSource::Repo,
            description:    String::new(),
            showcases:      String::new(),
            suggested_caps: vec![],
            sample_tasks:   tasks,
        }
    }

    #[test]
    fn spawn_view_prefill_fills_empty_task_on_template_select() {
        let mut state = SpawnViewState {
            templates: vec![
                make_spawn_template("scout"),
                make_spawn_template_with_tasks("journaler", vec!["Record today's findings.".into()]),
            ],
            template_idx: 0,
            ..Default::default()
        };
        state.select_template_next(); // navigate to journaler
        assert_eq!(state.template_idx, 1, "must be on journaler");
        assert_eq!(
            state.task_input,
            "Record today's findings.",
            "task_input must be pre-filled from sample_tasks[0]"
        );
    }

    #[test]
    fn spawn_view_prefill_noop_when_no_sample_tasks() {
        let mut state = SpawnViewState {
            templates: vec![
                make_spawn_template("scout"),   // no sample_tasks
                make_spawn_template("scout2"),  // no sample_tasks
            ],
            template_idx: 0,
            ..Default::default()
        };
        state.select_template_next();
        assert_eq!(state.template_idx, 1);
        assert!(state.task_input.is_empty(), "task_input must stay empty when no sample_tasks");
    }

    #[test]
    fn spawn_view_prefill_skips_when_task_already_filled() {
        let mut state = SpawnViewState {
            templates: vec![
                make_spawn_template_with_tasks("journaler", vec!["default sample".into()]),
            ],
            template_idx: 0,
            task_input: "user typed this".to_string(),
            ..Default::default()
        };
        state.prefill_task_if_empty();
        assert_eq!(
            state.task_input, "user typed this",
            "prefill_task_if_empty must not overwrite non-empty task_input"
        );
    }

    #[test]
    fn spawn_view_select_template_prev_prefills_sample_task() {
        let mut state = SpawnViewState {
            templates: vec![
                make_spawn_template_with_tasks("journaler", vec!["Record today's findings.".into()]),
                make_spawn_template("scout"),
            ],
            template_idx: 1,
            ..Default::default()
        };
        state.select_template_prev();
        assert_eq!(state.template_idx, 0, "must be on journaler after prev");
        assert_eq!(
            state.task_input,
            "Record today's findings.",
            "task_input must be pre-filled from sample_tasks[0] after select_template_prev"
        );
    }

    #[test]
    fn spawn_view_reset_after_template_change_clears_preview_and_result_msg() {
        let mut state = SpawnViewState {
            templates:    vec![make_spawn_template("a"), make_spawn_template("b")],
            template_idx: 0,
            preview:      Some("previous preview text".to_string()),
            result_msg:   Some("previous result".to_string()),
            ..Default::default()
        };
        state.select_template_next();
        assert!(state.preview.is_none(), "preview must be cleared after template navigation");
        assert!(state.result_msg.is_none(), "result_msg must be cleared after template navigation");
    }

    #[test]
    fn spawn_view_toggle_cap_at_out_of_bounds_is_noop() {
        let mut state = SpawnViewState {
            cap_toggles: vec![(
                agentd::capability::Capability::Spawn,
                "Spawn".to_string(),
                true,
            )],
            ..Default::default()
        };
        state.toggle_cap_at(99); // way out of bounds
        assert!(state.cap_toggles[0].2, "out-of-bounds toggle must not change any cap");
    }

    #[test]
    fn spawn_view_cap_prev_decrements_when_idx_greater_than_zero() {
        let mut state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
                (agentd::capability::Capability::Spawn, "Spawn2".to_string(), true),
            ],
            cap_idx: 1,
            ..Default::default()
        };
        state.cap_prev();
        assert_eq!(state.cap_idx, 0, "cap_prev must decrement when idx > 0");
    }

    #[test]
    fn spawn_view_cap_next_increments_when_not_at_last() {
        let mut state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
                (agentd::capability::Capability::Spawn, "Spawn2".to_string(), true),
            ],
            cap_idx: 0,
            ..Default::default()
        };
        state.cap_next();
        assert_eq!(state.cap_idx, 1, "cap_next must increment when idx < len-1");
    }

    #[test]
    fn spawn_view_enabled_caps_returns_empty_when_all_disabled() {
        let state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), false),
                (agentd::capability::Capability::FsRead { prefix: "/".into() }, "FsRead".to_string(), false),
            ],
            ..Default::default()
        };
        assert!(state.enabled_caps().is_empty(), "must return empty when all caps disabled");
    }

    #[test]
    fn spawn_view_disabled_caps_returns_unchecked() {
        let state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn,
                 "Spawn".to_string(), true),
                (agentd::capability::Capability::FsRead { prefix: "/workspace".into() },
                 "FsRead /workspace".to_string(), false),
            ],
            ..Default::default()
        };
        let disabled = state.disabled_caps();
        assert_eq!(disabled.len(), 1);
        assert_eq!(
            disabled[0],
            agentd::capability::Capability::FsRead { prefix: "/workspace".into() },
            "disabled_caps must return only the unchecked toggle"
        );
    }

    #[test]
    fn spawn_view_disabled_caps_empty_when_all_enabled() {
        let state = SpawnViewState {
            cap_toggles: vec![
                (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
            ],
            ..Default::default()
        };
        assert!(state.disabled_caps().is_empty(),
            "disabled_caps must be empty when all toggles are on");
    }

    #[test]
    fn spawn_view_do_spawn_passes_disabled_caps_to_pending_exec() {
        let _env_guard = ENV_MUTEX.lock().unwrap();
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        // Safety: test-only env mutation; ENV_MUTEX serializes all env-var-touching tests.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-test-fake"); }

        let mut state = SpawnViewState {
            templates:  vec![make_spawn_template("scout")],
            task_input: "test task".to_string(),
            cap_toggles: vec![(
                agentd::capability::Capability::FsRead { prefix: "/workspace".into() },
                "FsRead /workspace".to_string(),
                false, // disabled
            )],
            ..Default::default()
        };
        state.do_spawn();

        unsafe {
            match saved {
                Some(k) => std::env::set_var("ANTHROPIC_API_KEY", &k),
                None    => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }

        let exec = state.pending_exec.as_ref()
            .expect("pending_exec must be set when guards pass");
        assert_eq!(exec.disabled_caps.len(), 1,
            "PendingSpawn.disabled_caps must contain the one unchecked toggle");
        assert_eq!(
            exec.disabled_caps[0],
            agentd::capability::Capability::FsRead { prefix: "/workspace".into() },
        );
    }

    #[test]
    fn spawn_view_do_generate_sets_error_when_no_templates_loaded() {
        let mut state = SpawnViewState::default();
        // No templates — do_generate must set an error result_msg.
        state.do_generate(None);
        assert_eq!(state.result_msg.as_deref(), Some("No template selected."),
            "do_generate with no templates must set error result_msg");
        assert!(state.preview.is_none(), "preview must stay None on error");
    }

    #[test]
    fn spawn_view_do_spawn_sets_error_when_no_templates_loaded() {
        let mut state = SpawnViewState::default();
        // No templates — do_spawn must set an error result_msg.
        state.do_spawn();
        assert_eq!(state.result_msg.as_deref(), Some("No template selected."),
            "do_spawn with no templates must set error result_msg");
        assert!(state.pending_exec.is_none(), "pending_exec must stay None on error");
    }

    #[test]
    fn spawn_view_do_spawn_api_key_missing_sets_error() {
        let _env_guard = ENV_MUTEX.lock().unwrap();
        // Temporarily remove ANTHROPIC_API_KEY to exercise the env-var guard in do_spawn.
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        // Safety: test-only env mutation; ENV_MUTEX serializes all env-var-touching tests.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY"); }

        let mut state = SpawnViewState {
            templates:  vec![make_spawn_template("scout")],
            task_input: "test task".to_string(),
            ..Default::default()
        };
        state.do_spawn();

        if let Some(k) = saved {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", &k); }
        }

        assert!(
            state.result_msg.as_deref().map(|m| m.contains("ANTHROPIC_API_KEY")).unwrap_or(false),
            "missing API key must set result_msg containing 'ANTHROPIC_API_KEY'"
        );
        assert!(state.pending_exec.is_none(),
            "pending_exec must stay None when ANTHROPIC_API_KEY is absent");
    }
}
