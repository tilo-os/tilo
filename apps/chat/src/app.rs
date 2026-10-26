//! Root widget for the chat app.
//!
//! [`ChatApp`] owns the CRDT-derived snapshot (channels / categories /
//! users / messages / drafts), the per-peer view state (active channel,
//! open thread, call state, composer caret), and one real
//! [`ui::widget::VideoPlayer`] used as the host call tile. It does its own
//! manual layout: the conversation (timeline and composer) beside the
//! shell's channel sidebar, plus floating layers
//! (thread panel, emoji picker, edit-history popover, call huddle /
//! expanded overlay) are computed from `self.rect` each frame, and every
//! clickable area is recorded into `self.hits` for the next event.
//!
//! Convergent state lives in the Automerge document; this widget is a
//! projection + an input router, in keeping with the toolkit's
//! "state lives in the CRDT" contract.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender};
use ui::widget::TaffyTreeExt;
use ui::widget::paint_damage::PaintParts;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use tilo_content::ContentStore;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Widget;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Collaborative, ContextMenu as SharedContextMenu, Continuable, FloatingSurfaceProvider,
    MenuItem, NavFeed, SidebarDrawer, VideoPlayer, WindowBarLink,
};

use crate::call::CallState;
use crate::geometry::Chrome;
use crate::identity::LocalIdentity;
use crate::model::{self, Channel, Id, Message, User};
use crate::permissions::ChatPermissions;

// --- Floating call huddle home placement (fractions of the window) --------
// The docked huddle's resting video rect before the user drags it. The
// VideoPlayer's taffy node and the surrounding chrome derive from this so
// they stay aligned; the drag offset is applied on top in `apply_layout`.
// (The expanded overlay instead tiles the host into a responsive grid — see
// `overlay_tiles` — so it has no fixed fraction.)
pub(crate) const HUDDLE_VIDEO: (f64, f64, f64, f64) = (0.505, 0.300, 0.250, 0.230);

/// What the room's private conversations and drafts are keyed by. Chat runs
/// in one Tilo workspace, its room, and holds no workspaces of its own: an
/// agent conversation, a private draft and a call continuation name the
/// room they belong to by this one scope.
pub(crate) const ROOM_SCOPE: &str = "room";

/// Which composer currently owns keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Main,
    Thread,
}

/// A right-click context menu over a channel or category row, or the
/// inline rename editor it can open.
pub(crate) struct ContextMenu {
    pub target: MenuTarget,
    pub items: Vec<MenuAction>,
    /// Toolkit-owned paint, clamping, pointer/keyboard routing, focus trap,
    /// and accessibility. Chat only maps the selected row to its domain
    /// action.
    pub widget: SharedContextMenu,
}

impl ContextMenu {
    /// A menu whose rows are named by `label`, so a row can say what it will
    /// do to this target ("Collapse category", "Unpin message"), and whose
    /// rows `checked` says name a state, checked while it holds.
    pub(crate) fn labelled(
        target: MenuTarget,
        anchor: Point,
        items: Vec<MenuAction>,
        viewport: Rect,
        label: impl Fn(MenuAction) -> String,
        checked: impl Fn(MenuAction) -> Option<bool>,
    ) -> Self {
        let mut widget = SharedContextMenu::new();
        for action in &items {
            let name = label(*action);
            let item = if action.is_destructive() {
                MenuItem::destructive_selection(&name)
            } else {
                MenuItem::selection(&name)
            };
            let icon = match action {
                MenuAction::Rename | MenuAction::EditMessage => Icon::Pencil,
                MenuAction::EditChannel => Icon::Settings,
                MenuAction::Delete | MenuAction::DeleteMessage => Icon::Trash,
                MenuAction::BanAuthor => Icon::Ban,
                MenuAction::StopSharing => Icon::EyeOff,
                MenuAction::MoveUp => Icon::ChevronUp,
                MenuAction::MoveDown | MenuAction::ToggleCollapse => Icon::ChevronDown,
                MenuAction::NewChannelHere
                | MenuAction::NewVoiceChannelHere
                | MenuAction::NewCategory => Icon::Plus,
                MenuAction::Archive => Icon::Archive,
                MenuAction::PinMessage => Icon::Pin,
                MenuAction::AddReaction => Icon::SmilePlus,
                MenuAction::ReplyInThread => Icon::Reply,
                MenuAction::ReportMessage | MenuAction::ReportAuthor => Icon::Flag,
                MenuAction::Silence => Icon::VolumeX,
                MenuAction::Volume(_) => Icon::Volume2,
            };
            let item = match checked(*action) {
                Some(on) => item.with_checked(on),
                None => item,
            };
            widget.push(item.with_icon(icon));
        }
        if viewport.width() > 0.0 && viewport.height() > 0.0 {
            widget.open_at_clamped([anchor.x as f32, anchor.y as f32], viewport);
        } else {
            widget.open_at([anchor.x as f32, anchor.y as f32]);
        }
        Self {
            target,
            items,
            widget,
        }
    }
}

/// What a context menu / create flow acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    Channel(Id),
    Category(Id),
    /// One of your own messages — the action menu offers Edit.
    Message(Id),
    /// The room itself: what can be made in it.
    Room,
    /// Someone else in the call, by person or guest id: how loud they play
    /// on this device.
    CallParticipant(String),
}

/// The structural actions reachable from a context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Rename,
    /// Change a channel's name, category and topic in the channel sheet.
    EditChannel,
    Delete,
    MoveUp,
    MoveDown,
    NewChannelHere,
    NewVoiceChannelHere,
    NewCategory,
    ToggleCollapse,
    /// Archive / unarchive a channel — hides it from the sidebar (reversible
    /// from Ctrl+K): an edit of the channel's `archived` field.
    Archive,
    /// React to a message with an emoji: opens the picker for it. The
    /// reaction row's "+" exists only once a message has reactions, so this
    /// is how anyone adds the first one.
    AddReaction,
    /// Open a message's thread to reply in it, also when it has no replies
    /// yet (the thread bar exists only once one does).
    ReplyInThread,
    /// Load one of your own messages into the inline editor.
    EditMessage,
    /// Pin / unpin one of your own messages (shared truth every peer sees).
    PinMessage,
    /// Delete one of your own messages, or as a moderator someone else's.
    DeleteMessage,
    /// Report someone else's message to the room's moderators.
    ReportMessage,
    /// Report a message's author to the room's moderators.
    ReportAuthor,
    /// As a moderator, ban a message's author from the room.
    BanAuthor,
    /// Stop sharing the window a message of yours shares.
    StopSharing,
    /// Stop hearing someone in the call, on this device only, or hear them
    /// again.
    Silence,
    /// Play someone in the call at this percent of as they arrive, on this
    /// device only.
    Volume(u16),
}

impl MenuAction {
    pub fn label(self) -> String {
        crate::i18n::t(match self {
            MenuAction::Rename => "chat-menu-rename",
            MenuAction::EditChannel => "chat-menu-edit-channel",
            MenuAction::Delete => "chat-menu-delete",
            MenuAction::MoveUp => "chat-menu-move-up",
            MenuAction::MoveDown => "chat-menu-move-down",
            MenuAction::NewChannelHere => "chat-menu-new-channel",
            MenuAction::NewVoiceChannelHere => "chat-menu-new-voice-channel",
            MenuAction::NewCategory => "chat-menu-new-category",
            MenuAction::ToggleCollapse => "chat-menu-collapse-category",
            MenuAction::Archive => "chat-menu-archive-channel",
            MenuAction::AddReaction => "chat-emoji-add-reaction",
            MenuAction::ReplyInThread => "chat-menu-reply-in-thread",
            MenuAction::EditMessage => "chat-menu-edit-message",
            MenuAction::PinMessage => "chat-menu-pin-message",
            MenuAction::DeleteMessage => "chat-menu-delete-message",
            MenuAction::ReportMessage => "chat-menu-report-message",
            MenuAction::ReportAuthor => "chat-menu-report-sender",
            MenuAction::BanAuthor => "chat-menu-ban",
            MenuAction::StopSharing => "chat-share-stop",
            MenuAction::Silence => "chat-call-silence",
            MenuAction::Volume(_) => "chat-menu-volume",
        })
    }
    pub fn is_destructive(self) -> bool {
        matches!(
            self,
            MenuAction::Delete | MenuAction::DeleteMessage | MenuAction::BanAuthor
        )
    }
    /// Menu items for a channel row, in order: the destructive one last.
    pub fn for_channel() -> [MenuAction; 6] {
        [
            MenuAction::EditChannel,
            MenuAction::Rename,
            MenuAction::Archive,
            MenuAction::MoveUp,
            MenuAction::MoveDown,
            MenuAction::Delete,
        ]
    }
    /// Menu items for the room: the window bar's Create, and a right click
    /// in the conversation.
    pub fn for_room() -> [MenuAction; 2] {
        [MenuAction::NewChannelHere, MenuAction::NewCategory]
    }
    /// Menu items for a category row.
    pub fn for_category() -> [MenuAction; 5] {
        [
            MenuAction::NewChannelHere,
            MenuAction::NewVoiceChannelHere,
            MenuAction::Rename,
            MenuAction::ToggleCollapse,
            MenuAction::Delete,
        ]
    }
    /// Menu items for one of your own messages.
    pub fn for_message() -> [MenuAction; 5] {
        [
            MenuAction::AddReaction,
            MenuAction::ReplyInThread,
            MenuAction::EditMessage,
            MenuAction::PinMessage,
            MenuAction::DeleteMessage,
        ]
    }
    /// Menu items for someone else in the call: how loud they play here,
    /// silenced or at one of a few levels, on this device only.
    pub fn for_call_participant() -> [MenuAction; 5] {
        [
            MenuAction::Silence,
            MenuAction::Volume(50),
            MenuAction::Volume(100),
            MenuAction::Volume(150),
            MenuAction::Volume(200),
        ]
    }
    /// Menu items for someone else's message: reporting it or its sender,
    /// and removing it, which only the room's moderators may.
    pub fn for_others_message() -> [MenuAction; 5] {
        [
            MenuAction::AddReaction,
            MenuAction::ReplyInThread,
            MenuAction::ReportMessage,
            MenuAction::ReportAuthor,
            MenuAction::DeleteMessage,
        ]
    }
}

/// The bounded worker's result. File I/O and header sniffing happen before
/// this crosses back to the UI thread; the only UI-thread write is the
/// canonical hash-only descriptor + `SendMessage` action in `sync`.
pub(crate) enum AttachmentImportResult {
    Ready {
        token: u64,
        name: String,
        blob: String,
        size: u64,
        width: u32,
        height: u32,
        video: bool,
    },
    Failed {
        token: u64,
        reason: String,
    },
}

