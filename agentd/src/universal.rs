use std::{net::SocketAddr, time::SystemTime};

use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    config::{AgentConfig, AgentTier, IsolationMode},
    events::EventKind,
    flight_recorder::FlightRecorder,
};

/// A universal-tier agent: an external child process whose LLM traffic is
/// routed through the egress proxy. Contrast with native-tier AgentTask which
/// runs in-process.
pub struct UniversalAgent {
    pub id:            String,
    pub child:         tokio::process::Child,
    pub isolation:     IsolationMode,
    pub started_at:    SystemTime,
    pub cfg:           AgentConfig,
    /// Ephemeral key registered in ProxyRegistry for this agent's egress calls.
    pub ephemeral_key: String,
}

impl UniversalAgent {
    /// Spawn the child process. Clears the environment and injects only the
    /// approved variables (PATH, HOME, USER, LANG, TMPDIR) plus the egress
    /// proxy vars. The real ANTHROPIC_API_KEY is never passed to the child.
    pub fn spawn(
        cfg:           &AgentConfig,
        egress_addr:   SocketAddr,
        ephemeral_key: &str,
        recorder:      &FlightRecorder,
    ) -> Result<Self> {
        debug_assert_eq!(cfg.tier, AgentTier::Universal);

        let command = cfg
            .command
            .as_deref()
            .context("universal-tier agent requires `command` to be set")?;

        let effective_isolation = if cfg.isolation == IsolationMode::Gvisor {
            // Check that runsc is on PATH; fail-fast if absent — silently removing
            // the isolation boundary defeats the purpose of requesting gVisor.
            match which_runsc() {
                Some(_) => IsolationMode::Gvisor,
                None => {
                    recorder.record(
                        &cfg.id,
                        None,
                        EventKind::UniversalAgentIsolationDegraded,
                        json!({ "agent_id": &cfg.id, "reason": "runsc_not_found" }),
                    );
                    anyhow::bail!(
                        "agent '{}' requires isolation = \"gvisor\" but 'runsc' is not found on PATH.\n\
                         Install gVisor: https://gvisor.dev/docs/user_guide/install/\n\
                         Then verify with: runsc --version",
                        cfg.id
                    );
                }
            }
        } else {
            IsolationMode::None
        };

        let mut cmd = if effective_isolation == IsolationMode::Gvisor {
            let mut c = tokio::process::Command::new("runsc");
            c.arg("do").arg("--network=host");
            c.arg(command);
            c
        } else {
            tokio::process::Command::new(command)
        };

        for arg in &cfg.args {
            cmd.arg(arg);
        }

        // Clear environment; inject only the allowlist.
        cmd.env_clear();
        for var in &["PATH", "HOME", "USER", "LANG", "TMPDIR"] {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        // Egress proxy config — child gets its ephemeral key, not the real key.
        cmd.env("ANTHROPIC_API_KEY", ephemeral_key);
        cmd.env("ANTHROPIC_BASE_URL", format!("http://{egress_addr}"));

        // Null stdin: avoids sharing agentd's stdin fd with the child.
        cmd.stdin(std::process::Stdio::null());
        // Inherit parent stdio: child diagnostics flow to agentd's stderr;
        // the child's own stdout goes to agentd's stdout (pipeline-safe since
        // universal agents are not expected to write the native final-answer line).
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());

        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn universal agent '{}'", cfg.id))?;

        let pid = child.id();
        recorder.record(
            &cfg.id,
            None,
            EventKind::UniversalAgentStarted,
            json!({
                "isolation": match effective_isolation {
                    IsolationMode::Gvisor => "gvisor",
                    IsolationMode::None   => "none",
                },
                "pid": pid,
                "command": command,
            }),
        );

        Ok(Self {
            id:            cfg.id.clone(),
            child,
            isolation:     effective_isolation,
            started_at:    SystemTime::now(),
            cfg:           cfg.clone(),
            ephemeral_key: ephemeral_key.to_string(),
        })
    }

    /// Returns the process ID, if the child is still alive.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Non-blocking poll for exit. Returns `Some(status)` if the child has exited.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Graceful shutdown: SIGTERM, wait up to 5 s, then SIGKILL.
    pub async fn kill(&mut self) {
        use std::time::Duration;

        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            match tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await {
                Ok(_) => return,
                Err(_) => {
                    let _ = self.child.kill().await;
                }
            }
            return;
        }

        // Non-Unix or pid already gone — just force-kill.
        let _ = self.child.kill().await;
    }

    /// Wall-clock seconds this agent has been running.
    pub fn wall_seconds(&self) -> u64 {
        self.started_at
            .elapsed()
            .unwrap_or_default()
            .as_secs()
    }
}

