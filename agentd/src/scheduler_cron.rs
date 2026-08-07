//! Native cron scheduling for `[[jobs]]` (attn.4).
//!
//! `croner` supplies parsing and next-occurrence math (a hand-rolled parser was rejected at
//! `/autoplan` review — unattended calendar correctness on a PID-1 process is worth a small,
//! well-tested dependency). Everything `croner` does NOT give you — missed-fire catch-up,
//! schedule-change fingerprinting, and occurrence dedup — is hand-built here, ported from
//! `docker/cron_mcp.py`'s already-production-exercised semantics (`_apply_catchup`,
//! `_spec_fingerprint`, AUDIT-v0.97 P2-6) rather than invented fresh.
//!
//! UTC only. Nothing in this module (or anywhere else in the stack) understands local time —
//! CLAUDE.md flags this gap explicitly; this increment's stance is to name it loudly rather
//! than silently default to it. Every timestamp this module produces is a UTC epoch second.

use std::str::FromStr;

/// Per-job native-scheduling state. Persisted through the scheduler checkpoint (see
/// `checkpoint.rs`) so a restart doesn't lose next-fire tracking or double-fire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobScheduleState {
    /// Identity of the schedule string this state was computed against (see `fingerprint`).
    /// A restart with a CHANGED `schedule` must not honor a catch-up computed under the old
    /// expression — checked by the caller before trusting `next_fire_ts`/`persisted` here.
    pub fingerprint: String,
    /// Next computed fire time (UTC epoch seconds) under `fingerprint`'s schedule.
    pub next_fire_ts: i64,
    /// Occurrence id last acted on (fired, skipped, or shadow-logged) — see `occurrence_id`.
    /// Compared before dispatching so a crash between "decided to fire" and "checkpoint
    /// confirms it" cannot re-fire the SAME occurrence after restart; the existing
    /// `child_id` collision guard (derived from this same occurrence, see `scheduler.rs`)
    /// backs this up as a second, independent line of defense.
    pub last_occurrence_id: Option<String>,
    pub last_outcome: Option<JobFireOutcome>,
    /// Present only when `last_outcome == Some(Skipped)` — names which guard rejected the
    /// fire (unknown job id, child-id collision), surfaced to the operator instead of
    /// requiring a flight-log grep (attn.4 DX finding).
    #[serde(default)]
    pub last_skip_reason: Option<String>,
    /// Set at boot when `apply_catchup` decided this job's first fire is a missed-while-down
    /// catch-up (rather than a fresh on-time fire). Consumed and cleared by the first tick
    /// that acts on it, so the flag reflects only "was this the boot-time catch-up
    /// occurrence", not "any late fire ever".
    #[serde(default)]
    pub pending_catchup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum JobFireOutcome {
    Fired,
    Skipped,
    ShadowLogged,
    CaughtUp,
}

impl JobFireOutcome {
    pub const fn as_str(&self) -> &'static str {
        match self {
            JobFireOutcome::Fired => "fired",
            JobFireOutcome::Skipped => "skipped",
            JobFireOutcome::ShadowLogged => "shadow_logged",
            JobFireOutcome::CaughtUp => "caught_up",
        }
    }
}

/// Deterministic occurrence id — `job_id` + schedule `fingerprint` + the intended UTC epoch
/// second — so the SAME scheduled fire always produces the SAME id, no matter how many times
/// it's recomputed (e.g. across a restart), while a manual fire or a genuinely different
/// occurrence never collides with it. Charset-safe as a `child_id` component
/// (`[a-zA-Z0-9_-]`, per `scheduler.rs::validate_child_id`) — used directly to derive the
/// spawned child's id for scheduled fires, replacing the too-crude `{job_id}-{date}` scheme
/// (which breaks for multiple daily fires) with one unique per actual scheduled occurrence.
pub fn occurrence_id(job_id: &str, fingerprint: &str, intended_fire_ts: i64) -> String {
    let fp_digest = fingerprint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    format!("{job_id}-{fp_digest}-{intended_fire_ts}")
}

/// Identity of a schedule string — stable across restarts, changes iff the operator edits the
/// expression. Kept human-legible (not a hash) so it's directly readable in logs/checkpoints.
pub fn fingerprint(schedule: &str) -> String {
    schedule.trim().to_string()
}

