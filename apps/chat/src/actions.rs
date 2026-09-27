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
//! per-channel draft `Text`, diff-spliced as you type — see [`crate::doc`]),
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

use tilo_actions::remote::{InvokeArgument, WORKSPACE_REQUEST_TYPE, WorkspaceRequest};
use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, PermissionRequirement, Resource, ResourceProvider,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatOperation {
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
    /// How this peer lists a feedback channel's reports. Presentation only.
    SetReportView {
        channel: String,
        view: crate::feedback::ReportView,
    },
    /// Post, reply or react in a feedback channel. Chat writes it to the
    /// author's own comment record (see [`crate::comments`]), which is the
    /// only change a Commenter may make, so it is an operation Chat performs
    /// rather than a change to the conversation document. Without a
    /// collaboration identity it falls back to the conversation document.
    Comment {
        action: Box<ChatAction>,
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

/// An action whose work is a request to the workspace Chat runs in — opening
/// another app's resource, sharing a record into the room, replaying a
/// recording. The host forwards it to the broker, so it is admitted, checked
/// and answered like any other invocation, and a workspace that does not
/// offer the request fails it visibly rather than Chat claiming success.
#[allow(
    clippy::expect_used,
    reason = "WorkspaceRequest holds only strings and a JSON argument, so encoding cannot fail; ActionSpec's builder returns an EncodedAction directly"
)]
pub(crate) fn workspace_request_spec(
    id: &str,
    label: &str,
    description: &str,
    permission: PermissionRequirement,
    request: WorkspaceRequest,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        description,
        OperationKind::Effect,
        WORKSPACE_REQUEST_TYPE,
        Vec::new(),
        move |_| {
            EncodedAction::encode_operation(WORKSPACE_REQUEST_TYPE, &request)
                .expect("WorkspaceRequest is always serializable")
        },
    )
    .requiring(permission)
}

/// A participation action: in a feedback channel, the comment operation that
/// carries it; anywhere else, the document action itself.
#[allow(
    clippy::expect_used,
    reason = "ChatOperation is serde-derived from serializable fields; ActionSpec requires an immediate value"
)]
fn participation_spec(id: &str, label: &str, feedback: bool, action: &ChatAction) -> ActionSpec {
    if feedback {
        ActionSpec::operation(
            id,
            label,
            OperationKind::Effect,
            CHAT_OPERATION_TYPE,
            &ChatOperation::Comment {
                action: Box::new(action.clone()),
            },
        )
        .expect("ChatOperation is always serializable")
        .requiring(PermissionRequirement::Comment)
        .as_participant()
    } else {
        immediate_spec(id, label, action).requiring(PermissionRequirement::Comment)
    }
}

/// A prompted participation action (post, reply, react): the comment
/// operation in a feedback channel, the document action elsewhere.
fn participation_prompt(
    id: &str,
    label: &str,
    prompt: &str,
    placeholder: &str,
    feedback: bool,
    build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    let spec = if feedback {
        ActionSpec::prompted_operation(
            id,
            label,
            prompt,
            placeholder,
            OperationKind::Effect,
            CHAT_OPERATION_TYPE,
            move |text: &str| as_comment(build(text)),
        )
    } else {
        ActionSpec::prompted(
            id,
            label,
            prompt,
            placeholder,
            ExecutionLocus::ClientLocal,
            build,
        )
    };
    spec.requiring(PermissionRequirement::Comment)
        .as_participant()
}

/// Wrap a built participation action as the comment operation, for a
/// feedback channel's prompted and typed actions.
#[allow(
    clippy::expect_used,
    reason = "ChatOperation is serde-derived from serializable fields"
)]
fn as_comment(encoded: EncodedAction) -> EncodedAction {
    if encoded.type_id != CHAT_ACTION_TYPE {
        return encoded;
    }
    let Ok(action) = serde_json::from_slice::<ChatAction>(&encoded.payload) else {
        return encoded;
    };
    EncodedAction::encode_operation(
        CHAT_OPERATION_TYPE,
        &ChatOperation::Comment {
            action: Box::new(action),
        },
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
        /// Further things it is about (a remark's `references`), each its
        /// own chip.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        references: Vec<tilo_reference::ResourceRef>,
    },
    /// Replace a message's body, archiving the prior text into its edit history
    /// and flagging it edited. `ts` (the edit time) is minted at authoring.
    EditMessage {
        channel: String,
        id: String,
        text: String,
        ts: i64,
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
    /// Mark (or unmark) a channel as a feedback channel, whose top-level
    /// messages are reports (see [`crate::feedback`]). An Editor's decision.
    SetChannelFeedback {
        id: String,
        feedback: bool,
    },
    /// Set a report's status. An Editor's decision: a Commenter votes and
    /// replies but does not triage. `state` carries who set it and when,
    /// minted at authoring time.
    SetReportStatus {
        channel: String,
        msg: String,
        state: crate::feedback::ReportState,
    },
    /// Approve the forge issue drafted on message `msg`. An Editor's
    /// decision, like a report's status: the agent that drafted it files the
    /// issue only once this is in the conversation, which only an Editor can
    /// write.
    ApproveIssue {
        channel: String,
        msg: String,
        approval: crate::feedback::IssueApproval,
    },
    /// Withdraw the record attached to a message from this room. The row
    /// keeps its card; the author's peer removes the room's reference.
    WithdrawAttachment {
        channel: String,
        msg: String,
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
}

/// Durable, browseable history metadata for a Chat mutation.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive action-to-history mapping keeps every ChatAction variant compiler-checked"
)]
pub fn history_context(action: &ChatAction) -> tilo_actions::ActionContext {
    let mut context = tilo_actions::ActionContext::for_action("org.tilo.Chat", "Chat", action);
    let (label, channel, resource_id, kind) = match action {
        ChatAction::SendMessage { channel, id, .. } => {
            ("chat-undo-send-message", channel, id, "chat.message")
        }
        ChatAction::EditMessage { channel, id, .. } => {
            ("chat-undo-edit-message", channel, id, "chat.message")
        }
        ChatAction::DeleteMessage { channel, id } => {
            ("chat-undo-delete-message", channel, id, "chat.message")
        }
        ChatAction::AddReaction { channel, msg, .. } => {
            ("chat-undo-add-reaction", channel, msg, "chat.message")
        }
        ChatAction::RemoveReaction { channel, msg, .. } => {
            ("chat-undo-remove-reaction", channel, msg, "chat.message")
        }
        ChatAction::PinMessage {
            channel,
            msg,
            pinned,
        } => (
            if *pinned {
                "chat-undo-pin-message"
            } else {
                "chat-undo-unpin-message"
            },
            channel,
            msg,
            "chat.message",
        ),
        ChatAction::SetAgentWork { channel, msg, .. } => {
            ("chat-undo-update-agent-work", channel, msg, "chat.message")
        }
        ChatAction::SetReportStatus { channel, msg, .. } => {
            ("chat-undo-set-report-status", channel, msg, "chat.message")
        }
        ChatAction::ApproveIssue { channel, msg, .. } => {
            ("chat-undo-approve-issue", channel, msg, "chat.message")
        }
        ChatAction::WithdrawAttachment { channel, msg } => (
            "chat-undo-withdraw-attachment",
            channel,
            msg,
            "chat.message",
        ),
        ChatAction::SetChannelFeedback { id, feedback } => (
            if *feedback {
                "chat-undo-make-feedback-channel"
            } else {
                "chat-undo-make-ordinary-channel"
            },
            id,
            id,
            "chat.channel",
        ),
        ChatAction::CreateChannel { id, .. } => {
            ("chat-undo-create-channel", id, id, "chat.channel")
        }
        ChatAction::CreateCategory { id, .. } => {
            ("chat-undo-create-category", id, id, "chat.category")
        }
        ChatAction::RenameCategory { id, .. } => {
            ("chat-undo-rename-category", id, id, "chat.category")
        }
        ChatAction::DeleteCategory { id } => ("chat-undo-delete-category", id, id, "chat.category"),
        ChatAction::SetCategoryCollapsed { id, collapsed } => (
            if *collapsed {
                "chat-undo-collapse-category"
            } else {
                "chat-undo-expand-category"
            },
            id,
            id,
            "chat.category",
        ),
        ChatAction::ReorderChannel { id, up } => (
            if *up {
                "chat-undo-move-channel-up"
            } else {
                "chat-undo-move-channel-down"
            },
            id,
            id,
            "chat.channel",
        ),
        ChatAction::RenameChannel { id, .. } => {
            ("chat-undo-rename-channel", id, id, "chat.channel")
        }
        ChatAction::DeleteChannel { id } => ("chat-undo-delete-channel", id, id, "chat.channel"),
        ChatAction::ArchiveChannel { id, archived } => (
            if *archived {
                "chat-undo-archive-channel"
            } else {
                "chat-undo-unarchive-channel"
            },
            id,
            id,
            "chat.channel",
        ),
        ChatAction::MarkRead { channel, .. } => (
            "chat-undo-mark-channel-read",
            channel,
            channel,
            "chat.channel",
        ),
        ChatAction::JoinCall { channel, .. } => {
            ("chat-undo-join-call", channel, channel, "chat.call")
        }
        ChatAction::LeaveCall { channel, .. } => {
            ("chat-undo-leave-call", channel, channel, "chat.call")
        }
        ChatAction::SetCallState { channel, .. } => {
            ("chat-undo-update-call-state", channel, channel, "chat.call")
        }
        ChatAction::SetScreenShare {
            channel, sharing, ..
        } => (
            if *sharing {
                "chat-undo-start-presenting"
            } else {
                "chat-undo-stop-presenting"
            },
            channel,
            channel,
            "chat.call",
        ),
        ChatAction::SetCallTicket { channel, .. }
        | ChatAction::SetCallVideoCaps { channel, .. } => (
            "chat-undo-update-call-connection",
            channel,
            channel,
            "chat.call",
        ),
    };
    // The label a history list shows for the change, in the reader's words.
    context.action_label = crate::i18n::t(label);
    context.resource = Some(tilo_actions::ActionResource {
        id: format!("{kind}:{resource_id}"),
        kind: kind.to_string(),
        label: if kind == "chat.message" {
            crate::i18n::t("chat-undo-resource-message")
        } else {
            channel.clone()
        },
        // Workspace channel names are shared chrome; a message's body is not
        // in this context at all.
        sensitivity: tilo_actions::Sensitivity::Public,
    });
    context
}

impl Action for ChatAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Each helper matches only its own category's variants (a `_ => {}`
        // no-op for the rest), so exactly one of the three does real work per
        // call — split out of this trait method because a helper that isn't
        // itself a trait method can't live in this `impl Action for ..`
        // block (E0407).
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
}

