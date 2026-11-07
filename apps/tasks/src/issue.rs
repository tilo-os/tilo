//! An issue's page: its title, status and assignee, its description in the
//! block editor, its sub-issues, what it links to and its thread.
//!
//! Each part that shows the issue's state is built again only when that
//! part changes, so typing in the title or the description never rebuilds
//! the editor under the caret. The page draws no Back of its own: the
//! window bar's Back (and a narrow window's) closes it, and the bar's
//! location names it.

use crate::model::{
    Issue, Message, MessageKind, Status, Tracker, agent_assignee, issue_resource, person_assignee,
    tracker,
};
use tilo_rich_text::RichTextFieldViews;
use ui::prelude::*;
use ui::presence::ParticipantKind;
use ui::tilo_resource::NonBlank;
use ui::widget::{Badge, ChipStyle, Dropdown, Fact, Facts, Tone};

use crate::{Nav, reference, tr, tr_with};

pub(crate) fn page(cx: &ViewCx<Tracker>, id: Id, nav: &Nav) -> Element {
    let exists = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.contains(&id)
    });
    let nav = nav.clone();
    cx.switch(&exists, move |cx, exists| {
        if *exists {
            content(cx, &id, &nav)
        } else {
            Box::new(
                Column::new()
                    .with_rhythm(ui::tokens::Rhythm::Group)
                    .push(Box::new(
                        EmptyState::new("tasks-issue-gone-title")
                            .with_message("tasks-issue-gone-message")
                            .with_id("tasks-issue-gone"),
                    )),
            )
        }
    })
}

fn content(cx: &ViewCx<Tracker>, id: &Id, nav: &Nav) -> Element {
    let paths = Tracker::paths().issues.item(id);
    let number = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.get(&id).map_or(0, |issue| issue.number)
    });
    let header = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(
            Text::from_state(&number, |number| reference(*number))
                .with_role(TextRole::Title)
                .dimmed(),
        ))
        .push(Box::new(
            Container::new()
                .with_flex_grow()
                .with_no_padding()
                .with_child(Box::new(
                    cx.input(paths.title.clone())
                        .with_id("tasks-issue-title")
                        .with_accessible_name(tr("tasks-field-title"))
                        .with_plain_chrome()
                        .with_text_role(TextRole::Title),
                )),
        ));

    let body_id = format!("{}/body", issue_resource(id));
    let body_path = paths.body.clone();
    let locate_path = paths.body;
    let body = cx.rich_text_field(
        &body_id,
        move |doc| locate_path.object(doc),
        move |op| body_edit(&body_path, &op),
    );

    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Container::new()
                    .readable()
                    .centered()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .with_child(Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_rhythm(ui::tokens::Rhythm::Group)
                            .push(Box::new(header))
                            .push(facts(cx, id))
                            .push(labels(cx, id))
                            .push(Box::new(
                                Text::new("tasks-issue-description").with_role(TextRole::Heading),
                            ))
                            .push(body)
                            .push(sub_issues(cx, id, nav))
                            .push(links(cx, id))
                            .push(thread(cx, id)),
                    )),
            )),
    )
}

/// The edit of the description that `op` is: a field edit of the tracker.
#[allow(
    clippy::expect_used,
    reason = "a rich-text op is plain data that always serializes, and every field of the tracker offers its edits"
)]
fn body_edit(
    path: &ui::tilo_resource::Path<Tracker, tilo_rich_text::RichText>,
    op: &tilo_rich_text::RichTextAction,
) -> <Tracker as ui::tilo_resource::Resource>::Action {
    Tracker::edit_action(path.perform(op).expect("a rich-text op serializes"))
        .expect("the tracker offers field edits")
}

/// The status and the assignee, each a control on the issue's own action.
fn facts(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let labels: Vec<String> = Status::ALL
        .iter()
        .map(|status| tr(status.label_key()))
        .collect();
    let choices = labels.clone();
    let place_id = id.clone();
    let read_id = id.clone();
    let status = cx.control_with(
        |state| {
            Dropdown::from_state(labels, state)
                .with_id("tasks-issue-status")
                .with_accessible_name(tr("tasks-field-status"))
        },
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&read_id)
                .map(|issue| tr(issue.status.label_key()))
                .unwrap_or_default()
        },
        Tracker::PLACE.map(move |label: String| tracker::Place {
            issue: place_id.clone(),
            status: choices
                .iter()
                .position(|choice| *choice == label)
                .and_then(|at| Status::ALL.get(at).copied())
                .unwrap_or_default(),
            before: None,
        }),
    );
    let assign_id = id.clone();
    let read_id = id.clone();
    let assignee = cx.member_picker(
        vec![(String::new(), tr("tasks-assignee-nobody"))],
        |member| match member.kind {
            ParticipantKind::Person => Some(person_assignee(&member.id).0),
            ParticipantKind::Agent => Some(agent_assignee(&member.id).0),
            _ => None,
        },
        |picker| {
            Box::new(labeled_field(
                "tasks-field-assignee",
                picker
                    .with_id("tasks-issue-assignee")
                    .with_accessible_name(tr("tasks-field-assignee")),
            ))
        },
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&read_id)
                .and_then(|issue| issue.assignee.as_ref().map(|assignee| assignee.0.clone()))
                .unwrap_or_default()
        },
        Tracker::ASSIGN.map(move |assignee: String| tracker::Assign {
            issue: assign_id.clone(),
            assignee: Some(assignee),
        }),
    );
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(labeled_field("tasks-field-status", status)))
                    .push(assignee),
            ))
            .push(plan(cx, id)),
    )
}

