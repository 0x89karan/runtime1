//! Drift guard: every flight-recorder event kind must be documented in the
//! CONVENTIONS.md event taxonomy.
//!
//! `docs/CONVENTIONS.md` is the canonical operator/contributor reference for the
//! event taxonomy ("Record everything" — new behavior gets a new event kind AND a
//! table row). Nothing enforced that the doc kept pace with the code, so kinds
//! shipped without a row (the AUDIT-v0.86 doc-drift class, e.g. `mcp_tool_called`
//! documented under a name the code never emits). This test closes that: it asserts
//! that EVERY `EventKind::as_str()` value (via the exhaustive `EventKind::ALL` slice)
//! appears — backtick-wrapped — somewhere in CONVENTIONS.md.
//!
//! Negative control (how to trust this test): temporarily delete a kind's ROW from
//! the "Flight-recorder event taxonomy" table and re-run — this test must go RED
//! naming that kind, EVEN if the kind is still mentioned in prose elsewhere in the
//! doc. Restore the row to return to green. (Verified when this test was authored.)
//!
//! Scoping matters: an earlier version of this test accepted the kind appearing
//! backtick-wrapped ANYWHERE in the doc, which false-passed when a kind was dropped
//! from the table but survived in prose — i.e. it did not actually guard the table.
//! This version searches only TABLE ROWS within the taxonomy SECTION.

use agentd::events::EventKind;
use std::path::Path;

/// Header (level-2) that opens the event-taxonomy section, and the next level-2
/// header that closes it. The check runs only on table rows between the two.
const SECTION_HEADER: &str = "## Flight-recorder event taxonomy";

#[test]
fn every_event_kind_is_documented_in_the_taxonomy_table() {
    // CARGO_MANIFEST_DIR = <repo>/agentd; the doc lives at <repo>/docs/CONVENTIONS.md.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let conventions_path = manifest_dir.join("../docs/CONVENTIONS.md");
    let conventions = std::fs::read_to_string(&conventions_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", conventions_path.display()));

    // Isolate the taxonomy section: from its header to the next level-2 header.
    let after_header = conventions
        .split_once(SECTION_HEADER)
        .unwrap_or_else(|| panic!("CONVENTIONS.md missing section header {SECTION_HEADER:?}"))
        .1;
    let section = match after_header.find("\n## ") {
        Some(end) => &after_header[..end],
        None => after_header,
    };

    // Collect the backticked tokens that appear in TABLE ROWS only (lines whose
    // first non-space char is `|`). A kind mentioned only in the section's prose,
    // or in some other section, does not count — this is what makes the guard bite
    // on table drift specifically.
    let table_rows: String = section
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut missing: Vec<&'static str> = EventKind::ALL
        .iter()
        .map(|k| k.as_str())
        .filter(|kind| !table_rows.contains(&format!("`{kind}`")))
        .collect();
    missing.sort_unstable();

    assert!(
        missing.is_empty(),
        "event kinds emitted by the code but absent from the CONVENTIONS.md event \
         taxonomy TABLE (add a table row for each, under {SECTION_HEADER:?}): {missing:?}",
    );
}
