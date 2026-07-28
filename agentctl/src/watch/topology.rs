use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, Seek, SeekFrom},
    path::Path,
};

use super::reader::AgentInfo;

const FLIGHT_TAIL_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    pub id:        String,
    pub parent_id: Option<String>,
    pub status:    String,
}

/// Topology graph derived from agent snapshot + optional flight.jsonl.
#[derive(Debug, Clone, Default)]
pub struct TopologyGraph {
    pub nodes:         Vec<NodeInfo>,
    /// (sender_id, recipient_id) → count of messages sent.
    pub message_edges: HashMap<(String, String), usize>,
    pub parse_errors:  usize,
}

/// Build a topology graph from the current agent snapshot and (optionally) a
/// flight.jsonl path for message edge data.
pub fn build_graph(agents: &[AgentInfo], log_path: Option<&Path>) -> TopologyGraph {
    let nodes: Vec<NodeInfo> = agents
        .iter()
        .map(|a| NodeInfo {
            id:        a.id.clone(),
            parent_id: a.parent_id.clone(),
            status:    a.status.clone(),
        })
        .collect();

    let (message_edges, parse_errors) = match log_path {
        Some(p) => parse_message_edges(p),
        None    => (HashMap::new(), 0),
    };

    TopologyGraph { nodes, message_edges, parse_errors }
}

/// Read the last `FLIGHT_TAIL_BYTES` of a flight.jsonl file and extract
/// directed message edges from `message_sent` events.
fn parse_message_edges(
    path: &Path,
) -> (HashMap<(String, String), usize>, usize) {
    let mut edges: HashMap<(String, String), usize> = HashMap::new();
    let mut parse_errors: usize = 0;

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return (edges, 0),
    };
    let file_len = match file.seek(SeekFrom::End(0)) {
        Ok(n) => n,
        Err(_) => return (edges, 0),
    };
    let tail_start = file_len.saturating_sub(FLIGHT_TAIL_BYTES);
    if file.seek(SeekFrom::Start(tail_start)).is_err() {
        return (edges, 0);
    }

    let reader = std::io::BufReader::new(file);
    let mut lines = reader.lines();
    // If we seeked into the middle of the file, the first line may be partial — skip it.
    if tail_start > 0 {
        lines.next();
    }

    for line_result in lines {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => { parse_errors += 1; continue; }
        };
        let line = line.trim();
        if line.is_empty() { continue; }

        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => { parse_errors += 1; continue; }
        };

        if val.get("kind").and_then(|v| v.as_str()) == Some("message_sent") {
            let sender = val.get("agent").and_then(|v| v.as_str());
            // "to" is nested under "data" per the FlightRecorder event schema:
            //   { "agent": "...", "kind": "message_sent", "data": { "to": "...", "preview": "..." } }
            let to = val.get("data").and_then(|d| d.get("to")).and_then(|v| v.as_str());
            if let (Some(from), Some(to)) = (sender, to) {
                *edges.entry((from.to_string(), to.to_string())).or_insert(0) += 1;
            }
        }
    }

    (edges, parse_errors)
}

pub fn status_badge(status: &str) -> &'static str {
    match status {
        "running"        => "●running",
        "waiting"        => "⏸waiting",
        "deferred"       => "◌deferred",
        "awaiting_child" => "◎awaiting",
        s if s.starts_with("awaiting_approval") => "⏸pending",
        "done"           => "✓done",
        "failed"         => "✗failed",
        _                => "?unknown",
    }
}

