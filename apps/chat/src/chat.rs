//! The chat's messages: one message of a channel and everything a message
//! carries — its body, its edits, its reactions, its thread, what it attaches,
//! the agent's resource of its work, the window it shares — and how a message
//! is posted, quoted, replied to and referred to.
//!
//! The workspace's chat resource itself — its channels and categories, read marks,
//! and feedback — is [`chat_model`]'s ([`crate::chat::Chat`]), as the
//! person's drafts are: this module keeps the messages those resources hold in
//! parts beside them ([`crate::parts`], [`crate::messages`]).

use serde::{Deserialize, Serialize};
use tilo_document::ClipBlock;
use ui::tilo_resource::{self, Atom, Id, Identified, JsonSchema, Keyed, Model};

use tilo_actions::policy::MemberId;

pub use chat_model::chat::*;

/// Longest message body, in bytes: a conversation turn, not a document.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Longest agent display name recorded with provider output.
pub const MAX_AGENT_NAME_BYTES: usize = 80;
/// Largest attachment a message refers to.
pub const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ATTACHMENT_NAME_BYTES: usize = 255;
const MAX_ATTACHMENT_DIMENSION: u32 = 16_384;

pub use ui::tilo_resource::DIRECT_CONVERSATION_PREFIX;

/// One message of a channel.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Message {
    /// The channel it was posted in.
    #[model(readonly)]
    pub channel: Id,
    #[model(readonly)]
    pub author: MemberId,
    /// When it was written, in milliseconds since the epoch: the action's own
    /// time. Messages are ordered by it, so it is read to the millisecond.
    #[model(readonly)]
    pub ts: i64,
    /// Its plain text: what search, agents and notifications read.
    #[model(label, readonly)]
    pub text: String,
    /// Its paragraphs as written, with their formatting; empty for plain text.
    #[model(readonly)]
    pub body: Vec<ClipBlock>,
    /// Whether it was changed after it was posted.
    #[model(readonly)]
    pub edited: bool,
    /// Its earlier texts, oldest first.
    #[model(readonly)]
    pub history: Vec<Revision>,
    #[model(readonly)]
    pub parent: Option<Id>,
    /// The run of the answered message it quotes, when it answers a span rather than the whole
    /// message: what the quote over it shows, and where pressing the quote reveals it.
    #[model(readonly)]
    pub quoted: Option<Quote>,
    /// Who reacted with which emoji: the framework's reactions, each the
    /// invoker's own, so this read-only message still takes them.
    #[model(readonly)]
    pub reactions: ui::tilo_resource::Reactions,
    /// Who the text refers to, resolved when it was written.
    #[model(readonly)]
    pub mentions: Vec<crate::mention::Mention>,
    /// What it carries besides its words: the resources and parts of resources
    /// it was written by sharing, the words it quotes, and the files it
    /// posts, each in the terms of the app that keeps it.
    #[model(readonly)]
    pub attachments: Vec<tilo_reference::Attached>,
    /// Pinned messages are listed for everyone (`pin`).
    #[model(readonly)]
    pub pinned: bool,
    /// An agent's resource of what it is doing with the message this one
    /// answers.
    #[model(readonly)]
    pub work: Option<Work>,
    /// The name and avatar a bot showed for it instead of its own (a bridge
    /// posting an outside person's line); the author stays the bot.
    #[model(readonly)]
    pub appearance: Option<ShownAs>,
    /// The agent or app that wrote it for a person, as its send pinned them. `None` for a
    /// person's own words.
    #[model(readonly)]
    pub acting: Option<ui::tilo_resource::Acting>,
    /// The name the author went by where it wrote, for an author that is not a person: what
    /// the host that admitted it called it, kept so every reader names it after it left.
    #[model(readonly)]
    pub author_name: Option<String>,
    /// Where it was first written, when it was moved here from another chat:
    /// shown as its author's, at its time there, while `author` keeps who
    /// moved it.
    #[model(readonly)]
    pub origin: Option<ui::tilo_resource::Origin>,
    /// The window it shares live, when it was written by sharing one: the
    /// resource is the one its first resource attachment names.
    #[model(readonly)]
    pub share: Option<WindowShare>,
    /// What it asks one person, and where that question stands: it waits
    /// until they answer it, on every device, or its asker takes it back
    /// ([`crate::question`]).
    #[model(readonly)]
    pub question: Option<crate::question::Question>,
    /// What started the turn it asks for, when a person did not type it: a
    /// reminder, an event, news of work started for them, each for the one
    /// person its asking pinned ([`crate::cause`]).
    #[model(readonly)]
    pub cause: Option<crate::cause::Cause>,
    /// The stop of the turn it asks for, when the person the turn is for
    /// stopped it (`stop-agent`): the agent ends its work, and nothing
    /// answers the message until someone asks again
    /// ([`crate::summons::Stopped`]).
    #[model(readonly)]
    pub stopped: Option<crate::summons::Stopped>,
    /// The view it carries: a small piece of interface written in the app
    /// language, shown live where the message is read. Its author is this
    /// message's ([`crate::message_view`]).
    #[model(readonly)]
    pub view: Option<crate::message_view::View>,
    /// How many times a person asked the agent again for an answer to this
    /// message (`retry`): the attempt the agent answers in now. The answer of
    /// attempt `n` is the reply [`reply_id_at`] derives, so asking again never
    /// overwrites the reply that failed — the tries stand beside one another.
    #[model(readonly)]
    pub attempts: u32,
}

