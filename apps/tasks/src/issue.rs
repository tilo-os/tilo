//! An issue's page: its title, status and assignee, its description in the
//! block editor, its sub-issues, its agent's run and its thread. A private
//! issue's page is the same page over its own tracker, which only its
//! readers read.
//!
//! Each part that shows the issue's state is built again only when that
//! part changes, so typing in the title or the description never rebuilds
//! the editor under the caret.

use crate::model::{
    ConfirmationState, Issue, Message, MessageKind, RunPhase, Status, Tracker, issue_resource,
    tracker,
};
use tilo_rich_text::RichTextFieldViews;
use ui::prelude::*;
use ui::widget::{Badge, ChipStyle, Dropdown, Tone};

use crate::{Nav, reference, tr};

/// The page of `issue`, over the tracker that keeps it: built again when the
/// issue moves into a private tracker.
pub(crate) fn open(cx: &ViewCx<Tracker>, issue: &Id, nav: &Nav) -> Element {
    let Some(private) = nav.private.clone() else {
        return page(cx, issue.clone(), nav);
    };
    let home = cx.value_with_children(&private, {
        let issue = issue.clone();
        move |_, kept| {
            kept.iter()
                .find(|(_, tracker)| tracker.issues.contains(&issue))
                .map(|(home, _)| home.clone())
        }
    });
    let (issue, nav) = (issue.clone(), nav.clone());
    cx.switch(&home, move |cx, home| {
        let shown = home
            .as_ref()
            .and_then(|home| cx.record(&private.record(home), |cx| page(cx, issue.clone(), &nav)));
        shown.unwrap_or_else(|| page(cx, issue.clone(), &nav))
    })
}

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
                    .push(back(&nav))
                    .push(Box::new(
                        EmptyState::new("tasks-issue-gone-title")
                            .with_message("tasks-issue-gone-message")
                            .with_id("tasks-issue-gone"),
                    )),
            )
        }
    })
}

fn back(nav: &Nav) -> Element {
    let nav = nav.clone();
    Box::new(
        Button::new("tasks-issue-back", "tasks-back")
            .with_icon(ui::icons::Icon::ChevronLeft)
            .on_view(move || nav.open.set(None)),
    )
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
        .push(back(nav))
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
                            .push(privacy(cx, id, nav))
                            .push(Box::new(
                                Text::new("tasks-issue-description").with_role(TextRole::Heading),
                            ))
                            .push(body)
                            .push(sub_issues(cx, id, nav))
                            .push(links(cx, id))
                            .push(run(cx, id))
                            .push(thread(cx, id)),
                    )),
            )),
    )
}

/// Who reads a private issue; for another, choosing its readers makes it
/// private.
fn privacy(cx: &ViewCx<Tracker>, id: &Id, nav: &Nav) -> Element {
    let Some(private) = &nav.private else {
        return Box::new(Column::new());
    };
    let people = cx.people();
    if let Some(home) = nav.home(id) {
        let readers = private
            .reader_names(&people, &home)
            .unwrap_or_default()
            .join(", ");
        return Box::new(
            Row::new()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(
                    Badge::text(tr("tasks-private")).style(ChipStyle::Tinted(Tone::Warning)),
                ))
                .push(Box::new(
                    Text::new(&ui::i18n::I18nManager::format(
                        "tasks-private-readers",
                        &[("readers", readers.into())],
                    ))
                    .with_id("tasks-issue-readers")
                    .dimmed(),
                )),
        );
    }
    let choosing = cx.local(false);
    let shown = choosing.clone();
    let (private, issue) = (private.clone(), id.clone());
    let reveal = Button::new("tasks-issue-make-private", "tasks-private-make")
        .with_icon(ui::icons::Icon::Lock)
        .on_view(move || shown.set(!shown.get()));
    let chooser = cx.switch(&choosing, move |_, choosing| {
        if *choosing {
            private.readers_chooser(&people, &issue, "tasks-issue-private", "tasks-private-make")
        } else {
            Box::new(Column::new())
        }
    });
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(reveal))
            .push(chooser),
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
    let assignee = cx.control_with(
        |state| {
            TextInput::from_state(state)
                .with_id("tasks-issue-assignee")
                .with_accessible_name(tr("tasks-field-assignee"))
                .with_placeholder("tasks-issue-assignee-placeholder")
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
    let model_id = id.clone();
    let read_id = id.clone();
    let model = cx.control_with(
        |state| {
            TextInput::from_state(state)
                .with_id("tasks-issue-model")
                .with_accessible_name(tr("tasks-field-model"))
                .with_placeholder("tasks-issue-model-placeholder")
        },
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&read_id)
                .and_then(|issue| issue.model.as_ref())
                .map(|model| format!("{}/{}", model.profile, model.model))
                .unwrap_or_default()
        },
        Tracker::CHOOSE_MODEL.map(move |text: String| tracker::ChooseModel {
            issue: model_id.clone(),
            model: model_choice(&text),
        }),
    );
    Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(labeled_field("tasks-field-status", status)))
            .push(Box::new(labeled_field("tasks-field-assignee", assignee)))
            .push(Box::new(labeled_field("tasks-field-model", model))),
    )
}

/// The model typed as `provider/model`; nothing typed is the agent's own.
fn model_choice(text: &str) -> Option<crate::model::ModelChoice> {
    let (profile, model) = text.trim().split_once('/')?;
    Some(crate::model::ModelChoice {
        profile: profile.trim().to_owned(),
        model: model.trim().to_owned(),
    })
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
    let input = TextInput::from_state(&draft)
        .with_id("tasks-issue-label")
        .with_accessible_name(tr("tasks-issue-add-label"))
        .with_placeholder("tasks-issue-label-placeholder");
    let label_id = id.clone();
    let add = cx.actions().bind(
        Button::new("tasks-issue-add-label", "tasks-issue-add-label"),
        Tracker::ADD_LABEL.map(move |()| tracker::AddLabel {
            issue: label_id.clone(),
            label: take(&draft),
        }),
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
                "tasks-sub-issue-new",
            )),
    )
}

