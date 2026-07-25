//! par.1 / AUDIT-v0.97 P2-10 (audit86-P2-13) — agentctl raw-event-kind drift guard.
//!
//! agentctl depends on the `agentd` crate but matches flight-recorder events by RAW
//! STRING (`line.contains("\"kind\":\"...\"")`, `kind == "..."`) in several places.
//! A renamed or removed `EventKind` variant then compiles cleanly and *silently*
//! blanks a TUI filter / colour rule / orchestrate-loop branch — the exact failure
//! mode the audit flagged.
//!
//! This test pins every event-kind string agentctl's PRODUCTION code matches on and
//! asserts each is a real `EventKind::as_str()` value — the single source of truth
//! added in par.1 (`agentd/src/events.rs`). Rename a variant and this test fails,
//! naming the orphaned string, instead of the TUI going quietly dark.
//!
//! When you add a new `"kind":"..."` match to agentctl, add it to `AGENTCTL_KIND_MATCHES`
//! below (with its site) so it stays covered.

use std::collections::BTreeSet;

use agentd::flight_recorder::EventKind;

/// Every event-kind string agentctl PRODUCTION code matches on, with its site.
/// (Test-module fixtures such as inspector.rs's `"kind":"some_event"` are excluded
/// on purpose — they exercise the matcher, they are not contract with agentd.)
const AGENTCTL_KIND_MATCHES: &[&str] = &[
    // src/watch/inspector.rs — InspectorFilter::matches() + is_error_event() (lines ~47, ~62)
    // The Errors filter (par.1-ar-01) matches the real error kinds via `is_error_event`:
    "agent_failed",              // Errors filter (is_error_event)
    "error",                     // Errors filter (is_error_event)
    "mcp_http_error",            // Errors filter (is_error_event)
    "fuse_control_error",        // Errors filter (is_error_event)
    "egress_proxy_failed",       // Errors filter (is_error_event)
    "credential_refresh_failed", // Errors filter (is_error_event)
    "tool_result",               // Errors filter (is_error_event, gated on data.is_error:true)
    "sandbox_applied",           // Sandbox filter
    "sandbox_skipped",           // Sandbox filter
    "capability_denied",         // CapDenied filter
    "egress_brokered",           // Egress filter
    "egress_denied",             // Egress filter
    "action_receipt_emitted",    // Egress filter
    // src/watch/views.rs — inspector colour-coding (lines ~1229) shares `is_error_event`
    // (same error subset as above) + sandbox_* / capability_denied.
    // src/orchestrate.rs — SSE turn loop (lines ~164-191)
    "inference_stream_delta",
    "orchestrator_turn_complete",
    "agent_completed",
    "orchestrator_exited",
    // src/watch/converse.rs — chat rail SSE loop (lines ~289-596)
    "orchestrator_injected",
    "budget_exceeded",
    // src/watch/topology.rs — message-graph edges (lines ~312-328)
    "message_sent",
];

/// Strings agentctl matches on that are NOT emitted by agentd under any `EventKind`.
/// Documented here so the guard stays green while such a drift is TRACKED, not hidden.
///
/// EMPTY as of par.1-ar-01: the two former entries — `tool_error`, `inference_error` —
/// were the Inspector "Errors" filter (`inspector.rs`) and red colour rule (`views.rs`)
/// matching kinds agentd never emits, so the operator's error view was blind to every
/// tool + inference failure. Both sites now share `inspector::is_error_event`, which
/// matches the REAL error kinds (`tool_result`+`data.is_error`, `error`, `agent_failed`,
/// `mcp_http_error`, `fuse_control_error`, `egress_proxy_failed`, `credential_refresh_failed`),
/// all listed in `AGENTCTL_KIND_MATCHES` above. The bug is fixed; nothing is non-canonical now.
///
/// Invariant kept by `known_noncanonical_entries_are_actually_absent`: an entry may live
/// here ONLY while it is genuinely not a real kind AND agentctl still matches it. Add one
/// only when agentctl deliberately matches a string agentd doesn't emit, with the reason.
const KNOWN_NONCANONICAL: &[&str] = &[];

fn valid_kind_strings() -> BTreeSet<&'static str> {
    EventKind::ALL.iter().map(|k| k.as_str()).collect()
}

#[test]
fn every_agentctl_kind_match_is_a_real_event_kind() {
    let valid = valid_kind_strings();
    let allowed: BTreeSet<&str> = KNOWN_NONCANONICAL.iter().copied().collect();

    let mut orphans = Vec::new();
    for s in AGENTCTL_KIND_MATCHES {
        if !valid.contains(s) && !allowed.contains(s) {
            orphans.push(*s);
        }
    }
    assert!(
        orphans.is_empty(),
        "agentctl matches event-kind string(s) that no EventKind serializes to: {orphans:?}.\n\
         A variant was renamed/removed and a TUI filter/colour/loop branch is now dead.\n\
         Either restore the variant, update the agentctl match site to the new \
         `EventKind::as_str()` value, or (if intentionally non-canonical) document it \
         in KNOWN_NONCANONICAL with the reason."
    );
}

#[test]
fn known_noncanonical_entries_are_actually_absent() {
    // Self-cleaning allowlist: if a "known dead" string becomes a real kind (variant
    // added, or the matcher fixed to emit the real name), it MUST be removed from
    // KNOWN_NONCANONICAL. This fails loudly when that day comes so the allowlist can't rot.
    let valid = valid_kind_strings();
    let now_real: Vec<_> = KNOWN_NONCANONICAL
        .iter()
        .copied()
        .filter(|s| valid.contains(s))
        .collect();
    assert!(
        now_real.is_empty(),
        "these strings are now real EventKind values and must be REMOVED from \
         KNOWN_NONCANONICAL (the drift they documented is resolved): {now_real:?}"
    );
    // And they must actually be referenced by agentctl — a stale allowlist entry that
    // agentctl no longer matches is also drift to clean up.
    for s in KNOWN_NONCANONICAL {
        assert!(
            AGENTCTL_KIND_MATCHES.contains(s),
            "KNOWN_NONCANONICAL lists {s:?} but agentctl no longer matches it; remove it"
        );
    }
}
