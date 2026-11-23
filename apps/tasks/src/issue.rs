//! An issue's page: its title, status and assignee, its description in the
//! block editor, its sub-issues, what it links to and its thread.
//!
//! Each part that shows the issue's state is built again only when that
//! part changes, so typing in the title or the description never rebuilds
//! the editor under the caret. The page draws no Back of its own: the
//! window bar's Back (and a narrow window's) closes it, and the bar's
//! location names it.

use crate::issue_list::{IssueList, issue_list};
use crate::model::{
    Issue, Message, MessageKind, Priority, Status, Tracker, agent_assignee, issue_resource,
    person_assignee,
};
use tilo_rich_text::{RichTextField, RichTextSource, RichTextSurface};
use ui::framework::Chain;
use ui::prelude::*;
use ui::tilo_resource::NonBlank;
use ui::widget::arrange::Stack;
use ui::widget::{Badge, ButtonRole, ChipStyle, Fact, Facts, Tone};

use crate::{Nav, Tasks, Window, reference, tr, tr_with};

pub(crate) fn page(cx: &ViewCx<Tracker>, id: Id, nav: &Nav, window: &Window) -> Element {
    // The part that keeps the issue: the page is a view of that part, so its
    // fields bind the issue's paths there and every change goes to it. Built
    // again only when the issue is kept elsewhere (or no longer), never
    // while it is edited.
    let (held, labels) = match &window.chain {
        Some(chain) => {
            let (id, labelled) = (id.clone(), id.clone());
            (
                cx.chain_value(chain, move |_, parts| {
                    parts
                        .iter()
                        .rev()
                        .find(|(_, part)| part.issues.contains(&id))
                        .map(|(index, _)| *index)
                }),
                {
                    // The window's own reading of the issues, which the list already brought
                    // up to date: opening an issue reads none of the others again, and a
                    // change to one issue reads that issue.
                    let index = std::rc::Rc::clone(&window.index);
                    cx.chain_value(chain, move |_, parts| {
                        Labels::of(&crate::read_issues(&index, parts), &labelled)
                    })
                },
            )
        }
        None => (LocalState::new(None), LocalState::new(Labels::default())),
    };
    let page = Page {
        window: window.clone(),
        labels,
    };
    let nav = nav.clone();
    cx.switch(&held, move |cx, held| {
        // The window's reading of the issues leaves each one's thread and
        // description unread until a view shows the issue: this page does.
        if let (Some(index), Some(chain)) = (held, page.window.chain.as_ref()) {
            chain.demand(
                *index,
                ui::tilo_resource::Demand::none().with_keys([id.as_str()]),
            );
        }
        let part = held.and_then(|index| page.window.chain.as_ref()?.part_record(index));
        let shown = part.and_then(|part| {
            let (id, nav, page) = (id.clone(), nav.clone(), page.clone());
            cx.record(&part, move |cx| content(cx, &id, &nav, &page))
        });
        shown.unwrap_or_else(|| {
            Box::new(
                Column::new()
                    .with_rhythm(ui::tokens::Rhythm::Group)
                    .push(Box::new(
                        EmptyState::new("tasks-issue-gone-title")
                            .with_message("tasks-issue-gone-message")
                            .with_id("tasks-issue-gone"),
                    )),
            )
        })
    })
}

/// What an issue's page reads beyond the part that keeps it: the window,
/// and the labels every issue carries.
#[derive(Clone)]
struct Page {
    window: Window,
    labels: LocalState<Labels>,
}

