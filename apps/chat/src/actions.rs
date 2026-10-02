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
//! rename / delete / archive a channel, mark a channel read.
//!
//! **Presentation → never an action:** the compose-input buffer until Send (a
//! per-channel rich-text draft, written directly as you type — see
//! [`crate::doc`]),
//! scroll position, the current selection, which channel/thread is open, typing
//! indicators, and live presence. Nothing there replicates as a discrete
//! mutation.
//!
//! **External → never an action:** voice / screen-share *transport*, media
//! upload, and the network send itself. When a file is shared, the message **row
//! insert** is the action (it carries the attachment *by reference* — a
//! content hash, display name, and bounded metadata, never a local path or the
//! bytes); the media bytes remain outside the CRDT and hydrate on demand.
//!
//! ## A call is both halves, and only one of them is external
//!
//! That last rule used to swallow the whole of a call, which put chat's call
//! controls outside the model chat itself declares. They are two different
//! things:
//!
//! * The **transport** — the portal handshake for a screen source, publishing a
//!   track, dialling a peer by its ticket, the join cue — is external and must
//!   stay external. [`Action::apply`] has to be a pure function of `(payload,
//!   base document)` so a peer can replay it byte-identically; a replay that
//!   reached for a capture source on somebody else's machine would be far worse
//!   than merely wrong.
//! * The **voice-membership map** — who is in the call, who is muted or
//!   deafened, who is presenting, how to dial them — is shared document state
//!   every peer reads. Nothing distinguishes it from a reaction or a pin. So it
//!   is an action ([`ChatAction::JoinCall`] and its siblings), which is what
//!   makes mute and screen-share enumerable, attributable, and undoable through
//!   the same single route as everything else.
//!
//! What is *local* stays local and is neither: the expanded/collapsed call
//! overlay, the participants drawer, and this peer's own view of its call —
//! `call.muted` and friends — are the presentation half above. The action is
//! what gets written down for everyone else.
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

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_i64;

use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource,
};

use crate::doc::{ChatDoc, NewMessage, VoiceState};
use crate::model::{Attachment, ChannelKind};

/// Registry `type_id` for every Chat action.
pub const CHAT_ACTION_TYPE: &str = "chat.action";
pub const CHAT_OPERATION_TYPE: &str = "chat.operation";

/// Local presentation operations exported by Chat's resources. These are
/// deliberately separate from [`ChatAction`]: opening a conversation, message,
/// or person changes only this peer's view and must never enter the shared
/// document or its undo/history stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatOperation {
    /// Local host service command. Neither authority nor capture consent is
    /// carried in this replay-free effect.
    NativeVoice {
        channel: String,
        request: ui::native_media::Request,
    },
    OpenWorkspace {
        workspace: String,
    },
    OpenChannel {
        workspace: String,
        channel: String,
    },
    OpenCategory {
        workspace: String,
        category: String,
    },
    RevealMessage {
        workspace: String,
        channel: String,
        message: String,
        parent: Option<String>,
    },
    OpenPerson {
        user: String,
    },
    /// Start one local, bounded attachment import. The path is never
    /// replicated; a successful worker result becomes a hash-only
    /// `SendMessage` only during the owner's next sync.
    ImportAttachment {
        channel: String,
        path: String,
    },
    /// The message as a value (author, whether it is an agent, whole text,
    /// agent work); changes nothing.
    ReadMessage {
        message: String,
    },
}

/// Decode a Chat-local view operation addressed through a resource action.
///
/// # Errors
///
/// Returns an error when `type_id` belongs to Chat but `payload` is not a
/// serialized [`ChatOperation`].
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<ChatOperation>, String> {
    if type_id != CHAT_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::expect_used,
    reason = "ChatOperation is serde-derived from only serializable fields; ActionSpec requires an immediate value rather than a fallible builder"
)]
pub(crate) fn view_operation(id: &str, label: &str, operation: &ChatOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        CHAT_OPERATION_TYPE,
        operation,
    )
    .expect("ChatOperation is always serializable")
}

