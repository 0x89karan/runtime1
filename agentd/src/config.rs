use serde::{Deserialize, Serialize};

use crate::capability::Capability;

/// A discoverable identity card for an agent.
/// Built from `AgentConfig` at scheduler seed time; held by `ListAgentsTool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub id:          String,
    pub name:        String,
    pub description: String,
    pub skills:      Vec<String>,
}

impl From<&AgentConfig> for AgentCard {
    fn from(cfg: &AgentConfig) -> Self {
        Self {
            id:          cfg.id.clone(),
            name:        cfg.name.clone().unwrap_or_else(|| cfg.id.clone()),
            description: cfg.description.clone(),
            skills:      cfg.skills.clone(),
        }
    }
}

// deny_unknown_fields is intentionally omitted here to allow both [agent] and [[agents]]
// forms to coexist in the schema without serde rejecting the other key.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub agent: Option<AgentConfig>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Path for the flight log (JSONL). Defaults to "flight.jsonl" in the CWD.
    /// The --log-path CLI flag takes precedence over this field.
    pub log_path: Option<String>,
    /// Egress mediator configuration (p7.5+).
    #[serde(default)]
    pub egress: EgressConfig,
    /// Management HTTP API configuration (p7.7+).
    #[serde(default)]
    pub management: ManagementConfig,
    /// Credential broker configuration (cred.3+).
    #[serde(default)]
    pub credential_gateway: CredentialGatewayConfig,
}

/// Mutability class for a declared knowledge-base segment (p5.4+).
/// Re-exported from `memory::MutabilityClass` so config and runtime share one type.
pub use crate::memory::MutabilityClass as SegmentClass;

/// A single operator-seeded entry written to a segment at startup (p5.9/F-14).
///
/// Used to populate `canon` trust anchors that agents cannot write themselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedEntry {
    pub key: String,
    pub value: String,
}

/// A declared knowledge-base segment (p5.4+).
///
/// ```toml
/// [[memory.segments]]
/// name  = "project:notes"    # namespace agents reference in kb_put/kb_get
/// class = "log"              # "canon" | "log" | "scratch"
/// seed  = [                  # p5.9: operator-seeded entries (e.g. canon anchors)
///   { key = "guidelines", value = "Cite evidence." },
/// ]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentConfig {
    pub name: String,
    pub class: SegmentClass,
    /// Operator-seeded entries written to the segment at startup (F-14). The
    /// operator write bypasses agent-facing canon protection by design.
    #[serde(default)]
    pub seed: Vec<SeedEntry>,
}

/// Configuration for the durable key/value memory store (p5.1+).
///
/// ```toml
/// [memory]
/// store_path             = "memory.redb"   # relative to CWD or absolute
/// enabled                = true
/// max_entries_per_segment = 500            # p5.6: evict oldest beyond this
/// max_entry_age_days      = 90             # p5.6: evict entries older than N days
/// distill_on_complete     = false          # p5.6: summarise short-term → Tier 3
///
/// [[memory.segments]]          # p5.4: declare shared KB segments
/// name  = "project:notes"
/// class = "log"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Path to the redb database file.
    /// Relative paths are resolved relative to the CWD at startup.
    /// Absolute paths are used as-is.
    /// Container/production deployments should set an absolute path on a persistent
    /// mount, e.g. `store_path = "/run/memory/memory.redb"` (p5.3.5 detachable volume).
    #[serde(default = "default_memory_store_path")]
    pub store_path: String,
    /// Set to false to disable the memory store entirely.
    /// When false, `kv_get` and `kv_set` tools are not registered.
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,
    /// Declared shared KB segments (p5.4+). Each entry fixes the segment's
    /// mutability class in the store at startup.
    #[serde(default)]
    pub segments: Vec<SegmentConfig>,
    /// Per-segment capacity limit (p5.6). Oldest entries are evicted first once the
    /// count exceeds this value. `None` (the default) means no capacity-based eviction.
    #[serde(default)]
    pub max_entries_per_segment: Option<usize>,
    /// Per-segment age limit in days (p5.6). Entries older than this are evicted.
    /// `None` (the default) means no age-based eviction.
    #[serde(default)]
    pub max_entry_age_days: Option<u64>,
    /// When true, each completed agent's short-term memory buffer is distilled into
    /// a single Tier-3 inference summary at the end of the run (p5.6).
    /// Default false — existing demos unchanged.
    #[serde(default)]
    pub distill_on_complete: bool,
}

fn default_memory_store_path() -> String {
    "memory.redb".to_string()
}

fn default_memory_enabled() -> bool {
    true
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            store_path:               default_memory_store_path(),
            enabled:                  default_memory_enabled(),
            segments:                 Vec::new(),
            max_entries_per_segment:  None,
            max_entry_age_days:       None,
            distill_on_complete:      false,
        }
    }
}

