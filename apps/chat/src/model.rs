//! Pure data model for the chat workspace.
//!
//! These types are the in-memory projection of the Automerge document
//! (see [`crate::chat`]). The convergent state — channels, messages,
//! reactions, edit history, threads, attachments — lives in the CRDT;
//! this module is just the strongly-typed view the renderer consumes,
//! plus the pure helper logic (message grouping, reply counts, reaction
//! toggling) that has no business reaching into Automerge.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;
use ui::tilo_resource::MemberId;
use ui::widget::conversation::list::GROUPING_WINDOW_SECS;

/// String identifier (UUID or stable seed key). Used as Automerge map
/// keys so it must round-trip as a plain string.
pub type Id = String;

/// The local user. Authored messages + the user's own reactions are keyed
/// to this; the call host tile represents this user's camera.
pub const ME: &str = "u_me";

pub use chat_model::ChannelKind;

/// Whether someone is around.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ui::tilo_resource::Model,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    #[default]
    Online,
    Idle,
    Dnd,
    Offline,
}

impl std::str::FromStr for Presence {
    type Err = std::convert::Infallible;

    /// Never fails: an unrecognised key defaults to [`Presence::Online`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "offline" => Presence::Offline,
            _ => Presence::Online,
        })
    }
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Idle => "idle",
            Presence::Dnd => "dnd",
            Presence::Offline => "offline",
            Presence::Online => "online",
        }
    }
}

/// A collapsible group of the workspace's channels (Discord-style "category").
/// Channels carry a `category` id; the special empty id `""` means
/// "uncategorised" and renders above the first named category.
#[derive(Clone, Debug)]
pub struct Category {
    pub id: Id,
    pub name: String,
    pub order: u32,
    /// Collapsed categories hide their channels in the sidebar (the
    /// active channel still shows so you never lose your place). Each
    /// person's own ([`crate::preferences::ChatPreferences`]), not the
    /// document's.
    pub collapsed: bool,
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: Id,
    /// Owning category id, or `""` for uncategorised.
    pub category: Id,
    pub name: String,
    pub topic: String,
    pub kind: ChannelKind,
    pub order: u32,
    /// Unread top-level messages for the local peer. Derived (not stored) —
    /// filled in `crate::app::ChatApp::reload` from the read watermark; the
    /// active channel is always shown as 0.
    pub unread: u32,
    /// Unread messages that mention the local peer, derived like
    /// [`Self::unread`].
    pub mentions: u32,
    /// Archived channels are hidden from the sidebar to cut clutter but stay in
    /// the document (and reachable via the Ctrl+K command palette to unarchive).
    /// Convergent state — a plain scalar flag toggled by an action.
    pub archived: bool,
    /// A direct conversation between some of the workspace's people, rather
    /// than one of its named channels.
    pub direct: bool,
    /// Who a direct conversation is with; empty for a channel.
    pub members: Vec<MemberId>,
}

/// One person currently connected to a voice channel, with their live
/// audio state. Rendered both under the channel in the sidebar and as a
/// tile/chip in the call overlay.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors a distinct CRDT voice-membership field (talking/muted/deafened/audio_only/sharing) read straight off the shared state — grouping them would just relocate the boolean soup, not remove it"
)]
pub struct VoiceMember {
    pub user: MemberId,
    /// The name a bridged person is shown by on the other service; `None` for
    /// a person of the workspace, whose own name everyone knows.
    pub name: Option<String>,
    /// The service a bridged person is reached through (`discord`); `None` for
    /// a person of the workspace.
    pub service: Option<String>,
    /// True while this person's mic is transmitting above the speaking
    /// threshold — drives the speaking ring in both surfaces.
    pub talking: bool,
    pub muted: bool,
    pub deafened: bool,
    /// Camera off / audio-only member.
    pub audio_only: bool,
    /// True while this person is sharing their screen into the call (portal
    /// `ScreenCast` → the shared `TrackKind::Screen`). Shared truth in the CRDT so
    /// every peer reserves the featured screen tile and shows "N is sharing"
    /// the instant the share begins — before the first decoded frame arrives.
    pub sharing: bool,
    /// The peer's dialable media ticket (iroh node ticket in the live engine;
    /// a `sim://` placeholder otherwise). Published into the shared voice
    /// membership on join so every other peer can dial it directly — this map
    /// **is** the call's signalling channel, riding the same collab transport
    /// as the messages. `None` until the peer has announced one.
    pub ticket: Option<String>,
    /// Which video codecs the peer decodes, as announced on join. `None`
    /// for a peer whose build predates the announcement: it is treated as
    /// decoding H.264 only.
    pub video: Option<VideoCaps>,
}