fn content(cx: &ViewCx<IssueList>, id: &Id, nav: &Nav, page: &Page) -> Element {
    let window = &page.window;
    let paths = IssueList::paths().issues.item(id);
    let number = cx.value({
        let id = id.clone();
        move |part: &IssueList| part.issues.get(&id).map_or(0, |issue| issue.number)
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
    // The part that keeps the issue: its body's page edits and reads it there,
    // never the tracker's own document the page is a view of.
    let part = page.window.chain.as_ref().and_then(|chain| {
        chain
            .parts()
            .into_iter()
            .find(|(_, held)| held.issues.contains(id))
            .map(|(index, _)| index)
    });
    let body = IssueBody {
        field: RichTextField::new(
            cx.child::<tilo_rich_text::RichText>(&body_id, move |op| body_edit(&body_path, &op)),
            move |doc| locate_path.object(doc),
        ),
        kept: page.window.chain.clone().zip(part),
    };
    let body: Element = Box::new(RichTextSurface::over(body));

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
                            .push(properties(cx, id, page))
                            .push(plan(cx, id))
                            .push(body)
                            .push(sub_issues(cx, id, nav, window))
                            .push(links(cx, id))
                            .push(thread(cx, id)),
                    )),
            )),
    )
}

/// An issue's description as its page reads and edits it: the rich text kept
/// in the part that keeps the issue, not in the tracker the page is a view
/// of. Hosted, every sync reads the part's document through the chain; shown
/// without one, it reads the document it is given.
struct IssueBody {
    field: RichTextField,
    /// The chain and the part keeping the issue.
    kept: Option<(Chain<IssueList>, u64)>,
}

impl RichTextSource for IssueBody {
    fn read_only(&self) -> bool {
        self.field.read_only()
    }

    fn sync(&mut self, doc: &ui::automerge::AutoCommit) {
        let Self { field, kept } = self;
        match kept {
            Some((chain, part)) => {
                if chain.read_part(*part, |doc| field.sync(doc)).is_none() {
                    tracing::warn!(
                        part = *part,
                        "tasks: the part keeping the issue is not held"
                    );
                }
            }
            None => field.sync(doc),
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.revision()
    }

    fn read_from(&mut self, doc: &ui::automerge::AutoCommit) -> bool {
        let Self { field, kept } = self;
        match kept {
            Some((chain, part)) => chain
                .read_part(*part, |doc| field.read_from(doc))
                .unwrap_or(false),
            None => field.read_from(doc),
        }
    }

    fn with_state<T>(&self, read: impl FnOnce(&tilo_rich_text::RichTextState) -> T) -> Option<T> {
        self.field.with_state(read)
    }

    fn apply(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        edit: &tilo_rich_text::RichTextAction,
    ) -> Result<(), ui::tilo_actions::ActionError> {
        self.field.apply(edits, edit)
    }

    fn presence_keys(&self) -> Option<ui::rich_edit::PresenceKeyFn> {
        self.field.presence_keys()
    }
}

/// The edit of the description that `op` is: a field edit of the tracker.
#[allow(
    clippy::expect_used,
    reason = "a rich-text op is plain data that always serializes, and every field of the tracker offers its edits"
)]
fn body_edit(
    path: &ui::tilo_resource::Path<IssueList, tilo_rich_text::RichText>,
    op: &tilo_rich_text::RichTextAction,
) -> <IssueList as ui::tilo_resource::Resource>::Action {
    IssueList::edit_action(path.perform(op).expect("a rich-text op serializes"))
        .expect("the tracker offers field edits")
}

/// Where the issue stands, whom it is for and what it is filed under: one
/// row of properties under its title, each a chip that opens what it can be
/// set to — no field labels above them.
fn properties(cx: &ViewCx<IssueList>, id: &Id, window: &Page) -> Element {
    Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(status_picker(cx, id))
            .push(assignee_picker(cx, id))
            .push(labels(cx, id, &window.labels)),
    )
}

