//! Chat's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Chat performs on the shared workspace is a
//! [`ChatAction`]: a small serializable value that deterministically mutates the
//! CRDT (see [`crate::doc`]). Because messages, channels, reactions, pins and
//! read-watermarks already live in the shared document, an action replicates and
//! replays for free and feeds straight back into the view on the next `sync`. The
//! exact same [`EncodedAction`] is what the on-screen affordances (a Send, a
//! reaction chip, the channel context menu, the message action menu, the Ctrl+K
//! command palette) build and dispatch — one source of truth, one replay path.
//!
//! ## The split
//!
//! **Messages are the collaborative document.** Document-mutating → an action:
//! send / edit / delete a message, add / remove a reaction, pin / unpin, create /
//! rename / delete / archive a channel, mark a channel read, and every edit of
//! a channel's or thread's shared draft ([`ChatAction::EditDraft`]).
//!
//! **Presentation → never an action:** scroll position, the current
//! selection, which channel/thread is open, which categories are collapsed,
//! typing indicators, and live presence. Nothing there replicates as a
//! discrete mutation.
//!
//! **External → never an action:** voice / screen-share *transport*, media
//! upload, and the network send itself. When a file is shared, the message **row
//! insert** is the action (it carries the attachment *by reference* — a
//! content hash, display name, and bounded metadata, never a local path or the
//! bytes); the media bytes remain outside the CRDT and hydrate on demand.
//!
//! ## A call is not in the document
//!
//! Who is in a call, muted or presenting is live state of the shell's voice
//! service, which every member reads from it; Chat's call controls are
//! effects ([`crate::effects::ChatEffect`]) that ask that service, and nothing
//! about a call is written down.
//!
//! ## Determinism contract
//!
//! A message id and its timestamp are minted **at authoring time** and carried in
//! the payload ([`ChatAction::SendMessage`]), so [`Action::apply`] is a pure
//! function of `(payload, base document)` — no RNG, no wall-clock, no app state.
//! Where an operation derives a value from the document (a new channel's `order`,
//! a mark-read watermark) it is computed from the base state the replay shares, or
//! carried in the payload, and every reader iterates in the document's stable key
//! order. A malformed payload is a **no-op**, never an error, so a bad replay
//! can't wedge a peer.

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_i64;

use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceViewRangeSpec, ResourceViewSpec,
};
use tilo_document::RichTextAction;
use ui::tilo_resource::Effect as _;

use crate::effects::ChatEffect;

use crate::doc::{ChatDoc, NewMessage};
use crate::model::{Attachment, ChannelKind};

/// Registry `type_id` for every Chat action.
pub const CHAT_ACTION_TYPE: &str = "chat.action";

/// The authored payload of one direct-agent request. Its author always comes
/// from the authenticated action envelope, never this payload.
pub type AgentMessagePayload = tilo_actions::agent_message::AgentMessagePayload<
    Attachment,
    crate::mention::Mention,
    tilo_reference::ResourceRef,
    tilo_document::ClipBlock,
>;

/// Every collaborative operation Chat exports on the shared workspace.
///
/// Determinism contract: ids/timestamps are minted at authoring time and carried
/// in the payload; [`Action::apply`] never touches the RNG or the wall-clock and
/// derives any remaining value from the replay's shared base state in stable
/// order. Who wrote, reacted or read is the action's author, pinned with it
/// like its time, never a payload field. A malformed payload is a **no-op**
/// rather than an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatAction {
    /// Reviewed feedback, filed through the same attributed Chat document.
    Feedback(crate::feedback::FeedbackAction),
    /// Post a message into `channel`. The `id` and `ts` are minted once at
    /// authoring time and carried here, so replay is byte-identical. A chat
    /// message is discrete (not a co-edited flowing document), so a whole-message
    /// `text` payload is the correct grain. `parent` is `Some` for a threaded
    /// reply. `attachment` references portable shared bytes by content
    /// hash, so a peer never needs the sender's filesystem path.
    SendMessage {
        channel: String,
        id: String,
        ts: i64,
        text: String,
        parent: Option<String>,
        attachment: Option<Attachment>,
        /// Who the body refers to, resolved to participant ids when the message
        /// was authored (see [`crate::mention`]). Part of the payload — not
        /// derived at apply — so a replay on another peer produces byte-identical
        /// references without needing that peer's roster. `#[serde(default)]` so
        /// a frame from a release that predates mentions still decodes.
        #[serde(default)]
        mentions: Vec<crate::mention::Mention>,
        /// The address of the thing this message is about, when it was
        /// written by selecting something rather than typed from scratch.
        ///
        /// Part of the payload for the same reason `mentions` is: it is
        /// resolved once, by the app that owns the selection, and replaying
        /// it elsewhere must not depend on that app being open there.
        /// `#[serde(default)]` so a frame from a release that predates
        /// references still decodes.
        #[serde(default)]
        reference: Option<tilo_reference::ResourceRef>,
        /// Existing record provider; an address only, never authority or a URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_provider: Option<String>,
        /// The body as written (paragraphs, marks, mentions marked); `text`
        /// is its plain text. Empty for a plain message, and in a frame from
        /// a release that predates rich bodies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body: Vec<tilo_document::ClipBlock>,
    },
    /// Atomically create or reuse the deterministic direct channel and append
    /// a top-level Message addressed to `agent`. Queue and Steer are refused
    /// until their authoritative Task providers are installed.
    PostAgentMessage {
        workspace: String,
        agent: String,
        message: AgentMessagePayload,
    },
    /// Reply to one top-level Message in a direct Agent conversation. The
    /// signed action actor is the only author/Agent identity; none is carried
    /// in this payload.
    ReplyAgentMessage {
        channel: String,
        parent: String,
        id: String,
        ts: i64,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body: Vec<tilo_document::ClipBlock>,
    },
    /// Replace a message's body, archiving the prior text into its edit history
    /// and flagging it edited. `ts` (the edit time) is minted at authoring.
    EditMessage {
        channel: String,
        id: String,
        text: String,
        ts: i64,
        /// The new body as written; empty for a plain edit.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        body: Vec<tilo_document::ClipBlock>,
    },
    /// Delete a message row from `channel`.
    DeleteMessage {
        channel: String,
        id: String,
    },
    /// Add the author's `emoji` reaction to a message (idempotent).
    AddReaction {
        channel: String,
        msg: String,
        emoji: String,
    },
    /// Remove the author's `emoji` reaction from a message (idempotent).
    RemoveReaction {
        channel: String,
        msg: String,
        emoji: String,
    },
    /// Set (or clear) a message's pinned flag — shared truth every peer sees.
    PinMessage {
        channel: String,
        msg: String,
        pinned: bool,
    },
    /// Create a channel with a caller-chosen `id` at the end of `category` within
    /// `workspace`. The `id` is minted at authoring; the `order` is derived from
    /// the document's base state at apply, so replay is deterministic.
    CreateChannel {
        workspace: String,
        category: String,
        id: String,
        name: String,
        kind: ChannelKind,
    },
    /// Create a durable category.  Its id is minted at authoring time so the
    /// same replay produces the same grouping row on every peer.
    CreateCategory {
        workspace: String,
        id: String,
        name: String,
    },
    RenameCategory {
        id: String,
        name: String,
    },
    /// Delete the category while retaining its channels in the uncategorised
    /// group, matching the recoverable document policy.
    DeleteCategory {
        id: String,
    },
    /// Move a channel one slot in its current category.  The document derives
    /// the adjacent sibling from its replay base, just as the live UI does.
    ReorderChannel {
        id: String,
        up: bool,
    },
    /// Rename a channel.
    RenameChannel {
        id: String,
        name: String,
    },
    /// Delete a channel and its messages and draft.
    DeleteChannel {
        id: String,
    },
    /// Set (or clear) a channel's archived flag. Archived channels drop out of
    /// the sidebar but stay in the document (reachable via Ctrl+K to unarchive).
    ArchiveChannel {
        id: String,
        archived: bool,
    },
    /// Advance the author's read watermark for `channel` to `count`, minted
    /// at authoring time (the channel's top-level message count) and carried
    /// here so replay never re-reads live state.
    MarkRead {
        channel: String,
        count: i64,
    },
    /// Record what an agent is doing with the message `msg` replies to, and
    /// optionally supply the words `msg` did not have when it was posted.
    /// Only `msg`'s own author records its work.
    ///
    /// Deliberately not [`ChatAction::EditMessage`]. An agent that took a
    /// summons posts its reply *before* it has an answer, and the states it
    /// then passes through are not revisions of a sentence: routing them
    /// through `EditMessage` would archive each one into the message's edit
    /// history, flag the reply `edited`, and hang an "Edited" marker off an
    /// outcome nobody had rewritten. The body is
    /// written once, when there is one; the state is a separate fact with a
    /// separate field.
    SetAgentWork {
        channel: String,
        msg: String,
        work: crate::model::AgentWork,
        /// The agent's words, when it finally has them. Applied only to a
        /// body that is still empty, so this can never overwrite something
        /// somebody would want back.
        #[serde(default)]
        say: Option<String>,
    },

    /// Announce the author into the shared roster. Writes only fields that
    /// differ, so a repeat is a no-op.
    EnsureUser {
        name: String,
        initials: String,
        color_idx: usize,
        status: String,
    },
    /// The name every member shows for agent task `participant`, as the
    /// person hosting the task labelled it.
    NameAgent {
        participant: String,
        name: String,
    },
    /// One edit of the rich-text draft stored under `key` (a channel, or a
    /// thread), as its composer or a caller of the draft's child resource
    /// made it.
    EditDraft {
        key: String,
        edit: RichTextAction,
    },
    /// Empty the draft stored under `key`, once it was sent.
    ClearDraft {
        key: String,
    },
    /// Create the shared rich draft for a composer when it is missing or split.
    EnsureRichDraft {
        key: String,
    },
    /// The demo workspace a screenshot shows: its people, rooms and
    /// conversation, read by whoever applies it.
    SeedDemo {},
    /// The first workspace in a new chat record: its creator, the author,
    /// and two channels.
    SeedStarter {
        name: String,
        initials: String,
        color_idx: usize,
    },
    /// Structural initialization for the native host's creation workspace.
    SeedWorkspace {
        workspace: String,
        workspace_name: String,
        creator_name: String,
        creator_initials: String,
        color_idx: usize,
    },
}

/// Longest agent display name recorded beside an agent-authored message.
pub const MAX_AGENT_AUTHOR_NAME_BYTES: usize = 80;

/// Durable, browseable history metadata for a Chat mutation.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive action-to-history mapping keeps every ChatAction variant compiler-checked"
)]
pub fn history_context(action: &ChatAction) -> tilo_actions::ActionContext {
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.chat", "Chat", action);
    let (label, channel, resource_id, kind) = match action {
        ChatAction::Feedback(feedback) => {
            let (id, channel) = feedback.locus();
            ("File feedback", channel, id, "chat.feedback")
        }
        ChatAction::SendMessage { channel, id, .. } => {
            ("Send message", channel, id, "chat.message")
        }
        ChatAction::PostAgentMessage {
            workspace, message, ..
        } => ("Message agent", workspace, &message.id, "chat.message"),
        ChatAction::ReplyAgentMessage { channel, id, .. } => {
            ("Reply to message", channel, id, "chat.message")
        }
        ChatAction::EditMessage { channel, id, .. } => {
            ("Edit message", channel, id, "chat.message")
        }
        ChatAction::DeleteMessage { channel, id } => {
            ("Delete message", channel, id, "chat.message")
        }
        ChatAction::AddReaction { channel, msg, .. } => {
            ("Add reaction", channel, msg, "chat.message")
        }
        ChatAction::RemoveReaction { channel, msg, .. } => {
            ("Remove reaction", channel, msg, "chat.message")
        }
        ChatAction::PinMessage {
            channel,
            msg,
            pinned,
        } => (
            if *pinned {
                "Pin message"
            } else {
                "Unpin message"
            },
            channel,
            msg,
            "chat.message",
        ),
        ChatAction::SetAgentWork { channel, msg, .. } => {
            ("Update agent work", channel, msg, "chat.message")
        }
        ChatAction::CreateChannel { id, .. } => ("Create channel", id, id, "chat.channel"),
        ChatAction::CreateCategory { id, .. } => ("Create category", id, id, "chat.category"),
        ChatAction::RenameCategory { id, .. } => ("Rename category", id, id, "chat.category"),
        ChatAction::DeleteCategory { id } => ("Delete category", id, id, "chat.category"),
        ChatAction::ReorderChannel { id, up } => (
            if *up {
                "Move channel up"
            } else {
                "Move channel down"
            },
            id,
            id,
            "chat.channel",
        ),
        ChatAction::RenameChannel { id, .. } => ("Rename channel", id, id, "chat.channel"),
        ChatAction::DeleteChannel { id } => ("Delete channel", id, id, "chat.channel"),
        ChatAction::ArchiveChannel { id, archived } => (
            if *archived {
                "Archive channel"
            } else {
                "Unarchive channel"
            },
            id,
            id,
            "chat.channel",
        ),
        ChatAction::MarkRead { channel, .. } => {
            ("Mark channel read", channel, channel, "chat.channel")
        }
        ChatAction::EnsureUser { .. }
        | ChatAction::SeedStarter { .. }
        | ChatAction::SeedWorkspace { .. }
        | ChatAction::SeedDemo {} => {
            return context;
        }
        ChatAction::NameAgent { participant, .. } => {
            ("Name agent", participant, participant, "chat.person")
        }
        ChatAction::EnsureRichDraft { key } => ("Open draft", key, key, "chat.draft"),
        ChatAction::EditDraft { key, .. } => ("Edit draft", key, key, "chat.draft"),
        ChatAction::ClearDraft { key } => ("Clear draft", key, key, "chat.draft"),
    };
    context.action_label = label.to_string();
    context.resource = Some(tilo_actions::ActionResource {
        id: format!("{kind}:{resource_id}"),
        kind: kind.to_string(),
        label: if kind == "chat.message" {
            "Message".to_string()
        } else {
            channel.clone()
        },
    });
    context
}

