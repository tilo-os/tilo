//! The chat's messages: the verbs of a part ([`ChatPart`]), which part each
//! changes ([`Chained`]), and the parts a device holds read as one timeline
//! ([`Messages`]).
//!
//! A post goes to the head the device holds, or the part after it once the
//! head is full ([`crate::parts::post_part`]); anything done to a message
//! (an edit, a reaction, a reply in its thread, an agent's record of its
//! work on it) goes to the part that holds it. What a part's verb records
//! from the room's chat record — who a mention names — is filled in where it
//! is invoked ([`Chained::prepare`]), so the verb reads only its own part
//! and replays the same everywhere.
//!
//! Who posts, reacts or edits is whoever invokes the verb
//! ([`ActionCx::author`]), never an argument; when is the action's time
//! ([`ActionCx::now_ms`]).

use std::rc::Rc;

use tilo_document::ClipBlock;
use ui::framework::Chained;
use ui::tilo_resource::{self, ActionCx, Atom, Id, Json, Refusal};

use crate::model::ChannelKind;
use crate::parts::{self, ChatPart};
use crate::record::{
    AttachmentUpdate, Chat, MAX_AGENT_NAME_BYTES, MAX_MESSAGE_TEXT_BYTES, Message, Posting,
    ProviderStream, Reacted, Revision, SharedReference, Step, Transition, Work, WorkUpdate,
    reply_id,
};

fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an action in chat is always someone's"))
}

/// Refuses anyone but `message`'s author: editing and deleting a message is
/// its author's. Removing someone else's is moderation (`remove-message`),
/// which the room's roles decide.
fn own(cx: &ActionCx, message: &Message) -> Result<(), Refusal> {
    if author(cx)? == message.author.0 {
        Ok(())
    } else {
        Err(Refusal::new(
            "only its author can change a message; a moderator can remove it",
        ))
    }
}

fn seconds(cx: &ActionCx) -> i64 {
    cx.now_ms().div_euclid(1000)
}

impl ChatPart {
    /// The message text of the part, in bytes: what fills it
    /// ([`parts::PART_TEXT_BYTES`]).
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.messages
            .values()
            .map(|message| message.text.len())
            .sum()
    }

    /// Append a message to `channel` by `author` at `ts`, refusing what no
    /// one should see: a reply to a reply or to another channel's message,
    /// an oversized body or an invalid attachment. Whether the channel takes
    /// posts is the chat record's to say, where the post is invoked.
    pub(crate) fn append(
        &mut self,
        id: Id,
        channel: &Id,
        author: String,
        ts: i64,
        posting: Posting,
    ) -> Result<Id, Refusal> {
        if posting.text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        if posting
            .attachment
            .as_ref()
            .is_some_and(|attachment| !crate::record::valid_attachment(attachment))
        {
            return Err(Refusal::new(
                "the attachment is not a valid content reference",
            ));
        }
        if let Some(parent) = &posting.parent {
            let parent = self
                .messages
                .get(parent)
                .ok_or_else(|| Refusal::new("the message being answered no longer exists"))?;
            if parent.parent.is_some() || parent.channel != *channel {
                return Err(Refusal::new(
                    "a reply answers a top-level message of its channel",
                ));
            }
        }
        if posting
            .source_provider
            .as_deref()
            .is_some_and(|provider| !crate::record::valid_source_provider(provider))
        {
            return Err(Refusal::new("an existing record provider is required"));
        }
        self.messages.push(
            id.clone(),
            Message {
                channel: channel.clone(),
                author: Atom(author),
                ts,
                text: posting.text,
                body: Json(posting.body),
                parent: posting.parent,
                mentions: Json(posting.mentions),
                reference: Json(posting.reference),
                source_provider: posting.source_provider.map(Atom),
                attachment: Json(posting.attachment),
                appearance: Json(posting.appearance),
                ..Message::default()
            },
        );
        Ok(id)
    }
}