/// Configuration for the egress mediator and tamper-evident audit log (p7.5+).
///
/// ```toml
/// [egress]
/// evidence_path = "evidence.jsonl"   # default
/// key_path      = "egress-key.pkcs8" # default; created on first run
/// proxy_addr    = "127.0.0.1:8765"   # optional: bind HTTP stub (p7.5b readiness)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    /// Path to the tamper-evident action receipt log.
    #[serde(default = "default_evidence_path")]
    pub evidence_path: String,
    /// Path to the Ed25519 private key (PKCS8 DER). Created automatically on first run.
    #[serde(default = "default_egress_key_path")]
    pub key_path: String,
    /// If set, bind an HTTP stub server on this address (always returns 501).
    /// Fail-closed: agentd exits non-zero if bind fails.
    #[serde(default)]
    pub proxy_addr: Option<String>,
}

fn default_evidence_path() -> String {
    "evidence.jsonl".to_string()
}
fn default_egress_key_path() -> String {
    "egress-key.pkcs8".to_string()
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self {
            evidence_path: default_evidence_path(),
            key_path:      default_egress_key_path(),
            proxy_addr:    None,
        }
    }
}

/// Management HTTP API configuration (p7.7+).
///
/// Exposes scheduler state as JSON + SSE so agentctl can run on the Mac/Linux
/// host without FUSE access. Off by default; enable via `[management] enabled = true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagementConfig {
    /// Enable the management API. Defaults to false.
    #[serde(default)]
    pub enabled: bool,
    /// TCP port to bind (loopback only). Defaults to 7999.
    #[serde(default = "default_management_port")]
    pub port: u16,
    /// Bind address. Must resolve to loopback; agentd refuses to start otherwise.
    #[serde(default = "default_management_bind_addr")]
    pub bind_addr: String,
}

fn default_management_port() -> u16 {
    7999
}
fn default_management_bind_addr() -> String {
    "127.0.0.1".to_string()
}

impl Default for ManagementConfig {
    fn default() -> Self {
        Self {
            enabled:   false,
            port:      default_management_port(),
            bind_addr: default_management_bind_addr(),
        }
    }
}

/// Authentication style for a credential provider adapter (cred.3+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    /// RFC 6750 Bearer token: `Authorization: Bearer <token>`
    OauthBearer,
    /// API key in a named request header.
    ApiKeyHeader,
    /// API key as a URL query parameter.
    ApiKeyQuery,
}

/// TOML configuration for one credential provider adapter (cred.3+).
///
/// ```toml
/// [credential_gateway.providers.google]
/// auth_style    = "oauth-bearer"
/// upstream_base = "https://www.googleapis.com"
/// token_path    = "/run/secrets/google.json"
/// state_path    = "/data/state/oauth/google.json"
///
/// [credential_gateway.providers.brave-search]
/// auth_style    = "api-key-header"
/// upstream_base = "https://api.search.brave.com"
/// header_name   = "X-Subscription-Token"
/// secret_key    = "BRAVE_SEARCH_API_KEY"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub auth_style:    AuthStyle,
    pub upstream_base: String,
    /// Named header for `api-key-header` style (also scrubbed from caller requests).
    #[serde(default)]
    pub header_name:   Option<String>,
    /// Optional prefix prepended as `"{prefix} {credential}"` for `api-key-header`
    /// style. Required for APIs that expect `Authorization: Bearer <token>` instead
    /// of a raw token value. Must not contain `\r` or `\n` (validated at startup).
    #[serde(default)]
    pub header_value_prefix: Option<String>,
    /// Env var that holds the API key for `api-key-header` / `api-key-query` styles.
    #[serde(default)]
    pub secret_key:    Option<String>,
    /// Path to the JSON secrets file (OAuth flows); e.g. `/run/secrets/google.json`.
    #[serde(default)]
    pub token_path:    Option<String>,
    /// Path for writing runtime token state (OAuth access + refresh rotation).
    #[serde(default)]
    pub state_path:    Option<String>,
    /// Per-agent per-provider request-count cap enforced at the broker layer.
    /// `None` (default) = unlimited. `0` = block all requests to this provider.
    #[serde(default)]
    pub max_requests_per_agent: Option<u64>,
}

/// Configuration for the in-process credential broker (cred.3+).
///
/// ```toml
/// [credential_gateway]
/// enabled = true
///
/// [credential_gateway.providers.google]
/// auth_style    = "oauth-bearer"
/// upstream_base = "https://www.googleapis.com"
/// token_path    = "/run/secrets/google.json"
/// state_path    = "/data/state/oauth/google.json"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialGatewayConfig {
    /// Enable the credential broker. Defaults to false (off by default, opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Provider adapters keyed by provider name (lowercase, kebab-case).
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    /// Path for the per-agent request-cap persistence database (`caps.redb`).
    /// Defaults to `None` = in-memory only (resets on agentd restart).
    /// Set by `main.rs` to `<memory_store_dir>/caps.redb` when the credential
    /// gateway is enabled, so caps survive restarts without operator config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps_db_path: Option<String>,
}

