use std::{collections::HashMap, fs, io::{BufRead, BufReader}, path::Path};

use serde::Deserialize;

/// Sentinel values written by the surfaces crate into FUSE virtual files.
/// Must stay in sync with BUDGET_UNLIMITED_SENTINEL / TOOLS_NONE_SENTINEL in
/// surfaces/src/agents_fs.rs.
const BUDGET_UNLIMITED_SENTINEL: &str = "unlimited";
const TOOLS_NONE_SENTINEL: &str = "(none)";

/// Parsed content of /agents/system/budget
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysBudget {
    pub spent: u64,
    // Reserved for when the FUSE system/budget file emits the real total.
    #[allow(dead_code)]
    pub total: u64,
    /// ux.13-TUI: does `[scheduler] budget_reset_interval > 0` on the connected agentd?
    ///
    /// `false` means per-agent budget exhaustion TERMINATES the agent rather than deferring it to the
    /// next window — so the row-action overlay's Park is a kill, not a pause, and must not be labelled
    /// reversible. `#[serde(default)]` so an older agentd (whose `system/budget` file has no such key)
    /// reads as NOT resettable: the cautious direction, since it is also the config default.
    /// `alias` because the same datum has two wire names: the FUSE `system/budget` file (already scoped
    /// to budget) emits `resettable`, while the flat HTTP snapshot needs the `budget_` prefix. Accepting
    /// both means a rename on either surface cannot silently degrade this to `false` (/review).
    #[serde(default, alias = "budget_resettable")]
    pub resettable: bool,
}

/// Parsed content of /agents/system/queue
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysQueue {
    pub depth: usize,
}

/// Per-server enforcement record deserialized from /agents/system/sandbox or
/// /agents/<id>/sandbox.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ServerEnforcement {
    pub name:              String,
    #[serde(default)]
    pub transport:         String,
    #[serde(default)]
    pub isolation:         String,
    #[serde(default)]
    pub landlock:          bool,
    #[serde(default)]
    pub seccomp:           bool,
    #[serde(default)]
    pub spawn_enforcement: String,
    #[serde(default)]
    pub namespace_net:     bool,
    #[serde(default)]
    pub namespace_mount:   bool,
    #[serde(default)]
    pub landlock_net:      bool,
}

/// Parsed content of /agents/system/sandbox.
/// The `any_sandboxed` key was previously "applied" — accept both via alias.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysSandbox {
    /// `any_sandboxed` is the current key; `applied` is the pre-p6.8 alias.
    #[serde(alias = "applied")]
    pub any_sandboxed:  bool,
    #[serde(default)]
    pub servers:        Vec<ServerEnforcement>,
    #[serde(default)]
    pub degradations:   Vec<String>,
}

/// Parsed content of /agents/<id>/sandbox — per-agent view.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct AgentSandbox {
    #[serde(default)]
    pub servers: Vec<ServerEnforcement>,
}

/// Why an attention signal fired — mirrors `surfaces::AttentionReason` (ux.2a). Declaration
/// order matches the server's routing-priority order: `ApprovalPending` wins ties, then
/// `Degraded`, then `BudgetRisk`, then `EvaluationUnavailable` (lowest).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    ApprovalPending,
    Degraded,
    /// ux.2b: a tool call errored while the agent kept running (above BudgetRisk in routing).
    Error,
    BudgetRisk,
    EvaluationUnavailable,
    /// ux.2b: no completed progress event in the idle threshold — least urgent, routes last.
    Idle,
}

impl AttentionReason {
    /// Row-color severity — independent of routing priority (Design Fix 1): `Degraded` is
    /// more severe than `ApprovalPending` but does not win routing, since an approval is more
    /// actionable than most other signals even when less severe. `Error` is Critical (red)
    /// like `Degraded`; `Idle` is a Warning (yellow), not Critical.
    pub fn is_critical(&self) -> bool {
        matches!(self, AttentionReason::Degraded | AttentionReason::Error)
    }