fn apply_seed_starter(
    doc: &mut Doc,
    name: &str,
    initials: &str,
    color_idx: usize,
) -> Result<(), ActionError> {
    let id = author().unwrap_or_default();
    let id = id.as_str();
    if id.is_empty() || id.len() > 128 || name.len() > 128 || initials.len() > 8 {
        return Err(ActionError::Refused("workspace identity is invalid".into()));
    }
    ChatDoc::seed_starter(doc, id, name, initials);
    let _ = ChatDoc::ensure_user(
        doc,
        id,
        name,
        initials,
        color_idx,
        crate::model::Presence::Online,
    );
    Ok(())
}

fn apply_ensure_rich_draft(doc: &mut Doc, key: &str) -> Result<(), ActionError> {
    if key.is_empty() || key.len() > 256 {
        return Err(ActionError::Refused("draft key is invalid".into()));
    }
    ChatDoc::ensure_rich_draft(doc, key)
        .ok_or_else(|| ActionError::Refused("draft could not be created".into()))?;
    Ok(())
}

fn apply_seed_workspace(
    doc: &mut Doc,
    workspace: &str,
    workspace_name: &str,
    creator_name: &str,
    creator_initials: &str,
    color_idx: usize,
) -> Result<(), ActionError> {
    let creator = signed_by()?;
    if uuid::Uuid::parse_str(workspace).is_err()
        || workspace_name.trim().is_empty()
        || workspace_name.len() > 256
        || creator_name.trim().is_empty()
        || creator_name.len() > 128
        || creator_initials.len() > 8
    {
        return Err(ActionError::Refused(
            "workspace initialization is invalid".into(),
        ));
    }
    ChatDoc::seed_workspace(
        doc,
        workspace,
        workspace_name,
        &creator,
        creator_name,
        creator_initials,
        color_idx,
    )
    .map_err(ActionError::Refused)
}

/// Canonical request row for one broker-admitted invocation. Re-delivery of
/// that invocation therefore cannot append a second request.
#[must_use]
pub fn agent_request_message_id(invocation: tilo_actions::InvocationId) -> String {
    format!("m_{}", invocation.0.simple())
}

/// Build the single canonical Chat mutation for an explicit agent intent.
/// The public resource decides whether an intent is currently available;
/// apply independently refuses intents whose authoritative provider is not
/// installed.
#[must_use]
pub fn post_agent_message_action(
    workspace: &str,
    agent: &str,
    text: &str,
    intent: crate::model::AgentIntent,
    ts: i64,
) -> ChatAction {
    ChatAction::PostAgentMessage {
        workspace: workspace.to_owned(),
        agent: agent.to_owned(),
        message: AgentMessagePayload {
            id: String::new(),
            ts,
            text: text.to_owned(),
            intent,
            attachment: None,
            mentions: Vec::new(),
            reference: None,
            body: Vec::new(),
        },
    }
}

fn apply_agent_request(
    doc: &mut Doc,
    workspace: &str,
    agent: &str,
    message: &AgentMessagePayload,
) -> Result<(), ActionError> {
    if message.intent != crate::model::AgentIntent::Message {
        return Err(ActionError::Refused(
            "Queue and Steer require the workspace Task owner".into(),
        ));
    }
    let payload = serde_json::to_vec(&ChatAction::PostAgentMessage {
        workspace: workspace.to_owned(),
        agent: agent.to_owned(),
        message: message.clone(),
    })
    .map_err(ActionError::Encode)?;
    let mut cx = ui::tilo_resource::ActionCx::for_write(doc, &payload);
    let author = cx
        .author()
        .map(str::to_owned)
        .ok_or_else(|| ActionError::Refused("this change needs its author".into()))?;
    let id = cx.invocation().map_or_else(
        || {
            if message.id.is_empty() {
                format!("m_{}", cx.id())
            } else {
                message.id.clone()
            }
        },
        agent_request_message_id,
    );
    if message.text.trim().is_empty() || message.text.chars().count() > 8_000 {
        return Err(ActionError::Refused(
            "an agent message needs bounded text".into(),
        ));
    }
    if !ChatDoc::post_agent_message(
        doc,
        workspace,
        agent,
        &id,
        NewMessage {
            author,
            ts: message.ts,
            text: message.text.clone(),
            body: message.body.clone(),
            attachment: message.attachment.clone(),
            mentions: message.mentions.clone(),
            reference: message.reference.clone(),
            agent_intent: Some(message.intent),
            ..Default::default()
        },
    ) {
        return Err(ActionError::Refused(
            "the agent message is not valid in this conversation".into(),
        ));
    }
    Ok(())
}

impl Action for ChatAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        if let Self::Feedback(action) = self {
            return crate::feedback::apply(action, doc);
        }
        if let ChatAction::PostAgentMessage {
            workspace,
            agent,
            message,
        } = self
        {
            return apply_agent_request(doc, workspace, agent, message);
        }
        if let ChatAction::ReplyAgentMessage {
            channel,
            parent,
            id,
            ts,
            text,
            body,
        } = self
        {
            let author = signed_by()?;
            let _ =
                ChatDoc::reply_agent_message(doc, channel, parent, id, *ts, text, body, &author);
            return Ok(());
        }
        if let ChatAction::EnsureUser {
            name,
            initials,
            color_idx,
            status,
        } = self
        {
            let id = signed_by()?;
            let presence = status.parse().unwrap_or(crate::model::Presence::Online);
            let _ = ChatDoc::ensure_user(doc, &id, name, initials, *color_idx, presence);
            return Ok(());
        }
        if let ChatAction::NameAgent { participant, name } = self {
            ChatDoc::name_agent(doc, participant, &bounded_agent_name(name));
            return Ok(());
        }
        // Each helper matches only its own category's variants (a `_ => {}`
        // no-op for the rest), so exactly one of the three does real work per
        // call — split out of this trait method because a helper that isn't
        // itself a trait method can't live in this `impl Action for ..`
        // block (E0407).
        if let ChatAction::EnsureRichDraft { key } = self {
            return apply_ensure_rich_draft(doc, key);
        }
        if let ChatAction::EditDraft { key, edit } = self {
            apply_ensure_rich_draft(doc, key)?;
            let draft = ChatDoc::rich_draft(doc, key)
                .ok_or_else(|| ActionError::Refused("the draft is gone".into()))?;
            return edit.apply_under(doc, &draft);
        }
        if let ChatAction::ClearDraft { key } = self {
            ChatDoc::clear_draft(doc, key);
            return Ok(());
        }
        if let ChatAction::SeedStarter {
            name,
            initials,
            color_idx,
        } = self
        {
            return apply_seed_starter(doc, name, initials, *color_idx);
        }
        if let ChatAction::SeedDemo {} = self {
            ChatDoc::seed_demo(doc, &author().unwrap_or_else(|| crate::model::ME.into()));
            return Ok(());
        }
        if let ChatAction::SeedWorkspace {
            workspace,
            workspace_name,
            creator_name,
            creator_initials,
            color_idx,
        } = self
        {
            return apply_seed_workspace(
                doc,
                workspace,
                workspace_name,
                creator_name,
                creator_initials,
                *color_idx,
            );
        }
        self.apply_message(doc)?;
        self.apply_channel(doc)?;
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Chat mutations are client-local: each peer applies + replicates its own
        // sends/edits/reactions; peers replay them. There is no authoritative
        // ordering host for a message row.
        ExecutionLocus::ClientLocal
    }

    /// A read watermark is shared state about the channel, not a message. A
    /// record's starting workspace comes from outside and is nobody's undo.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            ChatAction::MarkRead { .. } => tilo_actions::ChangeKind::Housekeeping,
            ChatAction::SeedStarter { .. }
            | ChatAction::SeedWorkspace { .. }
            | ChatAction::SeedDemo {}
            | ChatAction::EnsureUser { .. }
            | ChatAction::NameAgent { .. } => tilo_actions::ChangeKind::External,
            _ => tilo_actions::ChangeKind::Content,
        }
    }
}

impl ChatAction {
    /// Message-row mutations: send, edit, delete, react, pin.
    fn apply_message(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ChatAction::SendMessage {
                channel,
                id,
                ts,
                text,
                parent,
                attachment,
                mentions,
                reference,
                source_provider,
                body,
            } => {
                let author = signed_by()?;
                if source_provider
                    .as_deref()
                    .is_some_and(|provider| reference.is_none() || !valid_source_provider(provider))
                {
                    return Err(ActionError::Refused(
                        "Invalid shared resource provider".into(),
                    ));
                }
                if reference
                    .as_ref()
                    .is_some_and(|reference| !valid_reference(reference))
                {
                    return Err(ActionError::Refused(
                        "Invalid shared resource address".into(),
                    ));
                }
                if !ChatDoc::accepts_message_action(
                    doc,
                    channel,
                    id,
                    text,
                    parent.as_deref(),
                    attachment.as_ref(),
                ) {
                    return Ok(());
                }
                ChatDoc::push_message_with_id(
                    doc,
                    channel,
                    id,
                    NewMessage {
                        author,
                        ts: *ts,
                        text: text.clone(),
                        parent: parent.clone(),
                        attachment: attachment.clone(),
                        mentions: mentions.clone(),
                        reference: reference.clone(),
                        source_provider: source_provider.clone(),
                        body: body.clone(),
                        ..Default::default()
                    },
                );
            }
            ChatAction::EditMessage {
                channel,
                id,
                text,
                ts,
                body,
            } => {
                ChatDoc::edit_message(doc, channel, id, text, body, *ts);
            }
            ChatAction::DeleteMessage { channel, id } => {
                ChatDoc::delete_message(doc, channel, id);
            }
            ChatAction::AddReaction {
                channel,
                msg,
                emoji,
            } => {
                ChatDoc::add_reaction(doc, channel, msg, emoji, &signed_by()?);
            }
            ChatAction::RemoveReaction {
                channel,
                msg,
                emoji,
            } => {
                ChatDoc::remove_reaction(doc, channel, msg, emoji, &signed_by()?);
            }
            ChatAction::PinMessage {
                channel,
                msg,
                pinned,
            } => {
                ChatDoc::set_message_pinned(doc, channel, msg, *pinned);
            }
            _ => {}
        }
        Ok(())
    }

    /// Channel/workspace-structure mutations: create, rename, delete,
    /// archive, mark-read, and the agent-work annotation on a message.
    fn apply_channel(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            ChatAction::CreateChannel {
                workspace,
                category,
                id,
                name,
                kind,
            } => {
                ChatDoc::insert_channel(doc, id, workspace, category, name, *kind);
            }
            ChatAction::CreateCategory {
                workspace,
                id,
                name,
            } => {
                ChatDoc::insert_category(doc, id, workspace, name);
            }
            ChatAction::RenameCategory { id, name } => {
                ChatDoc::rename_category(doc, id, name);
            }
            ChatAction::DeleteCategory { id } => {
                ChatDoc::delete_category(doc, id);
            }
            ChatAction::ReorderChannel { id, up } => {
                ChatDoc::reorder_channel(doc, id, *up);
            }
            ChatAction::RenameChannel { id, name } => {
                ChatDoc::rename_channel(doc, id, name);
            }
            ChatAction::DeleteChannel { id } => {
                ChatDoc::delete_channel(doc, id);
            }
            ChatAction::ArchiveChannel { id, archived } => {
                ChatDoc::set_channel_archived(doc, id, *archived);
            }
            ChatAction::MarkRead { channel, count } => {
                ChatDoc::set_read_watermark(doc, channel, &signed_by()?, *count);
            }
            ChatAction::SetAgentWork {
                channel,
                msg,
                work,
                say,
            } => {
                let author = signed_by()?;
                if ChatDoc::message_author(doc, channel, msg).is_some_and(|wrote| wrote != author) {
                    return Err(ActionError::Refused(
                        "only a message's author records its work".into(),
                    ));
                }
                ChatDoc::set_message_work(doc, channel, msg, work, say.as_deref());
            }
            _ => {}
        }
        Ok(())
    }
}

