//! A channel's page, as a view of a resource: its messages and its composer, built from the
//! same described components a tilolang view builds (one-app-model.md §2).
//!
//! Chat declares `Channel` as a resource and this page as its default view: a place that shows
//! a channel — `#general` typed on the canvas, or the channel an agent's answer names — draws
//! this page, with the people who spoke, what they said, who reacted and how, and the box that
//! sends the next message. Pressing an attachment places what it holds, which is the system's
//! open (shell.md §3), never Chat's; typing in the composer and pressing send posts to the
//! channel through Chat's own `post` action. Nothing here knows about windows, places or themes:
//! the page is drawn by the same host, in the person's own theme, as Mail's inbox or a Notes
//! notebook.
//!
//! An agent's reply carries its work, and the page draws what the reply carries: a turn still
//! being written says so quietly beside the words it has so far, and a turn that ended without
//! an answer leaves a row saying what happened with a Retry — the conversation's own `retry`,
//! which asks the agent again on the same message rather than posting the question twice
//! (shell.md §8.1).

use tilo_view::node;
use tilo_view::ui::{self, Gap};
use tilo_view::{Arg, Node, ResourceRef, Value, ViewCx, ViewResource};
use tilo_view_macros::view;

/// The app SDK, under a name the view's own `ui` (the described components) does not take:
/// a conversation's id is the SDK's `Id`, which the chat record's fields name.
use ::ui as sdk;
type Id = sdk::tilo_resource::Id;

/// The conversation toolkit's clock and day breaks: how a transcript labels a day, and the rule
/// a run of one person's messages follows, read from the toolkit the view draws with.
use sdk::widget::conversation::list::RUN_WINDOW_SECS;
use sdk::widget::conversation::{ConversationClock, DayBreaks};

/// One conversation of a workspace, as the place that shows it names it: a channel everyone in
/// the workspace talks in, or a conversation some of them do.
///
/// A channel is a resource: a place names it by its id (`chat.workspace/channels/c_general`), the session
/// publishes its reading — its name and topic, and the messages it holds, oldest first — and
/// this view draws it. Its messages come with it, so the page needs nothing else.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Channel {
    /// Its id, after `chat.workspace/channels/`.
    #[serde(default)]
    pub id: String,
    /// What the conversation is called: a channel's name (`general`), or the people it is with.
    #[serde(default)]
    pub name: String,
    /// What the channel is about, as whoever keeps it wrote it.
    #[serde(default)]
    pub topic: String,
    /// Whether it is a call rather than a conversation to read.
    #[serde(default)]
    pub voice: bool,
    /// The people its messages name, by the participant id each is written under.
    #[serde(default)]
    pub people: Vec<Person>,
    /// Its messages, oldest first, each with its body, reactions and what it answers.
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// Someone who speaks in a channel, as their name shows them.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Person {
    /// Their participant id, as messages and reactions name them.
    #[serde(default)]
    pub id: String,
    /// The name they show.
    #[serde(default)]
    pub name: String,
    /// An automated agent rather than a person.
    #[serde(default)]
    pub agent: bool,
}

/// What a channel's record does not itself say of the people in it, which the session reading
/// it knows: the name its reader goes by on this device — the very name the top bar shows them
/// — and the workspace's agents by their id. A record holds ids; a person is named where they
/// are read.
///
/// Chat's record names the people it was told about (a workspace's roster), and a session's
/// own record may never have been told of its reader or of the agent it asks: the reading is
/// then completed here rather than showing anyone their routing id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Names {
    /// The reader's own name, as this device shows them; empty when it knows none.
    pub me: String,
    /// Each agent of the workspace, by its id (the part after `agent:`), as the workspace
    /// keeps its name.
    pub agents: crate::projection::AgentNames,
}

/// One message of a channel, as its page shows it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the page's message carries independent facts the row draws (edited, pinned, retry, sending), each read off the record on its own"
)]
pub struct Message {
    /// Its id, after `chat.workspace/messages/`.
    #[serde(default)]
    pub id: String,
    /// Who wrote it: their participant id, which [`Channel::people`] names.
    #[serde(default)]
    pub author: String,
    /// When, in seconds since the epoch.
    #[serde(default)]
    pub ts: i64,
    /// Its text, one line per paragraph: what it says.
    #[serde(default)]
    pub text: String,
    /// Whether it was changed after it was posted.
    #[serde(default)]
    pub edited: bool,
    /// The message it answers, as the quote over it says it.
    #[serde(default)]
    pub reply_to: Option<Quote>,
    /// Who reacted with which emoji, each emoji once, in the order they were first used.
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// What it carries besides its words: the records it was shared from, the words it quotes,
    /// and the files it posts.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Whether it is pinned to its channel.
    #[serde(default)]
    pub pinned: bool,
    /// An agent's work on this message, when it is an agent's reply: what it is doing, or how
    /// it ended, drawn under the words it wrote. `None` on every ordinary message.
    #[serde(default)]
    pub work: Option<Work>,
    /// Retrying this reply asks the agent again: it failed, it is the reply the agent would
    /// write now (no newer reply has replaced it), and nobody has asked again yet.
    #[serde(default)]
    pub retry: bool,
    /// The message it answers, when it asks the agent again: the retry names both, so nothing
    /// re-posts the person's words. `None` on every other message.
    #[serde(default)]
    pub retry_summons: Option<String>,
    /// How many times the person asked again for an answer to this message (`retry`): the
    /// attempt the agent answers in now. A reply is of the attempt its question is in.
    #[serde(default)]
    pub attempts: u32,
    /// Sent from here and not written yet: the conversation's messages are on their way to this
    /// device, and it is written once they are here. Its byline says so in place of its time.
    #[serde(default)]
    pub sending: bool,
}

/// What an agent recorded of its work on a reply, as the page shows it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Work {
    /// Where the work is: `taken`, `waiting`, `doing`, `done`, `refused`, or a
    /// `provider_*` end.
    #[serde(default)]
    pub state: String,
    /// What is under way, in the agent's words (`doing`), or why it ended.
    #[serde(default)]
    pub detail: String,
}

impl Work {
    /// The reply is still being written: the agent took the turn and has not ended it.
    #[must_use]
    pub fn writing(&self) -> bool {
        matches!(self.state.as_str(), "taken" | "waiting" | "doing")
    }

    /// The turn ended without an answer the person can read: the model or transport failed, the
    /// person stopped it, the answer was cut off, or the agent refused.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(
            self.state.as_str(),
            "refused"
                | "provider_failed"
                | "provider_truncated"
                | "provider_cancelled"
                | "provider_refused"
        )
    }

    /// What happened, in one line, when the work failed.
    #[must_use]
    pub fn happened(&self) -> String {
        crate::i18n::t(match self.state.as_str() {
            "provider_cancelled" => "chat-agent-cancelled",
            "provider_truncated" => "chat-agent-truncated",
            "refused" | "provider_refused" => "chat-agent-refused",
            _ => "chat-agent-failed",
        })
    }
}

/// The message a reply answers, as the quote over it says it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Quote {
    /// The message it answers, after `chat.workspace/messages/`: what a press opens.
    #[serde(default)]
    pub id: String,
    /// Who wrote it.
    #[serde(default)]
    pub author: String,
    /// The start of what it said.
    #[serde(default)]
    pub excerpt: String,
}

/// Who reacted to a message with one emoji.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Reaction {
    /// The emoji.
    #[serde(default)]
    pub emoji: String,
    /// Who reacted with it.
    #[serde(default)]
    pub who: Vec<String>,
}

/// What a message carries: a record it was shared from, the words it quotes, or a file it posts.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Attachment {
    /// What it is: `resource`, `text`, `image`, `video` or `file`.
    #[serde(default)]
    pub kind: String,
    /// What a chip or a card says of it: a record's label, the words it quotes, a file's name.
    #[serde(default)]
    pub label: String,
    /// The resource it shows, when it shows one: what a press places.
    #[serde(default)]
    pub resource: Option<ResourceRef>,
    /// A picture's address, when it is one.
    #[serde(default)]
    pub src: String,
    /// What a picture shows, for assistive technology.
    #[serde(default)]
    pub alt: String,
    /// How big a file is, as a page says it ("1.4 MB"), when it names one.
    #[serde(default)]
    pub detail: String,
}

impl Channel {
    /// The resource a place names for this channel, by the id it is published under:
    /// `chat.workspace.channels:<id>` as a development session publishes it, and
    /// `chat.workspace/channels/<id>` as the chat's own record publishes the item — the very
    /// conversation Chat's own window shows and the one the canvas places when a person asks
    /// there (shell.md §8.1).
    #[must_use]
    pub fn resource(&self) -> ResourceRef {
        ResourceRef {
            id: format!("{}:{}", <Self as ViewResource>::KIND, self.id),
            ty: <Self as ViewResource>::KIND.to_owned(),
        }
    }

    /// Who wrote `author`, when the channel names them.
    #[must_use]
    pub fn person(&self, author: &str) -> Option<&Person> {
        self.people.iter().find(|person| person.id == author)
    }