/// What the issue links to, each a reference chip that opens it in the app
/// that keeps it. Nothing shows while it links to nothing.
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
    cx.switch(&links, |cx, links| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        if links.is_empty() {
            return Box::new(column) as Element;
        }
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        for link in links {
            let reference = ui::tilo_reference::ResourceRef::whole(
                link.kind.clone(),
                link.resource.clone(),
                link.label.clone(),
            );
            row = row.push(Box::new(cx.reference(&link.app, None, &reference)));
        }
        column = column
            .push(Box::new(
                Text::new("tasks-links").with_role(TextRole::Heading),
            ))
            .push(Box::new(row));
        Box::new(column) as Element
    })
}

fn status_badge(status: Status) -> Badge {
    let style = match status {
        Status::Done => ChipStyle::Tinted(Tone::Success),
        Status::InProgress | Status::InReview => ChipStyle::Tinted(Tone::Info),
        Status::Backlog | Status::Todo | Status::Cancelled => ChipStyle::Neutral,
    };
    Badge::text(tr(status.label_key())).style(style)
}

/// What the run reads as now: its phase, the question it asks, the agent.
#[derive(Clone, Debug, Default, PartialEq)]
struct RunView {
    agent: Option<String>,
    phase: Option<RunPhase>,
    question: Option<String>,
}

fn run_view(issue: &Issue) -> RunView {
    RunView {
        agent: issue.assigned_agent().map(str::to_owned),
        phase: issue.run.as_ref().map(|run| run.phase),
        question: issue
            .run
            .as_ref()
            .filter(|run| run.phase == RunPhase::Waiting)
            .and_then(|run| run.confirmation.as_ref())
            .filter(|question| question.state == ConfirmationState::Waiting)
            .map(|question| question.action.clone()),
    }
}

/// The agent's run: start it, stop it, answer what it asks.
fn run(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let state = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.get(&id).map(run_view).unwrap_or_default()
    });
    let id = id.clone();
    cx.switch(&state, move |cx, view| {
        let Some(agent) = &view.agent else {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element;
        };
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new("tasks-run").with_role(TextRole::Heading),
            ));
        let phase = view.phase.map_or_else(
            || tr("tasks-run-not-started"),
            |phase| tr(phase.label_key()),
        );
        column = column.push(Box::new(Text::new(&ui::i18n::I18nManager::format(
            "tasks-run-agent",
            &[
                (
                    "agent",
                    agent.trim_start_matches("agent:").to_owned().into(),
                ),
                ("phase", phase.into()),
            ],
        ))));
        if let Some(action) = &view.question {
            column = column.push(Box::new(
                Banner::warning(ui::i18n::I18nManager::format(
                    "tasks-run-asks",
                    &[("action", action.clone().into())],
                ))
                .with_id("tasks-run-question"),
            ));
            column = column.push(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(cx.button(
                        "tasks-run-confirm",
                        Tracker::ANSWER_CONFIRMATION.with(tracker::AnswerConfirmation {
                            issue: id.clone(),
                            confirm: true,
                            edits: None,
                            reason: None,
                        }),
                    )))
                    .push(Box::new(cx.button(
                        "tasks-run-decline",
                        Tracker::ANSWER_CONFIRMATION.with(tracker::AnswerConfirmation {
                            issue: id.clone(),
                            confirm: false,
                            edits: None,
                            reason: None,
                        }),
                    ))),
            ));
        }
        let going = view.phase.is_some_and(RunPhase::active);
        let control = if going {
            cx.button(
                "tasks-run-stop",
                Tracker::STOP_RUN.with(tracker::StopRun { issue: id.clone() }),
            )
        } else {
            cx.button(
                "tasks-run-approve",
                Tracker::APPROVE_RUN.with(tracker::ApproveRun { issue: id.clone() }),
            )
        };
        Box::new(column.push(Box::new(control))) as Element
    })
}

/// The thread: every message, oldest first, and the composer. While a run
/// is going, a message can go to the run instead of only the thread.
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
    let list = cx.switch(&messages, move |_, messages| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for message in messages {
            column = column.push(message_row(&people, message));
        }
        Box::new(column) as Element
    });
    let draft = cx.local(String::new());
    let input = TextInput::from_state(&draft)
        .multiline(true)
        .with_id("tasks-thread-draft")
        .with_accessible_name(tr("tasks-thread-draft"))
        .with_placeholder("tasks-thread-placeholder");
    let comment_draft = draft.clone();
    let comment_id = id.clone();
    let comment = cx.actions().bind(
        Button::new("tasks-thread-comment", "tasks-thread-comment"),
        Tracker::COMMENT.map(move |()| tracker::Comment {
            issue: comment_id.clone(),
            text: take(&comment_draft),
        }),
    );
    let run_draft = draft;
    let run_id = id.clone();
    let to_run = cx.actions().bind(
        Button::new("tasks-thread-to-run", "tasks-thread-to-run"),
        Tracker::SEND_MESSAGE.map(move |()| tracker::SendMessage {
            issue: run_id.clone(),
            text: take(&run_draft),
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
            .push(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(comment))
                    .push(Box::new(to_run)),
            )),
    )
}

/// The draft as it is sent, emptied for the next message.
fn take(draft: &LocalState<String>) -> String {
    let text = draft.get();
    draft.set(String::new());
    text
}

fn message_row(people: &ui::framework::People, message: &Message) -> Element {
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