/// A call member's announced video decode capabilities, as codec wire labels
/// (`"h264"`, `"vp9"`, `"av1"`, `"hevc-main10"`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoCaps {
    pub hardware_decode: Vec<String>,
    pub software_decode: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: MemberId,
    pub name: String,
    pub initials: String,
    pub status: Presence,
    /// An automated agent acting in the workspace rather than a person. The
    /// roster draws the distinction; a mention carries it forward.
    pub is_agent: bool,
}

/// Everyone who reacted to a message with one emoji: the framework's
/// reactions, grouped as a reaction strip shows them.
pub type Reaction = ui::tilo_resource::ReactionGroup;

/// One prior version of a message body, captured when the author edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditEntry {
    /// When this text was written, in milliseconds since the epoch.
    pub ts: i64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: Id,
    pub channel: Id,
    pub author: MemberId,
    /// When it was written, in milliseconds since the epoch: the action's own
    /// time. Messages are ordered by it, so it is read to the millisecond.
    pub ts: i64,
    /// The body's plain text (see [`crate::body`]): what search, agents,
    /// notifications and edit history read.
    pub text: String,
    /// The body as written, paragraphs with their marks; empty for a message
    /// written before bodies were rich, shown from `text` ([`Self::blocks`]).
    pub body: Vec<ui::rich_edit::BlockContent>,
    pub edited: bool,
    pub history: Vec<EditEntry>,
    /// `Some(parent_id)` when this message is a threaded reply.
    pub parent: Option<Id>,
    /// The run of the answered message it quotes, when it answers a span rather than the whole
    /// message: the words as they read, and where they stand in the answered message.
    pub quoted: Option<crate::chat::Quote>,
    pub reactions: Vec<Reaction>,
    /// Who this message refers to, resolved at authoring time. Structure, not
    /// prose: a consumer asks the message who it mentions instead of scanning
    /// `text` for `@`. See [`crate::mention`].
    pub mentions: Vec<crate::mention::Mention>,
    /// What it carries besides its words: the resources and parts of resources
    /// it was written by sharing, the words it quotes, and the files it
    /// posts, each in the terms of the app that keeps it.
    pub attachments: Vec<tilo_reference::Attached>,
    /// Pinned messages are flagged in the shared document so every peer sees the
    /// same pins. A plain scalar flag toggled by an action.
    pub pinned: bool,
    /// An agent's resource of what it did with the message this one replies to.
    ///
    /// Present only on a reply an agent authored when it took a summons, and
    /// updated in place by that agent as the work proceeds. `None` on every
    /// ordinary message, which is what keeps it honest as "an agent took this
    /// on" rather than a field every message carries an empty version of.
    pub work: Option<AgentWork>,
    /// The name and avatar a bot showed for this message instead of its own
    /// (a bridge posting an outside user's line). The author stays the bot.
    pub appearance: Option<tilo_actions::Appearance>,
    /// The agent or app that wrote it for a person, as the message keeps them.
    pub acting: Option<ui::tilo_resource::Acting>,
    /// The name the author went by where it wrote, for an author that is not a person.
    pub author_name: Option<String>,
    /// The window it shares live, and whether it still does.
    pub share: Option<crate::chat::WindowShare>,
    /// What it asks one person, and where that question stands.
    pub question: Option<crate::question::Question>,
    /// What started the turn it asks for, when a person did not type it.
    pub cause: Option<crate::cause::Cause>,
    /// The view it carries, written in the app language.
    pub view: Option<crate::message_view::View>,
    /// How many times a person asked the agent again for an answer to this
    /// message (`retry`): the attempt the agent answers in now.
    pub attempts: u32,
    /// Sent from here and not written yet: the part it goes to is on its way
    /// from the workspace. Shown as being sent, in a group of its own.
    pub sending: bool,
}

impl Message {
    /// The body to show: as written, or for an older message its text with
    /// its mentions marked.
    pub fn blocks(&self) -> Vec<ui::rich_edit::BlockContent> {
        if self.body.is_empty() {
            crate::body::legacy(&self.id, &self.text, &self.mentions)
        } else {
            self.body.clone()
        }
    }

    pub fn is_top_level(&self) -> bool {
        self.parent.is_none()
    }