/// Every descendant of `root` in the spawn tree, sorted, each id once, excluding `root` itself.
///
/// ux.13-TUI: the row-action overlay shows this before confirming a Cancel, because
/// `ControlCommand::Cancel` walks `parent_map` and flags the whole subtree — on this repo's own
/// `coordinator-demo.agents.toml` fixture, cancelling the coordinator stops THREE agents. Confirming
/// a blast radius nobody showed you is the design phase's C4.
///
/// **Cycle-safe by construction.** A `parent_id` cycle is a tested reality in this tree (the
/// `render_tree` cycle guard exists because it happened), and this walk runs on the RENDER thread, so
/// a naive frontier would hang the cockpit rather than merely print nonsense. The guard is the
/// scheduler's own: never enqueue an id already in the set (`scheduler.rs`'s `!subtree.contains(child)`).
///
/// The result is a FLOOR, not the truth: the snapshot is up to a poll interval stale and carries no
/// universal-tier parentage, so the server's own count can legitimately be higher. Callers must label
/// it "at least N".
pub fn descendants(graph: &TopologyGraph, root: &str) -> Vec<String> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(pid) = node.parent_id.as_deref() {
            children.entry(pid).or_default().push(node.id.as_str());
        }
    }
    // No per-parent sort: traversal order is not part of the contract (the result is sorted once at the
    // end), and sorting here implied a guarantee the LIFO walk does not make (/review).
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(root);
    let mut frontier: Vec<&str> = vec![root];
    while let Some(id) = frontier.pop() {
        for child in children.get(id).into_iter().flatten() {
            // The cycle guard: `seen` already holds `root`, so a child pointing back at its own
            // ancestor is skipped instead of re-expanded forever.
            if seen.insert(child) {
                out.push((*child).to_string());
                frontier.push(child);
            }
        }
    }
    out.sort();
    out
}

/// Render the topology graph as a list of text lines.
/// The caller handles scrolling by slicing `render_tree(...)[scroll..]`.
pub fn render_tree(graph: &TopologyGraph) -> Vec<String> {
    if graph.nodes.is_empty() {
        return vec!["  No agents running".to_string()];
    }

    let node_ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    let nodes_by_id: HashMap<&str, &NodeInfo> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Build parent_id → sorted children list.
    let mut children: HashMap<&str, Vec<&NodeInfo>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(pid) = node.parent_id.as_deref() {
            if node_ids.contains(pid) {
                children.entry(pid).or_default().push(node);
            }
        }
    }
    for v in children.values_mut() {
        v.sort_by_key(|n| n.id.as_str());
    }

    // Roots: agents with no parent, or whose parent is absent from the snapshot.
    let mut roots: Vec<&NodeInfo> = graph
        .nodes
        .iter()
        .filter(|n| n.parent_id.as_deref().is_none_or(|pid| !node_ids.contains(pid)))
        .collect();
    roots.sort_by_key(|n| n.id.as_str());

    let mut lines = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();

    for root in roots {
        render_node_rec(
            &root.id,
            "",    // no connector prefix for roots
            "  ", // 2-space indent for root's sub-content
            &nodes_by_id,
            &children,
            &graph.message_edges,
            &mut visited,
            &mut lines,
        );
        lines.push(String::new()); // blank line between trees
    }
    // Remove trailing blank separator.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push("  (no edges)".to_string());
    }
    lines
}

