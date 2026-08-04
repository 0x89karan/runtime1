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

    print!("{}", render_response(&v, args.n.is_some()));
    Ok(())
}

/// Render a whole `GET /api/v1/brief` response body.
///
/// This exists so the WIRE FIELD NAMES are testable. /review found that testing
/// `render_brief` directly — passing `Option<u64>` in by hand — cannot catch a rename of
/// `server_now` or `created_at` on either side: the output degrades to the deliberately
/// silent `None` path, every test stays green, and the staleness feature is gone. The first
/// attempt at a guard for this re-read the key inside the test body, which pinned the
/// TEST's literal rather than the production one and was itself vacuous (mutation-proven).
/// So the extraction lives here, and the test drives this function.
fn render_response(v: &Value, want_list: bool) -> String {
    let approvals = v["approvals_pending"].as_u64().unwrap_or(0);
    // The server's clock, so age comes from ONE clock rather than differencing this host's
    // against the daemon's. Absent on a pre-attn.1a agentd, in which case `None` disables
    // the staleness line rather than faking it.
    let server_now = v["server_now"].as_u64();
    // Resolved HERE, at the process boundary, and passed down explicitly. Reading the env
    // inside `render_brief` made the whole test suite environment-dependent — anyone with
    // AGENTCTL_BRIEF_STALE_HOURS exported got failing tests for no reason. Caught by
    // running the suite with it set; see TODOS test-flake-01 for why that class matters.
    let stale_after = stale_after_secs();
    let mut out = String::new();
    if want_list {
        let briefs = v["briefs"].as_array().cloned().unwrap_or_default();
        if briefs.is_empty() {
            return "📋 No briefs yet.\n".to_string();
        }
        for (i, b) in briefs.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            // Only index 0 is the newest — see `render_brief`'s `is_latest` contract.
            out.push_str(&render_brief(b, approvals, server_now, i == 0, stale_after));
        }
    } else {
        out.push_str(&render_brief(&v["brief"], approvals, server_now, true, stale_after));
    }
    out
}

/// Default staleness threshold: a brief older than this means the DAILY cron missed a cycle.
///
/// `TRIGGER_CRON=0 8 * * *` (docker-compose.yml) is the default schedule, so a healthy brief
/// is under 24 h old; the 2 h grace absorbs a slow cycle, a restart, and looking at 07:00
/// before the 08:00 fire.
///
/// This is tied to the DEFAULT cadence and the renderer cannot discover the operator's real
/// one — the management API does not report it. So a non-default schedule needs
/// `AGENTCTL_BRIEF_STALE_HOURS`: twice-daily (`0 8,17 * * *`, documented in compose) can miss
/// a cycle WITHOUT tripping 26 h, and anything slower than daily false-alarms on every
/// healthy brief. (/review, Codex.)
const STALE_AFTER_SECS: u64 = 26 * 3600;

/// Operator override for the staleness threshold, in hours.
///
/// Returns the default when unset, unparseable, or zero — a bad value must not silently
/// disable the warning (0 would make every brief stale) nor panic a read-only command.
/// Read once in `render_response`, never inside the renderer — see the note there.
fn stale_after_secs() -> u64 {
    match std::env::var("AGENTCTL_BRIEF_STALE_HOURS") {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(h) if h > 0 => h * 3600,
            _ => STALE_AFTER_SECS,
        },
        Err(_) => STALE_AFTER_SECS,
    }
}

/// Age of a brief against the server's own clock, or `None` when it cannot be computed.
///
/// Saturating on purpose: if a forward clock jump persisted a `created_at` in the future,
/// a wrapping subtraction would report a ~584-billion-hour-old brief. Clamping to 0 makes
/// it read as fresh, which is the honest failure direction — a clock anomaly should not
/// manufacture an alarm about the pipeline, and the next real cycle corrects it.
fn brief_age_secs(brief: &Value, server_now: Option<u64>) -> Option<u64> {
    let created = brief["created_at"].as_u64()?;
    Some(server_now?.saturating_sub(created))
}

/// "3h" / "2d 4h" — a compact age for humans.
fn humanize_age(secs: u64) -> String {
    let h = secs / 3600;
    if h < 1 {
        return format!("{}m", secs / 60);
    }
    if h < 48 {
        return format!("{h}h");
    }
    format!("{}d {}h", h / 24, h % 24)
}