    pub fn label(&self) -> &'static str {
        match self {
            AttentionReason::ApprovalPending       => "approval pending",
            AttentionReason::Degraded              => "degraded",
            AttentionReason::Error                 => "error",
            AttentionReason::BudgetRisk             => "budget risk",
            AttentionReason::EvaluationUnavailable => "evaluation unavailable",
            AttentionReason::Idle                  => "idle",
        }
    }
}

/// One active attention signal, parsed from /agents/<id>/attention (ux.2a).
#[derive(Deserialize, Debug, Clone)]
pub struct AttentionSignal {
    pub reason:   AttentionReason,
    #[serde(default)]
    pub since:    u64,
    #[serde(default)]
    pub evidence: Option<String>,
}

/// Parsed content of /agents/system/provider
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysProvider {
    pub model: String,
    pub backend: String,
}

/// Parsed content of /agents/system/isolation — device-level isolation tier.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysIsolation {
    /// Coarse tier: "full" | "capability" | "none".
    #[serde(default)]
    pub tier:     String,
    /// CPU architecture string (e.g. "x86_64", "aarch64").
    #[serde(default)]
    pub arch:     String,
    /// Absolute path to runsc binary, or None when gVisor absent.
    #[serde(default)]
    pub runsc:    Option<String>,
    /// True when Landlock ABI ≥ 1 is available.
    #[serde(default)]
    pub landlock: bool,
    /// True when seccomp-bpf enforcement is available (x86_64 only).
    #[serde(default)]
    pub seccomp:  bool,
}

/// One pending approval request, parsed from a JSON line in /agents/approvals.
#[derive(Deserialize, Debug, Clone)]
pub struct PendingAction {
    pub id:       String,
    pub agent_id: String,
    pub kind:     String,
    pub risk:     String,
    pub summary:  String,
    /// JSON-encoded args object (raw Value for flexible display in future views).
    #[serde(default)]
    #[allow(dead_code)]
    pub args:     serde_json::Value,
    #[serde(default)]
    pub age_secs: u64,
}

/// Snapshot of one running agent, assembled from per-file reads.
#[derive(Debug, Clone, Default)]
pub struct AgentInfo {
    pub id:               String,
    pub status:           String,
    /// Extra context for tuple-variant statuses: child ID for awaiting_child,
    /// approval ID for awaiting_approval. None for all other statuses.
    pub status_detail:    Option<String>,
    pub context_tokens:   u64,
    pub budget:           BudgetKind,
    /// Spend within the current budget window (ux.11a). Equals lifetime spend under
    /// legacy (interval=0) budgets. Rendered against `budget` in the agent list.
    pub windowed_spent:   u64,
    pub tools:            Vec<String>,
    pub parent_id:        Option<String>,
    pub sandbox:          Option<AgentSandbox>,
    pub egress_brokered:  u64,
    pub egress_denied:    u64,
    /// "native" | "universal" — parsed from /agents/<id>/tier
    pub tier:             String,
    /// Effective isolation mode for universal agents: "gvisor" | "none".
    pub isolation:        String,
    /// PID of the child process for universal-tier agents; 0 for native.
    pub pid:              u32,
    /// Active attention signals (ux.2a). Empty means "evaluated, clean" — see
    /// `AttentionReason::EvaluationUnavailable` for the "couldn't tell" case.
    pub attention:        Vec<AttentionSignal>,
}

#[derive(Debug, Clone, Default)]
pub enum BudgetKind {
    #[default]
    Unlimited,
    Tokens(u64),
}