/// Who invoked the action applying now, as its envelope pinned it: the
/// person or agent an action signs what it writes with.
pub(crate) fn author() -> Option<String> {
    tilo_actions::invoking_context()
        .and_then(|context| context.invoker)
        .map(|invoker| invoker.actor)
        .filter(|author| {
            // The broker's canonical Agent actor includes a namespace prefix;
            // that prefix is not part of the bounded Agent identifier. This
            // does not change ordinary authors or private ParticipantId keys.
            author.strip_prefix("agent:").map_or_else(
                || !author.is_empty() && author.len() <= 128,
                |agent| {
                    !agent.is_empty() && agent.len() <= 128 && !agent.chars().any(char::is_control)
                },
            )
        })
}

/// [`author`], for a change that records who made it: refused without one.
fn signed_by() -> Result<String, ActionError> {
    author().ok_or_else(|| ActionError::Refused("this change needs its author".into()))
}

/// Register Chat's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<ChatAction>(CHAT_ACTION_TYPE);
}

/// Encode any [`ChatAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Chat action into a
/// replayable envelope.
///
/// # Panics
/// Never in practice: every [`ChatAction`] field is a plain, always-encodable
/// value (String/i64/bool/Option/Vec and the same kind of struct/enum
/// recursively), so the underlying JSON encode cannot fail short of a
/// serializer bug. Returning a `Result` here would ripple into every
/// `build: Arc<dyn Fn(&str) -> EncodedAction>` closure below, which the
/// action-spec contract requires to hand back an `EncodedAction` directly.
#[allow(
    clippy::expect_used,
    reason = "ChatAction is a plain serde-derived enum with no non-serializable content, so encode cannot fail short of a serializer bug; the ActionInput::Prompted closure contract requires returning EncodedAction directly, not a Result"
)]
pub fn encode(action: &ChatAction) -> EncodedAction {
    EncodedAction::encode(CHAT_ACTION_TYPE, action).expect("ChatAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::events`) and the Ctrl+K resource tree below.
// ===========================================================================

fn bounded_agent_name(name: &str) -> String {
    let name = name.trim();
    let name = if name.is_empty() { "Agent" } else { name };
    let end = name.floor_char_boundary(MAX_AGENT_AUTHOR_NAME_BYTES);
    name.get(..end).unwrap_or(name).to_owned()
}

pub fn reply_agent_message_action(
    channel: &str,
    parent: &str,
    id: &str,
    ts: i64,
    text: &str,
) -> ChatAction {
    ChatAction::ReplyAgentMessage {
        channel: channel.to_owned(),
        parent: parent.to_owned(),
        id: id.to_owned(),
        ts,
        text: text.to_owned(),
        body: Vec::new(),
    }
}

pub fn send_message_action(
    channel: &str,
    id: &str,
    ts: i64,
    text: &str,
    parent: Option<String>,
    attachment: Option<Attachment>,
    mentions: Vec<crate::mention::Mention>,
) -> ChatAction {
    ChatAction::SendMessage {
        channel: channel.to_string(),
        id: id.to_string(),
        ts,
        text: text.to_string(),
        parent,
        attachment,
        mentions,
        reference: None,
        source_provider: None,
        body: Vec::new(),
    }
}

impl ChatAction {
    /// A send or an edit carrying the body as written (`text` its plain
    /// text); any other action unchanged.
    #[must_use]
    pub fn with_body(mut self, written: Vec<tilo_document::ClipBlock>) -> Self {
        match &mut self {
            ChatAction::SendMessage { body, .. }
            | ChatAction::ReplyAgentMessage { body, .. }
            | ChatAction::EditMessage { body, .. } => {
                *body = written;
            }
            ChatAction::PostAgentMessage { message, .. } => message.body = written,
            _ => {}
        }
        self
    }
}

pub fn edit_message_action(channel: &str, id: &str, text: &str, ts: i64) -> ChatAction {
    ChatAction::EditMessage {
        channel: channel.to_string(),
        id: id.to_string(),
        text: text.to_string(),
        ts,
        body: Vec::new(),
    }
}

pub fn delete_message_action(channel: &str, id: &str) -> ChatAction {
    ChatAction::DeleteMessage {
        channel: channel.to_string(),
        id: id.to_string(),
    }
}

pub fn add_reaction_action(channel: &str, msg: &str, emoji: &str) -> ChatAction {
    ChatAction::AddReaction {
        channel: channel.to_string(),
        msg: msg.to_string(),
        emoji: emoji.to_string(),
    }
}

pub fn remove_reaction_action(channel: &str, msg: &str, emoji: &str) -> ChatAction {
    ChatAction::RemoveReaction {
        channel: channel.to_string(),
        msg: msg.to_string(),
        emoji: emoji.to_string(),
    }
}

pub fn pin_message_action(channel: &str, msg: &str, pinned: bool) -> ChatAction {
    ChatAction::PinMessage {
        channel: channel.to_string(),
        msg: msg.to_string(),
        pinned,
    }
}

pub fn create_channel_action(
    workspace: &str,
    category: &str,
    id: &str,
    name: &str,
    kind: ChannelKind,
) -> ChatAction {
    ChatAction::CreateChannel {
        workspace: workspace.to_string(),
        category: category.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        kind,
    }
}

pub fn create_category_action(workspace: &str, id: &str, name: &str) -> ChatAction {
    ChatAction::CreateCategory {
        workspace: workspace.to_string(),
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn rename_category_action(id: &str, name: &str) -> ChatAction {
    ChatAction::RenameCategory {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_category_action(id: &str) -> ChatAction {
    ChatAction::DeleteCategory { id: id.to_string() }
}

pub fn reorder_channel_action(id: &str, up: bool) -> ChatAction {
    ChatAction::ReorderChannel {
        id: id.to_string(),
        up,
    }
}

pub fn rename_channel_action(id: &str, name: &str) -> ChatAction {
    ChatAction::RenameChannel {
        id: id.to_string(),
        name: name.to_string(),
    }
}

pub fn delete_channel_action(id: &str) -> ChatAction {
    ChatAction::DeleteChannel { id: id.to_string() }
}

pub fn archive_channel_action(id: &str, archived: bool) -> ChatAction {
    ChatAction::ArchiveChannel {
        id: id.to_string(),
        archived,
    }
}

pub fn mark_read_action(channel: &str, count: i64) -> ChatAction {
    ChatAction::MarkRead {
        channel: channel.to_string(),
        count,
    }
}

/// Record an agent's progress on the message `msg` replies to. `say` is the
/// agent's words, and is `None` until it has any — the ordinary case for
/// every state before the last one.
pub fn set_agent_work_action(
    channel: &str,
    msg: &str,
    mut work: crate::model::AgentWork,
    say: Option<&str>,
) -> ChatAction {
    // Stable trace ids are minted exactly once, before the action crosses the
    // replay boundary. A peer applying this payload only writes these ids.
    work.ensure_trace_ids();
    ChatAction::SetAgentWork {
        channel: channel.to_string(),
        msg: msg.to_string(),
        work,
        say: say.map(str::to_string),
    }
}

// ===========================================================================
// The published tree
// ===========================================================================

/// A channel as the published tree names it.
#[derive(Debug, Clone)]
pub struct ChannelResource {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub archived: bool,
    pub workspace: String,
    /// Stable agent target for a direct conversation. These channels do not
    /// publish the ordinary free-form channel mutation actions.
    pub conversation_with: Option<String>,
}

/// A durable channel grouping published alongside channels. Categories are
/// not merely paint-time headings: rename and delete are shared document
/// facts and therefore have ordinary resource actions and history; folding
/// one away is each person's own.
#[derive(Debug, Clone)]
pub struct CategoryResource {
    pub id: String,
    pub name: String,
}

/// A message as the published tree and search see it: who wrote it, a
/// display label (author + snippet), its words, and the rows hanging off it.
#[derive(Debug, Clone)]
pub struct MessageResource {
    pub id: String,
    pub channel: String,
    /// Who wrote it: an agent records its work only on its own messages.
    pub author: String,
    /// Whether that author is an agent.
    pub agent: bool,
    pub label: String,
    pub text: String,
    pub pinned: bool,
    pub parent: Option<String>,
    /// Address of the direct conversation containing this row, when any.
    pub conversation_with: Option<String>,
    /// Explicit direct-agent intent; ordinary rows leave it absent.
    pub agent_intent: Option<crate::model::AgentIntent>,
    /// An attachment is a durable, content-addressed child of the message,
    /// rather than an unsearchable bit of its paint. There can be only one in
    /// the current message schema, so its stable address is keyed by message.
    pub attachment: Option<AttachmentResource>,
    /// Live resource address; sharing it does not grant access to its target.
    pub reference: Option<tilo_reference::ResourceRef>,
    pub source_provider: Option<String>,
    /// Reactions are shared rows keyed by their canonical emoji value. Keeping
    /// them as children makes a reaction discoverable and actionable without
    /// forcing callers to parse the message body or inspect a rendered chip.
    pub reactions: Vec<ReactionResource>,
}

#[derive(Debug, Clone)]
pub struct AttachmentResource {
    pub name: String,
    pub blob: String,
}

#[derive(Debug, Clone)]
pub struct ReactionResource {
    pub emoji: String,
    pub count: usize,
}

/// Keep the published tree cheap even for a very large shared organisation:
/// a bounded page is published, and every omitted row resolves by id
/// ([`resolve_child`]).
pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;
const EAGER_WORKSPACE_LIMIT: usize = 8;
const EAGER_PEOPLE_LIMIT: usize = 23;
/// How many of a channel's latest messages it lists.
pub const RESOLVED_CHANNEL_MESSAGES: usize = 20;

/// # Panics
/// Never in practice: `ActionSpec::immediate` only fails if encoding `action`
/// fails, and — as with [`encode`] — a `ChatAction` is always encodable.
#[allow(
    clippy::expect_used,
    reason = "ChatAction is always encodable (see encode's reason), and a published child carries an ActionSpec directly, not a Result"
)]
fn immediate_spec(id: &str, label: &str, action: &ChatAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, CHAT_ACTION_TYPE, action)
        .expect("encode chat action")
        .reversible();
    match action {
        ChatAction::DeleteMessage { .. }
        | ChatAction::DeleteChannel { .. }
        | ChatAction::DeleteCategory { .. } => spec.destructive(),
        _ => spec,
    }
}

/// Attach the same operational promises to prompted and immediate document
/// actions. Chat's document executor supplies one undo boundary for each, so
/// all are reversible; only removal of durable rows needs confirmation.
fn annotate_contracts(resource: &mut Resource) {
    for action in &mut resource.actions {
        if action.kind == OperationKind::Document {
            action.contract.reversible = true;
        }
        if action.kind == OperationKind::Document && action.id == "delete" {
            let old = action.clone();
            *action = old.destructive();
        }
    }
    for child in &mut resource.children {
        annotate_contracts(child);
    }
}

/// A channel's palette label: `#name` for text, named as a voice channel
/// for voice.
fn channel_label(ch: &ChannelResource) -> String {
    let name = if ch.name.trim().is_empty() {
        "channel"
    } else {
        ch.name.trim()
    };
    ch.kind.label(name)
}

fn channel_resource(channel: &crate::model::Channel) -> ChannelResource {
    ChannelResource {
        id: channel.id.clone(),
        name: channel.name.clone(),
        kind: channel.kind,
        archived: channel.archived,
        workspace: channel.workspace.clone(),
        conversation_with: channel.conversation_with.clone(),
    }
}

/// A workspace: open it, and create a channel or a category in it.
fn publish_workspace(id: &str, name: &str) -> Resource {
    Resource::new(
        format!("chat.workspace:{id}"),
        "chat.workspace",
        if name.trim().is_empty() {
            "Workspace"
        } else {
            name
        },
    )
    .with_action(
        ChatEffect::OpenWorkspace {
            workspace: id.to_owned(),
        }
        .bound(),
    )
    .with_view(ResourceViewSpec::new("workspace", "Workspace", "open-workspace").with_primary(true))
    .with_action(new_channel_action_spec(id))
    .with_action(new_category_action_spec(id))
}

/// A channel: open, read, post and attach (a text channel), rename,
/// archive, mark read, delete; its call (a voice channel) and its latest
/// messages.
fn publish_channel(
    state: &crate::resource::ChatState,
    ch: &ChannelResource,
    messages: &[MessageResource],
) -> Resource {
    let mut channel = Resource::new(
        format!("chat.channel:{}", ch.id),
        "chat.channel",
        channel_label(ch),
    )
    .with_action(crate::semantic_read::action::spec(&ch.id))
    .with_action(
        ChatEffect::OpenChannel {
            channel: ch.id.clone(),
        }
        .bound(),
    );
    // Only a text channel has a timeline anyone can see (opening a voice
    // channel joins the call), so post is offered there alone, exactly like
    // the composer. `@Name` of a member becomes a structured mention, the
    // same as typing it in the composer.
    if ch.kind != ChannelKind::Voice {
        channel = channel.with_view(
            ResourceViewSpec::new("conversation", "Conversation", "open-channel")
                .with_primary(true),
        );
        if ch.conversation_with.is_none() {
            channel = channel.with_action(share_resource_spec(&ch.id));
            channel = channel.with_action(crate::feedback::submit_spec(&ch.id));
            channel = channel.with_action(crate::feedback::destination_read::spec(&ch.id));
            channel = channel.with_action(ActionSpec::prompted(
                "post",
                "Post message",
                "Message",
                "type a message… (@Name mentions a member)",
                ExecutionLocus::ClientLocal,
                {
                    let cid = ch.id.clone();
                    let room = state.participants();
                    move |text: &str| {
                        let id = format!("m_{}", uuid::Uuid::new_v4().simple());
                        let mentions = crate::mention::resolve(text, &room);
                        encode(&send_message_action(
                            &cid,
                            &id,
                            unix_now_i64(),
                            text,
                            None,
                            None,
                            mentions,
                        ))
                    }
                },
            ));
        }
    }
    if ch.conversation_with.is_none() {
        channel = channel.with_action(ActionSpec::prompted(
            "rename",
            "Rename channel",
            "Name",
            "Channel name",
            ExecutionLocus::ClientLocal,
            {
                let cid = ch.id.clone();
                move |name: &str| encode(&rename_channel_action(&cid, name.trim()))
            },
        ));
        channel = channel.with_action(if ch.archived {
            immediate_spec(
                "unarchive",
                "Unarchive channel",
                &archive_channel_action(&ch.id, false),
            )
        } else {
            immediate_spec(
                "archive",
                "Archive channel",
                &archive_channel_action(&ch.id, true),
            )
        });
    }
    // The watermark target is the channel's top-level count when the action
    // is built, carried in the payload.
    let top_level = messages
        .iter()
        .filter(|message| message.parent.is_none())
        .count();
    channel = channel.with_action(immediate_spec(
        "mark-read",
        "Mark as read",
        &mark_read_action(&ch.id, i64::try_from(top_level).unwrap_or(i64::MAX)),
    ));
    if ch.conversation_with.is_none() {
        channel = channel.with_action(immediate_spec(
            "delete",
            "Delete channel",
            &delete_channel_action(&ch.id),
        ));
    }
    if ch.kind == ChannelKind::Voice {
        channel = channel.with_child(publish_call(ch));
    }
    let recent = messages.len().saturating_sub(RESOLVED_CHANNEL_MESSAGES);
    for message in messages.get(recent..).unwrap_or_default() {
        channel = channel.with_child(publish_message(message));
    }
    annotate_contracts(&mut channel);
    channel
}

/// A portable address, never copied content, a local path or an access grant.
fn valid_reference(reference: &tilo_reference::ResourceRef) -> bool {
    let field = |value: &str, max: usize| {
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    field(&reference.kind, 128)
        && reference
            .kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && field(&reference.id, 2048)
        && !reference.id.starts_with(['/', '\\'])
        && !reference.id.contains("://")
        && field(&reference.label, 512)
        && reference.span.is_none_or(|span| span.start <= span.end)
}

/// An existing typed record provider. The host resolves and authorizes it;
/// storing this address neither admits a collaborator nor grants Read.
pub(crate) fn valid_source_provider(provider: &str) -> bool {
    let Some(rest) = provider.strip_prefix("record:") else {
        return false;
    };
    let Some((app, _record)) = rest.split_once(':') else {
        return false;
    };
    provider.len() <= 2048
        && !app.is_empty()
        && !provider.chars().any(char::is_control)
        && !provider.contains("://")
        && !provider.contains('/')
        && !provider.contains('\\')
}

fn share_resource_spec(channel: &str) -> ActionSpec {
    use tilo_actions::{ActionParam, ParamType};
    let channel = channel.to_owned();
    ActionSpec::typed(
        "share-resource",
        "Share live resource",
        "Share an address; existing resource permissions still apply",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "reference",
                ParamType::Object,
                "Tilo resource: kind, id, label, span?",
            )
            .with_schema(serde_json::json!({
                "type": "object", "additionalProperties": false,
                "required": ["kind", "id", "label"],
                "properties": {
                    "kind": {"type": "string", "maxLength": 128},
                    "id": {"type": "string", "maxLength": 2048},
                    "label": {"type": "string", "maxLength": 512},
                    "span": {"type": ["object", "null"], "additionalProperties": false,
                        "required": ["start", "end", "unit"],
                        "properties": {"start": {"type": "integer", "minimum": 0},
                            "end": {"type": "integer", "minimum": 0},
                            "unit": {"type": "string", "enum": ["Chars", "Bytes"]}}}
                }
            })),
            ActionParam::optional("text", ParamType::Text, "Words accompanying the resource"),
            ActionParam::optional(
                "sourceProvider",
                ParamType::Text,
                "Existing record provider; required to open or copy a browser link",
            ),
        ],
        move |args| {
            let reference = args.get("reference").and_then(|value| {
                serde_json::from_value::<tilo_reference::ResourceRef>(value.to_json()).ok()
            });
            let Some(reference) = reference.filter(valid_reference) else {
                return EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: b"A bounded Tilo resource address is required".to_vec(),
                };
            };
            let source_provider = args.text("sourceProvider").map(str::to_owned);
            if source_provider
                .as_deref()
                .is_some_and(|value| !valid_source_provider(value))
            {
                return EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: b"An existing record provider is required".to_vec(),
                };
            }
            let text = args
                .text("text")
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(&reference.label);
            if text.len() > crate::doc::MAX_MESSAGE_TEXT_BYTES {
                return EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: b"Shared resource message is too long".to_vec(),
                };
            }
            encode(&ChatAction::SendMessage {
                channel: channel.clone(),
                id: format!("m_{}", uuid::Uuid::new_v4().simple()),
                ts: unix_now_i64(),
                text: text.to_owned(),
                parent: None,
                attachment: None,
                mentions: Vec::new(),
                reference: Some(reference),
                source_provider,
                body: Vec::new(),
            })
        },
    )
}