    /// The first thing it carries that lives in an app: the resource and the
    /// part of it. What its share card names and its chip opens.
    pub fn resource(&self) -> Option<&tilo_reference::ResourceAt> {
        self.attachments.iter().find_map(|attached| match attached {
            tilo_reference::Attached::Resource { at, .. } => Some(at),
            tilo_reference::Attached::Text { .. }
            | tilo_reference::Attached::Image { .. }
            | tilo_reference::Attached::Video { .. }
            | tilo_reference::Attached::File { .. } => None,
        })
    }

    /// The files it posts, in the order it posts them.
    pub fn files(&self) -> impl Iterator<Item = &tilo_reference::Attached> {
        self.attachments
            .iter()
            .filter(|attached| attached.blob().is_some())
    }

    /// Whether any of it shows as a chip: a resource, or words quoted from one.
    pub fn carries_a_chip(&self) -> bool {
        self.attachments
            .iter()
            .any(tilo_reference::Attached::is_chip)
    }

    /// Whether a conversation shows nothing of it: a reply its agent ended without a word
    /// and with nothing to show for its work ([`AgentWork::left_nothing`]), carrying nothing
    /// else. A turn leaves such a reply under a message when what its person wrote next
    /// takes the turn along: the answer stands under their newest words, and this one is
    /// passed over wherever messages are shown or counted ([`stands`]). It stays in the
    /// conversation as it was written.
    pub fn says_nothing(&self) -> bool {
        self.work.as_ref().is_some_and(AgentWork::left_nothing)
            && self.text.trim().is_empty()
            && self.body.is_empty()
            && self.attachments.is_empty()
            && self.reactions.is_empty()
            && self.share.is_none()
            && self.question.is_none()
            && self.view.is_none()
            && !self.edited
            && !self.pinned
    }
}

/// What an agent in this workspace has done with a message addressed to it.
///
/// Written by the agent onto its own threaded reply, and updated in place as
/// the work proceeds. It is deliberately **not** the reply's body: replacing a
/// body four times would archive three revisions into the message's edit
/// history and flag it `(edited)`, claiming somebody reworded a sentence
/// nobody wrote yet, and burying the outcome under a trail of states.
///
/// Every variant is written at the moment the thing it names actually happens
/// — a summons taken, an operation chosen, an action applied, a refusal
/// written. None of them is a guess about what is about to happen, and none is
/// produced by a timer. That is the whole property: a reader who sees
/// [`AgentWorkState::Doing`] is reading a decision the agent has already made,
/// not an animation covering the fact that it has not.
///
/// It says what happened and when, never that anything is happening *now*.
/// Liveness is a different question with a different answer — whether the
/// agent is still in the workspace, which presence answers — because a process that
/// dies mid-instruction cannot retract a resource, and a resource that claimed to
/// be live would go on claiming it forever.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWork {
    /// Provider output received through ordered presentation events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<crate::agent_stream::AgentStream>,
    pub state: AgentWorkState,
    /// Milliseconds since the epoch at which the agent entered `state`.
    pub since: i64,
    /// The run's working resource of a failure, for whoever debugs it later: the
    /// transport's error, the partial answer, the provider's exposed reasoning.
    /// Never the reply's words, and shown only behind a disclosure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
    /// What answered the reply: the provider, the model, and what the turn
    /// cost and used. None for a model the server provides itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<crate::chat::Answered>,
    /// One row per operation the agent **dispatched**, in the order it
    /// dispatched them.
    ///
    /// Appended by the code that performs an operation, at the moment it
    /// hands the operation over, and rewritten in place when the app answers
    /// — never from the plan, and never from anything the model said about
    /// what it was going to do. That is the entire point of the field: a plan
    /// is an intention and a model can narrate an operation it never ran,
    /// while an entry here exists only because a dispatch happened. See
    /// [`AgentStep`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<AgentStep>,
}

