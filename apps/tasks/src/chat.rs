//! An issue as a conversation with its agent, in a window of its own.
//!
//! The chat is a view of the issue every other view shows, not a store of
//! its own: what was asked is the issue's title and description, the
//! conversation is its thread (`comments`), and the run is its `run`. The
//! person's words and the agent's read as the shared conversation list
//! does, the run's steps arriving in it as the agent posts them; what the
//! run looked up reads as reference chips; a question the run waits on
//! stands at the end, where the run waits, with the Answer that the host's
//! own sheet takes; and a field at the bottom tells the agent more through
//! the tracker's `tell-agent`, which steers the run going or starts the next
//! one with the message.
//!
//! A window is a chat when it was launched on [`uri`] (the published
//! `open-chat` effect asks the desktop for one beside the window it lands
//! in), so the shell, the palette and an agent all open it the same way.

use crate::model::{Issue, Message, MessageKind, Run, Tracker, tracker};
use ui::prelude::*;
use ui::tilo_resource::NonBlank;
use ui::widget::{
    AppModel, ButtonRole, ConversationLine, ConversationList, DeliveryState, Fact, Facts,
    ModelPath, When,
};
use ui::window_toolbar::{ToolbarLocation, WindowToolbar};

use crate::{reference, tr, tr_with};

/// The URI scheme Tasks claims: a chat window is launched on
/// `tilo-tasks:chat/<issue>`.
pub const SCHEME: &str = "tilo-tasks";
const CHAT_PREFIX: &str = "tilo-tasks:chat/";

/// The address a window that shows `issue` as a chat is launched on.
#[must_use]
pub fn uri(issue: &Id) -> String {
    format!("{CHAT_PREFIX}{issue}")
}

/// The issue a launch argument opens as a chat, when it names one.
pub(crate) fn target(argument: &str) -> Option<Id> {
    let issue = argument.strip_prefix(CHAT_PREFIX)?.trim();
    (!issue.is_empty() && !issue.contains('/')).then(|| Id::from_raw(issue))
}

/// The chat window for `issue`: the conversation filling it, no sidebar and
/// no list, and the issue named in the window bar.
pub(crate) fn window(cx: &ViewCx<Tracker>, issue: Id, tracker: &LocalState<Tracker>) -> Element {
    let shell = NavShell::new()
        .content_kind(ContentKind::Reading)
        .with_id("tasks.chat")
        .with_window_toolbar()
        .content(view(cx, &issue));
    Box::new(ChatWindow {
        bar: shell.window_bar_link(),
        shell,
        issue,
        tracker: tracker.clone(),
    })
}

/// The chat filling a window: the issue's conversation, or that it is gone.
pub(crate) fn view(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let exists = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.contains(&id)
    });
    let id = id.clone();
    cx.switch(&exists, move |cx, exists| {
        if *exists {
            content(cx, &id)
        } else {
            Box::new(
                EmptyState::new("tasks-issue-gone-title")
                    .with_message("tasks-issue-gone-message")
                    .with_id("tasks-chat-gone"),
            )
        }
    })
}

fn content(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    // The conversation scrolls and keeps to its end as steps arrive; the
    // run's state and the field stay below it, where the person writes.
    let conversation = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Section)
        .push(transcript(cx, id))
        .push(lookups(cx, id))
        .push(question(cx, id));
    // Both parts keep the scrollbar's lane, so the field below stands under
    // the conversation's column whether or not the conversation overflows.
    let scrolled = Scrollable::new_vertical()
        .fill_parent_height()
        .following_end()
        .with_stable_vertical_gutter()
        .with_semantic_key(format!("tasks.chat:{id}"))
        .with_child(Box::new(
            Container::new()
                .readable()
                .centered()
                .with_inset(ui::tokens::Rhythm::Group)
                .with_child(Box::new(conversation)),
        ));
    let bottom = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(run_state(cx, id))
        .push(composer(cx, id));
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_fill_remaining_height()
                    .push(Box::new(scrolled)),
            ))
            .push(Box::new(
                Scrollable::new_vertical()
                    .fit_content_height()
                    .with_stable_vertical_gutter()
                    .without_sequential_focus()
                    .with_child(Box::new(
                        Container::new()
                            .readable()
                            .centered()
                            .with_inset(ui::tokens::Rhythm::Group)
                            .with_child(Box::new(bottom)),
                    )),
            )),
    )
}