impl ChatAction {
    /// Whether this action names the participant performing it — a post's
    /// author, a reaction's user, a status or approval's "by", a call
    /// member. Such an action is only ever the performer's own word, so its
    /// published form is marked [`ActionSpec::as_participant`].
    pub fn speaks_for_participant(&self) -> bool {
        matches!(
            self,
            ChatAction::SendMessage { .. }
                | ChatAction::AddReaction { .. }
                | ChatAction::RemoveReaction { .. }
                | ChatAction::SetReportStatus { .. }
                | ChatAction::ApproveIssue { .. }
                | ChatAction::MarkRead { .. }
                | ChatAction::SetAgentWork { .. }
                | ChatAction::JoinCall { .. }
                | ChatAction::LeaveCall { .. }
                | ChatAction::SetCallState { .. }
                | ChatAction::SetScreenShare { .. }
                | ChatAction::SetCallTicket { .. }
                | ChatAction::SetCallVideoCaps { .. }
        )
    }

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
                references,
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
                        also: references.clone(),
                        ..Default::default()
                    },
                );
            }
            ChatAction::EditMessage {
                channel,
                id,
                text,
                ts,
            } => {
                ChatDoc::edit_message(doc, channel, id, text, *ts);
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
            ChatAction::SetReportStatus {
                channel,
                msg,
                state,
            } => {
                ChatDoc::set_report_status(doc, channel, msg, state);
            }
            ChatAction::ApproveIssue {
                channel,
                msg,
                approval,
            } => {
                ChatDoc::approve_issue(doc, channel, msg, approval);
            }
            ChatAction::WithdrawAttachment { channel, msg } => {
                ChatDoc::withdraw_attachment(doc, channel, msg);
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
            ChatAction::SetChannelFeedback { id, feedback } => {
                ChatDoc::set_channel_feedback(doc, id, *feedback);
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

#[allow(
    clippy::too_many_arguments,
    reason = "canonical leaf constructor mirrors the replayable SendMessage action payload; callers must supply every authored field before dispatch"
)]
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
        references: Vec::new(),
    }
}