    /// The message `id` says it answers, for the quote over a reply.
    #[must_use]
    pub fn quoted(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|message| message.id == id)
    }

    /// The resource of the message `id`, as the chat publishes its messages
    /// (`chat.workspace/messages/<id>`): what a row or a quote opens. There is none for a
    /// message with no id (a quote of one from an earlier part of the chat).
    #[must_use]
    pub fn message_resource(id: &str) -> Option<ResourceRef> {
        (!id.is_empty()).then(|| ResourceRef {
            id: format!("{MESSAGES_KIND}/messages/{id}"),
            ty: format!("{MESSAGES_KIND}.messages"),
        })
    }

    /// The page of the conversation `id` of `chat`, whose messages `rows` are — as the window
    /// projects them ([`crate::projection`]) — read by the person `me`: its name and topic, the
    /// people who spoke, and the messages it holds, each with what it answers, who reacted and
    /// what it carries.
    ///
    /// A conversation the record does not hold (a direct one nobody has written in yet) is
    /// named by the people it is with, as the window names it.
    ///
    /// [`Names`] completes what the record does not say: the reader's own name, and the
    /// workspace's agents by their id. Nobody is ever shown their routing id.
    #[must_use]
    pub fn read(
        chat: &crate::record::Chat,
        rows: &[crate::model::Message],
        id: &str,
        me: &str,
        names: &Names,
    ) -> Self {
        let channel_id = Id::from_raw(id);
        let channel = chat.conversation(&channel_id);
        let mut people: Vec<Person> = Vec::new();
        for row in rows {
            Self::named(&mut people, chat, names, me, &row.author);
        }
        let messages = rows
            .iter()
            .enumerate()
            .map(|(index, row)| Message {
                id: row.id.clone(),
                author: row.author.clone(),
                ts: row.ts,
                text: row.text.clone(),
                edited: row.edited,
                attempts: row.attempts,
                sending: row.sending,
                reply_to: row.parent.as_deref().and_then(|parent| {
                    // A reply that answers the message right above it needs no quote: the person
                    // reads what is answered just over the answer. A reply to anything else
                    // keeps its quote.
                    if index
                        .checked_sub(1)
                        .and_then(|before| rows.get(before))
                        .is_some_and(|before| before.id == parent)
                    {
                        return None;
                    }
                    rows.iter()
                        .find(|held| held.id == parent)
                        .map(|quoted| Quote {
                            id: quoted.id.clone(),
                            author: quoted.author.clone(),
                            excerpt: excerpt(&quoted.text),
                        })
                }),
                reactions: row
                    .reactions
                    .iter()
                    .map(|group| Reaction {
                        emoji: group.emoji.clone(),
                        who: group
                            .reactors
                            .iter()
                            .map(|reactor| reactor.by.clone())
                            .collect(),
                    })
                    .collect(),
                attachments: row.attachments.iter().map(Attachment::of).collect(),
                pinned: row.pinned,
                work: row.work.as_ref().map(work_of),
                // Retry is offered on the reply a person may ask again — the current attempt's,
                // which failed — and it names the message that reply answered.
                retry: retryable(rows, row),
                retry_summons: retryable(rows, row).then(|| row.parent.clone()).flatten(),
            })
            .collect();
        Self {
            id: id.to_owned(),
            // A channel keeps its name; a direct conversation is named by the people it is
            // with, as the window names it.
            name: match &channel {
                Some(channel) if channel.is_channel() && !channel.name.trim().is_empty() => {
                    channel.name.trim().to_owned()
                }
                _ => chat.display_names(&channel_id, Some(me), &names.agents),
            },
            topic: channel
                .as_ref()
                .map_or_else(String::new, |c| c.topic.clone()),
            voice: channel
                .as_ref()
                .is_some_and(|c| c.kind == crate::model::ChannelKind::Voice),
            people,
            messages,
        }
    }

    /// Add the person `author` to `people` when not already there, as they are named here: the
    /// record's roster first, then what the reading's [`Names`] say — the reader's own name,
    /// or the workspace's name for an agent — and otherwise Chat's honest placeholder for
    /// someone it cannot name. Never their routing id.
    fn named(
        people: &mut Vec<Person>,
        chat: &crate::record::Chat,
        names: &Names,
        me: &str,
        author: &str,
    ) {
        if people.iter().any(|held| held.id == author) {
            return;
        }
        let found = chat.people.iter().find(|person| person.id.0 == author);
        let agent = found.map_or_else(|| is_agent(author), |person| person.agent);
        let name = found
            .map(|person| person.name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| agent_name(chat, names, author))
            .or_else(|| (author == me).then(|| names.me.trim().to_owned()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| crate::identity::unnamed_label(author));
        people.push(Person {
            id: author.to_owned(),
            name,
            agent,
        });
    }
}

/// An agent's record of its work on a reply, as the page shows it: where the work is, and what
/// the agent said of it — what it is doing, or why it ended.
///
/// The words are the reply's own text while it streams and, once the turn ends, what the run
/// said became of it; the work's own detail is the provider's account of an end that failed, so
/// the page can say what happened without making the person open Chat's window.
fn work_of(work: &crate::model::AgentWork) -> Work {
    // A streamed end is the end: the record keeps the provider's own finish beside the state the
    // run wrote, and both say the same thing where both exist.
    let state = work
        .stream
        .as_ref()
        .and_then(|stream| stream.finish)
        .map_or_else(
            || match &work.state {
                crate::model::AgentWorkState::Taken => "taken",
                crate::model::AgentWorkState::Waiting { .. } => "waiting",
                crate::model::AgentWorkState::Doing { .. } => "doing",
                crate::model::AgentWorkState::Done { .. } => "done",
                crate::model::AgentWorkState::Refused => "refused",
                crate::model::AgentWorkState::ProviderFinished { outcome } => {
                    provider_state(*outcome)
                }
            },
            provider_state,
        )
        .to_owned();
    let detail = work
        .stream
        .as_ref()
        .map_or_else(String::new, |stream| stream.detail.clone());
    Work { state, detail }
}

/// The state a provider's finish is, spelled as [record-work](crate::messages) spells it.
fn provider_state(finish: crate::agent_stream::AgentFinish) -> &'static str {
    use crate::agent_stream::AgentFinish;
    match finish {
        AgentFinish::Complete => "provider_complete",
        AgentFinish::Truncated => "provider_truncated",
        AgentFinish::Refused => "provider_refused",
        AgentFinish::Cancelled => "provider_cancelled",
        AgentFinish::Failed => "provider_failed",
    }
}

/// Whether `row` is the reply a Retry asks the agent again: it is the agent's reply to the
/// message's current attempt (`chat`'s `record::reply_id_at`, derived from the summons, the agent
/// and the message's `attempts`), and its turn failed.
///
/// A retry moves the message to its next attempt, so the answer the agent writes then is a reply
/// of its own and this one stands as history — with no Retry to press a second time.
fn retryable(rows: &[crate::model::Message], row: &crate::model::Message) -> bool {
    let Some(work) = row.work.as_ref() else {
        return false;
    };
    let Some(summons) = row.parent.as_deref() else {
        return false;
    };
    let Some(asker) = rows.iter().find(|held| held.id == summons) else {
        return false;
    };
    if !work_of(work).failed() {
        return false;
    }
    row.id
        == crate::record::reply_id_at(
            &Id::from_raw(summons.to_owned()),
            &row.author,
            asker.attempts,
        )
        .to_string()
}

/// Whether `author` is an agent rather than a person: a participant spelled `agent:<id>`, or an
/// agent task spelled `agent-task:<task>`.
fn is_agent(author: &str) -> bool {
    author.starts_with("agent:") || author.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
}

/// The id an agent author is named by: the part after `agent:`.
fn agent_id(author: &str) -> Option<&str> {
    author.strip_prefix("agent:")
}

/// The name an agent author shows, as the record or the workspace names it: the name the
/// workspace keeps for it (Chat's own `agent_names`, or the workspace agents record the reading
/// carries), and otherwise what Chat calls an agent it has not been told the name of. An author
/// that is not an agent has no agent's name.
fn agent_name(chat: &crate::record::Chat, names: &Names, author: &str) -> Option<String> {
    if !is_agent(author) {
        return None;
    }
    chat.agent_names
        .get(author)
        .or_else(|| agent_id(author).and_then(|id| names.agents.get(id)))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            crate::i18n::ensure();
            Some(crate::i18n::t("chat-agent-kind"))
        })
}

/// The start of `text` a quote under a reply shows: its first line, cut short.
fn excerpt(text: &str) -> String {
    const SHORT: usize = 120;
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut cut: String = first.chars().take(SHORT).collect();
    if first.chars().count() > SHORT {
        cut.push('…');
    }
    cut
}

impl Attachment {
    /// What the message's attachment `attached` is, as its page draws it: a chip for a record,
    /// the words for a quote, a card for a file.
    #[must_use]
    pub fn of(attached: &tilo_reference::Attached) -> Self {
        use tilo_reference::Attached as Held;
        match attached {
            Held::Resource { at, label, .. } => Self {
                kind: "resource".to_owned(),
                label: label.clone(),
                resource: Some(ResourceRef {
                    id: at.reference.id.clone(),
                    ty: at.reference.kind.clone(),
                }),
                ..Self::default()
            },
            Held::Text { text, .. } => Self {
                kind: "text".to_owned(),
                label: excerpt(text),
                detail: String::new(),
                ..Self::default()
            },
            Held::Image { name, blob, .. } => Self {
                kind: "image".to_owned(),
                label: name.clone(),
                src: blob.clone(),
                alt: name.clone(),
                ..Self::default()
            },
            Held::Video { name, size, .. } | Held::File { name, size, .. } => Self {
                kind: if matches!(attached, Held::Video { .. }) {
                    "video".to_owned()
                } else {
                    "file".to_owned()
                },
                label: name.clone(),
                detail: sdk::format::size(*size),
                ..Self::default()
            },
        }
    }
}