impl Message {
    /// `text` by `author` in `channel` at `ts`, with nothing else to it.
    #[must_use]
    pub fn new(channel: Id, author: MemberId, ts: i64, text: String) -> Self {
        Self {
            channel,
            author,
            ts,
            text,
            body: Vec::new(),
            edited: false,
            history: Vec::new(),
            parent: None,
            quoted: None,
            reactions: ui::tilo_resource::Reactions::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            pinned: false,
            work: None,
            appearance: None,
            acting: None,
            author_name: None,
            origin: None,
            share: None,
            question: None,
            cause: None,
            stopped: None,
            view: None,
            attempts: 0,
        }
    }
}

/// A window shared live into a channel: everyone who reads the channel sees
/// the resource as it is now, drawn by its own app from their own copy of
/// its resource, and opens it with the access the sharer gave.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowShare {
    /// What the people who open it may do there.
    pub access: ShareAccess,
    /// Whether the sharer still has the window open: the embed says it is
    /// live while they do.
    pub open: bool,
    /// The sharer stopped sharing it: the message keeps saying what was
    /// shared, and no longer shows or opens it.
    pub ended: bool,
}

/// What the people a window is shared with may do when they open it.
#[derive(
    Model,
    Default,
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ShareAccess {
    /// Look at it: the window opens read-only.
    #[default]
    View,
    /// Work on it with the sharer.
    Edit,
}

impl ShareAccess {
    /// Its name in an action's argument and in the resource.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Edit => "edit",
        }
    }
}

/// The name and avatar a bot showed for a message instead of its own, as the
/// message keeps it: [`tilo_actions::Appearance`] as stored values.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownAs {
    /// The shown name.
    #[model(atom)]
    pub name: String,
    /// BLAKE3 hex of an image in the workspace's content store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// The outside account this stands for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<ShownPersona>,
}

/// An account on another service a message was shown as coming from.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownPersona {
    #[model(atom)]
    pub service: String,
    #[model(atom)]
    pub account: String,
    /// The workspace member the account is linked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked: Option<String>,
}

impl From<tilo_actions::Appearance> for ShownAs {
    fn from(appearance: tilo_actions::Appearance) -> Self {
        Self {
            name: appearance.name,
            avatar: appearance.avatar,
            persona: appearance.persona.map(|persona| ShownPersona {
                service: persona.service,
                account: persona.account,
                linked: persona.linked,
            }),
        }
    }
}