pub fn edit_message_action(channel: &str, id: &str, text: &str, ts: i64) -> ChatAction {
    ChatAction::EditMessage {
        channel: channel.to_string(),
        id: id.to_string(),
        text: text.to_string(),
        ts,
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

pub fn set_channel_feedback_action(id: &str, feedback: bool) -> ChatAction {
    ChatAction::SetChannelFeedback {
        id: id.to_string(),
        feedback,
    }
}

pub fn set_report_status_action(
    channel: &str,
    msg: &str,
    status: crate::feedback::ReportStatus,
    by: &str,
    since: i64,
) -> ChatAction {
    ChatAction::SetReportStatus {
        channel: channel.to_string(),
        msg: msg.to_string(),
        state: crate::feedback::ReportState {
            status,
            by: by.to_string(),
            since,
        },
    }
}

pub fn approve_issue_action(channel: &str, msg: &str, by: &str, since: i64) -> ChatAction {
    ChatAction::ApproveIssue {
        channel: channel.to_string(),
        msg: msg.to_string(),
        approval: crate::feedback::IssueApproval {
            by: by.to_string(),
            since,
        },
    }
}

pub fn withdraw_attachment_action(channel: &str, msg: &str) -> ChatAction {
    ChatAction::WithdrawAttachment {
        channel: channel.to_string(),
        msg: msg.to_string(),
    }
}

/// A vote is the canonical reaction ([`crate::feedback::VOTE_EMOJI`]), so the
/// vote and the reaction chip are one action.
pub fn vote_action(channel: &str, msg: &str, user: &str, vote: bool) -> ChatAction {
    if vote {
        add_reaction_action(channel, msg, crate::feedback::VOTE_EMOJI, user)
    } else {
        remove_reaction_action(channel, msg, crate::feedback::VOTE_EMOJI, user)
    }
}

/// Decode the typed `set-status` arguments into a status. Shared by the
/// resource action and the on-screen status menu.
///
/// # Errors
///
/// Names the argument that is missing or out of range.
pub fn report_status_from_args(
    status: &str,
    generation: Option<i64>,
    duplicate_of: Option<&str>,
) -> Result<crate::feedback::ReportStatus, String> {
    use crate::feedback::ReportStatus;
    Ok(match status {
        "open" => ReportStatus::Open,
        "planned" => ReportStatus::Planned,
        "in_progress" => ReportStatus::InProgress,
        "wont_fix" => ReportStatus::WontFix,
        "fixed" => ReportStatus::Fixed {
            generation: generation
                .and_then(|g| u64::try_from(g).ok())
                .ok_or("a fixed report names the generation that fixes it")?,
        },
        "duplicate" => ReportStatus::Duplicate {
            of: duplicate_of
                .map(str::trim)
                .filter(|of| !of.is_empty())
                .ok_or("a duplicate names the report it duplicates")?
                .trim_start_matches("chat.message:")
                .to_string(),
        },
        other => return Err(format!("{other:?} is not a report status")),
    })
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
    pub feedback: bool,
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
#[derive(Debug, Clone, Default)]
pub struct MessageResource {
    pub id: String,
    pub label: String,
    pub pinned: bool,
    pub parent: Option<String>,
    pub author: String,
    /// What the message is about, as its author selected it.
    pub reference: Option<tilo_reference::ResourceRef>,
    /// Further things it is about.
    pub also: Vec<tilo_reference::ResourceRef>,
    /// Present when the message is a report: top-level in a feedback channel.
    pub report: Option<ReportResource>,
    /// The message carries a drafted issue no Editor has approved yet.
    pub issue_pending: bool,
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
    /// The content hash of shared bytes; `None` for a record.
    pub blob: Option<String>,
    pub record: Option<tilo_reference::RecordAttachment>,
    pub withdrawn: bool,
    /// Whether the local peer posted it, and so owns sharing it into the room.
    pub mine: bool,
}

/// A report's standing, as its resource publishes it.
#[derive(Debug, Clone)]
pub struct ReportResource {
    pub status: crate::feedback::ReportStatus,
    pub votes: usize,
    pub voted_by_me: bool,
}

impl MessageResource {
    /// The resource view of `message` for the peer `me`. `feedback` says the
    /// message's channel is a feedback channel.
    pub fn of(message: &crate::model::Message, label: String, me: &str, feedback: bool) -> Self {
        Self {
            id: message.id.clone(),
            label,
            pinned: message.pinned,
            parent: message.parent.clone(),
            author: message.author.clone(),
            reference: message.reference.clone(),
            also: message.also.clone(),
            issue_pending: message
                .attachment
                .as_ref()
                .is_some_and(|attachment| attachment.issue().is_some())
                && message.approved.is_none(),
            report: (feedback && message.is_top_level()).then(|| ReportResource {
                status: crate::feedback::status_of(message),
                votes: crate::feedback::vote_count(message),
                voted_by_me: crate::feedback::voted_by(message, me),
            }),
            attachment: message
                .attachment
                .as_ref()
                .map(|attachment| AttachmentResource {
                    name: attachment.name().to_owned(),
                    blob: attachment.blob().map(str::to_owned),
                    record: attachment.record().cloned(),
                    withdrawn: message.attachment_withdrawn,
                    mine: message.author == me,
                }),
            reactions: message
                .reactions
                .iter()
                .map(|reaction| ReactionResource {
                    emoji: reaction.emoji.clone(),
                    count: reaction.count(),
                    reacted_by_me: reaction.reacted_by(me),
                })
                .collect(),
        }
    }
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
    /// Messages people wrote in their own comment records (see
    /// [`crate::comments`]), which the conversation document does not hold,
    /// with their reactions and the status Editors set.
    pub comment_messages: Vec<crate::model::Message>,
    /// This participant may comment but not write the conversation (a
    /// Commenter, a Contributor agent). What it posts, replies and reacts is
    /// then its own comment record in every channel, as everyone's is in a
    /// feedback channel.
    pub comment_only: bool,
}

impl ChatResourceCtx {
    /// Whether what this participant posts in `channel` is its own comment
    /// record rather than a row of the conversation document.
    #[must_use]
    pub fn writes_comments(&self, channel: &ChannelResource) -> bool {
        channel.feedback || self.comment_only
    }
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
    let spec = if action.speaks_for_participant() {
        spec.as_participant()
    } else {
        spec
    };
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

/// Build the resource tree Chat currently has open: the active workspace (root),
/// one `chat.channel:<id>` per channel, and — under the one open channel — a
/// `chat.message:<id>` per message. Every operation is the same action the
/// on-screen affordance dispatches.
pub fn provide_resources(ctx: &ChatResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let users = ChatDoc::list_users(doc);
    let mut resource_ctx = ctx.clone();
    resource_ctx.participants = users
        .iter()
        .map(|user| crate::mention::Participant {
            id: user.id.clone(),
            name: user.name.clone(),
            is_agent: user.is_agent,
        })
        .collect();
    let all_channels = ChatDoc::list_channels(doc);
    let mut forest = Vec::new();
    let mut remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT.saturating_sub(1 + EAGER_PEOPLE_LIMIT);
    let mut workspaces = ChatDoc::list_workspaces(doc);
    // Context first: the open room's siblings are more useful than an
    // arbitrary early workspace when only one bounded page is materialised.
    workspaces.sort_by_key(|workspace| workspace.id != ctx.workspace_id);
    for ws in workspaces.into_iter().take(EAGER_WORKSPACE_LIMIT) {
        if remaining == 0 {
            break;
        }
        let mut workspace = workspace_resource(&ws.id, &ws.name);
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
                feedback: channel.feedback,
            };
            workspace =
                workspace.with_child(build_channel_resource(&resource_ctx, doc, &ws.id, &ch, &[]));
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
            workspace = workspace.with_child(build_category_resource(&ws.id, &category));
            remaining = remaining.saturating_sub(1);
        }
        annotate_contracts(&mut workspace);
        forest.push(workspace);
    }

    let mut people = Resource::new(
        "chat.people",
        "chat.people",
        crate::i18n::t("chat-action-people"),
    );
    for user in users.into_iter().take(EAGER_PEOPLE_LIMIT) {
        people = people.with_child(person_resource(&ctx.me, &user));
    }
    forest.push(people);
    forest
}

/// A participant, with what one can do about meeting them: open their card,
/// message them in the room the two of you share, or invite them into this
/// workspace. The last two are the workspace's own requests, so they reach
/// the same direct-room and invite flows every other surface uses.
pub(crate) fn person_resource(me: &str, user: &crate::model::User) -> Resource {
    let mut person = Resource::new(
        format!("chat.person:{}", user.id),
        "chat.person",
        user.name.clone(),
    )
    .with_sensitivity(tilo_actions::Sensitivity::Personal)
    .with_action(view_operation(
        "open",
        &crate::i18n::t("chat-action-open-person"),
        &ChatOperation::OpenPerson {
            user: user.id.clone(),
        },
    ));
    if tilo_reference::DirectRoom::between(me, &user.id).is_some() {
        person = person
            .with_action(workspace_request_spec(
                "message",
                &crate::i18n::t("chat-action-message"),
                &crate::i18n::t("chat-action-open-the-conversation-only-the-two-of-you-are-in"),
                PermissionRequirement::Read,
                WorkspaceRequest::new(
                    tilo_actions::remote::PEOPLE_RESOURCE,
                    tilo_actions::remote::PERSON_MESSAGE_ACTION,
                    InvokeArgument::Text(user.id.clone()),
                ),
            ))
            .with_action(workspace_request_spec(
                "invite",
                &crate::i18n::t("chat-action-invite-to-workspace"),
                &crate::i18n::t("chat-action-invite-this-person-into-the-current-workspace"),
                PermissionRequirement::Invite,
                WorkspaceRequest::new(
                    tilo_actions::remote::PEOPLE_RESOURCE,
                    tilo_actions::remote::PERSON_INVITE_ACTION,
                    InvokeArgument::Text(user.id.clone()),
                ),
            ));
    }
    person
}

fn workspace_resource(id: &str, name: &str) -> Resource {
    Resource::new(
        format!("chat.workspace:{id}"),
        "chat.workspace",
        if name.trim().is_empty() {
            crate::i18n::t("chat-action-workspace")
        } else {
            name.to_string()
        },
    )
    .with_action(view_operation(
        "open",
        &crate::i18n::t("chat-action-open-workspace"),
        &ChatOperation::OpenWorkspace {
            workspace: id.to_string(),
        },
    ))
    .with_action(new_channel_action_spec(id))
    .with_action(immediate_spec(
        "add-feedback-channel",
        &crate::i18n::t("chat-action-add-the-feedback-channel"),
        &create_channel_action(
            id,
            "",
            tilo_reference::FEEDBACK_CHANNEL,
            "feedback",
            ChannelKind::Text,
        ),
    ))
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
        return Some(Resource::new(
            "chat.people",
            "chat.people",
            crate::i18n::t("chat-action-people"),
        ));
    }
    if let Some(workspace_id) = id.strip_prefix("chat.workspace:") {
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == workspace_id)?;
        let mut resource = workspace_resource(&workspace.id, &workspace.name);
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
            feedback: channel.feedback,
        };
        let mut resource = build_channel_resource(ctx, doc, &workspace_id, &model, &[]);
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
        let mut resource = build_category_resource(&workspace_id, &model);
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(user_id) = id.strip_prefix("chat.person:") {
        let user = ChatDoc::list_users(doc)
            .into_iter()
            .find(|user| user.id == user_id)?;
        return Some(person_resource(&ctx.me, &user));
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
            feedback: channel.feedback,
        };
        let mut resource = build_call_resource(ctx, doc, &channel.workspace, &model);
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(message_id) = id.strip_prefix("chat.attachment:") {
        let (channel, message) = find_message(ctx, doc, message_id)?;
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == channel.workspace)?;
        let channel_model = channel_resource(&channel);
        let model = MessageResource::of(&message, String::new(), &ctx.me, channel.feedback);
        let mut resource = build_attachment_resource(
            &workspace.id,
            &channel_model,
            &message.id,
            message.parent.as_ref(),
            model.attachment.as_ref()?,
        );
        annotate_contracts(&mut resource);
        return Some(resource);
    }
    if let Some(address) = id.strip_prefix("chat.reference:") {
        let (message_id, index) = match address.split_once('#') {
            Some((message, index)) => (message, index.parse::<usize>().ok()?),
            None => (address, 0),
        };
        let (channel, message) = find_message(ctx, doc, message_id)?;
        let reference = message.references().nth(index)?;
        let workspace = ChatDoc::list_workspaces(doc)
            .into_iter()
            .find(|workspace| workspace.id == channel.workspace)?;
        let mut resource = build_reference_resource(
            &workspace.id,
            &channel_resource(&channel),
            &(&message.id, index),
            message.parent.as_ref(),
            reference,
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
    let message = find_message(ctx, doc, message_id)?;
    let (channel, message) = message;
    let workspace = ChatDoc::list_workspaces(doc)
        .into_iter()
        .find(|workspace| workspace.id == channel.workspace)?;
    let channel_model = channel_resource(&channel);
    let users = ChatDoc::list_users(doc);
    let label = message_label(
        &message.author,
        &message.text,
        message.attachment.as_ref(),
        &users,
    );
    let message_model = MessageResource::of(&message, label, &ctx.me, channel.feedback);
    let mut resource = build_message_resource(ctx, &workspace.id, &channel_model, &message_model);
    annotate_contracts(&mut resource);
    Some(resource)
}

/// Resolve an immutable message row and its owning channel once. Resource
/// resolution must read the document, never a currently rendered projection.
fn find_message(
    ctx: &ChatResourceCtx,
    doc: &AutoCommit,
    message_id: &str,
) -> Option<(crate::model::Channel, crate::model::Message)> {
    let channels = ChatDoc::list_channels(doc);
    let mut found = channels.iter().find_map(|channel| {
        ChatDoc::list_messages(doc, &channel.id)
            .into_iter()
            .find(|message| message.id == message_id)
            .map(|message| (channel.clone(), message))
    });
    if found.is_none() {
        found = ctx
            .comment_messages
            .iter()
            .find(|message| message.id == message_id)
            .and_then(|message| {
                let channel = channels.iter().find(|c| c.id == message.channel)?;
                Some((channel.clone(), message.clone()))
            });
    }
    // The conversation's messages in a feedback channel carry reactions
    // from people's own records too.
    if let Some((channel, message)) = &mut found
        && channel.feedback
        && let Some(merged) = ctx.comment_messages.iter().find(|m| m.id == message.id)
    {
        message.reactions.clone_from(&merged.reactions);
    }
    found
}

fn channel_resource(channel: &crate::model::Channel) -> ChannelResource {
    ChannelResource {
        id: channel.id.clone(),
        name: channel.name.clone(),
        kind: channel.kind,
        archived: channel.archived,
        feedback: channel.feedback,
    }
}

/// Workspace-level: create a new text channel in the uncategorised group. The
/// fresh id is minted once, here, at authoring time (like the on-screen "+").
fn new_channel_action_spec(workspace_id: &str) -> ActionSpec {
    ActionSpec::prompted(
        "new-channel",
        crate::i18n::t("chat-action-new-channel"),
        crate::i18n::t("chat-action-name"),
        crate::i18n::t("chat-action-channel-name"),
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
        crate::i18n::t("chat-action-new-category"),
        crate::i18n::t("chat-action-name"),
        crate::i18n::t("chat-action-category-name"),
        ExecutionLocus::ClientLocal,
        {
            let workspace = workspace_id.to_string();
            move |name: &str| {
                let name = name.trim();
                let unnamed;
                let name = if name.is_empty() {
                    unnamed = crate::i18n::t("chat-action-new-category");
                    unnamed.as_str()
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
        &crate::i18n::t("chat-action-open-category"),
        &ChatOperation::OpenCategory {
            workspace: workspace_id.to_string(),
            category: category.id.clone(),
        },
    ))
    .with_action(ActionSpec::prompted(
        "rename",
        crate::i18n::t("chat-action-rename-category"),
        crate::i18n::t("chat-action-name"),
        crate::i18n::t("chat-action-category-name"),
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
        &crate::i18n::t(if category.collapsed {
            "chat-action-expand-category"
        } else {
            "chat-action-collapse-category"
        }),
        &set_category_collapsed_action(&category.id, !category.collapsed),
    ));
    resource.with_action(immediate_spec(
        "delete",
        &crate::i18n::t("chat-action-delete-category"),
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
    .with_action(view_operation(
        "open",
        &crate::i18n::t("chat-action-open-conversation"),
        &ChatOperation::OpenChannel {
            workspace: workspace_id.to_string(),
            channel: ch.id.clone(),
        },
    ));

    // Post a message (Send, from the palette). Id + timestamp minted in the
    // closure at authoring time so the payload is self-contained.
    channel = channel.with_action(
        participation_prompt(
            "post",
            &crate::i18n::t("chat-action-post-message"),
            &crate::i18n::t("chat-action-message"),
            &crate::i18n::t("chat-action-type-a-message"),
            ctx.writes_comments(ch),
            {
                let cid = ch.id.clone();
                let me = ctx.me.clone();
                let room = ctx.participants.clone();
                move |text: &str| {
                    let id = format!("m_{}", uuid::Uuid::new_v4().simple());
                    let mentions = crate::mention::resolve(text, &room);
                    encode(&send_message_action(
                        &cid,
                        &id,
                        &me,
                        unix_now_i64(),
                        text,
                        None,
                        None,
                        mentions,
                    ))
                }
            },
        )
        // Posting is a comment on the room: a Commenter may post and react.
        .requiring(PermissionRequirement::Comment),
    );

    // An agent and the picker use the same local import lifecycle. The path
    // stays local to the machine that can read it; only the resulting bytes
    // and canonical SendMessage action are collaborative.
    channel = channel.with_action(ActionSpec::prompted_operation(
        "attach",
        crate::i18n::t("chat-action-attach-file"),
        crate::i18n::t("chat-action-file-path"),
        "/var/workspace/report.pdf",
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

    // A remark composed elsewhere — something said about a resource, with the
    // recording of what happened — posted as an ordinary message here.
    channel = channel.with_action(remark_spec(&ch.id, &ctx.me, ctx.writes_comments(ch)));

    // Rename — a prompted action carrying the new name.
    channel = channel.with_action(ActionSpec::prompted(
        "rename",
        crate::i18n::t("chat-action-rename-channel"),
        crate::i18n::t("chat-action-name"),
        crate::i18n::t("chat-action-channel-name"),
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
            &crate::i18n::t("chat-action-unarchive-channel"),
            &archive_channel_action(&ch.id, false),
        ));
    } else {
        channel = channel.with_action(immediate_spec(
            "archive",
            &crate::i18n::t("chat-action-archive-channel"),
            &archive_channel_action(&ch.id, true),
        ));
    }

    // Mark the channel read: the watermark target is the current top-level
    // count, read from the document at authoring time and baked in.
    let count = ChatDoc::top_level_count(doc, &ch.id) as i64;
    channel = channel.with_action(immediate_spec(
        "mark-read",
        &crate::i18n::t("chat-action-mark-as-read"),
        &mark_read_action(&ch.id, &ctx.me, count),
    ));

    channel = channel.with_action(if ch.feedback {
        immediate_spec(
            "unmark-feedback",
            &crate::i18n::t("chat-action-make-an-ordinary-channel"),
            &set_channel_feedback_action(&ch.id, false),
        )
    } else {
        immediate_spec(
            "mark-feedback",
            &crate::i18n::t("chat-action-make-a-feedback-channel"),
            &set_channel_feedback_action(&ch.id, true),
        )
    });
    if ch.feedback {
        channel = channel.with_action(report_view_spec(&ch.id));
    }

    // Delete the whole channel.
    channel = channel.with_action(immediate_spec(
        "delete",
        &crate::i18n::t("chat-action-delete-channel"),
        &delete_channel_action(&ch.id),
    ));

    // A voice channel owns one durable call resource. The call's capture and
    // transport remain local effects, but every shared membership/control fact
    // lives beneath this same canonical address for history, collaboration, and
    // palette invocation.
    if ch.kind == ChannelKind::Voice {
        channel = channel.with_child(build_call_resource(ctx, doc, workspace_id, ch));
    }

    for m in messages {
        let message = build_message_resource(ctx, workspace_id, ch, m);
        channel = channel.with_child(message);
    }

    channel
}

/// The shared-state half of a voice call. The ticket and screen-capture source
/// deliberately have no prompted action: both are opaque local transport
/// outputs, not values a caller can safely invent. Join/leave and each visible
/// call preference are ordinary replayable document actions.
fn build_call_resource(
    ctx: &ChatResourceCtx,
    doc: &AutoCommit,
    workspace_id: &str,
    ch: &ChannelResource,
) -> Resource {
    let member = ChatDoc::list_voice_members(doc, &ch.id)
        .into_iter()
        .find(|member| member.user == ctx.me);
    let mut call = Resource::new(
        format!("chat.call:{}", ch.id),
        "chat.call",
        format!("Call: {}", channel_label(ch)),
    )
    .with_action(view_operation(
        "open",
        &crate::i18n::t("chat-action-open-call-conversation"),
        &ChatOperation::OpenChannel {
            workspace: workspace_id.to_string(),
            channel: ch.id.clone(),
        },
    ));
    if let Some(member) = member {
        call = call.with_action(immediate_spec(
            "leave",
            &crate::i18n::t("chat-action-leave-call"),
            &leave_call_action(&ch.id, &ctx.me),
        ));
        call = call.with_action(immediate_spec(
            if member.muted { "unmute" } else { "mute" },
            &crate::i18n::t(if member.muted {
                "chat-action-unmute"
            } else {
                "chat-action-mute"
            }),
            &call_state_action(
                &ch.id,
                &ctx.me,
                !member.muted,
                member.deafened,
                member.audio_only,
            ),
        ));
        call = call.with_action(immediate_spec(
            if member.deafened {
                "undeafen"
            } else {
                "deafen"
            },
            &crate::i18n::t(if member.deafened {
                "chat-action-undeafen"
            } else {
                "chat-action-deafen"
            }),
            &call_state_action(
                &ch.id,
                &ctx.me,
                member.muted,
                !member.deafened,
                member.audio_only,
            ),
        ));
        call = call.with_action(immediate_spec(
            if member.audio_only {
                "enable-camera"
            } else {
                "audio-only"
            },
            &crate::i18n::t(if member.audio_only {
                "chat-action-enable-camera"
            } else {
                "chat-action-audio-only"
            }),
            &call_state_action(
                &ch.id,
                &ctx.me,
                member.muted,
                member.deafened,
                !member.audio_only,
            ),
        ));
    } else {
        call = call.with_action(immediate_spec(
            "join",
            &crate::i18n::t("chat-action-join-call"),
            &join_call_action(&ch.id, &ctx.me),
        ));
    }
    call
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
    .with_sensitivity(tilo_actions::Sensitivity::Personal)
    .with_action(view_operation(
        "open",
        &crate::i18n::t("chat-action-reveal-message"),
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
        crate::i18n::t("chat-action-edit-message"),
        crate::i18n::t("chat-action-message"),
        crate::i18n::t("chat-action-new-text"),
        ExecutionLocus::ClientLocal,
        {
            let cid = ch.id.clone();
            let mid = m.id.clone();
            move |text: &str| encode(&edit_message_action(&cid, &mid, text, unix_now_i64()))
        },
    ));

    // React — prompted for an emoji; adds the local peer's reaction.
    message = message.with_action(
        participation_prompt(
            "react",
            &crate::i18n::t("chat-action-add-reaction"),
            &crate::i18n::t("chat-action-emoji"),
            "👍",
            ctx.writes_comments(ch),
            {
                let cid = ch.id.clone();
                let mid = m.id.clone();
                let me = ctx.me.clone();
                move |emoji: &str| encode(&add_reaction_action(&cid, &mid, emoji.trim(), &me))
            },
        )
        .requiring(PermissionRequirement::Comment),
    );

    message = with_participation_actions(message, ctx, ch, m);

    // Pin / unpin toggles the stored flag.
    if m.pinned {
        message = message.with_action(immediate_spec(
            "unpin",
            &crate::i18n::t("chat-action-unpin-message"),
            &pin_message_action(&ch.id, &m.id, false),
        ));
    } else {
        message = message.with_action(immediate_spec(
            "pin",
            &crate::i18n::t("chat-action-pin-message"),
            &pin_message_action(&ch.id, &m.id, true),
        ));
    }

    // Delete the message.
    message = message.with_action(immediate_spec(
        "delete",
        &crate::i18n::t("chat-action-delete-message"),
        &delete_message_action(&ch.id, &m.id),
    ));

    if let Some(attachment) = &m.attachment {
        message = message.with_child(build_attachment_resource(
            workspace_id,
            ch,
            &m.id,
            m.parent.as_ref(),
            attachment,
        ));
    }
    for (index, reference) in m.reference.iter().chain(m.also.iter()).enumerate() {
        message = message.with_child(build_reference_resource(
            workspace_id,
            ch,
            &(&m.id, index),
            m.parent.as_ref(),
            reference,
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

/// What a Commenter does with a message — reply, meet its author, and on a
/// report vote — plus an Editor's status.
fn with_participation_actions(
    mut message: Resource,
    ctx: &ChatResourceCtx,
    ch: &ChannelResource,
    m: &MessageResource,
) -> Resource {
    // Reply in the message's thread. A reply to a reply lands in the same
    // thread, which is the only depth a thread has.
    message = message.with_action(
        participation_prompt(
            "reply",
            &crate::i18n::t("chat-action-reply-in-thread"),
            &crate::i18n::t("chat-action-reply"),
            &crate::i18n::t("chat-action-type-a-reply"),
            ctx.writes_comments(ch),
            {
                let cid = ch.id.clone();
                let parent = m.parent.clone().unwrap_or_else(|| m.id.clone());
                let me = ctx.me.clone();
                let room = ctx.participants.clone();
                move |text: &str| {
                    let id = format!("m_{}", uuid::Uuid::new_v4().simple());
                    let mentions = crate::mention::resolve(text, &room);
                    encode(&send_message_action(
                        &cid,
                        &id,
                        &me,
                        unix_now_i64(),
                        text,
                        Some(parent.clone()),
                        None,
                        mentions,
                    ))
                }
            },
        )
        .requiring(PermissionRequirement::Comment),
    );

    // Who wrote it: the person card, with its own Message and Invite.
    message = message.with_action(view_operation(
        "open-author",
        &crate::i18n::t("chat-action-show-author"),
        &ChatOperation::OpenPerson {
            user: m.author.clone(),
        },
    ));

    if let Some(report) = &m.report {
        message = message.with_action(participation_spec(
            if report.voted_by_me { "unvote" } else { "vote" },
            &crate::i18n::t(if report.voted_by_me {
                "chat-action-remove-my-vote"
            } else {
                "chat-action-vote-for-this-report"
            }),
            ch.feedback,
            &vote_action(&ch.id, &m.id, &ctx.me, !report.voted_by_me),
        ));
        message = message.with_action(set_status_spec(&ch.id, &m.id, &ctx.me));
    }
    if m.issue_pending {
        message = message.with_action(approve_issue_spec(&ch.id, &m.id, &ctx.me));
    }
    message
}

/// Approve the issue a message drafts. Write, so an Editor's, like a
/// report's status: nothing is filed on a Commenter's or an agent's say.
fn approve_issue_spec(channel: &str, msg: &str, me: &str) -> ActionSpec {
    immediate_spec(
        "approve-issue",
        &crate::i18n::t("chat-action-approve-issue"),
        &approve_issue_action(channel, msg, me, unix_now_i64()),
    )
    .requiring(PermissionRequirement::Write)
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
    let mut resource = Resource::new(
        format!("chat.attachment:{message_id}"),
        "chat.attachment",
        attachment.name.clone(),
    )
    .with_sensitivity(tilo_actions::Sensitivity::Personal)
    .with_action(view_operation(
        "open-message",
        &crate::i18n::t("chat-action-reveal-shared-message"),
        &ChatOperation::RevealMessage {
            workspace: workspace_id.to_owned(),
            channel: ch.id.clone(),
            message: message_id.to_owned(),
            parent: parent.cloned(),
        },
    ));
    let Some(record) = &attachment.record else {
        return resource;
    };
    if attachment.withdrawn {
        // Withdrawn from this room: the author's peer takes the room's
        // reference back. A copy a member already synced stays theirs.
        if attachment.mine {
            resource = resource.with_action(record_index_spec(record, false));
        }
        return resource;
    }
    if record.is_session() {
        resource = resource.with_action(workspace_request_spec(
            "replay",
            &crate::i18n::t("chat-action-replay-recording"),
            &crate::i18n::t("chat-action-replay-this-recorded-session-in-a-sandboxed-replay-room"),
            PermissionRequirement::Read,
            WorkspaceRequest::new(
                tilo_actions::remote::SESSION_REPLAY_RESOURCE,
                tilo_actions::remote::SESSION_REPLAY_ACTION,
                InvokeArgument::Text(record.record.clone()),
            ),
        ));
    }
    if attachment.mine {
        resource = resource
            .with_action(record_index_spec(record, true))
            .with_action(immediate_spec(
                "withdraw",
                &crate::i18n::t("chat-action-withdraw-from-this-room"),
                &withdraw_attachment_action(&ch.id, message_id),
            ));
    }
    resource
}

/// Share (or take back) an attached record through the room's own index.
/// Offered only to programmatic callers: the author's Chat issues it when
/// the message is posted or withdrawn, and a person never has to.
fn record_index_spec(record: &tilo_reference::RecordAttachment, share: bool) -> ActionSpec {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "id".into(),
        serde_json::Value::String(record.record.clone()),
    );
    arguments.insert(
        "kind".into(),
        serde_json::Value::String(record.kind.clone()),
    );
    let mut spec = workspace_request_spec(
        if share { "share" } else { "unshare" },
        &crate::i18n::t(if share {
            "chat-action-share-record-into-this-room"
        } else {
            "chat-action-stop-sharing-record-with-this-room"
        }),
        &crate::i18n::t(if share {
            "chat-action-add-a-reference-from-this-room-to-the-attached-record-no-bytes-are-copied"
        } else {
            "chat-action-remove-this-room-s-reference-members-who-already-synced-it-keep-their-copy"
        }),
        PermissionRequirement::Comment,
        WorkspaceRequest::new(
            tilo_actions::remote::ROOM_RECORDS_RESOURCE,
            if share {
                tilo_actions::remote::ROOM_RECORD_REFERENCE_ACTION
            } else {
                tilo_actions::remote::ROOM_RECORD_UNREFERENCE_ACTION
            },
            InvokeArgument::Json(arguments),
        ),
    );
    spec.contract
        .tags
        .push(tilo_actions::PROGRAMMATIC_TAG.to_string());
    spec
}

/// The address a message carries, as a child the reader can open. Opening
/// asks the workspace to open the resource in the app that published it.
/// The resource id of a message's `index`th reference: the primary one is
/// `chat.reference:<message>`, a further one `chat.reference:<message>#<n>`.
pub fn reference_resource_id(message_id: &str, index: usize) -> String {
    if index == 0 {
        format!("chat.reference:{message_id}")
    } else {
        format!("chat.reference:{message_id}#{index}")
    }
}

fn build_reference_resource(
    workspace_id: &str,
    ch: &ChannelResource,
    &(message_id, index): &(&String, usize),
    parent: Option<&String>,
    reference: &tilo_reference::ResourceRef,
) -> Resource {
    let message_id = message_id.as_str();
    Resource::new(
        reference_resource_id(message_id, index),
        "chat.reference",
        reference.label.clone(),
    )
    .with_action(workspace_request_spec(
        "open",
        &crate::i18n::t("chat-action-open-referenced-item"),
        &crate::i18n::t("chat-action-open-what-this-message-is-about-in-the-app-it-belongs-to"),
        PermissionRequirement::Read,
        WorkspaceRequest::new(
            tilo_actions::remote::RESOURCES_RESOURCE,
            tilo_actions::remote::RESOURCE_OPEN_ACTION,
            InvokeArgument::Text(reference.id.clone()),
        ),
    ))
    .with_action(view_operation(
        "open-message",
        &crate::i18n::t("chat-action-reveal-message"),
        &ChatOperation::RevealMessage {
            workspace: workspace_id.to_owned(),
            channel: ch.id.clone(),
            message: message_id.to_owned(),
            parent: parent.cloned(),
        },
    ))
}

/// Post a remark another app composed: its text, the address of what it is
/// about, and optionally a record attached by reference. The fields a
/// [`tilo_reference::Remark`] carries, as typed arguments, so the shell's
/// composer, an agent and tilo-control post one the same way.
fn remark_spec(channel: &str, me: &str, feedback: bool) -> ActionSpec {
    let channel = channel.to_string();
    let me = me.to_string();
    let params = vec![
        ActionParam::required("text", ParamType::Text, "What is being said"),
        ActionParam::required(
            "reference",
            ParamType::Object,
            "What it is about: a tilo-reference ResourceRef {kind, id, label, span}",
        ),
        ActionParam::optional(
            "attachment",
            ParamType::Object,
            "A record shown with it: a tilo-reference RecordAttachment",
        ),
        ActionParam::optional(
            "mentions",
            ParamType::Array,
            "Who the text pings: tilo-reference MentionRefs",
        ),
        ActionParam::optional(
            "references",
            ParamType::Array,
            "Further things it is about, when several were picked: ResourceRefs",
        ),
        ActionParam::optional(
            "message",
            ParamType::Text,
            "The id to post the message under, when the sender already named \
             records after it (m_ and up to 62 of A-Z a-z 0-9 _ -)",
        ),
        ActionParam::optional(
            "parent",
            ParamType::Text,
            "The message whose thread this remark replies in",
        ),
        ActionParam::optional(
            "issue",
            ParamType::Object,
            "A forge issue drafted for a report, for an Editor to approve: \
             an IssueDraft {report, title, body}",
        ),
    ];
    let build = move |args: &tilo_actions::ActionArgs| match remark_from_args(args) {
        Ok((text, reference, also, attachment, mentions)) => encode(&ChatAction::SendMessage {
            channel: channel.clone(),
            id: args
                .text("message")
                .filter(|id| tilo_reference::is_chat_message_id(id))
                .map_or_else(
                    || format!("m_{}", uuid::Uuid::new_v4().simple()),
                    ToString::to_string,
                ),
            author: me.clone(),
            ts: unix_now_i64(),
            text,
            parent: args
                .text("parent")
                .filter(|id| tilo_reference::is_chat_message_id(id))
                .map(ToString::to_string),
            attachment,
            mentions,
            reference: Some(reference),
            references: also,
        }),
        Err(reason) => EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: reason.into_bytes(),
        },
    };
    let id = "remark";
    let label = crate::i18n::t("chat-action-post-a-remark");
    let placeholder = crate::i18n::t("chat-action-the-times-contradict-the-prose");
    let spec = if feedback {
        ActionSpec::typed_operation(
            id,
            label,
            placeholder,
            OperationKind::Effect,
            CHAT_OPERATION_TYPE,
            params,
            move |args| as_comment(build(args)),
        )
    } else {
        ActionSpec::typed(
            id,
            label,
            placeholder,
            ExecutionLocus::ClientLocal,
            params,
            build,
        )
    };
    spec.requiring(PermissionRequirement::Comment)
        .as_participant()
}

type RemarkParts = (
    String,
    tilo_reference::ResourceRef,
    Vec<tilo_reference::ResourceRef>,
    Option<Attachment>,
    Vec<crate::mention::Mention>,
);

fn remark_from_args(args: &tilo_actions::ActionArgs) -> Result<RemarkParts, String> {
    let json = args.to_json();
    let text = args.text("text").unwrap_or_default().to_string();
    let reference: tilo_reference::ResourceRef = json
        .get("reference")
        .cloned()
        .ok_or("a remark says what it is about")
        .and_then(|value| {
            serde_json::from_value(value).map_err(|_| "the reference is not a ResourceRef")
        })?;
    let also = match json.get("references") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(value) => serde_json::from_value::<Vec<tilo_reference::ResourceRef>>(value.clone())
            .map_err(|_| "the references are not ResourceRefs".to_string())?,
    };
    let record = match json.get("attachment") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<tilo_reference::RecordAttachment>(value.clone())
                .map_err(|_| "the attachment is not a RecordAttachment".to_string())?,
        ),
    };
    let issue = match json.get("issue") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<crate::feedback::IssueDraft>(value.clone())
                .ok()
                .filter(crate::feedback::IssueDraft::is_valid)
                .ok_or("the issue is not an IssueDraft")?,
        ),
    };
    let attachment = match (record, issue) {
        (Some(_), Some(_)) => return Err("a remark carries one attachment".into()),
        (Some(record), None) => Some(Attachment::Record(Box::new(record))),
        (None, Some(issue)) => Some(Attachment::Issue(Box::new(issue))),
        (None, None) => None,
    };
    let mentions = match json.get("mentions") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(value) => serde_json::from_value::<Vec<tilo_reference::MentionRef>>(value.clone())
            .map_err(|_| "the mentions are not MentionRefs".to_string())?
            .into_iter()
            .map(|m| crate::mention::Mention {
                user: m.user,
                start: m.start,
                len: m.len,
                is_agent: m.is_agent,
            })
            .collect(),
    };
    Ok((text, reference, also, attachment, mentions))
}

/// The status keys `set-status` accepts, in menu order.
pub const REPORT_STATUS_KEYS: [&str; 6] = [
    "open",
    "planned",
    "in_progress",
    "fixed",
    "wont_fix",
    "duplicate",
];

/// Set a report's status. Write, so an Editor's: a Commenter's catalogue
/// never offers it and the broker refuses it from one.
fn set_status_spec(channel: &str, msg: &str, me: &str) -> ActionSpec {
    let channel = channel.to_string();
    let msg = msg.to_string();
    let me = me.to_string();
    ActionSpec::typed(
        "set-status",
        crate::i18n::t("chat-action-set-report-status"),
        "open",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("status", ParamType::Text, "The report's new status")
                .with_choices(REPORT_STATUS_KEYS),
            ActionParam::optional(
                "generation",
                ParamType::Integer,
                "For `fixed`: the image generation that contains the fix",
            ),
            ActionParam::optional(
                "duplicate_of",
                ParamType::Resource,
                "For `duplicate`: the report (`chat.message:<id>`) this one repeats",
            ),
        ],
        move |args| match report_status_from_args(
            args.text("status").unwrap_or_default(),
            args.integer("generation"),
            args.text("duplicate_of"),
        ) {
            Ok(status) => encode(&set_report_status_action(
                &channel,
                &msg,
                status,
                &me,
                unix_now_i64(),
            )),
            Err(reason) => EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: reason.into_bytes(),
            },
        },
    )
    .requiring(PermissionRequirement::Write)
    .as_participant()
}

