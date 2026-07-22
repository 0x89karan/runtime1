//! DataSource abstraction: FUSE filesystem vs. management HTTP API (p7.7/dx.2).
//!
//! The trait provides snapshot loading plus approval read/write, so all callers
//! work identically whether agentd is local (FUSE) or remote (HTTP).

use serde_json::Value;

use super::reader::{
    self, AgentInfo, AgentSandbox, AttentionSignal, BudgetKind, PendingAction, ProvHealthInfo,
    ServerEnforcement, Snapshot, SysBudget, SysCredentials, SysIsolation, SysProvider, SysQueue,
    SysSandbox,
};

/// Spawn request sent to the management API (orch.1+).
#[derive(Debug, serde::Serialize)]
pub struct SpawnRequest {
    pub task:         String,
    pub id:           Option<String>,
    pub max_turns:    Option<u32>,
    pub token_budget: Option<u64>,
    /// When true, agent parks after each response awaiting next inject.
    pub orchestrated: bool,
}

/// Abstraction over FUSE filesystem and management HTTP API.
pub trait DataSource: Send + Sync {
    /// Load the current scheduler snapshot, including all agent info and system stats.
    fn load_snapshot(&self) -> Snapshot;
    /// Load the current list of pending operator approvals.
    fn load_approvals(&self) -> Vec<PendingAction>;
    /// Approve the pending action with the given id. Fail-closed: any error returns Err.
    fn approve(&self, id: &str) -> Result<(), String>;
    /// Deny the pending action with the given id and an optional reason. Fail-closed.
    fn deny(&self, id: &str, reason: Option<&str>) -> Result<(), String>;
    /// Approve and set an auto-approval rule for all future actions of the same kind.
    /// Falls back to plain approve on implementations that don't support it (HTTP path).
    fn approve_with_kind(&self, id: &str, _kind: &str) -> Result<(), String> {
        self.approve(id)
    }
    /// Spawn a new orchestrated agent. Returns the resolved agent ID or an error.
    fn spawn(&self, _req: &SpawnRequest) -> Result<String, String> {
        Err("spawn not supported on this data source (use --url to connect to management API)".to_string())
    }
    /// Inject a new user turn into a waiting orchestrated agent.
    fn inject(&self, _agent_id: &str, _text: &str) -> Result<(), String> {
        Err("inject not supported on this data source (use --url to connect to management API)".to_string())
    }
    /// Returns the base URL for SSE event streaming, if supported.
    fn event_stream_url(&self) -> Option<String> {
        None
    }
}

// ── FuseSource ─────────────────────────────────────────────────────────────

/// DataSource backed by the FUSE `/agents` filesystem.
pub struct FuseSource {
    pub agents_dir: std::path::PathBuf,
}

impl DataSource for FuseSource {
    fn load_snapshot(&self) -> Snapshot {
        // reader::load_snapshot already populates isolation via read_sys_isolation().
        reader::load_snapshot(&self.agents_dir)
    }

    fn load_approvals(&self) -> Vec<PendingAction> {
        reader::read_approvals(&self.agents_dir)
    }

    fn approve(&self, id: &str) -> Result<(), String> {
        let payload = serde_json::json!({"approve": {"id": id}});
        write_control_command(&self.agents_dir, &payload.to_string())
    }

    fn approve_with_kind(&self, id: &str, kind: &str) -> Result<(), String> {
        let payload = serde_json::json!({"approve": {"id": id, "auto_approve_kind": kind}});
        write_control_command(&self.agents_dir, &payload.to_string())
    }

    fn deny(&self, id: &str, reason: Option<&str>) -> Result<(), String> {
        let payload = if let Some(r) = reason {
            serde_json::json!({"reject": {"id": id, "reason": r}})
        } else {
            serde_json::json!({"reject": {"id": id}})
        };
        write_control_command(&self.agents_dir, &payload.to_string())
    }

    fn inject(&self, agent_id: &str, text: &str) -> Result<(), String> {
        let payload = serde_json::json!({"inject": {"agent_id": agent_id, "text": text}});
        write_control_command(&self.agents_dir, &payload.to_string())
    }
}

// ── HttpSource ──────────────────────────────────────────────────────────────

/// DataSource backed by the management HTTP API (p7.7).
pub struct HttpSource {
    pub base_url:    String,
    client:          reqwest::blocking::Client,
    mutation_client: reqwest::blocking::Client,
    /// Spawn waits up to 2 s for scheduler confirmation; this client must exceed that.
    spawn_client:    reqwest::blocking::Client,
}