/// What the issue works towards, how urgent it is and when it is wanted:
/// each a fact a person reads, never a control.
fn plan(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let planned = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.get(&id).map(Plan::of).unwrap_or_default()
    });
    cx.switch(&planned, |_cx, planned| {
        let mut facts = Vec::new();
        if let Some(label) = &planned.goal {
            facts.push(Fact::text(tr_with("tasks-fact-goal", &[("goal", label)])));
        }
        if let Some(priority) = planned.priority {
            facts.push(Fact::text(tr_with(
                "tasks-fact-priority",
                &[("priority", &priority.to_string())],
            )));
        }
        if let Some(due) = &planned.due {
            facts.push(Fact::text(tr_with("tasks-fact-due", &[("date", due)])));
        }
        if facts.is_empty() {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element;
        }
        Box::new(Facts::new(facts)) as Element
    })
}

/// How a issue is planned, as its page reads it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Plan {
    /// How its goal reads: the resource it works towards.
    goal: Option<String>,
    priority: Option<u8>,
    /// When it is wanted, as a person reads a date.
    due: Option<String>,
}

impl Plan {
    fn of(issue: &Issue) -> Self {
        Self {
            goal: issue.area.as_ref().map(|area| area.label.clone()),
            priority: issue.priority,
            due: issue
                .due_ms
                .map(|due_ms| ui::format::date_of(&ui::format::UnixSecs(due_ms / 1_000))),
        }
    }
}

/// The issue's labels, and a field to add one.
fn labels(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let current = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .map(|issue| issue.labels.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        }
    });
    let shown = cx.switch(&current, |_, labels| {
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        for label in labels {
            row = row.push(Box::new(Badge::text(label.clone())));
        }
        Box::new(row) as Element
    });
    let draft = cx.local(String::new());
    let label_id = id.clone();
    let label = Tracker::ADD_LABEL.try_map(move |label: String| {
        Some(tracker::AddLabel {
            issue: label_id.clone(),
            label: NonBlank::parse(&label)?,
        })
    });
    let input = cx.actions().submit_draft(
        TextInput::from_state(&draft)
            .with_id("tasks-issue-label")
            .with_accessible_name(tr("tasks-issue-add-label"))
            .with_placeholder("tasks-issue-label-placeholder"),
        &draft,
        label.clone(),
    );
    let add = cx.actions().bind_draft(
        Button::new("tasks-issue-add-label", "tasks-issue-add-label"),
        &draft,
        label,
    );
    Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(shown)
            .push(Box::new(input))
            .push(Box::new(add)),
    )
}

/// The issue's sub-issues, each opening its own page, and a field to add
/// one.
fn sub_issues(cx: &ViewCx<Tracker>, id: &Id, nav: &Nav) -> Element {
    let children = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .children(Some(&id))
                .map(|(child, issue)| {
                    (
                        child.clone(),
                        issue.number,
                        issue.title.clone(),
                        issue.status,
                    )
                })
                .collect::<Vec<_>>()
        }
    });
    let nav = nav.clone();
    let list = cx.switch(&children, move |_, children| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight);
        for (child, number, title, status) in children {
            let open = nav.clone();
            let child = child.clone();
            let label = format!("{} {}", reference(*number), title);
            column = column.push(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(
                        Button::new(&format!("tasks-sub-issue-{child}"), &label)
                            .on_view(move || open.open(&child)),
                    ))
                    .push(Box::new(status_badge(*status))),
            ));
        }
        Box::new(column) as Element
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-sub-issues").with_role(TextRole::Heading),
            ))
            .push(list)
            .push(crate::new_issue(
                cx,
                Some(id.clone()),
                None,
                "tasks-sub-issue-new",
            )),
    )
}

