//! Pure data model for the chat workspace.
//!
//! These types are the in-memory projection of the Automerge document
//! (see [`crate::record`]). The convergent state — channels, messages,
//! reactions, edit history, threads, attachments — lives in the CRDT;
//! this module is just the strongly-typed view the renderer consumes,
//! plus the pure helper logic (message grouping, reply counts, reaction
//! toggling) that has no business reaching into Automerge.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// String identifier (UUID or stable seed key). Used as Automerge map
/// keys so it must round-trip as a plain string.
pub type Id = String;

/// The local user. Authored messages + the user's own reactions are keyed
/// to this; the call host tile represents this user's camera.
pub const ME: &str = "u_me";

/// Two messages by the same author land in one visual group when they're
/// within this many seconds of each other (Slack/Discord convention).
pub const GROUP_WINDOW_SECS: i64 = 5 * 60;

/// A text channel holds a conversation; a voice channel holds a call.
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
pub enum ChannelKind {
    #[default]
    Text,
    Voice,
}

impl std::str::FromStr for ChannelKind {
    type Err = std::convert::Infallible;

    /// Never fails: an unrecognised key defaults to [`ChannelKind::Text`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "voice" => ChannelKind::Voice,
            _ => ChannelKind::Text,
        })
    }
}

impl ChannelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Voice => "voice",
            ChannelKind::Text => "text",
        }
    }
    /// How a channel of this kind is named in a label: `#name` for a text
    /// channel, "name, voice channel" for a voice channel.
    pub fn label(self, name: &str) -> String {
        match self {
            ChannelKind::Text => format!("#{name}"),
            ChannelKind::Voice => format!("{name}, voice channel"),
        }
    }
}

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

/// A collapsible group of the room's channels (Discord-style "category").
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
    /// Bare authenticated Agent id for a system-created direct conversation.
    pub conversation_with: Option<Id>,
}

/// One person currently connected to a voice channel, with their live
/// audio state. Rendered both under the channel in the sidebar and as a
/// tile/chip in the call overlay.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors a distinct CRDT voice-membership field (talking/muted/deafened/audio_only/sharing) read straight off the shared state — grouping them would just relocate the boolean soup, not remove it"
)]
pub struct VoiceMember {
    pub user: Id,
    /// True while this person's mic is transmitting above the speaking
    /// threshold — drives the speaking ring in both surfaces.
    pub talking: bool,
    pub muted: bool,
    pub deafened: bool,
    /// Camera off / audio-only participant.
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
    pub id: Id,
    pub name: String,
    pub initials: String,
    pub color_idx: usize,
    pub status: Presence,
    /// An automated agent acting in the room rather than a person. The
    /// roster draws the distinction; a mention carries it forward.
    pub is_agent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
pub enum Attachment {
    /// An image addressed by its content hash. `w`/`h` are the real pixel
    /// dimensions read at import time; bytes live in the content store, never as a path meaningful only on the sending peer.
    Image {
        name: String,
        blob: String,
        w: u32,
        h: u32,
    },
    /// A video addressed by its content hash. Rendered inline as a
    /// poster/preview card with a play affordance and the real resolution; the
    /// bytes are read from the content store on demand, never copied into
    /// the CRDT.
    /// `w`/`h` are the pixel dimensions when known (0 when the container header
    /// wasn't probed).
    Video {
        name: String,
        blob: String,
        w: u32,
        h: u32,
        size: u64,
    },
    /// A non-image file share (chip with name + size).
    File {
        name: String,
        blob: String,
        size: u64,
    },
}

impl Attachment {
    /// The portable content hash backing this
    /// attachment. It is safe to replicate and cannot expose a peer-local
    /// filesystem layout.
    pub fn blob(&self) -> &str {
        match self {
            Attachment::Image { blob, .. }
            | Attachment::Video { blob, .. }
            | Attachment::File { blob, .. } => blob,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Attachment::Image { name, .. }
            | Attachment::Video { name, .. }
            | Attachment::File { name, .. } => name,
        }
    }