impl From<ShownAs> for tilo_actions::Appearance {
    fn from(shown: ShownAs) -> Self {
        Self {
            name: shown.name,
            avatar: shown.avatar,
            persona: shown.persona.map(|persona| tilo_actions::Persona {
                service: persona.service,
                account: persona.account,
                linked: persona.linked,
            }),
        }
    }
}

/// An earlier text of a message.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    /// When this text was written, in milliseconds since the epoch.
    pub ts: i64,
    pub text: String,
}

/// The run of a message a reply quotes, as the reply carries it: the words as they read when
/// the reply was written, and where in the answered message they stand so the quote opens the
/// very words it names ([`crate::effects::ChatEffect::RevealMessageRange`]).
///
/// A reply that answers a whole message carries none: its `parent` is the message it answers,
/// and the quote over it shows the start of what that message says. A reply to a *span* of a
/// message carries one, and the quote shows the span instead.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Quote {
    /// The words as they read when the reply was written: what the quote shows, however the
    /// answered message changes after.
    #[model(label)]
    pub text: String,
    /// Where they stand in the answered message's text, in UTF-8 bytes of
    /// [`Message::text`]: what pressing the quote reveals. A body edited after the quote was
    /// taken may move the words; the span is kept as written, and the quote itself always
    /// reads from `text`.
    pub start: u64,
    /// Where the run ends, in UTF-8 bytes, exclusive.
    pub end: u64,
}

impl Quote {
    /// Whether the span a reply quotes is one this resource may keep: its words are some and
    /// bounded, and its span is ordered and inside a message of at most
    /// [`MAX_MESSAGE_TEXT_BYTES`].
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.text.trim().is_empty()
            && self.text.len() <= MAX_MESSAGE_TEXT_BYTES
            && self.start < self.end
            && usize::try_from(self.end).is_ok_and(|end| end <= MAX_MESSAGE_TEXT_BYTES)
    }
}

/// What answered an agent's reply: the provider as the workspace named it, the model, and what the
/// turn's calls cost and used. Kept on the reply, so a conversation says which model wrote each
/// answer even after the person switched models mid-session.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Answered {
    /// The provider, as the person's settings name it (`ChatGPT`, `Google`).
    pub provider: String,
    /// The model's id, as its provider names it.
    pub model: String,
    /// The model as a person reads it, where its provider labels it.
    #[serde(default)]
    pub label: String,
    /// What the turn's calls cost, in millionths of the currency.
    pub cost_micros: u64,
    /// The tokens the turn's calls counted: every input and output token.
    #[serde(default)]
    pub tokens: u64,
}

impl Answered {
    /// The line a reply says it by: the model as a person reads it, its
    /// provider, what the turn cost and, where its provider counted them,
    /// the tokens it used.
    #[must_use]
    pub fn summary(&self) -> String {
        use crate::i18n::t_args;
        let model = if self.label.trim().is_empty() {
            self.model.clone()
        } else {
            self.label.clone()
        };
        let cost = ui::format::money(self.cost_micros);
        let mut args: Vec<(&str, ui::i18n::ArgValue)> = vec![
            ("model", model.into()),
            ("provider", self.provider.clone().into()),
            ("cost", cost.into()),
        ];
        if self.tokens == 0 {
            t_args("chat-agent-answered", &args)
        } else {
            args.push((
                "tokens",
                ui::format::Formats::current().count(self.tokens).into(),
            ));
            t_args("chat-agent-answered-tokens", &args)
        }
    }
}