impl Config {
    /// Returns 1+ AgentConfigs from either the `[agent]` or `[[agents]]` TOML form.
    /// Errors if both are set (ambiguous), neither is set (nothing to run), or any
    /// agent id is duplicated.
    pub fn agent_configs(&self) -> anyhow::Result<Vec<AgentConfig>> {
        match (&self.agent, self.agents.is_empty()) {
            (Some(_), false) => anyhow::bail!(
                "cannot set both [agent] and [[agents]] in the same config; use one form"
            ),
            (None, true) => anyhow::bail!(
                "no agents configured; set [agent] for a single agent or [[agents]] for multiple"
            ),
            (Some(a), true) => Ok(vec![a.clone()]),
            (None, false) => Ok(self.agents.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Global token ceiling across all agents. 0 = unlimited.
    #[serde(default)]
    pub global_token_budget: u64,
    /// Maximum number of in-flight inference calls at once. 0 = unlimited.
    #[serde(default)]
    pub max_concurrent_inferences: usize,
    /// Maximum spawn nesting depth. 0 = spawning disabled. Default 4.
    #[serde(default = "default_max_spawn_depth")]
    pub max_spawn_depth: u32,
    /// Checkpoint the full scheduler state every N completed turns (after tool results).
    /// 0 = SIGTERM/SIGINT only. Default 1 (every turn).
    #[serde(default = "default_checkpoint_interval_turns")]
    pub checkpoint_interval_turns: u32,
}

fn default_max_spawn_depth() -> u32 {
    4
}

fn default_checkpoint_interval_turns() -> u32 {
    1
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            global_token_budget:       0,
            max_concurrent_inferences: 0,
            max_spawn_depth:           default_max_spawn_depth(),
            checkpoint_interval_turns: default_checkpoint_interval_turns(),
        }
    }
}

/// Input to the `spawn_agent` tool — describes the child agent to create.
/// Capabilities and max_turns inherit from parent when absent.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpawnConfig {
    /// If absent, auto-generated as `"{parent_id}-child-{seq}"`.
    pub child_id: Option<String>,
    /// The child's initial task (first user message).
    pub task: String,
    /// Scheduling priority. Default 0.
    #[serde(default)]
    pub priority: u32,
    /// Per-agent token ceiling. Inherits parent's remaining budget if absent.
    pub token_budget: Option<u64>,
}

/// The action an agent is requesting approval for. Passed by the agent as the
/// input to the `request_approval` tool; stored in `ParkedApproval` until resolved.
///
/// `kind` and `risk` are free-form strings in v1 (the harness defines taxonomy).
/// `linked` is intentionally absent — cross-action coordination is harness territory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingActionRequest {
    /// Short noun phrase: the action type, e.g. "write_file", "send_email".
    pub kind:       String,
    /// Operator-visible severity: "low" | "medium" | "high" (free-form in v1).
    pub risk:       String,
    /// One-sentence human summary of what the agent intends to do.
    pub summary:    String,
    /// Full argument set the agent will pass to the underlying tool once approved.
    /// The operator may override these via the `edits` field on Approve.
    pub args:       serde_json::Value,
    /// Optional snapshot of state before the action (diff context for the operator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_state: Option<serde_json::Value>,
    /// Optional snapshot of state after the action would complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_state:  Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_token_budget")]
    pub token_budget: u64,
    /// Scheduling priority. Higher value runs before lower. Default 0 (equal priority).
    #[serde(default)]
    pub priority: u32,
    /// Tool capabilities granted to this agent.
    /// `None` (field absent) = unrestricted access to all registered tools.
    /// `Some([])` = deny all tool use.
    /// `Some([...])` = allow only the listed capabilities.
    #[serde(default)]
    pub capabilities: Option<Vec<Capability>>,
    /// Display name for this agent. Defaults to `id` if absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Human-readable description of this agent's purpose.
    #[serde(default)]
    pub description: String,
    /// Free-form skill tags this agent advertises.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Execution tier. Default: native (in-process). Set to "universal" to run as
    /// an external child process. Requires `command` to be set.
    #[serde(default)]
    pub tier: AgentTier,
    /// Executable path for universal-tier agents. Ignored for native-tier agents.
    #[serde(default)]
    pub command: Option<String>,
    /// Additional arguments passed to the command. Static only (no template substitution).
    #[serde(default)]
    pub args: Vec<String>,
    /// Isolation mode for universal-tier agents. "none" (default) or "gvisor".
    /// Requires `runsc` on PATH when set to "gvisor".
    #[serde(default)]
    pub isolation: IsolationMode,
    /// Maximum wall-clock seconds before the universal agent is killed. 0 = no limit.
    #[serde(default)]
    pub max_wall_seconds: u64,
}

pub fn default_max_turns() -> u32 {
    20
}

pub fn default_token_budget() -> u64 {
    100_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// When true, use SSE streaming and print text chunks progressively to stdout.
    /// Applies to all agents in this config file. Default: true.
    #[serde(default = "default_streaming")]
    pub streaming: bool,
}