    /// Its size in bytes, when the sender's import measured one: an image's
    /// size is the store's, and an older one carries none rather than a guess.
    pub fn size(&self) -> Option<u64> {
        match self {
            Attachment::Image { .. } => None,
            Attachment::Video { size, .. } | Attachment::File { size, .. } => Some(*size),
        }
    }
}

/// Everyone who reacted to a message with one emoji: the framework's
/// reactions, grouped as a reaction strip shows them.
pub type Reaction = ui::tilo_resource::ReactionGroup;

/// One prior version of a message body, captured when the author edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRecord {
    pub ts: i64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: Id,
    pub channel: Id,
    pub author: Id,
    pub ts: i64,
    /// The body's plain text (see [`crate::body`]): what search, agents,
    /// notifications and edit history read.
    pub text: String,
    /// The body as written, paragraphs with their marks; empty for a message
    /// written before bodies were rich, shown from `text` ([`Self::blocks`]).
    pub body: Vec<ui::widget::BlockContent>,
    pub edited: bool,
    pub history: Vec<EditRecord>,
    /// `Some(parent_id)` when this message is a threaded reply.
    pub parent: Option<Id>,
    pub reactions: Vec<Reaction>,
    /// Who this message refers to, resolved at authoring time. Structure, not
    /// prose: a consumer asks the message who it mentions instead of scanning
    /// `text` for `@`. See [`crate::mention`].
    pub mentions: Vec<crate::mention::Mention>,
    /// What this message is *about*, when it was written by selecting
    /// something rather than by typing into the composer.
    ///
    /// The address, not a description of it: a reader resolves the block or
    /// the file the author had selected without reading `text` and without
    /// ever needing a screen position. `None` for an ordinary message.
    pub reference: Option<tilo_reference::ResourceRef>,
    /// Host-resolved existing record provider for this reference.
    pub source_provider: Option<String>,
    pub attachment: Option<Attachment>,
    /// Pinned messages are flagged in the shared document so every peer sees the
    /// same pins. A plain scalar flag toggled by an action.
    pub pinned: bool,
    /// An agent's record of what it did with the message this one replies to.
    ///
    /// Present only on a reply an agent authored when it took a summons, and
    /// updated in place by that agent as the work proceeds. `None` on every
    /// ordinary message, which is what keeps it honest as "an agent took this
    /// on" rather than a field every message carries an empty version of.
    pub work: Option<AgentWork>,
    /// The name and avatar a bot showed for this message instead of its own
    /// (a bridge posting an outside user's line). The author stays the bot.
    pub appearance: Option<tilo_actions::Appearance>,
    /// The window it shares live, and whether it still does.
    pub share: Option<crate::record::WindowShare>,
}

impl Message {
    /// The body to show: as written, or for an older message its text with
    /// its mentions marked.
    pub fn blocks(&self) -> Vec<ui::widget::BlockContent> {
        if self.body.is_empty() {
            crate::body::legacy(&self.id, &self.text, &self.mentions)
        } else {
            self.body.clone()
        }
    }

    pub fn is_top_level(&self) -> bool {
        self.parent.is_none()
    }
}

/// What an agent in this room has done with a message addressed to it.
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
/// agent is still in the room, which presence answers — because a process that
/// dies mid-instruction cannot retract a record, and a record that claimed to
/// be live would go on claiming it forever.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWork {
    /// Provider output received through ordered presentation events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<crate::agent_stream::AgentStream>,
    pub state: AgentWorkState,
    /// Epoch seconds at which the agent entered `state`.
    pub since: i64,
    /// How the agent came to choose what it chose, in the decider's own
    /// words. `None` until something has actually decided — an empty
    /// reasoning section would be a promise of an explanation that was never
    /// given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<AgentReasoning>,
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

/// How the agent decided, for a person reading the thread.
///
/// Recorded from the decision the agent actually took, so `route` cannot say
/// a model chose when the deterministic floor did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
pub struct AgentReasoning {
    /// Which path reached the decision — a model, or the agent's own keyword
    /// vocabulary. Never cosmetic: a turn decided on the floor must be
    /// findable as one afterwards.
    pub route: AgentRoute,
    /// The decider's own account of why. May be empty when the path that
    /// decided had nothing to say beyond which path it was.
    #[serde(default)]
    pub why: String,
}