impl ViewResource for Channel {
    // The item of Chat's `channels` collection, as its record publishes it
    // (`chat.workspace/channels/<id>`): the conversation a place shows, and the one the canvas
    // places when a person asks there.
    const KIND: &'static str = "chat.workspace.channels";
    const APP: &'static str = "chat";
}

/// The kind of the chat's messages record, as its own items are published under it: a message's
/// row and a reply's quote open `chat.workspace/messages/<id>`.
const MESSAGES_KIND: &str = "chat.workspace";

/// The channel's page: what it is called and about, its messages filling the column, and the
/// composer at its foot — a conversation above the box that answers it.
///
/// The page is the window's own body: a place draws it as it stands here, with the guttered,
/// readable column every page sets its blocks on (`page`), the transcript that scrolls at that
/// measure and follows its newest line, and the composer in the same column at the foot, as
/// Chat's own window draws them. A place that is narrow, or a window shown in another theme,
/// draws the same page: nothing here knows a width or a look.
#[view(default)]
fn page(c: &Channel, cx: &ViewCx) -> Node {
    // The column is a *filling stack*, not a nested page: a page inside another page (this one
    // is drawn within the place's own layout, and within the desktop's page) is normalized to a
    // plain stack whose props — `fill` and the `page` gutter among them — are dropped. A nested
    // page would leave the column hugging its content, the transcript growing past its room
    // under the header, and the topic line jammed on the transcript's first row. A filling
    // stack keeps both the fill it asks for and the gap it spaces its children by.
    let head = ui::page(
        &cx.key("head"),
        vec![ui::stack(
            &cx.key("header"),
            Gap::Tight,
            header_facts(c, cx),
        )],
    );
    let column = ui::fill(ui::stack(
        &cx.key("column"),
        Gap::Section,
        vec![head, ui::fill(transcript(c, cx))],
    ));
    // The composer stands docked at the foot, in the page's own gutter and measure, as every
    // conversation sets its reply box under the messages it answers (`ui.Docked`): the
    // transcript fills the column above it and scrolls, the composer keeps its own height.
    let docked: Node = ui::DockedBuilder::new(&cx.key("docked")).into();
    let docked = node::slot(docked, "foot", vec![composer(c, cx)]);
    let docked = node::with(docked, vec![column]);
    // The page is the window's whole body: it fills the height the place gives it, its column
    // takes the width, and the transcript scrolls inside — never the page scrolling away with
    // the composer under it.
    ui::fill(ui::page(&cx.key("page"), vec![ui::fill(docked)]))
}

/// Who the channel is: its name, and what it is about or that it is a call.
fn header_facts(c: &Channel, cx: &ViewCx) -> Vec<Node> {
    let mut facts: Vec<Node> = vec![
        ui::TextBuilder::new(&cx.key("name"), format!("#{}", c.name))
            .role(ui::TextRole::Title)
            .into(),
    ];
    let caption = if c.voice {
        crate::i18n::t("chat-voice-channel")
    } else {
        c.topic.trim().to_owned()
    };
    if !caption.is_empty() {
        facts.push(
            ui::TextBuilder::new(&cx.key("topic"), caption)
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
        );
    }
    facts
}

/// The channel's messages, oldest first, each a row saying who spoke, when, and what.
///
/// A transcript's rows are the ones every conversation draws: a day's divider over the first
/// message of a local day, and a run of one person's consecutive messages under their avatar and
/// name once, the time beside it. The day is the toolkit's (`ui.SectionDivider`), the avatar
/// (`ui.Avatar`), the clock the person's (`ui.Time`, read in their language and zone), and the
/// grouping the run's own rule ([`runs`]): a message continues the run above it when one person
/// wrote it within [`RUN_WINDOW_SECS`] of the one before, on the same local day.
fn transcript(c: &Channel, cx: &ViewCx) -> Node {
    // A conversation scrolls its transcript, opening at the newest message and following it as
    // it grows. It fills the room the page leaves it, so the composer stays at the foot and the
    // transcript is what scrolls.
    let scroll = ui::ScrollBuilder::new(&cx.key("transcript"))
        .follow_end(true)
        .into();
    if c.messages.is_empty() {
        return ui::fill(node::with(
            scroll,
            vec![ui::page(&cx.key("empty-page"), vec![empty(c, cx)])],
        ));
    }
    // The rows, in order: a day's divider, then each message with what it answers and says. A
    // run of one person's messages is one group, so the column's gap stands between runs — a
    // section's — and the rows inside a run meet tightly, the way every conversation draws
    // them: a person's name never sits jammed against what was said before it.
    let clock = ConversationClock::local();
    let grouped = grouped_messages(c, &clock);
    let mut rows: Vec<Node> = Vec::new();
    let mut run: Vec<Node> = Vec::new();
    let flush = |run: &mut Vec<Node>, rows: &mut Vec<Node>| {
        if !run.is_empty() {
            rows.push(ui::stack(
                &cx.key(format!("run-{}", rows.len())),
                Gap::None,
                std::mem::take(run),
            ));
        }
    };
    for (index, (day, message, continues)) in grouped.iter().enumerate() {
        if let Some(label) = day {
            flush(&mut run, &mut rows);
            rows.push(
                ui::SectionDividerBuilder::new(&cx.key(format!("day-{index}")))
                    .label(label.clone())
                    .into(),
            );
        }
        if !*continues {
            flush(&mut run, &mut rows);
        }
        run.push(message_row(c, message, *continues, cx));
    }
    flush(&mut run, &mut rows);
    // The rows are one page's column: the page sets their measure and gutter, and the column's
    // sibling gap stands between one run and the next, and around a day's divider.
    let column = ui::stack(&cx.key("messages"), Gap::Section, rows);
    node::with(
        ui::fill(node::with(
            scroll,
            vec![ui::page(&cx.key("rows-page"), vec![column])],
        )),
        vec![],
    )
}

/// What the transcript shows of the channel, in order: each message, whether a day begins over
/// it (its label, when one does), and whether it continues the run above it.
///
/// A run is one person's consecutive messages within [`RUN_WINDOW_SECS`] of one another on one
/// local day, so the avatar, the name and the time stand over the run's first message and the
/// others line up under it, as every conversation draws them. A day's divider stands over the
/// first message of a local day, never otherwise.
fn grouped_messages<'a>(
    c: &'a Channel,
    clock: &ConversationClock,
) -> Vec<(Option<String>, &'a Message, bool)> {
    let mut breaks = DayBreaks::new();
    let mut out: Vec<(Option<String>, &Message, bool)> = Vec::with_capacity(c.messages.len());
    let mut previous: Option<&Message> = None;
    for message in &c.messages {
        let day = clock.day(message.ts);
        let label = breaks.begins(day).then(|| clock.day_label(day));
        let continues = label.is_none()
            && previous.is_some_and(|before| {
                before.author == message.author
                    && message.ts.saturating_sub(before.ts) <= RUN_WINDOW_SECS
            });
        out.push((label, message, continues));
        previous = Some(message);
    }
    out
}

/// What a channel says while it holds no message: a word about the kind of conversation, or an
/// invitation to start it.
fn empty(c: &Channel, cx: &ViewCx) -> Node {
    if c.voice {
        ui::EmptyStateBuilder::new(&cx.key("empty"), "No one has joined this call yet")
            .icon("phone")
            .into()
    } else if c.topic.trim().is_empty() {
        ui::EmptyStateBuilder::new(&cx.key("empty"), "No messages yet")
            .icon("message-circle")
            .into()
    } else {
        ui::EmptyStateBuilder::new(&cx.key("empty"), "No messages yet")
            .message(c.topic.trim())
            .icon("message-circle")
            .into()
    }
}

/// One message: the avatar and the name with the time over a run's first message, then the
/// message it answers, what it says, the reactions on it and what it carries. A row that
/// continues the run above it leaves the avatar's room empty and starts at what it says.
///
/// The row is a page row: its author's name, and what it says, stand at the page's measure, and
/// the message's own menu, toolbar and swipe are what its type offers.
fn message_row(c: &Channel, message: &Message, continues: bool, cx: &ViewCx) -> Node {
    let author = c.person(&message.author);
    let said = message_said(c, message, cx);

    // The avatar and the name with the time, over a run's first message. A run's next message
    // keeps the avatar's room, empty, so a person's messages line up under their name once.
    let avatar = ui::AvatarBuilder::new(&cx.key(format!("avatar-{}", message.id)))
        .who(principal(author, &message.author))
        .square(false)
        .into();
    let leading: Node = if continues {
        // An avatar's room, empty: the toolkit's own (a person's face drawn with nothing in
        // it), so a message that continues a run lines up under the first one's avatar.
        ui::AvatarBuilder::new(&cx.key(format!("lead-{}", message.id)))
            .who(principal(author, &message.author))
            .room(true)
            .into()
    } else {
        avatar
    };
    let head: Vec<Node> = if continues {
        Vec::new()
    } else {
        let mut head = vec![
            ui::TextBuilder::new(
                &cx.key(format!("author-{}", message.id)),
                author_label(author, &message.author),
            )
            .role(ui::TextRole::Label)
            .into(),
        ];
        let when = cx.key(format!("when-{}", message.id));
        head.push(if message.sending {
            ui::TextBuilder::new(&when, crate::i18n::t("chat-message-sending"))
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into()
        } else {
            ui::TimeBuilder::new(&when, moment(message.ts))
                .role(ui::TextRole::Caption)
                .secondary(true)
                .past(true)
                // A message's line says when it was sent, by the clock the person reads
                // ("5:16 AM"); the day it falls on is the divider over its run.
                .clock(true)
                .into()
        });
        vec![ui::row(
            &cx.key(format!("by-{}", message.id)),
            Gap::Tight,
            head,
        )]
    };
    let mut column: Vec<Node> = head;
    column.push(said);
    let content = ui::stack(
        &cx.key(format!("content-{}", message.id)),
        Gap::Tight,
        column,
    );

    // What a message offers on hover: answer it, as Chat's own window shows the toolbar over a
    // message. It is Chat's own `post` with the message as the reply's parent, through the
    // composer's reply banner: the view's state holds which message the box answers.
    let bar = hover_actions(c, message, cx);
    let lined = ui::row(
        &cx.key(format!("message-{}", message.id)),
        Gap::Related,
        vec![leading, ui::fill(content)],
    );
    // The avatar stands at the top of the row, level with the author line: left to centre it,
    // the avatar of a message that carries a quote and a long answer would stand halfway down
    // the message, beside the words instead of the name — the very defect a person's own
    // avatar never shows because their message is one line.
    let lined = ui::arg(lined, "align", ui::text("start"));
    let body = node::with(lined, vec![bar]);
    // Activating the message places it: its own resource, which is the system's open.
    match Channel::message_resource(&message.id) {
        Some(target) => {
            let open = cx.open(&body, "activate", target, None);
            node::on(body, "activate", open)
        }
        None => body,
    }
}