/// Parse a schedule string as a STRICT 5-field cron expression. The single entry point for
/// turning a `schedule` string into a `croner::Cron` — every caller (validation, next-fire
/// computation) goes through this, never `croner::Cron::from_str` directly, so the field-
/// count check below can't be bypassed by a future call site.
///
/// `croner` accepts optional 6th (seconds) and 7th (year) fields by default — a real gap
/// found in review: an operator typo like `"0 8 * * * *"` (one extra field) would silently
/// re-interpret as a SIX-field seconds-bearing expression instead of being rejected, firing
/// at an unexpected time rather than degrading the job with a clear error (Codex review).
/// This project documents and promises a 5-field expression everywhere (docs, error
/// messages, `agentctl jobs`) — enforce that promise here, not just in prose.
fn parse_cron(schedule: &str) -> Result<croner::Cron, String> {
    let field_count = schedule.split_whitespace().count();
    if field_count != 5 {
        return Err(format!(
            "expected exactly 5 fields (minute hour day-of-month month day-of-week), got {field_count}"
        ));
    }
    croner::Cron::from_str(schedule).map_err(|e| e.to_string())
}

/// Compute the next occurrence strictly after `after_ts` (UTC epoch seconds) for a 5-field
/// cron expression. Errors carry the parse error's message — callers needing an operator-
/// facing message should use `Job::validate_schedule`, which adds the corrected-example
/// suffix.
pub fn next_fire_after(schedule: &str, after_ts: i64) -> Result<i64, String> {
    let cron = parse_cron(schedule)?;
    let after = chrono::DateTime::<chrono::Utc>::from_timestamp(after_ts, 0)
        .ok_or_else(|| format!("timestamp {after_ts} out of range"))?;
    let next = cron
        .find_next_occurrence(&after, false)
        .map_err(|e| e.to_string())?;
    Ok(next.timestamp())
}

/// Missed-fire catch-up decision. Inspired by `cron_mcp.py`'s `_apply_catchup`
/// (AUDIT-v0.97 P2-6) but **deliberately diverges from it in one load-bearing way**: this
/// returns the PERSISTED timestamp itself when catching up, never `now`.
///
/// `cron_mcp.py`'s version returns `now` because it has no occurrence-identity concept — it
/// only reports `{status:"fired"}` to an LLM, which decides what to do next. This module's
/// `next_fire_ts` IS an occurrence identity (fed into `occurrence_id` and, for native fires,
/// the derived `child_id`) — returning `now` here would mint a NEW identity for every
/// catch-up, tied to boot time rather than the original missed occurrence. Combined with a
/// restart that discards in-memory dedup state, that bug is fatal: a job that fired and
/// completed, then crashed/restarted before its NEXT real occurrence, would be caught up
/// under a **fresh, never-seen-before** identity — invisible to both the occurrence ledger
/// (which compares against the ORIGINAL timestamp) and the `child_id` collision guard (which
/// only catches an EXACT id match) — and get dispatched a second time. Returning `persisted`
/// preserves the original occurrence's identity: the tick that discovers `next_fire_ts <=
/// now` fires (or correctly skips, via the occurrence ledger) using the SAME identity the
/// missed occurrence always had, whether that tick happens immediately (this function still
/// makes it "due" right away, since `persisted <= now`) or was already recorded before a
/// crash (attn.4 adversarial review finding).
///
/// A persisted target under an OLD schedule must never be honored regardless — callers pass
/// `persisted = None` whenever `fingerprint` doesn't match, before calling this function
/// (see `fingerprint`).
pub fn apply_catchup(fresh: i64, persisted: Option<i64>, now: i64) -> i64 {
    match persisted {
        Some(p) if p <= now => p,
        _ => fresh,
    }
}

/// Human-readable interpretation of a schedule string for operator-facing surfaces
/// (`agentctl`, attn.4 DX finding: "don't make the operator hunt through docs to remember
/// field order" / "comments rot, render the interpretation instead"). Best-effort — falls
/// back to echoing the raw expression if `croner` can't describe it structurally; still
/// useful (raw cron is at least legible to anyone who already knows the syntax).
pub fn describe(schedule: &str) -> String {
    match parse_cron(schedule) {
        Ok(_) => format!("\"{schedule}\" (UTC)"),
        Err(e) => format!("\"{schedule}\" (UTC) — WARNING: {e}"),
    }
}