impl BudgetKind {
    pub fn display(&self) -> String {
        match self {
            BudgetKind::Unlimited  => "unlimited".to_string(),
            BudgetKind::Tokens(n)  => format!("{n}"),
        }
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Outcome of a read that needs to distinguish "file genuinely doesn't exist" from "a real
/// read error occurred" — used only where that distinction matters (currently
/// `read_agent_attention`). `read_trimmed` above collapses both to `None`, which is fine for
/// every other call site (they already treat "no content" as "use a fallback default"), but
/// wrong for a signal that must never silently render as "evaluated, clean" on account of a
/// transient FUSE hiccup or a permission bounce (ship-review finding, Claude adversarial
/// subagent: only `NotFound` should mean "genuinely missing"; every other `io::ErrorKind` is a
/// real failure and must produce `EvaluationUnavailable`, same as an unparseable file).
enum ReadOutcome {
    Missing,
    Content(String),
    Error,
}

fn read_trimmed_checked(path: &Path) -> ReadOutcome {
    match fs::read_to_string(path) {
        Ok(s) => ReadOutcome::Content(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReadOutcome::Missing,
        Err(_) => ReadOutcome::Error,
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&read_trimmed(path)?).ok()
}

/// Read and sort the list of agent IDs from the FUSE mountpoint root.
pub fn read_agent_ids(agents_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(agents_dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "system" || name == "kb" || name.starts_with('.') {
            continue;
        }
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids)
}

/// Read all virtual files for one agent directory.
pub fn read_agent_info(agents_dir: &Path, id: &str) -> AgentInfo {
    let dir = agents_dir.join(id);
    let status = read_trimmed(&dir.join("status")).unwrap_or_else(|| "unknown".to_string());
    let context_tokens = read_trimmed(&dir.join("context_size"))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let budget = match read_trimmed(&dir.join("budget")).as_deref() {
        Some(s) if s == BUDGET_UNLIMITED_SENTINEL => BudgetKind::Unlimited,
        None => BudgetKind::Unlimited,
        // u64::MAX is an unlimited sentinel too — mirror the HTTP source's mapping
        // (source.rs) so the two reader paths don't disagree (Claude ship review).
        Some(s) => match s.parse::<u64>() {
            Ok(u64::MAX) | Ok(0) => BudgetKind::Unlimited,
            Ok(n)                => BudgetKind::Tokens(n),
            Err(_)               => BudgetKind::Unlimited,
        },
    };
    let windowed_spent = read_trimmed(&dir.join("windowed_spend"))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(context_tokens);
    let tools = match read_trimmed(&dir.join("tools")).as_deref() {
        Some(s) if s == TOOLS_NONE_SENTINEL => vec![],
        None => vec![],
        Some(s) => s.lines().map(str::to_string).collect(),
    };
    let parent_id = match read_trimmed(&dir.join("parent")).as_deref() {
        Some("(none)") | None => None,
        Some(s)               => Some(s.to_string()),
    };
    let sandbox = read_agent_sandbox(agents_dir, id);
    let tier_raw = read_trimmed(&dir.join("tier")).unwrap_or_else(|| "native".to_string());
    // Tier file encodes isolation for universal agents as "universal:gvisor" or "universal:none".
    let (tier, isolation) = if let Some((t, iso)) = tier_raw.split_once(':') {
        (t.to_string(), iso.to_string())
    } else {
        (tier_raw, String::new())
    };
    let pid = read_trimmed(&dir.join("pid"))
        .and_then(|s| if s == "(none)" { None } else { s.parse::<u32>().ok() })
        .unwrap_or(0);
    let attention = read_agent_attention(agents_dir, id);
    AgentInfo {
        id: id.to_string(),
        status,
        status_detail: None,
        context_tokens,
        budget,
        windowed_spent,
        tools,
        parent_id,
        sandbox,
        egress_brokered: 0,
        egress_denied:   0,
        tier,
        isolation,
        pid,
        attention,
    }
}

/// Read /agents/<id>/sandbox
pub fn read_agent_sandbox(agents_dir: &Path, id: &str) -> Option<AgentSandbox> {
    read_json(&agents_dir.join(id).join("sandbox"))
}

/// Read /agents/<id>/attention (ux.2a). **Distinguishes "file missing" (no attention data
/// yet — an older agentd, or a brand-new agent — genuinely Clean) from "a real read/parse
/// failure"** (file present but unparseable, OR an io error other than NotFound — a transient
/// FUSE hiccup, a permission bounce). The latter must render as `EvaluationUnavailable`, never
/// silently collapse to Clean (Design Review's CRITICAL finding: a failed read must never be
/// mistaken for "nothing wrong" — an adversarial review found the original
/// `read_json(...).unwrap_or_default()` form collapsed BOTH cases to empty/Clean, defeating
/// that guarantee entirely; a follow-up adversarial pass found `read_trimmed`'s blanket
/// `Result::ok()` still collapsed non-NotFound io errors the same way — fixed here).
pub fn read_agent_attention(agents_dir: &Path, id: &str) -> Vec<AttentionSignal> {
    let evaluation_unavailable = || AttentionSignal {
        reason:   AttentionReason::EvaluationUnavailable,
        since:    now_unix(),
        evidence: Some("attention_file".to_string()),
    };
    match read_trimmed_checked(&agents_dir.join(id).join("attention")) {
        ReadOutcome::Missing => vec![],
        ReadOutcome::Error   => vec![evaluation_unavailable()],
        ReadOutcome::Content(content) => match serde_json::from_str::<Vec<AttentionSignal>>(&content) {
            Ok(signals) => signals,
            Err(_) => vec![evaluation_unavailable()],
        },
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Scan flight.jsonl and count `egress_brokered` and `egress_denied` events per
/// agent. Returns a map of agent_id → (brokered_count, denied_count).
///
/// The FlightRecorder wraps every event payload under a "data" key, so the
/// agent field lives at `data.agent`.
pub fn count_egress_by_agent(log_path: &Path) -> HashMap<String, (u64, u64)> {
    let file = match fs::File::open(log_path) {
        Ok(f)  => f,
        Err(_) => return HashMap::new(),
    };
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let is_brokered = line.contains("\"egress_brokered\"");
        let is_denied   = line.contains("\"egress_denied\"");
        if !is_brokered && !is_denied { continue; }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            let agent = val["data"]["agent"].as_str().unwrap_or("").to_string();
            if agent.is_empty() { continue; }
            let entry = counts.entry(agent).or_default();
            if is_brokered { entry.0 += 1; }
            if is_denied   { entry.1 += 1; }
        }
    }
    counts
}

/// Read /agents/approvals and parse each JSON line into a PendingAction.
///
/// Returns an empty vec when the file is absent, empty, or contains the "[]" sentinel
/// that agentd writes when there are no pending approvals.
pub fn read_approvals(agents_dir: &Path) -> Vec<PendingAction> {
    let content = match fs::read_to_string(agents_dir.join("approvals")) {
        Ok(s)  => s,
        Err(_) => return vec![],
    };
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return vec![];
    }
    trimmed
        .lines()
        .filter(|l| !l.is_empty() && *l != "[]")
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Read /agents/system/budget
pub fn read_sys_budget(agents_dir: &Path) -> Option<SysBudget> {
    read_json(&agents_dir.join("system").join("budget"))
}

/// Read /agents/system/queue
pub fn read_sys_queue(agents_dir: &Path) -> Option<SysQueue> {
    read_json(&agents_dir.join("system").join("queue"))
}

/// Read /agents/system/sandbox
pub fn read_sys_sandbox(agents_dir: &Path) -> Option<SysSandbox> {
    read_json(&agents_dir.join("system").join("sandbox"))
}

/// Read /agents/system/provider
pub fn read_sys_provider(agents_dir: &Path) -> Option<SysProvider> {
    read_json(&agents_dir.join("system").join("provider"))
}

/// Load a full snapshot: agent list + system files.
pub struct Snapshot {
    pub agents:      Vec<AgentInfo>,
    pub budget:      Option<SysBudget>,
    pub queue:       Option<SysQueue>,
    pub sandbox:     Option<SysSandbox>,
    pub provider:    Option<SysProvider>,
    pub isolation:   Option<SysIsolation>,
    pub credentials: Option<SysCredentials>,
    pub error:       Option<String>,
}

/// Read /agents/system/isolation
pub fn read_sys_isolation(agents_dir: &Path) -> Option<SysIsolation> {
    read_json(&agents_dir.join("system").join("isolation"))
}

/// Health of a single credential provider, deserialized from FUSE or HTTP.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ProvHealthInfo {
    pub name:            String,
    #[serde(default)]
    pub token_fresh:     bool,
    #[serde(default)]
    pub last_refresh_at: Option<u64>,
    #[serde(default)]
    pub expires_at:      Option<u64>,
    #[serde(default)]
    pub last_error:      Option<String>,
}

/// Parsed content of /agents/system/credentials or GET /api/v1/credentials.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SysCredentials {
    #[serde(default)]
    pub gateway_enabled:      bool,
    #[serde(default)]
    pub configured_providers: Vec<String>,
    #[serde(default)]
    pub provider_health:      Vec<ProvHealthInfo>,
}

/// Read /agents/system/credentials
pub fn read_sys_credentials(agents_dir: &Path) -> Option<SysCredentials> {
    read_json(&agents_dir.join("system").join("credentials"))
}

pub fn load_snapshot(agents_dir: &Path) -> Snapshot {
    let (agents, error) = match read_agent_ids(agents_dir) {
        Ok(ids) => {
            let agents = ids.iter().map(|id| read_agent_info(agents_dir, id)).collect();
            (agents, None)
        }
        Err(e) => (vec![], Some(format!("{e:#}"))),
    };
    Snapshot {
        budget:      read_sys_budget(agents_dir),
        queue:       read_sys_queue(agents_dir),
        sandbox:     read_sys_sandbox(agents_dir),
        provider:    read_sys_provider(agents_dir),
        isolation:   read_sys_isolation(agents_dir),
        credentials: read_sys_credentials(agents_dir),
        agents,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_isolation_full_parses() {
        let json = r#"{"tier":"full","arch":"x86_64","runsc":"/usr/bin/runsc","landlock":true,"seccomp":true}"#;
        let s: SysIsolation = serde_json::from_str(json).unwrap();
        assert_eq!(s.tier, "full");
        assert_eq!(s.arch, "x86_64");
        assert_eq!(s.runsc.as_deref(), Some("/usr/bin/runsc"));
        assert!(s.landlock);
        assert!(s.seccomp);
    }

    #[test]
    fn sys_isolation_none_parses() {
        let json = r#"{"tier":"none","arch":"aarch64","runsc":null,"landlock":false,"seccomp":false}"#;
        let s: SysIsolation = serde_json::from_str(json).unwrap();
        assert_eq!(s.tier, "none");
        assert!(s.runsc.is_none());
        assert!(!s.landlock);
        assert!(!s.seccomp);
    }

    #[test]
    fn sys_isolation_capability_landlock_only() {
        let json = r#"{"tier":"capability","arch":"aarch64","runsc":null,"landlock":true,"seccomp":false}"#;
        let s: SysIsolation = serde_json::from_str(json).unwrap();
        assert_eq!(s.tier, "capability");
        assert!(s.landlock);
        assert!(!s.seccomp);
    }

    #[test]
    fn read_sys_isolation_returns_none_when_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("system")).unwrap();
        let result = read_sys_isolation(tmp.path());
        assert!(result.is_none(), "missing isolation file must return None");
    }