/// What a message says and carries, in the order a conversation reads it: the message it answers
/// (a quote), that it is pinned, its words, its attachments, that it was edited, and the
/// reactions on it — one column under the byline.
fn message_said(c: &Channel, message: &Message, cx: &ViewCx) -> Node {
    let mut body: Vec<Node> = Vec::new();
    if let Some(work) = &message.work
        && work.writing()
    {
        // A reply still being written says so, quietly, in place: the words stream into the
        // very message they are, so the row grows and the transcript stays where it is.
        body.push(writing_row(message, work, cx));
    }
    if let Some(quote) = &message.reply_to {
        body.push(reply_quote(
            c,
            quote,
            &cx.key(format!("quote-{}", message.id)),
            cx,
        ));
    }
    if message.pinned {
        body.push(
            ui::TextBuilder::new(&cx.key(format!("pinned-{}", message.id)), "Pinned")
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
        );
    }
    if !message.text.trim().is_empty() {
        body.push(
            ui::TextBuilder::new(&cx.key(format!("text-{}", message.id)), message.text.trim())
                .role(ui::TextRole::Body)
                .into(),
        );
    }
    for (index, attachment) in message.attachments.iter().enumerate() {
        body.push(attachment_row(
            attachment,
            &cx.key(format!("attachment-{}-{index}", message.id)),
            cx,
        ));
    }
    if message.edited {
        body.push(
            ui::TextBuilder::new(&cx.key(format!("edited-{}", message.id)), "edited")
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
        );
    }
    if !message.reactions.is_empty() {
        body.push(reactions(
            c,
            message,
            &cx.key(format!("reactions-{}", message.id)),
        ));
    }
    if let Some(work) = &message.work
        && work.failed()
    {
        // A turn that ended without an answer the person can read says what happened where it
        // happened, with a Retry that asks the agent again on the same message.
        body.push(failed_row(message, work, cx));
    }
    ui::stack(&cx.key(format!("said-{}", message.id)), Gap::Tight, body)
}

/// The quiet line under a reply still being written (`ui.Spinner` beside a caption), naming what
/// is under way in the agent's words when it said any.
///
/// It stands over the words rather than replacing them: the streamed text is the reply's own,
/// so nobody's answer jumps — the line grows in place, at the page's measure, and leaves when
/// the turn ends.
fn writing_row(message: &Message, work: &Work, cx: &ViewCx) -> Node {
    let label = if work.detail.trim().is_empty() {
        "Writing…".to_owned()
    } else {
        work.detail.trim().to_owned()
    };
    let spinner: Node = ui::SpinnerBuilder::new(&cx.key(format!("writing-spinner-{}", message.id)))
        .label(label.clone())
        .beside(ui::TextRole::Caption)
        .into();
    let say: Node = ui::TextBuilder::new(&cx.key(format!("writing-{}", message.id)), &label)
        .role(ui::TextRole::Caption)
        .secondary(true)
        .into();
    ui::row(
        &cx.key(format!("writing-row-{}", message.id)),
        Gap::Tight,
        vec![spinner, say],
    )
}

/// The row a failed turn leaves in the conversation: what happened, why when the run said why,
/// and a Retry that asks the agent again on the same message.
///
/// It is a banner (`ui.Banner`, kind `error`), so it reads and is announced as the failure it is,
/// and its Retry is the conversation's own `retry` action — the same verb the REPL calls — with
/// the reply it retries. The row stays where it happened: the turn's place in the transcript is
/// the evidence that the person was not ignored.
fn failed_row(message: &Message, work: &Work, cx: &ViewCx) -> Node {
    let mut banner =
        ui::BannerBuilder::new(&cx.key(format!("failed-{}", message.id)), work.happened())
            .kind(ui::BannerKind::Error)
            .icon("circle-alert");
    let reason = work.detail.trim();
    if !reason.is_empty() {
        banner = banner.message(reason);
    }
    if message.retry {
        banner = banner.action(crate::i18n::t("chat-retry"));
    }
    let banner: Node = banner.into();
    if let Some(summons) = message.retry_summons.clone() {
        let fired = cx.action_with(
            &banner,
            "act",
            "retry",
            vec![Arg::text(message.id.clone()), Arg::text(summons)],
        );
        node::on(banner, "act", fired)
    } else {
        banner
    }
}

/// What a message offers while the pointer is over it or it holds focus, over its trailing
/// edge: answer it.
///
/// The reply is the view's own state ([`REPLYING`]): a press names the message the composer
/// answers, which the box at the foot reads, and the next message posted is its reply. The
/// message's other verbs (pin, react) are what its type offers on its own row, which a place of
/// the message draws; the page does not repeat them here.
fn hover_actions(_c: &Channel, message: &Message, cx: &ViewCx) -> Node {
    let reply: Node = ui::ButtonBuilder::new(&cx.key(format!("reply-{}", message.id)), "Reply")
        .icon("reply")
        .icon_only(true)
        .role(ui::ButtonRole::Plain)
        .into();
    let fired = cx.set(&reply, "press", REPLYING);
    let reply = node::on(reply, "press", fired);
    node::with(
        ui::HoverToolbarBuilder::new(&cx.key(format!("actions-{}", message.id))).into(),
        vec![reply],
    )
}

/// What a reply answers: who wrote it and the start of what it said, as the designed quote over
/// the reply (`ui.Quote`), naming who it answers and what they said. The message the reply is in
/// opens on activation, as every message does.
fn reply_quote(c: &Channel, quote: &Quote, key: &str, _cx: &ViewCx) -> Node {
    let author = c.person(&quote.author);
    let excerpt = if quote.excerpt.trim().is_empty() {
        // A message the page does not hold (one from an earlier part of the chat) still says
        // it quotes one, so the reply does not read as answering nothing.
        crate::i18n::t("chat-quote-earlier")
    } else {
        quote.excerpt.trim().to_owned()
    };
    ui::QuoteBuilder::new(key, &excerpt)
        .caption(crate::i18n::t("chat-quote-caption"))
        .author(principal(author, &quote.author))
        .into()
}

/// One thing a message carries: a record it was shared from (a row that places it), the words
/// it quotes, a picture, or a file it posts (a card naming it).
fn attachment_row(attachment: &Attachment, key: &str, cx: &ViewCx) -> Node {
    if let Some(target) = &attachment.resource {
        // A record is a reference: a row that shows it and places it when activated, which is
        // the system's open (shell.md §3) — Chat does not handle the press itself.
        let row: Node = ui::ListRowBuilder::new(key)
            .title(&attachment.label)
            .icon("corner-up-right")
            .item(target.clone())
            .dense(true)
            .into();
        let opened = cx.open(&row, "activate", target.clone(), None);
        return node::on(row, "activate", opened);
    }
    match attachment.kind.as_str() {
        "image" if !attachment.src.is_empty() => {
            ui::ImageBuilder::new(key, &attachment.src, &attachment.alt)
                .fit(ui::ImageFit::Contain)
                .into()
        }
        // A file or quoted words: a row naming what it is and how big, quiet under the message.
        _ => {
            let row: Node = ui::ListRowBuilder::new(key)
                .title(&attachment.label)
                .icon(if attachment.kind == "text" {
                    "quote"
                } else {
                    "paperclip"
                })
                .dense(true)
                .into();
            match attachment.detail.trim() {
                "" => row,
                detail => ui::put(row, "secondary", Value::Text(detail.to_owned())),
            }
        }
    }
}

/// The reactions on a message: a pill per emoji, with how many, the person's own drawn on.
fn reactions(c: &Channel, message: &Message, key: &str) -> Node {
    let emoji: Vec<String> = message
        .reactions
        .iter()
        .map(|reaction| reaction.emoji.clone())
        .collect();
    let by: Vec<Vec<tilo_view::Principal>> = message
        .reactions
        .iter()
        .map(|reaction| {
            reaction
                .who
                .iter()
                .map(|id| principal(c.person(id), id))
                .collect()
        })
        .collect();
    ui::ReactionsBuilder::new(key).emoji(emoji).by(by).into()
}

