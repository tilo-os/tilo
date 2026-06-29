//! `activity_timeline` example — eyeball the Phase 1 audit-pane and
//! history-scrubber widgets together with a seeded sample feed.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui --example activity_timeline
//! ```
//!
//! Notes:
//! - The example uses synthetic entries — no broker, no collab
//!   socket. The widget contract is "give me a `Vec<HistoryEntry>`",
//!   and that's what this program does.
//! - The scrubber + the timeline are stacked vertically in a
//!   `Column`. The scrubber is reusable across artifact kinds; the
//!   timeline is the per-channel activity pane.

use std::time::{Duration, SystemTime};

use ui::widget::{ActivityTimeline, Column, HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryScrubber};
use ui::{App, State};
use qos_collab_types::{ArtifactKind, OpClass};

fn entry(id: u8, author: &str, hue: f32, op: OpClass, summary: &str, secs_ago: u64) -> HistoryEntry {
    let mut bytes = [0u8; 16];
    bytes[0] = id;
    HistoryEntry {
        id: HistoryEntryId(bytes),
        author: HistoryAuthor::new(author, hue, format!("{:08x}", id as u32 * 0xdead_beef)),
        op_class: op,
        artifact_kind: ArtifactKind::Note,
        timestamp: SystemTime::now() - Duration::from_secs(secs_ago),
        summary: summary.into(),
        diff_payload: None,
    }
}

fn main() {
    // Build a sample audit-pane feed roughly mimicking the shape the
    // shell will produce in Phase 1: a mix of authors, op classes,
    // and ages. Order doesn't matter — the timeline sorts.
    let entries = vec![
        entry(1, "Alice", 200.0, OpClass::Splice, "+24 -3 chars", 30),
        entry(2, "Bob", 30.0, OpClass::Comment, "added a reply", 5 * 60),
        entry(3, "Alice", 200.0, OpClass::Move, "Moved to Done", 30 * 60),
        entry(4, "Carol", 280.0, OpClass::EditMeta, "renamed page", 2 * 3600),
        entry(5, "Dave", 100.0, OpClass::Tombstone, "Tombstoned card", 3 * 3600),
        entry(6, "Owner", 0.0, OpClass::Govern, "issued attestation", 24 * 3600),
        entry(7, "Alice", 200.0, OpClass::Splice, "+512 chars", 2 * 24 * 3600),
        entry(8, "Bob", 30.0, OpClass::Restore, "Restored card", 3 * 24 * 3600),
    ];

    let state = State::new();

    let timeline = ActivityTimeline::with_entries(entries.clone()).on_revert(|id| {
        eprintln!("revert clicked: {:?}", id);
    });
    let scrubber = HistoryScrubber::new(entries)
        .on_preview(|id| eprintln!("preview tick: {:?}", id))
        .on_restore(|id| eprintln!("restore tick: {:?}", id));

    let root_widget = Box::new(
        Column::new()
            .push(Box::new(scrubber))
            .push(Box::new(timeline)),
    );

    let app = App::new(state, root_widget);
    app.run();
}
