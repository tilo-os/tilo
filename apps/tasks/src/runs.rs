//! The task list: every task the tracker holds a run for, one row each.
//!
//! The tracker is the workspace's record, synced to every device that has
//! the room, so a task a person's other device runs is a row here as much as
//! one this device runs. A row says the step its run is in — the same chip
//! the issue and the board show — where the run works once its agent has
//! said so, and what the task spent of what it may cost. What the task was
//! broken into follows it, indented under their parent, so the work a run
//! split a task into is read under it.
//!
//! A task that waits for its person's answer reads first: nothing else can
//! move it. The ones going follow, then the ones that ended, which stay in
//! the list so a person finds there what they may take back. Pressing a row
//! or its chip opens the issue, the one task view with the chat and the
//! steps, where a run is paused, let on, stopped or its end taken back: the
//! state read here and the state acted on there never disagree.

use std::cmp::Reverse;

use ui::prelude::*;
use ui::widget::ListRow;

use crate::model::{Issue, Tracker};
use crate::{Filter, Nav, task_chip};

/// One row of the list: an issue, the run it carries when it carries one,
/// and how its own sub-tasks stand.
struct Row {
    id: Id,
    /// Its run, read once so the chip is built from the same run the row
    /// describes.
    run: Option<crate::model::Run>,
    number: u64,
    title: String,
    /// What its runs spent of what it may cost.
    cost: Option<String>,
    /// The run's device, as its agent reported it.
    device: Option<String>,
    /// Its closed and total direct sub-issues, counted, when it has any.
    progress: Option<(usize, usize)>,
}

impl Row {
    fn of(id: &Id, issue: &Issue, tracker: &Tracker) -> Self {
        let run = issue.run.clone();
        Self {
            id: id.clone(),
            cost: issue.cost_budget.map(|budget| {
                let spent = issue.spend.unwrap_or_default();
                crate::tr_with(
                    "tasks-fact-cost",
                    &[
                        ("spent", &spent.to_string()),
                        ("budget", &budget.to_string()),
                    ],
                )
            }),
            device: run
                .as_ref()
                .and_then(|run| run.device.clone())
                .map(|device| crate::tr_with("tasks-run-list-device", &[("device", &device)])),
            run,
            number: issue.number,
            title: issue.title.clone(),
            progress: Some(tracker.progress(id)).filter(|(_, total)| *total > 0),
        }
    }

    /// What the row says under its name: where the run works and what it
    /// spent, each its own line.
    fn secondary(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(device) = &self.device {
            lines.push(device.clone());
        }
        if let Some(cost) = &self.cost {
            lines.push(cost.clone());
        }
        if let Some((closed, total)) = self.progress {
            lines.push(crate::tr_with(
                "tasks-run-list-sub-tasks",
                &[
                    ("closed", &closed.to_string()),
                    ("total", &total.to_string()),
                ],
            ));
        }
        lines.join("\n")
    }
}

/// The tasks of the list and what they were broken into: `task` and its own
/// sub-tasks, deepest last. A task another row already holds as itself — a
/// sub-task that carries a run of its own — is not listed a second time
/// under its parent.
fn collect(tracker: &Tracker, id: &Id, listed: &mut Vec<Row>) {
    if listed.iter().any(|row| row.id == *id) {
        return;
    }
    let Some(issue) = tracker.issues.get(id) else {
        return;
    };
    listed.push(Row::of(id, issue, tracker));
    for (child, _) in tracker.children(Some(id)) {
        collect(tracker, child, listed);
    }
}

/// Every task the tracker holds a run for, the ones waiting for the person
/// first, then the ones going, then the ones that ended, the newest asked
/// first among equals.
fn rows(tracker: &Tracker, filter: &Filter, me: Option<&str>) -> Vec<Row> {
    let mut tasks: Vec<(u8, Reverse<u64>, Id)> = Vec::new();
    for (id, issue) in tracker.issues.iter() {
        let Some(run) = issue.run.as_ref() else {
            continue;
        };
        if !filter.matches(issue, me) {
            continue;
        }
        let order = if run.asks_its_person() {
            0
        } else if run.phase.active() {
            1
        } else {
            2
        };
        tasks.push((order, Reverse(issue.number), id.clone()));
    }
    tasks.sort();
    let mut listed = Vec::new();
    for (_, _, id) in tasks {
        collect(tracker, &id, &mut listed);
    }
    listed
}

pub(crate) fn view(cx: &ViewCx<Tracker>, nav: &Nav) -> Element {
    let chosen = nav.filter.clone();
    let me = cx.me().map(str::to_owned);
    let nav = nav.clone();
    let list = cx.switch(&chosen, move |cx, chosen| {
        let filter = chosen.as_deref().and_then(Filter::of).unwrap_or_default();
        let tracker = cx.value(Tracker::clone);
        let nav = nav.clone();
        let me = me.clone();
        cx.switch(&tracker, move |cx, tracker| {
            list_of(cx, tracker, &nav, &filter, me.as_deref())
        })
    });
    // The window bar names the view, as it does for the list and the board.
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(list),
    )
}

fn list_of(
    cx: &ViewCx<Tracker>,
    tracker: &Tracker,
    nav: &Nav,
    filter: &Filter,
    me: Option<&str>,
) -> Element {
    let rows = rows(tracker, filter, me);
    if rows.is_empty() {
        return Box::new(
            EmptyState::new("tasks-run-list-empty-title")
                .with_message("tasks-run-list-empty-message")
                .with_id("tasks-run-list-empty"),
        );
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight);
    for row in &rows {
        column = column.push(row_of(cx, nav, row));
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(column)),
    )
}

/// One task's row: the task, where it runs and what it spent, and the state
/// of its run as the chip every view shows.
fn row_of(cx: &ViewCx<Tracker>, nav: &Nav, row: &Row) -> Element {
    let open = nav.clone();
    let issue = row.id.clone();
    let mut list_row = ListRow::new(format!("tasks-run-{}", row.id), row.title.clone())
        .subtitle(crate::reference(row.number))
        .icon(ui::icons::Icon::Hourglass)
        .secondary(row.secondary())
        .on_view(move || open.open(&issue));
    if let Some(run) = &row.run {
        list_row = list_row.trailing(Box::new(task_chip::chip(cx, &row.id, run)));
    }
    Box::new(list_row)
}
