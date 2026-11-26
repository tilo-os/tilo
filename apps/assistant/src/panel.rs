//! The panel beside the conversation: the assistant's tasks by where they stand, the work it
//! does at a time and the events it reacts to. A task is an issue in Tasks, never a
//! sub-agent. The workspace host runs the events the person sets up with their assistant.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_reference::ResourceRef;
use ui::widget::{ListRow, Section, SectionHeader, Segment, ViewTabs};

use crate::sources::{Read, Stage, Task};
use crate::{AssistantEffect, Hands, Nav, Window, page, tr};

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
    let hands = hands.clone();
    let shown = nav.tab.clone();
    cx.switch(&nav.tab, move |cx, tab| {
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
            Tab::Active | Tab::Resolved => match hands.reads.tasks() {
                Read::NotHere => open_tasks(&hands),
                Read::Here(tasks) if *tab == Tab::Active => active(&tasks),
                Read::Here(tasks) => resolved(&tasks),
            },
            Tab::Scheduled => progress_work(cx, &hands),
        });
        page(parts)
    })
}

/// Open the tracker itself through Tasks' published resource, in this workspace.
fn open_tasks(hands: &Hands) -> Vec<Element> {
    let dispatcher = hands.dispatcher.clone();
    let tracker = ResourceRef::whole("tasks.tracker", "tasks.tracker", tr("assistant-tasks"));
    vec![
        Box::new(Text::new(&tr("assistant-work-in-tasks"))),
        Box::new(
            Button::new("assistant-open-tasks", "assistant-open-tasks")
                .opening_workspace_app(ui::apps::TASKS)
                .with_icon(Icon::ListChecks)
                .on_view(move || dispatcher.open_reference(ui::apps::TASKS, None, &tracker)),
        ),
    ]
}

/// The one sentence a part of the panel says in place of its rows: `key`, named `key` for
/// whoever drives the window.
fn empty_tasks(key: &str) -> Vec<Element> {
    vec![Box::new(
        EmptyState::new(tr(key))
            .with_id(key)
            .with_icon(Icon::ListChecks),
    )]
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
fn active(tasks: &[Task]) -> Vec<Element> {
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
        let rows = rows(tasks, stage, icon);
        if !rows.is_empty() {
            let count = rows.len();
            parts.push(Box::new(
                Section::new(rows).with_header(SectionHeader::group(tr(title)).count(count)),
            ));
        }
    }
    if parts.is_empty() {
        return empty_tasks("assistant-tasks-none-active");
    }
    parts
}

/// The tasks that are done with.
fn resolved(tasks: &[Task]) -> Vec<Element> {
    let rows = rows(tasks, Stage::Resolved, Icon::CircleCheck);
    if rows.is_empty() {
        empty_tasks("assistant-tasks-none-resolved")
    } else {
        vec![Box::new(Section::new(rows))]
    }
}

/// What the assistant does at a time, and what it does when something happens.
fn progress_work(cx: &ViewCx<Window>, hands: &Hands) -> Vec<Element> {
    let reads = &hands.reads;
    let (Read::Here(scheduled), Read::Here(events)) = (reads.scheduled(), reads.events()) else {
        return match reads.assistant() {
            Read::Here(Some(assistant)) => {
                let conversation = crate::agent_reference(&assistant.id, &assistant.name);
                let open = cx.actions().bind_effect_invocation(
                    Button::new("assistant-manage-events", "assistant-open-in-chat")
                        .with_icon(Icon::MessageCircle)
                        .opening_workspace_item(ui::apps::CHAT, &conversation),
                    &AssistantEffect::ViewMainChat,
                );
                vec![
                    Box::new(Text::new(&tr("assistant-events-in-chat"))),
                    Box::new(open),
                ]
            }
            Read::Here(None) => {
                let open = cx.actions().bind_effect_invocation(
                    Button::new("assistant-manage-events", "assistant-overview"),
                    &AssistantEffect::ViewSupervision,
                );
                vec![
                    Box::new(Text::new(&tr("assistant-events-need-assistant"))),
                    Box::new(open),
                ]
            }
            Read::NotHere => vec![crate::agents_not_here()],
        };
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