impl HttpSource {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        // Shorter timeout for mutations: approve/deny block the TUI event loop.
        let mutation_client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap_or_default();
        // Spawn blocks until the scheduler confirms the agent ID (2 s server timeout).
        // This client must exceed that window to avoid spurious 500ms timeouts that
        // leave the agent running on the server while the caller sees a failure.
        let spawn_client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        Self { base_url, client, mutation_client, spawn_client }
    }

    fn get_json(&self, path: &str) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let resp = self.client.get(&url).send()?;
        Ok(resp.json()?)
    }

    /// Fetch the morning brief (ux.11c). HTTP-only surface (no FUSE); `agentctl brief`
    /// calls this on an `HttpSource` directly. `n` requests the last N briefs.
    pub fn brief(&self, n: Option<usize>) -> anyhow::Result<Value> {
        match n {
            Some(k) => self.get_json(&format!("/api/v1/brief?n={k}")),
            None => self.get_json("/api/v1/brief"),
        }
    }

    fn post_mutation(&self, path: &str, body: Option<&Value>) -> Result<(), String> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let mut req = self.mutation_client.post(&url);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().map_err(|e| format!("HTTP error: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {} — action stays pending", resp.status().as_u16()))
        }
    }

}

impl DataSource for HttpSource {
    fn load_snapshot(&self) -> Snapshot {
        let val = match self.get_json("/api/v1/snapshot") {
            Ok(v)  => v,
            Err(e) => return Snapshot {
                agents:      vec![],
                budget:      None,
                queue:       None,
                sandbox:     None,
                provider:    None,
                isolation:   None,
                credentials: None,
                error:       Some(format!("HTTP error: {e:#}")),
            },
        };

        let agents: Vec<AgentInfo> = val["agents"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(agent_info_from_json)
            .collect();

        let budget = val["global_tokens_spent"].as_u64().map(|spent| SysBudget {
            spent,
            total: 0,
        });

        let queue = val["queue_depth"].as_u64().map(|d| SysQueue {
            depth: d as usize,
        });

        let sandbox = sandbox_from_json(&val["sandbox"]);

        let provider = val["provider_model"].as_str().map(|m| SysProvider {
            model:   m.to_string(),
            backend: "anthropic".to_string(),
        });

        let isolation = isolation_from_json(&val["isolation_caps"]);

        let credentials = self.get_json("/api/v1/credentials").ok()
            .and_then(|v| credentials_from_json(&v));

        Snapshot { agents, budget, queue, sandbox, provider, isolation, credentials, error: None }
    }

    fn load_approvals(&self) -> Vec<PendingAction> {
        match self.get_json("/api/v1/approvals") {
            Ok(v) => v.as_array().unwrap_or(&vec![]).iter().map(pending_action_from_json).collect(),
            Err(_) => vec![],
        }
    }

    fn approve(&self, id: &str) -> Result<(), String> {
        self.post_mutation(&format!("/api/v1/approvals/{id}/approve"), None)
    }

    fn deny(&self, id: &str, reason: Option<&str>) -> Result<(), String> {
        let body = reason.map(|r| serde_json::json!({"reason": r}));
        self.post_mutation(&format!("/api/v1/approvals/{id}/deny"), body.as_ref())
    }

    fn spawn(&self, req: &SpawnRequest) -> Result<String, String> {
        let body = serde_json::to_value(req).map_err(|e| e.to_string())?;
        // Use spawn_client (3 s) — server holds the connection open for up to 2 s
        // waiting for the scheduler to confirm the agent ID. The short mutation_client
        // (500 ms) would time out before the confirmation arrives.
        let url = format!("{}/api/v1/spawn", self.base_url.trim_end_matches('/'));
        let resp = self.spawn_client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| format!("spawn HTTP error: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let detail = resp.text().unwrap_or_default();
            return Err(if detail.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!("HTTP {}: {}", status.as_u16(), detail.trim())
            });
        }
        let val: serde_json::Value = resp.json().map_err(|e| format!("JSON decode error: {e}"))?;
        // Server returns 201 + {"agent_id": "..."} after confirmation (ar-02).
        let id = val["agent_id"].as_str().unwrap_or("operator-agent").to_string();
        Ok(id)
    }