/// How this peer lists a feedback channel's reports.
#[allow(
    clippy::expect_used,
    reason = "ChatOperation is serde-derived from serializable fields; the builder returns an EncodedAction directly"
)]
fn report_view_spec(channel: &str) -> ActionSpec {
    let channel = channel.to_string();
    ActionSpec::typed_operation(
        "report-view",
        crate::i18n::t("chat-action-sort-and-filter-reports"),
        "votes",
        OperationKind::View,
        CHAT_OPERATION_TYPE,
        vec![
            ActionParam::required("sort", ParamType::Text, "Most votes first, or newest first")
                .with_choices(["votes", "new"]),
            ActionParam::optional(
                "filter",
                ParamType::Text,
                "Which reports to show; active by default",
            )
            .with_choices(["active", "closed", "all"]),
        ],
        move |args| {
            use crate::feedback::{ReportFilter, ReportSort, ReportView};
            let view = ReportView {
                sort: if args.text("sort") == Some("new") {
                    ReportSort::New
                } else {
                    ReportSort::Votes
                },
                filter: match args.text("filter") {
                    Some("closed") => ReportFilter::Closed,
                    Some("all") => ReportFilter::All,
                    _ => ReportFilter::Active,
                },
            };
            EncodedAction::encode_operation(
                CHAT_OPERATION_TYPE,
                &ChatOperation::SetReportView {
                    channel: channel.clone(),
                    view,
                },
            )
            .expect("ChatOperation is always serializable")
        },
    )
    .requiring(PermissionRequirement::Read)
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
        &crate::i18n::t("chat-action-reveal-message"),
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
    resource = resource.with_action(
        immediate_spec(
            if reaction.reacted_by_me {
                "remove"
            } else {
                "add"
            },
            &crate::i18n::t(if reaction.reacted_by_me {
                "chat-action-remove-my-reaction"
            } else {
                "chat-action-add-my-reaction"
            }),
            &action,
        )
        .requiring(PermissionRequirement::Comment),
    );
    resource
}