/// What the conversation shows: the request the issue was opened with,
/// then every message of its thread that says something, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
struct Conversation {
    /// Who asked, when, and what: the issue's author, its making and its
    /// description (its title when it has none).
    asked: Option<(String, i64, String)>,
    messages: Vec<(Id, Message)>,
}

impl Conversation {
    fn of(issue: &Issue) -> Self {
        let body = issue.body.text();
        let request = if body.trim().is_empty() {
            issue.title.trim().to_owned()
        } else {
            body.trim().to_owned()
        };
        Self {
            asked: (!issue.author.is_empty() && !request.is_empty())
                .then(|| (issue.author.clone(), issue.created_ms, request)),
            messages: issue
                .comments
                .iter()
                .filter(|(_, message)| message.kind != MessageKind::Lookup)
                .map(|(id, message)| (id.clone(), message.clone()))
                .collect(),
        }
    }
}

/// How an author reads: a person by the name the room gives them, an agent
/// by its name, never as a person.
pub(crate) fn author_name(people: &ui::framework::People, author: &str) -> String {
    let named = people.name(author);
    if named == author {
        author.trim_start_matches("agent:").to_owned()
    } else {
        named
    }
}

/// The conversation's lines: the person's own on their side, everyone
/// else's (other people, the agent and its steps) under their name. A
/// message to the run reads as delivered until the agent takes it in, then
/// as read.
fn lines(
    conversation: &Conversation,
    me: Option<&str>,
    people: &ui::framework::People,
) -> Vec<ConversationLine> {
    let mine = |author: &str| me == Some(author);
    let mut lines = Vec::new();
    if let Some((author, at_ms, text)) = &conversation.asked {
        lines.push(if mine(author) {
            ConversationLine::outgoing("tasks-chat-asked", *at_ms, text)
        } else {
            ConversationLine::incoming(
                "tasks-chat-asked",
                *at_ms,
                author_name(people, author),
                text,
            )
        });
    }
    for (id, message) in &conversation.messages {
        let key = format!("tasks-chat-message-{id}");
        let line = if !message.by_agent && mine(&message.author) {
            let line = ConversationLine::outgoing(key, message.at_ms, &message.text);
            if message.kind == MessageKind::ToRun {
                line.with_delivery(if message.incorporated {
                    DeliveryState::Read
                } else {
                    DeliveryState::Delivered
                })
            } else {
                line
            }
        } else {
            ConversationLine::incoming(
                key,
                message.at_ms,
                author_name(people, &message.author),
                &message.text,
            )
        };
        lines.push(line);
    }
    lines
}

fn transcript(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let conversation = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .map(Conversation::of)
                .unwrap_or_default()
        }
    });
    let me = cx.me().map(str::to_owned);
    let people = cx.people();
    cx.switch(&conversation, move |_, conversation| {
        let lines = lines(conversation, me.as_deref(), &people);
        Box::new(ConversationList::new(lines).with_accessible_name(tr("tasks-chat-conversation")))
            as Element
    })
}

/// What the run looked up, each a chip that opens it where it lives.
fn lookups(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let read = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .map(|issue| {
                    issue
                        .comments
                        .iter()
                        .filter(|(_, message)| message.kind == MessageKind::Lookup)
                        .filter_map(|(_, message)| {
                            serde_json::from_str::<ui::tilo_reference::ResourceAt>(&message.lookup)
                                .ok()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    });
    cx.switch(&read, |cx, read| {
        if read.is_empty() {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element;
        }
        let mut chips = Row::new().with_rhythm(ui::tokens::Rhythm::Tight);
        for resource in read {
            chips = chips.push(Box::new(cx.reference(
                &resource.app,
                resource.record.as_deref(),
                &resource.reference,
            )));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(
                    Text::new("tasks-chat-looked-up")
                        .with_role(TextRole::Caption)
                        .dimmed(),
                ))
                .push(Box::new(chips)),
        ) as Element
    })
}

/// What the run waits on, as the person decides it: the action it asks
/// about and where.
#[derive(Clone, Debug, Default, PartialEq)]
struct Asking {
    agent: String,
    action: String,
    /// Where it acts, as the draft names it.
    place: Option<String>,
}

fn asking(issue: &Issue) -> Option<Asking> {
    let run = issue.run.as_ref().filter(|run| run.asks_its_person())?;
    let question = run.confirmation.as_ref()?;
    let label = |key: &str| {
        question
            .draft
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
    };
    Some(Asking {
        agent: run.agent.0.clone(),
        action: label("action_label").unwrap_or_else(|| question.action.clone()),
        place: label("resource_label"),
    })
}

/// The question the run waits on, at the end of the conversation where it
/// waits, with the Answer that puts it to the person on the host's own
/// sheet. This app never answers for them.
fn question(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let asked = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.get(&id).and_then(asking)
    });
    let people = cx.people();
    let id = id.clone();
    cx.switch(&asked, move |cx, asked| {
        let Some(asked) = asked else {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element;
        };
        let agent = author_name(&people, &asked.agent);
        let message = match &asked.place {
            Some(place) => tr_with(
                "tasks-chat-asks-in",
                &[("action", &asked.action), ("place", place)],
            ),
            None => asked.action.clone(),
        };
        Box::new(
            Banner::info(tr_with("tasks-chat-asks", &[("agent", &agent)]))
                .with_message(message)
                .with_id("tasks-chat-question")
                .with_action(crate::issue::answer_button(cx, &id).role(ButtonRole::Primary)),
        ) as Element
    })
}