/// A voice channel's call: the shell's voice service, controlled from this
/// device. Nothing about it is written to the document.
fn publish_call(ch: &ChannelResource) -> Resource {
    let channel = || ch.id.clone();
    Resource::new(
        format!("chat.call:{}", ch.id),
        "chat.call",
        crate::i18n::t_args("chat-call-title", &[("channel", channel_label(ch).into())]),
    )
    .with_action(ChatEffect::OpenChannel { channel: channel() }.bound())
    .with_action(ChatEffect::JoinCall { channel: channel() }.bound())
    .with_action(ChatEffect::LeaveCall { channel: channel() }.bound())
    .with_action(ChatEffect::Mute { channel: channel() }.bound())
    .with_action(ChatEffect::Unmute { channel: channel() }.bound())
    .with_action(ChatEffect::CallStatus { channel: channel() }.bound())
}

/// A message: reveal and read it, edit, react, pin or unpin, delete; an
/// agent's record of its work on its own message; its attachment and
/// reactions.
fn with_resource_navigation(mut message: Resource, m: &MessageResource) -> Resource {
    if m.reference.is_some() && m.source_provider.is_some() {
        message = message
            .with_action(
                ChatEffect::CopyResourceLink {
                    message: m.id.clone(),
                }
                .bound(),
            )
            .with_action(
                ChatEffect::OpenResourceLink {
                    message: m.id.clone(),
                }
                .bound(),
            );
    }
    message
}

fn publish_message(m: &MessageResource) -> Resource {
    let reveal = || {
        ChatEffect::RevealMessage {
            message: m.id.clone(),
        }
        .bound()
    };
    let mut message = Resource::new(
        format!("chat.message:{}", m.id),
        "chat.message",
        m.label.clone(),
    )
    .with_action(reveal())
    .with_view(
        ResourceViewSpec::new("message", "Message", "reveal-message")
            .with_primary(true)
            .with_range(ResourceViewRangeSpec::new("reveal-message-range")),
    )
    .with_action(
        ChatEffect::RevealMessageRange {
            message: m.id.clone(),
            start: 0,
            end: 0,
            unit: "Chars".into(),
        }
        .bound_asking(&["start", "end", "unit"]),
    )
    .with_action(
        ChatEffect::ReadMessage {
            message: m.id.clone(),
        }
        .bound(),
    );
    message = with_resource_navigation(message, m);
    message = message.with_action(ActionSpec::prompted(
        "edit",
        "Edit message",
        "Message",
        "Message text",
        ExecutionLocus::ClientLocal,
        {
            let cid = m.channel.clone();
            let mid = m.id.clone();
            move |text: &str| encode(&edit_message_action(&cid, &mid, text, unix_now_i64()))
        },
    ));
    message = message.with_action(ActionSpec::prompted(
        "react",
        "Add reaction",
        "Emoji",
        "👍",
        ExecutionLocus::ClientLocal,
        {
            let cid = m.channel.clone();
            let mid = m.id.clone();
            move |emoji: &str| encode(&add_reaction_action(&cid, &mid, emoji.trim()))
        },
    ));
    message = message.with_action(if m.pinned {
        immediate_spec(
            "unpin",
            "Unpin message",
            &pin_message_action(&m.channel, &m.id, false),
        )
    } else {
        immediate_spec(
            "pin",
            "Pin message",
            &pin_message_action(&m.channel, &m.id, true),
        )
    });
    message = message.with_action(immediate_spec(
        "delete",
        "Delete message",
        &delete_message_action(&m.channel, &m.id),
    ));
    if m.agent {
        message = message.with_action(set_work_spec(&m.channel, &m.id));
    }
    if let Some(attachment) = &m.attachment {
        message = message.with_child(
            Resource::new(
                format!("chat.attachment:{}", m.id),
                "chat.attachment",
                attachment.name.clone(),
            )
            .with_action(reveal()),
        );
    }
    for reaction in &m.reactions {
        message = message.with_child(publish_reaction(m, reaction, reveal()));
    }
    message
}

/// A reaction: both absolute edits of the reader's own reaction, the same
/// the painted chip dispatches.
fn publish_reaction(
    m: &MessageResource,
    reaction: &ReactionResource,
    reveal: ActionSpec,
) -> Resource {
    Resource::new(
        format!("chat.reaction:{}:{}", m.id, reaction.emoji),
        "chat.reaction",
        reaction.emoji.clone(),
    )
    .with_presentation(ui::tilo_actions::ResourcePresentation {
        detail: Some(format!("{} reactions", reaction.count)),
        ..Default::default()
    })
    .with_action(reveal)
    .with_action(immediate_spec(
        "add",
        "Add my reaction",
        &add_reaction_action(&m.channel, &m.id, &reaction.emoji),
    ))
    .with_action(immediate_spec(
        "remove",
        "Remove my reaction",
        &remove_reaction_action(&m.channel, &m.id, &reaction.emoji),
    ))
}