/// An agent's resource of its work on a message, written by that agent.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Work {
    /// Incremental provider output.
    pub stream: Option<ProviderStream>,
    /// Where it is, and since when: one authored claim, never mixed with
    /// another's time.
    pub state: Option<Transition>,
    /// One row per operation it dispatched, rewritten when the app answers.
    pub trace: Keyed<Step>,
    /// The run's working resource of a failure, for whoever debugs it later: the
    /// transport's error, the partial answer, the provider's exposed reasoning.
    /// Never the reply's words, and shown only behind a disclosure.
    pub failure_detail: Option<String>,
    /// What answered the reply's turn, and what it cost and used.
    pub answered: Option<Answered>,
}

/// Received provider output, stored as fields and CRDT text, not a JSON snapshot.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ProviderStream {
    pub run: String,
    pub sequence: u64,
    pub reasoning: String,
    pub progress: String,
    pub finish: Option<crate::agent_stream::AgentFinish>,
    pub detail: String,
}

impl ProviderStream {
    pub(crate) fn output(&self) -> crate::agent_stream::AgentStream {
        crate::agent_stream::AgentStream {
            run: self.run.clone(),
            sequence: self.sequence,
            reasoning: self.reasoning.clone(),
            progress: self.progress.clone(),
            finish: self.finish,
            detail: self.detail.clone(),
        }
    }
}

/// One state of an agent's work and when it entered it.
#[derive(Model, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub state: crate::model::AgentWorkState,
    pub since: i64,
}

/// One operation an agent dispatched, keyed by the id minted when it did.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Step {
    pub id: Atom,
    pub sequence: u64,
    pub resource: Atom,
    #[model(label)]
    pub label: String,
    pub outcome: Option<crate::model::AgentStepOutcome>,
}

impl Identified for Step {
    fn id(&self) -> Id {
        Id::from_raw(self.id.0.clone())
    }

    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.sequence.cmp(&other.sequence)
    }
}

/// What an agent resources of its work, as a verb takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkUpdate {
    /// `taken`, `waiting`, `doing`, `done`, `refused`, or a `provider_` terminal outcome.
    pub state: String,
    /// The app waited on (`waiting`).
    #[serde(default)]
    pub app: Option<String>,
    /// The operation in the app's words (`doing`, `done`).
    #[serde(default)]
    pub did: Option<String>,
    /// What it is about (`doing`).
    #[serde(default)]
    pub about: Option<String>,
    /// The operations dispatched so far, each by the id minted when it was.
    #[serde(default)]
    pub trace: Vec<StepUpdate>,
    /// The run's working resource of a failure, for whoever debugs it later;
    /// never the reply's words. Absent when nothing failed.
    #[serde(default)]
    pub detail: Option<String>,
    /// What answered the turn, and what it cost and used. Absent while the
    /// turn goes on and for a model the server provides itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<Answered>,
}

/// One dispatched operation, as an agent reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepUpdate {
    pub id: String,
    #[serde(default)]
    pub sequence: u64,
    pub resource: String,
    pub label: String,
    /// `asked`, `applied`, `no_change` or `refused`.
    pub outcome: String,
    /// Why it was refused.
    #[serde(default)]
    pub why: Option<String>,
}

/// Describe the file a `post-file` names where it is posted: read it from
/// this device's content store, so every replica posts the same picture or
/// file, of its true size, and content that is not here is refused before
/// anyone is shown a message pointing at nothing.
pub(crate) fn prepare_file_post(
    params: &mut crate::messages::chat_messages::PostFile,
) -> Result<(), String> {
    let address = params.content.trim();
    let hash = tilo_actions::content_hash(address)
        .unwrap_or(address)
        .to_owned();
    if !tilo_content::valid_reference(&hash) {
        return Err(format!(
            "{address:?} is not a content address: post what another step handed over"
        ));
    }
    let name = params.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 255 || name.contains(['/', '\\']) {
        return Err("a posted file has a short name without a slash".into());
    }
    let bytes =
        ui::tilo_resource::content::read_hash(&hash).map_err(|refusal| refusal.to_string())?;
    let meta = tilo_content::sniff(&bytes);
    params.content.clone_from(&hash);
    params.name.clone_from(&name);
    params.attachment = Some(
        if meta.mime.starts_with("image/") && meta.w > 0 && meta.h > 0 {
            tilo_reference::Attached::Image {
                name,
                blob: hash,
                w: meta.w,
                h: meta.h,
            }
        } else {
            tilo_reference::Attached::File {
                name,
                blob: hash,
                size: meta.len,
            }
        },
    );
    Ok(())
}