/// One operation the agent dispatched, and what came of it.
///
/// Written from the dispatch path itself. Nothing constructs one of these
/// from a plan, a model answer, or a sentence: a row is evidence that the
/// agent tried to perform this operation on this resource, and its
/// [`outcome`](Self::outcome) is what the attempt returned — or
/// [`AgentStepOutcome::Asked`] while it has not returned anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStep {
    /// A UUID minted when the dispatch is authored. Trace rows are keyed by
    /// this id in the CRDT, never by their display position, so concurrent
    /// appends cannot overwrite each other.
    pub id: String,
    /// The author's monotonic position in its trace. Concurrent appends may
    /// share a sequence; readers then tie-break by the stable [`Self::id`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sequence: u64,
    /// The resource the operation was addressed to, as the workspace addresses it.
    pub resource: String,
    /// The operation, in the owning app's own words. Falls back to the action
    /// id when the app exports no label — machine vocabulary is worse to read
    /// than a label, but better than showing nothing.
    pub label: String,
    pub outcome: AgentStepOutcome,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hook requires a reference to the field"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl AgentStep {
    /// Mint one dispatch row at the authoring boundary. Replays only carry the
    /// resulting id and never call this constructor.
    #[must_use]
    pub fn authored(
        resource: impl Into<String>,
        label: impl Into<String>,
        outcome: AgentStepOutcome,
        sequence: u64,
    ) -> Self {
        Self {
            id: format!("step_{}", uuid::Uuid::new_v4().simple()),
            sequence,
            resource: resource.into(),
            label: label.into(),
            outcome,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentStepOutcome {
    /// It has been handed to the app that owns it and has not answered.
    ///
    /// Written at the dispatch and rewritten with whatever comes back, so a
    /// row resting here is an operation that went out and never answered —
    /// true, and worth seeing. It is not a success:
    /// [`succeeded`](Self::succeeded) says so.
    Asked,
    /// It went through and changed the document.
    Applied,
    /// It went through and changed nothing. Ordinary — a proposal that
    /// already reads the way it was asked to writes nothing — and said out
    /// loud rather than shown as success, which would claim an edit that
    /// nobody would find.
    NoChange,
    /// It did not happen, for the stated reason.
    Refused {
        /// Why, in the words the agent used to say so.
        why: String,
    },
}

impl AgentStepOutcome {
    /// Whether the operation was actually carried out.
    pub fn succeeded(&self) -> bool {
        matches!(self, AgentStepOutcome::Applied | AgentStepOutcome::NoChange)
    }
}

impl AgentWork {
    /// What an agent reports of its work, refusing a state or outcome
    /// it does not name.
    pub fn from_update(update: &crate::chat::WorkUpdate) -> Result<Self, String> {
        let text = |value: &Option<String>| value.clone().unwrap_or_default();
        let state = match update.state.as_str() {
            "taken" => AgentWorkState::Taken,
            "waiting" => AgentWorkState::Waiting {
                app: text(&update.app),
            },
            "doing" => AgentWorkState::Doing {
                did: text(&update.did),
                about: text(&update.about),
            },
            "done" => AgentWorkState::Done {
                did: text(&update.did),
            },
            "refused" => AgentWorkState::Refused,
            "provider_complete" => AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Complete,
            },
            "provider_truncated" => AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Truncated,
            },
            "provider_refused" => AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Refused,
            },
            "provider_cancelled" => AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Cancelled,
            },
            "provider_failed" => AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Failed,
            },
            other => return Err(format!("`{other}` is not a state of an agent's work")),
        };
        let mut trace = Vec::with_capacity(update.trace.len());
        for step in &update.trace {
            if step.id.is_empty() || step.id.len() > 128 {
                return Err("a dispatched step carries the id minted when it was".into());
            }
            let outcome = match step.outcome.as_str() {
                "asked" => AgentStepOutcome::Asked,
                "applied" => AgentStepOutcome::Applied,
                "no_change" => AgentStepOutcome::NoChange,
                "refused" => AgentStepOutcome::Refused {
                    why: text(&step.why),
                },
                other => return Err(format!("`{other}` is not an outcome")),
            };
            trace.push(AgentStep {
                id: step.id.clone(),
                sequence: step.sequence,
                resource: step.resource.clone(),
                label: step.label.clone(),
                outcome,
            });
        }
        Ok(Self {
            state,
            since: 0,
            failure_detail: update.detail.clone(),
            answered: update.answered.clone(),
            trace,
            stream: None,
        })
    }

    /// Its state and trace.
    #[must_use]
    pub fn into_parts(self) -> (AgentWorkState, Vec<AgentStep>) {
        (self.state, self.trace)
    }

    pub fn new(state: AgentWorkState, since: i64) -> Self {
        Self {
            state,
            since,
            failure_detail: None,
            answered: None,
            trace: Vec::new(),
            stream: None,
        }
    }

    /// The same resource carrying `trace`.
    #[must_use]
    pub fn with_trace(mut self, trace: Vec<AgentStep>) -> Self {
        self.trace = trace;
        self.ensure_trace_ids();
        self
    }

    /// Mint ids for steps built in-process without one, at the *authoring*
    /// boundary. This is intentionally never invoked by CRDT replay: replay
    /// receives the ids minted here as part of the action payload.
    pub fn ensure_trace_ids(&mut self) {
        for (index, step) in self.trace.iter_mut().enumerate() {
            if step.id.is_empty() {
                step.id = format!("step_{}", uuid::Uuid::new_v4().simple());
            }
            if index > 0 && step.sequence == 0 {
                step.sequence = index as u64;
            }
        }
    }

    /// Whether the agent has stopped. A finished resource is the end of the
    /// story; an unfinished one is the agent's last word on it, which is not
    /// the same as a promise that more is coming.
    pub fn finished(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|stream| stream.finish.is_some())
            || self.state.finished()
    }

    /// Whether it ended with nothing to show for it: complete or applied, with no operation
    /// dispatched and no provider output. A reply shows nothing of such work; any other end,
    /// a step, or work still going is something a reply says.
    pub fn left_nothing(&self) -> bool {
        self.stream.is_none()
            && self.trace.is_empty()
            && matches!(
                self.state,
                AgentWorkState::Done { .. }
                    | AgentWorkState::ProviderFinished {
                        outcome: crate::agent_stream::AgentFinish::Complete
                    }
            )
    }
}