/// The issue's status, as the chip that opens every status it can take.
fn status_picker(cx: &ViewCx<IssueList>, id: &Id) -> Element {
    let read_id = id.clone();
    let status = cx.value(move |part: &IssueList| {
        part.issues
            .get(&read_id)
            .map(|issue| issue.status)
            .unwrap_or_default()
    });
    let id = id.clone();
    cx.switch(&status, move |cx, status| {
        let mut choices = Stack::stack();
        for choice in Status::ALL {
            let place_id = id.clone();
            let place = IssueList::PLACE.map(move |()| issue_list::Place {
                issue: place_id.clone(),
                status: choice,
                before: None,
                rank: None,
            });
            let label = tr(choice.label_key());
            let button = Button::new(
                &format!("tasks-issue-status-{}", choice.label_key()),
                &label,
            )
            .role(ButtonRole::Plain);
            choices = choices.push(Box::new(match cx.actions().binding(place) {
                Some(binding) => button.on_action(binding),
                None => button.with_enabled(false),
            }));
        }
        let label = tr(status.label_key());
        let mut chip = Button::new("tasks-issue-status", &label)
            .showing_choice()
            .with_accessible_name(&format!("{}: {label}", tr("tasks-field-status")));
        if let Some(tone) = status_tone(*status) {
            chip = chip.tone(tone);
        }
        Box::new(chip.with_popover(choices)) as Element
    })
}

/// The issue's assignee, as the chip of the person or agent it is for.
fn assignee_picker(cx: &ViewCx<IssueList>, id: &Id) -> Element {
    let read_id = id.clone();
    let assign_id = id.clone();
    let people = cx.people();
    cx.person_picker(
        "tasks-issue-assignee",
        tr("tasks-field-assignee"),
        tr("tasks-assignee-nobody"),
        |member| match &member.id {
            ui::permissions::MemberId::Person(id) => Some(person_assignee(id).0),
            ui::permissions::MemberId::Agent(id) => Some(agent_assignee(id).0),
            ui::permissions::MemberId::Bot(_) => None,
        },
        move |part: &IssueList| {
            part.issues
                .get(&read_id)
                .and_then(|issue| crate::board::assignee(&people, issue))
        },
        IssueList::ASSIGN.map(move |assignee: Option<String>| issue_list::Assign {
            issue: assign_id.clone(),
            assignee,
        }),
    )
}

/// Who wrote the issue and when, what it works towards, how urgent it is,
/// how much work it is, when it is wanted and the files attached to it:
/// each its own field a person reads, never a control and never one
/// sentence joined with separators. Who wrote it is the shared provenance
/// row: the author's chip, the tool it came from, and the day.
fn plan(cx: &ViewCx<IssueList>, id: &Id) -> Element {
    let planned = cx.value({
        let id = id.clone();
        move |part: &IssueList| part.issues.get(&id).map(Plan::of).unwrap_or_default()
    });
    let people = cx.people();
    cx.switch(&planned, move |_cx, planned| {
        let written = people.provenance(
            planned.author.as_ref(),
            &planned.author_name,
            planned.created_ms,
            planned.origin.as_ref(),
        );
        let mut facts = Vec::new();
        if let Some(label) = &planned.goal {
            facts.push(Fact::field(tr("tasks-field-goal"), label));
        }
        let priority = Priority::of_level(planned.priority);
        if priority != Priority::None {
            facts.push(Fact::text(tr_with(
                "tasks-fact-priority",
                &[("priority", &tr(priority.label_key()))],
            )));
        }
        if let Some(estimate) = planned.estimate {
            facts.push(Fact::text(tr_with(
                "tasks-fact-estimate",
                &[("points", &estimate.to_string())],
            )));
        }
        for file in &planned.attachments {
            facts.push(Fact::text(tr_with(
                "tasks-fact-attachment",
                &[("name", file)],
            )));
        }
        if let Some(due) = &planned.due {
            facts.push(Fact::text(tr_with("tasks-fact-due", &[("date", due)])));
        }
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(written.row("tasks-issue-written"));
        if !facts.is_empty() {
            column = column.push(Box::new(Facts::new(facts)));
        }
        Box::new(column) as Element
    })
}

/// How a issue is planned, as its page reads it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Plan {
    /// How its goal reads: the resource it works towards.
    goal: Option<String>,
    priority: Option<u8>,
    estimate: Option<u32>,
    /// When it is wanted, as a person reads a date.
    due: Option<String>,
    /// Who wrote it and when, and where it came from.
    author: Option<ui::tilo_resource::MemberId>,
    author_name: String,
    created_ms: i64,
    origin: Option<ui::tilo_resource::Origin>,
    /// The names of the files attached to it.
    attachments: Vec<String>,
}