/// Where the run is, as the quiet line under the conversation reads it.
#[derive(Clone, Debug, Default, PartialEq)]
struct RunLine {
    agent: String,
    /// The Fluent id of the step it is in, or that a person paused it.
    state: &'static str,
    /// Why it was paused, when its budget stopped it.
    reason: Option<String>,
    spend: Vec<String>,
}

fn run_line(issue: &Issue) -> Option<RunLine> {
    let run = issue.run.as_ref()?;
    Some(RunLine {
        agent: run.agent.0.clone(),
        state: run.state_key(),
        reason: run
            .paused
            .as_ref()
            .map(|paused| paused.reason.trim().to_owned())
            .filter(|reason| !reason.is_empty()),
        spend: spend_facts(run),
    })
}

/// What a run spent, each a fact of its own: the money (or that it cost
/// none), the tokens, the model.
fn spend_facts(run: &Run) -> Vec<String> {
    let Some(spend) = run.spend.as_ref().filter(|spend| spend.steps_taken > 0) else {
        return Vec::new();
    };
    let mut facts = vec![
        if spend.currency.is_empty() {
            tr("tasks-run-spent-nothing")
        } else {
            tr_with(
                "tasks-run-spent",
                &[
                    ("cost", &crate::model::money(spend.cost())),
                    ("currency", &spend.currency),
                ],
            )
        },
        tr_with(
            "tasks-chat-used",
            &[("tokens", &ui::format::count(spend.tokens()))],
        ),
    ];
    if let Some(step) = spend.steps.last().filter(|step| !step.model.is_empty()) {
        facts.push(tr_with("tasks-chat-model", &[("model", &step.model)]));
    }
    facts
}

/// The run's state in a quiet line: the agent and its step, what it spent;
/// and the reason when its budget paused it.
fn run_state(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let state = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| tracker.issues.get(&id).and_then(run_line)
    });
    let people = cx.people();
    cx.switch(&state, move |_, state| {
        let Some(state) = state else {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element;
        };
        // Facts of one priority drop from the last: the model first, the
        // agent's step never.
        let mut facts = vec![Fact::text(tr_with(
            "tasks-run-agent",
            &[
                ("agent", &author_name(&people, &state.agent)),
                ("phase", &tr(state.state)),
            ],
        ))];
        facts.extend(state.spend.iter().cloned().map(Fact::text));
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        if let Some(reason) = &state.reason {
            column = column.push(Box::new(
                Banner::warning(reason.clone()).with_id("tasks-chat-budget"),
            ));
        }
        Box::new(column.push(Box::new(Facts::new(facts)))) as Element
    })
}

/// The field that tells the agent more, with Send, and Stop while a run is
/// going. Send is the tracker's `tell-agent`: the run going reads the
/// message at its next step, and with none going the next run starts with
/// it, so the conversation goes on after a run ended. An issue no agent is
/// assigned to keeps its thread as a thread.
fn composer(cx: &ViewCx<Tracker>, id: &Id) -> Element {
    let has_agent = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .is_some_and(|issue| issue.assigned_agent().is_some())
        }
    });
    // The draft outlives a run starting or ending under it.
    let draft = cx.local(String::new());
    let id = id.clone();
    cx.switch(&has_agent, move |cx, has_agent| {
        if *has_agent {
            agent_composer(cx, &id, &draft)
        } else {
            thread_composer(cx, &id, &draft)
        }
    })
}