/// The foot of the conversation: the box a message is typed in, and the button that sends it.
///
/// The field is the composer's own draft, and what it holds is the `text` argument of Chat's own
/// `post` action: Enter sends what the field carries, and the button sends the draft the field is
/// bound to, so a press and an agent's step take the one path to the record.
fn composer(c: &Channel, cx: &ViewCx) -> Node {
    let field: Node = ui::TextInputBuilder::new(&cx.key("draft"))
        .placeholder(crate::i18n::t_args(
            "chat-channel-composer-placeholder",
            &[("channel", c.name.as_str().into())],
        ))
        .lines(1_i64)
        .entry(true)
        .into();
    // The field is its own draft: what is typed is the field's, and the entry lets go of it
    // once it submits, so the next message starts from an empty box.
    let input = node::bind(field, ViewCx::bind_state(DRAFT));
    // Enter hands the action the text the field carries as it submits.
    let input = post(cx, input, "submit", &c.id, Arg::Event);
    // The field takes everything the buttons leave, so the two read as one full-width control.
    let input = ui::fill(input);
    // The attach button: raises the system's own file chooser for the conversation (Chat's
    // `attach-file` effect with no path), whose files become the message's attachments.
    let attach: Node = ui::ButtonBuilder::new(&cx.key("attach"), crate::i18n::t("chat-attach"))
        .icon("paperclip")
        .icon_only(true)
        .role(ui::ButtonRole::Plain)
        .into();
    let fired = cx.action(&attach, "press", "attach-file");
    let attach = node::on(attach, "press", fired);
    // The button carries no text of its own: it sends the draft the field is bound to.
    let send: Node = ui::ButtonBuilder::new(&cx.key("send"), crate::i18n::t("chat-send"))
        .icon("send")
        .icon_only(true)
        .into();
    let send = post(cx, send, "press", &c.id, Arg::state(DRAFT));
    ui::row(&cx.key("composer"), Gap::Tight, vec![attach, input, send])
}

/// The name the composer's draft is held under in the view's own state.
const DRAFT: &str = "Composer";

/// The name the message being answered is held under in the view's own state: a message's Reply
/// button sets it, and the composer reads it as the next message's parent.
const REPLYING: &str = "Replying";

/// `node` with its event `event` running the chat's `post` of channel `channel`, with the text
/// `text`: what the composer's field and its button both do.
fn post(cx: &ViewCx, node: Node, event: &str, channel: &str, text: Arg) -> Node {
    let fired = cx.action_with(&node, event, "post", vec![Arg::text(channel), text]);
    node::on(node, event, fired)
}

/// What a message's byline says of who wrote it: their name, or their id when the channel does
/// not name them.
fn author_label(person: Option<&Person>, id: &str) -> String {
    person
        .map(|person| person.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| crate::identity::unnamed_label(id))
}

/// A person as a component draws them: their name, or Chat's honest placeholder for someone it
/// cannot name, never their id.
fn principal(person: Option<&Person>, id: &str) -> tilo_view::Principal {
    match person {
        Some(person) => tilo_view::Principal {
            id: person.id.clone(),
            name: person.name.trim().to_owned(),
        },
        None => tilo_view::Principal {
            id: id.to_owned(),
            name: crate::identity::unnamed_label(id),
        },
    }
}

