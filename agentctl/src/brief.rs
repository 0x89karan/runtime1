//! `agentctl brief` — pull the CoS morning brief (ux.11c).
//!
//! The brief is authored by agentd from durable run history and persisted, so this
//! command shows it regardless of when the operator looks (the F1 fix: a live-rail
//! push would have scrolled away before attach). HTTP-only surface: it talks to the
//! management API's `GET /api/v1/brief` directly (there is no FUSE `/agents/brief`).

use clap::Args;
use serde_json::Value;

use crate::watch::source::HttpSource;

const DEFAULT_URL: &str = "http://127.0.0.1:7999";

#[derive(Args, Debug)]
pub struct BriefArgs {
    /// Management API URL (default http://127.0.0.1:7999)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// Show the last N briefs instead of only the latest
    #[arg(long)]
    pub n: Option<usize>,
}

pub fn run(args: BriefArgs) -> anyhow::Result<()> {
    let base = args.url.unwrap_or_else(|| DEFAULT_URL.to_string());
    let source = HttpSource::new(base);
    let v = source
        .brief(args.n)
        .map_err(|e| anyhow::anyhow!("could not reach management API: {e}\nIs agentd running? Try --url http://HOST:7999"))?;

    // The endpoint returns {"error": "..."} on 503 (run history not configured). Without
    // this check that body has no "brief" key and would render as a misleading "No brief
    // yet" (review: both voices). Surface it as the error it is.
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("{err}");
    }

    let approvals = v["approvals_pending"].as_u64().unwrap_or(0);
    match args.n {
        Some(_) => {
            let briefs = v["briefs"].as_array().cloned().unwrap_or_default();
            if briefs.is_empty() {
                println!("📋 No briefs yet.");
                return Ok(());
            }
            for (i, b) in briefs.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                print!("{}", render_brief(b, approvals));
            }
        }
        None => print!("{}", render_brief(&v["brief"], approvals)),
    }
    Ok(())
}

/// Render one brief, attention-first (failures/approvals before counts, IDs on every
/// attention line). `brief` may be JSON null when none has been published yet.
fn render_brief(brief: &Value, approvals: u64) -> String {
    if brief.is_null() {
        return "📋 No brief yet — the Chief of Staff writes one each cron cycle.\n".to_string();
    }
    let run_count = brief["run_count"].as_u64().unwrap_or(0);
    let failed = brief["failed_count"].as_u64().unwrap_or(0);
    let window = window_label(brief);
    let mut out = String::new();

    if run_count == 0 {
        out.push_str(&format!("📋 Quiet night — 0 runs {window}"));
        if approvals > 0 {
            out.push_str(&format!(" · {approvals} need approval"));
        }
        out.push('\n');
        push_narrative(&mut out, brief);
        return out;
    }

    // Header: attention first (F4), never leads with the bare run count.
    out.push_str(&format!(
        "📋 {failed} failed · {approvals} need approval · {run_count} runs {window}\n"
    ));

    if let Some(items) = brief["items"].as_array() {
        for it in items {
            let icon = match it["status"].as_str().unwrap_or("") {
                "failed" => "✗",
                "running" => "⏳",
                "interrupted" => "⚠",
                _ => "•",
            };
            let agent = it["agent_id"].as_str().unwrap_or("?");
            let status = it["status"].as_str().unwrap_or("?");
            let run_id = it["run_id"].as_str().unwrap_or("?");
            let spend = it["spend"].as_u64().map(|s| format!("  {s}t")).unwrap_or_default();
            let detail = it["last_error"]
                .as_str()
                .or_else(|| it["stop_reason"].as_str())
                .map(|d| format!("  — {d}"))
                .unwrap_or_default();
            out.push_str(&format!("  {icon} {agent}  {status}  ({run_id}){spend}{detail}\n"));
        }
    }
    // Truncated attention items (>100) are NOT "ok" — surface them distinctly (review M1).
    let attention_overflow = brief["attention_overflow"].as_u64().unwrap_or(0);
    if attention_overflow > 0 {
        out.push_str(&format!("  ⚠ {attention_overflow} more need attention (see runs_query)\n"));
    }
    let overflow = brief["overflow_count"].as_u64().unwrap_or(0);
    if overflow > 0 {
        out.push_str(&format!("  ✓ {overflow} others ok\n"));
    }
    push_narrative(&mut out, brief);
    out
}

fn push_narrative(out: &mut String, brief: &Value) {
    if let Some(n) = brief["narrative"].as_str() {
        if !n.is_empty() {
            out.push_str(&format!("\n{n}\n"));
        }
    }
}

/// "past 24h" from the window span, falling back to empty when unavailable.
fn window_label(brief: &Value) -> String {
    match (brief["window_from"].as_u64(), brief["window_to"].as_u64()) {
        (Some(f), Some(t)) if t >= f => {
            let hours = (t - f) / 3600;
            if hours == 0 { "· past hour".to_string() } else { format!("· past {hours}h") }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_no_brief_when_null() {
        let s = render_brief(&Value::Null, 0);
        assert!(s.contains("No brief yet"));
    }

    #[test]
    fn quiet_night_states_zero_explicitly() {
        let b = json!({"run_count": 0, "failed_count": 0, "window_from": 0, "window_to": 86_400, "items": []});
        let s = render_brief(&b, 0);
        assert!(s.contains("Quiet night — 0 runs"));
        assert!(s.contains("past 24h"));
    }

    #[test]
    fn leads_with_attention_and_names_ids() {
        let b = json!({
            "run_count": 12, "failed_count": 1, "overflow_count": 10, "attention_overflow": 0,
            "window_from": 0, "window_to": 86_400,
            "items": [
                {"run_id": "scout:3", "agent_id": "scout", "status": "failed", "spend": 140, "last_error": "timeout"},
                {"run_id": "curator:2", "agent_id": "curator", "status": "running"}
            ],
            "narrative": "one failure overnight"
        });
        let s = render_brief(&b, 2);
        // Header leads with failures + approvals, not the run count.
        assert!(s.starts_with("📋 1 failed · 2 need approval · 12 runs"));
        assert!(s.contains("✗ scout  failed  (scout:3)  140t  — timeout"));
        assert!(s.contains("⏳ curator  running  (curator:2)"));
        assert!(s.contains("✓ 10 others ok"));
        assert!(s.contains("one failure overnight"));
    }

    #[test]
    fn truncated_attention_not_labeled_ok() {
        // Review M1: attention_overflow renders as "need attention", never folded into "ok".
        let b = json!({
            "run_count": 123, "failed_count": 120, "overflow_count": 3, "attention_overflow": 20,
            "window_from": 0, "window_to": 3600,
            "items": [{"run_id": "f:1", "agent_id": "f", "status": "failed"}]
        });
        let s = render_brief(&b, 0);
        assert!(s.contains("⚠ 20 more need attention"));
        assert!(s.contains("✓ 3 others ok"));
        assert!(!s.contains("✓ 20"), "truncated failures must not appear as ok");
    }
}