/// A category: open, fold or unfold, rename, delete.
fn publish_category(category: &CategoryResource) -> Resource {
    let mut resource = Resource::new(
        format!("chat.category:{}", category.id),
        "chat.category",
        category.name.clone(),
    )
    .with_action(
        ChatEffect::OpenCategory {
            category: category.id.clone(),
        }
        .bound(),
    )
    .with_action(ActionSpec::prompted(
        "rename",
        "Rename category",
        "Name",
        "Category name",
        ExecutionLocus::ClientLocal,
        {
            let id = category.id.clone();
            move |name: &str| encode(&rename_category_action(&id, name.trim()))
        },
    ));
    resource = resource.with_action(
        ChatEffect::ToggleCategory {
            category: category.id.clone(),
        }
        .bound(),
    );
    resource = resource.with_action(immediate_spec(
        "delete",
        "Delete category",
        &delete_category_action(&category.id),
    ));
    annotate_contracts(&mut resource);
    resource
}

fn publish_person(user: &crate::model::User) -> Resource {
    Resource::new(
        format!("chat.person:{}", user.id),
        "chat.person",
        crate::identity::shared_name_label(&user.id, &user.name),
    )
    .with_action(
        ChatEffect::OpenPerson {
            user: user.id.clone(),
        }
        .bound(),
    )
}

fn people_resource<'a>(people: impl IntoIterator<Item = &'a crate::model::User>) -> Resource {
    let mut node = Resource::new("chat.people", "chat.people", "People");
    for user in people {
        node = node.with_child(publish_person(user));
    }
    node
}

fn category_resource(category: &crate::model::Category) -> CategoryResource {
    CategoryResource {
        id: category.id.clone(),
        name: category.name.clone(),
    }
}

/// The workspaces, their channels (with their latest messages) and
/// categories, and the people page, as one bounded page of the published
/// tree.
pub(crate) fn publish_children(state: &crate::resource::ChatState) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT
        .saturating_sub(1)
        .saturating_sub(state.draft_keys.len())
        .saturating_sub(1 + EAGER_PEOPLE_LIMIT);
    let mut workspaces = state.workspaces.clone();
    workspaces.sort_by_key(|workspace| workspace.order);
    for ws in workspaces.into_iter().take(EAGER_WORKSPACE_LIMIT) {
        if remaining == 0 {
            break;
        }
        let mut workspace = publish_workspace(&ws.id, &ws.name);
        remaining = remaining.saturating_sub(1);
        let mut channels = state
            .channels
            .iter()
            .filter(|channel| channel.workspace == ws.id)
            .collect::<Vec<_>>();
        channels.sort_by_key(|channel| channel.order);
        for channel in channels.into_iter().take(remaining) {
            workspace = workspace.with_child(publish_channel(
                state,
                &channel_resource(channel),
                state.messages_of(&channel.id),
            ));
            remaining = remaining.saturating_sub(1);
        }
        let mut categories = state
            .categories
            .iter()
            .filter(|category| category.workspace == ws.id)
            .collect::<Vec<_>>();
        categories.sort_by_key(|category| category.order);
        for category in categories.into_iter().take(remaining) {
            workspace = workspace.with_child(publish_category(&category_resource(category)));
            remaining = remaining.saturating_sub(1);
        }
        annotate_contracts(&mut workspace);
        children.push(workspace);
    }
    children.push(people_resource(state.users.iter().take(EAGER_PEOPLE_LIMIT)));
    children
}

/// Resource `id` from the whole state, when the bounded page leaves it out.
/// A stale id resolves to nothing.
pub(crate) fn resolve_child(state: &crate::resource::ChatState, id: &str) -> Option<Resource> {
    if id == "chat.people" {
        return Some(people_resource(&state.users));
    }
    if let Some(workspace) = id.strip_prefix("chat.workspace:") {
        let workspace = state.workspaces.iter().find(|ws| ws.id == workspace)?;
        let mut resource = publish_workspace(&workspace.id, &workspace.name);
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(channel) = id.strip_prefix("chat.channel:") {
        let channel = state.channels.iter().find(|ch| ch.id == channel)?;
        return Some(publish_channel(
            state,
            &channel_resource(channel),
            state.messages_of(&channel.id),
        ));
    }
    if let Some(channel) = id.strip_prefix("chat.call:") {
        let channel = state
            .channels
            .iter()
            .find(|ch| ch.id == channel && ch.kind == ChannelKind::Voice)?;
        return Some(publish_call(&channel_resource(channel)));
    }
    if let Some(category) = id.strip_prefix("chat.category:") {
        let category = state.categories.iter().find(|c| c.id == category)?;
        return Some(publish_category(&category_resource(category)));
    }
    if let Some(user) = id.strip_prefix("chat.person:") {
        return state
            .users
            .iter()
            .find(|person| person.id == user)
            .map(publish_person);
    }
    if let Some(message) = id.strip_prefix("chat.attachment:") {
        let message = publish_message(state.message(message)?);
        return message
            .children
            .into_iter()
            .find(|child| child.id.as_str() == id);
    }
    if id.starts_with("chat.reaction:") {
        // Emoji is unconstrained Unicode and may itself contain ':', so the
        // id is matched against every message's reaction rows rather than
        // split heuristically.
        return state.messages().find_map(|message| {
            message
                .reactions
                .iter()
                .find(|reaction| format!("chat.reaction:{}:{}", message.id, reaction.emoji) == id)
                .map(|reaction| {
                    publish_reaction(
                        message,
                        reaction,
                        ChatEffect::RevealMessage {
                            message: message.id.clone(),
                        }
                        .bound(),
                    )
                })
        });
    }
    let message = state.message(id.strip_prefix("chat.message:")?)?;
    let mut resource = publish_message(message);
    annotate_contracts(&mut resource);
    Some(resource)
}

/// Workspace-level: create a new text channel in the uncategorised group. The
/// fresh id is minted once, here, at authoring time (like the on-screen "+").
fn new_channel_action_spec(workspace_id: &str) -> ActionSpec {
    ActionSpec::prompted(
        "new-channel",
        "New channel",
        "Name",
        "Channel name",
        ExecutionLocus::ClientLocal,
        {
            let ws = workspace_id.to_string();
            move |name: &str| {
                let name = name.trim();
                let name = if name.is_empty() { "new-channel" } else { name };
                let id = format!("c_{}", uuid::Uuid::new_v4().simple());
                encode(&create_channel_action(
                    &ws,
                    "",
                    &id,
                    name,
                    ChannelKind::Text,
                ))
            }
        },
    )
}

fn new_category_action_spec(workspace_id: &str) -> ActionSpec {
    ActionSpec::prompted(
        "new-category",
        "New category",
        "Name",
        "Category name",
        ExecutionLocus::ClientLocal,
        {
            let workspace = workspace_id.to_string();
            move |name: &str| {
                let name = name.trim();
                let name = if name.is_empty() {
                    "New category"
                } else {
                    name
                };
                let id = format!("cat_{}", uuid::Uuid::new_v4().simple());
                encode(&create_category_action(&workspace, &id, name))
            }
        },
    )
}

/// An agent's record of what it did, on a message it wrote: the same
/// [`ChatAction::SetAgentWork`] a delivered agent task writes.
fn set_work_spec(channel: &str, message: &str) -> ActionSpec {
    use tilo_actions::{ActionParam, ParamType};
    let channel = channel.to_owned();
    let message = message.to_owned();
    ActionSpec::typed(
        "set-work",
        "Record agent work",
        "taken, doing, done or refused",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("state", ParamType::Text, "Where the work stands")
                .with_choices(["taken", "doing", "done", "refused"]),
            ActionParam::optional(
                "did",
                ParamType::Text,
                "The operation, as its app labels it (doing, done)",
            ),
            ActionParam::optional("about", ParamType::Text, "What it is about (doing)"),
            ActionParam::optional("route", ParamType::Text, "How the operation was chosen")
                .with_choices(["model", "floor"]),
            ActionParam::optional("why", ParamType::Text, "How the agent decided"),
            ActionParam::optional(
                "steps",
                ParamType::Array,
                "Operations dispatched: {resource, label, outcome: asked|applied|no_change|refused, why?}",
            ),
            ActionParam::optional(
                "say",
                ParamType::Text,
                "The agent's words, for a message that has none yet",
            ),
        ],
        move |args| match agent_work_from_args(args) {
            Ok(work) => encode(&set_agent_work_action(
                &channel,
                &message,
                work,
                args.text("say"),
            )),
            Err(error) => EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: error.into_bytes(),
            },
        },
    )
}

fn agent_work_from_args(
    args: &tilo_actions::ActionArgs,
) -> Result<crate::model::AgentWork, String> {
    use crate::model::{
        AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
    };
    let did = || {
        args.text("did")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "doing and done work name the operation".to_string())
    };
    let state = match args.text("state").unwrap_or_default() {
        "taken" => AgentWorkState::Taken,
        "doing" => AgentWorkState::Doing {
            did: did()?,
            about: args.text("about").unwrap_or_default().to_owned(),
        },
        "done" => AgentWorkState::Done { did: did()? },
        "refused" => AgentWorkState::Refused,
        other => return Err(format!("unknown agent work state `{other}`")),
    };
    let mut trace = Vec::new();
    for (index, step) in args.array("steps").unwrap_or_default().iter().enumerate() {
        let text = |field: &str| step.get(field).and_then(serde_json::Value::as_str);
        let resource = text("resource").ok_or("a step names its resource")?;
        if resource.trim().is_empty() {
            return Err("a step names a non-empty resource".into());
        }
        let label = text("label").ok_or("a step names its operation's label")?;
        if label.trim().is_empty() {
            return Err("a step names a non-empty operation label".into());
        }
        let outcome = match text("outcome").ok_or("a step names its outcome")? {
            "asked" => AgentStepOutcome::Asked,
            "applied" => AgentStepOutcome::Applied,
            "no_change" => AgentStepOutcome::NoChange,
            "refused" => AgentStepOutcome::Refused {
                why: text("why").unwrap_or_default().to_owned(),
            },
            other => return Err(format!("unknown step outcome `{other}`")),
        };
        trace.push(AgentStep::authored(resource, label, outcome, index as u64));
    }
    let mut work = AgentWork::new(state, unix_now_i64()).with_trace(trace);
    let route = match args.text("route") {
        Some("model") => Some(AgentRoute::Model),
        Some("floor") => Some(AgentRoute::Floor),
        Some(other) => return Err(format!("unknown agent reasoning route `{other}`")),
        None if args.text("why").is_some() => {
            return Err("agent reasoning names whether the model or floor chose it".into());
        }
        None => None,
    };
    if let Some(route) = route {
        work = work.because(AgentReasoning {
            route,
            why: args.text("why").unwrap_or_default().to_owned(),
        });
    }
    Ok(work)
}

pub(crate) fn message_label_for_resource(
    author_id: &str,
    text: &str,
    attachment: Option<&crate::model::Attachment>,
    users: &[crate::model::User],
    agents: &std::collections::HashMap<String, String>,
) -> String {
    let author = agents.get(author_id).map_or_else(
        || {
            users
                .iter()
                .find(|user| user.id == author_id)
                .map_or(author_id, |user| user.name.as_str())
        },
        String::as_str,
    );
    let body = if text.trim().is_empty() {
        match attachment {
            Some(
                crate::model::Attachment::File { name, .. }
                | crate::model::Attachment::Image { name, .. }
                | crate::model::Attachment::Video { name, .. },
            ) => name.as_str(),
            None => "(no text)",
        }
    } else {
        text.trim()
    };
    let snippet = body.chars().take(72).collect::<String>();
    format!("{author}: {snippet}")
}

/// Apply `action` as `author` invoked it, as a window's dispatcher does.
#[cfg(test)]
pub(crate) fn apply_as(
    doc: &mut Doc,
    author: &str,
    action: &ChatAction,
) -> Result<(), ActionError> {
    let context = history_context(action).with_invoker(tilo_actions::ActionInvoker::actor(author));
    tilo_actions::apply_local_with_context(doc, action, encode(action), None, context).map(|_| ())
}

