//! Interactive `ConflictBanner` and `ConflictPicker` demo with mocked
//! `PickOne` alternatives.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui-examples --example ui-lab -- conflict_picker
//! ```
//!
//! Notes:
//! - This is a stand-alone demo. There is no real CRDT under it — we
//!   hand the picker a `ConflictKind::PickOne` with two `Alternative`
//!   instances and observe the `PickerOutcome` callbacks in stderr.
//! - The banner shows "Kanban move: 2 conflicts". Click
//!   `[Review]` to see the picker pick path; click `[Dismiss]` to fire
//!   the dismiss callback.
//! - The picker emits one of the four `PickerOutcome` variants
//!   (Picked, `KeepCurrent`, Cancel — `PickedMerged` needs a
//!   `MergeOrPick` kind, which the kanban scenario doesn't produce).

use std::sync::Arc;
use std::time::Duration;

use tilo_ui::widget::{Alternative, Column, ConflictBanner, ConflictKind, ConflictPicker};
use tilo_ui::{App, State};

fn alt(label: &str, summary: &str, author: &str, secs_ago: u64) -> Alternative {
    Alternative {
        label: label.into(),
        summary: summary.into(),
        author: author.into(),
        timestamp: tilo_ui::time::wall()
            .checked_sub(Duration::from_secs(secs_ago))
            .unwrap_or_else(tilo_ui::time::wall),
        payload: Arc::from(Vec::<u8>::new()),
    }
}

pub fn run(_args: &[String]) {
    let wake_scope = tilo_ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
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

    let app = App::from_scoped_parts(wake_scope, state, root_widget);
    app.run();
}