impl AgentWorkState {
    /// Whether this state ends the work: the agent is done, refused, or its
    /// provider finished.
    pub fn finished(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Refused | Self::ProviderFinished { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentWorkState {
    /// The summons was taken. The agent has read it and nothing is decided.
    Taken,
    /// The agent cannot act on it yet: the request names something owned by an
    /// app whose state has not reached this agent.
    ///
    /// A workspace arrives one document at a time, so this is a claim about timing
    /// and is spelled as one — it used to be a log line and nothing else,
    /// which made a wait indistinguishable from being ignored.
    Waiting {
        /// The app being waited on, as the workspace names it.
        app: String,
    },
    /// What the agent decided the request was — in the app's own words, not
    /// the address it dispatched by, because this line is read by a person.
    Doing {
        /// The operation, as the app labels it.
        did: String,
        /// What it is about, or empty when the request named no subject.
        about: String,
    },
    /// A provider turn ended. This says nothing about dispatched operations;
    /// their outcomes remain in the trace, and only `Done` claims application.
    ProviderFinished {
        outcome: crate::agent_stream::AgentFinish,
    },
    /// The operation was applied.
    Done {
        /// The operation that was applied, as the app labels it.
        did: String,
    },
    /// The agent will not act. The reply's body says why, in its own words.
    Refused,
}

/// A run of consecutive messages by one author shown as a single
/// avatar + name header followed by stacked bodies.
#[derive(Clone, Debug)]
pub struct MessageGroup {
    pub author: MemberId,
    /// Indices into the source `&[Message]` slice, in order.
    pub message_indices: Vec<usize>,
}

/// Group consecutive messages by the same author, shown the same way, within
/// [`GROUPING_WINDOW_SECS`]. A bridge's lines are all its own but each shows the
/// outside person who wrote it, so a line in another appearance starts a
/// group of its own.
///
/// With `threads`, a channel's replies (parent != None) are left out: they
/// render inside the thread panel. Without it, as a direct conversation shows
/// them, a reply stands where it was written, in time order among the
/// messages around it, and groups like any other message.
///
/// A reply that says nothing forms no group and breaks none ([`stands`]): the messages
/// either side of it group as if it were not between them.
pub fn group_messages(messages: &[Message], threads: bool) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if threads && !m.is_top_level() {
            continue;
        }
        if !stands(messages, m) {
            continue;
        }
        let extend = groups.last().is_some_and(|g| {
            g.author == m.author
                && g.message_indices
                    .last()
                    .and_then(|&last_idx| messages.get(last_idx))
                    .is_some_and(|prev| {
                        prev.appearance == m.appearance
                            && prev.sending == m.sending
                            // What started a turn stands alone, under its own name.
                            && prev.cause.is_none()
                            && m.cause.is_none()
                            && (m.ts - prev.ts).abs()
                                <= GROUPING_WINDOW_SECS.saturating_mul(1_000)
                    })
        });
        if extend {
            if let Some(g) = groups.last_mut() {
                g.message_indices.push(i);
            }
        } else {
            groups.push(MessageGroup {
                author: m.author.clone(),
                message_indices: vec![i],
            });
        }
    }
    groups
}

/// Whether `m` stands in what a conversation shows and counts: every message but a reply
/// that says nothing ([`Message::says_nothing`]). Such a reply stands only where a message
/// that says something answers it, so that what answers it has its place.
pub fn stands(messages: &[Message], m: &Message) -> bool {
    !m.says_nothing()
        || messages
            .iter()
            .any(|other| other.parent.as_deref() == Some(m.id.as_str()) && !other.says_nothing())
}

/// Number of replies whose `parent` is `parent_id`, of those that stand ([`stands`]).
pub fn reply_count(messages: &[Message], parent_id: &str) -> usize {
    messages
        .iter()
        .filter(|m| m.parent.as_deref() == Some(parent_id) && stands(messages, m))
        .count()
}

/// Distinct authors who replied in a thread, in first-seen order. Used to
/// render the stacked avatar cluster on the "N replies" affordance.
pub fn thread_members(messages: &[Message], parent_id: &str) -> Vec<MemberId> {
    let mut seen: Vec<MemberId> = Vec::new();
    for m in messages {
        if m.parent.as_deref() == Some(parent_id)
            && stands(messages, m)
            && !seen.contains(&m.author)
        {
            seen.push(m.author.clone());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, author: &str, ts: i64, parent: Option<&str>) -> Message {
        Message {
            id: id.into(),
            channel: "c".into(),
            author: MemberId::person(author),
            ts,
            text: "x".into(),
            body: Vec::new(),
            edited: false,
            history: vec![],
            parent: parent.map(std::string::ToString::to_string),
            quoted: None,
            reactions: vec![],
            mentions: vec![],
            attachments: Vec::new(),
            pinned: false,
            work: None,
            appearance: None,
            acting: None,
            author_name: None,
            share: None,
            question: None,
            cause: None,
            view: None,
            attempts: 0,
            sending: false,
        }
    }

    tilo_test_env::scenario! {
        /// The timeline's runs and thread counts, and the times it shows.
        /// Each case was a test of its own, on a fixture of its own; a failure
        /// names the cases that failed.
        fn the_timeline_groups_its_rows_and_formats_their_times;

        phase timeline_groups_top_level_runs_and_counts_thread_replies;
        {
            // Phase: grouping_merges_same_author_within_window.
            {
                let msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("b", "u1", 60_000, None),
                    msg("c", "u1", 120_000, None),
                ];
                let groups = group_messages(&msgs, true);
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].message_indices, vec![0, 1, 2]);
            }
            // Phase: grouping_splits_on_author_change_and_time_gap.
            {
                let msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("b", "u2", 30_000, None), // author change
                    msg("c", "u2", 30_000 + GROUPING_WINDOW_SECS * 1_000 + 1, None), // gap > window
                ];
                let groups = group_messages(&msgs, true);
                assert_eq!(groups.len(), 3);
            }
            // A bridge's lines split by the outside person each shows.
            {
                let shown = |name: &str| {
                    Some(tilo_actions::Appearance {
                        name: name.into(),
                        avatar: None,
                        persona: None,
                    })
                };
                let mut msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("b", "u1", 10, None),
                    msg("c", "u1", 20, None),
                ];
                for message in &mut msgs {
                    message.author = MemberId::bot("bridge");
                }
                msgs[0].appearance = shown("kim");
                msgs[1].appearance = shown("kim");
                msgs[2].appearance = shown("rae");
                let groups = group_messages(&msgs, true);
                assert_eq!(groups.len(), 2);
                assert_eq!(groups[0].message_indices, vec![0, 1]);
                assert_eq!(groups[1].message_indices, vec![2]);
            }
            // Phase: grouping_excludes_thread_replies.
            {
                let msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("r", "u1", 10, Some("a")), // reply — not in the timeline
                ];
                let groups = group_messages(&msgs, true);
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].message_indices, vec![0]);
            }
            // A direct conversation shows its replies among the messages they
            // answer, in time order, whoever wrote them.
            {
                let msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("r", "u2", 10, Some("a")),
                    msg("b", "u1", 20, None),
                ];
                let groups = group_messages(&msgs, false);
                assert_eq!(groups.len(), 3);
                assert_eq!(groups[1].message_indices, vec![1]);
            }
            // Phase: reply_count_and_members.
            {
                let msgs = vec![
                    msg("a", "u1", 0, None),
                    msg("r1", "u2", 10, Some("a")),
                    msg("r2", "u3", 20, Some("a")),
                    msg("r3", "u2", 30, Some("a")),
                ];
                assert_eq!(reply_count(&msgs, "a"), 3);
                assert_eq!(
                    thread_members(&msgs, "a"),
                    vec![MemberId::person("u2"), MemberId::person("u3")]
                );
            }
        }

        phase a_reply_that_says_nothing_forms_no_group_and_counts_as_no_reply;
        {
            // The reply a turn leaves when what its person wrote next takes the turn along: by
            // their agent, under their message, ended complete with no word and no step.
            let left = |id: &str, parent: &str, ts: i64| Message {
                author: MemberId::agent("tide"),
                text: String::new(),
                work: Some(AgentWork::new(
                    AgentWorkState::ProviderFinished {
                        outcome: crate::agent_stream::AgentFinish::Complete,
                    },
                    ts,
                )),
                ..msg(id, "u1", ts, Some(parent))
            };
            let said = |id: &str, parent: &str, ts: i64| Message {
                text: "Done.".into(),
                ..left(id, parent, ts)
            };
            let msgs = vec![
                msg("a", "u1", 0, None),
                left("ra", "a", 5),
                msg("b", "u1", 60, None),
                said("rb", "b", 90),
            ];
            // A direct conversation: their two messages are one group, and the answer follows.
            let groups = group_messages(&msgs, false);
            assert_eq!(
                groups
                    .iter()
                    .map(|group| group.message_indices.clone())
                    .collect::<Vec<_>>(),
                vec![vec![0, 2], vec![3]],
                "the reply left forms no group and breaks none"
            );
            // A channel: the thread under their first message holds no reply and nobody.
            assert_eq!(reply_count(&msgs, "a"), 0);
            assert_eq!(thread_members(&msgs, "a"), Vec::new());
            assert_eq!(reply_count(&msgs, "b"), 1);
            // What stands: a reply that did something, one at work, one whose model failed,
            // and one that a message with words answers.
            let step = AgentStep {
                id: "s1".into(),
                sequence: 0,
                resource: "tasks.workspace".into(),
                label: "Add task".into(),
                outcome: AgentStepOutcome::Applied,
            };
            let with = |work: AgentWork| Message {
                work: Some(work),
                ..left("ra", "a", 5)
            };
            let failed = AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Failed,
            };
            for (case, stands) in [
                (
                    "it did something",
                    with(AgentWork::new(AgentWorkState::Taken, 5).with_trace(vec![step.clone()])),
                ),
                ("it is at work", with(AgentWork::new(AgentWorkState::Taken, 5))),
                ("its model failed", with(AgentWork::new(failed, 5))),
            ] {
                let msgs = vec![msg("a", "u1", 0, None), stands];
                assert_eq!(reply_count(&msgs, "a"), 1, "{case}");
                assert_eq!(group_messages(&msgs, false).len(), 2, "{case}");
            }
            let answered = vec![
                msg("a", "u1", 0, None),
                left("ra", "a", 5),
                msg("c", "u2", 10, Some("ra")),
            ];
            assert_eq!(group_messages(&answered, false).len(), 3);
            assert_eq!(reply_count(&answered, "a"), 1);
            // Work left nothing where it ended complete or applied with no step; any other
            // end, a step, or work still going is something to show.
            let complete = AgentWorkState::ProviderFinished {
                outcome: crate::agent_stream::AgentFinish::Complete,
            };
            let applied = AgentWorkState::Done { did: String::new() };
            assert!(AgentWork::new(complete.clone(), 5).left_nothing());
            assert!(AgentWork::new(applied, 5).left_nothing());
            assert!(!AgentWork::new(complete, 5).with_trace(vec![step]).left_nothing());
            for state in [
                AgentWorkState::Taken,
                AgentWorkState::Refused,
                AgentWorkState::ProviderFinished {
                    outcome: crate::agent_stream::AgentFinish::Cancelled,
                },
                AgentWorkState::ProviderFinished {
                    outcome: crate::agent_stream::AgentFinish::Truncated,
                },
            ] {
                assert!(!AgentWork::new(state.clone(), 5).left_nothing(), "{state:?}");
            }
        }
    }
}