/// The picker enumerates files off-thread too: even metadata on a mounted or
/// hostile workspace must not stall the composer click. A workspace that
/// cannot be read lists nothing: this device's files are still offered.
pub(crate) struct AttachmentPickerResult {
    pub generation: u64,
    pub entries: Vec<crate::picker::PickEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct AttachmentImportPending {
    pub token: u64,
    pub channel: Id,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentConversationDrag {
    pub(crate) peer: String,
    pub(crate) channel: Id,
    pub(crate) origin: Point,
}

/// Everything the user can click. Recorded during `draw`, resolved on the
/// next mouse-press.
#[derive(Clone, Debug)]
pub(crate) enum Hit {
    HostModel(String),
    Channel(Id),
    DirectPerson(String),
    AgentWindow,
    AgentDragHandle,
    /// Make a channel in a category.
    AddChannel {
        category: Id,
    },
    /// Make a channel or a category: what can be made in the room.
    AddCategory,
    Reaction {
        msg: Id,
        emoji: String,
    },
    /// The add button under a message; `anchor` is the button, which the
    /// picker opens beside.
    AddReaction {
        msg: Id,
        anchor: Rect,
    },
    OpenThread(Id),
    EditHistory {
        msg: Id,
        anchor: Point,
    },
    /// Right-click target over one of your own messages — opens the message
    /// action menu (Edit). Left-click on it is a no-op.
    MessageMenu {
        msg: Id,
    },
    /// Commit the inline message editor to the CRDT.
    EditSave,
    /// Discard the inline message editor without writing.
    EditCancel,
    /// Focus/place the caret in the inline message editor.
    FocusEdit,
    EmojiChoice(String),
    /// Show one Unicode emoji group (index into the catalogue) in the picker.
    EmojiGroup(usize),
    /// The picker's "Add emoji" tile: choose a picture for the workspace.
    EmojiAddCustom,
    /// Focus/place the caret in the emoji picker's search field.
    FocusEmojiSearch,
    FocusMain,
    FocusThread,
    Send,
    /// Bring back the message of your own you just deleted.
    UndoDelete,
    /// Return a scrolled conversation to its newest message without changing
    /// any shared document state.
    JumpToLatest,
    ThreadScroll(bool),
    AgentReasoning(String),
    ComposerEmoji(Rect),
    /// Open the workspace file picker to share an attachment.
    Attach,
    /// One rung of an attachment's ladder: open it at full size, save a copy,
    /// or show the copy the person saved.
    Attachment {
        msg: Id,
        action: ui::widget::conversation::AttachmentAction,
    },
    /// Close the open attachment.
    CloseAttachment,
    /// Pick a specific file (by path) from the open attachment picker.
    AttachPick(String),
    /// Choose files on this device with the platform's file chooser.
    AttachDevice,
    StartCall,
    StopSharingLiveContext,
    CopyResourceLink(Id),
    OpenResource(Id),
    /// Open the window a message shares, as a window of this person's own.
    OpenShare(Id),
    /// Stop sharing the window a message of yours shares.
    StopShare(Id),
    /// Open the "Share a window" panel from the attach menu.
    ShareWindowPanel,
    /// What the people a window is shared with may do there.
    ShareAccess(crate::record::ShareAccess),
    /// Choose the window of this tab in the "Share a window" panel.
    SharePick(u64),
    /// Share the window the panel has chosen.
    ShareChosen,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    /// Hear nothing of the call (and be muted), or hear it again.
    CallDeafen,
    /// Turn this person's camera on or off in the call.
    CallCamera,
    /// Share this person's screen in the call, or stop.
    CallScreen,
    CallExpand,
    CallLeave,
    /// Someone else in the call, by person or guest id: opens how loud
    /// they play here.
    CallParticipant(String),
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    /// Show or hide the open channel's pinned messages in the detail drawer.
    TogglePins,
    /// Show a message where it was written (a pinned message's row).
    RevealMessage(Id),
    /// Open the card of a message's author (their avatar and name), or a
    /// member's in the list: who they are, Message and Follow. `place`
    /// tells the targets of one person apart.
    ShowPerson {
        person: String,
        place: String,
    },
    Dismiss, // click on a scrim closes the top-most popover
}

impl Hit {
    pub(crate) fn semantic_id(&self) -> String {
        match self {
            Self::Channel(id) => format!("chat.channel:{id}"),
            Self::DirectPerson(key) => format!("chat.person:{key}"),
            Self::AgentWindow => "chat.agent-window".into(),
            Self::AgentDragHandle => "chat.agent-drag".into(),
            Self::AddChannel { category } => format!("chat.add-channel:{category}"),
            Self::AddCategory => "chat.add-category".into(),
            Self::TogglePins => "chat.pins".into(),
            Self::RevealMessage(id) => format!("chat.reveal-message:{id}"),
            Self::ShowPerson { place, .. } => format!("chat.person-card:{place}"),
            Self::FocusMain => "chat.composer".into(),
            Self::FocusThread => "chat.thread-composer".into(),
            Self::FocusEdit => "chat.message-edit".into(),
            Self::HostModel(command) => format!("chat.model.{command}"),
            Self::Send => "chat.send".into(),
            Self::UndoDelete => "chat.undo-delete".into(),
            Self::JumpToLatest => "chat.jump-to-latest".into(),
            Self::ThreadScroll(end) => format!("chat.thread-scroll-{end}"),
            Self::AgentReasoning(id) => format!("chat.reasoning.{id}"),
            Self::ComposerEmoji(_) => "chat.composer-emoji".into(),
            Self::Attach => "chat.attach".into(),
            Self::Attachment { msg, action } => {
                format!("chat.attachment:{msg}:{}", action.key())
            }
            Self::CloseAttachment => "chat.close-attachment".into(),
            Self::AttachPick(path) => format!("chat.attach-pick:{path}"),
            Self::AttachDevice => "chat.attach-device".into(),
            Self::StartCall => "chat.start-call".into(),
            Self::StopSharingLiveContext => "chat.stop-sharing-live-context".into(),
            Self::CopyResourceLink(id) => format!("chat.copy-resource-link:{id}"),
            Self::OpenResource(id) => format!("chat.open-resource:{id}"),
            Self::OpenShare(id) => format!("chat.open-share:{id}"),
            Self::StopShare(id) => format!("chat.stop-sharing:{id}"),
            Self::ShareWindowPanel => "chat.share-window".into(),
            Self::ShareAccess(access) => format!("chat.share-access:{}", access.as_str()),
            Self::SharePick(tab) => format!("chat.share-pick:{tab}"),
            Self::ShareChosen => "chat.share-chosen".into(),
            Self::ToggleRightFlap => "chat.toggle-details".into(),
            Self::OpenThread(id) => format!("chat.open-thread:{id}"),
            Self::CloseThread => "chat.close-thread".into(),
            Self::Reaction { msg, emoji } => format!("chat.reaction:{msg}:{emoji}"),
            Self::AddReaction { msg, .. } => format!("chat.add-reaction:{msg}"),
            Self::EmojiChoice(emoji) => format!("chat.emoji-choice:{emoji}"),
            Self::EmojiGroup(group) => format!("chat.emoji-group:{group}"),
            Self::EmojiAddCustom => "chat.emoji-add".into(),
            Self::FocusEmojiSearch => "chat.emoji-search".into(),
            Self::EditHistory { msg, .. } => format!("chat.edit-history:{msg}"),
            Self::EditSave => "chat.edit-save".into(),
            Self::EditCancel => "chat.edit-cancel".into(),
            Self::CallMute => "chat.call-mute".into(),
            Self::CallDeafen => "chat.call-deafen".into(),
            Self::CallCamera => "chat.call-camera".into(),
            Self::CallScreen => "chat.call-screen".into(),
            Self::CallParticipantsToggle => "chat.call-participants".into(),
            Self::CallExpand => "chat.call-expand".into(),
            Self::CallLeave => "chat.call-leave".into(),
            Self::CallParticipant(id) => format!("chat.call-participant:{id}"),
            other => format!("chat.control:{other:?}"),
        }
    }
}

/// How many of `messages`' last `unread` top-level messages mention one of
/// `me` and were written by someone else.
fn unread_mentions(messages: &[Message], unread: usize, me: &[&str]) -> u32 {
    let top: Vec<&Message> = messages
        .iter()
        .filter(|message| message.is_top_level())
        .collect();
    let count = top
        .iter()
        .skip(top.len().saturating_sub(unread))
        .filter(|message| !me.contains(&message.author.as_str()))
        .filter(|message| {
            message
                .mentions
                .iter()
                .any(|mention| me.contains(&mention.user.as_str()))
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The whole app's mutable state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (attachment-picker/emoji-popover open flags, layout/render bookkeeping, and --shots-only staging), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ChatApp {
    pub(crate) host_model: crate::host_model::HostModel,
    pub(crate) expanded_reasoning: std::collections::HashSet<String>,
    // CRDT projection (rebuilt every sync / after a mutation).
    pub(crate) channels: Vec<Channel>,
    pub(crate) categories: Vec<crate::model::Category>,
    /// People evidenced in the room's chat: the local person plus authors,
    /// reactors, mentioned people, and voice participants in its channels.
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
    /// Whole-document snapshots used by global search. The visible `messages`
    /// are the open channel's.
    pub(crate) search_messages: Vec<Message>,
    pub(crate) search_users: Vec<User>,
    /// The reviewed feedback reports filed here: each one's id and text.
    pub(crate) reports: Vec<(Id, String)>,
    /// The items a search hit or an agent's `open` asked to show, by their
    /// published ids, shown on the next sync.
    pub(crate) opened: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// Voice membership per channel of the room (cached from the
    /// CRDT each reload so the sidebar can show presence without re-reading).
    pub(crate) voice_members: HashMap<Id, Vec<crate::model::VoiceMember>>,
    /// Cached plain text of the focused drafts, mirrored from the composers.
    /// Kept only so the composer's auto-grow height (`composer_height`) and
    /// the tests can read the draft without reaching into the editor; the
    /// source of truth is the draft in the document.
    pub(crate) draft_main: String,
    pub(crate) draft_thread: String,

    /// The message composers: the SDK rich-text editor over each surface's
    /// shared draft. Enter sends, Shift+Enter breaks the line.
    pub(crate) composer_main: crate::composer::DraftComposer,
    /// Why the last message was not sent, while its text waits in the
    /// composer to be sent again.
    pub(crate) not_sent: Option<String>,
    /// A message of the person's own they just deleted, while their Undo
    /// can take it back: the part of the chat it was in, and the channel
    /// it was deleted from.
    /// Any later step of theirs in the chat takes the offer down: Undo
    /// would take back that step instead.
    pub(crate) deleted: std::cell::RefCell<Option<(u64, Id)>>,
    /// The last message posted from the main composer, by channel: put back
    /// in the composer if the room refuses it after it was applied here.
    pub(crate) last_post: Option<(Id, String)>,
    /// The open sheet that makes a channel or a category, or changes a
    /// channel.
    pub(crate) channel_sheet: Option<crate::channel_sheet::ChannelSheet>,
    /// The detail drawer shows the open channel's pinned messages.
    pub(crate) pins_open: bool,
    /// The top of the main composer as last drawn, `None` when none was. A
    /// detail drawer floating over the conversation ends above it, so the
    /// composer stays whole and usable while pins or people are open.
    pub(crate) composer_top: Option<f64>,
    pub(crate) composer_thread: crate::composer::DraftComposer,

    // Per-peer view state.
    pub(crate) active_channel: Id,
    pub(crate) focus: Focus,
    /// Offset from the newest edge of the timeline. Zero means the reader is
    /// looking at the newest message; negative values move back through
    /// history. The renderer owns its exact bounds because wrapping and media
    /// cards determine them.
    pub(crate) scroll: f64,
    /// Independent, top-origin thread offset and production paint metrics.
    pub(crate) thread_scroll: ui::widget::Scrollable,
    pub(crate) thread_follow: bool,
    pub(crate) thread_body: Rect,
    /// A durable read watermark is only advanced while the reader can actually
    /// see the newest edge. This flag is local presentation state, refreshed
    /// from the measured timeline during paint.
    pub(crate) timeline_at_end: bool,
    /// A person is looking at this instance: a runner laid it out in a real
    /// viewport. A hosted/headless worker only pumps the model and never lays
    /// out, so it must not author read receipts under the host identity —
    /// read state belongs to the person's own device.
    pub(crate) presented: bool,
    /// The first layout just happened, or the timeline just arrived at its
    /// newest edge: request one more model frame so the channel in view is
    /// marked read without waiting for input.
    pub(crate) read_on_present: bool,
    /// Last measured timeline height. Keeps a reader's visual anchor stable if
    /// new messages arrive while they are reading older messages.
    pub(crate) timeline_total_height: f64,
    pub(crate) scroll_content: f64,
    pub(crate) scroll_viewport: f64,
    pub(crate) thread_open: Option<Id>,
    pub(crate) revealed_message: Option<Id>,
    pub(crate) revealed_range: Option<(Id, String, usize, usize)>,
    /// A search result needs one initial scroll into view. Keeping this
    /// separate from `revealed_message` lets its visual accent remain while
    /// the reader subsequently scrolls through nearby context.
    pub(crate) reveal_scroll_pending: bool,
    pub(crate) revealed_person: Option<Id>,
    pub(crate) emoji_open: bool,
    pub(crate) emoji_target: Option<Id>,
    pub(crate) emoji_anchor: Rect,
    /// The toolkit's shared picker, placed at `emoji_anchor` while
    /// `emoji_open`.
    pub(crate) emoji_picker: ui::widget::EmojiPicker,
    /// Each message's reaction pills, in the timeline (`false`) and the
    /// thread (`true`), with their arrival and count motion.
    pub(crate) reaction_strips:
        std::cell::RefCell<HashMap<(bool, Id), ui::widget::reaction_strip::ReactionStrip>>,
    pub(crate) history_open: Option<Id>,
    pub(crate) history_anchor: Point,
    /// Your own message currently being edited. The shared multiline input
    /// owns its working text, caret, selection and IME composition until Save
    /// submits the record's `edit-message`.
    pub(crate) editing: Option<Id>,
    pub(crate) edit_input: crate::composer::MessageEditor,

    // Attachment picker (browses the shared workspace for a file to share).
    pub(crate) attach_open: bool,
    pub(crate) attach_entries: Vec<crate::picker::PickEntry>,
    /// Exactly one import may run at a time. This is a deliberate resource
    /// bound: selecting another file while one is reading is refused rather
    /// than creating unbounded reader threads and byte buffers.
    pub(crate) attachment_pending: Option<AttachmentImportPending>,
    /// The platform file chooser, while it is open: the files chosen on
    /// this device arrive here with their bytes.
    pub(crate) device_pick: Option<ui::portal::ContentsChannel>,
    /// Files chosen, pasted or dropped that wait for the one import at a
    /// time: their names and bytes.
    pub(crate) device_files: VecDeque<(String, Vec<u8>)>,
    pub(crate) attachment_next_token: u64,
    pub(crate) attachment_result_tx: SyncSender<AttachmentImportResult>,
    pub(crate) attachment_result_rx: Receiver<AttachmentImportResult>,
    /// A settled import outcome for the picker/status UI and accessibility.
    pub(crate) attachment_status: Option<String>,
    /// The attachment the person opened over the conversation, and the copy
    /// they asked to save.
    pub(crate) attachment_view: Option<crate::attachment_view::AttachmentView>,
    pub(crate) attachment_save: Option<crate::attachment_view::AttachmentSave>,
    pub(crate) attachment_picker_pending: Option<u64>,
    pub(crate) attachment_picker_generation: u64,
    pub(crate) attachment_picker_result_tx: SyncSender<AttachmentPickerResult>,
    pub(crate) attachment_picker_result_rx: Receiver<AttachmentPickerResult>,

    // Structure editing.
    pub(crate) menu: Option<ContextMenu>,
    /// The channel or category whose name a menu asked to change, until
    /// the event that asked hands the sidebar the keyboard.
    pub(crate) sidebar_rename: Option<String>,

    // Call state.
    pub(crate) call: CallState,
    pub(crate) native_voice: crate::local_voice::LocalVoice,
    pub(crate) resource_navigation: crate::shared_resources::Navigation,
    /// Windows shared live into the room ([`crate::live_share`]).
    pub(crate) live_shares: crate::live_share::LiveShares,
    pub(crate) moderation: crate::moderation::Moderation,
    /// Measured control/header sizes of the expanded call overlay, refreshed
    /// by the paint pass and read by the renderer-less layout functions.
    pub(crate) call_overlay_metrics: crate::render_call::CallOverlayMetrics,
    /// `(renderer scale, theme scale, catalogue revision)` the overlay metrics
    /// were measured at; unchanged means the measurement is skipped.
    pub(crate) call_overlay_metrics_key: Option<(u32, u32, u64)>,
    pub(crate) realtime: crate::continuity::ChatRealtimeContinuity,
    pub(crate) call_chrome_motion: CallChromeMotion,
    /// The speaking ring on each face in the call.
    pub(crate) speaking: crate::call::SpeakingRings,
    /// How many rows the floating call window's participant chips take,
    /// measured by the paint pass.
    pub(crate) huddle_chip_rows: usize,
    pub(crate) pending_floating: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// Authoritative combined-output bounds supplied by the shell. Standalone
    /// app/test hosts leave this empty and fall back to the app rect.
    pub(crate) floating_surface_bounds: Rect,

    /// Opens the platform search when the header's search button is used.
    pub(crate) search_opener: ui::search::SearchOpener,

    /// The shell's sidebar: the channels this window shows in it and what
    /// the person does there ([`crate::sidebar`]).
    pub(crate) places: NavFeed,
    /// The shell's own controls in the window bar, and its sidebar's
    /// state; `None` outside a shell (a floated conversation, a test).
    pub(crate) bar: Option<WindowBarLink>,
    /// The detail drawer: people, pins, or the open thread.
    pub(crate) right_flap: SidebarDrawer,
    /// The chrome metrics of the last painted theme.
    pub(crate) chrome: Chrome,
    /// Local text preference also changes usable layout width; it is never
    /// shared with collaborators.
    pub(crate) layout_text_scale: f32,

    // The local peer's identity: how this peer authors messages, reactions,
    // voice membership, and its own read watermark in the shared document.
    // Distinct + stable per real peer (see [`crate::identity`]) so genuine
    // multi-participant chat attributes and de-duplicates correctly.
    pub(crate) me: LocalIdentity,

    // The room's answer for who everybody is, refreshed every frame from
    // presence (see `adopt_room`). A peer never writes its own name into
    // this map — only what the room says about a participant lands here.
    pub(crate) room_names: HashMap<Id, String>,
    /// The person each of the room's devices belongs to, where presence
    /// says: a call's roster names people, presence names their devices,
    /// and `room_names` carries both so either reads as the same name.
    pub(crate) room_persons: HashMap<Id, Id>,
    /// Who the room's presence places in each voice channel.
    pub(crate) room_voice: HashMap<Id, Vec<Id>>,
    /// The numbers people who share a name are told apart by, as the room
    /// resolves them, so every screen in it numbers the same people alike.
    pub(crate) room_labels: ui::presence::ParticipantLabels,
    /// What the room calls this window's person, numbered like anyone else.
    pub(crate) own_label: Option<String>,
    pub(crate) room_colors: HashMap<Id, (u8, u8, u8)>,
    /// Exact bare Agent ids from the authenticated room roster.
    pub(crate) room_agents: Vec<Id>,
    /// Explicit shot fixture; never set on the live launch path.
    capture_agent_presence: Option<PresenceCtx>,
    pub(crate) direct_person: Option<String>,
    pub(crate) person_subscription: Option<uuid::Uuid>,
    pub(crate) person_contacts: Vec<ui::person_chat::Contact>,
    pub(crate) person_messages: Vec<ui::person_chat::Message>,
    pub(crate) person_error: Option<String>,
    /// The conversation being opened, until the room answers.
    pub(crate) person_opening: Option<crate::person_conversation::Opening>,
    /// Opens another one replaced: each lane they open is released.
    pub(crate) person_abandoned: Vec<crate::person_conversation::Opening>,
    /// The message the composer is sending, until the room accepts it.
    pub(crate) person_sending: Option<crate::person_conversation::Sending>,
    pub(crate) person_body_views: HashMap<String, (String, crate::composer::MessageEditor)>,
    /// Retained direct conversations currently owned by the shared
    /// floating-surface host, keyed by their deterministic conversation id.
    /// The person's reduced-motion setting, applied to every floating
    /// placement this app owns (`Widget::set_motion_reduced`).
    pub(crate) motion_reduced: bool,
    pub(crate) agent_surfaces:
        HashMap<Id, crate::agent_conversation_surface::AgentConversationSurface>,
    pub(crate) agent_surface_requests: VecDeque<ui::floating_surface::FloatingSurfaceRequest>,
    pub(crate) focused_agent_surface: Option<String>,
    pub(crate) input_agent_surface: Option<String>,
    pub(crate) conversation_surface: bool,
    pub(crate) conversation_return_requested: bool,
    pub(crate) agent_conversation_drag: Option<AgentConversationDrag>,

    // Agent display names of agent-authored messages (`agent_authors` in the
    // document): what every member renders as "<name> (agent)".
    pub(crate) agent_authors: HashMap<Id, String>,
    /// What the room's host keeps of the files members post, as it says:
    /// a file it would not take is refused here, in plain words.
    pub(crate) room_files: crate::record::RoomFiles,
    /// Each shown message body, read-only in the SDK editor, by body size
    /// and message id, with the mention colours it was painted with.
    pub(crate) body_views:
        std::cell::RefCell<HashMap<(u32, Id), (u64, tilo_rich_text::RichTextView)>>,
    /// How far the body view sets its text inside the rect it is laid out
    /// in, as last read back from a laid-out paragraph. Bodies are laid out
    /// that far to the leading side, so their text starts on the message
    /// column's edge.
    pub(crate) body_text_inset: std::cell::Cell<f64>,
    /// The colours mentions are painted in, and what they were built from.
    pub(crate) mention_paint_cache:
        std::cell::RefCell<Option<(u64, ui::widget::marks::MentionPaint)>>,
    // Agent task participants this (headless host) worker serves, with the
    // display name the host labelled each with. A delivered `post` resolved
    // for one of them is authored by that participant.
    pub(crate) participant_labels: HashMap<Id, String>,

    // Other spellings of this window's person the room uses. See
    // `adopt_own_participant`: what was said under one of these is still
    // this person, so their lines go on reading as theirs.
    pub(crate) former_ids: Vec<Id>,

    // The OS permission gate: routes channel participation + file-embed
    // hydration through the toolkit's permission model. Refreshed from the
    // `PermissionContext` the compositor scopes around `draw`.
    pub(crate) perms: ChatPermissions,

    // The resource's dispatcher: every doc-mutating affordance (Send, a
    // reaction, the channel/message context menus) applies its verb or
    // field edit through it, permission-checked, attributed to this
    // window's person, recorded in history and replicated. See
    // [`crate::record`].
    pub(crate) dispatch: Option<ui::framework::Dispatcher<crate::resource::Chat>>,
    /// The read receipt last sent on its own and not answered yet, by
    /// channel, and the one the door refused: a receipt the door does not
    /// offer this person (a viewer) is not sent again until they read
    /// further.
    pub(crate) read_asked: Option<(String, crate::parts::ReadMark)>,
    pub(crate) read_refused: Option<(String, crate::parts::ReadMark)>,
    /// The chat's messages, kept in parts beside the record
    /// ([`crate::parts`]): what the window reads them from and posts, edits
    /// and reacts through. `None` where the window has none (a past
    /// version).
    pub(crate) chain: Option<ui::framework::Chain<crate::parts::ChatPart>>,
    /// The chain's revision the window last read its messages at.
    pub(crate) chain_seen: Option<u64>,
    /// The messages the chat record kept in itself, read once for the
    /// object holding them ([`crate::record::kept_messages`]).
    pub(crate) kept_messages: Option<(
        automerge::ObjId,
        std::rc::Rc<ui::tilo_resource::Collection<crate::record::Message>>,
    )>,
    /// What the chat record kept as the names of the people in it
    /// ([`crate::record::kept_names`]), read with the kept messages.
    pub(crate) kept_names: std::collections::BTreeMap<String, String>,
    /// The first part a screenshot scenario starts with, until written:
    /// messages by several people under the ids the scenario names.
    pub(crate) staged_start: Option<crate::parts::ChatPart>,
    /// A screenshot scenario's messages still to stage, each with who
    /// wrote it: staged once the room's records take a write (a frame's
    /// read of the record holds them).
    pub(crate) staged_parts: std::collections::VecDeque<(
        &'static str,
        ui::tilo_resource::Invoke<crate::parts::ChatPart>,
    )>,
    /// The scenario those messages are of.
    pub(crate) staged_scenario: Option<String>,
    /// The chat record as the window last read it: what a test's detached
    /// chain reads where its actions are invoked.
    #[cfg(test)]
    pub(crate) last_read: std::rc::Rc<std::cell::RefCell<std::rc::Rc<crate::record::Chat>>>,
    /// Why the newest read of the record did not read, while the channels,
    /// messages and drafts above keep what last read whole. The framework
    /// shows that reason in place of the view; this keeps every write derived
    /// from the projection waiting for a read that succeeds.
    pub(crate) chat_read: ui::tilo_resource::DocumentRead,
    /// The voice channel a started call joins once it is in the document.
    pub(crate) pending_call: Option<Id>,
    /// Ephemeral opt-in to publishing Chat navigation through viewport presence.
    pub(crate) sharing_context: bool,
    pub(crate) following_context: bool,
    pub(crate) context_follow_requested: bool,
    /// This person's Chat preferences, following their other devices; `None`
    /// where the host keeps none (a past version, a test).
    pub(crate) preferences: Option<ui::framework::Setting<crate::preferences::ChatPreferences>>,
    /// The categories this person folded away, as last read or toggled.
    pub(crate) collapsed: std::collections::BTreeSet<Id>,
    /// Every action an affordance applied, for tests that compare them with
    /// the published ones.
    #[cfg(test)]
    pub(crate) dispatched: std::cell::RefCell<Vec<crate::record::ChatAction>>,
    /// Every verb on the chat's messages an affordance applied, likewise.
    #[cfg(test)]
    pub(crate) dispatched_parts: std::cell::RefCell<Vec<crate::messages::ChatPartAction>>,

    // Persistence (None for ephemeral / `--shots` instances).

    // Background image loader/cache for attachment thumbnails (decodes off
    // the UI thread; real bytes come from `content`).
    pub(crate) images: ui::ImageStore,
    /// Where attachment bytes live. Messages carry only content hashes; the
    /// import worker and the image fetcher each hold a clone.
    pub(crate) content: ContentStore,
    /// The attachments' bytes as reads ([`ui::arriving`]): here, on their
    /// way from the room, or with no device that has them.
    pub(crate) content_reads: ui::collaboration::record_content::ContentReads,
    /// When the next attachment on its way steps its placeholder on, as the
    /// last frame painted them.
    pub(crate) arriving_wake: std::cell::Cell<Option<ui::time::Instant>>,

    // Layout + render caches.
    pub(crate) rect: Rect,
    root_node: Option<NodeId>,
    video_node: Option<NodeId>,
    pub(crate) video: VideoPlayer,
    pub(crate) hits: Vec<(Rect, Hit)>,
    /// Ordinal in `keyboard_targets()`. Derived from the current hit inventory
    /// so Tab and assistive focus traverse the same controls a pointer can use.
    pub(crate) keyboard_target: Option<usize>,
    pub(crate) last_mouse: Point,
    /// The live call control beneath the pointer. Kept as render-only state so
    /// attached and shell-owned copies share the same hover treatment without
    /// duplicating a second native-window control implementation.
    pub(crate) call_hovered: Option<Hit>,
    relayout: bool,
    /// True while either flap is mid-slide — drives `needs_animation_frame`
    /// so the loop keeps ticking only during the transition, then idles.
    flap_animating: bool,
    /// This window's person still has to be announced into the shared
    /// roster under the name this device knows them by.
    announce: Option<crate::identity::LocalIdentity>,
    /// A live window tells its person about mentions of them that arrive
    /// while they look elsewhere.
    mention_alerts: Option<crate::mention_alerts::MentionAlerts>,
    /// The person's own drafts record, so the sidebar can say where the
    /// unsent words are.
    pub(crate) drafts_record: Option<ui::framework::PersonRecord<crate::drafts::ChatDrafts>>,

    /// Latest decoded camera picture per person, shown in their call tile
    /// (this person's own among them, in the host tile). Only ever holds a
    /// picture the call delivered and [`Self::call_pictures`] decoded; a tile
    /// without one shows the person, never a made-up picture.
    pub(crate) remote_frames: HashMap<String, vello::peniko::ImageData>,

    /// Latest decoded **screen-share** picture per person, kept separate
    /// from their camera picture so a person can share their screen and keep
    /// their camera on at once: the screen fills the featured stage tile, the
    /// camera stays in their filmstrip cell. Until the first picture arrives
    /// the stage says whose screen it waits for.
    pub(crate) remote_screens: HashMap<String, vello::peniko::ImageData>,
    /// Decodes the call's pictures, off the UI thread.
    pub(crate) call_pictures: ui::call_pictures::CallPictures,
    /// The picture tiles the last frame drew: whose camera or screen, and
    /// their size in device pixels. What the call is asked to deliver, each
    /// at the layer that fits its tile.
    pub(crate) picture_tiles: std::cell::RefCell<Vec<PictureTile>>,
    /// Where each message shown last frame drew its own texts, by message
    /// id: what its accessible row's children are read and picked by.
    pub(crate) painted_messages: HashMap<Id, PaintedMessage>,
    /// A video call was started: the camera turns on once the call is live.
    pub(crate) camera_on_live: bool,

    /// The latest call/media problem to show in the call chrome (a refused
    /// control, a native voice issue, or no connected media transport).
    pub(crate) media_error: Option<String>,
    pub(crate) canonical_dirty: bool,
    /// The regions the last draws painted, compared to say which changed:
    /// a typed character repaints the composer, not the window.
    paint_damage: ui::widget::paint_damage::PaintDamage,
    /// Document heads and pending-operation count at the last reload,
    /// so an unchanged document is not reloaded and repainted every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    pub(crate) host_focused: bool,
    pub(crate) external_services: bool,
}

/// A picture tile a frame drew: whose camera or screen, and its size in
/// device pixels.
pub(crate) type PictureTile = (String, ui::native_media::PictureSource, (f64, f64));

/// Native external services are explicit; browser peer transport is installed later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRuntime {
    Native,
    Unavailable,
}

/// Whether this host imports attachments from native paths. Voice support
/// is reported independently through the host's local media lane.
pub(crate) fn native_file_access() -> bool {
    tilo_files::available(tilo_files::Feature::NativePaths)
}

impl ChatApp {
    /// The chat surface for `identity`, keeping attachment bytes in
    /// `content`; the document arrives with its first sync.
    pub fn new(identity: LocalIdentity, runtime: ChatRuntime, content: ContentStore) -> Self {
        let mut app = Self::bare(content);
        app.external_services = runtime == ChatRuntime::Native;
        app.me = identity;
        app.active_channel.clear();
        app.thread_open = None;
        app.media_error =
            (!app.external_services).then(|| crate::i18n::t("chat-voice-unavailable"));
        app
    }

    /// Announce this window's person into the shared roster on the next
    /// sync, under the name this device knows them by.
    pub(crate) fn announcing(mut self) -> Self {
        self.announce = Some(self.me.clone());
        self.mention_alerts = Some(crate::mention_alerts::MentionAlerts::default());
        self
    }

    /// A demo-reading instance for tests: the demo's own person and its
    /// default room, attachment bytes in memory.
    #[cfg(test)]
    pub(crate) fn new_ephemeral() -> Self {
        let mut app = Self::bare(ContentStore::memory());
        app.attach_framework(ui::framework::Dispatcher::detached(
            "so.tilo.chat",
            crate::model::ME,
        ));
        app.attach_detached_chain(crate::model::ME);
        app
    }

    /// Hold the chat's messages in a chain of this window's own, as `me`,
    /// its actions reading the record as the window last read it: a test's
    /// stand-in for the room's.
    #[cfg(test)]
    pub(crate) fn attach_detached_chain(&mut self, me: &str) {
        let last_read = std::rc::Rc::clone(&self.last_read);
        if let Some(chain) = ui::framework::Chain::detached("so.tilo.chat", me, move || {
            std::rc::Rc::clone(&last_read.borrow())
        }) {
            self.attach_chain(chain);
        }
    }

    /// Show `room`, a test's chat, reading and writing its messages through
    /// its chain and refreshing the record its actions read.
    #[cfg(test)]
    pub(crate) fn attach_room(&mut self, room: &crate::demo::Room) {
        self.last_read = room.base();
        if let Some(chain) = room.chain() {
            let me = self.me.id.clone();
            self.attach_chain(chain.as_reader(&me));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete app state is deliberately initialized together so bounded channels and their lifecycles cannot drift"
    )]
    fn bare(content: ContentStore) -> Self {
        // The shared picker and reaction strip name and search emoji through
        // Chat's catalogue.
        crate::i18n::offer_emoji_catalogue();
        // The host call tile is a real VideoPlayer with no frame until a live
        // capture feeds one; there is no synthesised stand-in. Its honest
        // "camera off" state is painted over the empty surface.
        let video = VideoPlayer::new();
        let (attachment_result_tx, attachment_result_rx) = std::sync::mpsc::sync_channel(1);
        let (attachment_picker_result_tx, attachment_picker_result_rx) =
            std::sync::mpsc::sync_channel(1);
        Self {
            channels: Vec::new(),
            categories: Vec::new(),
            users: Vec::new(),
            messages: Vec::new(),
            search_messages: Vec::new(),
            search_users: Vec::new(),
            reports: Vec::new(),
            opened: std::rc::Rc::default(),
            voice_members: HashMap::new(),
            draft_main: String::new(),
            draft_thread: String::new(),
            composer_main: crate::composer::DraftComposer::shared(),
            not_sent: None,
            deleted: std::cell::RefCell::default(),
            last_post: None,
            channel_sheet: None,
            pins_open: false,
            composer_top: None,
            composer_thread: crate::composer::DraftComposer::shared(),
            active_channel: "c_crit".into(),
            focus: Focus::Main,
            scroll: 0.0,
            thread_scroll: ui::widget::Scrollable::new_vertical().always_visible(),
            thread_follow: false,
            thread_body: Rect::ZERO,
            timeline_at_end: true,
            presented: false,
            read_on_present: false,
            timeline_total_height: 0.0,
            scroll_content: 0.0,
            scroll_viewport: 0.0,
            thread_open: Some("m_concept".into()),
            revealed_message: None,
            revealed_range: None,
            reveal_scroll_pending: false,
            revealed_person: None,
            emoji_open: false,
            emoji_target: None,
            emoji_anchor: Rect::ZERO,
            emoji_picker: ui::widget::EmojiPicker::framed(),
            reaction_strips: std::cell::RefCell::default(),
            history_open: None,
            history_anchor: Point::ZERO,
            editing: None,
            edit_input: crate::composer::MessageEditor::local(),
            attach_open: false,
            attach_entries: Vec::new(),
            attachment_pending: None,
            device_pick: None,
            device_files: VecDeque::new(),
            attachment_next_token: 0,
            attachment_result_tx,
            attachment_result_rx,
            attachment_status: None,
            attachment_view: None,
            attachment_save: None,
            attachment_picker_pending: None,
            attachment_picker_generation: 0,
            attachment_picker_result_tx,
            attachment_picker_result_rx,
            menu: None,
            sidebar_rename: None,
            call: CallState::default(),
            native_voice: crate::local_voice::LocalVoice::default(),
            resource_navigation: crate::shared_resources::Navigation::default(),
            live_shares: crate::live_share::LiveShares::default(),
            moderation: crate::moderation::Moderation::default(),
            call_overlay_metrics: crate::render_call::CallOverlayMetrics::default(),
            call_overlay_metrics_key: None,
            realtime: crate::continuity::ChatRealtimeContinuity::default(),
            call_chrome_motion: CallChromeMotion::default(),
            speaking: crate::call::SpeakingRings::default(),
            huddle_chip_rows: 0,
            pending_floating: None,
            floating_surface_bounds: Rect::ZERO,
            search_opener: ui::search::SearchOpener::new(),
            places: NavFeed::new(),
            bar: None,
            right_flap: Self::detail_drawer(),
            chrome: Chrome::default(),
            layout_text_scale: 1.0,
            me: LocalIdentity::demo(),
            room_names: HashMap::new(),
            room_persons: HashMap::new(),
            room_voice: HashMap::new(),
            room_labels: ui::presence::ParticipantLabels::default(),
            own_label: None,
            room_colors: HashMap::new(),
            room_agents: Vec::new(),
            capture_agent_presence: None,
            direct_person: None,
            person_subscription: None,
            person_contacts: Vec::new(),
            person_messages: Vec::new(),
            person_error: None,
            person_opening: None,
            person_abandoned: Vec::new(),
            person_sending: None,
            person_body_views: HashMap::new(),
            motion_reduced: false,
            agent_surfaces: HashMap::new(),
            agent_surface_requests: VecDeque::new(),
            focused_agent_surface: None,
            input_agent_surface: None,
            conversation_surface: false,
            conversation_return_requested: false,
            agent_conversation_drag: None,
            agent_authors: HashMap::new(),
            room_files: crate::record::RoomFiles::default(),
            body_views: std::cell::RefCell::new(HashMap::new()),
            body_text_inset: std::cell::Cell::new(0.0),
            mention_paint_cache: std::cell::RefCell::new(None),
            participant_labels: HashMap::new(),
            former_ids: Vec::new(),
            // Default to the deterministic local bus; the live binary
            // (`ChatApp::new`) upgrades to the real transport. This keeps
            // `--shots`/tests from binding a real iroh endpoint per instance.
            perms: ChatPermissions::permissive(),
            host_model: crate::host_model::HostModel::default(),
            expanded_reasoning: std::collections::HashSet::new(),
            dispatch: None,
            read_asked: None,
            read_refused: None,
            chain: None,
            chain_seen: None,
            kept_messages: None,
            kept_names: std::collections::BTreeMap::new(),
            staged_start: None,
            staged_parts: std::collections::VecDeque::new(),
            staged_scenario: None,
            #[cfg(test)]
            last_read: std::rc::Rc::default(),
            chat_read: ui::tilo_resource::DocumentRead::default(),
            pending_call: None,
            sharing_context: false,
            following_context: false,
            context_follow_requested: false,
            preferences: None,
            collapsed: std::collections::BTreeSet::new(),
            #[cfg(test)]
            dispatched: std::cell::RefCell::default(),
            #[cfg(test)]
            dispatched_parts: std::cell::RefCell::default(),
            images: ui::ImageStore::new(&crate::images::fetcher(content.clone())),
            content_reads: ui::collaboration::record_content::ContentReads::new(content.clone()),
            arriving_wake: std::cell::Cell::new(None),
            content,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            keyboard_target: None,
            last_mouse: Point::ZERO,
            call_hovered: None,
            relayout: false,
            flap_animating: false,
            announce: None,
            mention_alerts: None,
            drafts_record: None,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            call_pictures: ui::call_pictures::CallPictures::new(),
            picture_tiles: std::cell::RefCell::new(Vec::new()),
            painted_messages: HashMap::new(),
            camera_on_live: false,
            media_error: None,
            canonical_dirty: true,
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            synced: None,
            host_focused: true,
            external_services: true,
        }
    }

    /// Whether a call action is permitted, routed through the real OS
    /// permission gate for the active workspace. A View-only / ungranted peer
    /// is blocked from join/speak/camera/screen-share; leaving is always
    /// allowed so a downgraded peer can drop out.
    pub(crate) fn call_allows(&self, action: crate::call::CallAction) -> bool {
        match action {
            crate::call::CallAction::Leave => true,
            crate::call::CallAction::Camera | crate::call::CallAction::ShareScreen => false,
            _ => self.voice_service_available() && self.perms.allows(action),
        }
    }

    pub(crate) fn voice_service_available(&self) -> bool {
        self.external_services
            || self.native_voice.snapshot.state != ui::native_media::State::Unavailable
    }

    /// Whether the local peer may post / react / start a call in the active
    /// workspace (an Edit grant). The permissive default allows; a restricted
    /// context gates writes while still letting the peer read.
    pub(crate) fn can_participate(&self) -> bool {
        self.perms.can_participate()
    }

    /// Whether the local peer keeps the room's layout: makes, renames,
    /// moves, archives and deletes its channels and categories. A member
    /// who may not is not offered it.
    pub(crate) fn keeps_layout(&self) -> bool {
        self.perms.can_moderate()
    }

    /// `items` as the local peer may use them: a member still collapses a
    /// category, and the rest of the layout is kept by those who keep it.
    pub(crate) fn layout_menu(&self, mut items: Vec<MenuAction>) -> Vec<MenuAction> {
        if !self.keeps_layout() {
            items.retain(|action| *action == MenuAction::ToggleCollapse);
        }
        items
    }

    /// Whether the local peer may hydrate + render an attachment blob. An
    /// ungranted embed shows a locked card and never requests shared bytes.
    pub(crate) fn embed_allowed(&self, blob: &str) -> bool {
        self.perms.can_embed(blob)
    }

    /// The poster frame of the video behind `blob`, once the image store has
    /// it; `None` for a clip with no poster to show.
    pub(crate) fn video_poster(&self, blob: &str) -> Option<vello::peniko::ImageData> {
        crate::images::poster_key(blob).and_then(|key| self.images.get(&key))
    }

    // --- action dispatch ----------------------------------------------------

    /// Apply a verb or a field edit through the resource's dispatcher now:
    /// the single document-mutation path of every on-screen affordance, the
    /// same door the palette and agents use. The projection is reconciled
    /// from the document on the next sync.
    pub(crate) fn apply_now(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            // A refusal changes nothing; the dispatcher reports it.
            let _ = dispatch.submit_now(doc, invoke);
        }
    }

    /// applied after this pass: from a pass that only reads the document (a
    /// frame's sync, a worker's answer). A refusal is reported by the
    /// dispatcher.
    pub(crate) fn submit(&self, submit: impl ui::framework::Submit<crate::resource::Chat>) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(invoke);
        }
    }