fn default_streaming() -> bool {
    true
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            streaming: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default)]
    pub native: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// When `true`, startup fails if any MCP server omits the `capabilities` field.
    /// Ensures all servers are sandboxed; defaults to `false` for backward compat.
    #[serde(default)]
    pub mcp_require_capabilities: bool,
}

/// Execution tier for an agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentTier {
    /// In-process Rust agent (default). Uses the full inference loop.
    #[default]
    Native,
    /// External process wrapped in optional gVisor isolation (p7.6).
    /// Requires `command`, routes LLM traffic through the egress proxy.
    Universal,
}

/// Isolation mode for an MCP server subprocess or a universal-tier agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum IsolationMode {
    /// Default: Landlock + seccomp + Linux namespaces applied via pre_exec.
    #[default]
    None,
    /// Stronger isolation: wrap the server command with `runsc do` (gVisor).
    /// Requires `runsc` on PATH; fails fast at startup if not found.
    /// Landlock/seccomp/namespaces are NOT applied (gVisor's Sentry handles isolation).
    Gvisor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    /// Subprocess command for stdio transport. Required when `url` is absent.
    /// Must be empty when `url` is set (the two are mutually exclusive).
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP endpoint for Streamable HTTP transport (MCP spec 2025-03-26).
    /// When set, the server is contacted over HTTP/SSE instead of spawning a subprocess.
    /// Must start with `https://`. Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Maps HTTP header names to environment variable names whose values are the header values.
    /// Example: `{ Authorization = "LINEAR_MCP_TOKEN" }` reads `$LINEAR_MCP_TOKEN` at startup
    /// and sends it as the `Authorization` header on every request.
    /// The env var value (e.g. `"Bearer sk-lin-..."`) is the full header value, sent as-is.
    /// Note: OAuth-based servers require a future `auth_provider` field; only static tokens here.
    #[serde(default)]
    pub headers_env: std::collections::HashMap<String, String>,
    /// Capability-based sandbox rules applied to this MCP server subprocess (stdio only).
    /// `None` (field absent) = no sandbox — server runs unrestricted.
    /// `Some([])` = deny-all spawn + network isolation; no filesystem grants.
    /// `Some([...])` = exact capability set converted to Landlock + seccomp + namespace rules.
    /// Ignored for HTTP servers (externally isolated).
    #[serde(default)]
    pub capabilities: Option<Vec<Capability>>,
    /// Stronger isolation mode for stdio servers. `"none"` (default): pre_exec sandbox.
    /// `"gvisor"`: wrap command with `runsc do`; requires `runsc` on PATH.
    /// Ignored for HTTP servers.
    #[serde(default)]
    pub isolation: IsolationMode,
    /// Extra environment variables to pass to this MCP server subprocess (stdio only).
    /// Applied on top of the standard allowlist (PATH, HOME, USER, LANG, LC_ALL, TMPDIR).
    /// The parent process's full environment, including secrets, is NOT inherited.
    /// Ignored for HTTP servers (use `headers_env` for HTTP auth headers).
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Names of environment variables from the parent process to forward to this MCP server.
    /// Use this for API keys the subprocess needs (e.g. `BRAVE_SEARCH_API_KEY`).
    /// Each name is looked up in the parent env at startup; missing vars are silently skipped.
    /// Ignored for HTTP servers (use `headers_env` instead).
    #[serde(default)]
    pub passenv: Vec<String>,
}

impl McpServerConfig {
    /// Returns true if this server uses HTTP transport.
    pub fn is_http(&self) -> bool {
        self.url.is_some()
    }

    /// Validate transport config: exactly one of url/command must be set; url must be https.
    pub fn validate(&self) -> anyhow::Result<()> {
        match (&self.url, self.command.is_empty()) {
            (Some(_), false) => anyhow::bail!(
                "MCP server '{}': cannot set both 'url' and 'command' — \
                 use 'url' for HTTP transport or 'command' for stdio transport",
                self.name
            ),
            (None, true) => anyhow::bail!(
                "MCP server '{}': transport is 'stdio' but no command is set — \
                 add command = \"/path/to/server\" for stdio or url = \"https://...\" for HTTP",
                self.name
            ),
            (Some(url), true) => {
                if !url.starts_with("https://") {
                    anyhow::bail!(
                        "MCP server '{}': url must start with 'https://' (got {:?}) — \
                         plaintext HTTP is not allowed (tokens would be sent in clear text)",
                        self.name, url
                    );
                }
                // Reject credentials embedded in the URL (e.g. https://user:token@host/mcp)
                // which would persist secrets to disk, violating the secrets-from-env invariant.
                // Use headers_env to inject auth headers from environment variables instead.
                let after_scheme = &url["https://".len()..];
                let slash = after_scheme.find('/').unwrap_or(after_scheme.len());
                if after_scheme[..slash].contains('@') {
                    anyhow::bail!(
                        "MCP server '{}': embedding credentials in the URL is not allowed — \
                         use 'headers_env' to inject auth headers from environment variables",
                        self.name
                    );
                }
                Ok(())
            }
            (None, false) => Ok(()), // stdio, command present
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_config_round_trips() {
        let raw = r#"
[agent]
id = "scout"
task = "list the project dir and summarize Cargo.toml"
max_turns = 10
token_budget = 50000

[model]
provider = "anthropic"
model = "claude-sonnet-4-6"
max_tokens = 2048

[tools]
native = ["read_file", "list_dir"]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.agent.as_ref().unwrap().id, "scout");
        assert_eq!(cfg.agent.as_ref().unwrap().max_turns, 10);
        assert_eq!(cfg.agent.as_ref().unwrap().token_budget, 50_000);
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.model, "claude-sonnet-4-6");
        assert_eq!(cfg.model.max_tokens, 2048);
        assert_eq!(cfg.tools.native, vec!["read_file", "list_dir"]);
    }

