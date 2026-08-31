//! `conflict_picker` example — eyeball the Phase 4 conflict toolkit:
//! a `ConflictBanner` stacked above a `ConflictPicker` with mocked
//! `PickOne` alternatives.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui --example ui-lab -- conflict_picker
//! ```
//!
//! Notes:
//! - This is a stand-alone demo. There is no real CRDT under it — we
//!   hand the picker a `ConflictKind::PickOne` with two `Alternative`
//!   instances and observe the `PickerOutcome` callbacks in stderr.
//! - The banner shows "Kanban move — 2 conflicts in this doc". Click
//!   `[Review]` to see the picker pick path; click `[Dismiss]` to fire
//!   the dismiss callback.
//! - The picker emits one of the four `PickerOutcome` variants
//!   (Picked, `KeepCurrent`, Cancel — `PickedMerged` needs a
//!   `MergeOrPick` kind, which the kanban scenario doesn't produce).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ui::widget::{Alternative, Column, ConflictBanner, ConflictKind, ConflictPicker};
use ui::{App, State};

fn alt(label: &str, summary: &str, author: &str, secs_ago: u64) -> Alternative {
    Alternative {
        label: label.into(),
        summary: summary.into(),
        author: author.into(),
        timestamp: SystemTime::now()
            .checked_sub(Duration::from_secs(secs_ago))
            .unwrap_or_else(SystemTime::now),
        payload: Arc::from(Vec::<u8>::new()),
    }
}

pub fn run(_args: &[String]) {
    // Two writers concurrently moved the same kanban card to
    // different columns. Wrap that as a `PickOne` kind so the
    // picker shows them side-by-side.
    let kind = ConflictKind::PickOne {
        alternatives: vec![
            alt(
                "Move to Done",
                "Alice marked the card finished after testing",
                "Alice",
                2 * 60,
            ),
            alt(
                "Move to In Review",
                "Bob wants Carol to approve before merging",
                "Bob",
                60,
            ),
        ],
    };

    let mut banner = ConflictBanner::new("Kanban move")
        .on_review(|| eprintln!("[banner] review clicked"))
        .on_dismiss(|| eprintln!("[banner] dismiss clicked"));
    banner.set_conflict_count(2);

    let picker = ConflictPicker::new("Kanban move", kind)
        .on_pick(|outcome| eprintln!("[picker] outcome: {outcome:?}"));

    let state = State::new();
    let root_widget = Box::new(Column::new().push(Box::new(banner)).push(Box::new(picker)));

    let app = App::new(state, root_widget);
    app.run();
}
