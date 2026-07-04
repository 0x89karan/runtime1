//! DataSource abstraction: FUSE filesystem vs. management HTTP API (p7.7).
//!
//! The trait has one method: `load_snapshot()` returns the same `Snapshot` type
//! that the FUSE reader always produced, so callers don't change.

use serde_json::Value;

use super::reader::{
    self, AgentInfo, AgentSandbox, BudgetKind, ServerEnforcement,
    Snapshot, SysBudget, SysProvider, SysQueue, SysSandbox,
};

/// Abstraction over FUSE filesystem and management HTTP API.
pub trait DataSource: Send + Sync {
    /// Load the current scheduler snapshot, including all agent info and system stats.
    fn load_snapshot(&self) -> Snapshot;
}

// ── FuseSource ─────────────────────────────────────────────────────────────

/// DataSource backed by the FUSE `/agents` filesystem.
pub struct FuseSource {
    pub agents_dir: std::path::PathBuf,
}

impl DataSource for FuseSource {
    fn load_snapshot(&self) -> Snapshot {
        reader::load_snapshot(&self.agents_dir)
    }
}

// ── HttpSource ──────────────────────────────────────────────────────────────

/// DataSource backed by the management HTTP API (p7.7).
pub struct HttpSource {
    pub base_url: String,
    client: reqwest::blocking::Client,
}

impl HttpSource {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self { base_url, client }
    }

    fn get_json(&self, path: &str) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let resp = self.client.get(&url).send()?;
        Ok(resp.json()?)
    }
}

impl DataSource for HttpSource {
    fn load_snapshot(&self) -> Snapshot {
        let val = match self.get_json("/api/v1/snapshot") {
            Ok(v)  => v,
            Err(e) => return Snapshot {
                agents:   vec![],
                budget:   None,
                queue:    None,
                sandbox:  None,
                provider: None,
                error:    Some(format!("HTTP error: {e:#}")),
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

        Snapshot { agents, budget, queue, sandbox, provider, error: None }
    }
}

// ── JSON → reader type conversions ─────────────────────────────────────────

fn agent_info_from_json(v: &Value) -> AgentInfo {
    let id = v["id"].as_str().unwrap_or("").to_string();
    let status = v["status"].as_str().unwrap_or("unknown").to_string();
    let context_tokens = v["context_tokens"].as_u64().unwrap_or(0);

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

    AgentInfo {
        id,
        status,
        context_tokens,
        budget,
        tools,
        parent_id,
        sandbox,
        egress_brokered: 0,
        egress_denied:   0,
        tier,
        isolation,
        pid,
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
    fn agent_info_awaiting_child() {
        let v = serde_json::json!({
            "id": "a",
            "status": "awaiting_child",
            "status_detail": "child-1",
            "token_budget": 0,
        });
        let info = agent_info_from_json(&v);
        assert_eq!(info.status, "awaiting_child");
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

}