    // F-14/F-15: the shipped multi-agent memory demo MUST parse and be runnable.
    // It is the smoke test for the whole memory subsystem; CI runs `cargo test`,
    // so this guards against a non-parsing demo shipping again.
    #[test]
    fn shipped_demo_agents_toml_parses_and_is_runnable() {
        let raw = include_str!("../agents.toml");
        let cfg: Config = toml::from_str(raw)
            .expect("shipped agents.toml must parse (F-14: e.g. unknown `seed` field)");

        // F-15: the writer needs spawn_agent registered, not just the Spawn cap.
        assert!(
            cfg.tools.native.iter().any(|t| t == "spawn_agent"),
            "demo grants Spawn but must also list spawn_agent in [tools].native (F-15)"
        );

        // F-14: the canon segment must carry its operator seed.
        let canon = cfg
            .memory
            .segments
            .iter()
            .find(|s| matches!(s.class, SegmentClass::Canon))
            .expect("demo must declare a canon segment");
        assert!(
            !canon.seed.is_empty(),
            "canon trust-anchor segment must be operator-seeded (F-14)"
        );

        // The writer agent must hold Spawn so the spawn flow actually runs.
        let writer = cfg
            .agents
            .iter()
            .find(|a| a.id == "writer")
            .expect("demo must define the writer agent");
        assert!(writer.capabilities.is_some(), "writer must declare capabilities");
    }