/// Return the path to `runsc` if found on PATH, else None.
pub fn which_runsc() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .iter()
        .flat_map(|p| std::env::split_paths(p))
        .map(|d| d.join("runsc"))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tier_serde_roundtrip() {
        use crate::config::AgentTier;
        let n: AgentTier = serde_json::from_str("\"native\"").unwrap();
        assert_eq!(n, AgentTier::Native);
        let u: AgentTier = serde_json::from_str("\"universal\"").unwrap();
        assert_eq!(u, AgentTier::Universal);
        let d: AgentTier = serde_json::from_str("\"native\"").unwrap();
        assert_eq!(d, AgentTier::default());
    }

    #[test]
    fn isolation_mode_serde_roundtrip() {
        let n: IsolationMode = serde_json::from_str("\"none\"").unwrap();
        assert_eq!(n, IsolationMode::None);
        let g: IsolationMode = serde_json::from_str("\"gvisor\"").unwrap();
        assert_eq!(g, IsolationMode::Gvisor);
    }

    #[test]
    fn agent_config_universal_defaults() {
        let toml = r#"
            id = "bot"
            task = "do stuff"
            tier = "universal"
            command = "/usr/bin/python3"
        "#;
        let cfg: crate::config::AgentConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.tier, crate::config::AgentTier::Universal);
        assert_eq!(cfg.command.as_deref(), Some("/usr/bin/python3"));
        assert!(cfg.args.is_empty());
        assert_eq!(cfg.isolation, IsolationMode::None);
        assert_eq!(cfg.max_wall_seconds, 0);
    }

    #[test]
    fn config_backward_compat_no_tier_field() {
        // Agents without an explicit `tier` field must default to native.
        let toml = r#"
            id = "bot"
            task = "do stuff"
        "#;
        let cfg: crate::config::AgentConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.tier, crate::config::AgentTier::Native);
        assert!(cfg.command.is_none());
    }

    #[test]
    fn config_unknown_isolation_errors_for_agents() {
        // AC5: isolation = "firecracker" (unknown variant) must fail to parse for [[agents]].
        let toml = r#"
            id = "bot"
            task = "work"
            isolation = "firecracker"
        "#;
        assert!(
            toml::from_str::<crate::config::AgentConfig>(toml).is_err(),
            "unknown isolation value must be a TOML parse error"
        );
    }

    #[test]
    fn config_native_forbids_command() {
        // Validation: tier=native with command=set must be caught at startup.
        // This test verifies the config parses fine (the runtime rejects it, not the parser).
        let toml = r#"
            id = "bot"
            task = "work"
            tier = "native"
            command = "/usr/bin/python3"
        "#;
        let cfg: crate::config::AgentConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.tier, crate::config::AgentTier::Native);
        assert_eq!(cfg.command.as_deref(), Some("/usr/bin/python3"));
        // The main.rs startup validation loop rejects this combination.
        // Here we just confirm the field survives TOML parsing (serde does not reject it —
        // the enforcement is at the scheduler startup boundary, not in the type system).
    }

    #[test]
    fn universal_spawn_fails_without_command() {
        // tier=universal with command=None must return Err before touching the OS.
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = crate::config::AgentConfig {
            id:               "bot".to_string(),
            task:             "work".to_string(),
            max_turns:        1,
            token_budget:     1000,
            priority:         0,
            capabilities:     None,
            name:             None,
            description:      String::new(),
            skills:           vec![],
            tier:             crate::config::AgentTier::Universal,
            command:          None,
            args:             vec![],
            isolation:        IsolationMode::None,
            max_wall_seconds: 0,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let result = UniversalAgent::spawn(&cfg, addr, "test-key", &rec);
        let err = result.err().expect("expected Err").to_string();
        assert!(err.contains("command"), "error must mention 'command', got: {err}");
    }

    #[test]
    fn universal_agent_fail_fast_no_runsc() {
        // When isolation=gvisor is requested and runsc is absent, spawn() must return Err.
        // Only meaningful when runsc is NOT installed; skip if it is present.
        if which_runsc().is_some() {
            return; // runsc available — gvisor path works; this test covers the absent case
        }
        let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = crate::config::AgentConfig {
            id:               "gvisor-bot".to_string(),
            task:             "work".to_string(),
            max_turns:        1,
            token_budget:     1000,
            priority:         0,
            capabilities:     None,
            name:             None,
            description:      String::new(),
            skills:           vec![],
            tier:             crate::config::AgentTier::Universal,
            command:          Some("true".to_string()),
            args:             vec![],
            isolation:        IsolationMode::Gvisor,
            max_wall_seconds: 0,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let result = UniversalAgent::spawn(&cfg, addr, "test-key", &rec);
        let err = result.err().expect("expected Err when runsc absent").to_string();
        assert!(err.contains("runsc"), "error must mention 'runsc', got: {err}");
        // The IsolationDegraded event must be recorded before the error.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(log.contains("universal_agent_isolation_degraded"), "flight log must record isolation_degraded event");
    }

}