impl Plan {
    fn of(issue: &Issue) -> Self {
        Self {
            goal: issue.area.as_ref().map(|area| area.label.clone()),
            priority: issue.priority,
            estimate: issue.estimate,
            author: issue.author.clone(),
            author_name: issue.author_name.clone(),
            created_ms: issue.created_ms,
            origin: issue.origin.clone(),
            attachments: issue
                .attachments
                .values()
                .map(|file| file.name.clone())
                .collect(),
            due: issue
                .due_ms
                .map(|due_ms| ui::format::date_of(&ui::format::UnixSecs(due_ms / 1_000))),
        }
    }
}

/// The issue's labels, as the chips it carries, and the chip that opens the
/// ones the workspace already uses and the field that makes a new one.
fn labels(cx: &ViewCx<IssueList>, id: &Id, shown: &LocalState<Labels>) -> Element {
    let id = id.clone();
    cx.switch(shown, move |cx, shown| {
        let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        for label in &shown.mine {
            row = row.push(Box::new(Badge::text(label.clone())));
        }
        row = row.push(add_label(cx, &id, &shown.others));
        Box::new(row) as Element
    })
}

/// The issue's labels, and the ones the rest of the workspace carries.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Labels {
    /// The issue's own, in the order it holds them.
    mine: Vec<String>,
    /// Every label another issue carries, sorted: what the picker offers
    /// that this issue does not have yet.
    others: Vec<String>,
}

impl Labels {
    fn of(issues: &crate::Issues, id: &Id) -> Self {
        let mine: Vec<String> = issues
            .get(id)
            .map(|issue| issue.labels.iter().cloned().collect())
            .unwrap_or_default();
        let mut others: Vec<String> = issues
            .labels()
            .map(|(label, _)| label)
            .filter(|label| !mine.iter().any(|own| own == label))
            .map(str::to_owned)
            .collect();
        others.sort();
        others.dedup();
        Self { mine, others }
    }
}

/// The `+` after the labels: it opens the labels the workspace already uses,
/// each one adding it to the issue, and a field whose text makes a new one.
fn add_label(cx: &ViewCx<IssueList>, id: &Id, others: &[String]) -> Element {
    let draft = cx.local(String::new());
    let label_id = id.clone();
    let add = IssueList::ADD_LABEL.try_map(move |label: String| {
        Some(issue_list::AddLabel {
            issue: label_id.clone(),
            label: NonBlank::parse(&label)?,
        })
    });
    let mut content = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight);
    for label in others {
        let issue = id.clone();
        let name = label.clone();
        let add_one = IssueList::ADD_LABEL.try_map(move |()| {
            Some(issue_list::AddLabel {
                issue: issue.clone(),
                label: NonBlank::parse(&name)?,
            })
        });
        let button =
            Button::new(&format!("tasks-issue-label-{label}"), label).role(ButtonRole::Plain);
        content = content.push(Box::new(match cx.actions().binding(add_one) {
            Some(binding) => button.on_action(binding),
            None => button.with_enabled(false),
        }));
    }
    let input = cx.actions().submit_draft(
        TextInput::from_state(&draft)
            .with_id("tasks-issue-label-new")
            .with_accessible_name(tr("tasks-issue-new-label"))
            .with_placeholder("tasks-issue-label-placeholder"),
        &draft,
        add.clone(),
    );
    let create = cx.actions().bind_draft(
        Button::new("tasks-issue-label-create", "tasks-create-button"),
        &draft,
        add,
    );
    content = content.push(Box::new(
        Row::new()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(160.0)
                    .with_child(Box::new(input)),
            ))
            .push(Box::new(create)),
    ));
    let chip = Button::icon_only(
        "tasks-issue-add-label",
        ui::icons::Icon::Plus,
        &tr("tasks-issue-add-label"),
    )
    // The chip's face, as the fields before it wear: a bare glyph would read
    // as a mark on the row, not a control of it.
    .role(ButtonRole::Secondary);
    Box::new(chip.with_popover(content))
}