// The window's effects on a message (reveal it, read it) are offered on
// each message the chain publishes, as on the record's other items.
#[ui::tilo_resource::resource(
    kind = "chat.messages",
    label = "Messages",
    params = chat_part,
    effects = crate::effects::ChatEffect,
    item_views = crate::effects::item_views,
)]
impl ChatPart {
    /// Post a message.
    ///
    /// `@Name` of a member becomes a mention of them.
    #[action(incoming_message)]
    pub fn post(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The message; @Name mentions a member")] text: String,
        #[param("The message it answers in a thread")] parent: Option<Id>,
        #[param("Its paragraphs as written, with their formatting")] body: Option<Vec<ClipBlock>>,
        #[param("A file, image or video another action put in the store")] attachment: Option<
            AttachmentUpdate,
        >,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        let attachment = match attachment {
            Some(update) => Some(
                update
                    .attachment()
                    .ok_or_else(|| Refusal::new("an attachment is an image, a video or a file"))?,
            ),
            None => None,
        };
        if text.trim().is_empty() && attachment.is_none() && parent.is_none() {
            return Err(Refusal::new("a message says something"));
        }
        let id = cx.id();
        let by = author(cx)?;
        let ts = seconds(cx);
        self.append(
            id,
            &channel,
            by,
            ts,
            Posting {
                text,
                body: body.unwrap_or_default(),
                parent,
                attachment,
                appearance: cx.appearance(),
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Post a file.
    ///
    /// Posts a file another step handed over (a file from Files, a picture)
    /// under the name given, with words about it: a picture shows as one,
    /// anything else as a file to download.
    #[action(incoming_message, prepare = crate::record::prepare_file_post)]
    pub fn post_file(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The file another step handed over, by its content address", content)]
        content: String,
        #[param("The file's name, with its extension", example = "report.pdf")] name: String,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("Filled in from the content where it is posted; leave it out")] attachment: Option<
            AttachmentUpdate,
        >,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        let attachment = attachment
            .filter(|attachment| attachment.blob == content && attachment.name == name)
            .and_then(|attachment| attachment.attachment())
            .ok_or("the file is described where it is posted")?;
        let id = cx.id();
        let by = author(cx)?;
        let ts = seconds(cx);
        self.append(
            id,
            &channel,
            by,
            ts,
            Posting {
                text: text.unwrap_or_default(),
                attachment: Some(attachment),
                appearance: cx.appearance(),
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Share live resource.
    ///
    /// Post the address of something, with words about it; whoever opens it
    /// still needs their own access to it.
    #[action(incoming_message)]
    pub fn share_resource(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The thing: its kind, id, label and part")] reference: SharedReference,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("The record it lives in; needed to open or copy a link to it")]
        source_provider: Option<String>,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        let reference = reference.resolved();
        if !crate::record::valid_reference(&reference) {
            return Err(Refusal::new("a bounded Tilo resource address is required"));
        }
        let text = text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| reference.label.clone());
        let id = cx.id();
        let by = author(cx)?;
        let ts = seconds(cx);
        self.append(
            id,
            &channel,
            by,
            ts,
            Posting {
                text,
                reference: Some(reference),
                source_provider,
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Edit message.
    ///
    /// Its earlier text is kept in its history.
    #[action(item = messages)]
    pub fn edit_message(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        text: String,
        body: Option<Vec<ClipBlock>>,
        #[param("Filled in where it is edited; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        if text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        let ts = seconds(cx);
        let body = body.unwrap_or_default();
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        if row.text == text && row.body.0 == body {
            return Ok(());
        }
        if row.text != text {
            row.history.0.push(Revision {
                ts,
                text: std::mem::take(&mut row.text),
            });
            row.mentions = Json(mentions.unwrap_or_default());
            row.text = text;
        }
        row.body = Json(body);
        row.edited = true;
        Ok(())
    }

    /// Delete message.
    ///
    /// One of your own.
    #[action(item = messages, destructive)]
    pub fn delete_message(&mut self, cx: &mut ActionCx, message: &Id) -> Result<(), Refusal> {
        let row = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        self.messages
            .remove(message)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such message"))
    }

    /// Remove message.
    ///
    /// Someone else's message, as a moderator: the room's roles decide who
    /// may (Moderators and the owner).
    #[action(item = messages, moderation, destructive)]
    pub fn remove_message(&mut self, message: &Id) -> Result<(), Refusal> {
        self.messages
            .remove(message)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such message"))
    }

    /// Pin message.
    ///
    /// Pinned messages are listed for everyone; unpinning takes it off the
    /// list.
    #[action(item = messages)]
    pub fn pin(&mut self, message: &Id, pinned: bool) -> Result<(), Refusal> {
        self.messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?
            .pinned = pinned;
        Ok(())
    }

    /// React.
    ///
    /// Your reaction with an emoji.
    #[action(item = messages)]
    pub fn react(&mut self, cx: &mut ActionCx, message: &Id, emoji: String) -> Result<(), Refusal> {
        let me = author(cx)?;
        let emoji = emoji.trim().to_owned();
        if emoji.is_empty() || emoji.len() > 64 {
            return Err(Refusal::new("a reaction is one emoji"));
        }
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        row.reactions.insert(Reacted { emoji, user: me });
        Ok(())
    }

    /// Take back a reaction.
    #[action(item = messages)]
    pub fn unreact(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        emoji: String,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        row.reactions.remove(&Reacted {
            emoji: emoji.trim().to_owned(),
            user: me,
        });
        Ok(())
    }

    /// Record agent work.
    ///
    /// An agent's account of what it is doing with the message that
    /// summoned it, kept on its reply to that message: the reply is made
    /// the first time, and `say` gives it its words once, while it has none.
    #[action(item = messages)]
    pub fn record_work(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("Where the work is, what was done and why")] work: WorkUpdate,
        #[param("The reply's words, when it has none yet")] say: Option<String>,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let since = seconds(cx);
        let (state, reasoning, trace) = crate::model::AgentWork::from_update(&work)
            .map_err(Refusal::new)?
            .into_parts();
        let summons = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        let channel = summons.channel.clone();
        let thread = summons.parent.clone().unwrap_or_else(|| message.clone());
        let reply = reply_id(message, &me);
        if !self.messages.contains(&reply) {
            self.append(
                reply.clone(),
                &channel,
                me.clone(),
                since,
                Posting {
                    parent: Some(thread),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&reply)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if row.author.0 != me {
            return Err(Refusal::new("only the reply's author records its work"));
        }
        let record = row.work.get_or_insert_with(Work::default);
        let unchanged = record
            .state
            .0
            .as_ref()
            .is_some_and(|transition| transition.state == state);
        if !unchanged {
            record.state = Json(Some(Transition { state, since }));
        }
        // Each update says why, or that no reason is on record now.
        record.reasoning = Json(reasoning);
        for step in trace {
            let id = Id::from_raw(step.id.clone());
            let row = Step {
                id: Atom(step.id),
                sequence: step.sequence,
                resource: Atom(step.resource),
                label: step.label,
                outcome: Json(Some(step.outcome)),
            };
            if let Some(known) = record.trace.find_mut(&id) {
                *known = row;
            } else {
                record.trace.push(row);
            }
        }
        if let Some(say) = say
            && row.text.is_empty()
            && say.len() <= MAX_MESSAGE_TEXT_BYTES
        {
            row.text = say;
        }
        Ok(())
    }

    /// Record ordered provider output, never executable model actions.
    ///
    /// The invoker owns the reply; another author cannot append to it.
    /// Retrying requires a fresh reply and run, keeping partial answers intact.
    #[action]
    pub fn stream_output(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        reply: String,
        name: Option<String>,
        event: serde_json::Value,
    ) -> Result<(), Refusal> {
        use crate::agent_stream::{AgentOutput, AgentStream, AgentStreamEvent};
        let event: AgentStreamEvent = serde_json::from_value(event)
            .map_err(|_| Refusal::new("invalid provider output event"))?;
        if name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty() || name.len() > MAX_AGENT_NAME_BYTES)
        {
            return Err(Refusal::new("provider display name is empty or too long"));
        }
        let me = author(cx)?;
        let id = Id::from_raw(reply);
        let existing = self.messages.get(&id);
        if let Some(row) = existing {
            own(cx, row)?;
            if row.channel != channel {
                return Err(Refusal::new("reply belongs to another channel"));
            }
        }
        let prior = existing.and_then(|row| row.work.as_ref()?.stream.as_ref());
        let stream = prior.map_or_else(|| AgentStream::start(&event.run), ProviderStream::output);
        if !stream.accepts(&event)
            || (prior.is_none() && existing.is_some_and(|row| !row.text.is_empty()))
            || matches!(&event.event, AgentOutput::TextDelta { text }
                if existing.map_or(0, |row| row.text.len()) + text.len() > MAX_MESSAGE_TEXT_BYTES)
        {
            return Err(Refusal::new(
                "out-of-order, terminal or oversized provider output",
            ));
        }
        if existing.is_none() {
            self.append(
                id.clone(),
                &channel,
                me,
                seconds(cx),
                Posting {
                    appearance: name.map(|name| tilo_actions::Appearance {
                        name,
                        avatar: None,
                        persona: None,
                    }),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&id)
            .ok_or_else(|| Refusal::new("there is no such reply"))?;
        let work = row.work.get_or_insert_with(Work::default);
        if work.state.0.is_none() {
            work.state = Json(Some(Transition {
                state: crate::model::AgentWorkState::Taken,
                since: seconds(cx),
            }));
        }
        let record = work.stream.get_or_insert_with(|| ProviderStream {
            run: event.run.clone(),
            ..ProviderStream::default()
        });
        match &event.event {
            AgentOutput::TextDelta { text } => row.text.push_str(text),
            AgentOutput::ReasoningDelta { text } => record.reasoning.push_str(text),
            AgentOutput::Progress { label } => record.progress.clone_from(label),
            AgentOutput::Finished { outcome, detail } => {
                record.finish = Json(Some(*outcome));
                record.detail.clone_from(detail);
                work.state = Json(Some(Transition {
                    state: crate::model::AgentWorkState::ProviderFinished { outcome: *outcome },
                    since: seconds(cx),
                }));
            }
        }
        record.sequence = event.sequence;
        Ok(())
    }
}

/// Refuse a post to `channel` unless the chat has it as a text channel.
fn takes_posts(chat: &Chat, channel: &Id) -> Result<(), String> {
    let target = chat
        .channels
        .get(channel)
        .ok_or("there is no such channel")?;
    if target.kind == ChannelKind::Voice {
        return Err("a voice channel has no conversation to post in".into());
    }
    Ok(())
}

impl Chained for ChatPart {
    type Base = Chat;

    fn chain() -> ui::record::RecordId {
        parts::chain()
    }

    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Result<u64, String> {
        use chat_part::{EditMessage, Post, RecordWork, StreamOutput};
        let holding = |message: &Id| {
            parts
                .iter()
                .rev()
                .find(|(_, part)| part.messages.contains(message))
                .map(|(index, _)| *index)
                .ok_or_else(|| "there is no such message".to_owned())
        };
        let new = || {
            Ok(parts.last().map_or(0, |(head, part)| {
                parts::post_part(*head, part.messages.len(), part.text_bytes())
            }))
        };
        match &action.operation {
            // A reply goes to its parent's part, beside the message it
            // answers.
            ChatPartAction::Post(Post {
                parent: Some(parent),
                ..
            }) => holding(parent),
            ChatPartAction::Post(_)
            | ChatPartAction::PostFile(_)
            | ChatPartAction::ShareResource(_) => new(),
            // A reply being streamed stays where it started.
            ChatPartAction::StreamOutput(StreamOutput { reply, .. }) => {
                holding(&Id::from_raw(reply.clone())).or_else(|_| new())
            }
            ChatPartAction::RecordWork(RecordWork { message, .. })
            | ChatPartAction::EditMessage(EditMessage { message, .. }) => holding(message),
            other => match Self::item_of(other) {
                Some(message) => holding(&message),
                // A field edit of the part (a message inserted whole, as
                // a scenario stages one) goes to the head.
                None => Ok(parts.last().map_or(0, |(head, _)| *head)),
            },
        }
    }

    fn prepare(action: &mut Self::Action, chat: &Chat) -> Result<(), String> {
        match &mut action.operation {
            ChatPartAction::Post(post) => {
                takes_posts(chat, &post.channel)?;
                let body = post.body.clone().unwrap_or_default();
                post.mentions = Some(chat.mentions(&post.text, &body));
            }
            ChatPartAction::PostFile(post) => {
                takes_posts(chat, &post.channel)?;
                post.mentions = Some(chat.mentions(post.text.as_deref().unwrap_or_default(), &[]));
            }
            ChatPartAction::ShareResource(share) => {
                takes_posts(chat, &share.channel)?;
                let text = share
                    .text
                    .clone()
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| share.reference.label.clone());
                share.mentions = Some(chat.mentions(&text, &[]));
            }
            ChatPartAction::EditMessage(edit) => {
                let body = edit.body.clone().unwrap_or_default();
                edit.mentions = Some(chat.mentions(&edit.text, &body));
            }
            ChatPartAction::StreamOutput(stream) => takes_posts(chat, &stream.channel)?,
            _ => {}
        }
        Ok(())
    }
}

impl ChatPart {
    /// The message a verb on one message names.
    fn item_of(action: &ChatPartAction) -> Option<Id> {
        match action {
            ChatPartAction::DeleteMessage(chat_part::DeleteMessage { message })
            | ChatPartAction::RemoveMessage(chat_part::RemoveMessage { message })
            | ChatPartAction::Pin(chat_part::Pin { message, .. })
            | ChatPartAction::React(chat_part::React { message, .. })
            | ChatPartAction::Unreact(chat_part::Unreact { message, .. }) => Some(message.clone()),
            _ => None,
        }
    }
}

/// The parts of the chat's messages a device holds, read as one timeline:
/// every message oldest first, whichever part holds it.
#[derive(Clone, Debug, Default)]
pub struct Messages {
    parts: Vec<(u64, Rc<ChatPart>)>,
}

impl Messages {
    /// The parts held, oldest first.
    #[must_use]
    pub fn new(mut parts: Vec<(u64, Rc<ChatPart>)>) -> Self {
        parts.sort_by_key(|(index, _)| *index);
        Self { parts }
    }

    /// One part, numbered 0: for a reader of a single part's document.
    #[must_use]
    pub fn single(part: ChatPart) -> Self {
        Self::new(vec![(0, Rc::new(part))])
    }

    /// The parts held, oldest first.
    #[must_use]
    pub fn parts(&self) -> &[(u64, Rc<ChatPart>)] {
        &self.parts
    }

    /// The head held: the newest part.
    #[must_use]
    pub fn head(&self) -> Option<u64> {
        self.parts.last().map(|(index, _)| *index)
    }

    /// Every message held, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Id, &Message)> + '_ {
        self.parts.iter().flat_map(|(_, part)| part.messages.iter())
    }

    /// Every message held, oldest first, with the part holding it.
    pub fn located(&self) -> impl Iterator<Item = (u64, &Id, &Message)> + '_ {
        self.parts
            .iter()
            .flat_map(|(index, part)| part.messages.iter().map(move |(id, m)| (*index, id, m)))
    }

    /// How many messages are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.iter().map(|(_, part)| part.messages.len()).sum()
    }

    /// Whether no message is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The message `id`, in whichever part holds it.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&Message> {
        self.parts
            .iter()
            .rev()
            .find_map(|(_, part)| part.messages.get(id))
    }

    /// Whether a held part has the message `id`.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.part_of(id).is_some()
    }

    /// The number of the part holding the message `id`.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.parts
            .iter()
            .rev()
            .find(|(_, part)| part.messages.contains(id))
            .map(|(index, _)| *index)
    }

    /// The part a new post goes to ([`parts::post_part`]).
    #[must_use]
    pub fn post_part(&self) -> u64 {
        self.parts.last().map_or(0, |(head, part)| {
            parts::post_part(*head, part.messages.len(), part.text_bytes())
        })
    }

    /// The top-level messages of `channel` held, in every part.
    #[must_use]
    pub fn top_level(&self, channel: &Id) -> usize {
        self.iter()
            .filter(|(_, message)| message.channel == *channel && message.parent.is_none())
            .count()
    }

    /// How far someone who saw every top-level message of `channel` held
    /// has read it: the newest part with one of them, and how many it has.
    #[must_use]
    pub fn read_to_end(&self, channel: &Id) -> parts::ReadMark {
        self.parts
            .iter()
            .rev()
            .find_map(|(index, part)| {
                let seen = part
                    .messages
                    .values()
                    .filter(|message| message.channel == *channel && message.parent.is_none())
                    .count();
                (seen > 0).then(|| parts::ReadMark {
                    part: i64::try_from(*index).unwrap_or(i64::MAX),
                    seen: i64::try_from(seen).unwrap_or(i64::MAX),
                })
            })
            .unwrap_or_default()
    }

    /// The top-level messages of `channel` held after `mark`.
    #[must_use]
    pub fn unread(&self, channel: &Id, mark: parts::ReadMark) -> usize {
        self.parts
            .iter()
            .map(|(index, part)| {
                let index = i64::try_from(*index).unwrap_or(i64::MAX);
                let count = part
                    .messages
                    .values()
                    .filter(|message| message.channel == *channel && message.parent.is_none())
                    .count();
                match index.cmp(&mark.part) {
                    std::cmp::Ordering::Less => 0,
                    std::cmp::Ordering::Equal => count
                        .saturating_sub(usize::try_from(mark.seen.max(0)).unwrap_or(usize::MAX)),
                    std::cmp::Ordering::Greater => count,
                }
            })
            .sum()
    }
}

/// The parts `doc` holds when it is a part's record, read as one: for a
/// reader of one part document.
#[must_use]
pub fn read_part(doc: &tilo_actions::Doc) -> ChatPart {
    <ChatPart as tilo_resource::Resource>::read(doc).unwrap_or_default()
}

#[cfg(test)]
mod tests;