    fn inject(&self, agent_id: &str, text: &str) -> Result<(), String> {
        let body = serde_json::json!({"text": text});
        self.post_mutation(&format!("/api/v1/agents/{agent_id}/inject"), Some(&body))
    }

    fn event_stream_url(&self) -> Option<String> {
        Some(format!("{}/api/v1/events", self.base_url.trim_end_matches('/')))
    }
}

// ── JSON → reader type conversions ─────────────────────────────────────────

pub(crate) fn pending_action_from_json(v: &Value) -> PendingAction {
    PendingAction {
        id:       v["id"].as_str().unwrap_or("").to_string(),
        agent_id: v["agent_id"].as_str().unwrap_or("").to_string(),
        kind:     v["kind"].as_str().unwrap_or("").to_string(),
        risk:     v["risk"].as_str().unwrap_or("low").to_string(),
        summary:  v["summary"].as_str().unwrap_or("").to_string(),
        args:     v["args"].clone(),
        age_secs: v["age_secs"].as_u64().unwrap_or(0),
    }
}

fn agent_info_from_json(v: &Value) -> AgentInfo {
    let id = v["id"].as_str().unwrap_or("").to_string();
    let status = v["status"].as_str().unwrap_or("unknown").to_string();
    let status_detail = v["status_detail"].as_str().map(str::to_string);
    let context_tokens = v["context_tokens"].as_u64().unwrap_or(0);
    // Older agentd (pre-ux.11a) omits windowed_spent — fall back to lifetime spend.
    let windowed_spent = v["windowed_spent"].as_u64().unwrap_or(context_tokens);

    let budget = match v["token_budget"].as_u64() {
        Some(u64::MAX) | None => BudgetKind::Unlimited,
        Some(0)               => BudgetKind::Unlimited,
        Some(n)               => BudgetKind::Tokens(n),
    };

    let tools: Vec<String> = v["tools"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();

    let parent_id = v["parent_id"].as_str().map(str::to_string);

    // tier field: "native" | "universal" or "universal:gvisor" etc.
    let tier_raw = v["tier"].as_str().unwrap_or("native").to_string();
    let (tier, isolation) = if let Some((t, iso)) = tier_raw.split_once(':') {
        (t.to_string(), iso.to_string())
    } else {
        (tier_raw, String::new())
    };

    let pid = v["pid"].as_u64().unwrap_or(0) as u32;

    let sandbox = agent_sandbox_from_json(v);

    // Distinguishes "key absent" (older agentd, genuinely Clean) from "key present but
    // unparseable" (a real failure, must render EvaluationUnavailable — never silently
    // collapse both to Clean; same fix as reader::read_agent_attention's FUSE path).
    let attention: Vec<AttentionSignal> = match v.get("attention") {
        None => vec![],
        Some(val) => match serde_json::from_value::<Vec<AttentionSignal>>(val.clone()) {
            Ok(signals) => signals,
            Err(_) => vec![AttentionSignal {
                reason:   reader::AttentionReason::EvaluationUnavailable,
                since:    std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                evidence: Some("attention_field".to_string()),
            }],
        },
    };

    AgentInfo {
        id,
        status,
        status_detail,
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

fn agent_sandbox_from_json(v: &Value) -> Option<AgentSandbox> {
    let names = v["accessible_server_names"].as_array()?;
    if names.is_empty() {
        return None;
    }
    let servers = names
        .iter()
        .filter_map(|n| n.as_str())
        .map(|name| ServerEnforcement {
            name: name.to_string(),
            ..Default::default()
        })
        .collect();
    Some(AgentSandbox { servers })
}

fn isolation_from_json(v: &Value) -> Option<SysIsolation> {
    if v.is_null() {
        return None;
    }
    Some(SysIsolation {
        tier:     v["tier"].as_str().unwrap_or("none").to_string(),
        arch:     v["arch"].as_str().unwrap_or("").to_string(),
        runsc:    v["runsc"].as_str().map(str::to_string),
        landlock: v["landlock"].as_bool().unwrap_or(false),
        seccomp:  v["seccomp"].as_bool().unwrap_or(false),
    })
}

fn sandbox_from_json(v: &Value) -> Option<SysSandbox> {
    if v.is_null() {
        return None;
    }
    let any_sandboxed = v["any_sandboxed"].as_bool().unwrap_or(false);
    let servers: Vec<ServerEnforcement> = v["servers"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(server_enforcement_from_json)
        .collect();
    let degradations: Vec<String> = v["degradations"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    Some(SysSandbox { any_sandboxed, servers, degradations })
}

fn server_enforcement_from_json(v: &Value) -> ServerEnforcement {
    ServerEnforcement {
        name:              v["name"].as_str().unwrap_or("").to_string(),
        transport:         v["transport"].as_str().unwrap_or("").to_string(),
        isolation:         v["isolation"].as_str().unwrap_or("").to_string(),
        landlock:          v["landlock"].as_bool().unwrap_or(false),
        seccomp:           v["seccomp"].as_bool().unwrap_or(false),
        spawn_enforcement: v["spawn_enforcement"].as_str().unwrap_or("").to_string(),
        namespace_net:     v["namespace_net"].as_bool().unwrap_or(false),
        namespace_mount:   v["namespace_mount"].as_bool().unwrap_or(false),
        landlock_net:      v["landlock_net"].as_bool().unwrap_or(false),
    }
}

fn credentials_from_json(v: &Value) -> Option<SysCredentials> {
    if v.is_null() || (!v["gateway_enabled"].as_bool().unwrap_or(false) && v["enabled"] == Value::Bool(false)) {
        return None;
    }
    let configured_providers: Vec<String> = v["configured_providers"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect();
    let provider_health: Vec<ProvHealthInfo> = v["provider_health"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|p| ProvHealthInfo {
            name:            p["name"].as_str().unwrap_or("").to_string(),
            token_fresh:     p["token_fresh"].as_bool().unwrap_or(false),
            last_refresh_at: p["last_refresh_at"].as_u64(),
            expires_at:      p["expires_at"].as_u64(),
            last_error:      p["last_error"].as_str().map(str::to_string),
        })
        .collect();
    Some(SysCredentials {
        gateway_enabled: v["gateway_enabled"].as_bool().unwrap_or(false),
        configured_providers,
        provider_health,
    })
}

// ── FUSE control channel write ──────────────────────────────────────────────

/// Write `payload` bytes to `/agents/control` using an explicit `close(2)` so FUSE
/// flush errors are visible rather than silently swallowed by Rust's `File::drop`.
///
/// Moved here from `mod.rs` (dx.2) so `FuseSource` can implement `approve`/`deny`.
#[cfg(not(unix))]
pub(crate) fn write_control_command(
    _agents_dir: &std::path::Path,
    _payload: &str,
) -> Result<(), String> {
    Err("write_control_command not supported on this platform".to_string())
}

#[cfg(unix)]
pub(crate) fn write_control_command(
    agents_dir: &std::path::Path,
    payload: &str,
) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::io::IntoRawFd as _;

    let control = agents_dir.join("control");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&control)
        .map_err(|e| format!("open /agents/control: {e}"))?;
    f.write_all(payload.as_bytes())
        .map_err(|e| format!("write error: {e}"))?;
    let fd = f.into_raw_fd();
    // SAFETY: fd is valid and exclusively owned (into_raw_fd consumed the File).
    let rc = unsafe { libc::close(fd) };
    if rc != 0 {
        return Err(format!(
            "scheduler rejected command ({})",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Auto-detect the correct data source.
///
/// Priority:
/// 1. `url` argument (from `--url` flag or `AGENTCTL_URL` env)
/// 2. FUSE filesystem at `agents_dir` (default `/agents`)
/// 3. Management API health-check at `http://127.0.0.1:7999`
pub fn detect_source(
    url: Option<&str>,
    agents_dir: &std::path::Path,
) -> anyhow::Result<Box<dyn DataSource>> {
    // Explicit URL wins.
    if let Some(u) = url {
        return Ok(Box::new(HttpSource::new(u.to_string())));
    }

    // FUSE present?
    if agents_dir.join("system").exists() {
        return Ok(Box::new(FuseSource { agents_dir: agents_dir.to_path_buf() }));
    }

    // Try management API on the default port.
    let default_url = "http://127.0.0.1:7999";
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    if client.get(format!("{default_url}/healthz")).send()
        .is_ok_and(|r| r.status().is_success())
    {
        return Ok(Box::new(HttpSource::new(default_url.to_string())));
    }

    anyhow::bail!(
        "No agentd data source available.\n\
         - FUSE mountpoint {:?} has no 'system/' directory (is agentd running?)\n\
         - Management API at {default_url} is unreachable\n\
         Try: start agentd, or pass --url http://HOST:7999 to connect remotely.",
        agents_dir
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_info_from_json_running() {
        let v = serde_json::json!({
            "id": "agent-1",
            "status": "running",
            "context_tokens": 512,
            "token_budget": 50000_u64,
            "tools": ["read_file"],
            "parent_id": null,
            "tier": null,
            "pid": null,
            "accessible_server_names": [],
            "capabilities_unrestricted": true,
        });
        let info = agent_info_from_json(&v);
        assert_eq!(info.id, "agent-1");
        assert_eq!(info.status, "running");
        assert_eq!(info.context_tokens, 512);
        assert!(matches!(info.budget, BudgetKind::Tokens(50000)));
        assert_eq!(info.tools, vec!["read_file"]);
        assert_eq!(info.tier, "native");
        assert!(info.status_detail.is_none());
        assert!(info.attention.is_empty(), "no 'attention' key at all must default to Clean, not fail");
    }

    #[test]
    fn agent_info_from_json_parses_attention_signals() {
        let v = serde_json::json!({
            "id": "agent-1", "status": "running", "token_budget": 50000_u64,
            "attention": [{"reason": "approval_pending", "since": 10, "evidence": "act_1"}],
        });
        let info = agent_info_from_json(&v);
        assert_eq!(info.attention.len(), 1);
        assert_eq!(info.attention[0].reason, reader::AttentionReason::ApprovalPending);
        assert_eq!(info.attention[0].evidence.as_deref(), Some("act_1"));
    }

    #[test]
    fn agent_info_from_json_malformed_attention_becomes_evaluation_unavailable() {
        // A present-but-unparseable "attention" value is a real failure, not "nothing wrong" —
        // must render EvaluationUnavailable, never silently collapse to Clean (matches the
        // FUSE-path fix in reader::read_agent_attention).
        let v = serde_json::json!({
            "id": "agent-1", "status": "running", "token_budget": 50000_u64,
            "attention": "not an array",
        });
        let info = agent_info_from_json(&v);
        assert_eq!(info.attention.len(), 1);
        assert_eq!(info.attention[0].reason, reader::AttentionReason::EvaluationUnavailable);
    }

    #[test]
    fn agent_info_unlimited_budget() {
        let v = serde_json::json!({
            "id": "a",
            "status": "running",
            "token_budget": u64::MAX,
        });
        let info = agent_info_from_json(&v);
        assert!(matches!(info.budget, BudgetKind::Unlimited));
    }

    #[test]
    fn agent_info_awaiting_child_with_status_detail() {
        let v = serde_json::json!({
            "id": "a",
            "status": "awaiting_child",
            "status_detail": "child-1",
            "token_budget": 0,
        });
        let info = agent_info_from_json(&v);
        assert_eq!(info.status, "awaiting_child");
        assert_eq!(info.status_detail.as_deref(), Some("child-1"));
    }

    #[test]
    fn server_enforcement_from_json_fields() {
        let v = serde_json::json!({
            "name": "shell",
            "transport": "stdio",
            "isolation": "none",
            "landlock": true,
            "seccomp": false,
            "spawn_enforcement": "fork_vfork_only",
            "namespace_net": false,
            "namespace_mount": false,
            "landlock_net": false,
        });
        let se = server_enforcement_from_json(&v);
        assert_eq!(se.name, "shell");
        assert!(se.landlock);
    }

    #[test]
    fn pending_action_from_json_basic() {
        let v = serde_json::json!({
            "id": "act_0",
            "agent_id": "agent-1",
            "kind": "write_file",
            "risk": "medium",
            "summary": "Write config file",
            "age_secs": 5,
        });
        let a = pending_action_from_json(&v);
        assert_eq!(a.id, "act_0");
        assert_eq!(a.agent_id, "agent-1");
        assert_eq!(a.risk, "medium");
        assert_eq!(a.age_secs, 5);
    }

    #[test]
    fn http_source_approve_fails_without_server() {
        let src = HttpSource::new("http://127.0.0.1:19999".to_string());
        let result = src.approve("act_0");
        assert!(result.is_err(), "approve should fail when server is unreachable");
        let result = src.deny("act_0", Some("reason"));
        assert!(result.is_err(), "deny should fail when server is unreachable");
    }

    #[test]
    fn isolation_from_json_full_tier() {
        let v = serde_json::json!({
            "tier": "full",
            "arch": "x86_64",
            "runsc": "/usr/bin/runsc",
            "landlock": true,
            "seccomp": true,
        });
        let iso = isolation_from_json(&v).expect("must parse full tier");
        assert_eq!(iso.tier, "full");
        assert_eq!(iso.arch, "x86_64");
        assert_eq!(iso.runsc.as_deref(), Some("/usr/bin/runsc"));
        assert!(iso.landlock);
        assert!(iso.seccomp);
    }

    #[test]
    fn isolation_from_json_null_returns_none() {
        let v = serde_json::Value::Null;
        assert!(isolation_from_json(&v).is_none(), "null → None");
    }

    #[test]
    fn isolation_from_json_none_tier_and_null_runsc() {
        let v = serde_json::json!({
            "tier": "none",
            "arch": "aarch64",
            "runsc": null,
            "landlock": false,
            "seccomp": false,
        });
        let iso = isolation_from_json(&v).unwrap();
        assert_eq!(iso.tier, "none");
        assert!(iso.runsc.is_none());
        assert!(!iso.landlock);
    }

    #[test]
    fn isolation_from_json_empty_object_uses_defaults() {
        // An empty JSON object (non-null) must return Some with all unwrap_or defaults.
        let v = serde_json::json!({});
        let iso = isolation_from_json(&v).expect("empty object should return Some");
        assert_eq!(iso.tier, "none");
        assert_eq!(iso.arch, "");
        assert!(!iso.landlock);
        assert!(!iso.seccomp);
        assert!(iso.runsc.is_none());
    }

    #[test]
    fn detect_source_fuse_path_returns_fuse_source() {
        let tmp = tempfile::tempdir().unwrap();
        // Create system/ subdir to simulate a mounted FUSE filesystem.
        std::fs::create_dir(tmp.path().join("system")).unwrap();
        let src = detect_source(None, tmp.path()).unwrap();
        // FuseSource::load_snapshot should at least not panic on an empty dir.
        let _ = src.load_approvals();
    }

    #[test]
    fn detect_source_fallback_to_http_when_no_fuse() {
        let tmp = tempfile::tempdir().unwrap();
        // No system/ dir, no real HTTP server → should bail.
        let result = detect_source(None, tmp.path());
        assert!(result.is_err(), "should fail when neither FUSE nor HTTP is reachable");
    }

    // ── credentials_from_json ─────────────────────────────────────────────────

    #[test]
    fn credentials_from_json_null_returns_none() {
        let v = serde_json::Value::Null;
        assert!(credentials_from_json(&v).is_none(), "null → None");
    }

    #[test]
    fn credentials_from_json_enabled_false_returns_none() {
        let v = serde_json::json!({"enabled": false});
        assert!(credentials_from_json(&v).is_none(), "enabled=false → None");
    }

    #[test]
    fn credentials_from_json_gateway_enabled_parses_providers() {
        let v = serde_json::json!({
            "gateway_enabled": true,
            "configured_providers": ["google", "brave"],
            "provider_health": [
                {
                    "name": "google",
                    "token_fresh": true,
                    "last_refresh_at": 1720000000u64,
                    "expires_at": 1720003600u64,
                    "last_error": null,
                },
                {
                    "name": "brave",
                    "token_fresh": false,
                    "last_refresh_at": null,
                    "expires_at": null,
                    "last_error": "key_missing",
                }
            ]
        });
        let creds = credentials_from_json(&v).expect("should parse");
        assert!(creds.gateway_enabled);
        assert_eq!(creds.configured_providers, vec!["google", "brave"]);
        assert_eq!(creds.provider_health.len(), 2);
        let g = &creds.provider_health[0];
        assert_eq!(g.name, "google");
        assert!(g.token_fresh);
        assert_eq!(g.expires_at, Some(1720003600));
        let b = &creds.provider_health[1];
        assert!(!b.token_fresh);
        assert_eq!(b.last_error.as_deref(), Some("key_missing"));
    }

    #[test]
    fn credentials_from_json_empty_providers_returns_some() {
        let v = serde_json::json!({
            "gateway_enabled": true,
            "configured_providers": [],
            "provider_health": []
        });
        let creds = credentials_from_json(&v).expect("empty-but-enabled → Some");
        assert!(creds.gateway_enabled);
        assert!(creds.configured_providers.is_empty());
    }
}