    #[test]
    fn model_defaults_apply_when_section_absent() {
        let raw = r#"
[agent]
id = "minimal"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.model, "claude-sonnet-4-6");
        assert_eq!(cfg.model.max_tokens, 4096);
        assert_eq!(cfg.agent.as_ref().unwrap().max_turns, 20);
        assert_eq!(cfg.agent.as_ref().unwrap().token_budget, 100_000);
    }

    #[test]
    fn missing_agent_id_is_error() {
        let raw = r#"
[agent]
task = "no id here"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "expected Err when agent.id is missing");
    }

    #[test]
    fn agent_config_rejects_unknown_fields() {
        // deny_unknown_fields still applies on AgentConfig itself
        let raw = r#"
[agent]
id = "scout"
unknown_field = "bad"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "expected Err for unknown field in [agent]");
    }

    #[test]
    fn top_level_unknown_field_is_now_allowed() {
        // Config no longer has deny_unknown_fields — extra top-level keys are silently ignored
        let raw = r#"
[agent]
id = "scout"

[typo_section]
foo = "bar"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_ok(), "unknown top-level section should be ignored now");
    }

    #[test]
    fn agent_configs_single_agent_form() {
        let raw = r#"
[agent]
id = "solo"
task = "do something"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].id, "solo");
    }

    #[test]
    fn agent_configs_multi_agent_form() {
        let raw = r#"
[[agents]]
id = "alpha"
task = "task a"

[[agents]]
id = "beta"
task = "task b"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].id, "alpha");
        assert_eq!(cfgs[1].id, "beta");
    }

    #[test]
    fn agent_configs_both_set_is_error() {
        let raw = r#"
[agent]
id = "solo"
task = "task"

[[agents]]
id = "alpha"
task = "task a"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let result = cfg.agent_configs();
        assert!(result.is_err(), "expected Err when both [agent] and [[agents]] are set");
        assert!(result.unwrap_err().to_string().contains("cannot set both"));
    }

    #[test]
    fn agent_configs_neither_set_is_error() {
        let raw = r#"
[model]
model = "claude-sonnet-4-6"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let result = cfg.agent_configs();
        assert!(result.is_err(), "expected Err when neither [agent] nor [[agents]] is set");
        assert!(result.unwrap_err().to_string().contains("no agents configured"));
    }

    #[test]
    fn scheduler_config_explicit_values_parse() {
        let raw = r#"
[agent]
id = "a"

[scheduler]
global_token_budget = 1000
max_concurrent_inferences = 4
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.global_token_budget, 1000);
        assert_eq!(cfg.scheduler.max_concurrent_inferences, 4);
    }

    #[test]
    fn scheduler_config_defaults_to_unlimited() {
        let raw = r#"
[agent]
id = "a"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.global_token_budget, 0, "0 means unlimited");
        assert_eq!(cfg.scheduler.max_concurrent_inferences, 0, "0 means unlimited");
        assert_eq!(cfg.scheduler.max_spawn_depth, 4, "default spawn depth must be 4");
    }

    #[test]
    fn scheduler_config_max_spawn_depth_explicit() {
        let raw = r#"
[agent]
id = "a"

[scheduler]
max_spawn_depth = 2
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.max_spawn_depth, 2);
    }

    #[test]
    fn scheduler_config_default_impl_has_spawn_depth_4() {
        // SchedulerConfig::default() must return max_spawn_depth=4 so that
        // Rust-constructed configs (as used in tests) are not silently broken.
        let sc = SchedulerConfig::default();
        assert_eq!(sc.max_spawn_depth, 4);
    }

    #[test]
    fn spawn_config_deserializes_task_only() {
        let raw = r#"{"task": "do the thing"}"#;
        let sc: SpawnConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(sc.task, "do the thing");
        assert!(sc.child_id.is_none());
        assert_eq!(sc.priority, 0);
        assert!(sc.token_budget.is_none());
    }

    #[test]
    fn spawn_config_deserializes_all_fields() {
        let raw = r#"{"task":"sub","child_id":"my-child","priority":5,"token_budget":50000}"#;
        let sc: SpawnConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(sc.task, "sub");
        assert_eq!(sc.child_id.as_deref(), Some("my-child"));
        assert_eq!(sc.priority, 5);
        assert_eq!(sc.token_budget, Some(50_000));
    }

    #[test]
    fn spawn_config_missing_task_is_error() {
        let raw = r#"{"child_id": "orphan"}"#;
        let result: Result<SpawnConfig, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "SpawnConfig requires `task`");
    }

    #[test]
    fn agent_priority_parses_from_toml() {
        let raw = r#"
[[agents]]
id = "high"
task = "task"
priority = 10

[[agents]]
id = "low"
task = "task"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs[0].priority, 10);
        assert_eq!(cfgs[1].priority, 0, "absent priority defaults to 0");
    }

    #[test]
    fn capabilities_absent_defaults_to_none() {
        let raw = r#"
[agent]
id = "a"
task = "task"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert!(cfgs[0].capabilities.is_none(), "absent capabilities = unrestricted");
    }

    #[test]
    fn capabilities_empty_array_is_deny_all() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = []
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs[0].capabilities, Some(vec![]));
    }

    #[test]
    fn capabilities_fs_read_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = [{ FsRead = { prefix = "/workspace" } }]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(
            cfgs[0].capabilities,
            Some(vec![Capability::FsRead {
                prefix: "/workspace".to_string()
            }])
        );
    }

    #[test]
    fn capabilities_multiple_variants_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = [
  { FsRead = { prefix = "/workspace" } },
  { FsWrite = { prefix = "/tmp" } },
  { Mcp = { server = "echo", tools = ["echo_text"] } },
]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        let caps = cfgs[0].capabilities.as_ref().unwrap();
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0], Capability::FsRead { prefix: "/workspace".to_string() });
        assert_eq!(caps[1], Capability::FsWrite { prefix: "/tmp".to_string() });
        assert_eq!(
            caps[2],
            Capability::Mcp {
                server: "echo".to_string(),
                tools: vec!["echo_text".to_string()]
            }
        );
    }

    // ── p3.3: McpServerConfig capability field tests ─────────────────────────

    #[test]
    fn mcp_server_capabilities_absent_defaults_to_none() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.tools.mcp_servers.len(), 1);
        assert!(cfg.tools.mcp_servers[0].capabilities.is_none(),
            "absent capabilities field should default to None");
    }

    #[test]
    fn mcp_server_capabilities_empty_array_is_deny_all() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
capabilities = []
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.tools.mcp_servers[0].capabilities, Some(vec![]));
    }

    #[test]
    fn mcp_server_capabilities_with_fs_rules() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
capabilities = [
  { FsRead = { prefix = "/workspace" } },
  { FsWrite = { prefix = "/tmp" } },
]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let caps = cfg.tools.mcp_servers[0].capabilities.as_ref().unwrap();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0], Capability::FsRead { prefix: "/workspace".into() });
        assert_eq!(caps[1], Capability::FsWrite { prefix: "/tmp".into() });
    }

    // ── p4.2: IsolationMode + isolation field tests ──────────────────────────

    #[test]
    fn mcp_server_isolation_defaults_to_none() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.tools.mcp_servers[0].isolation, IsolationMode::None);
    }

    #[test]
    fn mcp_server_isolation_gvisor_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "secure"
command = "/usr/bin/python3"
isolation = "gvisor"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.tools.mcp_servers[0].isolation, IsolationMode::Gvisor);
    }

    #[test]
    fn mcp_server_isolation_unknown_value_is_error() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
