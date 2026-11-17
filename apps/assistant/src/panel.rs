//! The panel beside the conversation: the assistant's tasks by where they stand, the work it
//! does at a time and the events it reacts to. A task is an issue in Tasks, never a
//! sub-agent; scheduled work and events are Shortcuts the host runs.

use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ListRow, Section, SectionHeader, Segment, ViewTabs};

use crate::sources::{Read, Reads, Stage, Task};
use crate::{Hands, Nav, Window, page, tr};

/// What the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    /// The tasks that are not resolved, by where they stand.
    Active,
    Scheduled,
    Resolved,
}

impl Tab {
    const ALL: [Self; 3] = [Self::Active, Self::Scheduled, Self::Resolved];

    const fn label(self) -> &'static str {
        match self {
            Self::Active => "assistant-tab-active",
            Self::Scheduled => "assistant-tab-scheduled",
            Self::Resolved => "assistant-tab-resolved",
        }
    }
}

pub(crate) fn view(cx: &ViewCx<Window>, nav: &Nav, hands: &Hands) -> Element {
    let reads = hands.reads.clone();
    let shown = nav.tab.clone();
    cx.switch(&nav.tab, move |_cx, tab| {
        let tabs = ViewTabs::from_state(
            Tab::ALL
                .into_iter()
                .map(|tab| (tab, Segment::new(tr(tab.label()))))
                .collect(),
            &shown,
        )
        .with_id("assistant-panel-tabs")
        .with_accessible_name(tr("assistant-panel-views"));
        let mut parts: Vec<Element> = vec![Box::new(tabs)];
        parts.extend(match tab {
            Tab::Active => active(&reads),
            Tab::Scheduled => standing_work(&reads),
            Tab::Resolved => resolved(&reads),
        });
        page(parts)
    })
}

/// The one sentence a part of the panel says in place of its rows: `key`, named `key` for
/// whoever drives the window.
fn sentence(key: &str, icon: Option<Icon>) -> Vec<Element> {
    let state = EmptyState::new(tr(key)).with_id(key);
    vec![Box::new(match icon {
        Some(icon) => state.with_icon(icon),
        None => state,
    })]
}

fn rows(tasks: &[Task], stage: Stage, icon: Icon) -> Vec<Element> {
    tasks
        .iter()
        .filter(|task| task.stage == stage)
        .map(|task| {
            let mut row =
                ListRow::new(format!("assistant-task-{}", task.id), task.title.clone()).icon(icon);
            if let Some(note) = &task.note {
                row = row.secondary(note.clone());
            }
            Box::new(row) as Element
        })
        .collect()
}

/// The tasks that want the person, are being worked on or come next, by where they stand.
fn active(reads: &Reads) -> Vec<Element> {
    let tasks = match reads.tasks() {
        Read::Here(tasks) => tasks,
        Read::NotHere => return sentence("assistant-tasks-not-here", None),
    };
    let mut parts: Vec<Element> = Vec::new();
    for (stage, title, icon) in [
        (
            Stage::ReadyForReview,
            "assistant-ready-for-review",
            Icon::CircleCheck,
        ),
        (
            Stage::NeedsInput,
            "assistant-needs-input",
            Icon::MessageCircle,
        ),
        (
            Stage::InProgress,
            "assistant-in-progress",
            Icon::CircleDashed,
        ),
        (Stage::UpNext, "assistant-up-next", Icon::Clock),
    ] {
        let rows = rows(&tasks, stage, icon);
        if !rows.is_empty() {
            let count = rows.len();
            parts.push(Box::new(
                Section::new(rows).with_header(SectionHeader::group(tr(title)).count(count)),
            ));
        }
    }
    if parts.is_empty() {
        return sentence("assistant-tasks-none-active", Some(Icon::ListChecks));
    }
    parts
}

/// The tasks that are done with.
fn resolved(reads: &Reads) -> Vec<Element> {
    match reads.tasks() {
        Read::NotHere => sentence("assistant-tasks-not-here", None),
        Read::Here(tasks) => {
            let rows = rows(&tasks, Stage::Resolved, Icon::CircleCheck);
            if rows.is_empty() {
                sentence("assistant-tasks-none-resolved", Some(Icon::ListChecks))
            } else {
                vec![Box::new(Section::new(rows))]
            }
        }
    }
}

/// What the assistant does at a time, and what it does when something happens.
fn standing_work(reads: &Reads) -> Vec<Element> {
    let (Read::Here(scheduled), Read::Here(events)) = (reads.scheduled(), reads.events()) else {
        return sentence("assistant-scheduled-not-here", None);
    };
    let state = |on: bool| {
        tr(if on {
            "assistant-on"
        } else {
            "assistant-paused"
        })
    };
    let scheduled = scheduled
        .iter()
        .map(|work| {
            Box::new(
                ListRow::new(
                    format!("assistant-scheduled-{}", work.id),
                    work.name.clone(),
                )
                .icon(Icon::Clock)
                .secondary(work.when.clone())
                .value(state(work.on)),
            ) as Element
        })
        .collect();
    let events = events
        .iter()
        .map(|event| {
            Box::new(
                ListRow::new(format!("assistant-event-{}", event.id), event.name.clone())
                    .icon(Icon::Bell)
                    .secondary(event.does.clone())
                    .value(state(event.on)),
            ) as Element
        })
        .collect();
    vec![
        section("assistant-scheduled", scheduled, "assistant-scheduled-none"),
        section("assistant-events", events, "assistant-events-none"),
    ]
}

/// A titled part of the panel: its rows under their count, or the sentence `none`.
fn section(title: &str, rows: Vec<Element>, none: &str) -> Element {
    let count = rows.len();
    let rows = if rows.is_empty() {
        vec![Box::new(Text::new(&tr(none)).dimmed().with_id(none)) as Element]
    } else {
        rows
    };
    Box::new(Section::new(rows).with_header(SectionHeader::group(tr(title)).count(count)))
}
