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

/// One message of a channel, as its page shows it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
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
    #[must_use]
    pub fn read(
        chat: &crate::record::Chat,
        rows: &[crate::model::Message],
        id: &str,
        me: &str,
    ) -> Self {
        let channel_id = Id::from_raw(id);
        let channel = chat.conversation(&channel_id);
        let mut people: Vec<Person> = Vec::new();
        for row in rows {
            Self::named(&mut people, chat, &row.author);
        }
        let messages = rows
            .iter()
            .map(|row| Message {
                id: row.id.clone(),
                author: row.author.clone(),
                ts: row.ts,
                text: row.text.clone(),
                edited: row.edited,
                reply_to: row.parent.as_deref().and_then(|parent| {
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
                _ => chat.display_names(&channel_id, Some(me)),
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

    /// Add the person `author` to `people` when not already there, as the record names them.
    fn named(people: &mut Vec<Person>, chat: &crate::record::Chat, author: &str) {
        if people.iter().any(|held| held.id == author) {
            return;
        }
        let found = chat.people.iter().find(|person| person.id.0 == author);
        people.push(Person {
            id: author.to_owned(),
            name: found
                .map(|person| person.name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| author.to_owned()),
            agent: found.is_some_and(|person| person.agent),
        });
    }
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
    let mut above: Vec<Node> = vec![ui::page(
        &cx.key("head"),
        vec![ui::stack(
            &cx.key("header"),
            Gap::Tight,
            header_facts(c, cx),
        )],
    )];
    above.push(ui::fill(transcript(c, cx)));
    let conversation = ui::column(&cx.key("column"), Gap::None, above);
    // The composer stands at the foot of the column, in the page's own gutter and measure, so
    // the box that answers a message sits under the messages it answers.
    ui::page(
        &cx.key("page"),
        vec![ui::fill(ui::column(
            &cx.key("stack"),
            Gap::None,
            vec![conversation, ui::fill(composer(c, cx))],
        ))],
    )
}

/// Who the channel is: its name, and what it is about or that it is a call.
fn header_facts(c: &Channel, cx: &ViewCx) -> Vec<Node> {
    let mut facts: Vec<Node> = vec![
        ui::TextBuilder::new(&cx.key("name"), format!("#{}", c.name))
            .role(ui::TextRole::Title)
            .into(),
    ];
    let caption = if c.voice {
        "Voice channel"
    } else {
        c.topic.trim()
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
/// name once, the time beside it. The day is the toolkit's (`ui.DayDivider`), the avatar theirs
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
    // The rows, in order: a day's divider, then each message with what it answers and says.
    let clock = ConversationClock::local();
    let grouped = grouped_messages(c, &clock);
    let mut rows: Vec<Node> = Vec::new();
    for (index, (day, message, continues)) in grouped.iter().enumerate() {
        if let Some(label) = day {
            rows.push(
                ui::DayDividerBuilder::new(&cx.key(format!("day-{index}")), label.clone()).into(),
            );
        }
        rows.push(message_row(c, message, *continues, cx));
    }
    // The rows are one page's column: the page sets their measure and gutter, and a run's next
    // line stands a tight gap under the one before while a new run stands a section's gap under
    // it.
    let column = ui::stack(&cx.key("messages"), Gap::None, rows);
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
    let mut body: Vec<Node> = Vec::new();
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
    let said = ui::stack(&cx.key(format!("said-{}", message.id)), Gap::Tight, body);

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
            .square(false)
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
        head.push(
            ui::TimeBuilder::new(&cx.key(format!("when-{}", message.id)), moment(message.ts))
                .role(ui::TextRole::Caption)
                .secondary(true)
                .past(true)
                .into(),
        );
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
        "A message above".to_owned()
    } else {
        quote.excerpt.trim().to_owned()
    };
    ui::QuoteBuilder::new(key, &excerpt)
        .caption("In reply to")
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
        .placeholder(format!("Message #{}", c.name))
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
    let attach: Node = ui::ButtonBuilder::new(&cx.key("attach"), "Attach")
        .icon("paperclip")
        .icon_only(true)
        .role(ui::ButtonRole::Plain)
        .into();
    let fired = cx.action(&attach, "press", "attach-file");
    let attach = node::on(attach, "press", fired);
    // The button carries no text of its own: it sends the draft the field is bound to.
    let send: Node = ui::ButtonBuilder::new(&cx.key("send"), "Send")
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
        .unwrap_or_else(|| id.to_owned())
}

/// A person as a component draws them: their name, or their id when the channel does not name
/// them.
fn principal(person: Option<&Person>, id: &str) -> tilo_view::Principal {
    match person {
        Some(person) => tilo_view::Principal {
            id: person.id.clone(),
            name: person.name.trim().to_owned(),
        },
        None => tilo_view::Principal {
            id: id.to_owned(),
            name: String::new(),
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
        Channel::read(&workspace.read(), &rows, "c_crit", crate::model::ME)
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
        let dividers = keyed(&tree, "ui.DayDivider", "@day-");
        assert_eq!(
            dividers.len(),
            2,
            "a divider stands over the first message of each of two days"
        );
        for divider in dividers {
            let Value::Text(label) = divider.props.get("day").expect("a divider names its day")
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
            labels.iter().any(|label| label == "Attach"),
            "the composer offers attach: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label == "Send"),
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
                    && node.props.get("label") == Some(&Value::Text("Send".to_owned()))
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
}