/// Operator-facing validation, shared by `Job::validate_schedule` (config.rs) so the SAME
/// strict-5-field check backs both the boot-time degrade path and any future caller —
/// `Job::validate_schedule` adds the job-id + corrected-example wrapping on top of this.
pub fn validate(schedule: &str) -> Result<(), String> {
    parse_cron(schedule).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrence_id_stable_for_same_inputs() {
        let a = occurrence_id("cos-inbox", "0 8 * * *", 1_700_000_000);
        let b = occurrence_id("cos-inbox", "0 8 * * *", 1_700_000_000);
        assert_eq!(a, b);
    }

    #[test]
    fn occurrence_id_differs_on_fingerprint_or_time() {
        let a = occurrence_id("cos-inbox", "0 8 * * *", 1_700_000_000);
        let b = occurrence_id("cos-inbox", "0 9 * * *", 1_700_000_000);
        let c = occurrence_id("cos-inbox", "0 8 * * *", 1_700_003_600);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn occurrence_id_is_child_id_safe() {
        let id = occurrence_id("cos-inbox", "0 8,17 * * *", 1_700_000_000);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn fingerprint_trims_but_is_otherwise_literal() {
        assert_eq!(fingerprint("  0 8 * * *  "), "0 8 * * *");
        assert_ne!(fingerprint("0 8 * * *"), fingerprint("0 9 * * *"));
    }

    #[test]
    fn next_fire_after_daily_expression() {
        // 2026-08-07T07:00:00Z -> next 08:00 UTC fire is the same day.
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-07T07:00:00Z")
            .unwrap()
            .timestamp();
        let next = next_fire_after("0 8 * * *", after).unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-08-07T08:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(next, expected);
    }

    #[test]
    fn next_fire_after_rolls_to_next_day_once_past() {
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-07T09:00:00Z")
            .unwrap()
            .timestamp();
        let next = next_fire_after("0 8 * * *", after).unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-08-08T08:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(next, expected);
    }

    #[test]
    fn next_fire_after_twice_daily() {
        let after = chrono::DateTime::parse_from_rfc3339("2026-08-07T09:00:00Z")
            .unwrap()
            .timestamp();
        let next = next_fire_after("0 8,17 * * *", after).unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-08-07T17:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(next, expected);
    }

    #[test]
    fn next_fire_after_rejects_malformed_expression() {
        assert!(next_fire_after("not a cron string", 0).is_err());
        assert!(next_fire_after("99 8 * * *", 0).is_err()); // minute out of range
    }

    #[test]
    fn next_fire_after_rejects_out_of_range_timestamp() {
        assert!(next_fire_after("0 8 * * *", i64::MAX).is_err());
    }

    /// Codex review finding: `croner` accepts optional 6th (seconds) / 7th (year) fields by
    /// default. A 5-field-only project promise (docs, error messages, agentctl output) must
    /// reject anything else explicitly, not silently reinterpret it as a different-shaped
    /// expression.
    #[test]
    fn rejects_expressions_with_a_seconds_or_year_field() {
        assert!(next_fire_after("0 8 * * * *", 0).is_err(), "6-field (seconds) must be rejected");
        assert!(next_fire_after("0 8 * * * * 2027", 0).is_err(), "7-field (seconds+year) must be rejected");
        assert!(next_fire_after("0 8 * * *", 0).is_ok(), "sanity: the correct 5-field form still works");
    }

    #[test]
    fn apply_catchup_missed_fire_while_down_preserves_original_identity() {
        // Persisted target is in the past relative to "now" (we were down past it). Returns
        // the ORIGINAL persisted timestamp (500), NOT `now` (900) — this is the load-bearing
        // divergence from cron_mcp.py documented above: `now` would mint a fresh occurrence
        // identity every catch-up, breaking cross-restart dedup (attn.4 adversarial finding).
        assert_eq!(apply_catchup(1000, Some(500), 900), 500);
    }

    #[test]
    fn apply_catchup_exact_boundary_persisted_equals_now_is_a_catchup() {
        assert_eq!(apply_catchup(1000, Some(900), 900), 900);
    }

    #[test]
    fn apply_catchup_persisted_still_in_future() {
        assert_eq!(apply_catchup(1000, Some(950), 900), 1000);
    }

    #[test]
    fn apply_catchup_no_persisted_state() {
        assert_eq!(apply_catchup(1000, None, 900), 1000);
    }

    #[test]
    fn describe_valid_and_invalid() {
        assert_eq!(describe("0 8 * * *"), "\"0 8 * * *\" (UTC)");
        assert!(describe("not a cron string").contains("WARNING"));
    }
}