/// The issue's sub-issues, each opening its own page, and a field to add
/// one.
fn sub_issues(cx: &ViewCx<IssueList>, id: &Id, nav: &Nav, window: &Window) -> Element {
    let nav = nav.clone();
    let parent = id.clone();
    let list = cx.switch(&window.tasks, move |_, tasks: &Tasks| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight);
        for (child, issue) in tasks.issues().children(Some(&parent)) {
            let open = nav.clone();
            let child = child.clone();
            let label = format!("{} {}", reference(issue.number), issue.title);
            column = column.push(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(
                        Button::new(&format!("tasks-sub-issue-{child}"), &label)
                            .on_open(move || open.open(&child)),
                    ))
                    .push(Box::new(status_badge(issue.status))),
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
                &|| cx.local(String::new()),
                window,
                Some(id.clone()),
                None,
                "tasks-sub-issue-new",
            )),
    )
}

/// What the issue links to, each a reference chip that opens it in the app
/// that keeps it; and the way to link a note already written, picked by its
/// title in the system palette.
fn links(cx: &ViewCx<IssueList>, id: &Id) -> Element {
    let links = cx.value({
        let id = id.clone();
        move |part: &IssueList| {
            part.issues
                .get(&id)
                .map(|issue| issue.links.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        }
    });
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
                    .push(Box::new(link_note)),
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
        IssueList::LINK_NOTE.id(),
    );
    Button::new(&format!("tasks-link-note-{issue}"), "tasks-link-note")
        .with_icon(ui::icons::Icon::Link)
        .on_action(ui::search::open_search_binding(&request))
}

/// What a status says about the issue, as the tone a chip states it in.
fn status_tone(status: Status) -> Option<Tone> {
    match status {
        Status::Done => Some(Tone::Success),
        Status::InProgress | Status::InReview => Some(Tone::Info),
        Status::Backlog | Status::Todo | Status::Cancelled => None,
    }
}

fn status_badge(status: Status) -> Badge {
    Badge::text(tr(status.label_key()))
        .style(status_tone(status).map_or(ChipStyle::Neutral, ChipStyle::Tinted))
}

/// The thread: every message, oldest first, and the composer. A message is
/// the issue's own comment, written by whoever asks — a person here, or the
/// agent it is assigned to, through the same action.
fn thread(cx: &ViewCx<IssueList>, id: &Id) -> Element {
    let messages = cx.value({
        let id = id.clone();
        move |part: &IssueList| {
            part.issues
                .get(&id)
                .map(|issue| {
                    let comments = issue.comments.iter();
                    comments.map(|(comment, message)| (comment.clone(), message.clone()))
                })
                .into_iter()
                .flatten()
                .collect::<Vec<(Id, Message)>>()
        }
    });
    let people = cx.people();
    let issue = id.clone();
    let list = cx.switch(&messages, move |cx, messages| {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for (comment, message) in messages {
            // Its words are the comment's own, under the issue: an agent's comment is drawn on
            // them as it lands.
            let place = format!("{}/{}", issue.as_str(), comment.as_str());
            column = column.push(message_row(cx, &people, message, &place));
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
        IssueList::COMMENT.try_map(move |text: String| {
            Some(issue_list::Comment {
                issue: comment_id.clone(),
                text: NonBlank::parse(&text)?,
                origin: None,
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

fn message_row(
    cx: &ViewCx<IssueList>,
    people: &ui::framework::People,
    message: &Message,
    place: &str,
) -> Element {
    // A comment moved from another tracker reads as its author's there, on
    // its day there; any other as its author's here, by the name they acted
    // under where the workspace names them not. Who, where and when are the
    // shared provenance row's own fields.
    let written = people.provenance(
        message.author.as_ref(),
        &message.author_name,
        message.at_ms,
        message.origin.as_ref(),
    );
    let mut heading = Row::new()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(written.row("tasks-message-written"));
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
            .push(Box::new(Text::new(&message.text).edits("comments", place))),
    )
}