fn message_label(
    author_id: &str,
    text: &str,
    attachment: Option<&crate::model::Attachment>,
    users: &[crate::model::User],
) -> String {
    let author = users
        .iter()
        .find(|user| user.id == author_id)
        .map_or(author_id, |user| user.name.as_str());
    let unnamed;
    let body = if text.trim().is_empty() {
        if let Some(attachment) = attachment {
            attachment.name()
        } else {
            unnamed = crate::i18n::t("chat-action-attachment");
            unnamed.as_str()
        }
    } else {
        text.trim()
    };
    let snippet = body.chars().take(72).collect::<String>();
    format!("{author}: {snippet}")
}

impl ResourceProvider for crate::app::ChatApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.chat_resource_ctx(), doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
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

    fn ctx_from(doc: &AutoCommit) -> ChatResourceCtx {
        let channels = ChatDoc::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == "ws_acme")
            .map(|c| ChannelResource {
                id: c.id,
                name: c.name,
                kind: c.kind,
                archived: c.archived,
                feedback: c.feedback,
            })
            .collect();
        let messages = ChatDoc::list_messages(doc, "c_crit")
            .into_iter()
            .map(|m| MessageResource::of(&m, m.text.chars().take(24).collect(), model::ME, false))
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
            comment_messages: Vec::new(),
            comment_only: false,
        }
    }

    /// Every label the palette shows is the catalogue's words, never a key
    /// left unresolved.
    #[test]
    fn palette_labels_come_from_the_catalogue() {
        fn walk(resource: &Resource, labels: &mut Vec<String>) {
            labels.push(resource.label.clone());
            for action in &resource.actions {
                labels.push(action.label.clone());
            }
            for child in &resource.children {
                walk(child, labels);
            }
        }
        let doc = seeded();
        let mut labels = Vec::new();
        for resource in provide_resources(&ctx_from(&doc), &doc) {
            walk(&resource, &mut labels);
        }
        assert!(labels.iter().any(|label| label == "Open conversation"));
        assert!(
            labels.iter().all(|label| !label.starts_with("chat-")),
            "{:?}",
            labels
                .iter()
                .filter(|label| label.starts_with("chat-"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the resource covenant is intentionally asserted as one end-to-end inventory"
    )]
    fn resources_publish_a_bounded_snapshot_with_lazy_canonical_children() {
        // Phase: ctrl_k_publishes_workspace_channel_and_message_tree.
        // Ctrl+K keeps the workspace/channel snapshot small while message actions
        // remain available through stable lazy resolution.
        {
            let mut doc = seeded();
            let resources = provide_resources(&ctx_from(&doc), &doc);
            let ws = resources
                .iter()
                .find(|resource| resource.id.as_str() == "chat.workspace:ws_acme")
                .expect("Acme workspace root");
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
            let msg_model = MessageResource::of(&source, source.text.clone(), model::ME, false);
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
            let mut call_doc = AutoCommit::load(&doc.save()).unwrap();
            let join = ui::testing::action_contract::spec_encoding(join, None);
            let joined = registry().decode(&join).unwrap();
            let mut transport = LocalTransport::new();
            tilo_actions::dispatch(&mut call_doc, joined.as_ref(), join, &mut transport).unwrap();
            let joined_call = resolve_resource(&ctx, &call_doc, "chat.call:c_huddle").unwrap();
            let joined_actions: Vec<_> = joined_call
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for want in ["leave", "mute", "deafen", "audio-only"] {
                assert!(
                    joined_actions.contains(&want),
                    "call missing {want}: {joined_actions:?}"
                );
            }

            // Every exported action is reachable — flatten the forest (parents before
            // children) and confirm the message + workspace actions are present.
            let flat = tilo_actions::flatten(&resources);
            let flat_labels: Vec<&str> = flat.iter().map(|f| f.action.label.as_str()).collect();
            assert!(flat_labels.contains(&"New channel"), "{flat_labels:?}");
            assert!(!flat_labels.contains(&"Delete message"), "{flat_labels:?}");
        }
        // Phase: attachment_and_reaction_resources_resolve_with_canonical_actions.
        // Attachments and reaction aggregates are durable parts of a message, so
        // both must have a stable lazy address rather than being discoverable only
        // by scanning the painted timeline.
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
                    label: format!("Message {id}"),
                    pinned: false,
                    ..MessageResource::default()
                })
                .collect();
            let resources = provide_resources(&ctx, &doc);
            assert!(
                resources
                    .iter()
                    .flat_map(|workspace| &workspace.children)
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
                resources
                    .iter()
                    .flat_map(|resource| &resource.children)
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
            let channel = resources[0]
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
                &MessageResource::of(&source, source.text.clone(), model::ME, false),
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
                    .filter(|action| action.id != "open" && action.id != "open-author")
                    .all(|action| action.kind == OperationKind::Document)
            );
            let delete = channel.actions.iter().find(|a| a.id == "delete").unwrap();
            assert_eq!(delete.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                delete.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn message_actions_mutate_the_document_exactly_once() {
        // Phase: send_message_action_appends_the_row.
        // Sending a message through the action appends a row carrying the payload's
        // id, author and text.
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
        // An agent's progress on a summons, through the same action/doc path a
        // real dispatch takes: posted as an empty reply, taken, then finished
        // with words — and a body that already has words is never overwritten.
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
        // Add then remove a reaction through the action path (both mutating), and
        // the reaction comes and goes.
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
        // Idempotency is a doc-level guarantee: a duplicate add does not
        // double-count (this is what makes a duplicate action safe to apply twice).
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
        // Pin then delete a message through their actions.
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
        // Channel create / rename / archive / delete round-trip through actions.
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
            let category = resources[0]
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
        palette: ["New channel", "Rename channel", "Pin message", "Join call"],
        replay: [
            send_message => |_| encode(&send_message_action(
                "c_general",
                "m_conv",
                "u_a71c",
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

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use crate::doc::ChatDoc;
    use crate::feedback::{ReportStatus, VOTE_EMOJI, status_of, vote_count};
    use crate::model::{self, Attachment};
    use tilo_actions::LocalTransport;
    use tilo_actions::remote::{InvokeArgument, WorkspaceRequest};
    use tilo_reference::{
        Destination, DirectRoom, RecordAttachment, Redactions, Remark, ResourceRef, SessionLayer,
        SessionSummary,
    };

    const TS: i64 = 1_782_000_000;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn dispatch_encoded(doc: &mut AutoCommit, encoded: EncodedAction) {
        let decoded = registry().decode(&encoded).expect("a chat action decodes");
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    fn dispatch(doc: &mut AutoCommit, action: &ChatAction) {
        dispatch_encoded(doc, encode(action));
    }

    /// A feedback channel's participation is a comment operation. Without a
    /// collaboration identity Chat performs the action it carries on the
    /// conversation document, which is what this does.
    fn perform_comment(doc: &mut AutoCommit, encoded: &EncodedAction) {
        match decode_operation(&encoded.type_id, &encoded.payload) {
            Ok(Some(ChatOperation::Comment { action })) => dispatch(doc, &action),
            other => panic!("not a comment operation: {other:?}"),
        }
    }

    fn built(
        spec: &ActionSpec,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> EncodedAction {
        spec.build_json(args).expect("the declared arguments bind")
    }

    fn request(spec: &ActionSpec) -> WorkspaceRequest {
        let encoded = built(spec, &serde_json::Map::new());
        assert_eq!(
            encoded.type_id,
            tilo_actions::remote::WORKSPACE_REQUEST_TYPE
        );
        serde_json::from_slice(&encoded.payload).expect("a workspace request")
    }

    fn recording() -> RecordAttachment {
        RecordAttachment::session(
            "uuid:9f2c-send",
            "Send does nothing",
            "room-reporter",
            SessionSummary {
                apps: vec!["so.tilo.mail".into()],
                duration_ms: 83_000,
                steps: 7,
                layer: SessionLayer::Logic,
                redactions: Redactions {
                    text: 3,
                    regions: 0,
                },
                generation: Some(118),
            },
        )
    }

    /// A seeded document with an empty feedback channel in it.
    fn feedback_doc() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        dispatch(
            &mut doc,
            &create_channel_action("ws_tilo", "", "c_fb", "feedback", ChannelKind::Text),
        );
        dispatch(&mut doc, &set_channel_feedback_action("c_fb", true));
        doc
    }

    fn post(doc: &mut AutoCommit, id: &str, author: &str, parent: Option<&str>) {
        dispatch(
            doc,
            &send_message_action(
                "c_fb",
                id,
                author,
                TS,
                "a report",
                parent.map(str::to_string),
                None,
                Vec::new(),
            ),
        );
    }

    fn post_record(doc: &mut AutoCommit, id: &str, record: RecordAttachment) {
        dispatch(
            doc,
            &send_message_action(
                "c_fb",
                id,
                model::ME,
                TS,
                "",
                None,
                Some(Attachment::Record(Box::new(record))),
                Vec::new(),
            ),
        );
    }

    fn message(doc: &AutoCommit, id: &str) -> model::Message {
        ChatDoc::list_messages(doc, "c_fb")
            .into_iter()
            .find(|m| m.id == id)
            .expect("the message exists")
    }

    fn ctx(doc: &AutoCommit, me: &str) -> ChatResourceCtx {
        ChatResourceCtx {
            me: me.into(),
            workspace_id: "ws_tilo".into(),
            workspace_name: "Tilo Core".into(),
            channels: Vec::new(),
            active_channel: "c_fb".into(),
            messages: Vec::new(),
            participants: ChatDoc::list_users(doc)
                .into_iter()
                .map(|u| crate::mention::Participant {
                    id: u.id,
                    name: u.name,
                    is_agent: u.is_agent,
                })
                .collect(),
            comment_messages: Vec::new(),
            comment_only: false,
        }
    }

    fn resolved(doc: &AutoCommit, me: &str, id: &str) -> Resource {
        resolve_resource(&ctx(doc, me), doc, id).unwrap_or_else(|| panic!("{id} resolves"))
    }

    /// A comment-only participant's post, reply and remark are its own
    /// comment records in any channel, not only a feedback channel; a
    /// participant who may write keeps writing the conversation there.
    #[test]
    fn a_comment_only_participant_writes_comment_records_in_any_channel() {
        let mut doc = feedback_doc();
        post(&mut doc, "m_plain", "u_a71c", None);
        let plain = ChatDoc::list_channels(&doc)
            .into_iter()
            .find(|c| !c.feedback && c.kind == ChannelKind::Text)
            .expect("an ordinary text channel");
        dispatch(
            &mut doc,
            &send_message_action(
                &plain.id,
                "m_row",
                "u_a71c",
                TS,
                "hi",
                None,
                None,
                Vec::new(),
            ),
        );
        let is_comment = |spec: &ActionSpec| {
            let mut args = serde_json::Map::new();
            if spec.params().iter().any(|p| p.name == "reference") {
                args.insert("text".into(), "Send does nothing".into());
                args.insert(
                    "reference".into(),
                    serde_json::to_value(ResourceRef::whole("mail.draft", "mail.draft:d", "D"))
                        .unwrap(),
                );
            } else {
                args.insert(tilo_actions::PROMPTED_PARAM.into(), "hello".into());
            }
            built(spec, &args).type_id == CHAT_OPERATION_TYPE
        };
        let mut commenter = ctx(&doc, model::ME);
        commenter.comment_only = true;
        let channel = resolve_resource(&commenter, &doc, &format!("chat.channel:{}", plain.id))
            .expect("the channel");
        for id in ["post", "remark"] {
            assert!(
                is_comment(action(&channel, id).unwrap()),
                "{id} is a comment record"
            );
        }
        let message = resolve_resource(&commenter, &doc, "chat.message:m_row").expect("row");
        assert!(is_comment(action(&message, "reply").unwrap()));

        let writer = resolved(&doc, model::ME, &format!("chat.channel:{}", plain.id));
        assert!(
            !is_comment(action(&writer, "post").unwrap()),
            "a writer posts into the conversation"
        );
    }

    fn action<'a>(resource: &'a Resource, id: &str) -> Option<&'a ActionSpec> {
        resource.actions.iter().find(|a| a.id == id)
    }

    /// The remark an app composes arrives as chat's own `SendMessage`: its
    /// reference and recording become the row's reference and record
    /// attachment, and the delivery field chat does not model is ignored.
    #[test]
    fn a_remark_with_a_recording_becomes_a_message_with_a_chip_and_a_card() {
        let mut doc = feedback_doc();
        let remark = Remark::new(
            "the times contradict the prose",
            ResourceRef::whole("word_processor.block", "word-processor.block:blk_7", "¶3"),
            Destination::channel("c_fb", "feedback"),
        )
        .with_attachment(recording());
        let encoded = remark.to_chat_action("m_remark", model::ME, TS).unwrap();
        let read = tilo_reference::read_chat_action(&encoded).expect("a remark reads back");
        assert_eq!(read.attachment, Some(recording()));
        dispatch_encoded(&mut doc, encoded);

        let m = message(&doc, "m_remark");
        assert_eq!(
            m.reference.as_ref().map(|r| r.id.as_str()),
            Some("word-processor.block:blk_7")
        );
        assert_eq!(
            m.attachment,
            Some(Attachment::Record(Box::new(recording())))
        );
        assert_eq!(
            m.attachment.as_ref().and_then(Attachment::blob),
            None,
            "no bytes, only an address"
        );

        // The reference is an addressable child whose open is a workspace
        // request, so the chip, an agent and tilo-control open it one way.
        let chip = resolved(&doc, model::ME, "chat.reference:m_remark");
        let open = request(action(&chip, "open").expect("open"));
        assert_eq!(open.resource, tilo_actions::remote::RESOURCES_RESOURCE);
        assert_eq!(
            open.argument,
            InvokeArgument::Text("word-processor.block:blk_7".into())
        );
    }

    /// A private remark rides the pair's direct room and lands in its
    /// conversation as an ordinary message with its reference.
    #[test]
    fn a_private_remark_arrives_in_the_direct_room_conversation() {
        let (ada, team) = ("u_1f4c3a9b2d7e5061", "u_9a0b1c2d3e4f5061");
        let room = DirectRoom::between(ada, team).unwrap();
        let mut doc = AutoCommit::new();
        ChatDoc::seed_starter(&mut doc, ada, "Ada", "A");
        let encoded = Remark::new(
            "this crashed",
            ResourceRef::whole("files.entry", "files.entry:/w/a.png", "a.png"),
            Destination::participant(team, "Tilo team", false),
        )
        .to_direct_action(&room, "m_private", ada, TS)
        .unwrap();
        dispatch_encoded(&mut doc, encoded);
        let row = ChatDoc::list_messages(&doc, tilo_reference::ROOM_CONVERSATION)
            .into_iter()
            .find(|m| m.id == "m_private")
            .expect("the direct room's conversation holds it");
        assert_eq!(row.reference.unwrap().id, "files.entry:/w/a.png");
    }

    #[test]
    fn votes_are_the_canonical_reaction_and_count_once_per_person() {
        let mut doc = feedback_doc();
        post(&mut doc, "m_r", "u_a71c", None);
        for user in ["u_me", "u_3c9e", "u_me"] {
            dispatch(&mut doc, &vote_action("c_fb", "m_r", user, true));
        }
        dispatch(
            &mut doc,
            &add_reaction_action("c_fb", "m_r", "🎉", "u_8b02"),
        );
        assert_eq!(
            vote_count(&message(&doc, "m_r")),
            2,
            "a repeat vote is one vote"
        );
        dispatch(&mut doc, &vote_action("c_fb", "m_r", "u_3c9e", false));
        let m = message(&doc, "m_r");
        assert_eq!(vote_count(&m), 1);
        assert!(
            m.reactions.iter().any(|r| r.emoji == "🎉"),
            "other reactions stay ordinary reactions"
        );

        // The vote is the report resource's own action, Comment-gated.
        let report = resolved(&doc, "u_3c9e", "chat.message:m_r");
        let vote = action(&report, "vote").expect("a report offers a vote");
        assert_eq!(vote.contract.permission, PermissionRequirement::Comment);
        assert_eq!(vote.kind, OperationKind::Effect, "a comment Chat performs");
        perform_comment(
            &mut doc,
            &ui::testing::action_contract::spec_encoding(vote, None),
        );
        assert_eq!(vote_count(&message(&doc, "m_r")), 2);
        let report = resolved(&doc, "u_3c9e", "chat.message:m_r");
        assert!(action(&report, "unvote").is_some());
        assert!(
            message(&doc, "m_r")
                .reactions
                .iter()
                .any(|r| r.emoji == VOTE_EMOJI && r.reacted_by("u_3c9e"))
        );
    }

    /// Status is Write — an Editor's — while posting, replying, reacting and
    /// voting are Comment, so a Commenter's catalogue offers those and never
    /// status, and the broker refuses status from one.
    #[test]
    fn status_is_gated_to_editors_and_participation_to_commenters() {
        let mut doc = feedback_doc();
        post(&mut doc, "m_r", "u_a71c", None);
        let report = resolved(&doc, model::ME, "chat.message:m_r");
        assert_eq!(
            action(&report, "set-status").unwrap().contract.permission,
            PermissionRequirement::Write
        );
        for id in ["reply", "react", "vote"] {
            assert_eq!(
                action(&report, id).unwrap().contract.permission,
                PermissionRequirement::Comment,
                "{id} is a Commenter's"
            );
        }
        let channel = resolved(&doc, model::ME, "chat.channel:c_fb");
        assert_eq!(
            action(&channel, "post").unwrap().contract.permission,
            PermissionRequirement::Comment
        );
        assert_eq!(
            action(&channel, "unmark-feedback")
                .unwrap()
                .contract
                .permission,
            PermissionRequirement::Write
        );
        assert!(action(&channel, "report-view").is_some());

        // In an ordinary channel a message is not a report.
        dispatch(&mut doc, &set_channel_feedback_action("c_fb", false));
        let plain = resolved(&doc, model::ME, "chat.message:m_r");
        assert!(action(&plain, "set-status").is_none());
        assert!(action(&plain, "vote").is_none());

        let commenter = crate::permissions::ChatPermissions::from_context(
            ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(Some(ui::permissions::Role::Commenter), "u_c"),
                vec![],
            ),
        );
        assert!(commenter.can_comment(), "a Commenter votes");
        assert!(!commenter.can_participate(), "and does not set status");
    }

    #[test]
    fn set_status_decodes_typed_arguments_and_applies_only_to_reports() {
        let mut doc = feedback_doc();
        post(&mut doc, "m_a", "u_a71c", None);
        post(&mut doc, "m_b", "u_3c9e", None);
        post(&mut doc, "m_reply", "u_8b02", Some("m_a"));
        let report = resolved(&doc, model::ME, "chat.message:m_b");
        let spec = action(&report, "set-status").unwrap();

        let mut args = serde_json::Map::new();
        args.insert("status".into(), "fixed".into());
        args.insert("generation".into(), 121.into());
        dispatch_encoded(&mut doc, built(spec, &args));
        assert_eq!(
            status_of(&message(&doc, "m_b")),
            ReportStatus::Fixed { generation: 121 }
        );
        assert!(status_of(&message(&doc, "m_b")).fixed_for(121));
        assert_eq!(message(&doc, "m_b").report.unwrap().by, model::ME);

        assert!(report_status_from_args("fixed", None, None).is_err());
        assert!(report_status_from_args("duplicate", None, None).is_err());
        assert!(report_status_from_args("maybe", None, None).is_err());
        assert_eq!(
            report_status_from_args("duplicate", None, Some("chat.message:m_a")),
            Ok(ReportStatus::Duplicate { of: "m_a".into() })
        );

        let set = |status| set_report_status_action("c_fb", "m_b", status, "u_d417", TS);
        dispatch(&mut doc, &set(ReportStatus::Duplicate { of: "m_a".into() }));
        let duplicate = ReportStatus::Duplicate { of: "m_a".into() };
        assert_eq!(status_of(&message(&doc, "m_b")), duplicate);
        // Not a duplicate of itself, nor of a reply; and a reply has no status.
        dispatch(&mut doc, &set(ReportStatus::Duplicate { of: "m_b".into() }));
        dispatch(
            &mut doc,
            &set(ReportStatus::Duplicate {
                of: "m_reply".into(),
            }),
        );
        assert_eq!(status_of(&message(&doc, "m_b")), duplicate);
        dispatch(
            &mut doc,
            &set_report_status_action("c_fb", "m_reply", ReportStatus::WontFix, "u_d417", TS),
        );
        assert!(message(&doc, "m_reply").report.is_none());
        // In an ordinary channel nothing is a report.
        dispatch(&mut doc, &set_channel_feedback_action("c_fb", false));
        dispatch(
            &mut doc,
            &set_report_status_action("c_fb", "m_a", ReportStatus::Planned, "u_d417", TS),
        );
        assert!(message(&doc, "m_a").report.is_none());
        assert_eq!(
            history_context(&set(ReportStatus::Planned)).action_label,
            "Set report status"
        );
    }

    /// Posting a record shares it into the room through the room's own index
    /// — a workspace request, never copied bytes — and withdrawing takes the
    /// reference back while the card stays to say so.
    #[test]
    fn a_record_attachment_is_shared_and_withdrawn_by_reference() {
        let mut doc = feedback_doc();
        post_record(&mut doc, "m_rec", recording());
        let card = resolved(&doc, model::ME, "chat.attachment:m_rec");
        let share = action(&card, "share").expect("the author shares it");
        assert!(share.contract.is_programmatic());
        let shared = request(share);
        assert_eq!(shared.resource, tilo_actions::remote::ROOM_RECORDS_RESOURCE);
        assert_eq!(
            shared.action,
            tilo_actions::remote::ROOM_RECORD_REFERENCE_ACTION
        );
        let InvokeArgument::Json(args) = &shared.argument else {
            panic!("the reference names the record: {:?}", shared.argument);
        };
        assert_eq!(args["id"], "uuid:9f2c-send");
        assert_eq!(args["kind"], "session");
        let replay = request(action(&card, "replay").expect("a recording replays"));
        assert_eq!(
            replay.resource,
            tilo_actions::remote::SESSION_REPLAY_RESOURCE
        );
        assert_eq!(
            replay.argument,
            InvokeArgument::Text("uuid:9f2c-send".into())
        );
        assert!(action(&card, "withdraw").is_some());

        // Someone else's card replays, but its sharing is not theirs.
        let other = resolved(&doc, "u_a71c", "chat.attachment:m_rec");
        assert!(action(&other, "share").is_none());
        assert!(action(&other, "withdraw").is_none());
        assert!(action(&other, "replay").is_some());

        dispatch(&mut doc, &withdraw_attachment_action("c_fb", "m_rec"));
        let m = message(&doc, "m_rec");
        assert!(m.attachment_withdrawn);
        assert!(m.attachment.is_some(), "the card stays");
        let card = resolved(&doc, model::ME, "chat.attachment:m_rec");
        let unshared = request(action(&card, "unshare").expect("withdrawal drops the reference"));
        assert_eq!(
            unshared.action,
            tilo_actions::remote::ROOM_RECORD_UNREFERENCE_ACTION
        );
        assert!(action(&card, "replay").is_none());
        assert!(action(&card, "share").is_none());
    }

    /// The author's Chat asks the room once per record per session, however
    /// the message was posted, and asks it to drop a withdrawn one.
    #[test]
    fn the_authors_chat_asks_the_room_to_reference_each_record_once() {
        let mut doc = feedback_doc();
        post_record(&mut doc, "m_rec", recording());
        let mut app = crate::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.reload(&doc);
        let asked: Vec<_> = std::iter::from_fn(|| app.feedback.controls.pop_front())
            .map(|c| (c.address.resource, c.address.action))
            .collect();
        assert_eq!(
            asked,
            [("chat.attachment:m_rec".to_string(), "share".to_string())]
        );
        dispatch(&mut doc, &withdraw_attachment_action("c_fb", "m_rec"));
        app.reload(&doc);
        let asked: Vec<_> = std::iter::from_fn(|| app.feedback.controls.pop_front())
            .map(|c| c.address.action)
            .collect();
        assert_eq!(asked, ["unshare"]);
    }

    #[test]
    fn an_invalid_record_attachment_is_refused() {
        let mut doc = feedback_doc();
        let mut bad = recording();
        bad.record = "not-a-record".into();
        post_record(&mut doc, "m_bad", bad);
        assert!(ChatDoc::list_messages(&doc, "c_fb").is_empty());
    }

    /// The composer's typed path: the same fields a Remark carries.
    #[test]
    fn a_remark_posted_through_the_channel_action_keeps_its_address_and_record() {
        let mut doc = feedback_doc();
        let channel = resolved(&doc, model::ME, "chat.channel:c_fb");
        let spec = action(&channel, "remark").expect("a channel takes remarks");
        assert_eq!(spec.contract.permission, PermissionRequirement::Comment);
        let mut args = serde_json::Map::new();
        args.insert("text".into(), "Send does nothing".into());
        args.insert(
            "reference".into(),
            serde_json::to_value(ResourceRef::whole("mail.draft", "mail.draft:d_42", "Draft"))
                .unwrap(),
        );
        args.insert(
            "attachment".into(),
            serde_json::to_value(recording()).unwrap(),
        );
        args.insert("message".into(), "m_named".into());
        perform_comment(&mut doc, &built(spec, &args));
        let m = ChatDoc::list_messages(&doc, "c_fb").remove(0);
        assert_eq!(
            m.id, "m_named",
            "the message is posted under the id its recording was named after"
        );
        args.remove("message");
        assert_eq!(m.author, model::ME);
        assert_eq!(m.reference.unwrap().id, "mail.draft:d_42");
        assert_eq!(
            m.attachment,
            Some(Attachment::Record(Box::new(recording())))
        );

        args.insert("reference".into(), "not an address".into());
        assert!(spec.build_json(&args).is_err(), "a reference is an object");
        args.insert(
            "reference".into(),
            serde_json::json!({ "label": "no address" }),
        );
        let refused = built(spec, &args);
        assert_eq!(
            refused.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE
        );
    }

    /// An agent's drafted issue is a remark in the report's thread; only an
    /// Editor's `approve-issue` approves it, once, and only a draft can be.
    #[test]
    fn a_drafted_issue_replies_in_the_thread_and_only_an_editor_approves_it() {
        let mut doc = feedback_doc();
        post(&mut doc, "m_report", "u_a71c", None);
        let channel = resolved(&doc, model::ME, "chat.channel:c_fb");
        let spec = action(&channel, "remark").expect("a channel takes remarks");
        let draft = crate::feedback::IssueDraft {
            report: "m_report".into(),
            title: "Send does nothing".into(),
            body: "> a report".into(),
        };
        let mut args = serde_json::Map::new();
        args.insert("text".into(), "Drafted an issue".into());
        args.insert(
            "reference".into(),
            serde_json::to_value(ResourceRef::whole(
                "chat.message",
                "chat.message:m_report",
                "a report",
            ))
            .unwrap(),
        );
        args.insert("parent".into(), "m_report".into());
        args.insert("issue".into(), serde_json::to_value(&draft).unwrap());
        args.insert("message".into(), "m_draft".into());
        perform_comment(&mut doc, &built(spec, &args));
        let reply = ChatDoc::list_messages(&doc, "c_fb")
            .into_iter()
            .find(|m| m.id == "m_draft")
            .expect("the reply");
        assert_eq!(reply.parent.as_deref(), Some("m_report"));
        assert_eq!(
            reply.attachment.as_ref().and_then(Attachment::issue),
            Some(&draft)
        );
        assert_eq!(reply.approved, None);

        let resource = resolved(&doc, model::ME, "chat.message:m_draft");
        let approve = action(&resource, "approve-issue").expect("a draft can be approved");
        assert_eq!(approve.contract.permission, PermissionRequirement::Write);
        assert!(
            action(
                &resolved(&doc, model::ME, "chat.message:m_report"),
                "approve-issue"
            )
            .is_none(),
            "a report carries no draft"
        );

        dispatch(
            &mut doc,
            &approve_issue_action("c_fb", "m_draft", "u_team", 7),
        );
        dispatch(
            &mut doc,
            &approve_issue_action("c_fb", "m_draft", "u_other", 9),
        );
        assert_eq!(
            ChatDoc::issue_approval(&doc, "m_draft"),
            Some(crate::feedback::IssueApproval {
                by: "u_team".into(),
                since: 7
            }),
            "the first approval counts"
        );
        assert!(
            action(
                &resolved(&doc, model::ME, "chat.message:m_draft"),
                "approve-issue"
            )
            .is_none(),
            "an approved draft is not approved again"
        );
        dispatch(
            &mut doc,
            &approve_issue_action("c_fb", "m_report", "u_team", 7),
        );
        assert_eq!(
            ChatDoc::issue_approval(&doc, "m_report"),
            None,
            "only a draft is approved"
        );

        args.insert(
            "attachment".into(),
            serde_json::to_value(recording()).unwrap(),
        );
        assert_eq!(
            built(spec, &args).type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
            "a remark carries one attachment"
        );
    }

    /// A remark about several picked things carries them all, each its own
    /// addressable reference.
    #[test]
    fn a_remark_about_several_things_keeps_every_address() {
        let mut doc = feedback_doc();
        dispatch(&mut doc, &set_channel_feedback_action("c_fb", false));
        let remark = Remark::new(
            "these three buttons are misaligned",
            ResourceRef::whole("ui.node", "ui.node:save", "Save"),
            Destination::channel("c_fb", "feedback"),
        )
        .with_also(vec![
            ResourceRef::whole("ui.node", "ui.node:cancel", "Cancel"),
            ResourceRef::whole("ui.node", "ui.node:help", "Help"),
        ]);
        dispatch_encoded(
            &mut doc,
            remark.to_chat_action("m_many", model::ME, TS).unwrap(),
        );
        let m = message(&doc, "m_many");
        let ids: Vec<_> = m.references().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ui.node:save", "ui.node:cancel", "ui.node:help"]);
        let third = resolved(&doc, model::ME, "chat.reference:m_many#2");
        let open = request(action(&third, "open").unwrap());
        assert_eq!(open.argument, InvokeArgument::Text("ui.node:help".into()));
        assert!(resolve_resource(&ctx(&doc, model::ME), &doc, "chat.reference:m_many#3").is_none());
    }

    #[test]
    fn a_person_offers_message_and_invite_as_workspace_requests() {
        let doc = feedback_doc();
        let person = resolved(&doc, model::ME, "chat.person:u_a71c");
        let invite = action(&person, "invite").expect("invite");
        assert_eq!(invite.contract.permission, PermissionRequirement::Invite);
        for spec in [action(&person, "message").expect("message"), invite] {
            let asked = request(spec);
            assert_eq!(asked.resource, tilo_actions::remote::PEOPLE_RESOURCE);
            assert_eq!(asked.argument, InvokeArgument::Text("u_a71c".into()));
        }
        // Nobody messages themselves.
        let me = resolved(&doc, model::ME, "chat.person:u_me");
        assert!(action(&me, "message").is_none());
    }
}