    /// `submit` what Chat does on its own, for nobody's gesture (a read
    /// receipt): a refusal is Chat's to handle, never said over the window.
    fn submit_on_its_own(&self, submit: impl ui::framework::Submit<crate::resource::Chat>) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit_on_its_own(invoke);
        }
    }

    /// Whether the door refused the read receipt last sent: it then is not
    /// offered to this person, and the same receipt is not sent again.
    fn note_refused_read(&mut self) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        let refused = dispatch.refused();
        let read_refused = refused.with(|refused| {
            refused
                .as_ref()
                .is_some_and(|refused| refused.action == crate::resource::Chat::MARK_READ.id)
        });
        if read_refused {
            refused.set(None);
            if let Some(asked) = self.read_asked.take() {
                self.read_refused = Some(asked);
            }
        }
    }

    /// `submit` as the invocation it is, noted for tests.
    #[cfg_attr(
        not(test),
        allow(clippy::unused_self, reason = "a test also keeps what was submitted")
    )]
    fn dispatched(
        &self,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) -> Option<ui::tilo_resource::Invoke<crate::resource::Chat>> {
        let invoke = submit.into_invoke().ok()?;
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched.borrow_mut().push(action.operation);
        }
        Some(invoke)
    }

    /// The last action an on-screen affordance applied.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<crate::record::ChatAction> {
        self.dispatched.borrow().last().cloned()
    }

    /// Test-only: apply what this window submitted to `doc`, as the host
    /// applies it after a pass.
    #[cfg(test)]
    pub(crate) fn land(&self, doc: &mut AutoCommit) {
        if let Some(dispatch) = &self.dispatch {
            ui::framework::testing::resolve(dispatch, doc).expect("submitted edits apply");
        }
        // What was submitted on the messages applies to their parts.
        if let Some(chain) = &self.chain {
            ui::framework::testing::resolve(&chain.dispatcher(), doc)
                .expect("submitted edits apply");
        }
    }

    /// Test-only: apply what this window asked the person to confirm (a
    /// destructive verb), as the door does once they agree, and read the
    /// record again.
    #[cfg(test)]
    pub(crate) fn confirm_asked(&mut self, doc: &mut AutoCommit) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        let mut chat = ui::tilo_resource::Replica::<crate::resource::Chat>::from_doc(doc.clone())
            .as_invoker(&self.me.id);
        for action in dispatch.take_asked().expect("what was asked builds") {
            chat.apply_action(&action)
                .expect("a confirmed change applies");
        }
        *doc = chat.doc().clone();
        if let Some(chain) = &self.chain {
            chain
                .confirm_asked()
                .expect("a confirmed change of a message applies");
        }
        self.reload(doc);
    }

    /// Read and write the chat's messages through `chain` from now on.
    pub(crate) fn attach_chain(&mut self, chain: ui::framework::Chain<crate::parts::ChatPart>) {
        self.chain = Some(chain);
        self.chain_seen = None;
        self.reselect();
    }

    /// Stage the messages a screenshot of `scenario` shows in the chat's
    /// parts, once: the first part as the scenario starts it, then what is
    /// done with its messages, each by its author. A chain that holds
    /// messages already is left as it is.
    pub(crate) fn stage_parts(&mut self, scenario: Option<&str>) {
        let Some(chain) = &self.chain else {
            return;
        };
        if chain.head().is_some() {
            return;
        }
        let (start, steps) = crate::resource::part_scenario(scenario);
        self.staged_start = start;
        self.staged_parts = steps.into();
        self.staged_scenario = scenario.map(str::to_owned);
        self.flush_staged_parts();
    }

    /// Stage what is left of a scenario's messages, until the room's
    /// records refuse a write for now.
    pub(crate) fn flush_staged_parts(&mut self) {
        let Some(chain) = self.chain.clone() else {
            return;
        };
        let mut staged = false;
        if chain.takes_changes()
            && let Some(start) = self.staged_start.take()
        {
            // Missing messages show in the picture's comparison.
            let _ = chain.write_start(0, &start);
            staged = true;
        }
        while chain.takes_changes()
            && let Some((author, invoke)) = self.staged_parts.pop_front()
        {
            // A message that does not stage is missing from the picture,
            // which its comparison shows.
            let _ = chain.stage_as(&invoke, author);
            staged = true;
        }
        if staged {
            self.reselect();
            // The view state the scenario stages names its messages: staged
            // again over them once they are all there.
            if self.staged_parts.is_empty()
                && let Some(scenario) = self.staged_scenario.take()
            {
                self.stage_scenario(&scenario);
            }
            self.request_relayout();
        }
    }

    /// The parts of the chat's messages this window holds.
    pub fn held_messages(&self) -> crate::messages::Messages {
        self.chain
            .as_ref()
            .map(|chain| crate::messages::Messages::new(chain.parts()))
            .unwrap_or_default()
            .with_kept(
                self.kept_messages
                    .as_ref()
                    .map(|(_, kept)| kept.clone())
                    .unwrap_or_default(),
            )
    }

    /// Read the messages the chat record `doc` kept in itself, unless they
    /// were read from the same object already.
    fn read_kept_messages(&mut self, doc: &AutoCommit) {
        let holding = ui::tilo_resource::evolution::map_at(doc, &automerge::ROOT, "messages")
            .ok()
            .flatten();
        if self.kept_messages.as_ref().map(|(id, _)| id) == holding.as_ref() {
            return;
        }
        self.kept_names = if holding.is_some() {
            crate::record::kept_names(doc)
        } else {
            std::collections::BTreeMap::new()
        };
        self.kept_messages =
            holding.map(|id| (id, std::rc::Rc::new(crate::record::kept_messages(doc))));
    }

    /// Apply a verb on the chat's messages now, through the chain's door:
    /// to the part it changes, permission-checked and attributed to this
    /// window's person. One that needs confirming goes to the door, which
    /// asks first. The window reads the change on its next sync.
    pub(crate) fn apply_part(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::parts::ChatPart>,
    ) {
        // A refusal changes nothing; the dispatcher reports it.
        let _ = self.try_apply_part(doc, submit);
    }

    /// [`Self::apply_part`], saying why when the chat refused it.
    pub(crate) fn try_apply_part(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::parts::ChatPart>,
    ) -> Result<(), String> {
        let chain = self
            .chain
            .as_ref()
            .ok_or_else(|| "this window holds no messages of the chat".to_owned())?;
        let invoke = submit
            .into_invoke()
            .map_err(|_| "the message could not be put together".to_owned())?;
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched_parts.borrow_mut().push(action.operation);
        }
        self.deleted.take();
        chain
            .dispatcher()
            .submit_now(doc, invoke)
            .map_err(|error| error.to_string())
    }

    /// Submit a verb on the chat's messages to the door, applied after this
    /// pass: from a pass that only reads the document.
    pub(crate) fn submit_part(&self, submit: impl ui::framework::Submit<crate::parts::ChatPart>) {
        let Some(chain) = &self.chain else {
            return;
        };
        let Ok(invoke) = submit.into_invoke() else {
            return;
        };
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched_parts.borrow_mut().push(action.operation);
        }
        self.deleted.take();
        chain.dispatcher().submit(invoke);
    }

    /// Edit through `dispatcher` from now on.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Chat>,
    ) {
        self.dispatch = Some(dispatcher);
    }

    /// Keep the composers' drafts in the person's own `record`: theirs
    /// alone, on each of their devices, never in the room.
    #[must_use]
    pub(crate) fn with_drafts(
        mut self,
        record: Option<ui::framework::PersonRecord<crate::drafts::ChatDrafts>>,
    ) -> Self {
        if let Some(record) = record {
            self.composer_main.attach_drafts(record.clone());
            self.composer_thread.attach_drafts(record.clone());
            // The sidebar shows where the unsent words are, so it holds the
            // record too.
            self.drafts_record = Some(record);
        }
        self
    }

    /// Follow this person's preferences from now on.
    pub(crate) fn follow_preferences(
        &mut self,
        preferences: ui::framework::Setting<crate::preferences::ChatPreferences>,
    ) {
        self.collapsed = preferences.get().collapsed_categories;
        self.preferences = Some(preferences);
        self.reselect();
    }

    /// Fold category `id` away in this person's sidebar, or unfold it.
    pub(crate) fn toggle_category(&mut self, id: &str) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
        if let Some(preferences) = &self.preferences {
            let collapsed = self.collapsed.clone();
            // Unsaved, the fold still holds in this window.
            let _ = preferences.update(|stored| stored.collapsed_categories = collapsed);
        }
        for category in &mut self.categories {
            category.collapsed = self.collapsed.contains(&category.id);
        }
        self.canonical_dirty = true;
    }

    /// The document moved under an unchanged selection, or the selection
    /// moved: read it again on the next sync.
    pub(crate) fn reselect(&mut self) {
        self.synced = None;
        self.canonical_dirty = true;
    }

    /// React to `msg` with `emoji`, or take the reaction back: on a filed
    /// report's message the edit is the report's own reactions field in the
    /// chat record, which is where its message is read from; on any other
    /// message it is [`Self::reaction_toggle_action`] on the message's part.
    pub(crate) fn toggle_reaction(&self, doc: &mut ui::widget::Edits<'_>, msg: &str, emoji: &str) {
        let report = crate::feedback::report_of_message(msg)
            .filter(|report| self.reports.iter().any(|(id, _)| id.as_str() == *report));
        let Some(report) = report else {
            self.apply_part(doc, self.reaction_toggle_action(msg, emoji));
            return;
        };
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.includes(&self.me.id))
        });
        let reactions = crate::resource::Chat::paths()
            .feedback
            .item(&ui::tilo_resource::Id::from_raw(report))
            .reactions;
        self.apply_now(
            doc,
            if reacting {
                reactions.unreact(emoji)
            } else {
                reactions.react(emoji)
            },
        );
    }

    /// Build the reaction a click on `msg`/`emoji` should dispatch, deciding
    /// add-vs-remove from the currently-loaded state at authoring time. A
    /// toggle is a presentation decision; the result is the framework's
    /// idempotent `react` or `unreact` field edit of the message — the single
    /// source of truth both the reaction pill and the emoji picker resolve to.
    pub(crate) fn reaction_toggle_action(
        &self,
        msg: &str,
        emoji: &str,
    ) -> ui::tilo_resource::Edit<crate::parts::ChatPart> {
        use crate::parts::ChatPart;
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.includes(&self.me.id))
        });
        let reactions = ChatPart::paths()
            .messages
            .item(&ui::tilo_resource::Id::from_raw(msg))
            .reactions;
        if reacting {
            reactions.unreact(emoji)
        } else {
            reactions.react(emoji)
        }
    }

    /// Stage the view state of screenshot `scenario` over the record its
    /// actions wrote, once the frame is laid out. The demo scenarios read the
    /// design-crit conversation; the default is an ordinary reading frame, and
    /// collaboration states are explicit so a capture does not teach a
    /// floating call as Chat's resting state.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        if let Some(bar) = (self.bar.as_ref()).filter(|_| !matches!(scenario, "" | "channels")) {
            // A capture is of the conversation, on a window that shows one
            // column at a time too.
            bar.stage_pane(ui::widget::NavPane::Content);
        }
        if scenario != "starter" {
            self.active_channel = "c_crit".into();
            // The demo conversation is read by its own person: what they
            // wrote is the reader's.
            if !self.is_me(crate::model::ME) {
                self.former_ids.push(crate::model::ME.into());
            }
        }
        self.sharing_context = scenario == "live-context";
        self.stage_view(scenario);
        self.reselect();
        self.request_relayout();
        self.stage_floating_call(scenario);
    }

    /// The panels, menus and call state of `scenario`.
    #[allow(
        clippy::too_many_lines,
        reason = "the screenshot scenario dispatcher keeps each named visual contract explicit and adjacent"
    )]
    pub(crate) fn stage_view(&mut self, scenario: &str) {
        match scenario {
            "narrow" => {
                // Overlay-collapsed conversation: both drawers closed, no
                // thread, no floating call — the clean single-column reading
                // state.
                if let Some(bar) = &self.bar {
                    bar.stage_sidebar(false);
                }
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
            }
            "history" => {
                // A reader deliberately looking above the newest message. This
                // is the visual-review state for the local live-edge return
                // affordance; no synthetic message or read receipt is made.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.scroll = 220.0;
                self.timeline_at_end = false;
            }
            "compact" => {
                // Medium window: docked channel list, conversation reflowed,
                // members/thread summoned on demand (not forced open), no call.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
            }
            "people" => {
                self.thread_open = None;
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "namesakes" => {
                // Two people in the room go by one name: everyone sees them
                // numbered alike, in the timeline and the people list.
                let mut presence = PresenceCtx::empty();
                presence.set_self_participant(
                    ui::permissions::UserId(self.me.id.clone()),
                    self.me.name.clone(),
                );
                for (id, name) in [
                    ("u_aria", "Aria Chen"),
                    ("u_sora", "aria chen"),
                    ("u_milo", "Milo Park"),
                    ("u_devon", "Devon Reyes"),
                    ("u_iris", "Iris Wolfe"),
                ] {
                    presence.set_display_name(ui::permissions::UserId(id.into()), name);
                }
                self.capture_agent_presence = Some(presence);
                self.thread_open = None;
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "agent-streaming" | "agent-failed" | "agent-reasoning" | "agent-long-thread" => {
                if matches!(scenario, "agent-reasoning" | "agent-long-thread") {
                    self.expanded_reasoning.insert("shot_agent_stream".into());
                }
                self.thread_open =
                    (scenario == "agent-long-thread").then(|| "shot_agent_stream".into());
                self.right_flap.snap(scenario == "agent-long-thread");
                self.call = CallState::default();
            }
            "agent-work" => {
                self.thread_open = Some("m_agent_request".into());
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "thread-narrow" => {
                self.thread_open = Some("m_concept".into());
                self.right_flap.snap(true);
                self.call = CallState::default();
            }
            "collaboration" => {
                self.thread_open = Some("m_concept".into());
                self.right_flap.open();
                self.call.join("c_huddle");
                self.stage_connected_call();
                // Two people in the room's call beside this one, so the
                // sidebar shows them under the voice channel.
                self.native_voice.snapshot.rosters = vec![ui::native_media::Roster {
                    channel_id: "c_huddle".into(),
                    people: vec!["u_aria".to_owned(), "u_milo".to_owned()],
                    ..Default::default()
                }];
                self.refresh_native_streams();
            }
            "lounge" | "lounge-call" | "lounge-video" | "lounge-share" | "lounge-bridged" => {
                // A shared room with people in its voice channels: this
                // person in the Design Huddle with their microphone on, three
                // others beside them and two in the standup, as the room's
                // call rosters report them before anyone opens a call.
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.muted = false;
                self.call.expanded = scenario != "lounge";
                self.right_flap.snap(false);
                self.stage_connected_call();
                let me = self.me.id.clone();
                let mut huddle = vec![
                    me,
                    "u_aria".to_owned(),
                    "u_milo".to_owned(),
                    "u_sora".to_owned(),
                ];
                if scenario != "lounge" {
                    huddle.push("u_devon".to_owned());
                }
                // Cameras on, and Sora sharing her screen, as the room says.
                let video = !matches!(scenario, "lounge" | "lounge-call" | "lounge-bridged");
                let cameras: Vec<String> = if video {
                    vec![
                        "u_aria".to_owned(),
                        "u_devon".to_owned(),
                        "u_milo".to_owned(),
                    ]
                } else {
                    Vec::new()
                };
                let screens: Vec<String> = if scenario == "lounge-share" {
                    vec!["u_sora".to_owned()]
                } else {
                    Vec::new()
                };
                self.native_voice.snapshot.rosters = vec![
                    ui::native_media::Roster {
                        channel_id: "c_huddle".into(),
                        people: huddle,
                        cameras: cameras.clone(),
                        screens: screens.clone(),
                        ..Default::default()
                    },
                    ui::native_media::Roster {
                        channel_id: "c_standup".into(),
                        people: if scenario == "lounge" {
                            vec!["u_devon".to_owned(), "u_iris".to_owned()]
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                ];
                // Aria is talking: her face rings wherever it shows.
                self.native_voice.snapshot.speaking = vec!["u_aria".to_owned()];
                if scenario == "lounge-bridged" {
                    self.stage_bridged_call();
                }
                self.refresh_native_streams();
                // Their pictures: a moving test picture each, coded and
                // decoded exactly as a call's pictures are, so the tiles show
                // real decoded frames.
                for (variant, person) in (0_u8..).zip(&cameras) {
                    self.call_pictures.feed_test_pattern(
                        person,
                        ui::native_media::PictureSource::Camera,
                        (640, 360),
                        variant,
                    );
                }
                for person in &screens {
                    self.call_pictures.feed_test_pattern(
                        person,
                        ui::native_media::PictureSource::Screen,
                        (1280, 720),
                        3,
                    );
                }
                self.refresh_native_streams();
            }
            "starter" => {
                // The real first-run room: `#chat` and `#voice`, no demo content.
                self.thread_open = None;
                self.call = CallState::default();
            }
            "context" => {
                self.menu = Some(self.context_menu(
                    MenuTarget::Channel("c_frontend".into()),
                    Point::new(240.0, 230.0),
                    MenuAction::for_channel().to_vec(),
                ));
            }
            "call" => {
                // The expanded full-window call overlay against the demo
                // huddle (host + three remotes), so the tile grid and the
                // wrapped controls bar can be captured headless at any width.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = true;
                self.stage_connected_call();
            }
            "floating-call-attached"
            | "floating-call"
            | "floating-call-hover"
            | "floating-call-returning"
            | "floating-call-returned" => {
                // The after-layout shot hook stages the production ownership
                // edge once app-local coordinates are authoritative. All
                // three frames use the real live huddle renderer.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = false;
                self.stage_connected_call();
            }
            "share" => {
                // The expanded call while screen-sharing: the featured screen
                // tile fills the stage above the participant filmstrip, and the
                // controls bar shows the active "Stop share". Local share is
                // staged directly (the same `call.sharing` flag the real toggle
                // sets on a portal grant) so the presentation layout renders
                // headless without a portal or capture device.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = true;
                self.call.sharing = true;
                self.stage_connected_call();
            }
            "attach" => {
                // Show the workspace file picker with real, decodable
                // thumbnails (the demo scheme drives the same decode path as
                // a stored image). Ephemeral shot only.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.attach_entries = vec![
                    crate::picker::PickEntry {
                        path: crate::images::DEMO_CALL_GRID.into(),
                        name: "call-overlay-v3.png".into(),
                        size: 84_992,
                        is_video: false,
                    },
                    crate::picker::PickEntry {
                        path: crate::images::DEMO_SHARED_SHOT.into(),
                        name: "design-handoff.png".into(),
                        size: 132_480,
                        is_video: false,
                    },
                ];
                self.attach_open = true;
            }
            "attach-loading" => {
                self.stage_view("attach");
                // This mirrors the actual asynchronous picker state rather
                // than a hand-painted status string: no entries are usable
                // until the worker's generation-checked reply settles.
                self.attach_entries.clear();
                self.attachment_picker_pending = Some(0);
                self.attachment_status = Some("Loading attachments…".into());
            }
            "attach-upload" => {
                // A browser page, where the files to share are the person's
                // own: the chooser has answered and the upload is under way.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.external_services = false;
                self.attachment_status = Some("Uploading quarterly-plan.pdf…".into());
            }
            "attach-error" => {
                self.stage_view("attach");
                self.attachment_status =
                    Some("Not shared: the file is no longer there; choose it again".into());
            }
            "edit" => {
                // The inline message editor open on your own last message,
                // with an earlier message still carrying the static "(edited)"
                // marker — the whole edit story in one frame. Ephemeral shot
                // only: at seed time the messages aren't loaded yet, so the
                // working text is staged directly in the same shared input the
                // live Edit action uses.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                let text = "That's the one. Starting a huddle to walk through the grid — jump in. Moving it to 3pm so the west-coast folks can make it.";
                self.edit_input.set_text(text);
                self.editing = Some("m_me2".into());
                self.focus = Focus::Main;
                self.set_composer_focus();
            }
            "channels" => {
                // The sidebar at rest: unread channels, a mention waiting in
                // another one, and people talking in a voice channel.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                // Three people in the stand-up's call, as their presence
                // says.
                let mut presence = PresenceCtx::empty();
                let standup = Self::voice_presence_key("c_standup");
                for person in ["u_aria", "u_milo", "u_sora"] {
                    presence.set_caret(&standup, ui::permissions::UserId(person.into()), 0);
                }
                self.capture_agent_presence = Some(presence);
            }
            "mention" => {
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.focus = Focus::Main;
                self.set_composer_focus();
            }
            "new-channel" => {
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                let mut sheet = crate::channel_sheet::ChannelSheet::new_channel(
                    "cat_acme_main",
                    model::ChannelKind::Text,
                );
                sheet.fill("Release Notes", "Changelogs and launch dates");
                self.open_channel_sheet(sheet);
            }
            "composer" => {
                // The composer mid-compose: a multi-line draft with the caret,
                // so the shared TextInput's text + caret + focus ring can be
                // graded. Thread/call closed to keep the composer prominent.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.focus = Focus::Main;
                // The draft itself is the scenario's (`ChatShell::scenario`).
            }
            "emoji" => {
                // The reaction picker over your own last message.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.emoji_open = true;
                self.emoji_target = Some("m_me2".into());
                self.emoji_anchor = Rect::new(560.0, 300.0, 590.0, 330.0);
            }
            "edit-history" => {
                // Every version of an edited message.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.history_open = Some("m_sora".into());
                self.history_anchor = Point::new(424.0, 420.0);
            }
            "msgmenu" => {
                // The right-click action menu over one of your own messages,
                // offering Edit — the discoverable entry point to editing.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.menu = Some(self.context_menu(
                    MenuTarget::Message("m_me2".into()),
                    Point::new(360.0, 300.0),
                    MenuAction::for_message().to_vec(),
                ));
            }
            _ => {
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
            }
        }
    }

    /// Show the call as the voice service reports a joined call: connected,
    /// with nobody else's audio arriving yet.
    fn stage_connected_call(&mut self) {
        self.native_voice.channel = Some(self.call.channel.clone());
        self.native_voice.snapshot.state = ui::native_media::State::Live;
        self.native_voice.snapshot.issue = None;
        self.native_voice.snapshot.muted = self.call.muted;
    }

    #[doc(hidden)]
    pub(crate) fn stage_floating_call(&mut self, scenario: &str) {
        if !scenario.starts_with("floating-call") {
            return;
        }

        if scenario == "floating-call-attached" {
            self.call.placement.pin_handoff_for_visual_review(1_000);
            return;
        }

        // A live drag seeds `preferred_size` from the current call tile before
        // crossing the app edge. Derive this staged desktop rect the same way;
        // hard-coding a different size would manufacture a return-time resize
        // that the production pointer path never performs.
        let detached_size = self.call_video_rect().size();
        self.call.placement.set_preferred_size(detached_size);
        // Out over the desktop at the window's trailing side, whole at any
        // window size.
        let r = self.rect;
        let detached = Rect::from_origin_size(
            Point::new(
                (r.x0 + r.width() * 0.6).min(r.x1 - detached_size.width - self.chrome.gutter),
                r.y0 + r.height() * 0.375,
            ),
            detached_size,
        );
        self.call.placement.detach_to_desktop(detached);
        self.call.placement.set_desktop_transfer_covered(true);
        self.call.placement.confirm_desktop_adoption();

        if scenario == "floating-call-hover" {
            self.call_hovered = Some(Hit::CallCamera);
        }

        if let Some(permille) = scenario
            .strip_prefix("floating-call-collapse-")
            .and_then(|value| value.parse::<u16>().ok())
        {
            self.call.participants_expanded = false;
            self.call_chrome_motion
                .pin_for_visual_review(1.0 - f32::from(permille.min(1_000)) / 1_000.0);
        }

        if matches!(
            scenario,
            "floating-call-returning" | "floating-call-returned"
        ) {
            // Grabbed by its title bar and drawn back up and in, over the
            // conversation.
            let pointer = Point::new(
                detached.center().x,
                detached.y0 - self.chrome.call_title_h / 2.0,
            );
            self.call
                .placement
                .begin_drag(pointer, self.rect, self.desktop_floating_bounds());
            let _ = self.call.placement.update_drag(
                Point::new(r.x0 + r.width() * 0.5625, r.y0 + r.height() * 0.2625),
                self.rect,
                self.desktop_floating_bounds(),
            );
        }

        if scenario == "floating-call-returned" {
            let id = format!("chat-call:{}", self.call.channel);
            <Self as FloatingSurfaceProvider>::on_floating_surface_host_message(
                self,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id },
            );
        }

        // Make the screenshots on either side of shell ownership directly
        // comparable: steady detached state is relaxed; both return frames
        // show the same restrained midpoint deformation.
        let handoff_phase = if matches!(
            scenario,
            "floating-call-returning" | "floating-call-returned"
        ) {
            500
        } else {
            1_000
        };
        self.call
            .placement
            .pin_handoff_for_visual_review(handoff_phase);
    }

    /// Where a message's header and body were last painted, if they were.
    fn message_rect(&self, id: &str) -> Option<Rect> {
        self.hits.iter().find_map(|(rect, hit)| match hit {
            Hit::MessageMenu { msg } if msg.as_str() == id => Some(*rect),
            _ => None,
        })
    }

    fn conversation_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};

        if let Some(peer) = &self.direct_person {
            let rows = self
                .person_messages
                .iter()
                .map(|message| {
                    let author = if &message.author == peer {
                        self.person_display_name(peer)
                    } else {
                        self.me.name.clone()
                    };
                    AccessNode::new(Role::ListItem)
                        .with_name(format!("{author}: {}", message.text))
                        .with_role_description(crate::i18n::t("chat-message-kind"))
                        .with_hashed_key((&self.active_channel, &message.id, "private-message"))
                })
                .collect();
            return AccessNode::new(Role::List)
                .with_name(crate::i18n::t("chat-person-chat-private"))
                .with_children(rows)
                .with_hashed_key(("private-message-list", &self.active_channel));
        }
        let conversation = self
            .messages
            .iter()
            .filter(|message| {
                if self.thread_is_page() {
                    Some(&message.id) == self.thread_open.as_ref()
                        || message.parent.as_ref() == self.thread_open.as_ref()
                } else {
                    message.parent.is_none() || message.parent.as_ref() == self.thread_open.as_ref()
                }
            })
            .map(|message| {
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(self.model_message_label(message))
                    .with_role_description(crate::i18n::t("chat-message-kind"))
                    .with_hashed_key((&message.id, "message"));
                // A painted message is read and picked where it is drawn,
                // not over the whole conversation.
                if let Some(rect) = self.message_rect(&message.id) {
                    node = node.with_bounds(rect);
                }
                if let Some(parts) = self.painted_messages.get(&message.id) {
                    node = node.with_children(parts.access_children(&message.id));
                }
                if let Some(work) = &message.work {
                    node.with_description(crate::render_chat::agent_work_accessible_description(
                        work,
                    ))
                } else {
                    node
                }
            })
            .collect();
        AccessNode::new(Role::List)
            .with_name(crate::i18n::t("chat-messages"))
            .with_interface_name(true)
            .with_children(conversation)
            .with_hashed_key("message-list")
    }

    // --- model lookups ----------------------------------------------------
    pub(crate) fn user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    /// Take the room's view of the participants off the presence store.
    ///
    /// Called once per frame from `draw`. The presence store's participant
    /// kinds come from the host's certified People projection; Chat never
    /// infers Agent identity from a payload kind, display name or id prefix.
    /// Names and colours remain presentation metadata and grant no authority.
    ///
    /// The room's answer **replaces** what was held rather than merging into
    /// it. Merging is how a peer who has gone keeps a name and a colour on
    /// everyone else's screen for the rest of the session: presence forgets
    /// them on leave, and anything downstream that only ever learned would go
    /// on offering a departed participant in the `@` picker and painting their
    /// colour behind a mention.
    /// Take this frame's room: who is here and in which call, their names
    /// and colours, and the authority the window acts under. Whether any of
    /// it changed, so the channels show it (also while the conversation is
    /// put away and not drawn). In a call they show who speaks, which moves
    /// every frame.
    fn take_room(&mut self) -> bool {
        let before = (
            self.room_voice.clone(),
            self.room_names.clone(),
            self.room_colors.clone(),
            self.person_contacts.clone(),
        );
        let permissions = ui::presence::frame::frame(PresenceCtx::permission_context).flatten();
        let perms_changed = permissions.is_some_and(|permissions| {
            let changed = self.perms.context() != &permissions;
            self.perms = ChatPermissions::from_context(permissions);
            changed
        });
        if let Some(capture) = self.capture_agent_presence.clone() {
            self.adopt_room(&capture);
        } else if ui::presence::frame::frame(|presence| self.adopt_room(presence)).is_none() {
            return perms_changed;
        }
        let after = (
            &self.room_voice,
            &self.room_names,
            &self.room_colors,
            &self.person_contacts,
        );
        let changed = perms_changed || (&before.0, &before.1, &before.2, &before.3) != after;
        if changed || self.call.active {
            self.show_sidebar();
        }
        changed
    }

    pub fn adopt_room(&mut self, presence: &PresenceCtx) {
        self.person_contacts = ui::person_chat::contacts();
        self.adopt_own_participant(presence);
        self.room_voice = self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
            .filter_map(|channel| {
                let key = Self::voice_presence_key(&channel.id);
                // As people: a call's roster names each person once, and
                // where presence names their devices, each is its person.
                let mut people: Vec<Id> = Vec::new();
                for (user, _) in presence.carets.get(&key).into_iter().flatten() {
                    let person = presence.person_of(user).unwrap_or(user).0.clone();
                    if !people.contains(&person) {
                        people.push(person);
                    }
                }
                (!people.is_empty()).then(|| (channel.id.clone(), people))
            })
            .collect();
        self.room_agents = presence
            .names
            .keys()
            .filter(|id| presence.participant_kind(id) == ui::presence::ParticipantKind::Agent)
            .map(|id| {
                id.0.strip_prefix("agent:")
                    .unwrap_or(id.0.as_str())
                    .to_owned()
            })
            .collect();
        self.room_agents.sort();
        self.room_agents.dedup();
        self.room_persons.clear();
        self.room_names.retain(|id, _| {
            presence
                .names
                .contains_key(&ui::permissions::UserId(id.clone()))
        });
        self.room_colors.retain(|id, _| {
            presence
                .names
                .contains_key(&ui::permissions::UserId(id.clone()))
        });
        for (id, name) in &presence.names {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            self.room_names.insert(id.0.clone(), name.to_string());
            // A colour is the same kind of answer: assigned per room so two
            // people never collide, and shared with the caret pill and cursor.
            self.room_colors
                .insert(id.0.clone(), presence.color_for(id));
            // Where presence names a person's device, the person (as a
            // call's roster names them) goes by the same name and colour,
            // told apart from a namesake as the device is.
            if let Some(person) = presence.person_of(id) {
                self.room_persons.insert(id.0.clone(), person.0.clone());
                self.room_names
                    .insert(person.0.clone(), self.room_labels.label(&id.0, name));
                self.room_colors
                    .insert(person.0.clone(), presence.color_for(id));
            }
        }
    }

    /// Take the room's answer to "which of these participants am I".
    ///
    /// This window signs what it writes with the id its host gave it, the
    /// author every action of its person carries. A room that spells the
    /// same participant differently names them too: lines recorded under
    /// that spelling are still this person's, so their name stays beside
    /// them.
    fn adopt_own_participant(&mut self, presence: &PresenceCtx) {
        self.room_labels.clone_from(presence.labels());
        self.own_label = presence.self_label();
        if let Some(room_id) = presence.self_id()
            && room_id.0 != self.me.id
            && !self.former_ids.contains(&room_id.0)
        {
            self.former_ids.push(room_id.0.clone());
        }
        if let Some(name) = presence.self_name()
            && name != self.me.name
        {
            self.me.name = name.to_string();
            self.me.initials = crate::identity::initials_for(name);
        }
    }

    /// Whether `id` is this session's own participant, under its own id or
    /// a spelling the room uses for it.
    pub(crate) fn is_me(&self, id: &str) -> bool {
        id == self.me.id || self.former_ids.iter().any(|f| f == id)
    }

    pub(crate) fn agent_display_name(&self, agent: &str) -> String {
        self.room_names
            .get(&format!("agent:{agent}"))
            .or_else(|| self.room_names.get(agent))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| crate::i18n::t("chat-agent-kind"))
    }

    pub(crate) fn conversation_instance(parent: &Self) -> Self {
        let mut view = Self::bare(parent.content.clone());
        view.channels.clone_from(&parent.channels);
        view.categories.clone_from(&parent.categories);
        view.users.clone_from(&parent.users);
        view.messages.clone_from(&parent.messages);
        view.search_messages.clone_from(&parent.search_messages);
        view.search_users.clone_from(&parent.search_users);
        view.voice_members.clone_from(&parent.voice_members);
        view.me = parent.me.clone();
        view.perms = parent.perms.clone();
        view.room_names.clone_from(&parent.room_names);
        view.room_labels.clone_from(&parent.room_labels);
        view.own_label.clone_from(&parent.own_label);
        view.room_colors.clone_from(&parent.room_colors);
        view.room_agents.clone_from(&parent.room_agents);
        view.capture_agent_presence
            .clone_from(&parent.capture_agent_presence);
        view.agent_authors.clone_from(&parent.agent_authors);
        view.former_ids.clone_from(&parent.former_ids);
        view.person_subscription = parent.person_subscription;
        view.direct_person = parent
            .direct_person
            .as_ref()
            .filter(|peer| {
                parent
                    .person_subscription
                    .is_some_and(|token| ui::person_chat::retain_current(peer, token))
            })
            .cloned();
        view.person_contacts.clone_from(&parent.person_contacts);
        view.person_messages.clone_from(&parent.person_messages);
        view.person_error.clone_from(&parent.person_error);
        view.active_channel.clone_from(&parent.active_channel);
        view.conversation_surface = true;
        view.thread_open = None;
        view.right_flap.snap(false);
        view.external_services = parent.external_services;
        if let Some(dispatcher) = parent.dispatch.clone() {
            view.attach_framework(dispatcher);
        }
        if let Some(chain) = parent.chain.clone() {
            view.attach_chain(chain);
        }
        view
    }

    pub(crate) fn float_agent_conversation(&mut self) -> bool {
        let Some(peer) = self.direct_person.clone() else {
            return false;
        };
        if !self.main_composer_can_edit() || self.rect.area() <= 0.0 {
            return false;
        }
        let channel = self.active_channel.clone();
        if let Some(mut surface) = self.agent_surfaces.remove(&channel) {
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            ) {
                surface.move_editor_to_view(self);
                surface.placement.detach_to_desktop(surface.source);
                self.agent_surface_requests.push_back(surface.request());
            }
            self.agent_surfaces.insert(channel, surface);
            self.leave_floated_agent_route();
            return true;
        }
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(self, &peer);
        surface.placement.detach_to_desktop(surface.source);
        self.agent_surface_requests.push_back(surface.request());
        self.agent_surfaces.insert(channel, surface);
        self.leave_floated_agent_route();
        true
    }

    pub(crate) fn leave_floated_agent_route(&mut self) {
        let fallback = self
            .channels
            .iter()
            .filter(|channel| {
                !channel.archived
                    && channel.kind == model::ChannelKind::Text
                    && channel.conversation_with.is_none()
            })
            .min_by_key(|channel| channel.order)
            .map(|channel| channel.id.clone());
        self.close_person_conversation();
        self.thread_open = None;
        if let Some(channel) = fallback {
            self.active_channel = channel;
            self.composer_main.set_key(&self.active_channel);
        } else {
            self.active_channel.clear();
        }
        self.focus = Focus::Main;
        self.scroll = 0.0;
        self.request_relayout();
    }

    pub(crate) fn begin_agent_conversation_drag(&mut self, origin: Point) {
        let Some(peer) = self.direct_person.clone() else {
            return;
        };
        self.agent_conversation_drag = Some(AgentConversationDrag {
            peer,
            channel: self.active_channel.clone(),
            origin,
        });
    }

    pub(crate) fn update_agent_conversation_drag(&mut self, pointer: Point) {
        let Some(drag) = self.agent_conversation_drag.clone() else {
            return;
        };
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self.agent_surfaces.get_mut(&drag.channel) {
            let handoff = surface.placement.update_drag(pointer, self.rect, desktop);
            if handoff == ui::floating_surface::Handoff::Detached {
                self.agent_surface_requests.push_back(surface.request());
            }
            self.request_relayout();
            return;
        }
        if !ui::floating_surface::FloatingPlacement::is_beyond_detach_boundary(pointer, self.rect) {
            return;
        }
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(
                self, &drag.peer,
            );
        surface
            .placement
            .begin_drag(drag.origin, self.rect, desktop);
        if surface.placement.update_drag(pointer, self.rect, desktop)
            != ui::floating_surface::Handoff::Detached
        {
            surface.move_editor_to_parent(self);
            return;
        }
        self.agent_surface_requests.push_back(surface.request());
        self.agent_surfaces.insert(drag.channel, surface);
        self.leave_floated_agent_route();
        self.request_relayout();
    }

    pub(crate) fn end_agent_conversation_drag(&mut self) {
        let Some(drag) = self.agent_conversation_drag.take() else {
            return;
        };
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self.agent_surfaces.get_mut(&drag.channel) {
            surface.placement.end_drag(desktop);
        }
    }

    pub(crate) fn restore_attached_agent_surface(&mut self, channel: &str) {
        let Some(mut surface) = self.agent_surfaces.remove(channel) else {
            return;
        };
        let attached = matches!(
            surface.placement.attachment(),
            ui::floating_surface::Attachment::App { .. }
        );
        let route_is_active = self.active_channel == channel;
        if attached && route_is_active {
            surface.move_editor_to_parent(self);
            self.set_composer_focus();
        }
        // A returned person view is now owned by the parent. Do not retain an
        // invisible attached copy (and its subscription) after navigating away.
        let person_returned = attached && route_is_active;
        if (!surface.is_closed() || !route_is_active) && !person_returned {
            self.agent_surfaces.insert(channel.to_owned(), surface);
        }
    }

    fn restore_retained_agent_route(&mut self, channel: &str) {
        if !self.agent_surfaces.contains_key(channel) {
            return;
        }
        self.close_person_conversation();
        let Some(surface) = self.agent_surfaces.get(channel) else {
            return;
        };
        channel.clone_into(&mut self.active_channel);
        self.person_subscription = surface.view.person_subscription;
        self.direct_person = surface
            .view
            .direct_person
            .as_ref()
            .filter(|peer| {
                surface
                    .view
                    .person_subscription
                    .is_some_and(|token| ui::person_chat::retain_current(peer, token))
            })
            .cloned();
        self.person_contacts
            .clone_from(&surface.view.person_contacts);
        self.person_messages
            .clone_from(&surface.view.person_messages);
        self.thread_open = None;
        self.focus = Focus::Main;
        self.scroll = 0.0;
    }

    fn settle_returned_agent_surface(&mut self, id: &str, closed: bool) {
        let Some(channel) = self
            .agent_surfaces
            .iter()
            .find_map(|(channel, surface)| (surface.id == id).then(|| channel.clone()))
        else {
            return;
        };
        if !closed {
            self.restore_retained_agent_route(&channel);
        }
        if closed && let Some(surface) = self.agent_surfaces.get_mut(&channel) {
            surface.close();
        }
        if self.focused_agent_surface.as_deref() == Some(id) {
            self.focused_agent_surface = None;
        }
        if self.input_agent_surface.as_deref() == Some(id) {
            self.input_agent_surface = None;
        }
        self.restore_attached_agent_surface(&channel);
    }

    pub(crate) fn settle_agent_surface_commands(&mut self) {
        let requested = self
            .agent_surfaces
            .iter_mut()
            .find_map(|(channel, surface)| {
                std::mem::take(&mut surface.view.conversation_return_requested)
                    .then(|| (channel.clone(), surface.id.clone()))
            });
        let Some((channel, id)) = requested else {
            return;
        };
        if self
            .agent_surfaces
            .get(&channel)
            .is_some_and(|surface| surface.id == id)
            && let Some(surface) = self.agent_surfaces.get_mut(&channel)
        {
            surface.placement.request_app_return();
        }
    }

    pub(crate) fn main_composer_can_edit(&self) -> bool {
        if let Some(peer) = &self.direct_person {
            return self
                .person_contacts
                .iter()
                .any(|contact| &contact.public_key == peer)
                && self.person_error.is_none();
        }
        self.can_participate()
    }

    /// What to call `id`: the room's answer first, the document's stored name
    /// as an offline fallback, and the raw id last so a label is never blank.
    /// People who share a name carry the number the room tells them apart
    /// by ("Ada (2)"), this window's own person included.
    ///
    /// The precedence is the point. A client that could name itself would be
    /// telling every other peer what to call it, and two peers disagreeing
    /// about a third's name is a difference nobody can settle. So the room
    /// answers, and a peer only ever supplies its id.
    pub fn display_name(&self, id: &str) -> String {
        if let Some(agent) = id.strip_prefix("agent:")
            && self.room_agents.iter().any(|known| known == agent)
        {
            return self.agent_display_name(agent);
        }
        if self.is_me(id)
            && let Some(own) = &self.own_label
        {
            return own.clone();
        }
        if let Some(n) = self.room_names.get(id)
            && !n.trim().is_empty()
        {
            let n = self.room_labels.label(id, n);
            return if self.is_me(id) {
                n
            } else {
                crate::identity::shared_name_label(id, &n)
            };
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return agent.clone();
        }
        if let Some(u) = self.users.iter().find(|u| u.id == id)
            && !u.name.trim().is_empty()
        {
            let name = self.room_labels.label(id, &u.name);
            return if self.is_me(id) {
                name
            } else {
                crate::identity::shared_name_label(id, &name)
            };
        }
        // Your own name, which this had no way to reach until the room named
        // it: `me` is not necessarily a row in `users`, which is built from
        // the room document, and a room this peer has not spoken in has
        // never heard of it.
        if self.is_me(id) && !self.me.name.trim().is_empty() {
            return self.me.name.clone();
        }
        id.to_string()
    }

    /// What to call `id` on screen: this session's own participant by their
    /// name like anyone else, and an honest placeholder for someone the room
    /// has not named yet — nobody is ever shown their raw routing id.
    pub(crate) fn user_name(&self, id: &str) -> String {
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // room's address for itself still wrote its earlier lines under the
        // id it resolved at startup, and those are still its own. Their own
        // name, as everyone else reads it.
        if self.is_me(id) {
            let own = self.display_name(id);
            if own != id && !own.trim().is_empty() {
                return own;
            }
            return crate::identity::unnamed_label(id);
        }
        if let Some(agent) = id.strip_prefix("agent:")
            && self.room_agents.iter().any(|known| known == agent)
        {
            return crate::i18n::t_args(
                "chat-agent-author",
                &[("name", self.agent_display_name(agent).into())],
            );
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return format!("{agent} (agent)");
        }
        let named = self.display_name(id);
        // A name that happens to equal the id is still a name when the
        // document stores it (a browser mount's author is both).
        let stored = self
            .users
            .iter()
            .any(|u| u.id == id && u.name.trim() == named.trim());
        if (named != id || stored) && !named.trim().is_empty() {
            return crate::identity::shared_name_label(id, &named);
        }
        crate::identity::unnamed_label(id)
    }

    /// The avatar monogram for `id`, derived from whatever the room calls them
    /// so the letter never disagrees with the name beside it.
    pub fn initials_for(&self, id: &str) -> String {
        let name = self.display_name(id);
        let s = ui::widget::initials_for_name(&name);
        if s.is_empty() {
            name.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
        } else {
            s
        }
    }

    /// The participant's own colour, in the shared presence vocabulary: the
    /// room's assignment when it made one, otherwise the system-wide
    /// per-user colour every other surface derives the same way.
    pub fn participant_color(&self, id: &str) -> (u8, u8, u8) {
        self.room_colors
            .get(id)
            .copied()
            .unwrap_or_else(|| ui::presence::user_color(id))
    }

    /// The chat's people as the composers' `@` lists offer them beside the
    /// room's roster: someone who wrote here stays mentionable after they
    /// left the room.
    pub(crate) fn mention_people(&self) -> Vec<ui::widget::MentionCandidate> {
        self.users
            .iter()
            .map(|user| ui::widget::MentionCandidate {
                id: user.id.clone(),
                name: self.display_name(&user.id),
                kind: ui::presence::ParticipantKind::from(user.is_agent),
                color: self.participant_color(&user.id),
                is_self: self.is_me(&user.id),
            })
            .collect()
    }

    /// Everyone a message's `@name`s bind to when it is sent: the
    /// document's own users, plus anyone the room is talking about who has
    /// not yet written themselves into it.
    pub fn participants(&self) -> Vec<crate::mention::Participant> {
        let mut out: Vec<crate::mention::Participant> = self
            .users
            .iter()
            .map(|u| crate::mention::Participant {
                id: u.id.clone(),
                name: self.display_name(&u.id),
                is_agent: u.is_agent,
            })
            .collect();
        // Each person once: a device stands for its person, so neither the
        // person's alias in `room_names` nor a device whose person already
        // wrote themselves in is a second candidate.
        let aliases: std::collections::HashSet<&Id> = self.room_persons.values().collect();
        for id in self.room_names.keys() {
            let person_listed = self
                .room_persons
                .get(id)
                .is_some_and(|person| out.iter().any(|p| &p.id == person));
            if !aliases.contains(id) && !person_listed && !out.iter().any(|p| &p.id == id) {
                out.push(crate::mention::Participant {
                    id: id.clone(),
                    name: self.display_name(id),
                    is_agent: false,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub(crate) fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
    pub(crate) fn message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// The room's channels, ordered. Archived channels are hidden from the
    /// sidebar (they stay in the document and are reachable via Ctrl+K).
    pub(crate) fn ws_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self.channels.iter().filter(|c| !c.archived).collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// The room's categories, ordered.
    pub(crate) fn ws_categories(&self) -> Vec<&crate::model::Category> {
        let mut v: Vec<&crate::model::Category> = self.categories.iter().collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// The categories the sidebar lists: each one with a channel in it.
    /// Someone who keeps the room's layout sees the empty ones too, since
    /// that is where they add a category's first channel; to everyone else
    /// an empty heading is only noise between the channels.
    pub(crate) fn sidebar_categories(&self) -> Vec<&crate::model::Category> {
        let keeps_layout = self.keeps_layout();
        self.ws_categories()
            .into_iter()
            .filter(|category| keeps_layout || !self.category_channels(&category.id).is_empty())
            .collect()
    }

    /// Cached voice members of `channel` (empty when nobody is connected).
    pub(crate) fn voice_in(&self, channel: &str) -> &[crate::model::VoiceMember] {
        self.voice_members
            .get(channel)
            .map_or(&[], std::vec::Vec::as_slice)
    }

    /// Channels in `category`, ordered. Archived channels are hidden from the
    /// sidebar.
    pub(crate) fn category_channels(&self, category: &str) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.category == category && !c.archived)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// The composer for the surface the user is typing in.
    pub(crate) fn focused_composer(&self) -> &crate::composer::DraftComposer {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &self.composer_thread,
            _ => &self.composer_main,
        }
    }

    pub(crate) fn focused_composer_mut(&mut self) -> &mut crate::composer::DraftComposer {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &mut self.composer_thread,
            _ => &mut self.composer_main,
        }
    }

    /// Point keyboard focus at the composer for the current surface — but only
    /// when no inline editor / rename is capturing input. Idempotent; called
    /// from `reload` and after any focus change.
    pub(crate) fn set_composer_focus(&mut self) {
        let editing = self.editing.is_some() || self.channel_sheet.is_some() || !self.host_focused;
        // Without an open channel the main composer has nowhere to send, so
        // it takes no keys.
        let has_target = self.channel(&self.active_channel).is_some();
        let main = matches!(self.focus, Focus::Main) && !editing && has_target;
        let thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some() && !editing;
        self.composer_main.set_focus(main);
        self.composer_thread.set_focus(thread);
        self.edit_input
            .set_focus(self.host_focused && self.editing.is_some());
    }

    /// Presence scope of the composer of a channel or thread, so the others
    /// there see who is writing. The draft itself is each writer's own.
    pub(crate) fn draft_presence_key(draft_key: &str) -> ui::presence::PresenceKey {
        ui::presence::PresenceKey::Widget {
            path: vec![
                ui::presence::PathSegment::Field("chat_draft".into()),
                ui::presence::PathSegment::Field(draft_key.to_owned()),
            ],
        }
    }

    /// Presence scope of a voice channel: who is in its call.
    pub(crate) fn voice_presence_key(channel: &str) -> ui::presence::PresenceKey {
        ui::presence::PresenceKey::Widget {
            path: vec![
                ui::presence::PathSegment::Field("chat_voice".into()),
                ui::presence::PathSegment::Field(channel.to_owned()),
            ],
        }
    }

    /// Who is in voice channel `channel`: this person first while they are,
    /// then everyone the room's presence places there.
    /// Whether this person is in the call now. The call panel stays open
    /// after the call ended for a reason, to say why; then they are no
    /// longer in it, for their own count and for everyone else's list.
    pub(crate) fn in_call(&self) -> bool {
        self.call.active && !self.native_voice.ended_for_a_reason()
    }

    pub(crate) fn voice_people(&self, channel: &str) -> Vec<Id> {
        let mut people = Vec::new();
        if self.in_call() && self.call.channel == channel {
            people.push(self.me.id.clone());
        }
        // What the room's presence says, and who the room's call counts in.
        let present = self.room_voice.get(channel).into_iter().flatten();
        let counted = self.voice_in(channel).iter().map(|member| &member.user);
        for person in present.chain(counted) {
            if !self.is_me(person) && !people.contains(person) {
                people.push(person.clone());
            }
        }
        people
    }

    /// The focused composer's scope while the person is writing something
    /// in it (not an inline edit or rename): that they are, never what or
    /// where in it.
    pub(crate) fn composer_presence(
        &self,
    ) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        if !self.host_focused
            || self.editing.is_some()
            || !self.can_participate()
            || self.direct_person.is_some()
        {
            return None;
        }
        let (composer, key) = match self.focus {
            Focus::Thread => (&self.composer_thread, self.thread_draft_key()?),
            Focus::Main => (&self.composer_main, self.active_channel.clone()),
        };
        if key.is_empty() || composer.value().trim().is_empty() {
            return None;
        }
        Some((
            Self::draft_presence_key(&key),
            ui::PresencePayload::Caret { pos: 0 },
        ))
    }

    /// A text field's focus, value and selection (anchor, caret), for the
    /// hit that stands for it.
    fn text_field_access(&self, hit: &Hit) -> Option<(bool, String, (usize, usize))> {
        match hit {
            Hit::FocusMain | Hit::FocusThread => {
                let composer = if matches!(hit, Hit::FocusMain) {
                    &self.composer_main
                } else {
                    &self.composer_thread
                };
                Some((
                    composer.is_focused(),
                    composer.value(),
                    composer.selection(),
                ))
            }
            Hit::FocusEdit => Some((
                self.edit_input.is_focused(),
                self.edit_input.value(),
                self.edit_input.selection(),
            )),
            _ => None,
        }
    }

    pub(crate) fn thread_draft_key(&self) -> Option<String> {
        self.thread_open
            .as_ref()
            .map(|p| format!("{}::t::{}", self.active_channel, p))
    }

    /// Rebuild the snapshot from the CRDT. Called at the tail of `sync` and
    /// after every mutation so `draw` always reflects the latest document.
    /// Count each channel's unread mentions of this peer, the badge that
    /// says someone is waiting on you, not just talking, and tell about new
    /// ones in a live window.
    fn note_mentions(&mut self, per_channel: &[(Id, Vec<Message>)]) {
        let me: Vec<&str> = std::iter::once(self.me.id.as_str())
            .chain(self.former_ids.iter().map(String::as_str))
            .collect();
        let mentions: HashMap<Id, u32> = per_channel
            .iter()
            .filter(|(id, _)| *id != self.active_channel)
            .map(|(id, messages)| {
                let unread = self
                    .channels
                    .iter()
                    .find(|c| c.id == *id)
                    .map_or(0, |c| usize::try_from(c.unread).unwrap_or(usize::MAX));
                (id.clone(), unread_mentions(messages, unread, &me))
            })
            .collect();
        for c in &mut self.channels {
            c.mentions = mentions.get(&c.id).copied().unwrap_or_default();
        }
        if let Some(mut alerter) = self.mention_alerts.take() {
            let named: Vec<(Id, String, &[Message])> = per_channel
                .iter()
                .map(|(id, messages)| {
                    let name = self
                        .channels
                        .iter()
                        .find(|c| c.id == *id)
                        .map_or_else(String::new, |c| c.name.clone());
                    (id.clone(), name, messages.as_slice())
                })
                .collect();
            let reading = self.host_focused.then_some(self.active_channel.as_str());
            let now = ui::tilo_resource::origin_now_ms() / 1000;
            let alerts = alerter.take_new(&named, &me, (reading, now), |id| self.display_name(id));
            alerter.show(&alerts);
            self.mention_alerts = Some(alerter);
        }
    }

    /// Open the message a mention notification was about: the channel it was
    /// said in, and the message itself where it is loaded. A message no longer
    /// in view cannot be revealed, so the channel opens at its newest edge and
    /// the reason is the only thing missing.
    fn open_mention(&mut self, channel: &str, message: &str) -> bool {
        if self.reveal_message(message).is_ok() {
            return true;
        }
        if !self
            .channels
            .iter()
            .any(|candidate| candidate.id == channel)
        {
            return false;
        }
        channel.clone_into(&mut self.active_channel);
        self.thread_open = None;
        self.revealed_message = None;
        self.reset_timeline_to_latest();
        self.reselect();
        true
    }

    pub(crate) fn reload(&mut self, doc: &AutoCommit) {
        self.read_kept_messages(doc);
        // A document that does not read is not an empty chat: the channels,
        // messages and drafts that last read whole stay, and every write
        // derived from this projection waits for a read that succeeds.
        let Some(chat) = self.chat_read.adopt(crate::projection::try_read(doc)) else {
            return;
        };
        #[cfg(test)]
        {
            *self.last_read.borrow_mut() = std::rc::Rc::new(chat.clone());
        }
        let held = self.held_messages();
        self.chain_seen = self.chain.as_ref().map(ui::framework::Chain::revision);
        self.channels = chat.channel_rows();
        self.categories = chat.category_rows();
        for category in &mut self.categories {
            category.collapsed = self.collapsed.contains(&category.id);
        }
        // Keep the active channel valid (e.g. after it was deleted).
        if self.direct_person.is_none()
            && self.channel(&self.active_channel).is_none()
            && let Some(first) = self
                .ws_channels()
                .into_iter()
                .find(|c| c.kind == model::ChannelKind::Text)
                .map(|c| c.id.clone())
        {
            self.active_channel = first;
        }
        // Unread is per-peer derived state, not stored: the channel in view is
        // always read (0); every other channel shows its top-level messages
        // beyond this peer's read watermark. The watermark itself is advanced
        // by `mark_active_read` while a channel is viewed / on open.
        for c in &mut self.channels {
            c.unread = if c.id == self.active_channel {
                0
            } else {
                chat.unread(&held, &c.id, &self.me.id)
            };
        }
        // Read every channel's messages once per reload: the open channel,
        // search and the room's people directory all project from them
        // (reading them three times dominated typing with a long history).
        let per_channel: Vec<(Id, Vec<Message>)> = self
            .channels
            .iter()
            .map(|channel| (channel.id.clone(), chat.message_rows(&held, &channel.id)))
            .collect();
        self.note_mentions(&per_channel);
        self.messages = per_channel
            .iter()
            .find(|(id, _)| *id == self.active_channel)
            .map_or_else(
                || chat.message_rows(&held, &self.active_channel),
                |(_, messages)| messages.clone(),
            );
        self.search_messages = per_channel
            .iter()
            .flat_map(|(_, messages)| messages.iter().cloned())
            .collect();
        self.search_users = chat.user_rows();
        self.reports = chat
            .feedback
            .iter()
            .filter_map(|(id, report)| {
                Some((
                    id.to_string(),
                    crate::feedback::message::message(report)?.text,
                ))
            })
            .collect();
        // Drop the body views of messages no longer loaded.
        let shown: HashSet<&str> = self.messages.iter().map(|m| m.id.as_str()).collect();
        self.body_views
            .borrow_mut()
            .retain(|(_, id), _| shown.contains(id.as_str()));
        self.agent_authors = chat.agent_authors();
        self.room_files = chat.files;
        // Who is in a call is projected only from the authenticated local
        // voice service's response.
        self.refresh_native_streams();
        self.users = self.room_people_from(&chat, &per_channel);
        self.sync_reload_composers(doc);
    }

    fn sync_reload_composers(&mut self, doc: &AutoCommit) {
        // Point each composer at its surface's draft; the editors read it on
        // their next sync (`sync_composers`), keeping the caret while typing.
        if self.direct_person.is_some() {
            self.sync_person_draft();
        } else {
            self.composer_main.set_key(&self.active_channel);
            self.composer_main.sync(doc);
            self.draft_main = self.composer_main.value();
        }
        let channel = self
            .channel(&self.active_channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.composer_main
            .set_placeholder(&format!("Message #{channel}"));
        if self.direct_person.is_some() {
            self.composer_main
                .set_placeholder(&crate::i18n::t("chat-person-chat-placeholder"));
        }
        self.composer_main
            .set_local_author(Some(self.me.id.clone()));
        let thread_key = self.thread_draft_key().unwrap_or_default();
        self.composer_thread.set_key(&thread_key);
        self.composer_thread.sync(doc);
        self.draft_thread = if thread_key.is_empty() {
            String::new()
        } else {
            self.composer_thread.value()
        };
        self.composer_thread
            .set_placeholder(&crate::i18n::t("chat-thread-reply-placeholder"));
        self.composer_thread
            .set_local_author(Some(self.me.id.clone()));
        self.composer_main
            .set_read_only(!self.main_composer_can_edit());
        self.composer_thread.set_read_only(!self.can_participate());
        self.set_composer_focus();
    }

    /// Bring both composers up to date with their drafts. `--shots` stages
    /// its mid-compose draft here.
    pub(crate) fn sync_composers(&mut self, doc: &AutoCommit) {
        for composer in [&mut self.composer_main, &mut self.composer_thread] {
            composer.sync(doc);
        }
        self.draft_main = self.composer_main.value();
        self.draft_thread = self.composer_thread.value();
    }

    /// Project the document's identity directory onto evidence in the room's
    /// channels. Channel activity and voice membership are durable evidence;
    /// the local identity is included so a brand-new room remains a valid
    /// one-person surface.
    fn room_people_from(
        &self,
        chat: &crate::record::Chat,
        per_channel: &[(Id, Vec<Message>)],
    ) -> Vec<User> {
        let mut ids: HashSet<Id> = HashSet::from([self.me.id.clone()]);
        ids.extend(self.former_ids.iter().cloned());

        for channel in &self.channels {
            let messages = per_channel
                .iter()
                .find(|(id, _)| *id == channel.id)
                .map(|(_, messages)| messages);
            for message in messages.into_iter().flatten() {
                ids.insert(message.author.clone());
                for reaction in &message.reactions {
                    ids.extend(reaction.reactors.iter().map(|reactor| reactor.by.clone()));
                }
                ids.extend(message.mentions.iter().map(|mention| mention.user.clone()));
            }
        }

        let mut people: Vec<_> = chat
            .user_rows()
            .into_iter()
            .filter(|user| ids.contains(&user.id))
            .collect();
        for (id, name) in &self.kept_names {
            if ids.contains(id) && !people.iter().any(|user| user.id == *id) {
                people.push(User {
                    id: id.clone(),
                    name: name.clone(),
                    initials: ui::widget::initials_for_name(name),
                    color_idx: 0,
                    status: model::Presence::Offline,
                    is_agent: false,
                });
            }
        }
        if !people.iter().any(|user| self.is_me(&user.id)) {
            people.push(User {
                id: self.me.id.clone(),
                name: self.me.name.clone(),
                initials: self.me.initials.clone(),
                color_idx: self.me.color_idx,
                status: model::Presence::Online,
                is_agent: false,
            });
        }
        people.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        people
    }

    /// Announce this window's person into the shared roster once, under the
    /// name this device knows them by: what every member shows for them
    /// until the room names them. A repeat writes nothing.
    fn announce_me(&mut self, doc: &AutoCommit) {
        let Some(me) = self.announce.take() else {
            return;
        };
        // Introducing this person is derived from the roster the document
        // holds: an unreadable document says nothing about who is already in
        // the room, so nothing is written on its word.
        let Some(chat) = crate::projection::read(doc) else {
            self.announce = Some(me);
            return;
        };
        let color = u32::try_from(me.color_idx).unwrap_or_default();
        if (chat.channels.is_empty() && chat.people.is_empty())
            || chat.people.iter().any(|person| {
                person.id.0 == me.id
                    && person.name == me.name
                    && person.initials == me.initials
                    && person.color == color
                    && person.status == model::Presence::Online
            })
        {
            return;
        }
        self.submit(
            crate::resource::Chat::INTRODUCE.with(crate::record::chat::Introduce {
                name: me.name,
                initials: me.initials,
                color,
            }),
        );
    }

    /// Advance the local read watermark for the channel currently in view so
    /// its unread badge stays cleared and doesn't reappear once the user
    /// navigates away. Idempotent (a no-op when nothing new arrived), so it is
    /// safe to call every frame.
    pub(crate) fn mark_active_read(&mut self, doc: &AutoCommit) {
        if self.direct_person.is_some() || self.active_channel.is_empty() || !self.timeline_at_end {
            return;
        }
        let Some(chat) = crate::projection::read(doc) else {
            return;
        };
        let read = self
            .held_messages()
            .read_to_end(&ui::tilo_resource::Id::from_raw(
                self.active_channel.clone(),
            ));
        if read <= chat.read_mark(&self.active_channel, &self.me.id) {
            return;
        }
        self.note_refused_read();
        if self
            .read_refused
            .as_ref()
            .is_some_and(|(channel, refused)| *channel == self.active_channel && read <= *refused)
        {
            return;
        }
        // Read state is a durable collaboration fact.  It is authored through
        // the same door as the palette's "Mark as read", not a side write
        // that peers/history cannot account for.  The carried position keeps
        // a message that arrives meanwhile unread.  Nobody pressed anything
        // for it, so a refusal is not said over the window.
        self.read_asked = Some((self.active_channel.clone(), read));
        self.submit_on_its_own(crate::resource::Chat::MARK_READ.with(
            crate::record::chat::MarkRead {
                channel: ui::tilo_resource::Id::from_raw(self.active_channel.clone()),
                part: u64::try_from(read.part).unwrap_or_default(),
                seen: u32::try_from(read.seen).unwrap_or(u32::MAX),
            },
        ));
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|channel| channel.id == self.active_channel)
        {
            channel.unread = 0;
        }
    }

    pub(crate) fn reveal_category_resource(&mut self, category: &str) -> Result<(), String> {
        let Some(category_id) = self
            .categories
            .iter()
            .find(|item| item.id == category)
            .map(|item| item.id.clone())
        else {
            return Err("category no longer exists".into());
        };
        self.active_channel = self
            .channels
            .iter()
            .filter(|channel| {
                channel.category == category_id && channel.kind == model::ChannelKind::Text
            })
            .min_by_key(|channel| channel.order)
            .map_or_else(String::new, |channel| channel.id.clone());
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        Ok(())
    }

    /// The members roster shown in the right flap when no thread is open: every
    /// user in the room paired with the voice channel they're currently in
    /// (if any). Sorted in-call first, then by presence, then by name. Real
    /// projection of the shared user list + voice membership — no placeholders.
    pub(crate) fn roster_rows(&self) -> Vec<(User, Option<String>)> {
        // user id → the name of the voice channel they're connected to.
        let mut in_voice: HashMap<Id, String> = HashMap::new();
        for channel in self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
        {
            for person in self.voice_people(&channel.id) {
                in_voice.insert(person, channel.name.clone());
            }
        }
        let mut rows: Vec<(User, Option<String>)> = self
            .users
            .iter()
            .cloned()
            .map(|mut u| {
                u.status = self.live_status(&u);
                let voice = in_voice.get(u.id.as_str()).cloned();
                (u, voice)
            })
            .collect();
        let rank = |u: &User, voice: &Option<String>| -> u8 {
            if voice.is_some() {
                0
            } else {
                match u.status {
                    crate::model::Presence::Online => 1,
                    crate::model::Presence::Idle => 2,
                    crate::model::Presence::Dnd => 3,
                    crate::model::Presence::Offline => 4,
                }
            }
        };
        rows.sort_by(|a, b| {
            rank(&a.0, &a.1)
                .cmp(&rank(&b.0, &b.1))
                .then_with(|| a.0.name.cmp(&b.0.name))
        });
        // This session's person once, whichever of its spellings the
        // document recorded them under.
        let mut me_listed = false;
        rows.retain(|(person, _)| {
            if !self.is_me(&person.id) {
                return true;
            }
            !std::mem::replace(&mut me_listed, true)
        });
        rows
    }

    /// Whether `person` is here now. The record keeps the status someone
    /// introduced themselves with, and nobody writes "Offline" when a tab
    /// closes, so a departed visitor would read "Online" forever. While the
    /// room answers who is present, that answer decides: present people keep
    /// the away or busy they set, everyone else is offline. Without a room
    /// (a chat on this device alone) the recorded status stands.
    pub(crate) fn live_status(&self, person: &User) -> model::Presence {
        if self.room_names.is_empty() {
            return person.status;
        }
        let present = self.is_me(&person.id) || self.room_names.contains_key(&person.id);
        match (present, person.status) {
            (false, _) => model::Presence::Offline,
            (true, model::Presence::Offline) => model::Presence::Online,
            (true, status) => status,
        }
    }

    /// Whether this room has somebody besides the local participant. An
    /// agent is still a collaborator (and should surface collaboration
    /// controls), but it is never counted or labelled as a person.
    pub(crate) fn has_other_collaborator(&self) -> bool {
        self.users.iter().any(|u| !self.is_me(&u.id))
    }

    /// The roster's public destination name. `People` is precise when every
    /// entry is human; once an agent is present, `Collaborators` accurately
    /// covers both and the row itself keeps the agent visibly typed.
    pub(crate) fn roster_label(&self) -> &'static str {
        if self.users.iter().any(|u| u.is_agent) {
            "Collaborators"
        } else {
            "People"
        }
    }

    // --- window geometry ---------------------------------------------------
    //
    // The shell keeps the room's channels in its sidebar (`resource.rs`);
    // this window is the conversation beside them, and the detail drawer
    // (people, or the open thread) at its trailing edge. The toolkit's
    // `SidebarDrawer` decides, from the width, whether that drawer docks
    // beside the conversation or floats over it behind a scrim.

    /// The trailing detail drawer, summoned on demand. It docks while the
    /// conversation beside it keeps its minimum width.
    fn detail_drawer() -> SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        SidebarDrawer::new()
            .right()
            .closed()
            .docked_width(f64::from(m.inspector_width))
            .overlay_width(f64::from(m.inspector_width))
            .breakpoint(f64::from(m.content_min_width + m.inspector_width))
    }

    /// True when the shell's sidebar, the channels, shows.
    pub(crate) fn sidebar_open(&self) -> bool {
        self.bar
            .as_ref()
            .is_some_and(WindowBarLink::is_sidebar_open)
    }
    /// True when the detail drawer (people / thread) shows.
    pub(crate) fn right_flap_open(&self) -> bool {
        self.right_flap.is_open()
    }
    /// The detail drawer floats over the conversation.
    pub(crate) fn right_overlay(&self) -> bool {
        self.right_flap.is_overlay()
    }

    /// The conversation column: what the docked detail drawer leaves.
    pub(crate) fn content_rect(&self) -> Rect {
        let content = self.right_flap.content_rect();
        if content.width() > 0.0 {
            content
        } else {
            self.rect
        }
    }
    pub(crate) fn center_left(&self) -> f64 {
        self.content_rect().x0
    }
    pub(crate) fn center_right(&self) -> f64 {
        self.content_rect().x1
    }
    /// Reserve the height measured by the same editor that will be drawn.
    pub(crate) fn composer_height(&self) -> f64 {
        let chrome = &self.chrome;
        let pad = chrome.composer_min_h - chrome.body_line;
        (self.composer_main.measured_height() + pad)
            .clamp(chrome.composer_min_h, chrome.composer_max_h)
    }
    pub(crate) fn composer_rect(&self) -> Rect {
        let pad = self.chrome.composer_pad;
        // The note strip under the field is always kept, so a line appearing
        // in it never moves the field.
        let bottom = self.rect.y1 - pad.max(self.chrome.composer_note_h);
        let top_limit = self.rect.y0 + pad;
        let h = self
            .composer_height()
            .min((bottom - top_limit).max(self.chrome.composer_min_h));
        Rect::new(
            self.center_left() + self.chrome.gutter,
            bottom - h,
            self.center_right() - self.chrome.gutter,
            bottom,
        )
    }

    /// The thread's composer: the same control and height rule as the main
    /// composer, at the bottom of the thread panel `panel`.
    pub(crate) fn thread_composer_rect(&self, panel: Rect) -> Rect {
        let pad = self.chrome.composer_pad;
        let chrome = &self.chrome;
        let inset = chrome.composer_min_h - chrome.body_line;
        let h = (self.composer_thread.measured_height() + inset)
            .clamp(chrome.composer_min_h, chrome.composer_max_h);
        let bottom = panel.y1 - pad.max(chrome.composer_note_h);
        Rect::new(panel.x0 + pad, bottom - h, panel.x1 - pad, bottom)
    }
    /// Whether this window draws the floating call window over its content.
    /// At enlarged text the labelled Call control remains reachable in the
    /// header instead; a fixed floating huddle would cover the thread and
    /// composer instead of forming a usable secondary surface.
    pub(crate) fn app_draws_huddle(&self) -> bool {
        self.call.active
            && !self.call.expanded
            && self.layout_text_scale <= 1.25
            && ((!self.call.placement.is_return_pending()
                && matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::App { .. }
                ))
                || !self.call.placement.is_desktop_covered())
    }

    /// The timeline: from the top of the conversation to the composer; the
    /// conversation's name and controls are the window bar's. While the call
    /// window floats over it, the messages keep clear of it and wrap beside
    /// it, so it never covers what is being said.
    pub(crate) fn messages_rect(&self) -> Rect {
        let composer = self.composer_rect();
        let mut rect = Rect::new(
            self.center_left(),
            self.rect.y0,
            self.center_right(),
            composer.y0 - self.chrome.avatar_gap,
        );
        if self.app_draws_huddle() {
            let call = self.huddle_panel();
            let overlaps = call.x0 < rect.x1 && call.x1 > rect.x0 && call.y0 < rect.y1;
            if overlaps {
                rect.x1 = rect.x1.min(call.x0 - self.chrome.avatar_gap).max(rect.x0);
            }
        }
        rect
    }
    /// The detail drawer's panel. A thread is content, not a secondary
    /// sidebar: on a narrow window it becomes a pushed page over the whole
    /// window rather than a cramped floating panel.
    pub(crate) fn thread_rect(&self) -> Rect {
        if self.right_overlay() && self.thread_open.is_some() {
            return self.rect;
        }
        self.right_flap.sidebar_rect()
    }
    /// The open thread fills the window as a pushed page.
    pub(crate) fn thread_is_page(&self) -> bool {
        self.right_overlay() && self.thread_open.is_some() && self.right_flap_open()
    }

    /// Resolve the detail drawer against the window: docked or floating, and
    /// the rect it leaves the conversation. Cheap and deterministic, so the
    /// event path calls it too. Returns true while it slides.
    pub(crate) fn reconcile_flaps(&mut self) -> bool {
        if self.rect.width() < 1.0 {
            return false;
        }
        self.resolve_detail_drawer(self.rect);
        self.flap_animating = self.right_flap.is_animating();
        self.flap_animating
    }

    /// Resolve the detail drawer against `content`, the conversation.
    /// Floating over the conversation, pins and
    /// people end above the composer (and so does the scrim that dismisses
    /// them), so its emoji and Send stay in reach; a thread is a page of its
    /// own there and takes the whole window.
    fn resolve_detail_drawer(&mut self, content: Rect) {
        self.right_flap.resolve(content);
        if !self.right_flap.is_overlay() || self.thread_open.is_some() {
            return;
        }
        if let Some(top) = self.composer_top
            && top > content.y0
            && top < content.y1
        {
            self.right_flap
                .resolve(Rect::new(content.x0, content.y0, content.x1, top));
        }
    }

    /// Adopt `theme`'s metrics and advance the detail drawer's reveal for
    /// this frame (once per paint).
    pub(crate) fn advance_flaps(&mut self, theme: &Theme) {
        self.chrome = Chrome::of(theme);
        self.right_flap.scale_for(theme);
        if !self.reconcile_flaps() && self.rect.width() < 1.0 {
            return;
        }
        self.resolve_detail_drawer(self.rect);
        self.right_flap.tick(theme);
        self.flap_animating = self.right_flap.is_animating();
    }

    /// A context menu over `target` whose rows say what they do to it.
    pub(crate) fn context_menu(
        &self,
        target: MenuTarget,
        anchor: Point,
        items: Vec<MenuAction>,
    ) -> ContextMenu {
        let collapsed = match &target {
            MenuTarget::Category(id) => self
                .categories
                .iter()
                .any(|category| category.id == *id && category.collapsed),
            _ => false,
        };
        let pinned = match &target {
            MenuTarget::Message(id) => self.message(id).is_some_and(|m| m.pinned),
            _ => false,
        };
        let channel = matches!(target, MenuTarget::Channel(_));
        let listening = match &target {
            MenuTarget::CallParticipant(id) => Some(self.call_listening(id)),
            _ => None,
        };
        let checked = move |action: MenuAction| {
            let listening = listening?;
            match action {
                MenuAction::Silence => Some(listening.silenced),
                MenuAction::Volume(percent) => {
                    Some(!listening.silenced && listening.percent == percent)
                }
                _ => None,
            }
        };
        let label = move |action: MenuAction| match action {
            MenuAction::Silence => crate::i18n::t("chat-call-silence"),
            MenuAction::Volume(percent) => {
                crate::i18n::t_args("chat-call-volume", &[("percent", percent.into())])
            }
            MenuAction::ToggleCollapse if collapsed => crate::i18n::t("chat-menu-expand-category"),
            MenuAction::PinMessage if pinned => crate::i18n::t("chat-menu-unpin-message"),
            MenuAction::Delete if channel => crate::i18n::t("chat-menu-delete-channel"),
            MenuAction::Delete => crate::i18n::t("chat-menu-delete-category"),
            MenuAction::Rename if channel => crate::i18n::t("chat-menu-rename-channel"),
            MenuAction::Rename => crate::i18n::t("chat-menu-rename-category"),
            other => other.label(),
        };
        ContextMenu::labelled(target, anchor, items, self.menu_bounds(), label, checked)
    }

    /// Where a menu may open: the conversation, and the sidebar beside it
    /// whose rows a menu also opens from.
    fn menu_bounds(&self) -> Rect {
        self.places
            .list_rect()
            .filter(|list| list.width() > 0.0 && self.sidebar_open())
            .map_or(self.rect, |list| list.union(self.rect))
    }

    /// Keep the active channel and thread pointing at something that exists
    /// — used after seeding a fresh (real) room whose ids differ from the
    /// demo defaults, and after any structural delete. Returns true when the
    /// active channel changed (the caller must reload so the message list
    /// refreshes).
    pub(crate) fn ensure_selection(&mut self) -> bool {
        let channel_changed = self.direct_person.is_none()
            && !self.active_channel.is_empty()
            && self.channel(&self.active_channel).is_none();
        if channel_changed {
            self.active_channel.clear();
        }
        // Drop a stale open thread whose parent isn't loaded.
        if let Some(t) = self.thread_open.clone()
            && self.message(&t).is_none()
        {
            self.thread_open = None;
            if self.focus == Focus::Thread {
                self.focus = Focus::Main;
            }
        }
        channel_changed
    }

    /// The Design Huddle bridged to a Discord voice channel: two people
    /// there in the call beside the room's, one of them talking.
    fn stage_bridged_call(&mut self) {
        let guest = |account: &str, name: &str, avatar: Option<&str>| ui::native_media::Guest {
            id: ui::native_media::Guest::id_of("discord", account),
            name: name.into(),
            avatar: avatar.map(str::to_owned),
            service: "discord".into(),
            linked: None,
            muted: false,
        };
        if let Some(huddle) = self
            .native_voice
            .snapshot
            .rosters
            .iter_mut()
            .find(|roster| roster.channel_id == "c_huddle")
        {
            huddle.guests = vec![
                guest("80351110224678912", "Bo", Some("tilo-demo://avatar-bo")),
                guest("90351110224678912", "Kai", None),
            ];
            huddle.bridges = vec!["discord".into()];
        }
        self.native_voice.snapshot.speaking = vec!["discord:80351110224678912".to_owned()];
    }

    /// The decoded avatar `m`'s bot showed for it, requesting it from the
    /// content store (and so from the room) until it is there.
    pub(crate) fn shown_avatar(
        &self,
        m: &crate::model::Message,
    ) -> Option<vello::peniko::ImageData> {
        let avatar = m.appearance.as_ref()?.avatar.as_deref()?;
        self.images.get(&crate::images::blob_key(avatar))
    }

    /// Warm the attachment-image cache for the current channel and an open
    /// picker, so the frame shows decoded thumbnails as soon as the
    /// background loader has them.
    pub(crate) fn request_visible_images(&self) {
        // `self.messages` already holds every message in the active channel,
        // including threaded replies and the thread parent, so one pass covers
        // the timeline and the open thread panel.
        for m in &self.messages {
            let _ = self.shown_avatar(m);
            match &m.attachment {
                // Never hydrate the bytes of an embed this peer isn't granted.
                Some(crate::model::Attachment::Image { blob, .. }) if self.embed_allowed(blob) => {
                    let _ = self.images.get(&crate::images::blob_key(blob));
                }
                Some(crate::model::Attachment::Video { blob, .. }) if self.embed_allowed(blob) => {
                    let _ = self.video_poster(blob);
                }
                _ => {}
            }
        }
        // Also warm any thumbnails visible in an open attachment picker.
        for e in &self.attach_entries {
            let _ = self.images.get(&e.key());
        }
    }

    /// The host-tile video rect for the current call mode, including the
    /// dragged overlay offset (docked huddle only — the expanded grid is
    /// anchored). The single source of truth for both the `VideoPlayer` node
    /// placement and the surrounding call chrome.
    pub(crate) fn call_video_rect(&self) -> Rect {
        if self.call.expanded {
            // The host is the first cell of the responsive participant grid,
            // so the tile (and the VideoPlayer surface beneath it) fills its
            // share of the surface at every width — never a fixed fraction
            // that leaves the rest of the overlay empty.
            self.overlay_host_rect()
        } else {
            self.call
                .placement
                .rect(self.rect, self.desktop_floating_bounds())
        }
    }

    pub(crate) fn desktop_floating_bounds(&self) -> Rect {
        if self.floating_surface_bounds.area() > 0.0 {
            self.floating_surface_bounds
        } else {
            self.rect
        }
    }

    /// Why a picture chosen to add as an emoji was not, where the open
    /// picker says so.
    fn emoji_notice_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let (notice, footer) = self.emoji_picker.notice().filter(|_| self.emoji_open)?;
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                .with_name(notice.to_owned())
                .with_bounds(footer)
                .with_live(ui::a11y::Politeness::Assertive)
                .with_hashed_key(("emoji-picker-notice", notice)),
        )
    }

    pub(crate) fn push_hit(hits: &mut Vec<(Rect, Hit)>, rect: Rect, hit: Hit) {
        hits.push((rect, hit));
    }

    /// Pointer, keyboard and assistive technology all derive from this one
    /// immediate-mode control inventory. Purely decorative/scrim targets stay
    /// out; every included row has a useful spoken name and an activation path.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive semantic mapping keeps pointer, keyboard and assistive labels from drifting into separate inventories"
    )]
    pub(crate) fn semantic_hits_from(&self, hits: &[(Rect, Hit)]) -> Vec<SemanticHit> {
        use ui::a11y::Role;

        hits.iter()
            .filter_map(|(rect, hit)| {
                let copy = ControlName::copy;
                let content = ControlName::content;
                let (name, role, selected) = match hit {
                    Hit::AgentWindow => (
                        copy(if self.conversation_surface {
                            "chat-agent-return"
                        } else {
                            "chat-agent-float"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::FocusEdit => (copy("chat-edit-field"), Role::MultilineTextInput, false),
                    Hit::FocusMain => (copy("chat-composer"), Role::MultilineTextInput, false),
                    Hit::FocusThread => (
                        copy("chat-thread-composer"),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::HostModel(command) => {
                        (copy(&format!("chat-model-{command}")), Role::Button, false)
                    }
                    Hit::Send => (copy("chat-send-message"), Role::Button, false),
                    Hit::UndoDelete => (copy("chat-undo"), Role::Button, false),
                    Hit::JumpToLatest => (copy("chat-jump-latest-message"), Role::Button, false),
                    Hit::ThreadScroll(end) => (
                        copy(if *end {
                            "chat-thread-end"
                        } else {
                            "chat-thread-start"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentReasoning(id) => (
                        copy(if self.expanded_reasoning.contains(id) {
                            "chat-agent-hide-reasoning"
                        } else {
                            "chat-agent-show-reasoning"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::ComposerEmoji(_) => (
                        copy(&crate::i18n::t("chat-emoji-insert")),
                        Role::Button,
                        false,
                    ),
                    Hit::Attach => (
                        copy(&crate::i18n::t("chat-attach-share")),
                        Role::Button,
                        false,
                    ),
                    Hit::Attachment { action, .. } => (copy(&action.label()), Role::Button, false),
                    Hit::CloseAttachment => (
                        copy(&crate::i18n::t("chat-attachment-close")),
                        Role::Button,
                        false,
                    ),
                    Hit::AttachDevice => (
                        copy(&crate::i18n::t("chat-attach-device")),
                        Role::Button,
                        false,
                    ),
                    Hit::AttachPick(path) => (
                        content(crate::i18n::t_args(
                            "chat-attach-pick",
                            &[(
                                "name",
                                std::path::Path::new(path)
                                    .file_name()
                                    .and_then(std::ffi::OsStr::to_str)
                                    .unwrap_or(path)
                                    .to_owned()
                                    .into(),
                            )],
                        )),
                        Role::Button,
                        false,
                    ),
                    Hit::StartCall => (copy("chat-call-start-or-open"), Role::Button, false),
                    Hit::StopSharingLiveContext => (copy("chat-context-stop"), Role::Button, false),
                    Hit::CopyResourceLink(message) => (
                        ControlName {
                            text: self.resource_navigation.label(message, true),
                            interface: self.resource_navigation.label_is_copy(message),
                        },
                        Role::Button,
                        false,
                    ),
                    Hit::OpenResource(message) => (
                        content(
                            self.resource_navigation.open_label(
                                message,
                                &self
                                    .message(message)
                                    .and_then(|m| m.reference.as_ref().map(|r| r.label.clone()))
                                    .unwrap_or_default(),
                            ),
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenShare(message) => (
                        content(self.message(message).map_or_else(String::new, |m| {
                            crate::i18n::t_args(
                                "chat-share-open",
                                &[
                                    (
                                        "title",
                                        m.reference
                                            .as_ref()
                                            .map(|r| r.label.clone())
                                            .unwrap_or_default()
                                            .into(),
                                    ),
                                    ("name", self.user_name(&m.author).into()),
                                ],
                            )
                        })),
                        Role::Button,
                        false,
                    ),
                    Hit::StopShare(_) => (copy("chat-share-stop"), Role::Button, false),
                    Hit::ShareWindowPanel => (copy("chat-share-window"), Role::Button, false),
                    Hit::ShareAccess(access) => (
                        ControlName {
                            text: crate::live_share::access_label(*access),
                            interface: true,
                        },
                        Role::ToggleButton,
                        self.live_shares
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.access == *access),
                    ),
                    Hit::SharePick(tab) => (
                        content(
                            ui::window_share::windows()
                                .into_iter()
                                .find(|window| window.tab == *tab)
                                .map_or_else(String::new, |window| {
                                    crate::i18n::t_args(
                                        "chat-share-pick",
                                        &[("title", window.title.into())],
                                    )
                                }),
                        ),
                        Role::ToggleButton,
                        self.live_shares
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.chosen == Some(*tab)),
                    ),
                    Hit::ShareChosen => (copy("chat-share-window-button"), Role::Button, false),
                    Hit::ToggleRightFlap => (copy("chat-right-flap"), Role::Button, false),
                    Hit::TogglePins => (copy("chat-pins"), Role::ToggleButton, self.pins_open),
                    Hit::RevealMessage(message) => (
                        self.message(message).map_or_else(
                            || copy("chat-pinned-message"),
                            |m| content(format!("{}: {}", self.user_name(&m.author), m.text)),
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::ShowPerson { person, .. } => (
                        content(crate::i18n::t_args(
                            "chat-person-card",
                            &[("name", self.user_name(person).into())],
                        )),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenThread(_) => (copy("chat-thread-open"), Role::Button, false),
                    Hit::CloseThread => (copy("chat-thread-close"), Role::Button, false),
                    Hit::Reaction { msg, emoji } => {
                        let mine = self
                            .message(msg)
                            .and_then(|message| {
                                message
                                    .reactions
                                    .iter()
                                    .find(|reaction| reaction.emoji == *emoji)
                            })
                            .is_some_and(|reaction| reaction.includes(&self.me.id));
                        (
                            content(ui::widget::reaction_strip::pill_name(emoji, mine)),
                            Role::ToggleButton,
                            mine,
                        )
                    }
                    Hit::AddReaction { .. } => {
                        (copy("chat-emoji-add-reaction"), Role::Button, false)
                    }
                    Hit::EmojiChoice(emoji) => (
                        content(crate::i18n::t_args(
                            "chat-emoji-choose",
                            &[(
                                "name",
                                crate::i18n::emoji_catalogue()
                                    .get(emoji)
                                    .map_or(emoji.as_str(), |found| found.name())
                                    .into(),
                            )],
                        )),
                        Role::Button,
                        // The highlighted emoji, as the footer names it: a
                        // workspace emoji just added is the one selected.
                        self.emoji_picker.selected() == Some(emoji.as_str()),
                    ),
                    Hit::EmojiGroup(group) => (
                        // A group's label is the emoji catalogue's own fixed
                        // copy in the desktop's language.
                        crate::i18n::emoji_catalogue()
                            .groups()
                            .get(*group)
                            .map_or_else(
                                || copy("chat-emoji-group"),
                                |group| ControlName {
                                    text: group.label().to_owned(),
                                    interface: true,
                                },
                            ),
                        Role::Tab,
                        *group == self.emoji_picker.group(),
                    ),
                    Hit::EmojiAddCustom => (copy("chat-emoji-add"), Role::Button, false),
                    Hit::FocusEmojiSearch => (copy("chat-emoji-search"), Role::TextInput, false),
                    Hit::EditHistory { .. } => {
                        (copy("chat-edit-history-show"), Role::Button, false)
                    }
                    Hit::EditSave => (copy("chat-edit-save"), Role::Button, false),
                    Hit::EditCancel => (copy("chat-edit-cancel"), Role::Button, false),
                    Hit::CallMute => (
                        copy(if self.microphone_missing() {
                            "chat-call-no-microphone"
                        } else {
                            "chat-call-mute-action"
                        }),
                        Role::ToggleButton,
                        self.call.muted,
                    ),
                    Hit::CallDeafen => (
                        copy("chat-call-deafen"),
                        Role::ToggleButton,
                        self.call.deafened,
                    ),
                    Hit::CallCamera => (
                        copy("chat-call-camera-on"),
                        Role::ToggleButton,
                        !self.call.audio_only,
                    ),
                    Hit::CallScreen => (
                        copy("chat-call-share-screen"),
                        Role::ToggleButton,
                        self.call.sharing,
                    ),
                    Hit::CallParticipantsToggle => (
                        copy("chat-call-participants-show"),
                        Role::ToggleButton,
                        self.call.participants_expanded,
                    ),
                    Hit::CallExpand => (
                        copy("chat-call-expand-restore"),
                        Role::Button,
                        self.call.expanded,
                    ),
                    Hit::CallLeave => (copy("chat-call-leave-action"), Role::Button, false),
                    Hit::CallParticipant(id) => (
                        content(crate::i18n::t_args(
                            "chat-call-participant",
                            &[("name", self.call_name(id).into())],
                        )),
                        Role::Button,
                        false,
                    ),
                    _ => return None,
                };
                // The composer's pointer target is the whole box (clicking
                // its padding focuses it), but what assistive technology
                // reads as the text field is the band the shared `TextInput`
                // was laid out into — the attach / emoji / send buttons sit
                // beside that band, not inside it. Before the first paint
                // the band is empty, so the box stands in for it.
                let text_band = |field: &crate::composer::DraftComposer| {
                    let inner = field.rect();
                    if inner.width() > 0.0 && inner.height() > 0.0 {
                        inner
                    } else {
                        *rect
                    }
                };
                let rect = match hit {
                    Hit::FocusMain => text_band(&self.composer_main),
                    Hit::FocusThread => text_band(&self.composer_thread),
                    _ => *rect,
                };
                Some((rect, hit.clone(), name, role, selected))
            })
            .collect()
    }

    pub(crate) fn semantic_hits(&self) -> Vec<SemanticHit> {
        Self::deduplicate_semantic_hits(self.semantic_hits_from(&self.hits))
    }

    pub(crate) fn hover_tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        // A bridged author's service mark says where they write from.
        if let Some(tip) = self.painted_messages.values().find_map(|parts| {
            parts
                .service
                .as_ref()
                .filter(|(rect, _)| rect.contains(point))
                .cloned()
        }) {
            return Some(tip);
        }
        // A reaction pill says who reacted, with the strip's shared wording.
        if let Some((_, Hit::Reaction { msg, .. })) = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            && let Some(tip) = self
                .reaction_strips
                .borrow()
                .iter()
                .filter(|((_, id), _)| id == msg)
                .find_map(|(_, strip)| {
                    strip.tooltip_at(point, &|reactor| self.user_name(&reactor.by))
                })
        {
            return Some(tip);
        }
        if let Some((rect, hit)) = self.hits.iter().rev().find(|(rect, hit)| {
            rect.contains(point)
                && matches!(
                    hit,
                    Hit::ToggleRightFlap
                        | Hit::TogglePins
                        | Hit::CallMute
                        | Hit::CallDeafen
                        | Hit::Attach
                        | Hit::Send
                        | Hit::ComposerEmoji(_)
                        | Hit::AddReaction { .. }
                )
        }) && let Some((_, _, label, _, _)) = self
            .semantic_hits_from(&[(*rect, hit.clone())])
            .into_iter()
            .next()
        {
            return Some((*rect, label.text));
        }
        // Do not let the broad title region lend its text to a labelled
        // control layered above it (for example the Call button).
        if self
            .hits
            .iter()
            .rev()
            .any(|(rect, hit)| rect.contains(point) && !matches!(hit, Hit::Dismiss))
        {
            return None;
        }
        None
    }

    /// A semantic target names an operation, not a particular paint pass. A
    /// message can appear in both timeline and thread briefly; exporting one
    /// retained node keeps that operation's target unambiguous.
    fn deduplicate_semantic_hits(hits: Vec<SemanticHit>) -> Vec<SemanticHit> {
        let mut targets = std::collections::HashSet::new();
        hits.into_iter()
            .filter(|(_, hit, ..)| targets.insert(hit.semantic_id()))
            .collect()
    }
}

/// Where a shown message drew its own texts: the avatar, the author's name
/// and the time of a message that starts a group, and each laid-out line of
/// its body. All of it is the person's or the message's, none Chat's copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PaintedMessage {
    pub(crate) avatar: Option<Rect>,
    pub(crate) author: Option<(Rect, String)>,
    /// The mark of the service a bridged author writes from, with what it
    /// says (`From Discord`).
    pub(crate) service: Option<(Rect, String)>,
    pub(crate) time: Option<(Rect, String)>,
    pub(crate) lines: Vec<(Rect, String)>,
}

impl PaintedMessage {
    /// What of it shows inside `area`, each part cut to it.
    pub(crate) fn clipped(self, area: Rect) -> Self {
        let cut = |rect: Rect| {
            let cut = rect.intersect(area);
            (cut.width() > 0.0 && cut.height() > 0.0).then_some(cut)
        };
        let cut_text = |(rect, text): (Rect, String)| cut(rect).map(|rect| (rect, text));
        Self {
            avatar: self.avatar.and_then(cut),
            author: self.author.and_then(cut_text),
            service: self.service.and_then(cut_text),
            time: self.time.and_then(cut_text),
            lines: self.lines.into_iter().filter_map(cut_text).collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.avatar.is_none()
            && self.author.is_none()
            && self.service.is_none()
            && self.time.is_none()
            && self.lines.is_empty()
    }

    /// The row's children: the avatar as an image, then the name, the time
    /// and each body line as text, each with the rect it was drawn in.
    pub(crate) fn access_children(&self, id: &str) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let name = self.author.as_ref().map(|(_, name)| name.as_str());
        let mut children = Vec::new();
        if let Some(rect) = self.avatar {
            let mut avatar = AccessNode::new(Role::Image)
                .with_bounds(rect)
                .with_hashed_key((id, "avatar"));
            if let Some(name) = name {
                avatar = avatar.with_name(name);
            }
            children.push(avatar);
        }
        let texts = self.author.iter().map(|part| (part, "author"));
        for ((rect, text), part) in texts {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, part)),
            );
        }
        if let Some((rect, tip)) = &self.service {
            children.push(
                AccessNode::new(Role::Image)
                    .with_name(tip.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, "service")),
            );
        }
        let texts = self.time.iter().map(|part| (part, "time"));
        for ((rect, text), part) in texts {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, part)),
            );
        }
        for (index, (rect, text)) in self.lines.iter().enumerate() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, "line", index)),
            );
        }
        children
    }
}