#[cfg(test)]
fn apply_as_with_source(
    doc: &mut Doc,
    author: &str,
    source: tilo_actions::InvocationId,
    action: &ChatAction,
) -> Result<(), ActionError> {
    let context = history_context(action)
        .with_invoker(tilo_actions::ActionInvoker::actor(author))
        .with_source_invocation(Some(source));
    tilo_actions::apply_local_with_context(doc, action, encode(action), None, context).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ChatDoc;
    use crate::model::{self, ChannelKind};
    use crate::resource::{Chat, ChatState};
    use automerge::AutoCommit;
    use ui::tilo_resource::Resource as _;

    /// A fixed authoring timestamp for tests — actions carry it in the payload,
    /// so nothing here reads the wall-clock.
    const TS: i64 = 1_782_000_000;

    #[test]
    fn shared_agent_channel_identity_matches_original_fnv_wire_identity() {
        use std::hash::Hasher as _;
        for (workspace, agent) in [("", ""), ("workspace", "scribe"), ("ab", "c"), ("a", "bc")] {
            let part = |suffix| {
                let mut hash = fnv::FnvHasher::default();
                hash.write(b"tilo.chat.agent-conversation.v1\0");
                hash.write(workspace.as_bytes());
                hash.write_u8(0);
                hash.write(agent.as_bytes());
                hash.write_u8(suffix);
                hash.finish()
            };
            let id = uuid::Uuid::from_u128(u128::from(part(0)) << 64 | u128::from(part(1)));
            assert_eq!(
                ChatDoc::agent_conversation_channel_id(workspace, agent),
                format!("c_agent_{}", id.simple())
            );
        }
    }

    #[test]
    fn shared_agent_payload_preserves_exact_plain_wire_and_serde_defaults() {
        let action = post_agent_message_action(
            "workspace",
            "scribe",
            "hello",
            model::AgentIntent::Message,
            7,
        );
        let bytes = serde_json::to_vec(&action).unwrap();
        assert_eq!(bytes, br#"{"PostAgentMessage":{"workspace":"workspace","agent":"scribe","message":{"id":"","ts":7,"text":"hello","intent":"message","attachment":null,"mentions":[],"reference":null}}}"#);
        let shared: tilo_actions::agent_message::AgentMessageMutation<
            tilo_actions::agent_message::SerializedAgentMessagePayload,
        > = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&shared).unwrap(),
            serde_json::to_value(&action).unwrap()
        );
        let omitted = br#"{"PostAgentMessage":{"workspace":"workspace","agent":"scribe","message":{"ts":7,"text":"hello","intent":"message"}}}"#;
        let decoded: ChatAction = serde_json::from_slice(omitted).unwrap();
        assert_eq!(decoded, action);
    }

    #[test]
    fn shared_agent_payload_retains_rich_body_and_attachment_as_inert_owner_data() {
        let input = serde_json::json!({"PostAgentMessage": {
            "workspace": "workspace", "agent": "scribe", "message": {
                "ts": 7, "text": "hello", "intent": "message",
                "attachment": {"Image": {"name": "image", "blob": "opaque-content-address", "w": 2, "h": 3}},
                "body": [{"kind": "paragraph", "text": "hello", "marks": [], "level": null, "ordered": null, "align": null, "attrs": {}}]
            }
        }});
        let action: ChatAction = serde_json::from_value(input.clone()).unwrap();
        let shared: tilo_actions::agent_message::AgentMessageMutation<
            tilo_actions::agent_message::SerializedAgentMessagePayload,
        > = serde_json::from_value(input).unwrap();
        let app_wire = serde_json::to_value(&action).unwrap();
        assert_eq!(app_wire, serde_json::to_value(&shared).unwrap());
        let body = &app_wire["PostAgentMessage"]["message"]["body"];
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["text"], "hello");
        assert_eq!(
            app_wire["PostAgentMessage"]["message"]["attachment"]["Image"]["blob"],
            "opaque-content-address"
        );
    }

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    /// Apply `action` as the demo's own person did, as a window's
    /// dispatcher applies it.
    fn dispatch(doc: &mut AutoCommit, action: &ChatAction) {
        apply_as(doc, model::ME, action).unwrap();
    }

    fn agent_message(intent: crate::model::AgentIntent) -> ChatAction {
        ChatAction::PostAgentMessage {
            workspace: "ws_acme".into(),
            agent: "scribe".into(),
            message: AgentMessagePayload {
                id: "caller-controlled-id".into(),
                ts: TS,
                text: "Review the current proposal".into(),
                intent,
                attachment: None,
                mentions: Vec::new(),
                reference: None,
                body: Vec::new(),
            },
        }
    }

    fn direct_message_doc() -> (AutoCommit, String, String) {
        let mut doc = seeded();
        let source = tilo_actions::InvocationId(uuid::Uuid::from_u128(27));
        apply_as_with_source(
            &mut doc,
            model::ME,
            source,
            &agent_message(crate::model::AgentIntent::Message),
        )
        .unwrap();
        (
            doc,
            ChatDoc::agent_conversation_channel_id("ws_acme", "scribe"),
            agent_request_message_id(source),
        )
    }

    #[test]
    fn admitted_agent_message_uses_exact_source_and_atomically_creates_conversation() {
        let mut doc = seeded();
        let source = tilo_actions::InvocationId(uuid::Uuid::from_u128(17));
        let before = doc.get_heads();

        apply_as_with_source(
            &mut doc,
            model::ME,
            source,
            &agent_message(crate::model::AgentIntent::Message),
        )
        .unwrap();

        let channel = ChatDoc::agent_conversation_channel_id("ws_acme", "scribe");
        let conversation = ChatDoc::list_channels(&doc)
            .into_iter()
            .find(|candidate| candidate.id == channel)
            .unwrap();
        assert_eq!(conversation.conversation_with.as_deref(), Some("scribe"));
        let messages = ChatDoc::list_messages(&doc, &channel);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, agent_request_message_id(source));
        assert_eq!(messages[0].author, model::ME);
        assert_eq!(
            messages[0].agent_intent,
            Some(crate::model::AgentIntent::Message)
        );
        assert_eq!(doc.get_changes(&before).len(), 1);
        let published = tree(&doc);
        let direct = find(&published.children, &format!("chat.channel:{channel}"))
            .expect("the direct conversation is readable as a channel resource");
        assert!(
            direct.views.iter().any(|view| {
                view.id == "conversation" && view.primary && view.open_action == "open-channel"
            }),
            "direct conversations retain the normal resource navigation door"
        );
        assert!(ids(direct).contains(&"open-channel"));
        for unavailable in [
            "post",
            "submit-feedback",
            "rename",
            "archive",
            "unarchive",
            "delete",
        ] {
            assert!(!ids(direct).contains(&unavailable), "{unavailable}");
        }
        let ordinary_channel = find(&published.children, "chat.channel:c_general")
            .expect("ordinary channels still publish reviewed feedback");
        assert!(ids(ordinary_channel).contains(&"submit-feedback"));

        let ordinary = send_message_action(
            &channel,
            "m_ordinary",
            TS + 1,
            "bypass intent",
            None,
            None,
            Vec::new(),
        );
        let before_bypass = doc.save();
        apply_as(&mut doc, model::ME, &ordinary).unwrap();
        assert_eq!(
            doc.save(),
            before_bypass,
            "ordinary post cannot bypass intent"
        );

        let saved = doc.save();
        assert!(
            apply_as_with_source(
                &mut doc,
                model::ME,
                source,
                &agent_message(crate::model::AgentIntent::Message),
            )
            .is_err()
        );
        assert_eq!(doc.save(), saved, "a retry cannot append a second row");
    }

    #[test]
    fn agent_reply_preserves_the_full_bounded_agent_identifier() {
        let mut doc = seeded();
        let agent = "a".repeat(128);
        let mut request = agent_message(crate::model::AgentIntent::Message);
        let ChatAction::PostAgentMessage {
            agent: addressee, ..
        } = &mut request
        else {
            panic!("fixture must be a direct message request");
        };
        *addressee = agent.clone();
        let source = tilo_actions::InvocationId(uuid::Uuid::from_u128(28));
        apply_as_with_source(&mut doc, model::ME, source, &request).unwrap();
        let channel = ChatDoc::agent_conversation_channel_id("ws_acme", &agent);
        let actor = format!("agent:{agent}");
        let reply = reply_agent_message_action(
            &channel,
            &agent_request_message_id(source),
            "agent-boundary-reply",
            TS + 1,
            "Reply from the exact addressed Agent.",
        );
        apply_as(&mut doc, &actor, &reply).unwrap();
        let messages = ChatDoc::list_messages(&doc, &channel);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].author, actor);
        assert_eq!(
            messages[1].parent.as_deref(),
            Some(agent_request_message_id(source).as_str())
        );
    }

    #[test]
    fn agent_reply_boundary_does_not_expand_person_or_agent_id_limits() {
        let (mut doc, channel, parent) = direct_message_doc();
        let reply = reply_agent_message_action(&channel, &parent, "boundary-denied", TS, "no");
        let saved = doc.save();
        // A bounded but differently addressed author is accepted as an actor,
        // not as authority to reply to this conversation.
        apply_as(&mut doc, &"p".repeat(128), &reply).unwrap();
        assert_eq!(doc.save(), saved);
        for actor in [
            "p".repeat(129),
            format!("agent:{}", "a".repeat(129)),
            "agent:".into(),
            "agent:bad\nidentity".into(),
        ] {
            assert!(apply_as(&mut doc, &actor, &reply).is_err());
            assert_eq!(doc.save(), saved);
        }
    }

    #[test]
    fn only_the_addressed_agent_replies_to_an_exact_top_level_message_request() {
        let (mut doc, channel, parent) = direct_message_doc();
        let reply = reply_agent_message_action(
            &channel,
            &parent,
            "agent-reply-27",
            TS + 1,
            "Here is the answer.",
        );
        apply_as(&mut doc, "agent:scribe", &reply).unwrap();
        let messages = ChatDoc::list_messages(&doc, &channel);
        let written = messages
            .iter()
            .find(|message| message.id == "agent-reply-27")
            .unwrap();
        assert_eq!(written.author, "agent:scribe");
        assert_eq!(written.parent.as_deref(), Some(parent.as_str()));
        assert_eq!(written.agent_intent, None);
        assert!(written.work.is_none());
        let history = history_context(&reply);
        assert_eq!(history.action_label, "Reply to message");
        assert_eq!(
            history
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("chat.message:agent-reply-27")
        );

        let saved = doc.save();
        apply_as(&mut doc, "agent:scribe", &reply).unwrap();
        assert_eq!(doc.save(), saved, "an exact retry is a no-op");
        let changed = reply_agent_message_action(
            &channel,
            &parent,
            "agent-reply-27",
            TS + 2,
            "Changed replay payload",
        );
        apply_as(&mut doc, "agent:scribe", &changed).unwrap();
        assert_eq!(
            doc.save(),
            saved,
            "an id collision cannot rewrite the reply"
        );

        for (actor, candidate) in [
            (
                model::ME,
                reply_agent_message_action(&channel, &parent, "person-reply", TS, "no"),
            ),
            (
                "agent:other",
                reply_agent_message_action(&channel, &parent, "other-agent-reply", TS, "no"),
            ),
            (
                "agent:scribe",
                reply_agent_message_action("c_general", &parent, "wrong-channel", TS, "no"),
            ),
            (
                "agent:scribe",
                reply_agent_message_action(&channel, "missing", "missing-parent", TS, "no"),
            ),
        ] {
            apply_as(&mut doc, actor, &candidate).unwrap();
        }
        assert_eq!(doc.save(), saved, "wrong authority or parent is inert");
    }

    #[test]
    fn agent_reply_rejects_nested_and_task_intent_parents() {
        let (mut doc, channel, parent) = direct_message_doc();

        ChatDoc::push_message_with_id(
            &mut doc,
            &channel,
            "nested",
            NewMessage {
                author: model::ME.into(),
                ts: TS,
                text: "nested".into(),
                parent: Some(parent),
                ..Default::default()
            },
        );
        ChatDoc::push_message_with_id(
            &mut doc,
            &channel,
            "task-parent",
            NewMessage {
                author: model::ME.into(),
                ts: TS,
                text: "task".into(),
                agent_intent: Some(crate::model::AgentIntent::Queue),
                ..Default::default()
            },
        );
        let before_invalid_parents = doc.save();
        for (parent, id) in [("nested", "nested-reply"), ("task-parent", "task-reply")] {
            apply_as(
                &mut doc,
                "agent:scribe",
                &reply_agent_message_action(&channel, parent, id, TS, "no"),
            )
            .unwrap();
        }
        assert_eq!(
            doc.save(),
            before_invalid_parents,
            "nested and Task-intent parents are not Message requests"
        );
    }

    #[test]
    fn queue_and_steer_refuse_without_a_task_owner() {
        for intent in [
            crate::model::AgentIntent::Queue,
            crate::model::AgentIntent::Steer,
        ] {
            let mut doc = seeded();
            let before = doc.save();
            assert!(
                apply_as_with_source(
                    &mut doc,
                    model::ME,
                    tilo_actions::InvocationId(uuid::Uuid::from_u128(18)),
                    &agent_message(intent),
                )
                .is_err()
            );
            assert_eq!(doc.save(), before);
        }
    }

    #[test]
    fn public_set_work_arguments_preserve_route_and_refuse_unverifiable_claims() {
        let spec = set_work_spec("c_crit", "m_agent_reply");
        let build = |value: serde_json::Value| {
            let serde_json::Value::Object(arguments) = value else {
                panic!("fixture arguments are an object")
            };
            spec.build_json(&arguments)
        };
        for (route, expected) in [
            ("model", crate::model::AgentRoute::Model),
            ("floor", crate::model::AgentRoute::Floor),
        ] {
            let encoded = build(serde_json::json!({
                "state": "done",
                "did": "Add function",
                "route": route,
                "why": "The exported action matched the request.",
                "steps": [{
                    "resource": "calculator.workspace",
                    "label": "Add function",
                    "outcome": "applied"
                }]
            }))
            .expect("valid public arguments bind");
            assert_eq!(encoded.type_id, CHAT_ACTION_TYPE);
            let decoded: ChatAction = serde_json::from_slice(&encoded.payload).unwrap();
            let ChatAction::SetAgentWork { work, .. } = decoded else {
                panic!("set-work encodes SetAgentWork")
            };
            assert_eq!(work.reasoning.unwrap().route, expected);
            assert!(matches!(
                work.trace.as_slice(),
                [crate::model::AgentStep {
                    outcome: crate::model::AgentStepOutcome::Applied,
                    ..
                }]
            ));
        }

        for invalid in [
            serde_json::json!({"state": "doing"}),
            serde_json::json!({"state": "done", "did": "  "}),
            serde_json::json!({
                "state": "taken",
                "why": "A claim without provenance is not enough."
            }),
            serde_json::json!({
                "state": "taken",
                "steps": [{"resource": " ", "label": "Add function", "outcome": "asked"}]
            }),
            serde_json::json!({
                "state": "taken",
                "steps": [{"resource": "calculator.workspace", "label": " ", "outcome": "asked"}]
            }),
            serde_json::json!({
                "state": "taken",
                "steps": [{"resource": "calculator.workspace", "label": "Add function"}]
            }),
        ] {
            let encoded = build(invalid).expect("declared argument types still bind");
            assert_eq!(
                encoded.type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE
            );
            assert!(
                !encoded.payload.is_empty(),
                "the refusal explains what is invalid"
            );
        }

        let error = build(serde_json::json!({
            "state": "taken",
            "route": "guess"
        }))
        .expect_err("route is a constrained public argument");
        assert!(error.to_string().contains("route"), "{error}");
    }

    #[test]
    fn saved_relative_person_names_have_distinct_public_resource_labels() {
        let person = crate::model::User {
            id: "peer@2154".into(),
            name: "You".into(),
            initials: "Y".into(),
            color_idx: 0,
            status: crate::model::Presence::Online,
            is_agent: false,
        };
        let published = publish_person(&person);
        assert_eq!(published.id.as_str(), "chat.person:peer@2154");
        assert_ne!(published.label, "You");
        assert!(published.label.contains("2154"));
        assert_eq!(person.name, "You");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "open-person")
        );
    }

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        doc
    }

    #[test]
    fn workspace_receipt_seed_is_empty_creator_bound_and_durable() {
        use automerge::{ReadDoc as _, transaction::Transactable as _};

        let room = "7585df07-168d-4eff-a975-8c86fca6acaf";
        let action = ChatAction::SeedWorkspace {
            workspace: room.into(),
            workspace_name: "Design".into(),
            creator_name: "Ada".into(),
            creator_initials: "A".into(),
            color_idx: 2,
        };
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "future_field", "preserved")
            .unwrap();
        apply_as(&mut doc, "person:ada", &action).unwrap();
        assert_eq!(ChatDoc::list_workspaces(&doc)[0].id, room);
        assert_eq!(ChatDoc::list_users(&doc)[0].id, "person:ada");
        assert!(ChatDoc::list_channels(&doc).is_empty());
        assert!(ChatDoc::list_categories(&doc).is_empty());
        assert_eq!(
            doc.get(automerge::ROOT, "future_field")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("preserved")
        );
        let mut restored = AutoCommit::load(&doc.save()).unwrap();
        let heads = restored.get_heads();
        action
            .apply(&mut AutoCommit::new())
            .expect_err("no signed creator");
        let mut invalid = action.clone();
        if let ChatAction::SeedWorkspace { workspace, .. } = &mut invalid {
            *workspace = "not-a-room".into();
        }
        apply_as(&mut restored, "person:ada", &invalid).expect_err("room is a UUID");
        assert_eq!(restored.get_heads(), heads);
        if let ChatAction::SeedWorkspace { workspace, .. } = &mut invalid {
            *workspace = "65f79c6a-aa2c-46b5-99ab-46fe3df52d92".into();
        }
        apply_as(&mut restored, "person:ada", &invalid).expect_err("no room rebinding");
        assert_eq!(restored.get_heads(), heads);
        apply_as(&mut restored, "person:another", &action).unwrap();
        assert_eq!(ChatDoc::list_users(&restored).len(), 1);
        assert_eq!(ChatDoc::list_users(&restored)[0].id, "person:ada");
        assert_eq!(ChatDoc::list_workspaces(&restored).len(), 1);
        assert!(ChatDoc::list_channels(&restored).is_empty());
    }

    #[test]
    fn workspace_receipt_seed_accepts_the_hosts_full_name_limit() {
        let mut action = ChatAction::SeedWorkspace {
            workspace: "7585df07-168d-4eff-a975-8c86fca6acaf".into(),
            workspace_name: "x".repeat(257),
            creator_name: "Ada".into(),
            creator_initials: "A".into(),
            color_idx: 2,
        };
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        apply_as(&mut doc, "person:ada", &action).expect_err("host rejects 257 bytes");
        assert_eq!(doc.get_heads(), before);
        if let ChatAction::SeedWorkspace { workspace_name, .. } = &mut action {
            *workspace_name = "x".repeat(256);
        }
        apply_as(&mut doc, "person:ada", &action).unwrap();
        assert_eq!(ChatDoc::list_workspaces(&doc)[0].name.len(), 256);
        assert!(ChatDoc::list_channels(&doc).is_empty());
    }

    fn state(doc: &AutoCommit) -> ChatState {
        Chat::read(doc).unwrap()
    }

    /// The published tree: the resource and its children, as the palette,
    /// agents and scripts reach it.
    fn tree(doc: &AutoCommit) -> Resource {
        ui::tilo_resource::publish::<Chat>(Chat::KIND, Chat::LABEL, &state(doc))
    }

    fn find<'a>(resources: &'a [Resource], id: &str) -> Option<&'a Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    fn ids(resource: &Resource) -> Vec<&str> {
        resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    fn post(resource: &Resource, text: &str) -> ChatAction {
        let post = resource.actions.iter().find(|a| a.id == "post").unwrap();
        let tilo_actions::ActionInput::Prompted { build, .. } = &post.input else {
            panic!("post is a prompted action");
        };
        serde_json::from_slice(&build(text).payload).unwrap()
    }

    #[test]
    fn shared_space_replayed_resource_shares_converge_without_copying_content() {
        use tilo_automerge::ApplyIncoming as _;
        let mut first = seeded();
        let mut second = first.fork();
        let reference = tilo_reference::ResourceRef::ranged(
            "word_processor.block",
            "paragraph-7",
            "Proposal",
            tilo_reference::Span::chars(2, 9),
        );
        let action = ChatAction::SendMessage {
            channel: "c_general".into(),
            id: "m_shared_resource".into(),
            ts: 100,
            text: "Discuss this paragraph".into(),
            parent: None,
            attachment: None,
            mentions: Vec::new(),
            reference: Some(reference.clone()),
            source_provider: Some("record:so.tilo.word-processor:proposal".into()),
            body: Vec::new(),
        };
        apply_as(&mut first, "u_aria", &action).unwrap();
        // Peers receive the originating CRDT change, rather than inventing a
        // second list insertion by independently executing the same action.
        second.merge_incoming(&mut first).unwrap();
        apply_as(&mut second, "u_aria", &action).unwrap();
        first.merge_incoming(&mut second).unwrap();
        let messages = ChatDoc::list_messages(&first, "c_general");
        let shared: Vec<_> = messages
            .iter()
            .filter(|m| m.id == "m_shared_resource")
            .collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].reference, Some(reference));
        assert_eq!(shared[0].author, "u_aria");
        assert_eq!(
            shared[0].source_provider.as_deref(),
            Some("record:so.tilo.word-processor:proposal")
        );
        assert!(
            shared[0].attachment.is_none(),
            "no serialized document or media bytes"
        );
        let mut bad = action;
        if let ChatAction::SendMessage { reference, .. } = &mut bad {
            reference.as_mut().unwrap().span = Some(tilo_reference::Span {
                start: 9,
                end: 2,
                unit: tilo_reference::SpanUnit::Chars,
            });
        }
        assert!(
            apply_as(&mut first, "u_aria", &bad).is_err(),
            "wire replay validates too"
        );
    }

    #[test]
    fn the_tree_offers_every_operation_on_the_item_it_acts_on() {
        let doc = seeded();
        let root = tree(&doc);
        let workspace = find(&root.children, "chat.workspace:ws_acme").unwrap();
        assert_eq!(
            ids(workspace),
            ["open-workspace", "new-channel", "new-category"]
        );
        let crit = find(&root.children, "chat.channel:c_crit").unwrap();
        for id in [
            "read-messages",
            "open-channel",
            "post",
            "rename",
            "archive",
            "mark-read",
            "delete",
        ] {
            assert!(ids(crit).contains(&id), "{id}: {:?}", ids(crit));
        }
        let open = crit
            .actions
            .iter()
            .find(|action| action.id == "open-channel")
            .unwrap();
        assert_eq!(open.kind, OperationKind::View);
        assert!(
            crit.actions
                .iter()
                .filter(|action| action.kind == OperationKind::Document)
                .all(|action| action.contract.reversible)
        );
        let delete = crit.actions.iter().find(|a| a.id == "delete").unwrap();
        assert_eq!(delete.contract.risk, tilo_actions::RiskLevel::Destructive);

        let concept = find(&root.children, "chat.message:m_concept").unwrap();
        for id in [
            "reveal-message",
            "read-message",
            "edit",
            "react",
            "pin",
            "delete",
        ] {
            assert!(ids(concept).contains(&id), "{id}: {:?}", ids(concept));
        }
        assert_eq!(workspace.views[0].open_action, "open-workspace");
        assert_eq!(crit.views[0].open_action, "open-channel");
        assert_eq!(concept.views[0].open_action, "reveal-message");
        for resource in [workspace, crit, concept] {
            assert!(resource.views[0].validate(resource).is_empty());
        }
        let read = concept
            .actions
            .iter()
            .find(|a| a.id == "read-message")
            .unwrap();
        assert_eq!(read.kind, OperationKind::View);
        assert!(
            tilo_actions::ResourceViewDescriptor::declared_for(concept, "read-message").is_none(),
            "a read query does not navigate"
        );
        assert!(
            find(&root.children, "chat.channel:c_huddle")
                .unwrap()
                .views
                .is_empty(),
            "opening a voice channel must remain explicit call consent"
        );
        assert!(
            find(&concept.children, "chat.attachment:m_concept").is_some(),
            "the attachment is a child of its message"
        );
        let heart = find(&concept.children, "chat.reaction:m_concept:❤️").unwrap();
        assert!(ids(heart).contains(&"add") && ids(heart).contains(&"remove"));

        let huddle = find(&root.children, "chat.call:c_huddle").unwrap();
        for id in ["join-call", "leave-call", "mute", "unmute", "call-status"] {
            assert!(ids(huddle).contains(&id), "{id}: {:?}", ids(huddle));
        }
        assert!(
            huddle
                .actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "a call writes nothing to the document"
        );
        let people = find(&root.children, "chat.people").unwrap();
        assert!(find(&people.children, "chat.person:u_aria").is_some());
    }

    #[test]
    fn a_large_organisation_keeps_the_tree_bounded_and_resolves_the_rest() {
        fn count(resources: &[Resource]) -> usize {
            resources
                .iter()
                .map(|resource| 1 + count(&resource.children))
                .sum()
        }
        let mut doc = seeded();
        for index in 0..(EAGER_RESOURCE_SNAPSHOT_LIMIT + 12) {
            ChatDoc::insert_channel(
                &mut doc,
                &format!("c_late_{index:03}"),
                "ws_acme",
                "cat_acme_main",
                &format!("room-{index:03}"),
                ChannelKind::Text,
            );
        }
        let root = tree(&doc);
        assert!(find(&root.children, "chat.channel:c_late_107").is_none());
        let channels = root
            .children
            .iter()
            .flat_map(|workspace| workspace.children.iter())
            .filter(|child| child.kind == "chat.channel")
            .count();
        assert!(channels <= EAGER_RESOURCE_SNAPSHOT_LIMIT, "{channels}");
        assert!(count(&root.children) > 0);
        let state = state(&doc);
        let lazy = Chat::child(&state, "chat.channel:c_late_107")
            .expect("an omitted room remains directly addressable");
        assert!(ids(&lazy).contains(&"post"));
        for id in [
            "chat.message:m_concept",
            "chat.attachment:m_concept",
            "chat.reaction:m_concept:❤️",
            "chat.call:c_huddle",
            "chat.person:u_aria",
            "chat.category:cat_acme_main",
            "chat.workspace:ws_tilo",
        ] {
            assert!(Chat::child(&state, id).is_some(), "{id} resolves");
        }
        assert!(Chat::child(&state, "chat.message:gone").is_none());
    }

    /// A voice channel has no timeline anyone sees (opening it joins the
    /// call), so it offers no `post`, and a `SendMessage` into it is
    /// refused: an agent's post can never land where no member can read it.
    #[test]
    fn voice_channels_offer_no_post_and_refuse_messages() {
        let mut doc = seeded();
        let root = tree(&doc);
        let huddle = find(&root.children, "chat.channel:c_huddle").unwrap();
        assert!(!ids(huddle).contains(&"post"), "{:?}", ids(huddle));
        dispatch(
            &mut doc,
            &send_message_action(
                "c_huddle",
                "m_voice_post",
                TS,
                "unseen",
                None,
                None,
                Vec::new(),
            ),
        );
        assert!(
            ChatDoc::list_messages(&doc, "c_huddle")
                .iter()
                .all(|m| m.id != "m_voice_post")
        );
    }

    /// The published `post` is the same for everyone: whoever invokes it is
    /// the message's author, and `@Name` of a document member is a mention.
    #[test]
    fn a_post_is_authored_by_whoever_invokes_it() {
        let mut doc = seeded();
        let root = tree(&doc);
        let general = find(&root.children, "chat.channel:c_general").unwrap();
        let send = post(general, "thanks @Aria Chen, done");
        let ChatAction::SendMessage {
            mentions, text, id, ..
        } = &send
        else {
            panic!("post encodes a SendMessage")
        };
        assert_eq!(mentions.len(), 1, "{mentions:?}");
        assert_eq!(mentions[0].user, "u_aria");
        assert_eq!(mentions[0].slice(text), "@Aria Chen");
        apply_as(&mut doc, "agent-task:7", &send).unwrap();
        let message = ChatDoc::list_messages(&doc, "c_general")
            .into_iter()
            .find(|message| &message.id == id)
            .unwrap();
        assert_eq!(message.author, "agent-task:7");
        assert!(
            send.apply(&mut doc).is_err(),
            "a message without an author is refused"
        );
    }

    #[test]
    fn agents_are_named_by_their_host_and_record_work_on_their_own_messages() {
        use crate::model::{AgentWork, AgentWorkState};
        let agent = "agent-task:11111111-1111-4111-8111-111111111111";
        let mut doc = seeded();
        dispatch(
            &mut doc,
            &ChatAction::NameAgent {
                participant: agent.into(),
                name: "Tide".into(),
            },
        );
        dispatch(
            &mut doc,
            &ChatAction::NameAgent {
                participant: "person-1".into(),
                name: "Impostor".into(),
            },
        );
        let agents = ChatDoc::agent_authors(&doc);
        assert_eq!(agents.get(agent).map(String::as_str), Some("Tide"));
        assert!(
            !agents.contains_key("person-1"),
            "only an agent task is named"
        );

        apply_as(
            &mut doc,
            agent,
            &send_message_action(
                "c_crit",
                "m_reply",
                TS,
                "",
                Some("m_me1".into()),
                None,
                vec![],
            ),
        )
        .unwrap();
        let reply = find(&tree(&doc).children, "chat.message:m_reply")
            .cloned()
            .or_else(|| Chat::child(&state(&doc), "chat.message:m_reply"))
            .unwrap();
        assert!(ids(&reply).contains(&"set-work"), "{:?}", ids(&reply));
        assert!(reply.label.starts_with("Tide"), "{}", reply.label);

        let taken = AgentWork::new(AgentWorkState::Taken, TS);
        let record = set_agent_work_action("c_crit", "m_reply", taken.clone(), None);
        assert!(
            apply_as(&mut doc, model::ME, &record).is_err(),
            "nobody records work on another's message"
        );
        apply_as(&mut doc, agent, &record).unwrap();
        let m = ChatDoc::list_messages(&doc, "c_crit")
            .into_iter()
            .find(|m| m.id == "m_reply")
            .unwrap();
        assert_eq!(m.work, Some(taken));
    }

    #[test]
    fn message_actions_mutate_the_document_exactly_once() {
        use crate::model::{AgentReasoning, AgentRoute, AgentWork, AgentWorkState};

        let mut doc = seeded();
        let action = send_message_action(
            "c_general",
            "m_retry_once",
            TS,
            "retry-safe",
            None,
            None,
            Vec::new(),
        );
        dispatch(&mut doc, &action);
        dispatch(&mut doc, &action);
        let sent: Vec<_> = ChatDoc::list_messages(&doc, "c_general")
            .into_iter()
            .filter(|message| message.id == "m_retry_once")
            .collect();
        assert_eq!(sent.len(), 1, "a replayed send creates one row");
        assert_eq!(sent[0].author, model::ME);
        dispatch(
            &mut doc,
            &send_message_action("c_missing", "m_x", TS, "nowhere", None, None, Vec::new()),
        );
        assert!(ChatDoc::list_messages(&doc, "c_missing").is_empty());

        // An agent's words are written once, when there are any.
        dispatch(
            &mut doc,
            &send_message_action(
                "c_crit",
                "m_mine",
                TS,
                "",
                Some("m_me1".into()),
                None,
                vec![],
            ),
        );
        let done = AgentWork::new(AgentWorkState::Done { did: "Crop".into() }, TS + 5).because(
            AgentReasoning {
                route: AgentRoute::Floor,
                why: "matched the word processor".into(),
            },
        );
        dispatch(
            &mut doc,
            &set_agent_work_action("c_crit", "m_mine", done, Some("Cropped it.")),
        );
        dispatch(
            &mut doc,
            &set_agent_work_action(
                "c_crit",
                "m_mine",
                AgentWork::new(AgentWorkState::Refused, TS + 9),
                Some("never mind"),
            ),
        );
        let mine = ChatDoc::list_messages(&doc, "c_crit")
            .into_iter()
            .find(|m| m.id == "m_mine")
            .unwrap();
        assert_eq!(
            mine.text, "Cropped it.",
            "an written body is never replaced"
        );

        // Reactions are the author's own.
        dispatch(&mut doc, &add_reaction_action("c_crit", "m_me1", "🔥"));
        apply_as(
            &mut doc,
            "u_aria",
            &add_reaction_action("c_crit", "m_me1", "🔥"),
        )
        .unwrap();
        let fire = |doc: &AutoCommit| {
            ChatDoc::list_messages(doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_me1")
                .unwrap()
                .reactions
                .into_iter()
                .find(|r| r.emoji == "🔥")
        };
        assert_eq!(fire(&doc).unwrap().count(), 2);
        dispatch(&mut doc, &remove_reaction_action("c_crit", "m_me1", "🔥"));
        let left = fire(&doc).unwrap();
        assert!(left.reacted_by("u_aria") && !left.reacted_by(model::ME));

        // The read watermark is the reader's own.
        dispatch(&mut doc, &mark_read_action("c_crit", 3));
        assert_eq!(ChatDoc::read_watermark(&doc, "c_crit", model::ME), 3);
        assert_eq!(ChatDoc::read_watermark(&doc, "c_crit", "u_aria"), 0);

        dispatch(&mut doc, &pin_message_action("c_crit", "m_me1", true));
        dispatch(&mut doc, &delete_message_action("c_crit", "m_me1"));
        assert!(
            ChatDoc::list_messages(&doc, "c_crit")
                .iter()
                .all(|m| m.id != "m_me1")
        );
    }

    #[test]
    fn channel_and_category_actions_round_trip_and_target_history() {
        let mut doc = seeded();
        dispatch(
            &mut doc,
            &create_channel_action(
                "ws_acme",
                "cat_acme_main",
                "c_new",
                "design-ops",
                ChannelKind::Text,
            ),
        );
        dispatch(&mut doc, &rename_channel_action("c_new", "ops"));
        dispatch(&mut doc, &archive_channel_action("c_new", true));
        assert!(
            ChatDoc::list_channels(&doc)
                .iter()
                .any(|c| c.id == "c_new" && c.name == "ops" && c.archived)
        );
        dispatch(&mut doc, &delete_channel_action("c_new"));
        assert!(ChatDoc::list_channels(&doc).iter().all(|c| c.id != "c_new"));

        dispatch(
            &mut doc,
            &create_category_action("ws_acme", "cat_action", "Design"),
        );
        dispatch(
            &mut doc,
            &rename_category_action("cat_action", "Design reviews"),
        );
        let category = Chat::child(&state(&doc), "chat.category:cat_action").unwrap();
        for id in ["open-category", "rename", "delete"] {
            assert!(ids(&category).contains(&id), "{id}: {:?}", ids(&category));
        }
        assert_eq!(
            history_context(&delete_category_action("cat_action"))
                .resource
                .unwrap()
                .id,
            "chat.category:cat_action"
        );
        let context = history_context(&ChatAction::DeleteMessage {
            channel: "launch".into(),
            id: "msg-9".into(),
        });
        assert_eq!(context.action_label, "Delete message");
        assert_eq!(context.resource.unwrap().id, "chat.message:msg-9");
    }

    #[test]
    fn search_finds_every_kind_and_opens_it_through_the_tree() {
        let doc = seeded();
        let state = state(&doc);
        let entries = Chat::search(&state);
        for (needle, action) in [
            ("Acme Design", "open-workspace"),
            ("design-crit", "open-channel"),
            ("Aria Chen", "open-person"),
        ] {
            let entry = entries
                .iter()
                .find(|entry| {
                    entry.title.ends_with(needle)
                        && entry.action.as_ref().is_some_and(|a| a.action == action)
                })
                .unwrap_or_else(|| panic!("{needle} is found"));
            let address = entry.action.as_ref().unwrap();
            let resource = find(&tree(&doc).children, address.resource.as_str())
                .cloned()
                .or_else(|| Chat::child(&state, address.resource.as_str()))
                .unwrap();
            assert!(ids(&resource).contains(&action));
        }
        let message = entries
            .iter()
            .find(|entry| entry.text.contains("2x2 grid"))
            .expect("a message is found by its words");
        assert_eq!(message.action.as_ref().unwrap().action, "reveal-message");
    }

    /// The published tree, as the palette, agents and scripts see it.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        vec![tree(doc)]
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["New channel", "Rename channel", "Pin message", "Join call"],
        replay: [
            // The order is derived from the shared base state, so both replicas
            // land on the same change.
            create_channel => |_| encode(&create_channel_action(
                "ws_acme",
                "cat_acme_main",
                "c_sync",
                "sync",
                ChannelKind::Text,
            )),
        ],
        undo: [
            rename_channel => |_| encode(&rename_channel_action("c_crit", "undo me")),
        ],
        encodes: [
            delete_channel => ("chat.channel:c_crit", "delete", |_, _| {
                encode(&delete_channel_action("c_crit"))
            }),
            rename_channel => ("chat.channel:c_crit", "rename", arg: "planning", |_, _| {
                encode(&rename_channel_action("c_crit", "planning"))
            }),
            pin_message => ("chat.message:m_open", "pin", |_, _| {
                encode(&pin_message_action("c_crit", "m_open", true))
            }),
            remove_reaction => ("chat.reaction:m_concept:❤️", "remove", |_, _| {
                encode(&remove_reaction_action("c_crit", "m_concept", "❤️"))
            }),
        ],
    }
}