/// Every collaborative operation Chat exports on the shared workspace.
///
/// Determinism contract: ids/timestamps are minted at authoring time and carried
/// in the payload; [`Action::apply`] never touches the RNG or the wall-clock and
/// derives any remaining value from the replay's shared base state in stable
/// order. A malformed payload is a **no-op** rather than an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatAction {
    /// Post a message into `channel`. The `id` and `ts` are minted once at
    /// authoring time and carried here, so replay is byte-identical. A chat
    /// message is discrete (not a co-edited flowing document), so a whole-message
    /// `text` payload is the correct grain. `parent` is `Some` for a threaded
    /// reply. `attachment` references portable shared bytes by content
    /// hash, so a peer never needs the sender's filesystem path.
    SendMessage {
        channel: String,
        id: String,
        author: String,
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
        /// The body as written (paragraphs, marks, mentions marked); `text`
        /// is its plain text. Empty for a plain message, and in a frame from
        /// a release that predates rich bodies.
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
    /// Add `user`'s `emoji` reaction to a message (idempotent).
    AddReaction {
        channel: String,
        msg: String,
        emoji: String,
        user: String,
    },
    /// Remove `user`'s `emoji` reaction from a message (idempotent).
    RemoveReaction {
        channel: String,
        msg: String,
        emoji: String,
        user: String,
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
    SetCategoryCollapsed {
        id: String,
        collapsed: bool,
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
    /// Delete a channel and its messages, voice membership, and draft.
    DeleteChannel {
        id: String,
    },
    /// Set (or clear) a channel's archived flag. Archived channels drop out of
    /// the sidebar but stay in the document (reachable via Ctrl+K to unarchive).
    ArchiveChannel {
        id: String,
        archived: bool,
    },
    /// Advance `user`'s read watermark for `channel` to `count`, minted at
    /// authoring time (the channel's top-level message count) and carried here so
    /// replay never re-reads live state.
    MarkRead {
        channel: String,
        user: String,
        count: i64,
    },
    /// Record what an agent is doing with the message `msg` replies to, and
    /// optionally supply the words `msg` did not have when it was posted.
    ///
    /// Deliberately not [`ChatAction::EditMessage`]. An agent that took a
    /// summons posts its reply *before* it has an answer, and the states it
    /// then passes through are not revisions of a sentence: routing them
    /// through `EditMessage` would archive each one into the message's edit
    /// history, flag the reply `edited`, and hang an "(edited) · view
    /// history" affordance off an outcome nobody had rewritten. The body is
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

    // --- the call, as it appears in the document ---------------------------
    //
    // A call has two halves and only one of them is here. The transport —
    // the portal handshake, publishing a track, dialling a peer, the join
    // cue — is external and stays external; see the module note. What is
    // *written down* is the voice-membership map: who is in, who is muted,
    // who is presenting, how to dial them. That is shared document state
    // every peer reads, so by the rule this module opens with it is an
    // action, and it was the one part of chat that was not.
    /// Enter the call on `channel` as `user`, as far as the document is
    /// concerned: the membership row appears and everyone sees it.
    JoinCall {
        channel: String,
        user: String,
    },
    /// Leave it. The membership row goes, along with everything hanging off
    /// it — which is why this is one action and not a clear of each field.
    LeaveCall {
        channel: String,
        user: String,
    },
    /// This peer's mic and listening state, as the others read it.
    ///
    /// One action rather than four because it is one fact with four fields:
    /// a peer that unmuted and undeafened in the same gesture must not be
    /// two entries in the room's history, and must not be two steps to undo.
    SetCallState {
        channel: String,
        user: String,
        talking: bool,
        muted: bool,
        deafened: bool,
        audio_only: bool,
    },
    /// Whether this peer is presenting. Separate from [`Self::SetCallState`]
    /// because it is a separate decision with a separate gate: acquiring a
    /// screen goes through the portal, and only a granted source gets here.
    SetScreenShare {
        channel: String,
        user: String,
        sharing: bool,
    },
    /// How peers dial this one's media transport.
    ///
    /// An action because it writes to the shared document, and everything
    /// that writes to the shared document is one — a signalling row that
    /// replicated outside the action path would be a row a peer could not
    /// replay and history could not account for. It is deliberately *not*
    /// published as an addressable operation: the value is an opaque ticket
    /// only the local transport can mint, so there is nothing a caller
    /// naming this by hand could correctly pass.
    SetCallTicket {
        channel: String,
        user: String,
        ticket: String,
    },
    /// A message posted by an agent task through the same `post` operation a
    /// person uses: `send` is the ordinary [`ChatAction::SendMessage`], whose
    /// author is the invoking participant (`agent-task:<task>`), and `name` is
    /// that agent's display name, recorded beside the author so every member
    /// renders "<name> · agent" instead of the worker's own identity. Anything
    /// else inside, or an author outside the agent-task namespace, applies the
    /// inner action alone (no label).
    AgentAuthored {
        name: String,
        send: Box<ChatAction>,
    },
    /// Which video codecs this peer decodes, announced on join so a sender
    /// picks a codec every member of the call takes. Like the ticket it is
    /// minted locally (from the media stack) and not an addressable
    /// operation.
    SetCallVideoCaps {
        channel: String,
        user: String,
        hardware_decode: Vec<String>,
        software_decode: Vec<String>,
    },
    /// Announce this peer into the shared roster. Writes only fields that differ,
    /// so a repeat is a no-op.
    EnsureUser {
        id: String,
        name: String,
        initials: String,
        color_idx: usize,
        status: String,
    },
    /// Create the shared rich draft for a composer when it is missing or split.
    EnsureRichDraft {
        key: String,
    },
    /// The first workspace in a new chat record: one person, two channels.
    SeedStarter {
        id: String,
        name: String,
        initials: String,
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
    if let ChatAction::AgentAuthored { send, .. } = action
        && !matches!(**send, ChatAction::AgentAuthored { .. })
    {
        return history_context(send);
    }
    let mut context = tilo_actions::ActionContext::for_action("so.tilo.chat", "Chat", action);
    let (label, channel, resource_id, kind) = match action {
        ChatAction::SendMessage { channel, id, .. } => {
            ("Send message", channel, id, "chat.message")
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
        ChatAction::SetCategoryCollapsed { id, collapsed } => (
            if *collapsed {
                "Collapse category"
            } else {
                "Expand category"
            },
            id,
            id,
            "chat.category",
        ),
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
        ChatAction::JoinCall { channel, .. } => ("Join call", channel, channel, "chat.call"),
        ChatAction::LeaveCall { channel, .. } => ("Leave call", channel, channel, "chat.call"),
        ChatAction::SetCallState { channel, .. } => {
            ("Update call state", channel, channel, "chat.call")
        }
        ChatAction::SetScreenShare {
            channel, sharing, ..
        } => (
            if *sharing {
                "Start presenting"
            } else {
                "Stop presenting"
            },
            channel,
            channel,
            "chat.call",
        ),
        ChatAction::SetCallTicket { channel, .. }
        | ChatAction::SetCallVideoCaps { channel, .. } => {
            ("Update call connection", channel, channel, "chat.call")
        }
        ChatAction::EnsureUser { id, .. } => ("Join chat", id, id, "chat.user"),
        ChatAction::EnsureRichDraft { key } => ("Open draft", key, key, "chat.draft"),
        ChatAction::SeedStarter { id, .. } => ("Open workspace", id, id, "chat.workspace"),
        // A well-formed one delegated above; a nested wrapper applies nothing.
        ChatAction::AgentAuthored { .. } => return context,
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
    id: &str,
    name: &str,
    initials: &str,
    color_idx: usize,
) -> Result<(), ActionError> {
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

impl Action for ChatAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        if let ChatAction::AgentAuthored { name, send } = self {
            if matches!(**send, ChatAction::AgentAuthored { .. }) {
                return Ok(());
            }
            send.apply(doc)?;
            if let ChatAction::SendMessage { id, author, .. } = &**send {
                ChatDoc::label_agent_author(doc, id, author, name);
            }
            return Ok(());
        }
        if let ChatAction::EnsureUser {
            id,
            name,
            initials,
            color_idx,
            status,
        } = self
        {
            let presence = status.parse().unwrap_or(crate::model::Presence::Online);
            let _ = ChatDoc::ensure_user(doc, id, name, initials, *color_idx, presence);
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
        if let ChatAction::SeedStarter {
            id,
            name,
            initials,
            color_idx,
        } = self
        {
            return apply_seed_starter(doc, id, name, initials, *color_idx);
        }
        self.apply_message(doc);
        self.apply_channel(doc);
        self.apply_call(doc);
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Chat mutations are client-local: each peer applies + replicates its own
        // sends/edits/reactions; peers replay them. There is no authoritative
        // ordering host for a message row.
        ExecutionLocus::ClientLocal
    }

    /// A read watermark is shared state about the channel, not a message.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            ChatAction::MarkRead { .. } => tilo_actions::ChangeKind::Housekeeping,
            ChatAction::AgentAuthored { send, .. }
                if !matches!(**send, ChatAction::AgentAuthored { .. }) =>
            {
                send.change_kind()
            }
            _ => tilo_actions::ChangeKind::Content,
        }
    }
}

impl ChatAction {
    /// Message-row mutations: send, edit, delete, react, pin.
    fn apply_message(&self, doc: &mut Doc) {
        match self {
            ChatAction::SendMessage {
                channel,
                id,
                author,
                ts,
                text,
                parent,
                attachment,
                mentions,
                reference,
                body,
            } => {
                if !ChatDoc::accepts_message_action(
                    doc,
                    channel,
                    id,
                    text,
                    parent.as_deref(),
                    attachment.as_ref(),
                ) {
                    return;
                }
                ChatDoc::push_message_with_id(
                    doc,
                    channel,
                    id,
                    NewMessage {
                        author: author.clone(),
                        ts: *ts,
                        text: text.clone(),
                        parent: parent.clone(),
                        attachment: attachment.clone(),
                        mentions: mentions.clone(),
                        reference: reference.clone(),
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
                user,
            } => {
                ChatDoc::add_reaction(doc, channel, msg, emoji, user);
            }
            ChatAction::RemoveReaction {
                channel,
                msg,
                emoji,
                user,
            } => {
                ChatDoc::remove_reaction(doc, channel, msg, emoji, user);
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
    }

    /// Channel/workspace-structure mutations: create, rename, delete,
    /// archive, mark-read, and the agent-work annotation on a message.
    fn apply_channel(&self, doc: &mut Doc) {
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
            ChatAction::SetCategoryCollapsed { id, collapsed } => {
                ChatDoc::set_category_collapsed(doc, id, *collapsed);
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
            ChatAction::MarkRead {
                channel,
                user,
                count,
            } => {
                ChatDoc::set_read_watermark(doc, channel, user, *count);
            }
            ChatAction::SetAgentWork {
                channel,
                msg,
                work,
                say,
            } => {
                ChatDoc::set_message_work(doc, channel, msg, work, say.as_deref());
            }
            _ => {}
        }
    }

    /// The voice-membership half of a call: join/leave and the shared mic /
    /// screen-share / signalling state. See the module docs for why this
    /// half — and only this half — is an action.
    fn apply_call(&self, doc: &mut Doc) {
        match self {
            ChatAction::JoinCall { channel, user } => {
                ChatDoc::voice_join(doc, channel, user);
            }
            ChatAction::LeaveCall { channel, user } => {
                ChatDoc::voice_leave(doc, channel, user);
            }
            ChatAction::SetCallState {
                channel,
                user,
                talking,
                muted,
                deafened,
                audio_only,
            } => {
                ChatDoc::voice_set_state(
                    doc,
                    channel,
                    user,
                    VoiceState {
                        talking: *talking,
                        muted: *muted,
                        deafened: *deafened,
                        audio_only: *audio_only,
                    },
                );
            }
            ChatAction::SetScreenShare {
                channel,
                user,
                sharing,
            } => {
                ChatDoc::voice_set_sharing(doc, channel, user, *sharing);
            }
            ChatAction::SetCallTicket {
                channel,
                user,
                ticket,
            } => {
                ChatDoc::voice_set_ticket(doc, channel, user, ticket);
            }
            ChatAction::SetCallVideoCaps {
                channel,
                user,
                hardware_decode,
                software_decode,
            } => {
                ChatDoc::voice_set_video_caps(
                    doc,
                    channel,
                    user,
                    &crate::model::VideoCaps {
                        hardware_decode: hardware_decode.clone(),
                        software_decode: software_decode.clone(),
                    },
                );
            }
            _ => {}
        }
    }
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

/// `send` as authored by `me`: when `me` is an agent task participant the
/// room names (the invoker a delivered `post` resolved for), wrap it so the
/// agent's display name is recorded with the message; a person's send is
/// returned unchanged, byte-identical to before.
pub fn agent_authored(
    me: &str,
    room: &[crate::mention::Participant],
    send: ChatAction,
) -> ChatAction {
    let agent = me
        .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
        .then(|| room.iter().find(|p| p.id == me && p.is_agent))
        .flatten();
    match agent {
        Some(participant) => ChatAction::AgentAuthored {
            name: bounded_agent_name(&participant.name),
            send: Box::new(send),
        },
        None => send,
    }
}

fn bounded_agent_name(name: &str) -> String {
    let name = name.trim();
    let name = if name.is_empty() { "Agent" } else { name };
    let end = name.floor_char_boundary(MAX_AGENT_AUTHOR_NAME_BYTES);
    name.get(..end).unwrap_or(name).to_owned()
}

pub fn send_message_action(
    channel: &str,
    id: &str,
    author: &str,
    ts: i64,
    text: &str,
    parent: Option<String>,
    attachment: Option<Attachment>,
    mentions: Vec<crate::mention::Mention>,
) -> ChatAction {
    ChatAction::SendMessage {
        channel: channel.to_string(),
        id: id.to_string(),
        author: author.to_string(),
        ts,
        text: text.to_string(),
        parent,
        attachment,
        mentions,
        reference: None,
        body: Vec::new(),
    }
}

impl ChatAction {
    /// A send or an edit carrying the body as written (`text` its plain
    /// text); any other action unchanged.
    #[must_use]
    pub fn with_body(mut self, written: Vec<tilo_document::ClipBlock>) -> Self {
        if let ChatAction::SendMessage { body, .. } | ChatAction::EditMessage { body, .. } =
            &mut self
        {
            *body = written;
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

pub fn add_reaction_action(channel: &str, msg: &str, emoji: &str, user: &str) -> ChatAction {
    ChatAction::AddReaction {
        channel: channel.to_string(),
        msg: msg.to_string(),
        emoji: emoji.to_string(),
        user: user.to_string(),
    }
}

pub fn remove_reaction_action(channel: &str, msg: &str, emoji: &str, user: &str) -> ChatAction {
    ChatAction::RemoveReaction {
        channel: channel.to_string(),
        msg: msg.to_string(),
        emoji: emoji.to_string(),
        user: user.to_string(),
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

pub fn set_category_collapsed_action(id: &str, collapsed: bool) -> ChatAction {
    ChatAction::SetCategoryCollapsed {
        id: id.to_string(),
        collapsed,
    }
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

pub fn mark_read_action(channel: &str, user: &str, count: i64) -> ChatAction {
    ChatAction::MarkRead {
        channel: channel.to_string(),
        user: user.to_string(),
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

/// This peer's mic and listening state, as the others read it.
///
/// `talking` is derived rather than passed: for the local peer it is simply
/// "not muted", and deriving it in the one place both the control and the
/// palette build through is what stops the two from disagreeing about what a
/// muted peer looks like to everyone else.
pub fn call_state_action(
    channel: &str,
    user: &str,
    muted: bool,
    deafened: bool,
    audio_only: bool,
) -> ChatAction {
    ChatAction::SetCallState {
        channel: channel.to_string(),
        user: user.to_string(),
        talking: !muted,
        muted,
        deafened,
        audio_only,
    }
}

pub fn screen_share_action(channel: &str, user: &str, sharing: bool) -> ChatAction {
    ChatAction::SetScreenShare {
        channel: channel.to_string(),
        user: user.to_string(),
        sharing,
    }
}

pub fn join_call_action(channel: &str, user: &str) -> ChatAction {
    ChatAction::JoinCall {
        channel: channel.to_string(),
        user: user.to_string(),
    }
}

pub fn leave_call_action(channel: &str, user: &str) -> ChatAction {
    ChatAction::LeaveCall {
        channel: channel.to_string(),
        user: user.to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// A channel as the resource tree needs to see it — enough to build its label
/// and its actions. Derived from the app's already-loaded projection; archived
/// channels are included so Ctrl+K can unarchive them.
#[derive(Debug, Clone)]
pub struct ChannelResource {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub archived: bool,
}

/// A durable channel grouping published alongside channels.  Categories are
/// not merely paint-time headings: rename, delete and collapse are shared
/// document facts and therefore have ordinary resource actions and history.
#[derive(Debug, Clone)]
pub struct CategoryResource {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
}

/// A message as the resource tree needs to see it: its id, a display label
/// (author + snippet) and whether it is pinned (so pin toggles to unpin).
#[derive(Debug, Clone)]
pub struct MessageResource {
    pub id: String,
    /// Who wrote it: an agent records its work only on its own messages.
    pub author: String,
    pub label: String,
    pub pinned: bool,
    pub parent: Option<String>,
    /// An attachment is a durable, content-addressed child of the message,
    /// rather than an unsearchable bit of its paint. There can be only one in
    /// the current message schema, so its stable address is keyed by message.
    pub attachment: Option<AttachmentResource>,
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
    pub reacted_by_me: bool,
}

/// The presentation context Chat hands [`provide_resources`]: the local peer id,
/// the active workspace, its channels, and the messages of the one channel
/// currently open. Derived entirely from state the document already holds.
#[derive(Debug, Clone)]
pub struct ChatResourceCtx {
    pub me: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub channels: Vec<ChannelResource>,
    pub active_channel: String,
    pub messages: Vec<MessageResource>,
    /// The room, for resolving an `@mention` typed through the palette's
    /// "Post message" prompt the same way the composer does.
    pub participants: Vec<crate::mention::Participant>,
}

/// Keep the ordinary palette snapshot cheap even for a very large shared
/// organisation. Global search owns the complete paged catalogue and
/// [`ResourceProvider::resource`] resolves any omitted row on demand.
pub const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;
const EAGER_WORKSPACE_LIMIT: usize = 8;
const EAGER_PEOPLE_LIMIT: usize = 23;

/// # Panics
/// Never in practice: `ActionSpec::immediate` only fails if encoding `action`
/// fails, and — as with [`encode`] — a `ChatAction` is always encodable.
/// `provide_resources` (the only caller, via the palette's resource tree)
/// returns a plain `Vec<Resource>` to match [`ResourceProvider`]'s contract,
/// so there is no `Result` to propagate through.
#[allow(
    clippy::expect_used,
    reason = "ChatAction is always encodable (see encode's reason), and the ResourceProvider trait contract this feeds returns Vec<Resource> directly, not a Result"
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

/// A channel's palette label: `#name` for text, a speaker glyph for voice.
fn channel_label(ch: &ChannelResource) -> String {
    let name = if ch.name.trim().is_empty() {
        "channel"
    } else {
        ch.name.trim()
    };
    format!("{} {}", ch.kind.glyph(), name)
}

/// Workspace Document ops for [`tilo_resource::publish`] — create channel /
/// category. Open stays a View on the contextual standalone tree.
pub(crate) fn publish_workspace_document(id: &str, name: &str) -> Resource {
    let mut workspace = Resource::new(
        format!("chat.workspace:{id}"),
        "chat.workspace",
        if name.trim().is_empty() {
            "Workspace"
        } else {
            name
        },
    );
    workspace = workspace.with_action(new_channel_action_spec(id));
    workspace = workspace.with_action(new_category_action_spec(id));
    annotate_contracts(&mut workspace);
    workspace
}

/// Channel Document ops for [`tilo_resource::publish`] — post / rename /
/// archive / mark-read / delete. Open / attach / call stay Views/Effects.
pub(crate) fn publish_channel_document(
    me: &str,
    participants: &[crate::mention::Participant],
    ch: &ChannelResource,
    top_level_count: i64,
    messages: &[MessageResource],
) -> Resource {
    let mut channel = Resource::new(
        format!("chat.channel:{}", ch.id),
        "chat.channel",
        channel_label(ch),
    );
    let timeline = ch.kind != ChannelKind::Voice;
    if timeline {
        channel = channel.with_action(ActionSpec::prompted(
            "post",
            "Post message",
            "Message",
            "type a message… (@Name mentions a member)",
            ExecutionLocus::ClientLocal,
            {
                let cid = ch.id.clone();
                let me = me.to_owned();
                let room = participants.to_vec();
                move |text: &str| {
                    let id = format!("m_{}", uuid::Uuid::new_v4().simple());
                    let mentions = crate::mention::resolve(text, &room);
                    encode(&agent_authored(
                        &me,
                        &room,
                        send_message_action(
                            &cid,
                            &id,
                            &me,
                            unix_now_i64(),
                            text,
                            None,
                            None,
                            mentions,
                        ),
                    ))
                }
            },
        ));
    }
    channel = channel.with_action(ActionSpec::prompted(
        "rename",
        "Rename channel",
        "Name",
        "channel name…",
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
            move |name: &str| encode(&rename_channel_action(&cid, name.trim()))
        },
    ));
    if ch.archived {
        channel = channel.with_action(immediate_spec(
            "unarchive",
            "Unarchive channel",
            &archive_channel_action(&ch.id, false),
        ));
    } else {
        channel = channel.with_action(immediate_spec(
            "archive",
            "Archive channel",
            &archive_channel_action(&ch.id, true),
        ));
    }
    channel = channel.with_action(immediate_spec(
        "mark-read",
        "Mark as read",
        &mark_read_action(&ch.id, me, top_level_count),
    ));
    channel = channel.with_action(immediate_spec(
        "delete",
        "Delete channel",
        &delete_channel_action(&ch.id),
    ));
    for message in messages {
        channel = channel.with_child(publish_message_document(me, participants, ch, message));
    }
    annotate_contracts(&mut channel);
    channel
}

/// Message Document ops for [`tilo_resource::publish`] — edit / react / pin /
/// delete. Reveal / read stay Views on the contextual tree.
pub(crate) fn publish_message_document(
    me: &str,
    participants: &[crate::mention::Participant],
    ch: &ChannelResource,
    m: &MessageResource,
) -> Resource {
    let mut message = Resource::new(
        format!("chat.message:{}", m.id),
        "chat.message",
        m.label.clone(),
    );
    message = message.with_action(ActionSpec::prompted(
        "edit",
        "Edit message",
        "Message",
        "new text…",
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
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
            let cid = ch.id.clone();
            let mid = m.id.clone();
            let me = me.to_owned();
            move |emoji: &str| encode(&add_reaction_action(&cid, &mid, emoji.trim(), &me))
        },
    ));
    if m.pinned {
        message = message.with_action(immediate_spec(
            "unpin",
            "Unpin message",
            &pin_message_action(&ch.id, &m.id, false),
        ));
    } else {
        message = message.with_action(immediate_spec(
            "pin",
            "Pin message",
            &pin_message_action(&ch.id, &m.id, true),
        ));
    }
    message = message.with_action(immediate_spec(
        "delete",
        "Delete message",
        &delete_message_action(&ch.id, &m.id),
    ));
    let ctx = ChatResourceCtx {
        me: me.to_owned(),
        workspace_id: String::new(),
        workspace_name: String::new(),
        channels: Vec::new(),
        active_channel: ch.id.clone(),
        messages: Vec::new(),
        participants: participants.to_vec(),
    };
    if m.author == me && acts_as_agent(&ctx) {
        message = message.with_action(set_work_spec(&ch.id, &m.id));
    }
    annotate_contracts(&mut message);
    message
}

/// Category Document ops for [`tilo_resource::publish`].
pub(crate) fn publish_category_document(category: &CategoryResource) -> Resource {
    let mut resource = Resource::new(
        format!("chat.category:{}", category.id),
        "chat.category",
        category.name.clone(),
    )
    .with_action(ActionSpec::prompted(
        "rename",
        "Rename category",
        "Name",
        "category name…",
        ExecutionLocus::ClientLocal,
        {
            let id = category.id.clone();
            move |name: &str| encode(&rename_category_action(&id, name.trim()))
        },
    ));
    resource = resource.with_action(immediate_spec(
        if category.collapsed {
            "expand"
        } else {
            "collapse"
        },
        if category.collapsed {
            "Expand category"
        } else {
            "Collapse category"
        },
        &set_category_collapsed_action(&category.id, !category.collapsed),
    ));
    resource = resource.with_action(immediate_spec(
        "delete",
        "Delete category",
        &delete_category_action(&category.id),
    ));
    annotate_contracts(&mut resource);
    resource
}

/// Workspace / channel / category / message Document children for
/// [`tilo_resource::publish`]. Bounded like the standalone eager page.
pub(crate) fn publish_document_children(state: &crate::resource::ChatState) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT
        .saturating_sub(1)
        .saturating_sub(state.draft_keys.len());
    let mut workspaces = state.workspaces.clone();
    workspaces.sort_by_key(|workspace| workspace.order);
    for ws in workspaces.into_iter().take(EAGER_WORKSPACE_LIMIT) {
        if remaining == 0 {
            break;
        }
        let mut workspace = publish_workspace_document(&ws.id, &ws.name);
        remaining = remaining.saturating_sub(1);

        let mut channels = state
            .channels
            .iter()
            .filter(|channel| channel.workspace == ws.id)
            .cloned()
            .collect::<Vec<_>>();
        channels.sort_by_key(|channel| channel.order);
        for channel in channels.into_iter().take(remaining) {
            let ch = ChannelResource {
                id: channel.id.clone(),
                name: channel.name,
                kind: channel.kind,
                archived: channel.archived,
            };
            let top_level = state
                .channel_top_level
                .iter()
                .find(|(id, _)| id == &ch.id)
                .map_or(0, |(_, count)| *count);
            let messages = state
                .channel_messages
                .iter()
                .find(|(id, _)| id == &ch.id)
                .map_or(&[][..], |(_, messages)| messages.as_slice());
            workspace = workspace.with_child(publish_channel_document(
                &state.me,
                &state.participants,
                &ch,
                top_level,
                messages,
            ));
            remaining = remaining.saturating_sub(1);
        }
        let mut categories = state
            .categories
            .iter()
            .filter(|category| category.workspace == ws.id)
            .cloned()
            .collect::<Vec<_>>();
        categories.sort_by_key(|category| category.order);
        for category in categories.into_iter().take(remaining) {
            let category = CategoryResource {
                id: category.id,
                name: category.name,
                collapsed: category.collapsed,
            };
            workspace = workspace.with_child(publish_category_document(&category));
            remaining = remaining.saturating_sub(1);
        }
        children.push(workspace);
    }
    children.push(people_resource(
        state
            .participants
            .iter()
            .map(|person| (person.id.as_str(), person.name.as_str())),
    ));
    children
}

fn people_resource<'a>(people: impl IntoIterator<Item = (&'a str, &'a str)>) -> Resource {
    let mut people_node = Resource::new("chat.people", "chat.people", "People");
    for (id, name) in people.into_iter().take(EAGER_PEOPLE_LIMIT) {
        people_node = people_node.with_child(
            Resource::new(format!("chat.person:{id}"), "chat.person", name).with_action(
                view_operation(
                    "open",
                    "Open person",
                    &ChatOperation::OpenPerson {
                        user: id.to_string(),
                    },
                ),
            ),
        );
    }
    people_node
}

fn index_resources(resource: &Resource, index: &mut std::collections::HashMap<String, Resource>) {
    index.insert(resource.id.as_str().to_string(), resource.clone());
    for child in &resource.children {
        index_resources(child, index);
    }
}

/// Views and Effects from the contextual page, without a second copy of a
/// document action the published resource already carries. Messages stay off
/// the eager page.
fn overlay_missing(base: &mut Resource, extra: Resource) {
    for action in extra.actions {
        if !base.actions.iter().any(|have| have.id == action.id) {
            base.actions.push(action);
        }
    }
    for child in extra.children {
        if child.kind == "chat.message" {
            continue;
        }
        if !base.children.iter().any(|have| have.id == child.id) {
            base.children.push(child);
        }
    }
}

/// Build the resource tree Chat currently has open: the active workspace (root),
/// one `chat.channel:<id>` per channel, and — under the one open channel — a
/// `chat.message:<id>` per message. Every operation is the same action the
/// on-screen affordance dispatches.
#[allow(
    clippy::too_many_lines,
    reason = "chat resource surface still overlays channel children for the standalone path"
)]
pub fn provide_resources(ctx: &ChatResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let state =
        <crate::resource::Chat as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
    let published =
        ui::tilo_resource::publish::<crate::resource::Chat>("chat.workspace", "Chat", &state);
    let users = ChatDoc::list_users(doc);
    let mut resource_ctx = ctx.clone();
    // Mentionable people: the room roster the caller supplied (workspace
    // members under their room names, as the composer's `@` picker offers
    // them) first, then document users the roster does not name. Replacing
    // the roster with document rows alone left members who never posted, and
    // the owner's room name, unresolvable for a catalogue `post`.
    for user in &users {
        if !resource_ctx.participants.iter().any(|p| p.id == user.id) {
            resource_ctx.participants.push(crate::mention::Participant {
                id: user.id.clone(),
                name: user.name.clone(),
                is_agent: user.is_agent,
            });
        }
    }
    let all_channels = ChatDoc::list_channels(doc);
    let mut root = published;
    // The eager page is the published workspace tree, plus Views and Effects
    // the document children do not carry (open, attach, call). Messages stay
    // lazy. Hosted ResourceRoot keeps the document children as published.
    let mut published_nodes = std::collections::HashMap::new();
    for child in &root.children {
        index_resources(child, &mut published_nodes);
    }
    root.children
        .retain(|child| child.id.as_str().starts_with("chat.draft:"));
    // The publish root, its rich-text draft children, and the people page are
    // already spoken for; the remainder is the eager workspace/channel page.
    let mut remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT
        .saturating_sub(1)
        .saturating_sub(state.draft_keys.len())
        .saturating_sub(1 + EAGER_PEOPLE_LIMIT);
    let mut workspaces = ChatDoc::list_workspaces(doc);
    // Context first: the open room's siblings are more useful than an
    // arbitrary early workspace when only one bounded page is materialised.
    workspaces.sort_by_key(|workspace| workspace.id != ctx.workspace_id);
    for ws in workspaces.into_iter().take(EAGER_WORKSPACE_LIMIT) {
        if remaining == 0 {
            break;
        }
        let workspace_id = format!("chat.workspace:{}", ws.id);
        let mut workspace = published_nodes
            .remove(&workspace_id)
            .unwrap_or_else(|| workspace_resource(&ws.id, &ws.name));
        workspace.children.clear();
        overlay_missing(&mut workspace, workspace_resource(&ws.id, &ws.name));
        remaining = remaining.saturating_sub(1);

        let mut channels = all_channels
            .iter()
            .filter(|channel| channel.workspace == ws.id)
            .cloned()
            .collect::<Vec<_>>();
        channels.sort_by_key(|channel| channel.order);
        for channel in channels.into_iter().take(remaining) {
            let ch = ChannelResource {
                id: channel.id.clone(),
                name: channel.name,
                kind: channel.kind,
                archived: channel.archived,
            };
            let contextual = build_channel_resource(&resource_ctx, doc, &ws.id, &ch, &[]);
            let channel_id = format!("chat.channel:{}", ch.id);
            let mut channel_node = published_nodes
                .remove(&channel_id)
                .unwrap_or_else(|| contextual.clone());
            channel_node
                .children
                .retain(|child| child.kind != "chat.message");
            overlay_missing(&mut channel_node, contextual);
            workspace = workspace.with_child(channel_node);
            remaining = remaining.saturating_sub(1);
        }
        let mut categories = ChatDoc::list_categories(doc)
            .into_iter()
            .filter(|category| category.workspace == ws.id)
            .collect::<Vec<_>>();
        categories.sort_by_key(|category| category.order);
        for category in categories.into_iter().take(remaining) {
            let category = CategoryResource {
                id: category.id,
                name: category.name,
                collapsed: category.collapsed,
            };
            let contextual = build_category_resource(&ws.id, &category);
            let category_id = format!("chat.category:{}", category.id);
            let mut category_node = published_nodes
                .remove(&category_id)
                .unwrap_or_else(|| contextual.clone());
            overlay_missing(&mut category_node, contextual);
            workspace = workspace.with_child(category_node);
            remaining = remaining.saturating_sub(1);
        }
        annotate_contracts(&mut workspace);
        root = root.with_child(workspace);
    }

    let mut people = published_nodes
        .remove("chat.people")
        .unwrap_or_else(|| Resource::new("chat.people", "chat.people", "People"));
    for user in users.iter().take(EAGER_PEOPLE_LIMIT) {
        let person_id = format!("chat.person:{}", user.id);
        if people
            .children
            .iter()
            .any(|child| child.id.as_str() == person_id)
        {
            continue;
        }
        people = people.with_child(
            Resource::new(person_id, "chat.person", user.name.clone()).with_action(view_operation(
                "open",
                "Open person",
                &ChatOperation::OpenPerson {
                    user: user.id.clone(),
                },
            )),
        );
    }
    root = root.with_child(people);
    vec![root]
}

fn workspace_resource(id: &str, name: &str) -> Resource {
    Resource::new(
        format!("chat.workspace:{id}"),
        "chat.workspace",
        if name.trim().is_empty() {
            "Workspace"
        } else {
            name
        },
    )
    .with_action(view_operation(
        "open",
        "Open workspace",
        &ChatOperation::OpenWorkspace {
            workspace: id.to_string(),
        },
    ))
    .with_action(new_channel_action_spec(id))
    .with_action(new_category_action_spec(id))
}

/// Resolve a stable resource from the complete document without expanding the
/// bounded contextual page. Stale ids intentionally fail closed.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive stable-id resolver keeps every resource kind and its canonical action builder adjacent"
)]
pub fn resolve_resource(ctx: &ChatResourceCtx, doc: &AutoCommit, id: &str) -> Option<Resource> {
    if id == "chat.people" {
        let users = ChatDoc::list_users(doc);
        return Some(people_resource(
            users
                .iter()
                .map(|user| (user.id.as_str(), user.name.as_str())),
        ));
    }
    if let Some(workspace_id) = id.strip_prefix("chat.workspace:") {
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == workspace_id)?;
        // Document creates stay the published specs. Open is the view the
        // published workspace does not carry.
        let mut resource = publish_workspace_document(&workspace.id, &workspace.name);
        overlay_missing(
            &mut resource,
            workspace_resource(&workspace.id, &workspace.name),
        );
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(channel_id) = id.strip_prefix("chat.channel:") {
        let channel = ChatDoc::list_channels(doc)
            .into_iter()
            .find(|channel| channel.id == channel_id)?;
        let workspace_id = channel.workspace.clone();
        let model = ChannelResource {
            id: channel.id,
            name: channel.name,
            kind: channel.kind,
            archived: channel.archived,
        };
        // Asked for by name, a channel lists its latest messages: a caller
        // watching the conversation finds each new one here. The eager tree
        // stays bounded and lists none.
        let users = ChatDoc::list_users(doc);
        let agents = ChatDoc::agent_authors(doc);
        let mut messages = ChatDoc::list_messages(doc, &model.id);
        let top_level_count = i64::try_from(messages.len()).unwrap_or(i64::MAX);
        let recent = messages.split_off(messages.len().saturating_sub(RESOLVED_CHANNEL_MESSAGES));
        let recent = recent
            .into_iter()
            .map(|message| message_resource(ctx, message, &users, &agents))
            .collect::<Vec<_>>();
        let mut resource =
            publish_channel_document(&ctx.me, &ctx.participants, &model, top_level_count, &recent);
        let contextual = build_channel_resource(ctx, doc, &workspace_id, &model, &recent);
        let mut views = contextual.clone();
        views.children.retain(|child| child.kind != "chat.message");
        overlay_missing(&mut resource, views);
        for message in contextual
            .children
            .into_iter()
            .filter(|child| child.kind == "chat.message")
        {
            if let Some(existing) = resource
                .children
                .iter_mut()
                .find(|child| child.id == message.id)
            {
                for action in message.actions {
                    if !existing.actions.iter().any(|have| have.id == action.id) {
                        existing.actions.push(action);
                    }
                }
            } else {
                resource.children.push(message);
            }
        }
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(category_id) = id.strip_prefix("chat.category:") {
        let category = ChatDoc::list_categories(doc)
            .into_iter()
            .find(|category| category.id == category_id)?;
        let workspace_id = category.workspace.clone();
        let model = CategoryResource {
            id: category.id,
            name: category.name,
            collapsed: category.collapsed,
        };
        let mut resource = publish_category_document(&model);
        overlay_missing(
            &mut resource,
            build_category_resource(&workspace_id, &model),
        );
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(user_id) = id.strip_prefix("chat.person:") {
        let user = ChatDoc::list_users(doc)
            .into_iter()
            .find(|user| user.id == user_id)?;
        return Some(
            Resource::new(format!("chat.person:{}", user.id), "chat.person", user.name)
                .with_action(view_operation(
                    "open",
                    "Open person",
                    &ChatOperation::OpenPerson { user: user.id },
                )),
        );
    }
    if let Some(channel_id) = id.strip_prefix("chat.call:") {
        let channel = ChatDoc::list_channels(doc)
            .into_iter()
            .find(|channel| channel.id == channel_id && channel.kind == ChannelKind::Voice)?;
        let model = ChannelResource {
            id: channel.id,
            name: channel.name,
            kind: channel.kind,
            archived: channel.archived,
        };
        let mut resource = build_call_resource(ctx, doc, &channel.workspace, &model);
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(message_id) = id.strip_prefix("chat.attachment:") {
        let (channel, message) = find_message(doc, message_id)?;
        let attachment = message.attachment.as_ref()?;
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == channel.workspace)?;
        let channel_model = channel_resource(&channel);
        let mut resource = build_attachment_resource(
            &workspace.id,
            &channel_model,
            &message.id,
            message.parent.as_ref(),
            &AttachmentResource {
                name: attachment.name().to_owned(),
                blob: attachment.blob().to_owned(),
            },
        );
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(rest) = id.strip_prefix("chat.reaction:") {
        // Emoji is unconstrained Unicode and may itself contain ':' (custom
        // emoji commonly do), so resolve against the complete message-id
        // catalogue rather than splitting an opaque stable id heuristically.
        let (channel, message, reaction) =
            ChatDoc::list_channels(doc)
                .into_iter()
                .find_map(|channel| {
                    ChatDoc::list_messages(doc, &channel.id)
                        .into_iter()
                        .find_map(|message| {
                            message.reactions.iter().find_map(|reaction| {
                                let candidate = format!("{}:{}", message.id, reaction.emoji);
                                (candidate == rest)
                                    .then(|| (channel.clone(), message.clone(), reaction.clone()))
                            })
                        })
                })?;
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == channel.workspace)?;
        let channel_model = channel_resource(&channel);
        let reaction_count = reaction.count();
        let reacted_by_me = reaction.reacted_by(&ctx.me);
        let mut resource = build_reaction_resource(
            ctx,
            &workspace.id,
            &channel_model,
            &message.id,
            message.parent.as_ref(),
            &ReactionResource {
                emoji: reaction.emoji,
                count: reaction_count,
                reacted_by_me,
            },
        );
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    let message_id = id.strip_prefix("chat.message:")?;
    let message = find_message(doc, message_id)?;
    let (channel, message) = message;
    let workspace = ChatDoc::list_workspaces(doc)
        .into_iter()
        .find(|workspace| workspace.id == channel.workspace)?;
    let channel_model = channel_resource(&channel);
    let users = ChatDoc::list_users(doc);
    let message_model = message_resource(ctx, message, &users, &ChatDoc::agent_authors(doc));
    let mut resource =
        publish_message_document(&ctx.me, &ctx.participants, &channel_model, &message_model);
    overlay_missing(
        &mut resource,
        build_message_resource(ctx, &workspace.id, &channel_model, &message_model),
    );
    annotate_contracts(&mut resource);
    Some(resource)
}

/// How many of a channel's latest messages its on-request resource lists.
pub const RESOLVED_CHANNEL_MESSAGES: usize = 20;

fn message_resource(
    ctx: &ChatResourceCtx,
    message: crate::model::Message,
    users: &[crate::model::User],
    agents: &std::collections::HashMap<String, String>,
) -> MessageResource {
    MessageResource {
        label: message_label(
            &message.author,
            &message.text,
            message.attachment.as_ref(),
            users,
            agents,
        ),
        id: message.id,
        author: message.author,
        pinned: message.pinned,
        parent: message.parent,
        attachment: message
            .attachment
            .as_ref()
            .map(|attachment| AttachmentResource {
                name: attachment.name().to_owned(),
                blob: attachment.blob().to_owned(),
            }),
        reactions: message
            .reactions
            .iter()
            .map(|reaction| ReactionResource {
                emoji: reaction.emoji.clone(),
                count: reaction.count(),
                reacted_by_me: reaction.reacted_by(&ctx.me),
            })
            .collect(),
    }
}

/// Resolve an immutable message row and its owning channel once. Resource
/// resolution must read the document, never a currently rendered projection.
fn find_message(
    doc: &AutoCommit,
    message_id: &str,
) -> Option<(crate::model::Channel, crate::model::Message)> {
    ChatDoc::list_channels(doc).into_iter().find_map(|channel| {
        ChatDoc::list_messages(doc, &channel.id)
            .into_iter()
            .find(|message| message.id == message_id)
            .map(|message| (channel, message))
    })
}

fn channel_resource(channel: &crate::model::Channel) -> ChannelResource {
    ChannelResource {
        id: channel.id.clone(),
        name: channel.name.clone(),
        kind: channel.kind,
        archived: channel.archived,
    }
}

/// Workspace-level: create a new text channel in the uncategorised group. The
/// fresh id is minted once, here, at authoring time (like the on-screen "+").
fn new_channel_action_spec(workspace_id: &str) -> ActionSpec {
    ActionSpec::prompted(
        "new-channel",
        "New channel",
        "Name",
        "channel name…",
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
        "category name…",
        ExecutionLocus::ClientLocal,
        {
            let workspace = workspace_id.to_string();
            move |name: &str| {
                let name = name.trim();
                let name = if name.is_empty() {
                    "New Category"
                } else {
                    name
                };
                let id = format!("cat_{}", uuid::Uuid::new_v4().simple());
                encode(&create_category_action(&workspace, &id, name))
            }
        },
    )
}

fn build_category_resource(workspace_id: &str, category: &CategoryResource) -> Resource {
    let mut resource = Resource::new(
        format!("chat.category:{}", category.id),
        "chat.category",
        category.name.clone(),
    )
    .with_action(view_operation(
        "open",
        "Open category",
        &ChatOperation::OpenCategory {
            workspace: workspace_id.to_string(),
            category: category.id.clone(),
        },
    ))
    .with_action(ActionSpec::prompted(
        "rename",
        "Rename category",
        "Name",
        "category name…",
        ExecutionLocus::ClientLocal,
        {
            let id = category.id.clone();
            move |name: &str| encode(&rename_category_action(&id, name.trim()))
        },
    ));
    resource = resource.with_action(immediate_spec(
        if category.collapsed {
            "expand"
        } else {
            "collapse"
        },
        if category.collapsed {
            "Expand category"
        } else {
            "Collapse category"
        },
        &set_category_collapsed_action(&category.id, !category.collapsed),
    ));
    resource.with_action(immediate_spec(
        "delete",
        "Delete category",
        &delete_category_action(&category.id),
    ))
}

/// A channel resource: its actions (post/rename/archive/mark-read/delete),
/// plus — only when it's the open channel — its messages as sub-resources.
#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "the channel action inventory is deliberately adjacent to its canonical resource; ChatOperation consists only of serde-serializable strings, so ActionSpec's infallible builder cannot return an error"
)]
fn build_channel_resource(
    ctx: &ChatResourceCtx,
    doc: &AutoCommit,
    workspace_id: &str,
    ch: &ChannelResource,
    messages: &[MessageResource],
) -> Resource {
    let mut channel = Resource::new(
        format!("chat.channel:{}", ch.id),
        "chat.channel",
        channel_label(ch),
    )
    .with_action(crate::semantic_read::action::spec(&ch.id))
    .with_action(view_operation(
        "open",
        "Open conversation",
        &ChatOperation::OpenChannel {
            workspace: workspace_id.to_string(),
            channel: ch.id.clone(),
        },
    ));

    // Post a message (Send, from the palette). Id + timestamp minted in the
    // closure at authoring time so the payload is self-contained. Only a text
    // channel has a timeline anyone can see (opening a voice channel joins the
    // call), so post/attach are offered there alone, exactly like the composer.
    // `@Name` of a room member becomes a structured mention (`mention::resolve`
    // against the room roster), the same as typing it in the composer.
    let timeline = ch.kind != ChannelKind::Voice;
    if timeline {
        channel = channel.with_action(ActionSpec::prompted(
            "post",
            "Post message",
            "Message",
            "type a message… (@Name mentions a member)",
            ExecutionLocus::ClientLocal,
            {
                let cid = ch.id.clone();
                let me = ctx.me.clone();
                let room = ctx.participants.clone();
                move |text: &str| {
                    let id = format!("m_{}", uuid::Uuid::new_v4().simple());
                    let mentions = crate::mention::resolve(text, &room);
                    encode(&agent_authored(
                        &me,
                        &room,
                        send_message_action(
                            &cid,
                            &id,
                            &me,
                            unix_now_i64(),
                            text,
                            None,
                            None,
                            mentions,
                        ),
                    ))
                }
            },
        ));

        // An agent and the picker use the same local import lifecycle. The path
        // stays local to the machine that can read it; only the resulting bytes
        // and canonical SendMessage action are collaborative.
        channel = channel.with_action(ActionSpec::prompted_operation(
            "attach",
            "Attach file",
            "File path",
            "Documents/report.pdf",
            OperationKind::Effect,
            CHAT_OPERATION_TYPE,
            {
                let channel = ch.id.clone();
                move |path: &str| {
                    EncodedAction::encode_operation(
                        CHAT_OPERATION_TYPE,
                        &ChatOperation::ImportAttachment {
                            channel: channel.clone(),
                            path: path.trim().to_owned(),
                        },
                    )
                    .expect("ChatOperation is always serializable")
                }
            },
        ));
    }

    // Rename — a prompted action carrying the new name.
    channel = channel.with_action(ActionSpec::prompted(
        "rename",
        "Rename channel",
        "Name",
        "channel name…",
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
            move |name: &str| encode(&rename_channel_action(&cid, name.trim()))
        },
    ));

    // Archive / unarchive toggles the stored flag.
    if ch.archived {
        channel = channel.with_action(immediate_spec(
            "unarchive",
            "Unarchive channel",
            &archive_channel_action(&ch.id, false),
        ));
    } else {
        channel = channel.with_action(immediate_spec(
            "archive",
            "Archive channel",
            &archive_channel_action(&ch.id, true),
        ));
    }

    // Mark the channel read: the watermark target is the current top-level
    // count, read from the document at authoring time and baked in.
    let count = ChatDoc::top_level_count(doc, &ch.id) as i64;
    channel = channel.with_action(immediate_spec(
        "mark-read",
        "Mark as read",
        &mark_read_action(&ch.id, &ctx.me, count),
    ));

    // Delete the whole channel.
    channel = channel.with_action(immediate_spec(
        "delete",
        "Delete channel",
        &delete_channel_action(&ch.id),
    ));

    // A stable channel address exposes local call effects. Durable history
    // remains separate from current call authority, presence, and consent.
    if ch.kind == ChannelKind::Voice {
        channel = channel.with_child(build_call_resource(ctx, doc, workspace_id, ch));
    }

    for m in messages {
        let message = build_message_resource(ctx, workspace_id, ch, m);
        channel = channel.with_child(message);
    }

    channel
}

/// Governed call controls are local effects. Durable legacy membership rows
/// never establish a live session or microphone permission.
fn build_call_resource(
    _ctx: &ChatResourceCtx,
    _doc: &AutoCommit,
    workspace_id: &str,
    ch: &ChannelResource,
) -> Resource {
    use ui::native_media::Command;
    let mut call = Resource::new(
        format!("chat.call:{}", ch.id),
        "chat.call",
        format!("Call: {}", channel_label(ch)),
    )
    .with_action(view_operation(
        "open",
        "Open call conversation",
        &ChatOperation::OpenChannel {
            workspace: workspace_id.into(),
            channel: ch.id.clone(),
        },
    ));
    for (id, label, command) in [
        (
            "join",
            "Join muted",
            Command::Join {
                channel_id: ch.id.clone(),
            },
        ),
        ("leave", "Leave call", Command::Leave),
        ("mute", "Mute microphone", Command::SetMuted { muted: true }),
        (
            "unmute",
            "Request microphone",
            Command::SetMuted { muted: false },
        ),
        ("status", "Call status", Command::Status),
    ] {
        call = call.with_action(voice_operation(id, label, &ch.id, command));
    }
    call.with_action(ActionSpec::prompted_operation(
        "select-input",
        "Select microphone",
        "Input",
        "Input from call status",
        OperationKind::Effect,
        CHAT_OPERATION_TYPE,
        {
            let channel = ch.id.clone();
            move |input: &str| {
                EncodedAction::encode_operation(
                    CHAT_OPERATION_TYPE,
                    &ChatOperation::NativeVoice {
                        channel: channel.clone(),
                        request: ui::native_media::Request::new(Command::SelectInput {
                            input_id: input.trim().into(),
                        }),
                    },
                )
                // Unreachable for these bounded string fields; refuse at
                // invocation rather than panicking the resource builder.
                .unwrap_or_else(|error| EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: error.to_string().into_bytes(),
                })
            }
        },
    ))
}

#[allow(
    clippy::expect_used,
    reason = "native call commands contain only serializable bounded values"
)]
pub(crate) fn voice_operation(
    id: &str,
    label: &str,
    channel: &str,
    command: ui::native_media::Command,
) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::Effect,
        CHAT_OPERATION_TYPE,
        &ChatOperation::NativeVoice {
            channel: channel.into(),
            request: ui::native_media::Request::new(command),
        },
    )
    .expect("native call request is serializable")
}

/// A message resource: edit/react (prompted), pin-or-unpin, and delete.
fn build_message_resource(
    ctx: &ChatResourceCtx,
    workspace_id: &str,
    ch: &ChannelResource,
    m: &MessageResource,
) -> Resource {
    let mut message = Resource::new(
        format!("chat.message:{}", m.id),
        "chat.message",
        m.label.clone(),
    )
    .with_action(view_operation(
        "open",
        "Reveal message",
        &ChatOperation::RevealMessage {
            workspace: workspace_id.to_string(),
            channel: ch.id.clone(),
            message: m.id.clone(),
            parent: m.parent.clone(),
        },
    ));

    // Edit — prompted; the edit timestamp is minted in the closure.
    message = message.with_action(ActionSpec::prompted(
        "edit",
        "Edit message",
        "Message",
        "new text…",
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
            let mid = m.id.clone();
            move |text: &str| encode(&edit_message_action(&cid, &mid, text, unix_now_i64()))
        },
    ));

    // React — prompted for an emoji; adds the local peer's reaction.
    message = message.with_action(ActionSpec::prompted(
        "react",
        "Add reaction",
        "Emoji",
        "👍",
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
            let mid = m.id.clone();
            let me = ctx.me.clone();
            move |emoji: &str| encode(&add_reaction_action(&cid, &mid, emoji.trim(), &me))
        },
    ));

    // Pin / unpin toggles the stored flag.
    if m.pinned {
        message = message.with_action(immediate_spec(
            "unpin",
            "Unpin message",
            &pin_message_action(&ch.id, &m.id, false),
        ));
    } else {
        message = message.with_action(immediate_spec(
            "pin",
            "Pin message",
            &pin_message_action(&ch.id, &m.id, true),
        ));
    }

    // Delete the message.
    message = message.with_action(immediate_spec(
        "delete",
        "Delete message",
        &delete_message_action(&ch.id, &m.id),
    ));

    message = message.with_action(read_message_spec(&m.id));
    if m.author == ctx.me && acts_as_agent(ctx) {
        message = message.with_action(set_work_spec(&ch.id, &m.id));
    }

    if let Some(attachment) = &m.attachment {
        message = message.with_child(build_attachment_resource(
            workspace_id,
            ch,
            &m.id,
            m.parent.as_ref(),
            attachment,
        ));
    }
    for reaction in &m.reactions {
        message = message.with_child(build_reaction_resource(
            ctx,
            workspace_id,
            ch,
            &m.id,
            m.parent.as_ref(),
            reaction,
        ));
    }

    message
}

/// Whether the tree is authored as an agent: an agent task participant the
/// room names as an agent (a delivered task, or a host that acts as one).
fn acts_as_agent(ctx: &ChatResourceCtx) -> bool {
    ctx.me
        .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
        && ctx
            .participants
            .iter()
            .any(|participant| participant.id == ctx.me && participant.is_agent)
}

/// What `read` returns for a message.
pub fn message_read_schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::json!({"type":"object",
            "required":["id","channel","parent","author","authorName","agent","mine","ts","edited","text","work"],
            "properties":{
                "id":{"type":"string"},"channel":{"type":"string"},"parent":{"type":["string","null"]},
                "author":{"type":"string"},"authorName":{"type":"string"},"agent":{"type":"boolean"},
                "mine":{"type":"boolean"},"ts":{"type":"integer"},"edited":{"type":"boolean"},
                "text":{"type":"string"},"work":{"type":["object","null"]}
            }})
    })
}