/// A painted control's spoken name, and whether it is Chat's own fixed copy:
/// a catalogue message with nothing of a person or a resource in it. A
/// feedback report shows such names as they are and hides the rest until
/// the reporter shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ControlName {
    pub(crate) text: String,
    pub(crate) interface: bool,
}

impl ControlName {
    /// The catalogue message `key`, the same for everyone.
    pub(crate) fn copy(key: &str) -> Self {
        Self {
            text: crate::i18n::t(key),
            interface: true,
        }
    }

    /// A name that carries someone's words, a person, a file or a resource.
    pub(crate) fn content(text: String) -> Self {
        Self {
            text,
            interface: false,
        }
    }
}

/// One painted control in the inventory pointer, keyboard and assistive
/// technology share: where it is, what it does, its name, its role and
/// whether it is selected or checked.
pub(crate) type SemanticHit = (Rect, Hit, ControlName, ui::a11y::Role, bool);

/// Compute an absolute rect from window-relative fractions.
pub(crate) fn frac_rect(win: Rect, f: (f64, f64, f64, f64)) -> Rect {
    let x0 = win.x0 + f.0 * win.width();
    let y0 = win.y0 + f.1 * win.height();
    Rect::new(x0, y0, x0 + f.2 * win.width(), y0 + f.3 * win.height())
}