isolation = "firecracker"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "unknown isolation value should fail to parse");
    }

    // ── p4.1: mcp_require_capabilities tests ─────────────────────────────────

    #[test]
    fn mcp_require_capabilities_defaults_to_false() {
        let raw = r#"
[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(!cfg.tools.mcp_require_capabilities,
            "mcp_require_capabilities must default to false");
    }

    #[test]
    fn mcp_require_capabilities_true_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[tools]
mcp_require_capabilities = true
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.tools.mcp_require_capabilities);
    }

    // ── p4.5: log_path config field tests ────────────────────────────────────

    #[test]
    fn log_path_absent_defaults_to_none() {
        let raw = r#"
[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.log_path.is_none(), "absent log_path must default to None");
    }

    #[test]
    fn log_path_parses_when_set() {
        let raw = r#"
log_path = "/var/log/agentd/flight.jsonl"

[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.log_path.as_deref(), Some("/var/log/agentd/flight.jsonl"));
    }

    // ── p7.1: HTTP MCP server config tests ────────────────────────────────────

    #[test]
    fn http_server_config_round_trips() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
headers_env = { Authorization = "LINEAR_MCP_TOKEN" }
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let srv = &cfg.tools.mcp_servers[0];
        assert_eq!(srv.name, "linear");
        assert_eq!(srv.url.as_deref(), Some("https://mcp.linear.app/mcp"));
        assert_eq!(srv.headers_env.get("Authorization").map(|s| s.as_str()), Some("LINEAR_MCP_TOKEN"));
        assert!(srv.command.is_empty(), "command defaults to empty for HTTP server");
    }

    #[test]
    fn http_server_validate_ok() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.tools.mcp_servers[0].validate().is_ok());
    }

    #[test]
    fn http_server_validate_rejects_http_url() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "bad"
url = "http://insecure.example.com/mcp"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.tools.mcp_servers[0].validate().unwrap_err();
        assert!(err.to_string().contains("https://"), "got: {err}");
    }

    #[test]
    fn both_url_and_command_is_validation_error() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "bad"
url = "https://example.com/mcp"
command = "/usr/bin/server"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.tools.mcp_servers[0].validate().unwrap_err();
        assert!(err.to_string().contains("both"), "got: {err}");
    }

    #[test]
    fn stdio_server_validate_rejects_missing_command() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "files"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.tools.mcp_servers[0].validate().unwrap_err();
        assert!(err.to_string().contains("no command"), "got: {err}");
    }

    #[test]
    fn is_http_returns_true_for_url_server() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "linear"
url = "https://mcp.linear.app/mcp"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.tools.mcp_servers[0].is_http());
    }

    #[test]
    fn is_http_returns_false_for_stdio_server() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(!cfg.tools.mcp_servers[0].is_http());
    }

    #[test]
    fn headers_env_defaults_to_empty() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.tools.mcp_servers[0].headers_env.is_empty());
    }

    #[test]
    fn http_server_validate_rejects_url_with_embedded_credentials() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "bad"
url = "https://user:secret@mcp.example.com/mcp"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.tools.mcp_servers[0].validate().unwrap_err();
        assert!(err.to_string().contains("headers_env"), "got: {err}");
    }

    #[test]
    fn http_server_validate_rejects_non_https_scheme() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "bad"
url = "ftp://example.com/mcp"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.tools.mcp_servers[0].validate().unwrap_err();
        assert!(err.to_string().contains("https://"), "got: {err}");
    }

    // ── p4.7: McpServerConfig.env field tests ────────────────────────────────

    #[test]
    fn mcp_server_env_field_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"

[tools.mcp_servers.env]
MY_KEY = "my_value"
OTHER = "42"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let env = &cfg.tools.mcp_servers[0].env;
        assert_eq!(env.get("MY_KEY").map(|s| s.as_str()), Some("my_value"));
        assert_eq!(env.get("OTHER").map(|s| s.as_str()), Some("42"));
    }

    #[test]
    fn mcp_server_env_defaults_to_empty() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[tools.mcp_servers]]
name = "echo"
command = "/usr/bin/echo"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.tools.mcp_servers[0].env.is_empty(),
            "absent env field must default to empty map");
    }

    // ── p5.1: MemoryConfig tests ──────────────────────────────────────────────

    #[test]
    fn memory_config_defaults_when_section_absent() {
        let raw = r#"
[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.memory.store_path, "memory.redb");
        assert!(cfg.memory.enabled);
    }

    #[test]
    fn memory_config_explicit_values_parse() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[memory]
store_path = "/var/lib/agentd/mem.redb"
enabled = false
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.memory.store_path, "/var/lib/agentd/mem.redb");
        assert!(!cfg.memory.enabled);
    }

    // ── p5.6: eviction config tests ──────────────────────────────────────────

    #[test]
    fn memory_config_eviction_fields_default_to_none_and_false() {
        let raw = r#"
[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.memory.max_entries_per_segment.is_none(), "max_entries_per_segment defaults None");
        assert!(cfg.memory.max_entry_age_days.is_none(), "max_entry_age_days defaults None");
        assert!(!cfg.memory.distill_on_complete, "distill_on_complete defaults false");
    }

    #[test]
    fn memory_config_eviction_fields_parse() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[memory]