    #[test]
    fn read_sys_isolation_parses_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("system");
        std::fs::create_dir(&sys).unwrap();
        std::fs::write(
            sys.join("isolation"),
            r#"{"tier":"capability","arch":"x86_64","runsc":null,"landlock":true,"seccomp":false}"#,
        ).unwrap();
        let result = read_sys_isolation(tmp.path());
        assert!(result.is_some());
        let iso = result.unwrap();
        assert_eq!(iso.tier, "capability");
        assert!(iso.landlock);
        assert!(!iso.seccomp);
    }

    #[test]
    fn sys_budget_parses_json() {
        let s: SysBudget = serde_json::from_str(r#"{"spent":12345,"total":0}"#).unwrap();
        assert_eq!(s.spent, 12345);
        assert_eq!(s.total, 0);
    }

    #[test]
    fn sys_queue_parses_json() {
        let s: SysQueue = serde_json::from_str(r#"{"depth":3}"#).unwrap();
        assert_eq!(s.depth, 3);
    }

    #[test]
    fn sys_sandbox_false_parses() {
        let s: SysSandbox = serde_json::from_str(
            r#"{"any_sandboxed":false,"servers":[],"degradations":[]}"#
        ).unwrap();
        assert!(!s.any_sandboxed);
    }

    #[test]
    fn sys_sandbox_true_parses() {
        let s: SysSandbox = serde_json::from_str(
            r#"{"any_sandboxed":true,"servers":[],"degradations":[]}"#
        ).unwrap();
        assert!(s.any_sandboxed);
    }

    #[test]
    fn sys_sandbox_applied_alias_accepted() {
        // Pre-p6.8 "applied" key must still deserialize correctly.
        let s: SysSandbox = serde_json::from_str(r#"{"applied":true}"#).unwrap();
        assert!(s.any_sandboxed, "alias 'applied' must map to any_sandboxed");
    }

    #[test]
    fn sys_sandbox_servers_and_degradations_parse() {
        let json = r#"{
            "any_sandboxed": true,
            "servers": [
                {"name":"search","isolation":"none","landlock":true,"seccomp":true,
                 "spawn_enforcement":"fork_vfork_only","namespace_net":false,
                 "namespace_mount":false,"landlock_net":false}
            ],
            "degradations": ["landlock_net_unavailable"]
        }"#;
        let s: SysSandbox = serde_json::from_str(json).unwrap();
        assert!(s.any_sandboxed);
        assert_eq!(s.servers.len(), 1);
        assert_eq!(s.servers[0].name, "search");
        assert!(s.servers[0].landlock);
        assert_eq!(s.degradations, vec!["landlock_net_unavailable"]);
    }

    #[test]
    fn agent_sandbox_no_servers_parses() {
        let s: AgentSandbox = serde_json::from_str(r#"{"servers":[]}"#).unwrap();
        assert!(s.servers.is_empty());
    }

    #[test]
    fn agent_sandbox_with_server_parses() {
        let json = r#"{"servers":[
            {"name":"fs","isolation":"none","landlock":true,"seccomp":false,
             "spawn_enforcement":"none","namespace_net":false,
             "namespace_mount":false,"landlock_net":false}
        ]}"#;
        let s: AgentSandbox = serde_json::from_str(json).unwrap();
        assert_eq!(s.servers.len(), 1);
        assert_eq!(s.servers[0].name, "fs");
        assert!(s.servers[0].landlock);
        assert!(!s.servers[0].seccomp);
    }

    #[test]
    fn read_agent_sandbox_returns_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let result = read_agent_sandbox(tmp.path(), "a");
        assert!(result.is_none(), "missing sandbox file must return None");
    }

    #[test]
    fn read_agent_sandbox_parses_empty_servers() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("sandbox", r#"{"servers":[]}"#)]);
        let result = read_agent_sandbox(tmp.path(), "a");
        assert!(result.is_some());
        assert!(result.unwrap().servers.is_empty());
    }

    #[test]
    fn read_agent_attention_returns_empty_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let result = read_agent_attention(tmp.path(), "a");
        assert!(result.is_empty(), "missing attention file must default to empty (clean), not panic");
    }

    #[test]
    fn read_agent_attention_parses_signal() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[
            ("attention", r#"[{"reason":"approval_pending","since":10,"evidence":"act_1"}]"#),
        ]);
        let result = read_agent_attention(tmp.path(), "a");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reason, AttentionReason::ApprovalPending);
        assert_eq!(result[0].evidence.as_deref(), Some("act_1"));
    }

    #[test]
    fn read_agent_attention_malformed_json_becomes_evaluation_unavailable_not_clean() {
        // A present-but-unparseable file is a real failure, not "nothing wrong" — must never
        // silently collapse to Clean (Design Review CRITICAL finding; adversarial review
        // caught this exact collapse in the original implementation).
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("attention", "not json")]);
        let result = read_agent_attention(tmp.path(), "a");
        assert_eq!(result.len(), 1, "malformed attention file must not panic, but also must not silently vanish");
        assert_eq!(result[0].reason, AttentionReason::EvaluationUnavailable);
    }

    /// Ship-review finding (Claude adversarial subagent): `read_trimmed`'s blanket `Result::ok()`
    /// collapsed EVERY io error (not just NotFound) to None → Clean, silently repeating the
    /// same "failed read looks like nothing wrong" bug the malformed-JSON case above already
    /// fixed. This test forces a real non-NotFound io error (NotADirectory, portable across
    /// platforms) by making the "attention" path component a file instead of a directory.
    #[test]
    fn read_agent_attention_non_not_found_io_error_becomes_evaluation_unavailable_not_clean() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        // "attention" is a FILE here, not a directory — so joining it with a further path
        // component and reading fails with NotADirectory, not NotFound.
        std::fs::write(tmp.path().join("a").join("attention"), b"irrelevant").unwrap();
        let bogus_dir = tmp.path().join("a").join("attention").join("nested");
        let result = read_agent_attention(&bogus_dir, "x");
        assert_eq!(
            result.len(), 1,
            "a real (non-NotFound) io error must render EvaluationUnavailable, not silently vanish to Clean"
        );
        assert_eq!(result[0].reason, AttentionReason::EvaluationUnavailable);
    }

    #[test]
    fn read_agent_info_populates_sandbox_field() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[
            ("status", "running\n"),
            ("sandbox", r#"{"servers":[]}"#),
        ]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(info.sandbox.is_some(), "sandbox field must be populated from file");
    }

    #[test]
    fn sys_provider_parses_json() {
        let s: SysProvider = serde_json::from_str(
            r#"{"model":"claude-sonnet-4-6","backend":"anthropic"}"#
        ).unwrap();
        assert_eq!(s.model, "claude-sonnet-4-6");
        assert_eq!(s.backend, "anthropic");
    }

    #[test]
    fn budget_kind_unlimited_displays_correctly() {
        assert_eq!(BudgetKind::Unlimited.display(), "unlimited");
    }

    #[test]
    fn budget_kind_tokens_displays_number() {
        assert_eq!(BudgetKind::Tokens(50_000).display(), "50000");
    }

    #[test]
    fn sys_budget_default_is_zero() {
        let b = SysBudget::default();
        assert_eq!(b.spent, 0);
        assert_eq!(b.total, 0);
    }

    // ── BudgetKind edge cases ─────────────────────────────────────────────────

    #[test]
    fn budget_kind_zero_tokens() {
        assert_eq!(BudgetKind::Tokens(0).display(), "0");
    }

    #[test]
    fn budget_kind_large_tokens() {
        assert_eq!(BudgetKind::Tokens(u64::MAX).display(), u64::MAX.to_string());
    }

    // ── read_agent_ids: filesystem-based tests ───────────────────────────────

    #[test]
    fn read_agent_ids_skips_system_kb_and_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Create directories that should be skipped.
        std::fs::create_dir(dir.join("system")).unwrap();
        std::fs::create_dir(dir.join("kb")).unwrap();
        std::fs::create_dir(dir.join(".hidden")).unwrap();
        // Create one real agent dir.
        std::fs::create_dir(dir.join("scout-1")).unwrap();
        let ids = read_agent_ids(dir).unwrap();
        assert_eq!(ids, vec!["scout-1"]);
    }

    #[test]
    fn read_agent_ids_skips_plain_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A plain file must not be returned — only dirs.
        std::fs::write(dir.join("not-a-dir"), b"content").unwrap();
        std::fs::create_dir(dir.join("real-agent")).unwrap();
        let ids = read_agent_ids(dir).unwrap();
        assert_eq!(ids, vec!["real-agent"]);
    }

    #[test]
    fn read_agent_ids_returns_sorted_list() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in &["gamma", "alpha", "beta"] {
            std::fs::create_dir(dir.join(name)).unwrap();
        }
        let ids = read_agent_ids(dir).unwrap();
        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn read_agent_ids_empty_dir_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        let ids = read_agent_ids(tmp.path()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn read_agent_ids_nonexistent_dir_returns_err() {
        let result = read_agent_ids(std::path::Path::new("/nonexistent/no/such/path"));
        assert!(result.is_err());
    }

    // ── read_agent_info: filesystem-based tests ──────────────────────────────

    fn write_agent_files(dir: &std::path::Path, id: &str, files: &[(&str, &str)]) {
        let agent_dir = dir.join(id);
        std::fs::create_dir_all(&agent_dir).unwrap();
        for (name, content) in files {
            std::fs::write(agent_dir.join(name), content).unwrap();
        }
    }

    #[test]
    fn read_agent_info_returns_unknown_status_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.status, "unknown");
    }

    #[test]
    fn read_agent_info_reads_status_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("status", "running\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.status, "running");
    }

    #[test]
    fn read_agent_info_reads_context_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("context_size", "4321\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.context_tokens, 4321);
    }

    #[test]
    fn read_agent_info_context_tokens_defaults_to_zero_on_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.context_tokens, 0);
    }

    #[test]
    fn read_agent_info_context_tokens_defaults_to_zero_on_unparseable() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("context_size", "not-a-number\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.context_tokens, 0);
    }

    #[test]
    fn read_agent_info_budget_unlimited_string() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("budget", "unlimited\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(matches!(info.budget, BudgetKind::Unlimited));
    }

    #[test]
    fn read_agent_info_budget_numeric_string() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("budget", "100000\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(matches!(info.budget, BudgetKind::Tokens(100_000)));
    }

    #[test]
    fn read_agent_info_budget_unparseable_falls_back_to_unlimited() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("budget", "garbage\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(matches!(info.budget, BudgetKind::Unlimited),
            "unparseable budget must fall back to Unlimited");
    }

    #[test]
    fn read_agent_info_tools_none_string_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("tools", "(none)\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(info.tools.is_empty());
    }

    #[test]
    fn read_agent_info_tools_missing_file_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let info = read_agent_info(tmp.path(), "a");
        assert!(info.tools.is_empty());
    }

    #[test]
    fn read_agent_info_tools_newline_separated_list() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("tools", "read_file\nwrite_file\nlist_dir\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.tools, vec!["read_file", "write_file", "list_dir"]);
    }

    // ── read_agent_info: parent_id ───────────────────────────────────────────

    #[test]
    fn read_agent_info_parent_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        // Only create the agent dir, no "parent" file.
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        let info = read_agent_info(tmp.path(), "a");
        assert!(info.parent_id.is_none(), "missing parent file must yield None");
    }

    #[test]
    fn read_agent_info_parent_sentinel_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("parent", "(none)\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(info.parent_id.is_none(), "\"(none)\" sentinel must yield None");
    }

    #[test]
    fn read_agent_info_parent_id_returns_some() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("parent", "coordinator\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert_eq!(info.parent_id.as_deref(), Some("coordinator"));
    }

    // ── load_snapshot: error path ─────────────────────────────────────────────

    #[test]
    fn load_snapshot_nonexistent_dir_produces_error_field() {
        let snap = load_snapshot(std::path::Path::new("/nonexistent/no/such/path"));
        assert!(snap.error.is_some(), "load_snapshot must populate error when dir doesn't exist");
        assert!(snap.agents.is_empty(), "agents must be empty on error");
    }

    #[test]
    fn load_snapshot_valid_dir_has_no_error() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = load_snapshot(tmp.path());
        assert!(snap.error.is_none());
        assert!(snap.agents.is_empty());
    }

    #[test]
    fn read_agent_info_budget_negative_string_falls_back_to_unlimited() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent_files(tmp.path(), "a", &[("budget", "-1\n")]);
        let info = read_agent_info(tmp.path(), "a");
        assert!(matches!(info.budget, BudgetKind::Unlimited),
            "negative budget string must fall back to Unlimited");
    }

    #[test]
    fn read_sys_isolation_returns_none_on_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = tmp.path().join("system");
        std::fs::create_dir(&sys).unwrap();
        std::fs::write(sys.join("isolation"), b"not valid json!!!").unwrap();
        let result = read_sys_isolation(tmp.path());
        assert!(result.is_none(), "malformed JSON must return None, not panic");
    }
}