impl ui::semantic_document_read::SemanticReaders for ChatApp {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::feedback::READERS
    }
}

impl Continuable for ChatApp {
    fn continuation_metadata(
        &self,
        _state: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let continuation = self.call_continuation()?;
        let bytes = serde_json::to_vec(&continuation).ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: crate::continuity::CHAT_CONTINUATION_SCHEMA,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        self.restore_call_continuation(metadata, state)
    }
}

impl Collaborative for ChatApp {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.queue_realtime_registration();
        if self.realtime.target_media_ready && self.realtime_target_media_ready() {
            self.realtime.target_media_ready = false;
        }
        let mut requests = std::mem::take(&mut self.realtime.requests);
        self.settle_resource_navigation();
        requests.extend(self.resource_navigation.remote_mut().take_collab_requests());
        requests.extend(self.moderation_requests());
        self.native_voice.video.wanted = self.wanted_pictures();
        let before = self.native_voice.snapshot.state;
        requests.extend(self.native_voice.take_requests(ui::time::now()));
        if before != self.native_voice.snapshot.state {
            self.project_native_voice();
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        match event {
            ui::collaboration::CollabEvent::RemoteAction { message } => {
                self.complete_moderation(message);
                self.resource_navigation
                    .remote_mut()
                    .handle_collab_event(event);
                self.settle_resource_navigation();
            }
            ui::collaboration::CollabEvent::FollowChanged { peer_id } => {
                self.context_follow_requested = peer_id.is_some();
                self.following_context = false;
                self.request_relayout();
            }
            ui::collaboration::CollabEvent::NativeMediaCommandResult { request_id, result } => {
                if self.native_voice.complete(*request_id, result) {
                    self.project_native_voice();
                }
                let arrived = std::mem::take(&mut self.native_voice.video.arrived);
                if !arrived.is_empty() {
                    self.call_pictures.receive(arrived);
                }
            }
            ui::collaboration::CollabEvent::RealtimeCommandResult { result, .. } => match result {
                Ok(ui::realtime_continuity::RealtimeReply::Registered { session }) => {
                    self.realtime.registration_pending = false;
                    self.realtime.session = Some(session.clone());
                }
                Ok(ui::realtime_continuity::RealtimeReply::Accepted) => {
                    self.realtime.accepted_target = true;
                }
                Ok(ui::realtime_continuity::RealtimeReply::Rejected { .. }) | Err(_) => {
                    self.realtime.registration_pending = false;
                }
                _ => {}
            },
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::Offer {
                    session, intent, ..
                },
            ) if intent.provider == "tilo.chat" => {
                let Some(workspace_id) = intent.semantic_position.clone() else {
                    return;
                };
                let offered = crate::continuity::ChatCallContinuation {
                    workspace_id,
                    channel_id: intent.room_id.clone(),
                    media: intent.media,
                    controls: intent.controls,
                };
                if offered.valid() {
                    self.realtime.offer = Some((session.clone(), offered));
                }
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::CaptureAuthorized {
                    session,
                    capture_authorizations,
                },
            ) if self
                .realtime
                .offer
                .as_ref()
                .is_some_and(|(offered_session, _)| offered_session == session) =>
            {
                let _ = self.accept_realtime_offer(capture_authorizations.clone());
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::StopSource { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                self.realtime.stop_source = Some(session.clone());
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::RolledBack { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                // Source deliberately remains live. Only discard the pending
                // target edge; no call/media mutation occurs here.
                self.realtime.offer = None;
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::Ended { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                self.realtime.session = None;
            }
            _ => {}
        }
    }
}

impl ui::widget::controls::ActionControls for ChatApp {
    /// The header's search button asks the platform to open search over
    /// Chat; everything else comes from the children.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.search_opener.take().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
}

impl Widget for ChatApp {
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        // The record is lent to the settle pass: the room's other records
        // take writes now.
        self.flush_staged_parts();
        self.apply_window_toolbar(edits);
        self.take_sidebar_events(edits);
        self.follow_window_shares(edits);
    }

    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::collaboration::participants::Participants,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::view::capability::ViewState,
        dyn ui::semantic_document_read::SemanticReaders
    );

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.call.placement.set_reduce_motion(reduced);
        for surface in self.agent_surfaces.values_mut() {
            surface.placement.set_reduce_motion(reduced);
        }
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        if self.thread_open.is_some()
            && (self.thread_is_page() || matches!(self.focus, Focus::Thread))
        {
            return Some(ui::widget::PrimaryScrollRegion {
                key: "thread",
                owner_generation: None,
                metrics: self.thread_scroll.primary_vertical_scroll()?.metrics,
            });
        }
        Some(ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: ui::widget::ScrollMetrics::new(
                self.scroll_content,
                self.scroll_viewport,
                (self.scroll_content - self.scroll_viewport).max(0.0)
                    - self
                        .scroll
                        .clamp(0.0, (self.scroll_content - self.scroll_viewport).max(0.0)),
            ),
        })
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        action.apply_to(self, Self::primary_vertical_scroll, |app, region, next| {
            if region.key == "thread" {
                app.thread_scroll.set_offset_y(ui::num::f32_of_f64(next));
            } else {
                app.scroll = region.metrics.max_scroll() - next;
                app.timeline_at_end = app.scroll <= 0.5;
            }
            app.canonical_dirty = true;
        })
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.host_focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.host_focused = focused;
        if !focused {
            self.right_flap.set_resize_focused(false);
        }
        self.canonical_dirty = true;
        self.set_composer_focus();
    }
    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values()
                .find(|surface| surface.id == id)
        {
            return surface.view.ime_content();
        }
        self.host_focused.then_some(ui::ime::InputContent::TEXT)
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values()
                .find(|surface| surface.id == id)
        {
            return surface.view.ime_cursor_rect();
        }
        if !self.host_focused {
            return None;
        }
        if self.editing.is_some() {
            self.edit_input.ime_cursor_rect()
        } else {
            self.focused_composer().ime_cursor_rect()
        }
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        if self.thread_open.is_some() {
            self.thread_scroll.sync(doc, path);
        }
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.apply_realtime_stop();
        self.establish_realtime_target();
        self.announce_me(doc);
        // A fold made on another of this person's devices.
        if let Some(preferences) = &self.preferences {
            let collapsed = preferences.get().collapsed_categories;
            if collapsed != self.collapsed {
                self.collapsed = collapsed;
                self.reselect();
            }
        }
        self.settle_attachment_picker();
        self.settle_device_files();
        self.settle_refused_post();
        self.settle_attachment_import();
        self.refresh_person_conversation();
        {
            use automerge::transaction::Transactable as _;
            // Reload and repaint only when the document moved: committed heads
            // differ, or a transaction is or was open. Service completions run
            // before this comparison so their canonical changes are projected
            // in the same frame without making idle sync reload continuously.
            let heads = doc.base_heads();
            let pending = doc.pending_ops();
            // The messages move on their own, in the parts beside the
            // record.
            let parts_moved = self
                .chain
                .as_ref()
                .is_some_and(|chain| Some(chain.revision()) != self.chain_seen);
            let unchanged = !parts_moved
                && self
                    .synced
                    .as_ref()
                    .is_some_and(|(synced_heads, synced_pending)| {
                        *synced_heads == heads && *synced_pending == 0 && pending == 0
                    });
            if !unchanged {
                self.canonical_dirty = true;
                self.reload(doc);
                self.synced = Some((heads, pending));
            }
        }
        if self.ensure_selection() {
            // Re-picking the channel changes which messages apply.
            self.canonical_dirty = true;
            self.reload(doc);
        }
        self.join_pending_call();
        self.open_requested();
        // The channel in view is read: advance its watermark (no-op when
        // nothing new arrived) so its unread badge stays cleared. Only a
        // presented instance has a viewer; a headless host worker never
        // authors read receipts.
        if self.presented {
            self.mark_active_read(doc);
        }
        self.sync_composers(doc);
        self.request_visible_images();
        self.sync_call_pictures();
        // Native capture/transport belongs to the shell's lease-bound service.
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host video tile is an absolute leaf. In the docked huddle it
        // sits at a *fractional* home (sized correctly even before `self.rect`
        // is known); the dragged overlay offset is applied as an origin shift
        // in `apply_layout`. In the expanded overlay it instead tracks the
        // first cell of the responsive participant grid so the surface fills
        // its share at every width. That cell depends on the window size, so
        // it is expressed in absolute px derived from the last known
        // `self.rect`; before any layout pass the rect is empty, in which case
        // the node collapses to zero size (nothing to paint) and the next
        // frame — once the real size is known — positions it exactly.
        let video_style = if self.call.expanded {
            let host = self.call_video_rect();
            let (left, top, w, h) = if host.width() > 1.0 && host.height() > 1.0 {
                (
                    (host.x0 - self.rect.x0) as f32,
                    (host.y0 - self.rect.y0) as f32,
                    host.width() as f32,
                    host.height() as f32,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            Style {
                position: Position::Absolute,
                inset: taffy::Rect {
                    left: taffy::style_helpers::length(left),
                    top: taffy::style_helpers::length(top),
                    right: LengthPercentageAuto::auto(),
                    bottom: LengthPercentageAuto::auto(),
                },
                size: Size::from_lengths(w, h),
                ..Default::default()
            }
        } else {
            let f = HUDDLE_VIDEO;
            Style {
                position: Position::Absolute,
                inset: taffy::Rect {
                    left: percent(f.0 as f32),
                    top: percent(f.1 as f32),
                    right: LengthPercentageAuto::auto(),
                    bottom: LengthPercentageAuto::auto(),
                },
                size: Size::from_percent(f.2 as f32, f.3 as f32),
                ..Default::default()
            }
        };
        let video_node = taffy.widget_leaf(video_style, node_map, &mut self.video);
        self.video_node = Some(video_node);

        // `new_with_children` only fails when a child belongs to a different
        // tree; `video_node` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "video_node was just created in this same tree"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    size: Size::from_percent(1.0_f32, 1.0_f32),
                    ..Default::default()
                },
                &[video_node],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        self.root_node = Some(root);
        root
    }

    /// The host call tile has a node of its own; Chat routes and paints it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.video);
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if !self.presented {
            self.presented = true;
            self.read_on_present = true;
        }
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if !self.call.expanded {
            self.call
                .placement
                .set_preferred_size(frac_rect(self.rect, HUDDLE_VIDEO).size());
            self.call
                .placement
                .set_attached_chrome(self.huddle_attached_chrome());
        }
        if let Some(vn) = self.video_node {
            // The docked tile's taffy node retains its responsive fractional
            // size, while the shared floating placement owns its actual
            // origin. Offset the node's origin so its resolved local location
            // lands exactly on that shared rect.
            let off = if self.call.expanded {
                Point::new(self.rect.x0, self.rect.y0)
            } else {
                let desired = self.call_video_rect();
                let video_layout = taffy.layout(vn).copied().unwrap_or_default();
                Point::new(
                    desired.x0 - f64::from(video_layout.location.x),
                    desired.y0 - f64::from(video_layout.location.y),
                )
            };
            self.video.apply_layout(taffy, vn, off);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The attachments painted below say when they step on next.
        self.arriving_wake.set(None);
        // Adopt this theme's chrome and advance the drawers for this frame.
        self.layout_text_scale = theme.text_scale;
        self.advance_flaps(theme);
        self.reconcile_huddle_motion(tr, theme);
        self.reconcile_speaking(theme);

        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        // The picture tiles this frame draws note themselves again, and so
        // do the messages it shows.
        self.picture_tiles.borrow_mut().clear();
        self.painted_messages.clear();
        // Every region is drawn as a part of its own, compared with the
        // last frame's: what Chat reports as damage is the regions whose
        // drawing changed.
        let mut parts = PaintParts::new(scene);
        let window = self.rect;

        // The conversation stands on the window ground the framework painted.
        // A pushed thread owns the narrow page, including its input targets.
        if !self.thread_is_page() {
            self.draw_conversation(&mut parts, scene, tf, tr, theme, presence, &mut hits);
        }
        self.draw_drawers(&mut parts, scene, tf, tr, theme, presence, &mut hits);
        self.declare_window_toolbar();

        // Call layers. The host tile is a real VideoPlayer, painted only once
        // a real capture has fed it a frame; otherwise the honest camera-off
        // placeholder covers the host tile.
        let host_has_frame = self.video.latest_frame().is_some();
        if self.call.active && self.call.expanded {
            // Measure the controls and header with this frame's text scale
            // before anything reads their geometry.
            self.measure_call_overlay(tr, theme);
            parts.draw(scene, "chat-call", window, |scene| {
                self.draw_call_overlay_bg(scene, tf, tr, theme);
                if host_has_frame {
                    self.video.draw(scene, tf, tr, theme, presence);
                }
                self.draw_host_camera_off(scene, tf, tr, theme);
                self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
            });
        } else if self.app_draws_huddle() {
            // Settled, the call window paints its panel and shadow; moving,
            // it may be anywhere in the window.
            let placement = &self.call.placement;
            let bounds = if placement.is_dragging() || placement.is_handoff_animating() {
                window
            } else {
                ui::widget::floating_surface::painted_bounds(self.huddle_panel(), theme)
                    .intersect(window)
            };
            parts.draw(scene, "chat-call", bounds, |scene| {
                self.draw_live_huddle(scene, tf, tr, theme, presence, &mut hits);
            });
        }

        // Popovers over the conversation; the context menu paints in the
        // top layer, from `draw_overlay`.
        if self.emoji_open {
            parts.draw(scene, "chat-emoji", window, |scene| {
                self.draw_emoji_picker(scene, tf, tr, theme, presence, &mut hits);
            });
        }
        if self.history_open.is_some() {
            parts.draw(scene, "chat-history", window, |scene| {
                self.draw_history_popover(scene, tf, tr, theme, &mut hits);
            });
        }
        if self.attach_open {
            parts.draw(scene, "chat-attach", window, |scene| {
                self.draw_attach_picker(scene, tf, tr, theme, &mut hits);
            });
        }
        if self.live_shares.panel.is_some() {
            parts.draw(scene, "chat-share", window, |scene| {
                self.draw_share_panel(scene, tf, tr, theme, &mut hits);
            });
        }
        self.declare_share_embeds();

        // Immediate-mode keyboard focus needs a visible counterpart. The ring
        // is painted last so it remains legible on sidebar and popovers.
        let semantic = Self::deduplicate_semantic_hits(self.semantic_hits_from(&hits));
        if self.host_focused
            && let Some(index) = self.keyboard_target
        {
            if let Some((rect, ..)) = semantic.get(index) {
                parts.draw(scene, "chat-focus", window, |scene| {
                    ui::widget::paint_focus_ring(
                        scene,
                        tf,
                        *rect,
                        f64::from(theme.tokens.radii.item),
                        theme,
                    );
                });
            } else {
                self.keyboard_target = None;
            }
        }

        let frame = parts.finish(scene, vec![window], tf, theme);
        self.paint_damage.record(window, frame);
        self.hits = hits;
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A focused composer's editor floats its menus over the timeline.
        for composer in [&mut self.composer_main, &mut self.composer_thread] {
            if composer.is_focused() && composer.rect().width() > 0.0 {
                composer.draw_overlay(scene, tf, tr, theme, presence);
            }
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.widget.draw_overlay(scene, tf, tr, theme, presence);
        }
        if self.channel_sheet.is_some() {
            let categories: Vec<(Id, String)> = self
                .ws_categories()
                .into_iter()
                .map(|category| (category.id.clone(), category.name.clone()))
                .collect();
            let window = self.rect;
            let focused = self.host_focused;
            if let Some(sheet) = self.channel_sheet.as_mut() {
                sheet.paint(
                    ui::paint::PaintCtx::new(scene, tf, tr, theme),
                    presence,
                    window,
                    &categories,
                    focused,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // What is typed belongs to the control that has the keyboard. A
        // desktop where no window holds it offers each key to every window
        // in turn, and Chat must not write into its composer what was not
        // typed into it.
        if !self.host_focused
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return;
        }
        let secondary = self
            .input_agent_surface
            .as_deref()
            .or(self.focused_agent_surface.as_deref());
        if let Some(id) = secondary
            && let Some(surface) = self
                .agent_surfaces
                .values_mut()
                .find(|surface| surface.id == id)
        {
            surface.view.handle_event(event, ctx);
            self.canonical_dirty |= surface.view.canonical_dirty;
            self.settle_agent_surface_commands();
            return;
        }
        // Embedded inputs request focus with a temporary layout node. Map
        // that actual request to Chat's retained keyboard/IME owner.
        let was_handled = *ctx.handled;
        // The detail drawer takes its show/hide shortcut, Escape and a press
        // on its scrim, unless a popover over the conversation is open and
        // dismisses first.
        if self.menu.is_none()
            && self.channel_sheet.is_none()
            && !self.attach_open
            && self.live_shares.panel.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.thread_is_page()
        {
            self.reconcile_flaps();
            if self.right_flap.handle_event(event, ctx) {
                *ctx.handled = true;
                self.canonical_dirty = true;
                return;
            }
        }
        self.canonical_dirty |= self.right_flap.take_resize_redraw();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) {
            self.right_flap.set_resize_focused(false);
        }
        let requested = self.dispatch_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.handled,
        );
        // Pointer motion that neither Chat nor its inputs consumed changes
        // nothing visible; every other event may. Marking every event dirty
        // made each pointer move anywhere on the desktop repaint everything.
        if !matches!(event, WindowEvent::CursorMoved { .. }) || *ctx.handled || requested {
            self.canonical_dirty = true;
        }
        if !was_handled && requested && ctx.focus_target.is_none() {
            *ctx.focus_target = Some(ctx.node_id);
        }
        // A menu's Rename names the row in the sidebar, which takes the
        // keyboard for it.
        if let Some(place) = self.sidebar_rename.take() {
            self.places.begin_rename(&place, ctx.focus_target);
        }
    }

    /// Chat's painted controls and elided header text take their tooltips
    /// through the toolkit's one mechanism, which shows them after a dwell
    /// and takes them away when the person acts or the pointer leaves.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.hover_tooltip_target(point)
    }

    /// The flap's resize edge, then the painted control under the pointer:
    /// a text field's I-beam, a handle's grab, a pointing hand on what a
    /// press activates. A popover's scrim asks for nothing.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if let Some(resize) = self.right_flap.resize_cursor(point) {
            return Some(resize);
        }
        let (_, hit) = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))?;
        match hit {
            Hit::Dismiss => None,
            Hit::FocusEmojiSearch => ui::Widget::cursor_shape(&self.emoji_picker, point),
            Hit::AgentDragHandle | Hit::CallDragHandle => Some(CursorIcon::Grab),
            hit if self.text_field_access(hit).is_some() => Some(CursorIcon::Text),
            _ => Some(CursorIcon::Pointer),
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let controls = self
            .semantic_hits()
            .into_iter()
            .enumerate()
            .map(|(index, (rect, hit, name, role, selected))| {
                let input = self.text_field_access(&hit);
                let focused = self.host_focused
                    && self.keyboard_target.map_or_else(
                        || input.as_ref().is_some_and(|(focused, _, _)| *focused),
                        |target| target == index,
                    );
                let mut state = AccessState::new().focusable(true).focused(focused);
                // Of the buttons, the picker's emoji say which is the
                // highlighted one.
                if matches!(role, Role::Tab | Role::ListItem) || matches!(hit, Hit::EmojiChoice(_))
                {
                    state = state.selected(selected);
                }
                if matches!(role, Role::ToggleButton) {
                    state = state.checked(selected);
                }
                let mut node = AccessNode::new(role)
                    .with_name(name.text)
                    .with_interface_name(name.interface)
                    .with_state(state)
                    .with_bounds(rect)
                    .with_key(index as u64)
                    .with_action_target(hit.semantic_id());
                if let Some((_, value, (anchor, focus))) = input {
                    node = node.with_value(&value).with_text_selection(anchor, focus);
                } else if let Hit::Reaction { msg, emoji } = &hit
                    && let Some(reaction) = self.message(msg).and_then(|message| {
                        message
                            .reactions
                            .iter()
                            .find(|reaction| reaction.emoji == *emoji)
                    })
                {
                    node =
                        node.with_value(ui::widget::reaction_strip::pill_count(reaction.count()));
                }
                node
            })
            .collect();

        let conversation = self.conversation_accessibility();
        let mut children: Vec<AccessNode> = controls;
        children.extend(self.right_flap.resize_accessibility());
        if let Some(peer) = &self.direct_person {
            children.push(
                AccessNode::new(Role::Heading { level: 1 })
                    .with_name(self.person_display_name(peer))
                    .with_description(crate::i18n::t("chat-person-chat-private"))
                    .with_hashed_key(("person-conversation-heading", &self.active_channel)),
            );
        }
        if let Some(channel) = self.channel(&self.active_channel) {
            let mut heading = AccessNode::new(Role::Heading { level: 1 })
                .with_name(channel.kind.label(&channel.name))
                .with_hashed_key(("conversation-heading", &channel.id));
            if !channel.topic.trim().is_empty() {
                // The desktop header deliberately elides the topic before the
                // member/call cluster, and narrow layouts may omit it entirely.
                // Keep the complete string available to screen readers.
                heading = heading.with_description(channel.topic.clone());
            }
            children.push(heading);
        }
        children.push(conversation);
        // The call panel's count is painted in its title bar: read where it
        // is drawn, and no stop for the keyboard. Its status (what is
        // wrong, else how it is connected) is read with it.
        if self.call.active
            && let Some((title_bar, _)) = self
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::CallDragHandle))
        {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.call_count_label())
                    .with_description(self.call_status_label())
                    .with_bounds(*title_bar)
                    .with_hashed_key(("call-count", &self.call.channel)),
            );
        }
        children.extend(self.emoji_notice_accessibility());
        if let Some(menu) = self.menu.as_ref()
            && let Some(menu_node) = menu.widget.accessibility()
        {
            children.push(menu_node);
        }
        // Existing Chat controls are exported by their typed domain resources,
        // which carry device and destructive-operation metadata. Keep their
        // synthetic accessibility subtrees outside generic action projection.
        // Only the shared separators use the generic widget effect contract.
        for child in &mut children {
            if !matches!(
                child.action_target.as_deref(),
                Some("sidebar-resize" | "inspector-resize")
            ) {
                child.semantic_key = None;
            }
        }
        Some(
            AccessNode::new(Role::Group)
                .with_semantic_key("chat-surface")
                .with_name(crate::i18n::t("chat-surface"))
                .with_interface_name(true)
                .with_description(crate::i18n::t("chat-surface-detail"))
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        self.semantic_hits()
            .get(*index)
            .map(|(rect, ..)| *rect)
            .or_else(|| {
                let offset = index.checked_sub(self.semantic_hits().len())?;
                self.right_flap
                    .resize_accessibility()
                    .filter(|_| offset == 0)
                    .and_then(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect))
            })
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.semantic_hits().len() {
            let offset = index.saturating_sub(self.semantic_hits().len());
            let target = self
                .right_flap
                .resize_accessibility()
                .filter(|_| offset == 0)
                .and_then(|node| node.action_target);
            return target.is_some_and(|target| {
                self.perform_drawer_resize(&target, &ui::a11y::Action::Focus)
            });
        }
        self.right_flap.set_resize_focused(false);
        self.keyboard_target = Some(*index);
        true
    }

    /// Semantic targets are the same inventory used by pointer and Tab/Enter
    /// input. Keeping this adapter at the widget boundary prevents an
    /// assistive-technology activation from taking a separate, untracked
    /// mutation path: `on_hit` submits the same verb or field edit through
    /// the dispatcher, for history and collaboration.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if self.perform_drawer_resize(target, action) {
            return true;
        }
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("context-menu:item:") {
            return self.perform_menu_semantic_action(target, action, state, path);
        }
        let hit = self
            .semantic_hits()
            .into_iter()
            .find_map(|(_, hit, ..)| (hit.semantic_id() == target).then_some(hit));
        let Some(hit) = hit else {
            return false;
        };
        // These immediate-mode editors are real TextInputs, even though their
        // chrome is painted by Chat. Preserve their truthful text semantics:
        // focus goes through the same focus route as a click, values remain a
        // local draft until Send/Save, and selections stay inside the toolkit
        // editor rather than becoming a second text model.
        if matches!(hit, Hit::FocusMain | Hit::FocusThread | Hit::FocusEdit) {
            return self.perform_editor_semantic_action(hit, action, state, path);
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        if matches!(hit, Hit::Send) && self.direct_person.is_some() {
            return self.send_person_focused();
        }
        let messages_before = if matches!(hit, Hit::Send) {
            crate::projection::read(state).map(|chat| {
                chat.message_rows(&self.held_messages(), &self.active_channel)
                    .len()
            })
        } else {
            None
        };
        self.on_hit(hit, state);
        // A document that does not read says nothing about what the send did;
        // it was performed, and the record's own refusal is the answer.
        messages_before.is_none_or(|before| {
            crate::projection::read(state)
                .map(|chat| {
                    chat.message_rows(&self.held_messages(), &self.active_channel)
                        .len()
                })
                .is_none_or(|after| after == before.saturating_add(1))
        })
    }

    fn name(&self) -> &'static str {
        "ChatApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.widget.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu
            .as_ref()
            .and_then(|menu| menu.widget.modal_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }

    /// Keep the loop ticking only while something is actually moving: a flap
    /// mid-slide, an overlay being dragged, or attachment images still
    /// decoding in the background. A live-but-static call does **not** force
    /// continuous redraws (the roster/rings are static), so an idle call
    /// no longer pins a core.
    fn update(&mut self, _doc: &AppModel, _path: &ModelPath) -> bool {
        let model_changed = self.pump_model();
        let room_changed = self.take_room();
        // A mention chosen from the host's notifications opens its own
        // message, in the channel it was said in.
        self.settle_attachment_save();
        let opened = self
            .mention_alerts
            .as_ref()
            .and_then(crate::mention_alerts::MentionAlerts::take_open)
            .is_some_and(|(channel, message)| self.open_mention(&channel, &message));
        // Re-run `sync` once after the first layout so the channel already in
        // view acquires its read receipt on open.
        std::mem::take(&mut self.read_on_present) || model_changed || room_changed || opened
    }

    fn needs_animation_frame(&self) -> bool {
        self.read_on_present
            || self.host_model.needs_frame()
            || self.resource_navigation.pending()
            || (self.thread_open.is_some() && self.thread_scroll.needs_animation_frame())
            || self.flap_animating
            || self.call_chrome_motion.animating
            || self.speaking.animating
            || self.call.placement.is_dragging()
            || self.call.placement.is_handoff_animating()
            || self.agent_surfaces.values().any(|surface| {
                surface.placement.is_dragging() || surface.placement.is_handoff_animating()
            })
            || !self.images.is_idle()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // An attachment on its way steps its placeholder on at its
        // deadline; its bytes arriving wake the window themselves.
        self.native_voice
            .next_deadline(ui::time::now())
            .into_iter()
            .chain(self.arriving_wake.get())
            .min()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.right_flap.is_resizing()
            || self.call.placement.is_dragging()
            || self.agent_conversation_drag.is_some()
            || (self.thread_open.is_some() && self.thread_scroll.wants_pointer_capture())
    }

    /// Attachment thumbnails or call pictures still decoding: a capture
    /// waits for them.
    fn has_pending_visual_work(&self) -> bool {
        !self.images.is_idle() || self.call_pictures.pending() > 0
    }

    fn needs_redraw(&self) -> bool {
        self.canonical_dirty
            || (self.thread_open.is_some() && self.thread_scroll.needs_redraw())
            || self
                .arriving_wake
                .get()
                .is_some_and(|at| at <= ui::motion::frame_now())
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }
    fn damage_covers_own_box(&self) -> bool {
        self.paint_damage.covers_own_bounds()
    }
    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }
    fn mark_drawn(&mut self) {
        self.canonical_dirty = false;
        self.paint_damage.mark_drawn();
        self.composer_main.mark_drawn();
        self.composer_thread.mark_drawn();
        self.thread_scroll.mark_drawn();
    }
}