/// What the issue links to, each a reference chip that opens it in the app
/// that keeps it; the way to write a note for it in Notes, which links the
/// note here; and the way to link a note already written, picked by its
/// title in the system palette.
fn links(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let links = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .map(|issue| issue.links.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        }
    });
    let write_note = cx
        .effect_button(
            "tasks-write-note",
            &crate::TasksEffect::WriteNote { issue: id.clone() },
        )
        .with_icon(ui::icons::Icon::Plus);
    let link_note = link_note_button(id);
    let chips = cx.switch(&links, |cx, links| {
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        for link in links {
            let reference = ui::tilo_reference::ResourceRef::whole(
                link.kind.clone(),
                link.resource.clone(),
                link.label.clone(),
            );
            row = row.push(Box::new(cx.reference(
                &link.app,
                link.record.as_deref(),
                &reference,
            )));
        }
        Box::new(row) as Element
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(
                        Container::new()
                            .with_flex_grow()
                            .with_no_padding()
                            .with_child(Box::new(
                                Text::new("tasks-links").with_role(TextRole::Heading),
                            )),
                    ))
                    .push(Box::new(link_note))
                    .push(Box::new(write_note)),
            ))
            .push(chips),
    )
}

/// The button that opens the system palette on the issue's "Link a note",
/// straight on the picker of the note.
fn link_note_button(id: &Id) -> Button {
    let issue = ui::tilo_resource::items::item_resource_id::<Tracker>("issues", id);
    let request = ui::search::SearchRequest::asking(
        ui::search::ScopeDecl::new(issue.clone(), crate::tr("tasks-link-note-scope")),
        Tracker::LINK_NOTE.id(),
    );
    Button::new(&format!("tasks-link-note-{issue}"), "tasks-link-note")
        .with_icon(ui::icons::Icon::Link)
        .on_action(ui::search::open_search_binding(&request))
}

fn status_badge(status: Status) -> Badge {
    let style = match status {
        Status::Done => ChipStyle::Tinted(Tone::Success),
        Status::InProgress | Status::InReview => ChipStyle::Tinted(Tone::Info),
        Status::Backlog | Status::Todo | Status::Cancelled => ChipStyle::Neutral,
    };
    Badge::text(tr(status.label_key())).style(style)
}

/// The thread: every message, oldest first, and the composer. A message is
/// the issue's own comment, written by whoever asks — a person here, or the
/// agent it is assigned to, through the same action.
fn thread(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let messages = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .map(|issue| issue.comments.values().cloned().collect::<Vec<Message>>())
                .unwrap_or_default()
        }
    });
    let people = cx.people();
    let list = cx.switch(&messages, move |cx, messages| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for message in messages {
            column = column.push(message_row(cx, &people, message));
        }
        Box::new(column) as Element
    });
    let draft = cx.local(String::new());
    let input = TextInput::from_state(&draft)
        .multiline(true)
        .with_id("tasks-thread-draft")
        .with_accessible_name(tr("tasks-thread-draft"))
        .with_placeholder("tasks-thread-placeholder");
    let comment_id = id.clone();
    let send = cx.actions().bind_draft(
        Button::new("tasks-thread-send", "tasks-thread-send"),
        &draft,
        Tracker::COMMENT.try_map(move |text: String| {
            Some(tracker::Comment {
                issue: comment_id.clone(),
                text: NonBlank::parse(&text)?,
            })
        }),
    );
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-thread").with_role(TextRole::Heading),
            ))
            .push(list)
            .push(Box::new(input))
            .push(Box::new(send)),
    )
}

fn message_row(cx: &ViewCx<Tracker>, people: &ui::framework::People, message: &Message) -> Element {
    let author = if message.by_agent {
        message.author.trim_start_matches("agent:").to_owned()
    } else {
        people.name(&message.author)
    };
    let mut heading = Row::new()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(Text::new(&author).strong()));
    match message.kind {
        MessageKind::Comment => {}
        MessageKind::ToRun => {
            heading = heading.push(Box::new(Badge::text(tr(if message.incorporated {
                "tasks-message-read-by-run"
            } else {
                "tasks-message-to-run"
            }))));
        }
        MessageKind::FromRun => {
            heading = heading.push(Box::new(
                Badge::text(tr("tasks-message-from-run")).style(ChipStyle::Tinted(Tone::Info)),
            ));
        }
        // A lookup is the run having read this, through system search under
        // its own permissions: the resource reads as the chip a link does, so
        // the person follows it where the run went without asking again.
        MessageKind::Step => {
            return crate::program_step::step_row(cx, heading, message);
        }
        MessageKind::Lookup | MessageKind::Answer => {
            heading = heading.push(Box::new(
                Badge::text(tr(if message.kind == MessageKind::Answer {
                    "tasks-message-answer"
                } else {
                    "tasks-message-lookup"
                }))
                .style(ChipStyle::Neutral),
            ));
            let Some(resource) =
                serde_json::from_str::<ui::tilo_reference::ResourceAt>(&message.lookup).ok()
            else {
                return Box::new(heading) as Element;
            };
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(ui::tokens::Rhythm::Tight)
                    .push(Box::new(heading))
                    .push(Box::new(cx.reference(
                        &resource.app,
                        resource.record.as_deref(),
                        &resource.reference,
                    ))),
            );
        }
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(heading))
            .push(Box::new(Text::new(&message.text))),
    )
}