/// A moment as a component's `time` takes it: RFC 3339 in UTC, as `Item::time` and `ui.Time`
/// read it.
fn moment(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map_or_else(String::new, |at| {
        at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel page over the demo workspace's `#design-crit`: what a place showing it draws.
    fn demo_channel() -> Channel {
        let workspace = crate::demo::Workspace::demo().expect("the demo workspace");
        let rows = workspace.message_rows("c_crit");
        Channel::read(
            &workspace.read(),
            &rows,
            "c_crit",
            crate::model::ME,
            &Names::default(),
        )
    }

    fn view_cx(channel: &Channel) -> ViewCx {
        ViewCx::new(
            channel.resource(),
            "page",
            tilo_view::Principal {
                id: crate::model::ME.to_owned(),
                name: "Pat".to_owned(),
            },
            std::collections::BTreeMap::new(),
        )
    }

    /// Every text a tree shows, in its props.
    fn texts(node: &Node) -> Vec<String> {
        let mut out: Vec<String> = node
            .props
            .values()
            .filter_map(|value| match value {
                Value::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        for child in &node.children {
            out.extend(texts(child));
        }
        out
    }

    /// Every node of the tree, in order.
    fn walk(node: &Node) -> Vec<&Node> {
        let mut out = vec![node];
        for child in &node.children {
            out.extend(walk(child));
        }
        out
    }

    /// Whether the tree holds a node of `kind`.
    fn has_kind(node: &Node, kind: &str) -> bool {
        node.kind == kind || node.children.iter().any(|child| has_kind(child, kind))
    }

    /// Whether every node passes the catalogue check the host applies.
    fn passes(tree: &Node) {
        let validator = tilo_view::patch::validate::Validator::new(
            tilo_view::node::catalogue(),
            tilo_view::patch::validate::PatchLimits::default(),
        );
        let batch = vec![tilo_view::patch::ToUi::Open {
            lane: tilo_view::patch::Lane::Root,
            target: ResourceRef {
                id: "chat.workspace/channels/c_crit".to_owned(),
                ty: "chat.workspace.channels".to_owned(),
            },
            tree: tree.clone(),
            front: None,
        }];
        validator
            .to_ui(&batch)
            .expect("the view is one the host builds");
    }

    #[test]
    fn the_page_draws_the_channels_name_and_its_messages() {
        let channel = demo_channel();
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let shown = texts(&tree);
        assert!(
            shown
                .iter()
                .any(|text| text == &format!("#{}", channel.name)),
            "the page names the channel: {shown:?}"
        );
        for message in &channel.messages {
            if message.text.trim().is_empty() {
                continue;
            }
            assert!(
                shown.iter().any(|text| text.trim() == message.text.trim()),
                "the page shows `{}`: {shown:?}",
                message.text
            );
        }
        assert!(
            has_kind(&tree, "ui.Scroll"),
            "the messages are a scrolling transcript"
        );
        for message in &channel.messages {
            if message.text.trim().is_empty() {
                continue;
            }
            assert!(
                shown.iter().any(|text| text.trim() == message.text.trim()),
                "the page shows `{}`: {shown:?}",
                message.text
            );
        }
    }

    /// Every node of the tree whose key holds `suffix` and whose kind is `kind`.
    fn keyed<'a>(tree: &'a Node, kind: &str, suffix: &str) -> Vec<&'a Node> {
        walk(tree)
            .into_iter()
            .filter(|node| node.kind == kind && node.key.contains(suffix))
            .collect()
    }

    /// A message of `channel` written by `author` at `ts`, as the record's projection reads one.
    fn row(channel: &str, id: &str, author: &str, ts: i64, text: &str) -> crate::model::Message {
        crate::model::Message {
            id: id.to_owned(),
            channel: channel.to_owned(),
            author: author.to_owned(),
            ts,
            text: text.to_owned(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            reactions: Vec::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            pinned: false,
            work: None,
            appearance: None,
            share: None,
            attempts: 0,
            sending: false,
        }
    }

    /// A channel of a record that knows nobody: a real session's ask conversation, whose record
    /// was created by the ask and never introduced to its reader or its agent, with one message
    /// from the person seated here under their root key and one from the agent answering them.
    fn unacquainted_channel(me: &str, agent: &str) -> Channel {
        let mut chat = crate::record::Chat::default();
        chat.open_channel(
            Id::from_raw("c_ask"),
            crate::record::Conversation {
                name: "Assistant".to_owned(),
                participants: vec![
                    crate::record::Participant::person(me).atom(),
                    crate::record::Participant::agent(agent).atom(),
                ],
                ..crate::record::Conversation::default()
            },
        );
        let rows = vec![
            row("c_ask", "m_ask", me, 1_782_470_400, "hello"),
            row(
                "c_ask",
                "m_reply",
                &format!("agent:{agent}"),
                1_782_470_460,
                "Hi",
            ),
        ];
        let mut names = Names {
            me: "Quinn Davis".to_owned(),
            ..Names::default()
        };
        names
            .agents
            .insert(agent.to_owned(), "Assistant".to_owned());
        Channel::read(&chat, &rows, "c_ask", me, &names)
    }

    #[test]
    fn a_real_sessions_person_is_named_never_shown_their_id() {
        // A session's Chat record is the ask's, and the ask never told it who reads it: the
        // person's message is authored by their root key, a 64-hex routing id. The page draws
        // the name this device shows them — "quinn davis" — never that key.
        const ME: &str = "ba944082ab57230fec2b518bd058a677d056dc648ac5c766f392f5f69fc1ee8c";
        let channel = unacquainted_channel(ME, "assistant");
        // The reading itself names them: their avatar and byline carry the name, not the key.
        let person = channel
            .person(ME)
            .expect("the author is in the channel's people");
        assert_eq!(person.name, "Quinn Davis", "the person is named");
        assert!(!person.name.contains(ME), "never their routing id");
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let authors = keyed(&tree, "ui.Text", "@author-");
        let shown: Vec<String> = authors
            .iter()
            .flat_map(|node| texts(node))
            .filter(|text| !text.trim().is_empty())
            .collect();
        assert!(
            shown.iter().any(|name| name == "Quinn Davis"),
            "the byline is the person's name: {shown:?}"
        );
        assert!(
            !shown.iter().any(|name| name.contains(ME)),
            "no byline shows the routing id: {shown:?}"
        );
        // Their avatar's letters are their initials, and it is called by their name, never by
        // the id: the very face the top bar draws.
        let avatar = keyed(&tree, "ui.Avatar", "@avatar-")
            .into_iter()
            .next()
            .expect("the person's avatar");
        assert_eq!(
            avatar.props.get("who"),
            Some(&Value::Principal(tilo_view::Principal {
                id: ME.to_owned(),
                name: "Quinn Davis".to_owned(),
            })),
            "the avatar is drawn for the person by name, not by id"
        );
    }

    #[test]
    fn an_agents_answer_is_signed_with_its_name() {
        // An agent's reply is authored `agent:<id>`: the page draws the agent's name, the one
        // the workspace keeps for it, never its id.
        const ME: &str = "ba944082ab57230fec2b518bd058a677d056dc648ac5c766f392f5f69fc1ee8c";
        let channel = unacquainted_channel(ME, "assistant");
        let agent = channel
            .person("agent:assistant")
            .expect("the agent is in the channel's people");
        assert!(agent.agent, "the author is an agent");
        assert_eq!(agent.name, "Assistant", "the agent is named");
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let shown: Vec<String> = keyed(&tree, "ui.Text", "@author-")
            .iter()
            .flat_map(|node| texts(node))
            .filter(|text| !text.trim().is_empty())
            .collect();
        assert!(
            shown.iter().any(|name| name == "Assistant"),
            "the agent's byline is its name: {shown:?}"
        );
        assert!(
            !shown.iter().any(|name| name.contains("agent:")),
            "no byline shows the agent's id: {shown:?}"
        );
    }

    #[test]
    fn a_person_the_record_cannot_name_is_never_shown_their_id() {
        // Someone neither the record nor the reading's names know: the page says the honest
        // placeholder Chat uses for an unnamed person, never the routing key it holds.
        const STRANGER: &str = "f00dfeedf00dfeedf00dfeedf00dfeedf00dfeedf00dfeedf00dfeedf00dfeed";
        let mut chat = crate::record::Chat::default();
        chat.open_channel(
            Id::from_raw("c_ask"),
            crate::record::Conversation {
                name: "Assistant".to_owned(),
                ..crate::record::Conversation::default()
            },
        );
        let rows = vec![row("c_ask", "m_ask", STRANGER, 1_782_470_400, "hello")];
        let channel = Channel::read(&chat, &rows, "c_ask", crate::model::ME, &Names::default());
        let person = channel.person(STRANGER).expect("the author is named");
        assert!(
            !person.name.contains(STRANGER),
            "the placeholder never spells the key: {}",
            person.name
        );
        assert!(
            person.name.contains("eed"),
            "the placeholder names them by the last of their key: {}",
            person.name
        );
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let shown: Vec<String> = keyed(&tree, "ui.Text", "@author-")
            .iter()
            .flat_map(|node| texts(node))
            .collect();
        assert!(
            !shown.iter().any(|name| name.contains(STRANGER)),
            "no byline shows the routing key: {shown:?}"
        );
    }

    #[test]
    fn each_message_says_who_wrote_it_and_when() {
        let channel = demo_channel();
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        // A run's first message leads with its author's name, drawn from the channel's people,
        // never their id; a message continuing the run leaves the name to the run's first.
        let clock = ConversationClock::local();
        let grouped = grouped_messages(&channel, &clock);
        let starts: Vec<&Message> = grouped
            .iter()
            .filter(|(_, _, continues)| !continues)
            .map(|(_, message, _)| *message)
            .collect();
        let named = keyed(&tree, "ui.Text", "@author-");
        assert_eq!(
            named.len(),
            starts.len(),
            "every run names its author once, and only a run's first"
        );
        let authors: Vec<String> = named
            .iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(name)) => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(
            authors.iter().any(|name| name == "Aria Chen"),
            "the demo's people are named: {authors:?}"
        );
        assert!(
            !authors.iter().any(|name| name.starts_with("u_")),
            "no author shows as their id: {authors:?}"
        );
        // Every message says when it was written through `ui.Time`, whose `at` is the message's
        // own instant: the component reads it in the person's language and zone, never as a raw
        // ISO stamp.
        let when = keyed(&tree, "ui.Time", "@when-");
        assert_eq!(
            when.len(),
            starts.len(),
            "every run's first message carries its time"
        );
        for node in &when {
            // The moment the time component reads is the message's own instant, as RFC 3339
            // (`ui.Time` renders it in the person's language and zone, never as this stamp).
            let at = node.props.get("at").expect("a time says its moment");
            let Value::Text(at) = at else {
                panic!("a time's `at` is text");
            };
            assert!(
                chrono::DateTime::parse_from_rfc3339(at).is_ok(),
                "a time's `at` is an instant: {at}"
            );
        }
    }

    #[test]
    fn a_day_divider_stands_between_two_days() {
        // Two messages a day apart: the transcript marks the second day with a divider, the
        // toolkit's own, labelled as the person reads a day.
        let mut channel = demo_channel();
        channel.messages = vec![
            Message {
                id: "m_day_one".to_owned(),
                author: crate::model::ME.to_owned(),
                ts: 1_782_470_400, // 2026-06-30
                text: "First day".to_owned(),
                ..Message::default()
            },
            Message {
                id: "m_day_two".to_owned(),
                author: crate::model::ME.to_owned(),
                ts: 1_782_470_400 + 86_400, // the day after
                text: "Second day".to_owned(),
                ..Message::default()
            },
        ];
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let dividers = keyed(&tree, "ui.SectionDivider", "@day-");
        assert_eq!(
            dividers.len(),
            2,
            "a divider stands over the first message of each of two days"
        );
        for divider in dividers {
            let Value::Text(label) = divider.props.get("label").expect("a divider names its day")
            else {
                panic!("a day's label is text");
            };
            assert!(!label.is_empty(), "a divider names its day");
        }
    }

    #[test]
    fn a_run_of_one_person_names_them_once() {
        // Two messages from one person within the run window: one avatar-and-name run, so the
        // second message leaves the name to the first, as every conversation draws a run.
        let mut channel = demo_channel();
        channel.messages = vec![
            Message {
                id: "m_a".to_owned(),
                author: crate::model::ME.to_owned(),
                ts: 1_782_470_400,
                text: "One".to_owned(),
                ..Message::default()
            },
            Message {
                id: "m_b".to_owned(),
                author: crate::model::ME.to_owned(),
                ts: 1_782_470_400 + 60,
                text: "Two".to_owned(),
                ..Message::default()
            },
        ];
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        let clock = ConversationClock::local();
        let grouped = grouped_messages(&channel, &clock);
        assert_eq!(
            grouped
                .iter()
                .filter(|(_, _, continues)| !continues)
                .count(),
            1,
            "one run of two consecutive messages from one person"
        );
        assert_eq!(
            keyed(&tree, "ui.Avatar", "@avatar-").len() + keyed(&tree, "ui.Avatar", "@lead-").len(),
            2,
            "each message has an avatar (or its empty lead), the run's first their face, the next a lead"
        );
        // The run's second message draws an empty lead: an avatar's room with nothing in it.
        let lead = keyed(&tree, "ui.Avatar", "@lead-");
        assert_eq!(lead.len(), 1, "the run's second message has an empty lead");
        assert_eq!(
            lead[0].props.get("room"),
            Some(&Value::Bool(true)),
            "the lead is an avatar's room, empty"
        );
    }

    #[test]
    fn the_composer_is_full_width_with_attach_and_send() {
        let channel = demo_channel();
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let composer = walk(&tree)
            .into_iter()
            .find(|node| node.key.contains("@composer"))
            .expect("the composer's row");
        // The composer's row holds an attach button, the field and its send button, in order.
        let buttons: Vec<&Node> = composer
            .children
            .iter()
            .filter(|node| node.kind == "ui.Button")
            .collect();
        let labels: Vec<String> = buttons
            .iter()
            .filter_map(|node| match node.props.get("label") {
                Some(Value::Text(label)) => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels
                .iter()
                .any(|label| *label == crate::i18n::t("chat-attach")),
            "the composer offers attach: {labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| *label == crate::i18n::t("chat-send")),
            "the composer offers send: {labels:?}"
        );
        // The field is full width: a `fill` argument asks for the room the buttons leave.
        let field = composer
            .children
            .iter()
            .find(|node| node.kind == "ui.TextInput")
            .expect("the composer's field");
        assert_eq!(
            field.props.get("fill"),
            Some(&Value::Float(1.0)),
            "the composer's field takes the room the buttons leave"
        );
    }

    #[test]
    fn the_composer_posts_the_typed_text_to_the_channel() {
        let channel = demo_channel();
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let field = walk(&tree)
            .into_iter()
            .find(|node| node.kind == "ui.TextInput")
            .expect("the composer's field");
        assert_eq!(
            cx.effect(&field.key, "submit"),
            Some(tilo_view::Effect::Action {
                action: "post".to_owned(),
                args: vec![tilo_view::Arg::text("c_crit"), tilo_view::Arg::Event],
            }),
            "the field posts what is typed to the channel"
        );
        let send = walk(&tree)
            .into_iter()
            .find(|node| {
                node.kind == "ui.Button"
                    && node.props.get("label") == Some(&Value::Text(crate::i18n::t("chat-send")))
            })
            .expect("the composer's send button");
        assert_eq!(
            cx.effect(&send.key, "press"),
            Some(tilo_view::Effect::Action {
                action: "post".to_owned(),
                args: vec![
                    tilo_view::Arg::text("c_crit"),
                    tilo_view::Arg::state("Composer"),
                ],
            }),
            "the send button posts the draft the field holds"
        );
    }

    #[test]
    fn an_attachments_press_is_the_systems_open() {
        let mut channel = demo_channel();
        let shared = ResourceRef {
            id: "notes.pages/page_1".to_owned(),
            ty: "notes.page".to_owned(),
        };
        channel.messages.push(Message {
            id: "m_shared".to_owned(),
            author: channel.people[0].id.clone(),
            ts: 1_782_470_400,
            attachments: vec![Attachment {
                kind: "resource".to_owned(),
                label: "Q3 plan".to_owned(),
                resource: Some(shared.clone()),
                ..Attachment::default()
            }],
            ..Message::default()
        });
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let row = walk(&tree)
            .into_iter()
            .find(|node| {
                node.kind == "ui.ListRow"
                    && node.props.get("title") == Some(&Value::Text("Q3 plan".to_owned()))
            })
            .expect("the shared record's row");
        assert_eq!(
            cx.effect(&row.key, "activate"),
            Some(tilo_view::Effect::Open {
                target: shared,
                view: None,
            }),
            "activating the attachment places it, which is the system's open"
        );
    }

    #[test]
    fn a_reading_that_has_not_arrived_still_draws_the_page() {
        let channel = Channel::default();
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        assert!(
            texts(&tree).iter().any(|text| text == "No messages yet"),
            "an empty channel says so"
        );
    }

    #[test]
    fn a_reply_to_the_message_above_it_carries_no_quote() {
        // An agent answering the message right above it needs no quote: the person reads what is
        // answered just over the answer. A reply to anything else keeps its quote.
        const ME: &str = "ba944082ab57230fec2b518bd058a677d056dc648ac5c766f392f5f69fc1ee8c";
        let mut chat = crate::record::Chat::default();
        chat.open_channel(
            Id::from_raw("c_ask"),
            crate::record::Conversation {
                name: "Assistant".to_owned(),
                participants: vec![
                    crate::record::Participant::person(ME).atom(),
                    crate::record::Participant::agent("assistant").atom(),
                ],
                ..crate::record::Conversation::default()
            },
        );
        let message = |id: &str, author: &str, ts: i64, text: &str, parent: Option<&str>| {
            let mut message = row("c_ask", id, author, ts, text);
            message.parent = parent.map(str::to_owned);
            message
        };
        let rows = vec![
            message("m_one", ME, 1_782_470_400, "first", None),
            message("m_two", ME, 1_782_470_410, "second", None),
            message(
                "m_three",
                "agent:assistant",
                1_782_470_420,
                "answers the second",
                Some("m_two"),
            ),
            message(
                "m_four",
                "agent:assistant",
                1_782_470_430,
                "answers the first",
                Some("m_one"),
            ),
        ];
        let mut names = Names {
            me: "Quinn Davis".to_owned(),
            ..Names::default()
        };
        names
            .agents
            .insert("assistant".to_owned(), "Assistant".to_owned());
        let channel = Channel::read(&chat, &rows, "c_ask", ME, &names);
        let by_id = |id: &str| {
            channel
                .messages
                .iter()
                .find(|message| message.id == id)
                .unwrap_or_else(|| panic!("the message {id}"))
        };
        assert!(
            by_id("m_three").reply_to.is_none(),
            "a reply to the message right above it needs no quote"
        );
        assert!(
            by_id("m_four").reply_to.is_some(),
            "a reply to a message further up keeps its quote"
        );
        // And the page draws no quote for the one, and one for the other.
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        assert!(
            keyed(&tree, "ui.Quote", "@quote-m_three").is_empty(),
            "no quote stands over the reply to the message above it"
        );
        assert_eq!(
            keyed(&tree, "ui.Quote", "@quote-m_four").len(),
            1,
            "the reply to a message further up carries its quote"
        );
    }

    #[test]
    fn a_messages_avatar_stands_on_its_author_line() {
        // Every message's avatar leads the line its author's name and the time stand on: the
        // row starts its own children at the top, so the avatar of a message that carries a
        // quote and a long answer stays level with the byline and never drops to the middle of
        // the words underneath it. The person's own message and the agent's reply draw alike.
        const ME: &str = "ba944082ab57230fec2b518bd058a677d056dc648ac5c766f392f5f69fc1ee8c";
        let mut chat = crate::record::Chat::default();
        chat.open_channel(
            Id::from_raw("c_ask"),
            crate::record::Conversation {
                name: "Assistant".to_owned(),
                participants: vec![
                    crate::record::Participant::person(ME).atom(),
                    crate::record::Participant::agent("assistant").atom(),
                ],
                ..crate::record::Conversation::default()
            },
        );
        let mut said = row("c_ask", "m_reply", "agent:assistant", 1_782_470_420, "Hi");
        said.parent = Some("m_ask".to_owned());
        let rows = vec![row("c_ask", "m_ask", ME, 1_782_470_400, "hello"), said];
        let mut names = Names {
            me: "Quinn Davis".to_owned(),
            ..Names::default()
        };
        names
            .agents
            .insert("assistant".to_owned(), "Assistant".to_owned());
        let channel = Channel::read(&chat, &rows, "c_ask", ME, &names);
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        for id in ["m_ask", "m_reply"] {
            let node = walk(&tree)
                .into_iter()
                .find(|node| node.kind == "row" && node.key.contains(&format!("@message-{id}")))
                .unwrap_or_else(|| panic!("the message {id}'s row"));
            assert_eq!(
                node.props.get("align"),
                Some(&Value::Text("start".to_owned())),
                "the message {id}'s row starts its avatar on the author line"
            );
            // The avatar is the row's lead, outside the content that holds the byline and the
            // body: a growing body never moves the face.
            let avatar = node
                .children
                .iter()
                .find(|child| child.kind == "ui.Avatar")
                .unwrap_or_else(|| panic!("the message {id}'s row leads with its avatar"));
            assert!(
                avatar.key.contains("avatar-"),
                "the row's lead is the author's avatar: {}",
                avatar.key
            );
            let content = node
                .children
                .iter()
                .find(|child| child.kind == "stack" && child.key.contains("content-"))
                .unwrap_or_else(|| panic!("the message {id}'s content"));
            let byline = walk(content)
                .into_iter()
                .find(|node| node.kind == "row" && node.key.contains(&format!("by-{id}")))
                .unwrap_or_else(|| panic!("the message {id}'s byline stands in its content"));
            assert!(
                !has_kind(byline, "ui.Avatar"),
                "the byline carries no avatar of its own: the row's lead does"
            );
        }
    }

    #[test]
    fn a_messages_time_is_the_clock_the_person_reads() {
        // A message's line says when it was sent by the clock ("5:16 AM"), never the day alone:
        // the day it falls on is the divider over its run, so a time showing "Jun 26" repeats
        // the divider and says nothing of when the message was written.
        let mut channel = demo_channel();
        // 2026-06-26 09:16 UTC, read in the zone the page reads clocks in.
        channel.messages = vec![Message {
            id: "m_when".to_owned(),
            author: crate::model::ME.to_owned(),
            ts: 1_782_465_360,
            text: "Sent at a moment".to_owned(),
            ..Message::default()
        }];
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let when = keyed(&tree, "ui.Time", "@when-");
        assert_eq!(when.len(), 1, "the run's first message carries its time");
        assert_eq!(
            when[0].props.get("clock"),
            Some(&Value::Bool(true)),
            "the time reads as the clock, not as how long ago it was"
        );
        // The component's own reading of that moment: the time of day in the person's zone,
        // which says a clock and not a date.
        let label = sdk::widget::clock_text(1_782_465_360);
        assert!(
            label.contains(':'),
            "the time of day names the hour and minute: {label}"
        );
        assert!(
            !label.contains("Jun"),
            "a message's time is not its day: {label}"
        );
    }

    #[test]
    fn a_new_authors_row_stands_a_runs_gap_under_the_one_before() {
        // Two people's messages, then a third: each new author's row stands as a run of its
        // own, and the column's section gap separates runs, so a name never sits jammed
        // against what was said before it. A run's second line stays inside its own group,
        // where the rows meet tightly.
        let mut channel = demo_channel();
        channel.messages = vec![
            Message {
                id: "m_one".to_owned(),
                author: "u_aria".to_owned(),
                ts: 1_782_470_400,
                text: "First".to_owned(),
                ..Message::default()
            },
            Message {
                id: "m_two".to_owned(),
                author: "u_aria".to_owned(),
                ts: 1_782_470_460,
                text: "Still the same run".to_owned(),
                ..Message::default()
            },
            Message {
                id: "m_three".to_owned(),
                author: "u_milo".to_owned(),
                ts: 1_782_470_520,
                text: "Another person".to_owned(),
                ..Message::default()
            },
        ];
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        // The column spaces its children (runs) by a section's gap.
        let column = walk(&tree)
            .into_iter()
            .find(|node| node.key.contains("@messages"))
            .expect("the messages' column");
        assert_eq!(
            column.props.get("gap"),
            Some(&Value::Text("section".to_owned())),
            "a run stands a section's gap under the one before it"
        );
        // The two runs: the first holds the first person's two messages, meeting tightly.
        let runs: Vec<&Node> = column
            .children
            .iter()
            .filter(|node| node.key.contains("@run-"))
            .collect();
        assert_eq!(runs.len(), 2, "two people's messages make two runs");
        assert_eq!(
            runs[0].props.get("gap"),
            Some(&Value::Text("none".to_owned())),
            "a run's own messages meet, as one person's next line does"
        );
        assert_eq!(
            runs[0].children.len(),
            2,
            "the first run holds both of the first person's messages"
        );
        assert_eq!(
            runs[1].children.len(),
            1,
            "the second is the other person's"
        );
    }

    /// A page holding one agent reply being written: the person's message it answers, and the
    /// reply with the words streamed so far and the work's `state`.
    fn writing_channel(state: &str, partial: &str) -> Channel {
        let mut channel = demo_channel();
        channel.people.push(Person {
            id: "agent:tide".to_owned(),
            name: "Tide".to_owned(),
            agent: true,
        });
        channel.messages.push(Message {
            id: "m_asked".to_owned(),
            author: crate::model::ME.to_owned(),
            ts: 1_782_470_300,
            text: "Add the revenue curve".to_owned(),
            ..Message::default()
        });
        channel.messages.push(Message {
            id: crate::record::reply_id(&Id::from_raw("m_asked"), "agent:tide").to_string(),
            author: "agent:tide".to_owned(),
            ts: 1_782_470_400,
            text: partial.to_owned(),
            reply_to: Some(Quote {
                id: "m_asked".to_owned(),
                author: crate::model::ME.to_owned(),
                excerpt: "Add the revenue curve".to_owned(),
            }),
            work: Some(Work {
                state: state.to_owned(),
                detail: String::new(),
            }),
            ..Message::default()
        });
        channel
    }

    #[test]
    fn a_reply_being_written_says_so_beside_the_words_it_has_so_far() {
        // A turn the agent has taken and not ended draws a quiet "Writing…" row, in place, and
        // the words streamed so far are the message's own text: nothing jumps when more arrive.
        for state in ["taken", "doing"] {
            let channel = writing_channel(state, "On it: the curve is");
            let cx = view_cx(&channel);
            let tree = page(&channel, &cx);
            passes(&tree);
            let shown = texts(&tree);
            assert!(
                shown.iter().any(|text| text == "Writing…"),
                "a reply being written says so ({state}): {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|text| text.trim() == "On it: the curve is"),
                "the words streamed so far stand in the message ({state}): {shown:?}"
            );
            // The spinner is one line of the caption it sits beside, never a standalone orb.
            let spinners = keyed(&tree, "ui.Spinner", "@writing-spinner-");
            assert_eq!(spinners.len(), 1, "one writing row ({state})");
            assert_eq!(
                spinners[0].props.get("beside"),
                Some(&Value::Enum("caption".to_owned())),
                "the spinner is sized to the line it sits beside"
            );
            assert!(
                !has_kind(&tree, "ui.Banner"),
                "a reply that is still being written is no failure ({state})"
            );
        }
    }

    #[test]
    fn a_failed_turn_says_what_happened_and_offers_a_retry() {
        // A turn that ended without an answer the person can read leaves a row saying what
        // happened, and a Retry that asks the agent again on the very same message.
        for (state, happened) in [
            ("provider_failed", "chat-agent-failed"),
            ("provider_cancelled", "chat-agent-cancelled"),
            ("refused", "chat-agent-refused"),
        ] {
            let mut channel = writing_channel(state, "Half an answer");
            if let Some(reply) = channel.messages.last_mut() {
                reply.retry = true;
                reply.retry_summons = Some("m_asked".to_owned());
                reply.work = Some(Work {
                    state: state.to_owned(),
                    detail: "The provider connection closed".to_owned(),
                });
            }
            let cx = view_cx(&channel);
            let tree = page(&channel, &cx);
            passes(&tree);
            let rows = keyed(&tree, "ui.Banner", "@failed-");
            assert_eq!(rows.len(), 1, "one row says what happened ({state})");
            let props = &rows[0].props;
            assert_eq!(
                props.get("title"),
                Some(&Value::Text(crate::i18n::t(happened))),
                "the row says what happened ({state})"
            );
            assert_eq!(
                props.get("kind"),
                Some(&Value::Enum("error".to_owned())),
                "it is an error, announced as one ({state})"
            );
            assert_eq!(
                props.get("message"),
                Some(&Value::Text("The provider connection closed".to_owned())),
                "the run's own reason is under it ({state})"
            );
            assert_eq!(
                props.get("action"),
                Some(&Value::Text(crate::i18n::t("chat-retry"))),
                "the row offers a Retry ({state})"
            );
            assert_eq!(
                cx.effect(&rows[0].key, "act"),
                Some(tilo_view::Effect::Action {
                    action: "retry".to_owned(),
                    // The reply, and the message it answers: the verb asks the agent again on
                    // that message, and posts nothing.
                    args: vec![
                        Arg::text(
                            crate::record::reply_id(&Id::from_raw("m_asked"), "agent:tide")
                                .to_string(),
                        ),
                        Arg::text("m_asked"),
                    ],
                }),
                "Retry is the conversation's own verb, with the reply it retries ({state})"
            );
            assert!(
                !has_kind(&tree, "ui.Spinner"),
                "a turn that ended is not still being written ({state})"
            );
        }
    }

    #[test]
    fn a_reply_of_an_earlier_attempt_offers_no_retry() {
        // A retry moves the question to its next attempt, and the answer is a reply of its own:
        // the reply that failed stands as history, and a Retry there would ask a third time.
        let mut channel = writing_channel("provider_failed", "");
        // The question moved on after that reply was written.
        channel.messages[0].attempts = 1;
        channel.messages.push(Message {
            id: "m_reply_2".to_owned(),
            author: "agent:tide".to_owned(),
            ts: 1_782_470_500,
            text: "The curve is drawn".to_owned(),
            reply_to: Some(Quote {
                id: "m_asked".to_owned(),
                author: crate::model::ME.to_owned(),
                excerpt: "Add the revenue curve".to_owned(),
            }),
            work: Some(Work {
                state: "provider_complete".to_owned(),
                detail: String::new(),
            }),
            ..Message::default()
        });
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let rows: Vec<&Node> = keyed(&tree, "ui.Banner", "@failed-");
        assert_eq!(rows.len(), 1, "the failed attempt still says what happened");
        assert!(
            !rows[0].props.contains_key("action"),
            "an earlier attempt offers no Retry: the question is in a later one"
        );
        assert!(
            cx.effect(&rows[0].key, "act").is_none(),
            "the earlier attempt runs nothing when pressed"
        );
    }

    #[test]
    fn a_failed_reply_is_the_one_a_retry_asks_again() {
        // The failed reply of the question's current attempt is the one a Retry asks again: the
        // page reads the record's own work — a provider end from an earlier turn beside a state
        // the run wrote — and derives the reply's id from the summons, the agent and the
        // attempt, as Chat does.
        let workspace = crate::demo::Workspace::demo().expect("the demo workspace");
        let mut rows = workspace.message_rows("c_crit");
        let summons = rows[0].id.clone();
        let reply_id =
            crate::record::reply_id(&Id::from_raw(summons.clone()), "agent:tide").to_string();
        rows.push(crate::model::Message {
            id: reply_id.clone(),
            channel: "c_crit".to_owned(),
            author: "agent:tide".to_owned(),
            ts: 1_782_470_400,
            text: String::new(),
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: Some(summons),
            work: Some(crate::model::AgentWork {
                stream: Some(crate::agent_stream::AgentStream {
                    run: "7:0".to_owned(),
                    sequence: 3,
                    reasoning: String::new(),
                    progress: String::new(),
                    finish: Some(crate::agent_stream::AgentFinish::Failed),
                    detail: "The model connection closed".to_owned(),
                }),
                ..crate::model::AgentWork::new(
                    crate::model::AgentWorkState::ProviderFinished {
                        outcome: crate::agent_stream::AgentFinish::Failed,
                    },
                    1_782_470_400,
                )
            }),
            reactions: Vec::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            pinned: false,
            appearance: None,
            share: None,
            attempts: 0,
            sending: false,
        });
        let chat = workspace.read();
        let channel = Channel::read(&chat, &rows, "c_crit", crate::model::ME, &Names::default());
        let reply = channel.messages.last().expect("the reply");
        assert_eq!(reply.id, reply_id, "the reply is the attempt's own");
        assert_eq!(
            reply.work,
            Some(Work {
                state: "provider_failed".to_owned(),
                detail: "The model connection closed".to_owned(),
            }),
            "the page projects the record's failed end and its reason"
        );
        assert!(
            reply.retry,
            "the newest failed reply is the one a Retry asks again"
        );
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        assert_eq!(
            keyed(&tree, "ui.Banner", "@failed-").len(),
            1,
            "the page draws the row where the failure happened"
        );
    }

    #[test]
    fn a_message_still_being_sent_says_so_in_place_of_its_time() {
        // Sent from here while its conversation's messages arrive: the page shows it as the
        // window does, with "Sending…" where its time will stand.
        let workspace = crate::demo::Workspace::demo().expect("the demo workspace");
        let mut rows = workspace.message_rows("c_crit");
        // A day after the rest, so it starts a run of its own and carries its byline.
        let ts = rows.iter().map(|row| row.ts).max().unwrap_or_default() + 86_400;
        let mut sent = row("c_crit", "m_sent", crate::model::ME, ts, "On its way");
        sent.sending = true;
        rows.push(sent);
        let chat = workspace.read();
        let channel = Channel::read(&chat, &rows, "c_crit", crate::model::ME, &Names::default());
        assert!(
            channel
                .messages
                .last()
                .is_some_and(|message| message.sending)
        );
        let cx = view_cx(&channel);
        let tree = page(&channel, &cx);
        passes(&tree);
        let said: Vec<String> = keyed(&tree, "ui.Text", "@when-m_sent")
            .iter()
            .flat_map(|node| texts(node))
            .collect();
        assert_eq!(said, [crate::i18n::t("chat-message-sending")]);
        assert!(
            keyed(&tree, "ui.Time", "@when-m_sent").is_empty(),
            "no time yet"
        );
    }
}