fn field(draft: &LocalState<String>, placeholder: &str) -> TextInput {
    TextInput::from_state(draft)
        .with_id("tasks-chat-draft")
        .with_accessible_name(tr("tasks-thread-draft"))
        .with_placeholder(placeholder)
        .with_min_lines(2)
}

fn send_button() -> Button {
    Button::new("tasks-chat-send", "tasks-thread-send")
        .with_icon(ui::icons::Icon::Send)
        .role(ButtonRole::Primary)
}

fn agent_composer(cx: &ViewCx<Tracker>, id: &Id, draft: &LocalState<String>) -> Element {
    let tell_id = id.clone();
    let tell = Tracker::TELL_AGENT.try_map(move |text: String| {
        Some(tracker::TellAgent {
            issue: tell_id.clone(),
            text: NonBlank::parse(&text)?,
        })
    });
    let input =
        cx.actions()
            .submit_draft(field(draft, "tasks-chat-placeholder"), draft, tell.clone());
    let send = cx.actions().bind_draft(send_button(), draft, tell);
    let stop = cx.button(
        "tasks-run-stop",
        Tracker::STOP_RUN.with(tracker::StopRun { issue: id.clone() }),
    );
    let resume = cx.button(
        "tasks-run-resume",
        Tracker::RESUME_RUN.with(tracker::ResumeRun { issue: id.clone() }),
    );
    let shown_stop = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .and_then(Issue::active_run)
                .is_some()
        }
    });
    let shown_resume = cx.value({
        let id = id.clone();
        move |tracker: &Tracker| {
            tracker
                .issues
                .get(&id)
                .and_then(Issue::active_run)
                .is_some_and(|run| run.paused.is_some())
        }
    });
    let controls = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(When::from_state(
            &shown_stop,
            |shown| *shown,
            Box::new(stop),
        )))
        .push(Box::new(When::from_state(
            &shown_resume,
            |shown| *shown,
            Box::new(resume),
        )))
        .push(Box::new(Spacer::new()))
        .push(Box::new(send));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(input))
            .push(Box::new(controls)),
    )
}

fn thread_composer(cx: &ViewCx<Tracker>, id: &Id, draft: &LocalState<String>) -> Element {
    let send_id = id.clone();
    let send_message = Tracker::SEND_MESSAGE.try_map(move |text: String| {
        Some(tracker::SendMessage {
            issue: send_id.clone(),
            text: NonBlank::parse(&text)?,
        })
    });
    let input = cx.actions().submit_draft(
        field(draft, "tasks-thread-placeholder"),
        draft,
        send_message.clone(),
    );
    let send = cx.actions().bind_draft(send_button(), draft, send_message);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(input))
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(send)),
            )),
    )
}

/// The chat window, with the issue it shows named in the window's bar.
struct ChatWindow {
    shell: NavShell,
    bar: ui::widget::WindowBarLink,
    issue: Id,
    tracker: LocalState<Tracker>,
}

impl ChatWindow {
    fn window_toolbar(&self) -> WindowToolbar {
        let mut toolbar = self.bar.slots();
        let label = self.tracker.with(|tracker| {
            tracker
                .issues
                .get(&self.issue)
                .map(|issue| format!("{} {}", reference(issue.number), issue.title))
        });
        toolbar.location = Some(ToolbarLocation {
            label: label.unwrap_or_else(|| tr("tasks-chat-conversation")),
            ..ToolbarLocation::default()
        });
        toolbar
    }

    fn take_events(&self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            self.bar.event(&event);
        }
        asked
    }

    fn declare(&self) {
        ui::window_toolbar::declare(Some(self.window_toolbar()));
    }
}

impl ui::widget::Decorator for ChatWindow {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_events();
        self.declare();
        ui::widget::Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let asked = self.take_events();
        self.declare();
        ui::widget::Widget::update(&mut self.shell, state, path) || asked
    }

    fn name(&self) -> &'static str {
        "ChatWindow"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_is_launched_on_its_issues_address_and_nothing_else() {
        let issue = Id::from_raw("a1b2");
        assert_eq!(target(&uri(&issue)), Some(issue));
        assert_eq!(target("tilo-tasks:chat/"), None);
        assert_eq!(target("tilo-tasks:chat/a/b"), None);
        assert_eq!(target("tilo-tasks:issue/a1b2"), None);
    }
}