/// Which path chose the operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(rename_all = "snake_case")]
pub enum AgentRoute {
    /// A model was asked and its answer was in the room's exported catalogue.
    Model,
    /// No model decided — the agent matched the request against the words the
    /// apps put on their own operations.
    Floor,
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
    /// The resource the operation was addressed to, as the room addresses it.
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
    /// What an agent reports of its work, refusing a state, route or outcome
    /// it does not name.
    ///
    /// # Errors
    /// Returns why the report is not a record of work.
    pub fn from_update(update: &crate::record::WorkUpdate) -> Result<Self, String> {
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
        let reasoning = match update.route.as_deref() {
            None => None,
            Some("model") => Some(AgentReasoning {
                route: AgentRoute::Model,
                why: text(&update.why),
            }),
            Some("floor") => Some(AgentReasoning {
                route: AgentRoute::Floor,
                why: text(&update.why),
            }),
            Some(other) => return Err(format!("`{other}` is not a route")),
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
            reasoning,
            trace,
            stream: None,
        })
    }

    /// Its state, reasoning and trace.
    #[must_use]
    pub fn into_parts(self) -> (AgentWorkState, Option<AgentReasoning>, Vec<AgentStep>) {
        (self.state, self.reasoning, self.trace)
    }

    pub fn new(state: AgentWorkState, since: i64) -> Self {
        Self {
            state,
            since,
            reasoning: None,
            trace: Vec::new(),
            stream: None,
        }
    }

    /// The same record with the decider's account attached.
    #[must_use]
    pub fn because(mut self, reasoning: AgentReasoning) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// The same record carrying `trace`.
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

    /// Whether the agent has stopped. A finished record is the end of the
    /// story; an unfinished one is the agent's last word on it, which is not
    /// the same as a promise that more is coming.
    pub fn finished(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|stream| stream.finish.is_some())
            || matches!(
                self.state,
                AgentWorkState::Done { .. }
                    | AgentWorkState::Refused
                    | AgentWorkState::ProviderFinished { .. }
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
    /// A room arrives one document at a time, so this is a claim about timing
    /// and is spelled as one — it used to be a log line and nothing else,
    /// which made a wait indistinguishable from being ignored.
    Waiting {
        /// The app being waited on, as the room names it.
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

/// A run of consecutive top-level messages by one author shown as a single
/// avatar + name header followed by stacked bodies.
#[derive(Clone, Debug)]
pub struct MessageGroup {
    pub author: Id,
    /// Indices into the source `&[Message]` slice, in order.
    pub message_indices: Vec<usize>,
}

/// Group consecutive **top-level** messages (parent == None) by the same
/// author, shown the same way, within [`GROUP_WINDOW_SECS`]. A bridge's
/// lines are all its own but each shows the outside person who wrote it,
/// so a line in another appearance starts a group of its own. Threaded
/// replies are excluded — they render inside the thread panel, not the
/// main timeline.
pub fn group_messages(messages: &[Message]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if !m.is_top_level() {
            continue;
        }
        let extend = groups.last().is_some_and(|g| {
            g.author == m.author
                && g.message_indices
                    .last()
                    .and_then(|&last_idx| messages.get(last_idx))
                    .is_some_and(|prev| {
                        prev.appearance == m.appearance
                            && (m.ts - prev.ts).abs() <= GROUP_WINDOW_SECS
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

/// Number of replies whose `parent` is `parent_id`.
pub fn reply_count(messages: &[Message], parent_id: &str) -> usize {
    messages
        .iter()
        .filter(|m| m.parent.as_deref() == Some(parent_id))
        .count()
}

/// Distinct authors who replied in a thread, in first-seen order. Used to
/// render the stacked avatar cluster on the "N replies" affordance.
pub fn thread_participants(messages: &[Message], parent_id: &str) -> Vec<Id> {
    let mut seen: Vec<Id> = Vec::new();
    for m in messages {
        if m.parent.as_deref() == Some(parent_id) && !seen.contains(&m.author) {
            seen.push(m.author.clone());
        }
    }
    seen
}

/// An epoch-seconds timestamp's time of day, as the person reads clocks.
pub fn clock(ts: i64) -> String {
    let secs_in_day = ts.rem_euclid(86_400);
    ui::clock::hour_minute(
        ui::num::u32_of_i64(secs_in_day / 3600),
        ui::num::u32_of_i64(secs_in_day % 3600 / 60),
    )
}

/// Human relative label for a message timestamp relative to `now` —
/// "Today" / "Yesterday" / "3 days ago", in the desktop's language. Used
/// for the date divider.
pub fn day_label(ts: i64, now: i64) -> String {
    let day = ts.div_euclid(86_400);
    let today = now.div_euclid(86_400);
    let ago = (today - day).max(0);
    ui::format::near_day(-ago).unwrap_or_else(|| ui::format::relative(ago * 86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, author: &str, ts: i64, parent: Option<&str>) -> Message {
        Message {
            id: id.into(),
            channel: "c".into(),
            author: author.into(),
            ts,
            text: "x".into(),
            body: Vec::new(),
            edited: false,
            history: vec![],
            parent: parent.map(std::string::ToString::to_string),
            reactions: vec![],
            mentions: vec![],
            reference: None,
            source_provider: None,
            attachment: None,
            pinned: false,
            work: None,
            appearance: None,
            share: None,
        }
    }

    #[test]
    fn timeline_groups_top_level_runs_and_counts_thread_replies() {
        // Phase: grouping_merges_same_author_within_window.
        {
            let msgs = vec![
                msg("a", "u1", 0, None),
                msg("b", "u1", 60, None),
                msg("c", "u1", 120, None),
            ];
            let groups = group_messages(&msgs);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].message_indices, vec![0, 1, 2]);
        }
        // Phase: grouping_splits_on_author_change_and_time_gap.
        {
            let msgs = vec![
                msg("a", "u1", 0, None),
                msg("b", "u2", 30, None),       // author change
                msg("c", "u2", 30 + 999, None), // gap > window
            ];
            let groups = group_messages(&msgs);
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
                msg("a", "bot:bridge", 0, None),
                msg("b", "bot:bridge", 10, None),
                msg("c", "bot:bridge", 20, None),
            ];
            msgs[0].appearance = shown("kim");
            msgs[1].appearance = shown("kim");
            msgs[2].appearance = shown("rae");
            let groups = group_messages(&msgs);
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
            let groups = group_messages(&msgs);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].message_indices, vec![0]);
        }
        // Phase: reply_count_and_participants.
        {
            let msgs = vec![
                msg("a", "u1", 0, None),
                msg("r1", "u2", 10, Some("a")),
                msg("r2", "u3", 20, Some("a")),
                msg("r3", "u2", 30, Some("a")),
            ];
            assert_eq!(reply_count(&msgs, "a"), 3);
            assert_eq!(thread_participants(&msgs, "a"), vec!["u2", "u3"]);
        }
    }

    #[test]
    fn timestamps_format_as_the_persons_clock_and_a_relative_day() {
        // Phase: clock_reads_the_time_of_day_in_the_configured_style.
        {
            let hm = ui::clock::hour_minute;
            assert_eq!(clock(0), hm(0, 0));
            assert_eq!(clock(14 * 3600 + 32 * 60), hm(14, 32));
            assert_eq!(clock(23 * 3600 + 59 * 60), hm(23, 59));
            assert_eq!(clock(3 * 86_400 + 14 * 3600 + 32 * 60), hm(14, 32));
            assert_eq!(clock(-60), hm(23, 59), "before the epoch wraps the day");
        }
        // Phase: day_label_relative.
        {
            let now = 100 * 86_400 + 3600;
            assert_eq!(day_label(now, now), "Today");
            assert_eq!(day_label(now - 86_400, now), "Yesterday");
            assert_eq!(day_label(now - 3 * 86_400, now), "3 days ago");
        }
    }
}

#[cfg(test)]
mod agent_record_tests {
    use super::{
        AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
    };

    fn step(resource: &str, label: &str, outcome: AgentStepOutcome) -> AgentStep {
        AgentStep::authored(resource, label, outcome, 0)
    }

    #[test]
    fn agent_work_records_keep_their_wire_shape_across_versions() {
        // Phase: a_record_without_reasoning_or_a_trace_still_parses.
        // A record written by an older agent — one with neither field — still
        // reads. The whole record travels as a single JSON scalar in the shared
        // document, and a record that will not parse is deliberately read as no
        // claim about the agent at all; adding a field must not be what causes
        // that.
        {
            let old = r#"{"state":{"state":"taken"},"since":1000}"#;
            let work: AgentWork = serde_json::from_str(old).expect("an older record still reads");
            assert_eq!(work.state, AgentWorkState::Taken);
            assert_eq!(work.since, 1000);
            assert_eq!(work.reasoning, None);
            assert!(work.trace.is_empty());
            // Pinned as a literal, not round-tripped from `to_string`: a test that
            // generated the input with today's code would go on passing after a
            // change that stopped yesterday's records from reading at all.
            assert_eq!(
                serde_json::to_string(&AgentWork::new(AgentWorkState::Taken, 1000)).unwrap(),
                old,
                "the wire shape of a plain record changed"
            );
        }
        // Phase: an_empty_reasoning_and_trace_are_absent_from_the_wire.
        // And a record with nothing to add serialises back to what it was, so an
        // agent that never decided anything does not start carrying an empty
        // explanation nobody wrote.
        {
            let json = serde_json::to_string(&AgentWork::new(AgentWorkState::Taken, 1000))
                .expect("serialise");
            assert!(
                !json.contains("reasoning"),
                "empty reasoning leaked: {json}"
            );
            assert!(!json.contains("trace"), "empty trace leaked: {json}");
        }
        // Phase: a_reasoning_and_a_trace_survive_a_round_trip.
        {
            let work = AgentWork::new(AgentWorkState::Done { did: "crop".into() }, 42)
                .because(AgentReasoning {
                    route: AgentRoute::Model,
                    why: "the request named the photograph".into(),
                })
                .with_trace(vec![
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
            // The route is not decoration: a turn decided on the floor has to be
            // findable as one afterwards.
            assert!(
                json.contains("\"route\":\"model\""),
                "route missing: {json}"
            );
        }
        // Phase: a_row_that_has_not_answered_yet_survives_the_wire.
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

    #[test]
    fn agent_progress_counts_only_carried_out_steps_and_finished_states() {
        // Phase: only_an_applied_or_no_change_outcome_counts_as_carried_out.
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
        // Phase: finished_is_true_only_once_the_agent_has_stopped.
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