max_entries_per_segment = 500
max_entry_age_days = 90
distill_on_complete = true
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.memory.max_entries_per_segment, Some(500));
        assert_eq!(cfg.memory.max_entry_age_days, Some(90));
        assert!(cfg.memory.distill_on_complete);
    }

    #[test]
    fn capabilities_kb_write_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "t"
capabilities = [{ KbWrite = { segment = "agent:scratch" } }]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let caps = cfg.agent_configs().unwrap();
        assert_eq!(
            caps[0].capabilities,
            Some(vec![Capability::KbWrite { segment: "agent:scratch".to_string() }])
        );
    }

    #[test]
    fn capabilities_kb_read_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "t"
capabilities = [{ KbRead = { segment = "agent:scratch" } }]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let caps = cfg.agent_configs().unwrap();
        assert_eq!(
            caps[0].capabilities,
            Some(vec![Capability::KbRead { segment: "agent:scratch".to_string() }])
        );
    }

    // ── p5.4: SegmentConfig / SegmentClass tests ──────────────────────────────

    #[test]
    fn segment_config_log_class_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[memory.segments]]
name = "kb:events"
class = "log"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.memory.segments.len(), 1);
        assert_eq!(cfg.memory.segments[0].name, "kb:events");
        assert!(matches!(cfg.memory.segments[0].class, crate::config::SegmentClass::Log));
    }

    #[test]
    fn segment_config_canon_class_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[memory.segments]]
name = "kb:docs"
class = "canon"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(matches!(cfg.memory.segments[0].class, crate::config::SegmentClass::Canon));
    }

    #[test]
    fn segment_config_scratch_class_parses() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[memory.segments]]
name = "kb:scratch"
class = "scratch"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(matches!(cfg.memory.segments[0].class, crate::config::SegmentClass::Scratch));
    }

    #[test]
    fn segment_config_multiple_segments_parse() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[memory.segments]]
name = "kb:log"
class = "log"

[[memory.segments]]
name = "kb:canon"
class = "canon"

[[memory.segments]]
name = "kb:notes"
class = "scratch"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.memory.segments.len(), 3);
        assert_eq!(cfg.memory.segments[0].name, "kb:log");
        assert_eq!(cfg.memory.segments[1].name, "kb:canon");
        assert_eq!(cfg.memory.segments[2].name, "kb:notes");
    }

    #[test]
    fn segment_config_invalid_class_is_error() {
        let raw = r#"
[agent]
id = "a"
task = "t"

[[memory.segments]]
name = "kb:bad"
class = "invalid"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "unknown segment class must fail to parse");
    }

    #[test]
    fn memory_config_defaults_have_empty_segments() {
        let raw = r#"
[agent]
id = "a"
task = "t"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.memory.segments.is_empty(), "default segments must be empty");
    }

    // ── p1.6: AgentCard tests ─────────────────────────────────────────────────

    #[test]
    fn agent_card_name_defaults_to_id() {
        let cfg = AgentConfig {
            id:              "scout".to_string(),
            task:            String::new(),
            max_turns:       20,
            token_budget:    100_000,
            priority:        0,
            capabilities:    None,
            name:            None,
            description:     String::new(),
            skills:          vec![],
            tier:            AgentTier::Native,
            command:         None,
            args:            vec![],
            isolation:       IsolationMode::None,
            max_wall_seconds: 0,
        };
        let card = AgentCard::from(&cfg);
        assert_eq!(card.id, "scout");
        assert_eq!(card.name, "scout", "name should default to id when absent");
        assert_eq!(card.description, "");
        assert!(card.skills.is_empty());
    }

    #[test]
    fn agent_card_explicit_name_and_skills() {
        let cfg = AgentConfig {
            id:              "reader".to_string(),
            task:            String::new(),
            max_turns:       20,
            token_budget:    100_000,
            priority:        0,
            capabilities:    None,
            name:            Some("File Reader".to_string()),
            description:     "Reads files".to_string(),
            skills:          vec!["read".to_string(), "summarize".to_string()],
            tier:            AgentTier::Native,
            command:         None,
            args:            vec![],
            isolation:       IsolationMode::None,
            max_wall_seconds: 0,
        };
        let card = AgentCard::from(&cfg);
        assert_eq!(card.name, "File Reader");
        assert_eq!(card.description, "Reads files");
        assert_eq!(card.skills, vec!["read", "summarize"]);
    }

    #[test]
    fn agent_config_name_description_skills_parse_from_toml() {
        let raw = r#"
[agent]
id = "assistant"
name = "My Assistant"
description = "A helpful assistant"
skills = ["research", "write"]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let ac = cfg.agent.unwrap();
        assert_eq!(ac.name.as_deref(), Some("My Assistant"));
        assert_eq!(ac.description, "A helpful assistant");
        assert_eq!(ac.skills, vec!["research", "write"]);
    }
}