/// Render one brief, attention-first (failures/approvals before counts, IDs on every
/// attention line).
///
/// * `brief` — may be JSON null when none has been published yet.
/// * `approvals` — live pending-approval count, overlaid by the API at request time.
/// * `server_now` — the SERVER's unix clock (`GET /api/v1/brief`), or `None` against a
///   pre-attn.1a agentd. Age is computed from this rather than the local clock so that
///   `--url` pointed at another host does not report skew as staleness.
/// * `stale_after` — threshold in seconds, resolved by the caller (`render_response`) so
///   this function is a pure function of its arguments and the test suite cannot be
///   perturbed by the operator's environment.
/// * `is_latest` — whether this is the newest brief. Gates the `⚠ STALE` banner ONLY;
///   the age suffix renders regardless. In `--n K` mode the caller passes `i == 0`, which
///   is correct because `GET /api/v1/brief?n=K` returns newest-first (agentd's
///   `RunsStore::list_briefs` iterates `.rev()`, pinned by its `list_briefs_newest_first`
///   test). If that server-side ordering ever changed, the banner would land on the OLDEST
///   brief — this comment is the only place that cross-crate contract is recorded.
fn render_brief(
    brief: &Value,
    approvals: u64,
    server_now: Option<u64>,
    is_latest: bool,
    stale_after: u64,
) -> String {
    if brief.is_null() {
        return "📋 No brief yet — the Chief of Staff writes one each cron cycle.\n".to_string();
    }
    let run_count = brief["run_count"].as_u64().unwrap_or(0);
    let failed = brief["failed_count"].as_u64().unwrap_or(0);
    let window = window_label(brief);
    let mut out = String::new();

    // attn.1a: staleness FIRST, above everything.
    //
    // Without this line an eight-day-old brief renders identically to one written five
    // minutes ago, so the operator reads stale content as today's news and the pipeline
    // having stopped is invisible. That is not hypothetical: ~/.agentos-output/ held
    // three briefs in fifteen days and nothing on any surface said so.
    //
    // It leads rather than trails because everything below it is untrustworthy when it
    // fires — the counts, the attention items and the narrative all describe a window
    // that closed days ago.
    // Only the LATEST brief's age says anything about whether the pipeline is alive.
    // In `--n K` mode briefs 2..K are old BY DEFINITION — that is what a history listing
    // is — so banner-ing each of them turned `agentctl brief --n 3` into three false
    // "the pipeline has missed a cycle" alarms. The age SUFFIX still renders on every
    // brief (it is useful there); only the alarm is latest-only.
    let age = brief_age_secs(brief, server_now);
    if let Some(secs) = age {
        if is_latest && secs > stale_after {
            out.push_str(&format!(
                "⚠ STALE — this brief is {} old; the pipeline has missed at least one daily cycle.\n\
                 \x20 Everything below describes that window, not today. Check: docker compose ps cos\n",
                humanize_age(secs)
            ));
        }
    }

    if run_count == 0 {
        out.push_str(&format!("📋 Quiet night — 0 runs {window}"));
        if approvals > 0 {
            out.push_str(&format!(" · {approvals} need approval"));
        }
        out.push_str(&age_suffix(age));
        out.push('\n');
        // R3.4-ar-02 (QA on the real binary, attn.2): this branch used to `return` here,
        // before ever reaching the suppressed_count block below — so a genuinely quiet
        // run_count (the inbox job's own run excluded from the count, or simply nothing
        // else ran) silently hid a real, nonzero suppressed_count. Driving publish_brief
        // against a live agentd reproduced it directly: three real briefs, all with
        // run_count=0, and `agentctl brief` printed nothing about suppression for any of
        // them, including the one carrying suppressed_count=7.
        push_suppressed_count_line(&mut out, brief);
        push_narrative(&mut out, brief);
        return out;
    }

    // Header: attention first (F4), never leads with the bare run count.
    // attn.1a appends the age so even a FRESH brief says when it was written — the
    // operator should never have to guess whether they are looking at today's.
    out.push_str(&format!(
        "📋 {failed} failed · {approvals} need approval · {run_count} runs {window}{}\n",
        age_suffix(age)
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
    push_suppressed_count_line(&mut out, brief);
    push_narrative(&mut out, brief);
    out
}

/// R3.4 (attn.2): model-reported, and Some(0) is a real "reported, nothing suppressed"
/// answer distinct from None ("didn't report"). Shown whenever present, even at 0 — the
/// whole reason this field exists is so silent suppression cannot look like a track with
/// no problem to solve (attn.2 plan, R3.4). Absent (pre-R3.4 agentd, or the inbox job
/// didn't report one) renders nothing, matching every other optional count on this brief.
///
/// Called from BOTH `render_brief` branches (quiet-night early-return and the normal
/// path) — R3.4-ar-02 found that only the normal path called this, so a quiet run_count
/// silently hid a real suppressed_count.
fn push_suppressed_count_line(out: &mut String, brief: &Value) {
    if let Some(suppressed) = brief["suppressed_count"].as_u64() {
        if suppressed > 0 {
            out.push_str(&format!("  ⚠ {suppressed} suppressed (exceeded Gmail fetch cap)\n"));
        } else {
            out.push_str("  ✓ 0 suppressed (all matching mail reviewed)\n");
        }
    }
}

/// " · written 3h ago", or empty when the age is unknown (pre-attn.1a daemon).
///
/// Deliberately shown on fresh briefs too, not only stale ones: an absent warning is a
/// weaker signal than a present timestamp, and this is the line that answers "is this
/// today's?" without the operator having to know what STALE_AFTER_SECS is.
fn age_suffix(age: Option<u64>) -> String {
    match age {
        Some(secs) => format!(" · written {} ago", humanize_age(secs)),
        None => String::new(),
    }
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
        let s = render_brief(&Value::Null, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("No brief yet"));
    }

    #[test]
    fn quiet_night_states_zero_explicitly() {
        let b = json!({"run_count": 0, "failed_count": 0, "window_from": 0, "window_to": 86_400, "items": []});
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("Quiet night — 0 runs"));
        assert!(s.contains("past 24h"));
    }

    #[test]
    fn quiet_night_still_surfaces_a_real_suppressed_count() {
        // R3.4-ar-02: found by driving a real agentd — publish_brief with run_count=0 (the
        // inbox job's own run wasn't counted) and suppressed_count=7 rendered NOTHING about
        // suppression, because the quiet-night branch returned before the suppressed_count
        // block. Fixed by calling push_suppressed_count_line from both branches.
        let b = json!({
            "run_count": 0, "failed_count": 0, "window_from": 0, "window_to": 86_400,
            "items": [], "suppressed_count": 7
        });
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("Quiet night — 0 runs"));
        assert!(s.contains("⚠ 7 suppressed (exceeded Gmail fetch cap)"));
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
        let s = render_brief(&b, 2, None, true, STALE_AFTER_SECS);
        // Header leads with failures + approvals, not the run count.
        assert!(s.starts_with("📋 1 failed · 2 need approval · 12 runs"));
        assert!(s.contains("✗ scout  failed  (scout:3)  140t  — timeout"));
        assert!(s.contains("⏳ curator  running  (curator:2)"));
        assert!(s.contains("✓ 10 others ok"));
        assert!(s.contains("one failure overnight"));
    }

    #[test]
    fn suppressed_count_absent_renders_nothing() {
        // Pre-R3.4 agentd, or an inbox job that didn't report one — must not fabricate a count.
        let b = json!({
            "run_count": 1, "failed_count": 0, "window_from": 0, "window_to": 86_400,
            "items": []
        });
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(!s.contains("suppressed"));
    }

    #[test]
    fn suppressed_count_zero_is_shown_not_omitted() {
        // R3.4: Some(0) is "reported, nothing suppressed" — a real answer, not silence.
        // Omitting it here would let suppression look unmeasured rather than measured-as-zero.
        let b = json!({
            "run_count": 1, "failed_count": 0, "window_from": 0, "window_to": 86_400,
            "items": [], "suppressed_count": 0
        });
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("✓ 0 suppressed (all matching mail reviewed)"));
    }

    #[test]
    fn suppressed_count_nonzero_warns() {
        let b = json!({
            "run_count": 1, "failed_count": 0, "window_from": 0, "window_to": 86_400,
            "items": [], "suppressed_count": 7
        });
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("⚠ 7 suppressed (exceeded Gmail fetch cap)"));
    }

    #[test]
    fn truncated_attention_not_labeled_ok() {
        // Review M1: attention_overflow renders as "need attention", never folded into "ok".
        let b = json!({
            "run_count": 123, "failed_count": 120, "overflow_count": 3, "attention_overflow": 20,
            "window_from": 0, "window_to": 3600,
            "items": [{"run_id": "f:1", "agent_id": "f", "status": "failed"}]
        });
        let s = render_brief(&b, 0, None, true, STALE_AFTER_SECS);
        assert!(s.contains("⚠ 20 more need attention"));
        assert!(s.contains("✓ 3 others ok"));
        assert!(!s.contains("✓ 20"), "truncated failures must not appear as ok");
    }

    // ── attn.1a: staleness ────────────────────────────────────────────────────
    // The defect these guard: a brief written eight days ago rendered identically to
    // one written five minutes ago, so "the pipeline stopped" was invisible on every
    // surface. ~/.agentos-output/ held 3 briefs in 15 days and nothing said so.

    fn brief_at(created_at: u64) -> Value {
        json!({
            "created_at": created_at, "run_count": 4, "failed_count": 0,
            "overflow_count": 4, "attention_overflow": 0,
            "window_from": 0, "window_to": 86_400, "items": []
        })
    }

    #[test]
    fn stale_brief_is_flagged_before_anything_else() {
        // 8 days old — the real gap between 07-23 and 07-31.
        let now = 1_000_000_000u64;
        let s = render_brief(&brief_at(now - 8 * 86_400), 0, Some(now), true, STALE_AFTER_SECS);
        assert!(s.starts_with("⚠ STALE"), "staleness must LEAD, got: {s}");
        assert!(s.contains("8d 0h"), "age must be stated, got: {s}");
        assert!(
            s.contains("not today"),
            "must say the content is not current, got: {s}"
        );
        // The warning must precede the counts it invalidates.
        assert!(s.find("⚠ STALE").unwrap() < s.find("📋").unwrap());
    }

    #[test]
    fn fresh_brief_has_no_stale_warning_but_still_states_its_age() {
        let now = 1_000_000_000u64;
        let s = render_brief(&brief_at(now - 3 * 3600), 0, Some(now), true, STALE_AFTER_SECS);
        assert!(!s.contains("STALE"), "3h-old brief must not be flagged: {s}");
        assert!(
            s.contains("written 3h ago"),
            "a fresh brief must STILL say when it was written — an absent warning is a \
             weaker signal than a present timestamp. got: {s}"
        );
    }

    #[test]
    fn stale_threshold_boundary_is_exclusive_at_26h() {
        let now = 1_000_000_000u64;
        // Exactly at the threshold: not yet stale (the 08:00 fire may be imminent).
        let at = render_brief(&brief_at(now - STALE_AFTER_SECS), 0, Some(now), true, STALE_AFTER_SECS);
        assert!(!at.contains("STALE"), "26h exactly must not be stale: {at}");
        // One second past: stale.
        let past = render_brief(&brief_at(now - STALE_AFTER_SECS - 1), 0, Some(now), true, STALE_AFTER_SECS);
        assert!(past.starts_with("⚠ STALE"), "26h+1s must be stale: {past}");
    }

    #[test]
    fn future_created_at_reads_as_fresh_rather_than_584_billion_hours_old() {
        // A forward clock jump can persist created_at ahead of now. A wrapping
        // subtraction would report u64::MAX secs and scream STALE at a healthy
        // pipeline; saturating to 0 fails in the honest direction.
        let now = 1_000_000_000u64;
        let s = render_brief(&brief_at(now + 7200), 0, Some(now), true, STALE_AFTER_SECS);
        assert!(!s.contains("STALE"), "clock skew must not manufacture an alarm: {s}");
        assert!(s.contains("written 0m ago"), "expected clamped-to-zero age, got: {s}");
    }

    #[test]
    fn missing_server_now_disables_staleness_instead_of_faking_it() {
        // A pre-attn.1a agentd does not send server_now. Guessing with the CLIENT's
        // clock would report daemon/client skew as pipeline staleness.
        let s = render_brief(&brief_at(1), 0, None, true, STALE_AFTER_SECS);
        assert!(!s.contains("STALE"));
        assert!(!s.contains("written"), "no age claim without a server clock: {s}");
    }

    #[test]
    fn quiet_night_also_carries_the_stale_warning() {
        // The 0-run path returns early — it must not skip the staleness check, or a
        // dead pipeline reads as a run of quiet nights, which is the worst confusion
        // available here.
        let now = 1_000_000_000u64;
        let b = json!({
            "created_at": now - 5 * 86_400, "run_count": 0, "failed_count": 0,
            "window_from": 0, "window_to": 86_400, "items": []
        });
        let s = render_brief(&b, 0, Some(now), true, STALE_AFTER_SECS);
        assert!(s.starts_with("⚠ STALE"), "quiet-night path must flag staleness: {s}");
        assert!(s.contains("Quiet night"));
        assert!(s.contains("written 5d 0h ago"));
    }

    #[test]
    fn history_listing_raises_no_stale_alarm_for_its_older_entries() {
        // `agentctl brief --n 3` lists the last three briefs. Briefs 2 and 3 are OLD BY
        // DEFINITION — that is what a history listing is. Only the LATEST brief's age
        // says anything about whether the pipeline is alive.
        let now = 1_000_000_000u64;
        let latest = brief_at(now - 3 * 3600);      // 3h — healthy
        let older  = brief_at(now - 8 * 86_400);    // 8d — normal for history
        let oldest = brief_at(now - 15 * 86_400);   // 15d — normal for history

        let out = format!("{}{}{}",
            render_brief(&latest, 0, Some(now), true, STALE_AFTER_SECS),
            render_brief(&older,  0, Some(now), false, STALE_AFTER_SECS),
            render_brief(&oldest, 0, Some(now), false, STALE_AFTER_SECS));

        let alarms = out.matches("⚠ STALE").count();
        assert_eq!(alarms, 0,
            "a history listing whose newest brief is 3h old must raise NO stale alarm; \
             got {alarms}. Output:\n{out}");
    }

    #[test]
    fn age_is_read_from_the_wire_fields_the_server_actually_emits() {
        // /review (testing specialist, CRITICAL): the other staleness tests inject
        // `Option<u64>` DIRECTLY into render_brief, so none of them exercises the
        // `v["server_now"].as_u64()` extraction in `run()`. Nothing in the workspace pinned
        // the literal key on the producer to the literal key on the consumer — rename
        // either `server_now` or `created_at` on either side and the output degrades to the
        // silent `None` path that `missing_server_now_disables_staleness_instead_of_faking_it`
        // deliberately blesses as CORRECT. Every test stays green; the feature is gone.
        //
        // This test goes THROUGH the extraction, against the exact body shape
        // agentd/src/management.rs emits.
        let now = 1_000_000_000u64;
        let body = json!({
            "brief": {
                "created_at": now - 40 * 3600,
                "run_count": 4, "failed_count": 0, "overflow_count": 4,
                "attention_overflow": 0, "window_from": 0, "window_to": 86_400, "items": []
            },
            "approvals_pending": 0,
            "server_now": now,
        });

        // Drive the PRODUCTION extraction. Re-reading the key inside this test would pin
        // the test's own literal and catch nothing — that mistake was made here once and
        // caught by mutation.
        let s = render_response(&body, false);
        // Assert on the AGE SUFFIX, not the STALE banner: `render_response` is the boundary
        // that reads AGENTCTL_BRIEF_STALE_HOURS, so asserting the banner here would make
        // this test fail for anyone with that variable exported — the very
        // environment-dependence the threshold was just moved to fix. The suffix proves both
        // wire keys were read; the banner logic is covered by the threshold tests above,
        // which pass STALE_AFTER_SECS explicitly.
        assert!(
            s.contains("written 40h ago"),
            "a 40h brief read off the wire must report its age. If this fails, the \
             `server_now` or `created_at` key drifted between management.rs and here. Got: {s}"
        );

        // And the list arm, which reads `briefs` rather than `brief`.
        let list_body = json!({
            "briefs": [body["brief"].clone()],
            "approvals_pending": 0,
            "server_now": now,
        });
        let l = render_response(&list_body, true);
        assert!(l.contains("written 40h ago"), "list arm lost the age: {l}");
    }

    #[test]
    fn brief_without_created_at_does_not_fake_an_age_against_a_live_server_clock() {
        // `brief_age_secs` has TWO independent `?` returns; only the server_now=None one was
        // covered. This is the other: a modern daemon that DID send server_now, against a
        // record whose `created_at` is missing or non-integer. Silent drop is the chosen
        // behaviour (better than a fabricated age), so pin it rather than leave it untested.
        let now = 1_000_000_000u64;
        let b = json!({"run_count": 4, "failed_count": 0, "window_from": 0,
                       "window_to": 86_400, "items": []});
        let s = render_brief(&b, 0, Some(now), true, STALE_AFTER_SECS);
        assert!(!s.contains("STALE"), "must not invent an alarm from a missing field: {s}");
        assert!(!s.contains("written"), "must not claim an age it cannot compute: {s}");

        let mut b2 = b.clone();
        b2["created_at"] = json!("2026-07-31T08:00:00Z"); // as_u64() rejects a string
        let s2 = render_brief(&b2, 0, Some(now), true, STALE_AFTER_SECS);
        assert!(!s2.contains("written"), "a string created_at must not render an age: {s2}");
    }

    #[test]
    fn humanize_age_spans_minutes_hours_days() {
        assert_eq!(humanize_age(0), "0m");
        assert_eq!(humanize_age(1800), "30m");
        assert_eq!(humanize_age(3600), "1h");
        assert_eq!(humanize_age(47 * 3600), "47h");
        assert_eq!(humanize_age(48 * 3600), "2d 0h");
        assert_eq!(humanize_age(50 * 3600), "2d 2h");
    }
}