/// Recursive tree renderer.
///
/// - `line_prefix`    — text prepended to this node's own line (e.g. `"  ├─spawn→ "`)
/// - `subtree_prefix` — text prepended to every sub-line (msg edges, children)
#[allow(clippy::too_many_arguments)]
fn render_node_rec(
    node_id:        &str,
    line_prefix:    &str,
    subtree_prefix: &str,
    nodes_by_id:    &HashMap<&str, &NodeInfo>,
    children:       &HashMap<&str, Vec<&NodeInfo>>,
    msg_edges:      &HashMap<(String, String), usize>,
    visited:        &mut HashSet<String>,
    lines:          &mut Vec<String>,
) {
    if visited.contains(node_id) {
        lines.push(format!("{}[cycle: {}]", line_prefix, node_id));
        return;
    }
    visited.insert(node_id.to_string());

    let badge = nodes_by_id
        .get(node_id)
        .map(|n| status_badge(&n.status))
        .unwrap_or("?unknown");
    lines.push(format!("{}{} {}", line_prefix, node_id, badge));

    // Sent message edges (this node → peer).
    let mut sent: Vec<(&str, usize)> = msg_edges
        .iter()
        .filter(|((from, _), _)| from == node_id)
        .map(|((_, to), &cnt)| (to.as_str(), cnt))
        .collect();
    sent.sort_by_key(|(p, _)| *p);
    for (peer, cnt) in &sent {
        lines.push(format!("{}╌→ {}  sent {}", subtree_prefix, peer, cnt));
    }
    // Received message edges (peer → this node).
    let mut recv: Vec<(&str, usize)> = msg_edges
        .iter()
        .filter(|((_, to), _)| to == node_id)
        .map(|((from, _), &cnt)| (from.as_str(), cnt))
        .collect();
    recv.sort_by_key(|(p, _)| *p);
    for (peer, cnt) in &recv {
        lines.push(format!("{}←╌ {}  received {}", subtree_prefix, peer, cnt));
    }

    // Child spawn edges.
    let empty: Vec<&NodeInfo> = vec![];
    let kids = children.get(node_id).unwrap_or(&empty);
    for (i, child) in kids.iter().enumerate() {
        let is_last = i == kids.len() - 1;
        // connector is 9 display columns wide; continuation must match.
        let connector    = if is_last { "└─spawn→ " } else { "├─spawn→ " };
        let continuation = if is_last { "         " } else { "│        " };
        let child_line_prefix    = format!("{}{}", subtree_prefix, connector);
        let child_subtree_prefix = format!("{}{}", subtree_prefix, continuation);
        render_node_rec(
            &child.id,
            &child_line_prefix,
            &child_subtree_prefix,
            nodes_by_id,
            children,
            msg_edges,
            visited,
            lines,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::reader::{AgentInfo, BudgetKind};

    fn make_agent(id: &str, parent: Option<&str>, status: &str) -> AgentInfo {
        AgentInfo {
            id:              id.to_string(),
            status:          status.to_string(),
            status_detail:   None,
            context_tokens:  0,
            budget:          BudgetKind::Unlimited,
            windowed_spent:  0,
            tools:           vec![],
            parent_id:       parent.map(str::to_string),
            sandbox:         None,
            egress_brokered: 0,
            egress_denied:   0,
            tier:            "native".to_string(),
            isolation:       String::new(),
            pid:             0,
            attention:       vec![],
        }
    }

    // ── build_graph ───────────────────────────────────────────────────────────

    #[test]
    fn build_graph_empty_agents() {
        let g = build_graph(&[], None);
        assert!(g.nodes.is_empty());
        assert!(g.message_edges.is_empty());
        assert_eq!(g.parse_errors, 0);
    }

    #[test]
    fn build_graph_single_agent_no_log() {
        let agents = vec![make_agent("root", None, "running")];
        let g = build_graph(&agents, None);
        assert_eq!(g.nodes.len(), 1);
        assert_eq!(g.nodes[0].id, "root");
        assert!(g.nodes[0].parent_id.is_none());
    }

    #[test]
    fn build_graph_preserves_parent_id() {
        let agents = vec![
            make_agent("parent", None, "running"),
            make_agent("child", Some("parent"), "done"),
        ];
        let g = build_graph(&agents, None);
        let child = g.nodes.iter().find(|n| n.id == "child").unwrap();
        assert_eq!(child.parent_id.as_deref(), Some("parent"));
    }

    #[test]
    fn build_graph_missing_log_path_returns_empty_edges() {
        let agents = vec![make_agent("a", None, "running")];
        let g = build_graph(&agents, Some(Path::new("/nonexistent/no-such-file.jsonl")));
        assert!(g.message_edges.is_empty());
        assert_eq!(g.parse_errors, 0);
    }

    #[test]
    fn build_graph_parses_message_sent_events() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"agent":"a","kind":"message_sent","data":{{"to":"b","preview":"hi"}}}}"#).unwrap();
        writeln!(tmp, r#"{{"agent":"a","kind":"message_sent","data":{{"to":"b","preview":"hello"}}}}"#).unwrap();
        writeln!(tmp, r#"{{"agent":"b","kind":"message_sent","data":{{"to":"a","preview":"ack"}}}}"#).unwrap();
        tmp.flush().unwrap();

        let agents = vec![make_agent("a", None, "running"), make_agent("b", None, "running")];
        let g = build_graph(&agents, Some(tmp.path()));
        assert_eq!(g.message_edges.get(&("a".to_string(), "b".to_string())), Some(&2));
        assert_eq!(g.message_edges.get(&("b".to_string(), "a".to_string())), Some(&1));
    }

    #[test]
    fn build_graph_skips_malformed_lines_and_counts_errors() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "not json at all").unwrap();
        writeln!(tmp, r#"{{"agent":"a","kind":"message_sent","data":{{"to":"b"}}}}"#).unwrap();
        tmp.flush().unwrap();

        let agents = vec![make_agent("a", None, "running"), make_agent("b", None, "running")];
        let g = build_graph(&agents, Some(tmp.path()));
        assert!(g.parse_errors > 0, "malformed lines must increment parse_errors");
        assert_eq!(g.message_edges.get(&("a".to_string(), "b".to_string())), Some(&1));
    }

    // ── status_badge ─────────────────────────────────────────────────────────

    #[test]
    fn status_badge_running() { assert_eq!(status_badge("running"), "●running"); }

    #[test]
    fn status_badge_done() { assert_eq!(status_badge("done"), "✓done"); }

    #[test]
    fn status_badge_failed() { assert_eq!(status_badge("failed"), "✗failed"); }

    #[test]
    fn status_badge_unknown() { assert_eq!(status_badge("mystery"), "?unknown"); }

    // ── render_tree ───────────────────────────────────────────────────────────

    #[test]
    fn render_tree_empty_graph_shows_no_agents() {
        let g = TopologyGraph::default();
        let lines = render_tree(&g);
        assert_eq!(lines, vec!["  No agents running"]);
    }

    #[test]
    fn render_tree_single_root_no_children() {
        let g = build_graph(&[make_agent("solo", None, "running")], None);
        let lines = render_tree(&g);
        assert!(lines[0].contains("solo"), "first line must contain the root agent id");
        assert!(lines[0].contains("●running"), "first line must contain the status badge");
    }

    #[test]
    fn render_tree_parent_child_relationship() {
        let agents = vec![
            make_agent("parent", None, "running"),
            make_agent("child", Some("parent"), "done"),
        ];
        let g = build_graph(&agents, None);
        let lines = render_tree(&g);
        let all = lines.join("\n");
        assert!(all.contains("parent"), "output must contain parent id");
        assert!(all.contains("child"),  "output must contain child id");
        assert!(all.contains("spawn→"), "output must show spawn edge connector");
    }

    #[test]
    fn render_tree_cycle_guard_does_not_panic() {
        // Manually construct a graph with a cycle.
        let mut g = TopologyGraph::default();
        g.nodes.push(NodeInfo { id: "a".into(), parent_id: Some("b".into()), status: "running".into() });
        g.nodes.push(NodeInfo { id: "b".into(), parent_id: Some("a".into()), status: "running".into() });
        // A closed cycle with no roots produces "(no edges)" — no panic, no infinite loop.
        let lines = render_tree(&g);
        assert!(!lines.is_empty(), "render must return non-empty output even with a cycle");
        // Both nodes have parents in-snapshot so neither is a root; render_tree falls
        // through to the "(no edges)" sentinel rather than entering render_node_rec.
        let all = lines.join("\n");
        assert!(!all.contains("panic"), "must not panic");
    }

    #[test]
    fn render_tree_multiple_roots_blank_separated() {
        let agents = vec![
            make_agent("a", None, "running"),
            make_agent("b", None, "done"),
        ];
        let g = build_graph(&agents, None);
        let lines = render_tree(&g);
        // There should be a blank separator line between the two roots.
        assert!(lines.iter().any(|l| l.is_empty()), "multiple roots must be separated by a blank line");
    }

    #[test]
    fn render_tree_message_edges_appear_for_sender() {
        let agents = vec![
            make_agent("a", None, "running"),
            make_agent("b", None, "running"),
        ];
        let mut g = build_graph(&agents, None);
        g.message_edges.insert(("a".to_string(), "b".to_string()), 3);
        let lines = render_tree(&g);
        let all = lines.join("\n");
        assert!(all.contains("╌→"), "sent edge indicator must appear");
        assert!(all.contains("sent 3"), "sent count must appear");
    }

    #[test]
    fn render_tree_received_edges_appear_for_recipient() {
        let agents = vec![
            make_agent("a", None, "running"),
            make_agent("b", None, "running"),
        ];
        let mut g = build_graph(&agents, None);
        g.message_edges.insert(("a".to_string(), "b".to_string()), 2);
        let lines = render_tree(&g);
        let all = lines.join("\n");
        // From b's perspective, a sent to b => b received from a
        assert!(all.contains("←╌"), "received edge indicator must appear for recipient");
        assert!(all.contains("received 2"), "received count must appear");
    }

    // ── ux.13-TUI: the Cancel blast-radius walk ───────────────────────────────

    fn graph_of(agents: &[(&str, Option<&str>)]) -> TopologyGraph {
        let list: Vec<AgentInfo> = agents.iter()
            .map(|(id, parent)| make_agent(id, *parent, "running"))
            .collect();
        build_graph(&list, None)
    }

    /// C4: this is the number the confirm dialog shows. On this repo's own
    /// `coordinator-demo.agents.toml` shape, `x` on the coordinator stops THREE agents, and a dialog
    /// naming one id understates the blast radius by two.
    #[test]
    fn descendants_walks_the_whole_subtree_not_just_direct_children() {
        let g = graph_of(&[
            ("cos-coordinator", None),
            ("scout-1", Some("cos-coordinator")),
            ("scout-2", Some("cos-coordinator")),
            ("scout-2-helper", Some("scout-2")),   // a grandchild: cascade is not one level
            ("unrelated", None),
        ]);
        assert_eq!(descendants(&g, "cos-coordinator"), ["scout-1", "scout-2", "scout-2-helper"]);
        assert_eq!(descendants(&g, "scout-2"), ["scout-2-helper"]);
        assert!(descendants(&g, "unrelated").is_empty());
        assert!(descendants(&g, "nonexistent").is_empty(), "an unknown id is empty, not a panic");
    }

    /// E7: a `parent_id` cycle is a TESTED reality here (see `render_tree_cycle_guard_does_not_panic`,
    /// which exists because it happened), and this walk runs on the RENDER thread — a naive frontier
    /// would hang the cockpit mid-incident rather than merely print nonsense. The test is written to
    /// FAIL BY TIMEOUT if the guard is removed, which is the only way a hang can be asserted.
    #[test]
    fn descendants_terminates_on_a_parent_cycle() {
        let mut g = TopologyGraph::default();
        g.nodes.push(NodeInfo { id: "a".into(), parent_id: Some("c".into()), status: "running".into() });
        g.nodes.push(NodeInfo { id: "b".into(), parent_id: Some("a".into()), status: "running".into() });
        g.nodes.push(NodeInfo { id: "c".into(), parent_id: Some("b".into()), status: "running".into() });
        let found = descendants(&g, "a");
        assert_eq!(found, ["b", "c"], "each node once, and the walk must end");
        // Self-parenthood is the degenerate case.
        let mut g2 = TopologyGraph::default();
        g2.nodes.push(NodeInfo { id: "s".into(), parent_id: Some("s".into()), status: "running".into() });
        assert!(descendants(&g2, "s").is_empty(), "an agent is not its own descendant");
    }
}