impl ChatApp {
    fn perform_editor_semantic_action(
        &mut self,
        hit: Hit,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        use ui::a11y::Action;

        // `keyboard_target` is the immediate-mode Tab ring. A semantic
        // editor action moves focus into the actual TextInput instead, so
        // let that editor be the single focused accessibility child.
        self.keyboard_target = None;

        match action {
            Action::Focus | Action::Activate => {
                self.on_hit(hit, state);
                return true;
            }
            Action::SetValue(_) | Action::SetSelection { .. } => match hit {
                Hit::FocusMain => {
                    self.focus = Focus::Main;
                    self.composer_main.ensure_draft(state);
                    if !self.composer_main.perform_action(action, state, path) {
                        return false;
                    }
                    self.reload(state);
                }
                Hit::FocusThread => {
                    if !self.perform_thread_editor_action(action, state, path) {
                        return false;
                    }
                }
                Hit::FocusEdit => {
                    if self.editing.is_none() {
                        return false;
                    }
                    if !self.edit_input.perform_action(action, state, path) {
                        return false;
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        self.set_composer_focus();
        true
    }

    fn perform_thread_editor_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        if self.thread_draft_key().is_none() {
            return false;
        }
        self.focus = Focus::Thread;
        self.composer_thread.ensure_draft(state);
        if !self.composer_thread.perform_action(action, state, path) {
            return false;
        }
        self.reload(state);
        true
    }

    fn perform_menu_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        let Some(mut menu) = self.menu.take() else {
            return false;
        };
        let menu_handled = menu
            .widget
            .perform_semantic_action(target, action, state, path);
        let selection = menu.widget.take_selection();
        let selected = selection.and_then(|index| menu.items.get(index).copied());
        let menu_target = menu.target.clone();
        if menu.widget.is_open() {
            self.menu = Some(menu);
        }
        if let Some(selected) = selected {
            if self.can_participate() {
                self.apply_menu_action(menu_target, selected, state);
            }
            return true;
        }
        menu_handled
    }

    fn perform_drawer_resize(&mut self, target: &str, action: &ui::a11y::Action) -> bool {
        if matches!(action, ui::a11y::Action::Focus) {
            self.right_flap.set_resize_focused(false);
        }
        let handled = self.right_flap.perform_resize_action(target, action);
        if handled {
            self.keyboard_target = None;
            self.reconcile_flaps();
            self.canonical_dirty = true;
        }
        handled
    }
}

impl ui::presence::capability::PublishesPresence for ChatApp {
    /// That its person is writing in a composer, else which voice channel
    /// they are in: what the others' sidebars show under it.
    fn presence_payload(&self) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        self.composer_presence().or_else(|| {
            (self.in_call() && !self.call.channel.is_empty()).then(|| {
                (
                    Self::voice_presence_key(&self.call.channel),
                    ui::PresencePayload::Caret { pos: 0 },
                )
            })
        })
    }
}

impl ui::collaboration::participants::Participants for ChatApp {
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        if participant.agent_task().is_some() {
            self.participant_labels
                .entry(participant.as_str().to_owned())
                .or_default();
        }
        false
    }
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.participant_labels.remove(participant.as_str());
    }
    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        if participant.agent_task().is_some() {
            let label = label.trim();
            self.participant_labels
                .insert(participant.as_str().to_owned(), label.to_owned());
            // Every member names the agent's messages as the host labelled
            // the task.
            if !label.is_empty()
                && self
                    .agent_authors
                    .get(participant.as_str())
                    .map(String::as_str)
                    != Some(label)
            {
                self.submit(crate::resource::Chat::NAME_AGENT.with(
                    crate::record::chat::NameAgent {
                        participant: participant.as_str().to_owned(),
                        name: label.to_owned(),
                    },
                ));
            }
            // A host acting as this agent: it is also the local participant.
            if participant.as_str() == self.me.id && !label.is_empty() {
                label.clone_into(&mut self.me.name);
                self.me.initials = crate::identity::initials_for(label);
            }
        }
    }
}