/// A portable address, never copied content, a local path or an access grant.
///
/// Its kind may be empty when its id is a resource's published address
/// (`tl:<app>:<id>`, as a tilolang app publishes one): the address names the
/// resource and the catalogue its kind, exactly as [`tilo_reference::Embed`]
/// reads one, so a reference to a resource no catalogue has loaded yet still
/// attaches and resolves where it is read. Any other id names its kind.
pub(crate) fn valid_reference(reference: &tilo_reference::ResourceRef) -> bool {
    let field = |value: &str, max: usize| {
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    let published = |id: &str| {
        // `tl:<app>:<id>`: the app is dotted and never holds a `:`, so the address says its app
        // and its published id, and its kind is whatever the catalogue resolves it to.
        id.strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .is_some_and(|(app, id)| !app.is_empty() && !id.is_empty())
    };
    // A named kind is bounded like any other address; an empty one is only a resource's
    // published address, the form the catalogue reads without a kind.
    let kind = if reference.kind.is_empty() {
        published(&reference.id)
    } else {
        field(&reference.kind, 128)
            && reference
                .kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    kind && field(&reference.id, 2048)
        && !reference.id.starts_with(['/', '\\'])
        && !reference.id.contains("://")
        && field(&reference.label, 512)
        && reference.span.is_none_or(|span| span.start <= span.end)
}

/// What a message may carry: a bounded address or part of one, words worth
/// reading, or a file this workspace's store holds.
pub(crate) fn valid_attached(attached: &tilo_reference::Attached) -> bool {
    use tilo_reference::Attached;
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= MAX_ATTACHMENT_NAME_BYTES
            && !name.chars().any(char::is_control)
    };
    let bytes = |size: u64| (1..=MAX_ATTACHMENT_BYTES).contains(&size);
    match attached {
        Attached::Resource { at, anchor, label } => {
            valid_resource_at(at) && valid_anchor(anchor.as_ref()) && label.len() <= 512
        }
        Attached::Text { text, from } => {
            !text.trim().is_empty()
                && text.len() <= MAX_MESSAGE_TEXT_BYTES
                && from.as_ref().is_none_or(valid_resource_at)
        }
        Attached::Image { name, blob, w, h } => {
            valid_name(name)
                && tilo_content::valid_reference(blob)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(w)
                && (1..=MAX_ATTACHMENT_DIMENSION).contains(h)
        }
        Attached::Video {
            name,
            blob,
            w,
            h,
            size,
        } => {
            valid_name(name)
                && tilo_content::valid_reference(blob)
                && bytes(*size)
                && *w <= MAX_ATTACHMENT_DIMENSION
                && *h <= MAX_ATTACHMENT_DIMENSION
        }
        Attached::File { name, blob, size } => {
            valid_name(name) && tilo_content::valid_reference(blob) && bytes(*size)
        }
    }
}

/// The ids of the items an anchor names, and the path and cursors of a text
/// run: the item ids an app publishes, which are bounded like a place's.
fn valid_anchor(anchor: Option<&tilo_reference::Anchor>) -> bool {
    use tilo_reference::Anchor;
    let item = |id: &str| !id.is_empty() && id.len() <= 2048 && !id.chars().any(char::is_control);
    match anchor {
        None => true,
        Some(Anchor::Item { id }) => item(id),
        Some(Anchor::Items { ids }) => {
            !ids.is_empty() && ids.len() <= 256 && ids.iter().all(|id| item(id))
        }
        Some(Anchor::Text { path, start, end }) => {
            path.len() <= 2048
                && ui::automerge::Cursor::try_from(start.as_str()).is_ok()
                && ui::automerge::Cursor::try_from(end.as_str()).is_ok()
        }
    }
}

/// An existing resource of an existing app, and the resource inside it. The
/// host resolves and authorizes it; storing this address neither admits a
/// member nor grants Read.
pub(crate) fn valid_resource_at(at: &tilo_reference::ResourceAt) -> bool {
    let field = |value: &str| {
        !value.is_empty()
            && value.len() <= 2048
            && !value.chars().any(char::is_control)
            && !value.contains("://")
            && !value.contains('/')
            && !value.contains('\\')
    };
    field(&at.app) && at.record.as_deref().is_none_or(field) && valid_reference(&at.reference)
}

/// The id of `agent`'s reply to the message `summons`: the same on every
/// replica and for every resource of its work, so the agent never needs to be
/// told the id of what it wrote.
///
/// It is the reply of the first attempt. A message a person asked again
/// (`retry`) is answered in a reply of its own ([`reply_id_at`]), never in the
/// one that failed.
#[must_use]
pub fn reply_id(summons: &Id, agent: &MemberId) -> Id {
    reply_id_at(summons, agent, 0)
}

/// The id of `agent`'s reply to `summons` on attempt `attempt` (`0` the first,
/// [`Message::attempts`] the one being written now): derived the same way on
/// every replica, so an agent never needs to be told the id of what it wrote,
/// and a retried turn never overwrites the reply that failed.
#[must_use]
pub fn reply_id_at(summons: &Id, agent: &MemberId, attempt: u32) -> Id {
    use std::hash::Hasher as _;
    let mut hash = fnv::FnvHasher::default();
    hash.write(summons.as_str().as_bytes());
    hash.write(&[0]);
    hash.write(&agent.actor_bytes());
    if attempt > 0 {
        hash.write(&[0]);
        hash.write(&attempt.to_le_bytes());
    }
    Id::from_raw(format!("m_reply_{:016x}", hash.finish()))
}

/// A new message's fields beyond its channel, author and time.
#[derive(Default)]
pub(crate) struct Posting {
    pub text: String,
    pub body: Vec<ClipBlock>,
    pub parent: Option<Id>,
    /// The run of the message it answers that it quotes, when it answers a span.
    pub quoted: Option<Quote>,
    /// What it carries besides its words.
    pub attachments: Vec<tilo_reference::Attached>,
    pub appearance: Option<tilo_actions::Appearance>,
    pub share: Option<WindowShare>,
    /// What it asks one person.
    pub question: Option<crate::question::Question>,
    /// What started the turn it asks for, when a person did not type it.
    pub cause: Option<crate::cause::Cause>,
    /// The view it carries.
    pub view: Option<crate::message_view::View>,
    /// Who the text refers to, as the chat resource resolved it where the
    /// message was written.
    pub mentions: Vec<crate::mention::Mention>,
    /// Where it was first written, when it was moved here from elsewhere.
    pub origin: Option<ui::tilo_resource::Origin>,
}

/// Who a message refers to: the mentions marked in its paragraphs as written,
/// or for plain text each `@Name` of a member the workspace names
/// ([`ui::tilo_resource::members`]).
pub(crate) fn mentions(text: &str, body: &[ClipBlock]) -> Vec<crate::mention::Mention> {
    let members = ui::tilo_resource::members::named();
    if body.is_empty() {
        return crate::mention::resolve(text, &members);
    }
    let blocks: Vec<_> = body.iter().map(tilo_rich_text::content_of_clip).collect();
    crate::body::mentions(&blocks, &members)
}

#[cfg(test)]
mod mention_tests;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod tests;