#[cfg(test)]
mod agent_resource_tests {
    use super::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};

    fn step(resource: &str, label: &str, outcome: AgentStepOutcome) -> AgentStep {
        AgentStep::authored(resource, label, outcome, 0)
    }

    tilo_test_env::scenario! {
        fn agent_work_resources_keep_their_wire_shape_across_versions;

        phase a_resource_without_a_trace_still_parses;
        // A resource written by an older agent — one without it — still
        // reads. The whole resource travels as a single JSON scalar in the shared
        // document, and a resource that will not parse is deliberately read as no
        // claim about the agent at all; adding a field must not be what causes
        // that.
        {
            let old = r#"{"state":{"state":"taken"},"since":1000}"#;
            let work: AgentWork = serde_json::from_str(old).expect("an older resource still reads");
            assert_eq!(work.state, AgentWorkState::Taken);
            assert_eq!(work.since, 1000);
            assert!(work.trace.is_empty());
            // Pinned as a literal, not round-tripped from `to_string`: a test that
            // generated the input with today's code would go on passing after a
            // change that stopped yesterday's resources from reading at all.
            assert_eq!(
                serde_json::to_string(&AgentWork::new(AgentWorkState::Taken, 1000)).unwrap(),
                old,
                "the wire shape of a plain resource changed"
            );
        }

        phase an_empty_trace_is_absent_from_the_wire;
        {
            let json = serde_json::to_string(&AgentWork::new(AgentWorkState::Taken, 1000))
                .expect("serialise");
            assert!(!json.contains("trace"), "empty trace leaked: {json}");
        }

        phase a_trace_survives_a_round_trip;
        {
            let work =
                AgentWork::new(AgentWorkState::Done { did: "crop".into() }, 42).with_trace(vec![
                    step("photo:1", "Crop", AgentStepOutcome::Applied),
                    step("photo:1", "Rename", AgentStepOutcome::NoChange),
                    step(
                        "doc:2",
                        "Insert table",
                        AgentStepOutcome::Refused {
                            why: "not allowed here".into(),
                        },
                    ),
                ]);
            let json = serde_json::to_string(&work).expect("serialise");
            let back: AgentWork = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, work);
        }

        phase a_row_that_has_not_answered_yet_survives_the_wire;
        // A row that is still in flight survives the wire, and says so — what
        // one peer wrote when it handed an operation over is what the next one
        // reads while it waits. A variant that did not round-trip would arrive
        // as whatever `Deserialize` fell back to, which is how "asked" becomes a
        // claim about an outcome.
        {
            let work = AgentWork::new(
                AgentWorkState::Waiting {
                    app: "Photos".into(),
                },
                42,
            )
            .with_trace(vec![step(
                "photos:app",
                "Open Photos",
                AgentStepOutcome::Asked,
            )]);
            let json = serde_json::to_string(&work).expect("serialise");
            assert!(json.contains("\"outcome\":\"asked\""), "wire shape: {json}");
            let back: AgentWork = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, work);
        }
    }

    tilo_test_env::scenario! {
        fn agent_progress_counts_only_carried_out_steps_and_finished_states;

        phase only_an_applied_or_no_change_outcome_counts_as_carried_out;
        // A refusal is never counted as work carried out — the difference
        // between "it did three things" and "it did two and was stopped once".
        {
            assert!(AgentStepOutcome::Applied.succeeded());
            assert!(AgentStepOutcome::NoChange.succeeded());
            assert!(!AgentStepOutcome::Refused { why: "no".into() }.succeeded());
            // An operation that has been handed over and not answered has not
            // been carried out. Counting it would make a row that exists to say
            // "no answer yet" read as one of the things that went through.
            assert!(!AgentStepOutcome::Asked.succeeded());
        }

        phase finished_is_true_only_once_the_agent_has_stopped;
        // Whether the agent has stopped is read off the state alone.
        {
            assert!(!AgentWork::new(AgentWorkState::Taken, 0).finished());
            assert!(
                !AgentWork::new(
                    AgentWorkState::Waiting {
                        app: "Photos".into()
                    },
                    0
                )
                .finished()
            );
            assert!(
                !AgentWork::new(
                    AgentWorkState::Doing {
                        did: "Crop".into(),
                        about: String::new()
                    },
                    0
                )
                .finished()
            );
            assert!(AgentWork::new(AgentWorkState::Done { did: "Crop".into() }, 0).finished());
            assert!(AgentWork::new(AgentWorkState::Refused, 0).finished());
        }
    }
}