impl ChatApp {
    /// The one rendering path for a docked or shell-presented huddle. The
    /// shell calls this exact method after ownership transfer, so video,
    /// participants, controls and hit regions remain live without duplication.
    fn draw_live_huddle(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // The app copy, transfer proxy and native shell surface all pass
        // through this one presentation transform. Applying the handoff
        // flourish in an ownership-specific caller made the first app-owned
        // frame lose its soft deformation even though its geometry matched.
        let transform = self.call.placement.presentation_transform(
            transform,
            self.huddle_panel(),
            theme.reduce_motion,
        );
        self.draw_huddle_bg(scene, transform, text_renderer, theme, hits);
        // The large tile shows someone else once anyone is; this person's
        // own camera only while alone in the call.
        if self.huddle_features_me() {
            if self.video.latest_frame().is_some() {
                self.video
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            self.draw_host_camera_off(scene, transform, text_renderer, theme);
        } else {
            self.draw_huddle_featured(scene, transform, text_renderer, theme);
        }
        self.draw_huddle_fg(scene, transform, text_renderer, theme, hits);
    }

    /// The detail drawer over (or beside) the conversation: the open thread,
    /// the pins or the workspace's people, over its scrim when it floats.
    fn draw_drawers(
        &mut self,
        parts: &mut PaintParts,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // The detail drawer: the open thread, or the people in the workspace
        // (so the people button always reveals a real panel).
        if self.right_flap_open() || self.right_flap.reveal() > 0.001 {
            if self.thread_is_page() {
                let page = self.thread_rect();
                parts.draw(scene, "chat-thread", self.rect, |scene| {
                    ui::surface::region(scene, tf, page, ui::surface::Region::Content, theme);
                    ChatApp::push_hit(hits, page, Hit::FocusThread);
                    self.draw_thread(scene, tf, tr, theme, presence, hits);
                });
            } else {
                let scrim = self.right_flap.scrim_rect();
                if scrim.width() > 0.5 {
                    ChatApp::push_hit(hits, scrim, Hit::Dismiss);
                }
                let panel = self.right_flap.sidebar_rect();
                if panel.width() > 0.5 {
                    // Clipped to its panel, shadow and all.
                    parts.draw(scene, "chat-detail", panel, |scene| {
                        crate::paint::push_clip(scene, tf, panel);
                        let local = self.right_flap.paint_panel(scene, tf, theme);
                        if self.thread_open.is_some() {
                            self.draw_thread(scene, tf, tr, &local, presence, hits);
                        } else if self.pins_open {
                            self.draw_pins(scene, tf, tr, &local, hits);
                        } else {
                            self.draw_members(scene, tf, tr, &local, hits);
                        }
                        scene.pop_layer();
                    });
                }
            }
        }
        if self.thread_is_page() {
            return;
        }
        parts.draw(scene, "chat-dividers", self.rect, |scene| {
            self.right_flap.paint_resize(scene, tf, theme);
        });
    }

    /// Flag a taffy rebuild (kept for parity; the video rect is now set in
    /// `apply_layout` so most state changes don't need it, but call expand
    /// still requests a clean relayout).
    pub(crate) fn request_relayout(&mut self) {
        self.relayout = true;
        self.canonical_dirty = true;
    }
}

#[cfg(test)]
mod identity_on_screen_tests;

mod call_chrome_motion;
mod floating_surfaces;
#[cfg(test)]
mod live_draft_presence_tests;
mod own_delete;
pub(crate) use call_chrome_motion::CallChromeMotion;