/// Read one message as a value, for a caller that follows the conversation
/// through the action door rather than the screen: the label carries only
/// the author's name and the first words.
fn read_message_spec(message: &str) -> ActionSpec {
    view_operation(
        "read",
        "Read message",
        &ChatOperation::ReadMessage {
            message: message.to_owned(),
        },
    )
    .with_contract(tilo_actions::ActionContract {
        result: tilo_actions::ResultSpec::Json {
            schema: message_read_schema().clone(),
        },
        ..tilo_actions::ActionContract::view()
    })
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
    let did = || args.text("did").unwrap_or_default().to_owned();
    let state = match args.text("state").unwrap_or_default() {
        "taken" => AgentWorkState::Taken,
        "doing" => AgentWorkState::Doing {
            did: did(),
            about: args.text("about").unwrap_or_default().to_owned(),
        },
        "done" => AgentWorkState::Done { did: did() },
        "refused" => AgentWorkState::Refused,
        other => return Err(format!("unknown agent work state `{other}`")),
    };
    let mut trace = Vec::new();
    for (index, step) in args.array("steps").unwrap_or_default().iter().enumerate() {
        let text = |field: &str| step.get(field).and_then(serde_json::Value::as_str);
        let resource = text("resource").ok_or("a step names its resource")?;
        let label = text("label").ok_or("a step names its operation's label")?;
        let outcome = match text("outcome").unwrap_or("applied") {
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
    if let Some(why) = args.text("why") {
        work = work.because(AgentReasoning {
            route: AgentRoute::Model,
            why: why.to_owned(),
        });
    }
    Ok(work)
}

/// The portable attachment is an addressable child of its message. Opening it
/// deliberately reveals the owning message: fetching/opening the bytes happens in
/// the content app that knows the file type, whereas Chat's truthful action is
/// to navigate to the durable conversation record that shared it.
fn build_attachment_resource(
    workspace_id: &str,
    ch: &ChannelResource,
    message_id: &str,
    parent: Option<&String>,
    attachment: &AttachmentResource,
) -> Resource {
    Resource::new(
        format!("chat.attachment:{message_id}"),
        "chat.attachment",
        attachment.name.clone(),
    )
    .with_action(view_operation(
        "open-message",
        "Reveal shared message",
        &ChatOperation::RevealMessage {
            workspace: workspace_id.to_owned(),
            channel: ch.id.clone(),
            message: message_id.to_owned(),
            parent: parent.cloned(),
        },
    ))
}

/// A reaction is both a durable aggregate and a direct action surface. The
/// canonical resource contains the current count in its label and its one
/// available mutation is the same absolute Add/Remove action the painted chip
/// dispatches; it never reimplements a local toggle.
fn build_reaction_resource(
    ctx: &ChatResourceCtx,
    workspace_id: &str,
    ch: &ChannelResource,
    message_id: &str,
    parent: Option<&String>,
    reaction: &ReactionResource,
) -> Resource {
    let label = format!("{} · {}", reaction.emoji, reaction.count);
    let mut resource = Resource::new(
        format!("chat.reaction:{message_id}:{}", reaction.emoji),
        "chat.reaction",
        label,
    )
    .with_action(view_operation(
        "open-message",
        "Reveal message",
        &ChatOperation::RevealMessage {
            workspace: workspace_id.to_owned(),
            channel: ch.id.clone(),
            message: message_id.to_owned(),
            parent: parent.cloned(),
        },
    ));
    let action = if reaction.reacted_by_me {
        remove_reaction_action(&ch.id, message_id, &reaction.emoji, &ctx.me)
    } else {
        add_reaction_action(&ch.id, message_id, &reaction.emoji, &ctx.me)
    };
    resource = resource.with_action(immediate_spec(
        if reaction.reacted_by_me {
            "remove"
        } else {
            "add"
        },
        if reaction.reacted_by_me {
            "Remove my reaction"
        } else {
            "Add my reaction"
        },
        &action,
    ));
    resource
}

fn message_label(
    author_id: &str,
    text: &str,
    attachment: Option<&crate::model::Attachment>,
    users: &[crate::model::User],
    agents: &std::collections::HashMap<String, String>,
) -> String {
    message_label_for_resource(author_id, text, attachment, users, agents)
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

// Inherent resource publication (was ResourceProvider)
impl crate::app::ChatApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let ctx = self.chat_resource_ctx();
        // Heads identify the committed document exactly; frames with pending
        // (uncommitted) operations rebuild.
        if automerge::transaction::Transactable::pending_ops(doc) != 0 {
            return provide_resources(&ctx, doc);
        }
        let heads: Vec<String> = automerge::transaction::Transactable::base_heads(doc)
            .iter()
            .map(ToString::to_string)
            .collect();
        let key = format!("{heads:?}|{ctx:?}");
        if let Some((cached, resources)) = self.resource_memo.borrow().as_ref()
            && *cached == key
        {
            return resources.clone();
        }
        let resources = provide_resources(&ctx, doc);
        *self.resource_memo.borrow_mut() = Some((key, resources.clone()));
        resources
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        resolve_resource(&self.chat_resource_ctx(), doc, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::ChatDoc;
    use crate::model::{self, ChannelKind};
    use tilo_actions::LocalTransport;

    /// A fixed authoring timestamp for tests — actions carry it in the payload,
    /// so nothing here reads the wall-clock.
    const TS: i64 = 1_782_000_000;

    /// The published tree is one `publish` root; workspaces hang under it.
    fn workspace<'a>(resources: &'a [Resource], id: &str) -> &'a Resource {
        let want = format!("chat.workspace:{id}");
        resources
            .iter()
            .find(|resource| resource.id.as_str() == want)
            .or_else(|| {
                resources.iter().find_map(|root| {
                    root.children
                        .iter()
                        .find(|resource| resource.id.as_str() == want)
                })
            })
            .unwrap_or_else(|| panic!("{want} published"))
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Dispatch an action through the executor, as the on-screen affordance path
    /// does.
    fn dispatch(doc: &mut AutoCommit, action: &ChatAction) {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        doc
    }

    #[test]
    fn native_voice_resources_are_local_effects_despite_legacy_membership() {
        let mut doc = seeded();
        ChatDoc::voice_join(&mut doc, "c_standup", model::ME);
        let call = resolve_resource(&ctx_from(&doc), &doc, "chat.call:c_standup").unwrap();
        let leave = call.actions.iter().find(|a| a.id == "leave").unwrap();
        assert_eq!(leave.kind, OperationKind::Effect);
        assert_eq!(leave.operation_type.as_deref(), Some(CHAT_OPERATION_TYPE));
        assert!(
            call.actions.iter().any(|a| a.id == "join"),
            "durable rows cannot claim a local session"
        );
        assert!(!call.actions.iter().any(|a| a.id == "enable-camera"));
        assert!(call.actions.iter().any(|a| a.id == "status"));
        assert!(call.actions.iter().any(|a| a.id == "select-input"));
    }

    fn ctx_from(doc: &AutoCommit) -> ChatResourceCtx {
        let channels = ChatDoc::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == "ws_acme")
            .map(|c| ChannelResource {
                id: c.id,
                name: c.name,
                kind: c.kind,
                archived: c.archived,
            })
            .collect();
        let messages = ChatDoc::list_messages(doc, "c_crit")
            .into_iter()
            .map(|m| MessageResource {
                author: m.author.clone(),
                id: m.id,
                label: m.text.chars().take(24).collect(),
                pinned: m.pinned,
                parent: m.parent,
                attachment: m.attachment.as_ref().map(|attachment| AttachmentResource {
                    name: attachment.name().to_owned(),
                    blob: attachment.blob().to_owned(),
                }),
                reactions: m
                    .reactions
                    .iter()
                    .map(|reaction| ReactionResource {
                        emoji: reaction.emoji.clone(),
                        count: reaction.count(),
                        reacted_by_me: reaction.reacted_by(model::ME),
                    })
                    .collect(),
            })
            .collect();
        ChatResourceCtx {
            me: model::ME.to_string(),
            workspace_id: "ws_acme".to_string(),
            workspace_name: "Acme Design".to_string(),
            channels,
            active_channel: "c_crit".to_string(),
            messages,
            participants: ChatDoc::list_users(doc)
                .into_iter()
                .map(|u| crate::mention::Participant {
                    id: u.id,
                    name: u.name,
                    is_agent: u.is_agent,
                })
                .collect(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the resource covenant is intentionally asserted as one end-to-end inventory"
    )]
    fn resources_publish_a_bounded_snapshot_with_lazy_canonical_children() {
        // Phase: ctrl_k_publishes_workspace_channel_and_message_tree.
        {
            let doc = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);
            let ws = workspace(&resources, "ws_acme");
            assert_eq!(ws.kind, "chat.workspace");
            let ws_actions: Vec<&str> = ws.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(ws_actions.contains(&"new-channel"), "{ws_actions:?}");

            // The open channel (c_crit) carries message children; a channel exports
            // rename / archive / mark-read / delete.
            let crit = ws
                .children
                .iter()
                .find(|c| c.id.as_str() == "chat.channel:c_crit")
                .expect("the open channel is a resource");
            assert_eq!(crit.kind, "chat.channel");
            let c_actions: Vec<&str> = crit.actions.iter().map(|a| a.id.as_str()).collect();
            for want in ["post", "rename", "archive", "mark-read", "delete"] {
                assert!(
                    c_actions.contains(&want),
                    "channel missing {want}: {c_actions:?}"
                );
            }
            assert!(crit.children.is_empty(), "messages resolve lazily");
            let source = ChatDoc::list_messages(&doc, "c_crit").remove(0);
            let ctx = ctx_from(&doc);
            let ch = ctx.channels.iter().find(|ch| ch.id == "c_crit").unwrap();
            let msg_model = MessageResource {
                author: source.author.clone(),
                id: source.id,
                label: source.text,
                pinned: source.pinned,
                parent: source.parent,
                attachment: source
                    .attachment
                    .as_ref()
                    .map(|attachment| AttachmentResource {
                        name: attachment.name().to_owned(),
                        blob: attachment.blob().to_owned(),
                    }),
                reactions: source
                    .reactions
                    .iter()
                    .map(|reaction| ReactionResource {
                        emoji: reaction.emoji.clone(),
                        count: reaction.count(),
                        reacted_by_me: reaction.reacted_by(model::ME),
                    })
                    .collect(),
            };
            let msg = build_message_resource(&ctx, "ws_acme", ch, &msg_model);
            assert_eq!(msg.kind, "chat.message");
            let m_actions: Vec<&str> = msg.actions.iter().map(|a| a.id.as_str()).collect();
            for want in ["edit", "react", "pin", "delete"] {
                assert!(
                    m_actions.contains(&want),
                    "message missing {want}: {m_actions:?}"
                );
            }

            // Non-open channels are exported too, so a global message hit can be
            // resolved without granting it a private navigation escape hatch.
            let general = ws
                .children
                .iter()
                .find(|c| c.id.as_str() == "chat.channel:c_general")
                .expect("a second channel is present");
            assert!(general.children.is_empty());

            let call = resolve_resource(&ctx, &doc, "chat.call:c_huddle")
                .expect("voice call resolves as its own canonical resource");
            assert_eq!(call.kind, "chat.call");
            let call_actions: Vec<_> = call
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for want in ["open", "join"] {
                assert!(
                    call_actions.contains(&want),
                    "call missing {want}: {call_actions:?}"
                );
            }
            let join = call
                .actions
                .iter()
                .find(|action| action.id == "join")
                .expect("join action");
            assert_eq!(join.kind, OperationKind::Effect);
            let operation: ChatOperation = serde_json::from_slice(
                &ui::testing::action_contract::spec_encoding(join, None).payload,
            )
            .unwrap();
            assert!(matches!(
                operation,
                ChatOperation::NativeVoice {
                    request: ui::native_media::Request {
                        command: ui::native_media::Command::Join { .. },
                        ..
                    },
                    ..
                }
            ));
            for want in ["leave", "mute", "unmute", "status"] {
                assert!(call_actions.contains(&want));
            }
            assert!(!call_actions.contains(&"deafen") && !call_actions.contains(&"audio-only"));

            // Every exported action is reachable — flatten the forest (parents before
            // children) and confirm the message + workspace actions are present.
            let flat = tilo_actions::flatten(&resources);
            let flat_labels: Vec<&str> = flat.iter().map(|f| f.action.label.as_str()).collect();
            assert!(flat_labels.contains(&"New channel"), "{flat_labels:?}");
            assert!(!flat_labels.contains(&"Delete message"), "{flat_labels:?}");
        }

        // Phase: attachment_and_reaction_resources_resolve_with_canonical_actions.
        {
            let doc = seeded();
            let ctx = ctx_from(&doc);

            let attachment = resolve_resource(&ctx, &doc, "chat.attachment:m_concept")
                .expect("attached message exposes its portable child resource");
            assert_eq!(attachment.kind, "chat.attachment");
            assert!(
                attachment
                    .actions
                    .iter()
                    .any(|action| action.id == "open-message")
            );

            let reaction = resolve_resource(&ctx, &doc, "chat.reaction:m_concept:❤️")
                .expect("reaction aggregate exposes its canonical child resource");
            assert_eq!(reaction.kind, "chat.reaction");
            let remove = reaction
                .actions
                .iter()
                .find(|action| action.id == "remove")
                .expect("your current reaction offers the shared remove action");
            let expected = encode(&remove_reaction_action(
                "c_crit",
                "m_concept",
                "❤️",
                model::ME,
            ));
            assert_eq!(
                ui::testing::action_contract::spec_encoding(remove, None).payload,
                expected.payload
            );
        }

        // Phase: ten_thousand_messages_do_not_expand_the_open_snapshot.
        {
            let doc = seeded();
            let mut ctx = ctx_from(&doc);
            ctx.messages = (0..10_000)
                .map(|id| MessageResource {
                    id: format!("message-{id}"),
                    author: model::ME.to_owned(),
                    label: format!("Message {id}"),
                    pinned: false,
                    parent: None,
                    attachment: None,
                    reactions: Vec::new(),
                })
                .collect();
            let resources = provide_resources(&ctx, &doc);
            assert!(
                workspace(&resources, "ws_acme")
                    .children
                    .iter()
                    .filter(|resource| resource.kind == "chat.channel")
                    .all(|channel| channel
                        .children
                        .iter()
                        .all(|child| child.kind != "chat.message"))
            );
        }

        // Phase: large_organisation_keeps_context_bounded_and_resolves_omitted_room.
        {
            fn resource_count(resources: &[Resource]) -> usize {
                resources
                    .iter()
                    .map(|resource| 1 + resource_count(&resource.children))
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
            let ctx = ctx_from(&doc);
            let resources = provide_resources(&ctx, &doc);
            assert!(resource_count(&resources) <= EAGER_RESOURCE_SNAPSHOT_LIMIT);
            assert!(
                workspace(&resources, "ws_acme")
                    .children
                    .iter()
                    .all(|resource| resource.id.as_str() != "chat.channel:c_late_107")
            );
            let lazy = resolve_resource(&ctx, &doc, "chat.channel:c_late_107")
                .expect("an omitted room remains directly addressable");
            assert_eq!(lazy.kind, "chat.channel");
            assert!(lazy.actions.iter().any(|action| action.id == "open"));
            assert!(lazy.actions.iter().any(|action| action.id == "post"));
        }

        // Phase: resource_actions_declare_undo_and_delete_confirmation.
        {
            let doc = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);
            let channel = workspace(&resources, "ws_acme")
                .children
                .iter()
                .find(|r| r.id.as_str() == "chat.channel:c_crit")
                .unwrap();
            assert!(
                channel
                    .actions
                    .iter()
                    .filter(|action| action.kind == OperationKind::Document)
                    .all(|action| action.contract.reversible)
            );
            let open = channel
                .actions
                .iter()
                .find(|action| action.id == "open")
                .unwrap();
            assert_eq!(open.kind, OperationKind::View);
            assert!(!open.contract.reversible);
            let attach = channel
                .actions
                .iter()
                .find(|action| action.id == "attach")
                .unwrap();
            assert_eq!(attach.kind, OperationKind::Effect);
            assert_eq!(attach.operation_type.as_deref(), Some(CHAT_OPERATION_TYPE));
            let source = ChatDoc::list_messages(&doc, "c_crit").remove(0);
            let ctx = ctx_from(&doc);
            let ch = ctx.channels.iter().find(|ch| ch.id == "c_crit").unwrap();
            let message = build_message_resource(
                &ctx,
                "ws_acme",
                ch,
                &MessageResource {
                    author: source.author.clone(),
                    id: source.id,
                    label: source.text,
                    pinned: source.pinned,
                    parent: source.parent,
                    attachment: source
                        .attachment
                        .as_ref()
                        .map(|attachment| AttachmentResource {
                            name: attachment.name().to_owned(),
                            blob: attachment.blob().to_owned(),
                        }),
                    reactions: source
                        .reactions
                        .iter()
                        .map(|reaction| ReactionResource {
                            emoji: reaction.emoji.clone(),
                            count: reaction.count(),
                            reacted_by_me: reaction.reacted_by(model::ME),
                        })
                        .collect(),
                },
            );
            assert_eq!(
                message
                    .actions
                    .iter()
                    .find(|action| action.id == "open")
                    .unwrap()
                    .kind,
                OperationKind::View
            );
            assert!(
                message
                    .actions
                    .iter()
                    .filter(|action| action.id != "open" && action.id != "read")
                    .all(|action| action.kind == OperationKind::Document)
            );
            let read = message.actions.iter().find(|a| a.id == "read").unwrap();
            assert_eq!(read.kind, OperationKind::View);
            assert!(matches!(
                read.contract.result,
                tilo_actions::ResultSpec::Json { .. }
            ));
            let delete = channel.actions.iter().find(|a| a.id == "delete").unwrap();
            assert_eq!(delete.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                delete.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
        }
    }

    /// A voice channel has no timeline anyone sees (opening it joins the
    /// call), so it offers no `post`/`attach`, and a `SendMessage` into it is
    /// refused: an agent's post can never land where no member can read it.
    #[test]
    fn voice_channels_offer_no_post_and_refuse_messages() {
        let mut doc = seeded();
        let voice = ChatDoc::list_channels(&doc)
            .into_iter()
            .find(|c| c.workspace == "ws_acme" && c.kind == ChannelKind::Voice)
            .expect("seeded voice channel");
        let resources = provide_resources(&ctx_from(&doc), &doc);
        let ws = workspace(&resources, "ws_acme");
        let channel = |id: &str| {
            ws.children
                .iter()
                .find(|c| c.id.as_str() == format!("chat.channel:{id}"))
                .unwrap_or_else(|| panic!("{id} published"))
        };
        let ids = |r: &Resource| r.actions.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
        let voice_actions = ids(channel(&voice.id));
        assert!(
            !voice_actions.iter().any(|a| a == "post" || a == "attach"),
            "{voice_actions:?}"
        );
        let text_actions = ids(channel("c_crit"));
        assert!(text_actions.iter().any(|a| a == "post"), "{text_actions:?}");
        assert!(
            text_actions.iter().any(|a| a == "attach"),
            "{text_actions:?}"
        );
        dispatch(
            &mut doc,
            &send_message_action(
                &voice.id,
                "m_voice_post",
                model::ME,
                TS,
                "unseen",
                None,
                None,
                Vec::new(),
            ),
        );
        assert!(
            ChatDoc::list_messages(&doc, &voice.id)
                .iter()
                .all(|m| m.id != "m_voice_post")
        );
    }

    /// `@Name` in a catalogue `post` resolves against the room roster the
    /// caller supplies (workspace members under their room names), not only
    /// document users: a member who never posted is still mentionable.
    #[test]
    fn post_resolves_mentions_of_roster_members_without_document_rows() {
        let doc = seeded();
        let mut ctx = ctx_from(&doc);
        ctx.participants.push(crate::mention::Participant {
            id: "member-ada".into(),
            name: "Ada Lovelace".into(),
            is_agent: false,
        });
        let resources = provide_resources(&ctx, &doc);
        let crit = workspace(&resources, "ws_acme")
            .children
            .iter()
            .find(|c| c.id.as_str() == "chat.channel:c_crit")
            .unwrap();
        let post = crit.actions.iter().find(|a| a.id == "post").unwrap();
        // The message id is minted per build, so read the palette's build
        // directly rather than comparing it with the external path.
        let tilo_actions::ActionInput::Prompted { build, .. } = &post.input else {
            panic!("post is a prompted action");
        };
        let encoded = build("thanks @Ada Lovelace, done");
        let decoded: ChatAction = serde_json::from_slice(&encoded.payload).unwrap();
        let ChatAction::SendMessage { mentions, text, .. } = decoded else {
            panic!("post encodes a SendMessage")
        };
        assert_eq!(mentions.len(), 1, "{mentions:?}");
        assert_eq!(mentions[0].user, "member-ada");
        assert_eq!(mentions[0].slice(&text), "@Ada Lovelace");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn message_actions_mutate_the_document_exactly_once() {
        // Phase: send_message_action_appends_the_row.
        {
            let mut doc = seeded();
            let before = ChatDoc::list_messages(&doc, "c_general").len();
            dispatch(
                &mut doc,
                &send_message_action(
                    "c_general",
                    "m_test1",
                    model::ME,
                    TS,
                    "hello",
                    None,
                    None,
                    Vec::new(),
                ),
            );
            let msgs = ChatDoc::list_messages(&doc, "c_general");
            assert_eq!(msgs.len(), before + 1);
            let m = msgs.iter().find(|m| m.id == "m_test1").unwrap();
            assert_eq!(m.text, "hello");
            assert_eq!(m.author, model::ME);
        }

        // Phase: send_message_retries_are_exactly_once_and_reject_invalid_targets.
        {
            let mut doc = seeded();
            let action = send_message_action(
                "c_general",
                "m_retry_once",
                model::ME,
                TS,
                "retry-safe",
                None,
                None,
                Vec::new(),
            );
            dispatch(&mut doc, &action);
            dispatch(&mut doc, &action);
            assert_eq!(
                ChatDoc::list_messages(&doc, "c_general")
                    .iter()
                    .filter(|message| message.id == "m_retry_once")
                    .count(),
                1,
                "replaying one authored action cannot create a second resource row"
            );

            dispatch(
                &mut doc,
                &send_message_action(
                    "c_missing",
                    "m_missing_target",
                    model::ME,
                    TS,
                    "must not create a hidden channel",
                    None,
                    None,
                    Vec::new(),
                ),
            );
            assert!(ChatDoc::list_messages(&doc, "c_missing").is_empty());
        }

        // Phase: set_agent_work_action_records_progress_and_only_ever_supplies_empty_words.
        {
            use crate::model::{AgentReasoning, AgentRoute, AgentWork, AgentWorkState};

            let mut doc = seeded();
            dispatch(
                &mut doc,
                &send_message_action(
                    "c_crit",
                    "m_reply",
                    "agent_claude",
                    TS,
                    "",
                    Some("m_me1".into()),
                    None,
                    Vec::new(),
                ),
            );
            let taken = AgentWork::new(AgentWorkState::Taken, TS);
            dispatch(
                &mut doc,
                &set_agent_work_action("c_crit", "m_reply", taken.clone(), None),
            );
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_reply")
                .unwrap();
            assert_eq!(m.work, Some(taken), "the taken record was not stored");
            assert_eq!(m.text, "", "no words yet — the body must stay empty");

            let done = AgentWork::new(AgentWorkState::Done { did: "Crop".into() }, TS + 5).because(
                AgentReasoning {
                    route: AgentRoute::Floor,
                    why: "matched the word processor".into(),
                },
            );
            dispatch(
                &mut doc,
                &set_agent_work_action("c_crit", "m_reply", done.clone(), Some("Cropped it.")),
            );
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_reply")
                .unwrap();
            assert_eq!(m.work, Some(done), "the finished record was not stored");
            assert_eq!(m.text, "Cropped it.", "the agent's words were not supplied");

            // A later state must not restate the body — it is authored once.
            let refused = AgentWork::new(AgentWorkState::Refused, TS + 9);
            dispatch(
                &mut doc,
                &set_agent_work_action("c_crit", "m_reply", refused.clone(), Some("never mind")),
            );
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_reply")
                .unwrap();
            assert_eq!(m.work, Some(refused), "the last state must still win");
            assert_eq!(
                m.text, "Cropped it.",
                "an already-written body must never be overwritten"
            );
        }

        // Phase: reaction_add_and_remove_actions_round_trip.
        {
            let mut doc = seeded();
            dispatch(
                &mut doc,
                &add_reaction_action("c_crit", "m_me1", "🔥", model::ME),
            );
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_me1")
                .unwrap();
            let fire = m.reactions.iter().find(|r| r.emoji == "🔥").unwrap();
            assert_eq!(fire.count(), 1, "the add landed one reaction");
            dispatch(
                &mut doc,
                &remove_reaction_action("c_crit", "m_me1", "🔥", model::ME),
            );
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_me1")
                .unwrap();
            assert!(
                !m.reactions.iter().any(|r| r.emoji == "🔥"),
                "remove cleared it"
            );
        }

        // Phase: add_reaction_is_idempotent_at_the_doc_level.
        {
            let mut doc = seeded();
            ChatDoc::add_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
            ChatDoc::add_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_me1")
                .unwrap();
            let fire = m.reactions.iter().find(|r| r.emoji == "🔥").unwrap();
            assert_eq!(fire.count(), 1, "a repeated add does not double-count");
        }

        // Phase: pin_and_delete_message_actions.
        {
            let mut doc = seeded();
            dispatch(&mut doc, &pin_message_action("c_crit", "m_me1", true));
            let m = ChatDoc::list_messages(&doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_me1")
                .unwrap();
            assert!(m.pinned, "the message is pinned");
            dispatch(&mut doc, &delete_message_action("c_crit", "m_me1"));
            assert!(
                ChatDoc::list_messages(&doc, "c_crit")
                    .into_iter()
                    .all(|m| m.id != "m_me1"),
                "the message row was deleted"
            );
        }
    }

    #[test]
    fn channel_and_category_actions_round_trip_and_target_history() {
        // Phase: channel_crud_actions_round_trip.
        {
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
            assert!(
                ChatDoc::list_channels(&doc)
                    .iter()
                    .any(|c| c.id == "c_new" && c.name == "design-ops")
            );
            dispatch(&mut doc, &rename_channel_action("c_new", "ops"));
            assert!(
                ChatDoc::list_channels(&doc)
                    .iter()
                    .any(|c| c.id == "c_new" && c.name == "ops")
            );
            dispatch(&mut doc, &archive_channel_action("c_new", true));
            assert!(
                ChatDoc::list_channels(&doc)
                    .iter()
                    .any(|c| c.id == "c_new" && c.archived)
            );
            dispatch(&mut doc, &delete_channel_action("c_new"));
            assert!(ChatDoc::list_channels(&doc).iter().all(|c| c.id != "c_new"));
        }

        // Phase: category_mutations_replay_through_resources_and_history.
        {
            let mut doc = seeded();
            dispatch(
                &mut doc,
                &create_category_action("ws_acme", "cat_action", "Design"),
            );
            dispatch(
                &mut doc,
                &rename_category_action("cat_action", "Design reviews"),
            );
            dispatch(&mut doc, &set_category_collapsed_action("cat_action", true));
            assert!(
                ChatDoc::list_categories(&doc)
                    .iter()
                    .any(|category| category.id == "cat_action"
                        && category.name == "Design reviews"
                        && category.collapsed)
            );

            let resources = provide_resources(&ctx_from(&doc), &doc);
            let category = workspace(&resources, "ws_acme")
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "chat.category:cat_action")
                .unwrap();
            assert!(category.actions.iter().any(|action| action.id == "open"));
            assert!(category.actions.iter().any(|action| action.id == "rename"));
            assert!(category.actions.iter().any(|action| action.id == "expand"));
            assert!(category.actions.iter().any(|action| action.id == "delete"));
            assert_eq!(
                history_context(&delete_category_action("cat_action"))
                    .resource
                    .unwrap()
                    .id,
                "chat.category:cat_action"
            );
        }

        // Phase: message_action_history_targets_the_message.
        {
            let context = history_context(&ChatAction::DeleteMessage {
                channel: "launch".into(),
                id: "msg-9".into(),
            });
            assert_eq!(context.action_label, "Delete message");
            let resource = context.resource.expect("message resource");
            assert_eq!(resource.id, "chat.message:msg-9");
            assert_eq!(resource.kind, "chat.message");
        }
    }

    /// The open workspace snapshot plus the lazily resolved resources Ctrl+K,
    /// search and agents reach by address: every message of the open channel
    /// (with its attachment and reaction children) and the voice call.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let ctx = ctx_from(doc);
        let mut resources = provide_resources(&ctx, doc);
        let lazy = ChatDoc::list_messages(doc, "c_crit")
            .into_iter()
            .map(|message| format!("chat.message:{}", message.id));
        resources.extend(
            lazy.map(|id| {
                resolve_resource(&ctx, doc, &id).unwrap_or_else(|| panic!("{id} resolves"))
            }),
        );
        resources
    }

    fn first_crit_message(doc: &AutoCommit) -> String {
        ChatDoc::list_messages(doc, "c_crit").remove(0).id
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["New channel", "Rename channel", "Pin message", "Join muted"],
        replay: [
            send_message => |_| encode(&send_message_action(
                "c_general",
                "m_conv",
                "u_aria",
                TS,
                "converge",
                None,
                None,
                Vec::new(),
            )),
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
            send_message => |_| encode(&send_message_action(
                "c_general",
                "m_undo",
                model::ME,
                TS,
                "undo me",
                None,
                None,
                Vec::new(),
            )),
        ],
        encodes: [
            delete_channel => ("chat.channel:c_crit", "delete", |_, _| {
                encode(&delete_channel_action("c_crit"))
            }),
            rename_channel => ("chat.channel:c_crit", "rename", arg: "planning", |_, _| {
                encode(&rename_channel_action("c_crit", "planning"))
            }),
            pin_message => ("chat.message", "pin", |doc, _| {
                encode(&pin_message_action("c_crit", &first_crit_message(doc), true))
            }),
            remove_reaction => ("chat.reaction:m_concept:❤️", "remove", |_, _| {
                encode(&remove_reaction_action("c_crit", "m_concept", "❤️", model::ME))
            }),
        ],
    }
}
