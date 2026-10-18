//! Root widget for the chat app.
//!
//! [`ChatApp`] owns the CRDT-derived snapshot (workspaces / channels /
//! users / messages / drafts), the per-peer view state (active channel,
//! open thread, call state, composer caret), and one real
//! [`ui::widget::VideoPlayer`] used as the host call tile. It does its own
//! manual layout: the four regions (channel sidebar, channel header,
//! message timeline, composer) plus floating layers
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
use ui::widget::tooltip::TooltipHost;
use ui::widget::{
    Collaborative, ContextMenu as SharedContextMenu, Continuable, FloatingSurfaceProvider,
    MenuItem, SearchField, SidebarDrawer, TextInput, VideoPlayer,
};

use crate::agent_tasks::{AgentComposerIntent, AgentTaskClient, AgentTaskView};
use crate::call::CallState;
use crate::geometry::Chrome;
use crate::identity::LocalIdentity;
use crate::model::{self, Channel, Id, Message, User, Workspace};
use crate::permissions::ChatPermissions;

// --- Floating call huddle home placement (fractions of the window) --------
// The docked huddle's resting video rect before the user drags it. The
// VideoPlayer's taffy node and the surrounding chrome derive from this so
// they stay aligned; the drag offset is applied on top in `apply_layout`.
// (The expanded overlay instead tiles the host into a responsive grid — see
// `overlay_tiles` — so it has no fixed fraction.)
pub(crate) const HUDDLE_VIDEO: (f64, f64, f64, f64) = (0.505, 0.300, 0.250, 0.230);

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
    /// do to this target ("Collapse category", "Unpin message").
    pub(crate) fn labelled(
        target: MenuTarget,
        anchor: Point,
        items: Vec<MenuAction>,
        viewport: Rect,
        label: impl Fn(MenuAction) -> String,
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
                MenuAction::MoveUp => Icon::ChevronUp,
                MenuAction::MoveDown | MenuAction::ToggleCollapse => Icon::ChevronDown,
                MenuAction::NewChannelHere
                | MenuAction::NewVoiceChannelHere
                | MenuAction::NewCategory => Icon::Plus,
                MenuAction::Archive => Icon::Archive,
                MenuAction::PinMessage => Icon::Pin,
                MenuAction::ReportMessage | MenuAction::ReportAuthor => Icon::Flag,
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
    /// The workspace background / the sidebar's "+" — create affordances.
    Workspace(Id),
}

/// The structural actions reachable from a context menu. A left-swipe on a
/// channel row resolves to the channel menu's `Delete`, so the gesture and the
/// menu can never drift (swipe⇔menu parity).
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
}

impl MenuAction {
    pub fn label(self) -> &'static str {
        match self {
            MenuAction::Rename => "Rename",
            MenuAction::EditChannel => "Edit channel",
            MenuAction::Delete => "Delete",
            MenuAction::MoveUp => "Move up",
            MenuAction::MoveDown => "Move down",
            MenuAction::NewChannelHere => "New channel",
            MenuAction::NewVoiceChannelHere => "New voice channel",
            MenuAction::NewCategory => "New category",
            MenuAction::ToggleCollapse => "Collapse category",
            MenuAction::Archive => "Archive channel",
            MenuAction::EditMessage => "Edit message",
            MenuAction::PinMessage => "Pin message",
            MenuAction::DeleteMessage => "Delete message",
            MenuAction::ReportMessage => "Report message",
            MenuAction::ReportAuthor => "Report sender",
            MenuAction::BanAuthor => "Ban from the room",
        }
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
    /// Menu items for the workspace: the sidebar header's "+" and its
    /// background.
    pub fn for_workspace() -> [MenuAction; 2] {
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
    /// The action a swipe-left on a channel row resolves to: the channel
    /// menu's destructive action.
    pub fn channel_swipe_left() -> MenuAction {
        MenuAction::Delete
    }
    /// Menu items for one of your own messages.
    pub fn for_message() -> [MenuAction; 3] {
        [
            MenuAction::EditMessage,
            MenuAction::PinMessage,
            MenuAction::DeleteMessage,
        ]
    }
    /// Menu items for someone else's message: reporting it or its sender,
    /// and removing it, which only the room's moderators may.
    pub fn for_others_message() -> [MenuAction; 3] {
        [
            MenuAction::ReportMessage,
            MenuAction::ReportAuthor,
            MenuAction::DeleteMessage,
        ]
    }
}

/// An in-progress swipe on a channel row (mirrors the context-menu primary
/// action on release).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowSwipe {
    pub channel_index: usize,
    pub origin: Point,
    pub current: Point,
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
    pub(crate) agent: Id,
    pub(crate) channel: Id,
    pub(crate) origin: Point,
}

impl RowSwipe {
    pub fn dx(&self) -> f64 {
        (self.origin.x - self.current.x).max(0.0)
    }
}

/// Everything the user can click. Recorded during `draw`, resolved on the
/// next mouse-press.
#[derive(Clone, Debug)]
pub(crate) enum Hit {
    /// Open the cross-workspace conversation inbox. This is navigation, not a
    /// second kind of Tilo workspace.
    Inbox,
    HostModel(String),
    /// Return from Inbox to channels belonging to the active Tilo workspace.
    CurrentContext,
    Channel(Id),
    Agent(Id),
    DirectPerson(String),
    AgentWindow,
    AgentDragHandle,
    AgentIntent(AgentComposerIntent),
    AgentTask(Box<crate::agent_tasks::AgentTaskFrontier>),
    AgentTaskMore,
    AgentStart,
    AgentStop,
    /// A voice-channel member chip in the sidebar — clicking joins the call.
    VoiceChannel(Id),
    /// Toggle a category's collapsed state.
    CategoryToggle(Id),
    /// The "+" create-channel button on a category header.
    AddChannel {
        workspace: Id,
        category: Id,
    },
    /// The "+" create-category button in the sidebar header.
    AddCategory(Id),
    /// Focus/place the caret in the inline structural rename field.
    FocusRename,
    Reaction {
        msg: Id,
        emoji: String,
    },
    AddReaction {
        msg: Id,
        anchor: Point,
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
    /// Focus/place the caret in the emoji picker's search field.
    FocusEmojiSearch,
    FocusMain,
    FocusThread,
    Send,
    /// Return a scrolled conversation to its newest message without changing
    /// any shared document state.
    JumpToLatest,
    ThreadScroll(bool),
    AgentReasoning(String),
    ComposerEmoji(Point),
    /// Open the workspace file picker to share an attachment.
    Attach,
    /// Pick a specific file (by path) from the open attachment picker.
    AttachPick(String),
    /// Choose files on this device with the platform's file chooser.
    AttachDevice,
    StartCall,
    StopSharingLiveContext,
    CopyResourceLink(Id),
    OpenResourceLink(Id),
    /// Open the platform search over Chat.
    Search,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    CallExpand,
    CallLeave,
    /// Toggle the left flap (the channel sidebar).
    ToggleLeftFlap,
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    /// Show or hide the open channel's pinned messages in the detail drawer.
    TogglePins,
    /// Show a message where it was written (a pinned message's row).
    RevealMessage(Id),
    Dismiss, // click on a scrim closes the top-most popover
}

impl Hit {
    pub(crate) fn semantic_id(&self) -> String {
        match self {
            Self::Inbox => "chat.inbox".into(),
            Self::CurrentContext => "chat.current-context".into(),
            Self::Channel(id) => format!("chat.channel:{id}"),
            Self::DirectPerson(key) => format!("chat.person:{key}"),
            Self::Agent(id) => format!("chat.agent:{id}"),
            Self::AgentWindow => "chat.agent-window".into(),
            Self::AgentDragHandle => "chat.agent-drag".into(),
            Self::AgentIntent(intent) => format!("chat.agent-intent:{intent:?}"),
            Self::AgentTask(task) => format!(
                "chat.agent-task:{}:{}:{}",
                task.task_id,
                task.run_epoch.unwrap_or_default(),
                task.after_revision.unwrap_or_default()
            ),
            Self::AgentTaskMore => "chat.agent-task-more".into(),
            Self::AgentStart => "chat.agent-start".into(),
            Self::AgentStop => "chat.agent-stop".into(),
            Self::VoiceChannel(id) => format!("chat.voice-channel:{id}"),
            Self::CategoryToggle(id) => format!("chat.category-toggle:{id}"),
            Self::AddChannel {
                workspace,
                category,
            } => format!("chat.add-channel:{workspace}:{category}"),
            Self::AddCategory(id) => format!("chat.add-category:{id}"),
            Self::TogglePins => "chat.pins".into(),
            Self::RevealMessage(id) => format!("chat.reveal-message:{id}"),
            Self::FocusMain => "chat.composer".into(),
            Self::FocusThread => "chat.thread-composer".into(),
            Self::FocusRename => "chat.rename".into(),
            Self::FocusEdit => "chat.message-edit".into(),
            Self::HostModel(command) => format!("chat.model.{command}"),
            Self::Send => "chat.send".into(),
            Self::JumpToLatest => "chat.jump-to-latest".into(),
            Self::ThreadScroll(end) => format!("chat.thread-scroll-{end}"),
            Self::AgentReasoning(id) => format!("chat.reasoning.{id}"),
            Self::ComposerEmoji(_) => "chat.composer-emoji".into(),
            Self::Attach => "chat.attach".into(),
            Self::AttachPick(path) => format!("chat.attach-pick:{path}"),
            Self::AttachDevice => "chat.attach-device".into(),
            Self::StartCall => "chat.start-call".into(),
            Self::StopSharingLiveContext => "chat.stop-sharing-live-context".into(),
            Self::CopyResourceLink(id) => format!("chat.copy-resource-link:{id}"),
            Self::OpenResourceLink(id) => format!("chat.open-resource-link:{id}"),
            Self::Search => "chat.search".into(),
            Self::ToggleLeftFlap => "chat.toggle-navigation".into(),
            Self::ToggleRightFlap => "chat.toggle-details".into(),
            Self::OpenThread(id) => format!("chat.open-thread:{id}"),
            Self::CloseThread => "chat.close-thread".into(),
            Self::Reaction { msg, emoji } => format!("chat.reaction:{msg}:{emoji}"),
            Self::AddReaction { msg, .. } => format!("chat.add-reaction:{msg}"),
            Self::EmojiChoice(emoji) => format!("chat.emoji-choice:{emoji}"),
            Self::EmojiGroup(group) => format!("chat.emoji-group:{group}"),
            Self::FocusEmojiSearch => "chat.emoji-search".into(),
            Self::EditHistory { msg, .. } => format!("chat.edit-history:{msg}"),
            Self::EditSave => "chat.edit-save".into(),
            Self::EditCancel => "chat.edit-cancel".into(),
            Self::CallMute => "chat.call-mute".into(),
            Self::CallParticipantsToggle => "chat.call-participants".into(),
            Self::CallExpand => "chat.call-expand".into(),
            Self::CallLeave => "chat.call-leave".into(),
            other => format!("chat.control:{other:?}"),
        }
    }
}

/// One sampled motion value owns the call huddle's participant reveal. The
/// panel silhouette, controls, hit regions, compositor presentation and native
/// window allocation all read this same value, so a content-driven resize can
/// never leave those layers a frame apart.
#[derive(Debug)]
pub(crate) struct CallChromeMotion {
    transition: ui::Transition,
    target_open: bool,
    initialized: bool,
    pub(crate) sampled: f32,
    pub(crate) animating: bool,
    /// Deterministic visual-review hook; production leaves this `None`.
    pinned: Option<f32>,
}

impl Default for CallChromeMotion {
    fn default() -> Self {
        Self {
            transition: ui::Transition::open(),
            target_open: true,
            initialized: false,
            sampled: 1.0,
            animating: false,
            pinned: None,
        }
    }
}

impl CallChromeMotion {
    pub(crate) fn reconcile(&mut self, open: bool, theme: &Theme) {
        if let Some(value) = self.pinned {
            self.sampled = value;
            self.animating = false;
            return;
        }
        if !self.initialized {
            self.transition = if open {
                ui::Transition::open()
            } else {
                ui::Transition::closed()
            };
            self.target_open = open;
            self.initialized = true;
        } else if open != self.target_open {
            let spring = theme.tokens.motion.springs.subtle;
            self.transition.set_open(
                open,
                if theme.reduce_motion {
                    ui::time::Duration::ZERO
                } else {
                    theme.tokens.motion.durations.base()
                },
                ui::Curve::Spring {
                    stiffness: spring.stiffness,
                    damping: spring.damping,
                    mass: spring.mass,
                },
            );
            self.target_open = open;
        }
        let now = ui::time::now();
        self.sampled = self.transition.value(now);
        self.animating = self.transition.is_animating(now);
    }

    pub(crate) fn reset(&mut self) {
        if self.pinned.is_none() {
            self.initialized = false;
            self.animating = false;
        }
    }

    pub(crate) fn pin_for_visual_review(&mut self, value: f32) {
        self.pinned = Some(value.clamp(0.0, 1.0));
        self.sampled = self.pinned.unwrap_or(0.0);
        self.animating = false;
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

#[cfg(test)]
mod call_chrome_motion_tests {
    use super::CallChromeMotion;

    #[test]
    fn call_chrome_motion_lands_without_animating_when_reduced_or_pinned() {
        // Phase: reduced_motion_snaps_the_same_geometry_driver_to_its_target.
        {
            let mut motion = CallChromeMotion::default();
            motion.reconcile(true, &ui::theme::Theme::dark());
            motion.reconcile(false, &ui::theme::Theme::dark().with_reduce_motion(true));
            assert_eq!(motion.sampled, 0.0);
            assert!(!motion.animating);
        }

        // Phase: visual_review_can_pin_a_true_mid_morph_geometry.
        {
            let mut motion = CallChromeMotion::default();
            motion.pin_for_visual_review(0.5);
            motion.reconcile(false, &ui::theme::Theme::dark());
            assert_eq!(motion.sampled, 0.5);
            assert!(!motion.animating);
        }
    }
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
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) channels: Vec<Channel>,
    pub(crate) categories: Vec<crate::model::Category>,
    /// People evidenced in the active chat workspace: the local person plus
    /// authors, reactors, mentioned people, and voice participants in that
    /// workspace's channels. The backing document's `users` map is a global
    /// directory, so exposing it directly here would make people from another
    /// workspace appear in this workspace's People/call chrome.
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
    /// Whole-document snapshots used by global search. The visible `messages`
    /// and `users` remain workspace-scoped projections for rendering.
    pub(crate) search_messages: Vec<Message>,
    pub(crate) search_users: Vec<User>,
    /// The reviewed feedback reports filed here: each one's id and text.
    pub(crate) reports: Vec<(Id, String)>,
    /// The items a search hit or an agent's `open` asked to show, by their
    /// published ids, shown on the next sync.
    pub(crate) opened: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// Voice membership per channel in the active workspace (cached from the
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
    /// The last message posted from the main composer, by channel: put back
    /// in the composer if the room refuses it after it was applied here.
    pub(crate) last_post: Option<(Id, String)>,
    /// The open sheet that makes a channel or a category, or changes a
    /// channel.
    pub(crate) channel_sheet: Option<crate::channel_sheet::ChannelSheet>,
    /// The detail drawer shows the open channel's pinned messages.
    pub(crate) pins_open: bool,
    pub(crate) composer_thread: crate::composer::DraftComposer,

    // Per-peer view state.
    pub(crate) active_ws: Id,
    /// The Tilo workspace Chat was opened in. Inbox may temporarily display a
    /// conversation sourced from another workspace; this anchor lets the
    /// person return to their contextual channels without changing OS state.
    pub(crate) context_ws: Id,
    /// Inbox is a cross-workspace navigation surface, deliberately distinct
    /// from the contextual channel list and from the CRDT's legacy grouping.
    pub(crate) inbox_open: bool,
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
    /// The first layout just happened; request one more model frame so the
    /// channel already in view is marked read without waiting for input.
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
    pub(crate) emoji_anchor: Point,
    /// The picker's query; empty shows the selected group.
    pub(crate) emoji_search: SearchField,
    pub(crate) emoji_group: usize,
    /// How far the picker grid is scrolled, and the rect it scrolls in.
    pub(crate) emoji_scroll: f64,
    pub(crate) emoji_grid: Rect,
    /// This user's recently picked emoji, most recent first.
    pub(crate) emoji_recents: emoji::Recents,
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
    pub(crate) attachment_picker_pending: Option<u64>,
    pub(crate) attachment_picker_generation: u64,
    pub(crate) attachment_picker_result_tx: SyncSender<AttachmentPickerResult>,
    pub(crate) attachment_picker_result_rx: Receiver<AttachmentPickerResult>,

    // Structure editing.
    pub(crate) menu: Option<ContextMenu>,
    /// Channel/category currently being renamed. The shared single-line input
    /// owns the transient working name; structural CRDT policy stays here.
    pub(crate) rename: Option<MenuTarget>,
    pub(crate) rename_input: TextInput,
    pub(crate) swipe: Option<RowSwipe>,

    // Call state.
    pub(crate) call: CallState,
    pub(crate) native_voice: crate::local_voice::LocalVoice,
    pub(crate) resource_navigation: crate::shared_resources::Navigation,
    pub(crate) moderation: crate::moderation::Moderation,
    /// Measured control/header sizes of the expanded call overlay, refreshed
    /// by the paint pass and read by the renderer-less layout functions.
    pub(crate) call_overlay_metrics: crate::render_call::CallOverlayMetrics,
    /// `(renderer scale, theme scale, catalogue revision)` the overlay metrics
    /// were measured at; unchanged means the measurement is skipped.
    pub(crate) call_overlay_metrics_key: Option<(u32, u32, u64)>,
    pub(crate) realtime: crate::continuity::ChatRealtimeContinuity,
    pub(crate) call_chrome_motion: CallChromeMotion,
    /// How many rows the floating call window's participant chips take,
    /// measured by the paint pass.
    pub(crate) huddle_chip_rows: usize,
    pub(crate) pending_floating: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// Authoritative combined-output bounds supplied by the shell. Standalone
    /// app/test hosts leave this empty and fall back to the app rect.
    pub(crate) floating_surface_bounds: Rect,

    /// Opens the platform search when the header's search button is used.
    pub(crate) search_opener: ui::search::SearchOpener,

    // Responsive flaps.
    pub(crate) left_flap: SidebarDrawer,
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
    /// Agent navigation entry; the task client pins the conversation's exact run.
    pub(crate) direct_agent: Option<Id>,
    pub(crate) direct_person: Option<String>,
    pub(crate) person_subscription: Option<uuid::Uuid>,
    pub(crate) person_workspace: Option<String>,
    pub(crate) person_contacts: Vec<ui::person_chat::Contact>,
    pub(crate) person_messages: Vec<ui::person_chat::Message>,
    pub(crate) person_error: Option<String>,
    pub(crate) person_body_views: HashMap<String, (String, crate::composer::MessageEditor)>,
    pub(crate) agent_intents: ui::LocalState<HashMap<(Id, Id), AgentComposerIntent>>,
    pub(crate) agent_tasks: AgentTaskClient,
    /// Retained direct-Agent conversations currently owned by the shared
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
    /// The chat's messages, kept in parts beside the record
    /// ([`crate::parts`]): what the window reads them from and posts, edits
    /// and reacts through. `None` where the window has none (a past
    /// version).
    pub(crate) chain: Option<ui::framework::Chain<crate::parts::ChatPart>>,
    /// The chain's revision the window last read its messages at.
    pub(crate) chain_seen: Option<u64>,
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
    /// One shared tooltip controller for Chat's immediate-mode controls. This
    /// gives icon-only buttons and elided header text the same dwell behaviour
    /// as toolkit widgets without allocating a tooltip per hit region.
    pub(crate) tooltips: TooltipHost,
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

    /// Latest decoded RGBA frame per remote peer id, shown in that peer's call
    /// tile (camera or screen-share). The native voice path carries no video,
    /// so nothing fills it today and a tile shows the honest placeholder; it
    /// never fabricates a picture.
    pub(crate) remote_frames: HashMap<String, vello::peniko::ImageData>,

    /// Latest decoded RGBA **screen-share** frame per remote peer id, kept
    /// separate from their camera frame so a peer can share their screen and
    /// keep their camera on at once — the screen fills the featured stage tile,
    /// the camera stays in that peer's filmstrip cell. The native voice path
    /// carries no video, so it stays empty and the featured tile shows the
    /// honest "connecting" placeholder rather than a fabricated feed.
    pub(crate) remote_screens: HashMap<String, vello::peniko::ImageData>,

    /// The latest call/media problem to show in the call chrome (a refused
    /// control, a native voice issue, or no connected media transport).
    pub(crate) media_error: Option<String>,
    pub(crate) canonical_dirty: bool,
    /// Document heads and pending-operation count at the last reload,
    /// so an unchanged document is not reloaded and repainted every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    pub(crate) host_focused: bool,
    pub(crate) external_services: bool,
}

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
        app.active_ws.clear();
        app.context_ws.clear();
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
        // The host call tile is a real VideoPlayer with no frame until a live
        // capture feeds one; there is no synthesised stand-in. Its honest
        // "camera off" state is painted over the empty surface.
        let video = VideoPlayer::new();
        let (attachment_result_tx, attachment_result_rx) = std::sync::mpsc::sync_channel(1);
        let (attachment_picker_result_tx, attachment_picker_result_rx) =
            std::sync::mpsc::sync_channel(1);
        Self {
            workspaces: Vec::new(),
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
            last_post: None,
            channel_sheet: None,
            pins_open: false,
            composer_thread: crate::composer::DraftComposer::shared(),
            active_ws: "ws_acme".into(),
            context_ws: "ws_acme".into(),
            inbox_open: false,
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
            emoji_anchor: Point::ZERO,
            emoji_search: SearchField::new().with_placeholder(&crate::i18n::t("chat-emoji-search")),
            emoji_group: 0,
            emoji_scroll: 0.0,
            emoji_grid: Rect::ZERO,
            emoji_recents: emoji::Recents::default(),
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
            attachment_picker_pending: None,
            attachment_picker_generation: 0,
            attachment_picker_result_tx,
            attachment_picker_result_rx,
            menu: None,
            rename: None,
            rename_input: Self::new_rename_input(),
            swipe: None,
            call: CallState::default(),
            native_voice: crate::local_voice::LocalVoice::default(),
            resource_navigation: crate::shared_resources::Navigation::default(),
            moderation: crate::moderation::Moderation::default(),
            call_overlay_metrics: crate::render_call::CallOverlayMetrics::default(),
            call_overlay_metrics_key: None,
            realtime: crate::continuity::ChatRealtimeContinuity::default(),
            call_chrome_motion: CallChromeMotion::default(),
            huddle_chip_rows: 0,
            pending_floating: None,
            floating_surface_bounds: Rect::ZERO,
            search_opener: ui::search::SearchOpener::new(),
            left_flap: Self::navigation_drawer(),
            right_flap: Self::detail_drawer(),
            chrome: Chrome::default(),
            layout_text_scale: 1.0,
            me: LocalIdentity::demo(),
            room_names: HashMap::new(),
            room_voice: HashMap::new(),
            room_labels: ui::presence::ParticipantLabels::default(),
            own_label: None,
            room_colors: HashMap::new(),
            room_agents: Vec::new(),
            capture_agent_presence: None,
            direct_agent: None,
            direct_person: None,
            person_subscription: None,
            person_workspace: None,
            person_contacts: Vec::new(),
            person_messages: Vec::new(),
            person_error: None,
            person_body_views: HashMap::new(),
            agent_intents: ui::LocalState::new(HashMap::new()),
            agent_tasks: AgentTaskClient::new(),
            motion_reduced: false,
            agent_surfaces: HashMap::new(),
            agent_surface_requests: VecDeque::new(),
            focused_agent_surface: None,
            input_agent_surface: None,
            conversation_surface: false,
            conversation_return_requested: false,
            agent_conversation_drag: None,
            agent_authors: HashMap::new(),
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
            chain: None,
            chain_seen: None,
            staged_parts: std::collections::VecDeque::new(),
            staged_scenario: None,
            #[cfg(test)]
            last_read: std::rc::Rc::default(),
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
            content,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            keyboard_target: None,
            last_mouse: Point::ZERO,
            call_hovered: None,
            tooltips: TooltipHost::new(),
            relayout: false,
            flap_animating: false,
            announce: None,
            mention_alerts: None,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            media_error: None,
            canonical_dirty: true,
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
    /// parts, each written by its author, once: a chain that holds messages
    /// already is left as it is.
    pub(crate) fn stage_parts(&mut self, scenario: Option<&str>) {
        let Some(chain) = &self.chain else {
            return;
        };
        if chain.head().is_some() {
            return;
        }
        self.staged_parts = crate::resource::part_scenario(scenario).into();
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
            self.composer_thread.attach_drafts(record);
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

    /// Build the reaction action a click on `msg`/`emoji` should dispatch,
    /// deciding add-vs-remove from the currently-loaded state at authoring time.
    /// A toggle is a presentation decision; the resulting action is a pure,
    /// idempotent add or remove — the single source of truth both the reaction
    /// chip and the emoji picker resolve to.
    pub(crate) fn reaction_toggle_action(
        &self,
        msg: &str,
        emoji: &str,
    ) -> ui::tilo_resource::Invoke<crate::parts::ChatPart> {
        use crate::messages::chat_part;
        use crate::parts::ChatPart;
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.reacted_by(&self.me.id))
        });
        let message = ui::tilo_resource::Id::from_raw(msg);
        let emoji = emoji.to_owned();
        if reacting {
            ChatPart::UNREACT.with(chat_part::Unreact { message, emoji })
        } else {
            ChatPart::REACT.with(chat_part::React { message, emoji })
        }
    }

    /// Stage the view state of screenshot `scenario` over the record its
    /// actions wrote, once the frame is laid out. The demo scenarios read the
    /// design-crit conversation; the default is an ordinary reading frame, and
    /// collaboration states are explicit so a capture does not teach a
    /// floating call as Chat's resting state.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        if scenario != "starter" {
            self.active_ws = "ws_acme".into();
            self.context_ws = "ws_acme".into();
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
                // Overlay-collapsed conversation: both flaps closed, no thread,
                // no floating call — the clean single-column reading state.
                self.left_flap.snap(false);
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
            "agent-task"
            | "agent-task-compact"
            | "agent-task-queued"
            | "agent-task-queued-compact" => {
                let mut presence = PresenceCtx::empty();
                let agent = ui::permissions::UserId("agent:scribe".into());
                presence.set_display_name(agent.clone(), "Scribe");
                presence.set_participant_kind(agent, ui::presence::ParticipantKind::Agent);
                self.capture_agent_presence = Some(presence);
                self.room_agents = vec!["scribe".into()];
                self.room_names.insert("scribe".into(), "Scribe".into());
                if scenario.starts_with("agent-task-queued") {
                    self.agent_tasks
                        .stage_queued_capture(&self.active_ws, "scribe");
                } else {
                    self.agent_tasks.stage_capture(&self.active_ws, "scribe");
                }
                self.open_agent_conversation("scribe");
                self.set_direct_agent_intent(AgentComposerIntent::Steer);
                self.left_flap.snap(false);
                self.right_flap.snap(false);
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
                // Two audio streams in the room, so the sidebar shows the
                // people under the voice channel.
                self.native_voice.snapshot.publishers = (1..=2)
                    .map(|index| format!("{}/{index:064x}", "11".repeat(32)))
                    .collect();
                self.refresh_native_streams();
            }
            "starter" => {
                // The real first-run empty workspace (no demo content).
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
                    &self.active_ws,
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
            "inbox" => {
                // Every workspace's conversations, unread first.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.inbox_open = true;
            }
            "emoji" => {
                // The reaction picker over your own last message.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.emoji_open = true;
                self.emoji_target = Some("m_me2".into());
                self.emoji_anchor = Point::new(560.0, 300.0);
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
            self.call_hovered = Some(Hit::CallLeave);
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
                let node = AccessNode::new(Role::ListItem)
                    .with_name(self.model_message_label(message))
                    .with_hashed_key((&message.id, "message"));
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
            .with_name("Messages")
            .with_children(conversation)
            .with_hashed_key("message-list")
    }

    // --- model lookups ----------------------------------------------------
    #[cfg(test)]
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
    pub fn adopt_room(&mut self, presence: &PresenceCtx) {
        self.person_contacts = ui::person_chat::contacts();
        self.adopt_own_participant(presence);
        self.room_voice = self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
            .filter_map(|channel| {
                let key = Self::voice_presence_key(&channel.id);
                let people: Vec<Id> = presence
                    .carets
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .map(|(user, _)| user.0.clone())
                    .collect();
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
        if self
            .direct_agent
            .as_ref()
            .is_some_and(|agent| !self.room_agents.contains(agent))
            && !self.conversation_surface
        {
            self.leave_floated_agent_route();
        }
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

    pub(crate) fn open_agent_conversation(&mut self, agent: &str) -> bool {
        if !self.room_agents.iter().any(|known| known == agent) {
            return false;
        }
        self.close_person_conversation();
        self.direct_agent = Some(agent.to_owned());
        self.active_channel =
            crate::record::agent_conversation_channel_id(&self.active_ws, agent).to_string();
        self.inbox_open = false;
        self.thread_open = None;
        self.focus = Focus::Main;
        self.agent_tasks.observe(&self.active_ws, agent);
        self.sync_agent_route();
        self.scroll = 0.0;
        let channel = self.active_channel.clone();
        self.restore_attached_agent_surface(&channel);
        self.request_relayout();
        true
    }

    pub(crate) fn conversation_instance(parent: &Self, agent: &str) -> Self {
        let mut view = Self::bare(parent.content.clone());
        view.workspaces.clone_from(&parent.workspaces);
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
        view.context_ws.clone_from(&parent.context_ws);
        view.active_ws.clone_from(&parent.active_ws);
        view.direct_agent.clone_from(&parent.direct_agent);
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
        view.person_workspace.clone_from(&parent.person_workspace);
        view.person_contacts.clone_from(&parent.person_contacts);
        view.person_messages.clone_from(&parent.person_messages);
        view.person_error.clone_from(&parent.person_error);
        view.active_channel.clone_from(&parent.active_channel);
        view.agent_intents = parent.agent_intents.clone();
        if view.direct_agent.is_some() {
            view.agent_tasks.observe(&view.active_ws.clone(), agent);
        }
        view.conversation_surface = true;
        view.thread_open = None;
        view.left_flap.snap(false);
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
        let Some(agent) = self
            .direct_agent
            .clone()
            .or_else(|| self.direct_person.clone())
        else {
            return false;
        };
        let available = if self.direct_person.is_some() {
            self.main_composer_can_edit()
        } else {
            self.direct_agent_is_available()
        };
        if !available || self.rect.area() <= 0.0 {
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
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(
                self, &agent,
            );
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
                channel.workspace == self.active_ws
                    && !channel.archived
                    && channel.kind == model::ChannelKind::Text
                    && channel.conversation_with.is_none()
            })
            .min_by_key(|channel| channel.order)
            .map(|channel| channel.id.clone());
        self.direct_agent = None;
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
        let Some(agent) = self
            .direct_agent
            .clone()
            .or_else(|| self.direct_person.clone())
        else {
            return;
        };
        self.agent_conversation_drag = Some(AgentConversationDrag {
            agent,
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
                self,
                &drag.agent,
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
        let person_returned = attached
            && route_is_active
            && matches!(
                surface.conversation,
                crate::composer::ConversationKey::Person { .. }
            );
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
        self.active_ws.clone_from(&surface.view.active_ws);
        channel.clone_into(&mut self.active_channel);
        self.direct_agent.clone_from(&surface.view.direct_agent);
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
        self.person_workspace
            .clone_from(&surface.view.person_workspace);
        self.person_contacts
            .clone_from(&surface.view.person_contacts);
        self.person_messages
            .clone_from(&surface.view.person_messages);
        self.inbox_open = false;
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

    pub(crate) fn direct_agent_is_available(&self) -> bool {
        self.direct_agent
            .as_ref()
            .is_some_and(|agent| self.room_agents.contains(agent))
    }

    pub(crate) fn direct_agent_intent(&self) -> AgentComposerIntent {
        let Some(agent) = &self.direct_agent else {
            return AgentComposerIntent::Queue;
        };
        self.agent_intents.with(|intents| {
            intents
                .get(&(self.active_ws.clone(), agent.clone()))
                .copied()
                .unwrap_or_else(|| {
                    if self
                        .direct_agent_tasks()
                        .and_then(AgentTaskView::current)
                        .is_some_and(|task| task.active_agent.is_some())
                    {
                        AgentComposerIntent::Steer
                    } else {
                        AgentComposerIntent::Queue
                    }
                })
        })
    }

    pub(crate) fn agent_draft_route(&self) -> Option<String> {
        let agent = self.direct_agent.as_ref()?;
        if self.direct_agent_intent() == AgentComposerIntent::Steer {
            return self.direct_agent_tasks()?.draft_route();
        }
        Some(agent.clone())
    }

    pub(crate) fn sync_agent_route(&mut self) {
        if let Some(route) = self.agent_draft_route() {
            self.composer_main.set_agent_draft(&self.active_ws, &route);
            self.draft_main = self.composer_main.private_plain().unwrap_or_default();
        }
    }

    fn settle_agent_drafts(&mut self) {
        for (route, revision) in self.agent_tasks.take_cleared_drafts() {
            self.composer_main
                .clear_private_for_revision(&self.active_ws, &route, revision);
            if self.agent_tasks.view(&route).is_some_and(|view| {
                matches!(view.receipt, crate::agent_tasks::AgentTaskReceipt::Queued)
            }) {
                self.agent_intents.update(|intents| {
                    _ = intents.insert((self.active_ws.clone(), route), AgentComposerIntent::Steer);
                });
            }
        }
        self.sync_agent_route();
    }

    pub(crate) fn set_direct_agent_intent(&mut self, intent: AgentComposerIntent) {
        let Some(agent) = self.direct_agent.clone() else {
            return;
        };
        if self.direct_agent_intent_allowed(intent) {
            self.agent_intents
                .update(|intents| _ = intents.insert((self.active_ws.clone(), agent), intent));
            self.sync_agent_route();
        }
    }

    pub(crate) fn direct_agent_tasks(&self) -> Option<&AgentTaskView> {
        self.agent_tasks.view(self.direct_agent.as_deref()?)
    }

    pub(crate) fn direct_agent_intent_allowed(&self, intent: AgentComposerIntent) -> bool {
        if !self.direct_agent_is_available() {
            return false;
        }
        match intent {
            AgentComposerIntent::Queue => self.direct_agent_tasks().is_some(),
            AgentComposerIntent::Steer => self
                .direct_agent_tasks()
                .is_some_and(AgentTaskView::can_steer),
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
        self.direct_agent.as_ref().map_or_else(
            || self.can_participate(),
            |_| {
                self.direct_agent_is_available()
                    && (self.direct_agent_intent() == AgentComposerIntent::Queue
                        || self
                            .direct_agent_tasks()
                            .and_then(AgentTaskView::selected_identity)
                            .is_some_and(|(_, epoch)| epoch > 0))
            },
        )
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
        if self
            .native_voice
            .snapshot
            .publishers
            .iter()
            .any(|namespace| namespace == id)
        {
            let tail = id.ceil_char_boundary(id.len().saturating_sub(6));
            return format!("Audio stream {}", id.get(tail..).unwrap_or(id));
        }
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
                "chat-agent-task-author",
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
        for id in self.room_names.keys() {
            if !out.iter().any(|p| &p.id == id) {
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

    /// Channels in the active workspace, ordered. Archived channels are hidden
    /// from the sidebar (they stay in the document and are reachable via Ctrl+K).
    pub(crate) fn ws_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.workspace == self.active_ws && !c.archived)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// Categories in the active workspace, ordered.
    pub(crate) fn ws_categories(&self) -> Vec<&crate::model::Category> {
        let mut v: Vec<&crate::model::Category> = self
            .categories
            .iter()
            .filter(|c| c.workspace == self.active_ws)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    pub(crate) fn category(&self, id: &str) -> Option<&crate::model::Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Cached voice members of `channel` (empty when nobody is connected).
    pub(crate) fn voice_in(&self, channel: &str) -> &[crate::model::VoiceMember] {
        self.voice_members
            .get(channel)
            .map_or(&[], std::vec::Vec::as_slice)
    }

    /// Channels in `category` within the active workspace, ordered. Archived
    /// channels are hidden from the sidebar.
    pub(crate) fn category_channels(&self, category: &str) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.workspace == self.active_ws && c.category == category && !c.archived)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// A single-line local field for channel/category names. The host commits
    /// the trimmed value through the target-specific CRDT action on submit.
    fn new_rename_input() -> TextInput {
        TextInput::local().with_enter_submits(true)
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
        let editing = self.editing.is_some()
            || self.rename.is_some()
            || self.channel_sheet.is_some()
            || !self.host_focused;
        // Without an open channel (or an agent to talk to) the main composer
        // has nowhere to send, so it takes no keys.
        let has_target =
            self.direct_agent.is_some() || self.channel(&self.active_channel).is_some();
        let main = matches!(self.focus, Focus::Main) && !editing && has_target;
        let thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some() && !editing;
        self.composer_main.set_focus(main);
        self.composer_thread.set_focus(thread);
        self.edit_input
            .set_focus(self.host_focused && self.editing.is_some());
        ui::Widget::set_focus(
            &mut self.rename_input,
            self.host_focused && self.rename.is_some(),
        );
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
    pub(crate) fn voice_people(&self, channel: &str) -> Vec<Id> {
        let mut people = Vec::new();
        if self.call.active && self.call.channel == channel {
            people.push(self.me.id.clone());
        }
        for person in self.room_voice.get(channel).into_iter().flatten() {
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
            || self.rename.is_some()
            || !self.can_participate()
            || self.direct_agent.is_some()
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
            Hit::FocusRename => {
                let field = &self.rename_input;
                let (anchor, focus) = field
                    .selection_anchor_focus_public()
                    .unwrap_or_else(|| (field.cursor_pos(), field.cursor_pos()));
                Some((
                    ui::Widget::is_focused(field),
                    field.value().to_owned(),
                    (anchor, focus),
                ))
            }
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

    pub(crate) fn reload(&mut self, doc: &AutoCommit) {
        let chat = crate::projection::read(doc);
        #[cfg(test)]
        {
            *self.last_read.borrow_mut() = std::rc::Rc::new(chat.clone());
        }
        let held = self.held_messages();
        self.chain_seen = self.chain.as_ref().map(ui::framework::Chain::revision);
        self.workspaces = chat.workspace_rows();
        self.channels = chat.channel_rows();
        self.categories = chat.category_rows();
        for category in &mut self.categories {
            category.collapsed = self.collapsed.contains(&category.id);
        }
        if !self.workspaces.iter().any(|w| w.id == self.context_ws)
            && let Some(context) = self
                .workspaces
                .iter()
                .find(|w| w.id == self.active_ws)
                .or_else(|| self.workspaces.first())
        {
            self.context_ws = context.id.clone();
        }
        // Keep the active channel valid (e.g. after a workspace switch).
        if self.direct_agent.is_none()
            && self.direct_person.is_none()
            && self
                .channel(&self.active_channel)
                .map(|c| c.workspace.clone())
                != Some(self.active_ws.clone())
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
        // search and the workspace people directory all project from them
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
                let submission = report.submission.0.as_ref()?;
                Some((id.to_string(), submission.feedback.text.clone()))
            })
            .collect();
        // Drop the body views of messages no longer loaded.
        let shown: HashSet<&str> = self.messages.iter().map(|m| m.id.as_str()).collect();
        self.body_views
            .borrow_mut()
            .retain(|(_, id), _| shown.contains(id.as_str()));
        self.agent_authors = chat.agent_authors();
        // Who is in a call is projected only from the authenticated local
        // voice service's response.
        self.refresh_native_streams();
        self.users = self.workspace_people_from(&chat, &per_channel);
        self.sync_reload_composers(doc);
    }

    fn sync_reload_composers(&mut self, doc: &AutoCommit) {
        // Point each composer at its surface's draft; the editors read it on
        // their next sync (`sync_composers`), keeping the caret while typing.
        if self.direct_person.is_some() {
            self.sync_person_draft();
        } else if self.direct_agent.is_some() {
            self.sync_agent_route();
        } else {
            self.composer_main.set_key(&self.active_channel);
            self.composer_main.sync(doc);
            self.draft_main = self.composer_main.value();
        }
        let placeholder = self.direct_agent.as_ref().map_or_else(
            || {
                let channel = self
                    .channel(&self.active_channel)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                format!("Message #{channel}")
            },
            |agent| {
                crate::i18n::t_args(
                    "chat-agent-message-placeholder",
                    &[("name", self.agent_display_name(agent).into())],
                )
            },
        );
        self.composer_main.set_placeholder(&placeholder);
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
        self.composer_thread.set_placeholder("Reply in thread");
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

    /// Project the document-global identity directory onto evidence local to
    /// the active chat workspace. Membership is not stored separately in this
    /// document, so claiming every known identity is "in" the selected
    /// workspace would be dishonest. Channel activity and voice membership
    /// are durable, workspace-owned evidence; the local identity is included
    /// so a brand-new workspace remains a valid one-person surface.
    fn workspace_people_from(
        &self,
        chat: &crate::record::Chat,
        per_channel: &[(Id, Vec<Message>)],
    ) -> Vec<User> {
        let mut ids: HashSet<Id> = HashSet::from([self.me.id.clone()]);
        ids.extend(self.former_ids.iter().cloned());

        for channel in self
            .channels
            .iter()
            .filter(|channel| channel.workspace == self.active_ws)
        {
            let messages = per_channel
                .iter()
                .find(|(id, _)| *id == channel.id)
                .map(|(_, messages)| messages);
            for message in messages.into_iter().flatten() {
                ids.insert(message.author.clone());
                for reaction in &message.reactions {
                    ids.extend(reaction.users.iter().cloned());
                }
                ids.extend(message.mentions.iter().map(|mention| mention.user.clone()));
            }
        }

        let mut people: Vec<_> = chat
            .user_rows()
            .into_iter()
            .filter(|user| ids.contains(&user.id))
            .collect();
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
        let chat = crate::projection::read(doc);
        let color = u32::try_from(me.color_idx).unwrap_or_default();
        if chat.workspaces.is_empty()
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
        let chat = crate::projection::read(doc);
        let read = self
            .held_messages()
            .read_to_end(&ui::tilo_resource::Id::from_raw(
                self.active_channel.clone(),
            ));
        if read <= chat.read_mark(&self.active_channel, &self.me.id) {
            return;
        }
        // Read state is a durable collaboration fact.  It is authored through
        // the same door as the palette's "Mark as read", not a side write
        // that peers/history cannot account for.  The carried position keeps
        // a message that arrives meanwhile unread.
        self.submit(
            crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
                channel: ui::tilo_resource::Id::from_raw(self.active_channel.clone()),
                part: u64::try_from(read.part).unwrap_or_default(),
                seen: u32::try_from(read.seen).unwrap_or(u32::MAX),
            }),
        );
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|channel| channel.id == self.active_channel)
        {
            channel.unread = 0;
        }
    }

    pub(crate) fn reveal_workspace_resource(&mut self, workspace: String) -> Result<(), String> {
        if !self.workspaces.iter().any(|item| item.id == workspace) {
            return Err("workspace no longer exists".into());
        }
        self.active_ws = workspace;
        self.active_channel.clear();
        self.inbox_open = false;
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        Ok(())
    }

    pub(crate) fn reveal_category_resource(
        &mut self,
        workspace: String,
        category: &str,
    ) -> Result<(), String> {
        let Some(category_id) = self
            .categories
            .iter()
            .find(|item| item.id == category && item.workspace == workspace)
            .map(|item| item.id.clone())
        else {
            return Err("category no longer exists".into());
        };
        self.active_ws = workspace;
        self.active_channel = self
            .channels
            .iter()
            .filter(|channel| {
                channel.workspace == self.active_ws
                    && channel.category == category_id
                    && channel.kind == model::ChannelKind::Text
            })
            .min_by_key(|channel| channel.order)
            .map_or_else(String::new, |channel| channel.id.clone());
        self.inbox_open = false;
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        Ok(())
    }

    /// The members roster shown in the right flap when no thread is open: every
    /// user in the workspace paired with the voice channel they're currently in
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
            .map(|u| {
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

    /// Whether this workspace has somebody besides the local participant. An
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
    // Two drawers frame the conversation: the navigation drawer (the
    // workspace's channel list) at the leading edge, and the detail
    // drawer (people, or the open thread) at the trailing edge. The toolkit's
    // `SidebarDrawer` decides, from the window width, whether each docks
    // beside the conversation or floats over it behind a scrim, eases the
    // reveal and remembers the person's show/hide choice per size class.

    /// The leading navigation drawer: the workspace's channel list, at the
    /// shell's sidebar metrics. One workspace is one place, so there is no
    /// rail of other places beside it. It docks only while the conversation
    /// column keeps the shared detail minimum; below that it becomes a
    /// floating drawer.
    fn navigation_drawer() -> SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        SidebarDrawer::new().content_min_width(f64::from(m.detail_min_width))
    }

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

    /// True when the navigation drawer shows (docked, or revealed floating).
    pub(crate) fn left_flap_open(&self) -> bool {
        self.left_flap.is_open()
    }
    /// True when the detail drawer (people / thread) shows.
    pub(crate) fn right_flap_open(&self) -> bool {
        self.right_flap.is_open()
    }
    /// The navigation drawer floats over the conversation (narrow window).
    pub(crate) fn left_overlay(&self) -> bool {
        self.left_flap.is_overlay()
    }
    /// The detail drawer floats over the conversation.
    pub(crate) fn right_overlay(&self) -> bool {
        self.right_flap.is_overlay()
    }

    /// The navigation drawer's panel: header row and channel list.
    pub(crate) fn nav_rect(&self) -> Rect {
        self.left_flap.sidebar_rect()
    }
    /// The channel list, under the header row.
    pub(crate) fn sidebar_rect(&self) -> Rect {
        let list = self.left_flap.list_rect();
        let y0 = (list.y0 + self.chrome.header_h).min(list.y1);
        Rect::new(list.x0, y0, list.x1, list.y1)
    }
    /// The conversation column: what the docked drawers leave.
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
    /// The conversation's part of the window's header row.
    pub(crate) fn header_rect(&self) -> Rect {
        Rect::new(
            self.center_left(),
            self.rect.y0,
            self.center_right(),
            self.rect.y0 + self.chrome.header_h,
        )
    }
    /// Reserve the height measured by the same editor that will be drawn.
    pub(crate) fn composer_height(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let chrome = &self.chrome;
        let pad = chrome.composer_min_h - chrome.body_line;
        let input = (self.composer_main.measured_height() + pad)
            .clamp(chrome.composer_min_h, chrome.composer_max_h);
        if self.direct_agent.is_some() {
            input
                + self.agent_task_controls_height(
                    tr,
                    theme,
                    (self.center_right() - self.center_left() - 2.0 * chrome.gutter).max(0.0),
                )
        } else {
            input
        }
    }
    pub(crate) fn composer_rect(&self, tr: &mut TextRenderer, theme: &Theme) -> Rect {
        let pad = self.chrome.composer_pad;
        // The note strip under the field is always kept, so a line appearing
        // in it never moves the field.
        let bottom = self.rect.y1 - pad.max(self.chrome.composer_note_h);
        let top_limit = self.rect.y0 + self.chrome.header_h + pad;
        let h = self
            .composer_height(tr, theme)
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

    /// The timeline: between the header row and the composer. While the call
    /// window floats over it, the messages keep clear of it and wrap beside
    /// it, so it never covers what is being said.
    pub(crate) fn messages_rect(&self, tr: &mut TextRenderer, theme: &Theme) -> Rect {
        let composer = self.composer_rect(tr, theme);
        let mut rect = Rect::new(
            self.center_left(),
            self.rect.y0 + self.chrome.header_h,
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

    /// Resolve both drawers against the current window: docked or floating,
    /// and the rects they leave the conversation. Cheap and deterministic, so
    /// the event path calls it too. Returns true while either drawer slides.
    pub(crate) fn reconcile_flaps(&mut self) -> bool {
        if self.rect.width() < 1.0 {
            return false;
        }
        let content = self.left_flap.resolve(self.rect);
        self.right_flap.resolve(content);
        self.flap_animating = self.left_flap.is_animating() || self.right_flap.is_animating();
        self.flap_animating
    }

    /// Adopt `theme`'s metrics and advance both drawers' reveals for this
    /// frame (once per paint).
    pub(crate) fn advance_flaps(&mut self, theme: &Theme) {
        self.chrome = Chrome::of(theme);
        self.left_flap.scale_for(theme);
        self.right_flap.scale_for(theme);
        // The window's header row holds the sidebar toggle; a floating
        // drawer opens under it and the row stays live.
        let row = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.chrome.header_h,
        );
        let _ = self.left_flap.header(row, theme);
        if !self.reconcile_flaps() && self.rect.width() < 1.0 {
            return;
        }
        self.left_flap.tick(theme);
        let content = self.left_flap.content_rect();
        self.right_flap.resolve(content);
        self.right_flap.tick(theme);
        self.flap_animating = self.left_flap.is_animating() || self.right_flap.is_animating();
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
        let label = move |action: MenuAction| match action {
            MenuAction::ToggleCollapse if collapsed => "Expand category".to_owned(),
            MenuAction::PinMessage if pinned => "Unpin message".to_owned(),
            MenuAction::Delete if channel => "Delete channel".to_owned(),
            MenuAction::Delete => "Delete category".to_owned(),
            MenuAction::Rename if channel => "Rename channel".to_owned(),
            MenuAction::Rename => "Rename category".to_owned(),
            other => other.label().to_owned(),
        };
        ContextMenu::labelled(target, anchor, items, self.rect, label)
    }

    /// Keep the active workspace / channel / thread pointing at something that
    /// exists — used after seeding a fresh (real) starter whose ids differ
    /// from the demo defaults, and after any structural delete. Returns true
    /// when the active workspace changed (the caller must reload so the
    /// channel + message lists refresh).
    pub(crate) fn ensure_selection(&mut self) -> bool {
        let ws_changed = if !self.workspaces.iter().any(|w| w.id == self.active_ws)
            && let Some(first) = self.workspaces.first()
        {
            self.active_ws = first.id.clone();
            self.context_ws = first.id.clone();
            self.active_channel.clear();
            true
        } else {
            false
        };
        // Drop a stale open thread whose parent isn't in this workspace.
        if let Some(t) = self.thread_open.clone()
            && self.message(&t).is_none()
        {
            self.thread_open = None;
            if self.focus == Focus::Thread {
                self.focus = Focus::Main;
            }
        }
        ws_changed
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
    pub(crate) fn semantic_hits_from(
        &self,
        hits: &[(Rect, Hit)],
    ) -> Vec<(Rect, Hit, String, ui::a11y::Role, bool)> {
        use ui::a11y::Role;

        hits.iter()
            .filter_map(|(rect, hit)| {
                let (name, role, selected) = match hit {
                    Hit::Inbox => {
                        let unread: u32 = self.channels.iter().map(|channel| channel.unread).sum();
                        let suffix = if unread == 0 {
                            String::new()
                        } else {
                            format!(", {unread} unread")
                        };
                        (format!("Inbox{suffix}"), Role::Tab, self.inbox_open)
                    }
                    Hit::CurrentContext => {
                        let name = self
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == self.context_ws)
                            .map_or("Current workspace", |workspace| workspace.name.as_str());
                        (
                            format!("{name} channels, current workspace"),
                            Role::Tab,
                            !self.inbox_open,
                        )
                    }
                    Hit::Channel(id) | Hit::VoiceChannel(id) => {
                        let channel = self.channel(id)?;
                        let source = self
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == channel.workspace)
                            .map_or("Tilo workspace", |workspace| workspace.name.as_str());
                        let unread = if channel.unread == 0 {
                            String::new()
                        } else {
                            format!(", {} unread", channel.unread)
                        };
                        let context = if self.inbox_open {
                            format!(", from {source}")
                        } else {
                            String::new()
                        };
                        (
                            format!("{}{context}{unread}", channel.kind.label(&channel.name)),
                            Role::ListItem,
                            channel.id == self.active_channel,
                        )
                    }
                    Hit::DirectPerson(key) => (self.person_display_name(key), Role::Button, false),
                    Hit::Agent(agent) => (
                        format!(
                            "{}, {}",
                            self.agent_display_name(agent),
                            crate::i18n::t("chat-agent-kind")
                        ),
                        Role::ListItem,
                        self.direct_agent.as_deref() == Some(agent.as_str()),
                    ),
                    Hit::AgentWindow => (
                        if self.conversation_surface {
                            crate::i18n::t("chat-agent-return")
                        } else {
                            crate::i18n::t("chat-agent-float")
                        },
                        Role::Button,
                        false,
                    ),
                    Hit::AgentIntent(intent) => {
                        let label = match intent {
                            AgentComposerIntent::Queue => crate::i18n::t("chat-agent-mode-queue"),
                            AgentComposerIntent::Steer => crate::i18n::t("chat-agent-mode-steer"),
                        };
                        (label, Role::Tab, *intent == self.direct_agent_intent())
                    }
                    Hit::AgentTask(task) => (
                        task.title.clone(),
                        Role::ListItem,
                        self.direct_agent_tasks()
                            .and_then(AgentTaskView::current)
                            .is_some_and(|current| {
                                current.task_id == task.task_id
                                    && current.run_epoch == task.run_epoch
                                    && current.after_revision == task.after_revision
                            }),
                    ),
                    Hit::AgentTaskMore => {
                        (crate::i18n::t("chat-agent-task-next"), Role::Button, false)
                    }
                    Hit::AgentStart => (crate::i18n::t("chat-agent-start"), Role::Button, false),
                    Hit::AgentStop => (crate::i18n::t("chat-agent-stop"), Role::Button, false),
                    Hit::CategoryToggle(id) => {
                        let category = self.categories.iter().find(|category| category.id == *id);
                        let name = category.map_or("category", |category| category.name.as_str());
                        let collapsed = category.is_some_and(|category| category.collapsed);
                        (
                            format!("{} {name}", if collapsed { "Expand" } else { "Collapse" }),
                            Role::ToggleButton,
                            !collapsed,
                        )
                    }
                    Hit::AddChannel { .. } => ("Add channel".into(), Role::Button, false),
                    Hit::AddCategory(_) => {
                        ("Create channel or category".into(), Role::Button, false)
                    }
                    Hit::FocusRename => ("Rename field".into(), Role::TextInput, false),
                    Hit::FocusEdit => ("Edit message".into(), Role::MultilineTextInput, false),
                    Hit::FocusMain => ("Message composer".into(), Role::MultilineTextInput, false),
                    Hit::FocusThread => (
                        "Thread reply composer".into(),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::HostModel(command) => (
                        crate::i18n::t(&format!("chat-model-{command}")),
                        Role::Button,
                        false,
                    ),
                    Hit::Send => ("Send message".into(), Role::Button, false),
                    Hit::JumpToLatest => ("Jump to latest message".into(), Role::Button, false),
                    Hit::ThreadScroll(end) => (
                        crate::i18n::t(if *end {
                            "chat-thread-end"
                        } else {
                            "chat-thread-start"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentReasoning(id) => (
                        crate::i18n::t(if self.expanded_reasoning.contains(id) {
                            "chat-agent-hide-reasoning"
                        } else {
                            "chat-agent-show-reasoning"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::ComposerEmoji(_) => {
                        (crate::i18n::t("chat-emoji-insert"), Role::Button, false)
                    }
                    Hit::Attach => ("Share a file".into(), Role::Button, false),
                    Hit::AttachDevice => ("Upload from this device".into(), Role::Button, false),
                    Hit::AttachPick(path) => (
                        format!(
                            "Attach {}",
                            std::path::Path::new(path)
                                .file_name()
                                .and_then(std::ffi::OsStr::to_str)
                                .unwrap_or(path)
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::StartCall => ("Start or open call".into(), Role::Button, false),
                    Hit::StopSharingLiveContext => {
                        (crate::i18n::t("chat-context-stop"), Role::Button, false)
                    }
                    Hit::CopyResourceLink(message) => (
                        self.resource_navigation.label(message, false, true),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenResourceLink(message) => (
                        self.resource_navigation.label(message, true, true),
                        Role::Button,
                        false,
                    ),
                    Hit::Search => ("Search Chat".into(), Role::Button, false),
                    Hit::ToggleLeftFlap => {
                        ("Show conversation navigation".into(), Role::Button, false)
                    }
                    Hit::ToggleRightFlap => ("Show people or thread".into(), Role::Button, false),
                    Hit::TogglePins => {
                        ("Pinned messages".into(), Role::ToggleButton, self.pins_open)
                    }
                    Hit::RevealMessage(message) => (
                        self.message(message).map_or_else(
                            || "Pinned message".to_owned(),
                            |m| format!("{}: {}", self.user_name(&m.author), m.text),
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenThread(_) => ("Open message thread".into(), Role::Button, false),
                    Hit::CloseThread => (crate::i18n::t("chat-thread-close"), Role::Button, false),
                    Hit::Reaction { msg, emoji } => {
                        let mine = self
                            .message(msg)
                            .and_then(|message| {
                                message
                                    .reactions
                                    .iter()
                                    .find(|reaction| reaction.emoji == *emoji)
                            })
                            .is_some_and(|reaction| reaction.reacted_by(&self.me.id));
                        (
                            crate::i18n::t_args(
                                if mine {
                                    "chat-reaction-remove"
                                } else {
                                    "chat-reaction-add"
                                },
                                &[("emoji", emoji.as_str().into())],
                            ),
                            Role::ToggleButton,
                            mine,
                        )
                    }
                    Hit::AddReaction { .. } => (
                        crate::i18n::t("chat-emoji-add-reaction"),
                        Role::Button,
                        false,
                    ),
                    Hit::EmojiChoice(emoji) => (
                        crate::i18n::t_args(
                            "chat-emoji-choose",
                            &[(
                                "name",
                                crate::i18n::emoji_catalogue()
                                    .get(emoji)
                                    .map_or(emoji.as_str(), |found| found.name())
                                    .into(),
                            )],
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::EmojiGroup(group) => (
                        crate::i18n::emoji_catalogue()
                            .groups()
                            .get(*group)
                            .map_or_else(
                                || crate::i18n::t("chat-emoji-group"),
                                |group| group.label().to_owned(),
                            ),
                        Role::Tab,
                        *group == self.emoji_group,
                    ),
                    Hit::FocusEmojiSearch => {
                        (crate::i18n::t("chat-emoji-search"), Role::TextInput, false)
                    }
                    Hit::EditHistory { .. } => ("Show edit history".into(), Role::Button, false),
                    Hit::EditSave => ("Save message edit".into(), Role::Button, false),
                    Hit::EditCancel => ("Cancel message edit".into(), Role::Button, false),
                    Hit::CallMute => (
                        "Mute microphone".into(),
                        Role::ToggleButton,
                        self.call.muted,
                    ),
                    Hit::CallParticipantsToggle => (
                        "Show call participants".into(),
                        Role::ToggleButton,
                        self.call.participants_expanded,
                    ),
                    Hit::CallExpand => (
                        "Expand or restore call".into(),
                        Role::Button,
                        self.call.expanded,
                    ),
                    Hit::CallLeave => ("Leave call".into(), Role::Button, false),
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

    pub(crate) fn semantic_hits(&self) -> Vec<(Rect, Hit, String, ui::a11y::Role, bool)> {
        Self::deduplicate_semantic_hits(self.semantic_hits_from(&self.hits))
    }

    pub(crate) fn hover_tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        if let Some((rect, hit)) = self.hits.iter().rev().find(|(rect, hit)| {
            rect.contains(point)
                && matches!(
                    hit,
                    Hit::Inbox
                        | Hit::CurrentContext
                        | Hit::Search
                        | Hit::ToggleLeftFlap
                        | Hit::ToggleRightFlap
                        | Hit::TogglePins
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
            return Some((*rect, label));
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
        let header = self.header_rect();
        if header.contains(point)
            && let Some(channel) = self.channel(&self.active_channel)
        {
            let mut label = channel.kind.label(&channel.name);
            if !channel.topic.trim().is_empty() {
                label.push_str(" — ");
                label.push_str(channel.topic.trim());
            }
            return Some((header, label));
        }
        None
    }

    /// A semantic target names an operation, not a particular paint pass. A
    /// message can appear in both timeline and thread briefly; exporting one
    /// retained node keeps that operation's target unambiguous.
    fn deduplicate_semantic_hits(
        hits: Vec<(Rect, Hit, String, ui::a11y::Role, bool)>,
    ) -> Vec<(Rect, Hit, String, ui::a11y::Role, bool)> {
        let mut targets = std::collections::HashSet::new();
        hits.into_iter()
            .filter(|(_, hit, ..)| targets.insert(hit.semantic_id()))
            .collect()
    }
}

/// Compute an absolute rect from window-relative fractions.
pub(crate) fn frac_rect(win: Rect, f: (f64, f64, f64, f64)) -> Rect {
    let x0 = win.x0 + f.0 * win.width();
    let y0 = win.y0 + f.1 * win.height();
    Rect::new(x0, y0, x0 + f.2 * win.width(), y0 + f.3 * win.height())
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
        requests.extend(self.agent_tasks.actions_mut().take_collab_requests());
        for surface in self.agent_surfaces.values_mut() {
            requests.extend(
                surface
                    .view
                    .agent_tasks
                    .actions_mut()
                    .take_collab_requests(),
            );
        }
        let before = self.native_voice.snapshot.state;
        requests.extend(self.native_voice.take_requests(ui::time::now()));
        if before != self.native_voice.snapshot.state {
            self.project_native_voice();
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.agent_tasks.actions_mut().handle_collab_event(event);
        for surface in self.agent_surfaces.values_mut() {
            surface
                .view
                .agent_tasks
                .actions_mut()
                .handle_collab_event(event);
        }
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

impl FloatingSurfaceProvider for ChatApp {
    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
        self.agent_surfaces
            .values()
            .find(|surface| surface.id == id && !surface.is_closed())?
            .accessibility()
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        let Some((source, child_target)) = target
            .strip_prefix("agent-surface:")
            .and_then(|target| target.split_once('|'))
        else {
            return false;
        };
        if source != id {
            return false;
        }
        let Some(surface) = self
            .agent_surfaces
            .values_mut()
            .find(|surface| surface.id == id && !surface.is_closed())
        else {
            return false;
        };
        let handled = surface
            .view
            .perform_semantic_action(child_target, action, state, path);
        self.settle_agent_surface_commands();
        handled
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(ui::ime::InputContent, Rect)> {
        let surface = self
            .agent_surfaces
            .values()
            .find(|surface| surface.id == id && !surface.is_closed())?;
        Some((surface.view.ime_content()?, surface.view.ime_cursor_rect()?))
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.agent_surface_requests
            .pop_front()
            .or_else(|| self.pending_floating.take())
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let desktop = self.desktop_floating_bounds();
        for surface in self.agent_surfaces.values_mut() {
            if surface.is_closed() {
                continue;
            }
            surface.observe_theme(theme);
            surface.view.perms = self.perms.clone();
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_labels.clone_from(&self.room_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            surface.view.canonical_dirty |= surface.view.agent_tasks.pump();
            surface.view.sync_agent_route();
            surface.view.settle_agent_drafts();
            surface.view.refresh_person_conversation();
            if let Some(agent) = surface.view.direct_agent.clone() {
                surface
                    .view
                    .agent_tasks
                    .observe(&surface.view.active_ws.clone(), &agent);
            }
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            ) || surface.placement.is_return_pending()
            {
                let content = surface.placement.rect(surface.source, desktop);
                surface.view.rect = content;
                let view = &mut surface.view;
                ui::floating_surface::paint_app_owned_content(
                    scene,
                    transform,
                    content,
                    theme,
                    |scene| {
                        view.draw(scene, transform, text_renderer, theme, presence);
                        true
                    },
                );
            }
        }
        if !self.call.active
            || self.call.expanded
            || (!self.call.placement.is_return_pending()
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return;
        }
        let mut hits = Vec::new();
        self.draw_live_huddle(scene, transform, text_renderer, theme, presence, &mut hits);
        self.hits.extend(hits);
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        let desktop = self.desktop_floating_bounds();
        if let Some(surface) = self
            .agent_surfaces
            .values_mut()
            .find(|surface| surface.id == id)
        {
            if surface.is_closed() {
                return false;
            }
            surface.observe_theme(theme);
            surface.view.perms = self.perms.clone();
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_labels.clone_from(&self.room_labels);
            surface.view.own_label.clone_from(&self.own_label);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.messages.clone_from(&self.messages);
            surface.view.channels.clone_from(&self.channels);
            surface.view.canonical_dirty |= surface.view.agent_tasks.pump();
            surface.view.sync_agent_route();
            surface.view.settle_agent_drafts();
            surface.view.refresh_person_conversation();
            if let Some(agent) = surface.view.direct_agent.clone() {
                surface
                    .view
                    .agent_tasks
                    .observe(&surface.view.active_ws.clone(), &agent);
            }
            let content = surface.placement.rect(surface.source, desktop);
            surface.view.rect = content;
            let view = &mut surface.view;
            return ui::floating_surface::paint_app_owned_content(
                scene,
                transform,
                content,
                theme,
                |scene| {
                    view.draw(scene, transform, text_renderer, theme, presence);
                    true
                },
            );
        }
        let call_id = format!("chat-call:{}", self.call.channel);
        if id != call_id || !self.call.active {
            return false;
        }
        let mut hits = Vec::new();
        self.draw_live_huddle(scene, transform, text_renderer, theme, presence, &mut hits);
        self.hits.extend(hits);
        true
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        let mut presentations: Vec<_> = self
            .agent_surfaces
            .values()
            .filter_map(|surface| surface.presentation(self.desktop_floating_bounds()))
            .collect();
        let returning_to_app = self.call.placement.is_return_pending();
        if !self.call.active
            || self.call.expanded
            || (!returning_to_app
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return presentations;
        }
        presentations.push(ui::floating_surface::FloatingSurfacePresentation {
            id: format!("chat-call:{}", self.call.channel),
            content_rect: self.call_video_rect(),
            window_rect: self.huddle_panel().inflate(8.0, 8.0),
            input_rect: Some(self.huddle_panel()),
            returning_to_app,
        });
        presentations
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        let mut ids = self
            .agent_surfaces
            .values()
            .filter(|surface| !surface.is_closed())
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            self.call
                .active
                .then(|| format!("chat-call:{}", self.call.channel)),
        );
        ids
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.floating_surface_bounds = bounds;
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        let desktop = self.desktop_floating_bounds();
        let returned = match message {
            ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id }
            | ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id } => {
                Some((id.clone(), false))
            }
            ui::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                Some((id.clone(), true))
            }
            _ => None,
        };
        if self
            .agent_surfaces
            .values_mut()
            .any(|surface| surface.host_message(message, desktop))
        {
            if let Some((id, closed)) = returned {
                self.settle_returned_agent_surface(&id, closed);
            }
            self.canonical_dirty = true;
            return;
        }
        let id = format!("chat-call:{}", self.call.channel);
        if let ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
            id: request_id,
            content_rect,
            transfer_covered,
            coordinate_offset,
            ..
        } = message
            && request_id == &id
            && let Ok(rect) = ui::floating_surface::protocol_rect(*content_rect)
        {
            self.call.placement.prepare_desktop_rect_with_offset(
                rect,
                vello::kurbo::Vec2::new(coordinate_offset[0], coordinate_offset[1]),
            );
            self.call
                .placement
                .set_desktop_transfer_covered(*transfer_covered);
            return;
        }
        if let ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
            id: request_id,
            rect,
        } = message
            && request_id == &id
            && ui::floating_surface::protocol_rect(*rect).is_ok()
        {
            self.call.placement.confirm_desktop_adoption();
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id: request_id }
                | ui::floating_surface::FloatingSurfaceHostMessage::Closed { id: request_id }
                if request_id == &id
        ) {
            let app = self.rect;
            let desktop = self.desktop_floating_bounds();
            self.call.placement.reattach_to_app(app, desktop);
            return;
        }
        if matches!(
            message,
            ui::floating_surface::FloatingSurfaceHostMessage::Suspended { id: request_id }
                if request_id == &id
        ) {
            self.call.placement.confirm_app_adoption(self.rect);
        }
    }

    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        if focused {
            self.focused_agent_surface = id.map(str::to_owned);
        } else if self.focused_agent_surface.as_deref() == id {
            self.focused_agent_surface = None;
        }
        for surface in self.agent_surfaces.values_mut() {
            surface
                .view
                .set_focus(focused && id == Some(surface.id.as_str()));
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        self.focused_agent_surface = id.map(str::to_owned);
        for surface in self.agent_surfaces.values_mut() {
            surface.view.set_focus(id == Some(surface.id.as_str()));
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        self.input_agent_surface = id.map(str::to_owned);
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
    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        // The record is lent to the settle pass: the room's other records
        // take writes now.
        self.flush_staged_parts();
    }

    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::collaboration::participants::Participants,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::view::capability::ViewState
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

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
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
            self.left_flap.set_resize_focused(false);
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
        if self.rename.is_some() {
            self.rename_input.ime_cursor_rect()
        } else if self.editing.is_some() {
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
        self.canonical_dirty |= self.agent_tasks.pump();
        self.settle_agent_drafts();
        if let Some(agent) = self.direct_agent.clone() {
            self.agent_tasks.observe(&self.active_ws, &agent);
            self.sync_agent_route();
        }
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
            // Re-picking the workspace changes which channels/messages apply.
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
        // Adopt the OS-provided permission context so channel participation and
        // file-embed hydration are gated by the real grant policy (the
        // compositor scopes each app's context around its draw every frame).
        if let Some(permissions) = presence.permission_context() {
            self.perms = ChatPermissions::from_context(permissions);
        }
        // Adopt the room's answer for who everybody is. Names and colours are
        // the room's to give: they arrive on the collaboration context stream
        // and land in the presence store, and this is the one door an app has
        // onto that store. Taken first so the same frame that learns a name
        // paints it.
        if let Some(capture) = self.capture_agent_presence.clone() {
            self.adopt_room(&capture);
        } else {
            self.adopt_room(presence);
        }
        // Adopt this theme's chrome and advance the drawers for this frame.
        self.layout_text_scale = theme.text_scale;
        self.advance_flaps(theme);
        self.reconcile_huddle_motion(tr, theme);
        self.tooltips.tick(ui::time::now());

        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        // The conversation stands on the window ground the framework painted.
        // A pushed thread owns the narrow page, including its input targets.
        if !self.thread_is_page() {
            self.draw_conversation(scene, tf, tr, theme, presence, &mut hits);
        }
        self.draw_drawers(scene, tf, tr, theme, presence, &mut hits);

        // Call layers. The host tile is a real VideoPlayer, painted only once
        // a real capture has fed it a frame; otherwise the honest camera-off
        // placeholder covers the host tile.
        let host_has_frame = self.video.latest_frame().is_some();
        if self.call.active && self.call.expanded {
            // Measure the controls and header with this frame's text scale
            // before anything reads their geometry.
            self.measure_call_overlay(tr, theme);
            self.draw_call_overlay_bg(scene, tf, tr, theme);
            if host_has_frame {
                self.video.draw(scene, tf, tr, theme, presence);
            }
            self.draw_host_camera_off(scene, tf, tr, theme);
            self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
        } else if self.app_draws_huddle() {
            self.draw_live_huddle(scene, tf, tr, theme, presence, &mut hits);
        }

        // Popovers over the conversation; the context menu paints in the
        // top layer, from `draw_overlay`.
        if self.emoji_open {
            self.draw_emoji_picker(scene, tf, tr, theme, presence, &mut hits);
        }
        if self.history_open.is_some() {
            self.draw_history_popover(scene, tf, tr, theme, &mut hits);
        }
        if self.attach_open {
            self.draw_attach_picker(scene, tf, tr, theme, &mut hits);
        }

        // Immediate-mode keyboard focus needs a visible counterpart. The ring
        // is painted last so it remains legible on sidebar and popovers.
        let semantic = Self::deduplicate_semantic_hits(self.semantic_hits_from(&hits));
        if self.host_focused
            && let Some(index) = self.keyboard_target
        {
            if let Some((rect, ..)) = semantic.get(index) {
                ui::widget::paint_focus_ring(
                    scene,
                    tf,
                    *rect,
                    f64::from(theme.tokens.radii.item),
                    theme,
                );
            } else {
                self.keyboard_target = None;
            }
        }

        if let Some((label, anchor)) = self.tooltips.visible() {
            ui::widget::tooltip::draw_hover_tooltip(scene, tf, tr, theme, anchor, label, self.rect);
        }

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
        // The navigation drawer takes its toggle, its show/hide shortcut,
        // Escape and a press on its scrim, unless a popover over the
        // conversation is open and dismisses first.
        if self.menu.is_none()
            && self.channel_sheet.is_none()
            && !self.attach_open
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.thread_is_page()
        {
            self.reconcile_flaps();
            if self.left_flap.handle_event(event, ctx) || self.right_flap.handle_event(event, ctx) {
                *ctx.handled = true;
                self.canonical_dirty = true;
                return;
            }
        }
        self.canonical_dirty |=
            self.left_flap.take_resize_redraw() | self.right_flap.take_resize_redraw();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) {
            self.left_flap.set_resize_focused(false);
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
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.left_flap
            .resize_cursor(point)
            .or_else(|| self.right_flap.resize_cursor(point))
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
                if matches!(role, Role::Tab | Role::ListItem) {
                    state = state.selected(selected);
                }
                if matches!(role, Role::ToggleButton) {
                    state = state.checked(selected);
                }
                let mut node = AccessNode::new(role)
                    .with_name(name)
                    .with_state(state)
                    .with_bounds(rect)
                    .with_key(index as u64)
                    .with_action_target(hit.semantic_id());
                if let Some((_, value, (anchor, focus))) = input {
                    node = node.with_value(&value).with_text_selection(anchor, focus);
                }
                node
            })
            .collect();

        let conversation = self.conversation_accessibility();
        let mut children: Vec<AccessNode> = controls;
        children.extend(self.left_flap.resize_accessibility());
        children.extend(self.right_flap.resize_accessibility());
        if let Some(peer) = &self.direct_person {
            children.push(
                AccessNode::new(Role::Heading { level: 1 })
                    .with_name(self.person_display_name(peer))
                    .with_description(crate::i18n::t("chat-person-chat-private"))
                    .with_bounds(self.header_rect())
                    .with_hashed_key(("person-conversation-heading", &self.active_channel)),
            );
        }
        if let Some(agent) = self.direct_agent.as_deref() {
            let mut heading = AccessNode::new(Role::Heading { level: 1 })
                .with_name(self.header_identity_text().name)
                .with_bounds(self.header_rect())
                .with_hashed_key(("agent-conversation-heading", agent));
            if let Some(tasks) = self.direct_agent_tasks() {
                let lines = crate::render_chat::agent_task_status_lines(tasks);
                let description = lines.receipt.map_or_else(
                    || lines.primary.clone(),
                    |receipt| format!("{}\n{receipt}", lines.primary),
                );
                heading = heading.with_description(description);
            }
            children.push(heading);
        } else if let Some(channel) = self.channel(&self.active_channel) {
            let mut heading = AccessNode::new(Role::Heading { level: 1 })
                .with_name(channel.kind.label(&channel.name))
                .with_bounds(self.header_rect())
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
                .with_name("Chat")
                .with_description(if self.inbox_open {
                    "Inbox: conversations across Tilo workspaces"
                } else {
                    "Channels in the current Tilo workspace"
                })
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
                [
                    self.left_flap.resize_accessibility(),
                    self.right_flap.resize_accessibility(),
                ]
                .into_iter()
                .flatten()
                .nth(offset)
                .and_then(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect))
            })
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.semantic_hits().len() {
            let offset = index.saturating_sub(self.semantic_hits().len());
            let target = [
                self.left_flap.resize_accessibility(),
                self.right_flap.resize_accessibility(),
            ]
            .into_iter()
            .flatten()
            .nth(offset)
            .and_then(|node| node.action_target);
            return target.is_some_and(|target| {
                self.perform_drawer_resize(&target, &ui::a11y::Action::Focus)
            });
        }
        self.left_flap.set_resize_focused(false);
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
        if matches!(
            hit,
            Hit::FocusMain | Hit::FocusThread | Hit::FocusRename | Hit::FocusEdit
        ) {
            return self.perform_editor_semantic_action(hit, action, state, path);
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        if let Hit::AgentTask(frontier) = &hit {
            return self.select_agent_task(frontier);
        }
        if matches!(hit, Hit::AgentStart) {
            return self.start_agent_task();
        }
        if matches!(hit, Hit::AgentStop) {
            return self.stop_agent_task();
        }
        if matches!(hit, Hit::Send) && self.direct_person.is_some() {
            return self.send_person_focused();
        }
        if matches!(hit, Hit::Send) && self.direct_agent.is_some() {
            return self.send_agent_focused(state);
        }
        let messages_before = matches!(hit, Hit::Send).then(|| {
            crate::projection::read(state)
                .message_rows(&self.held_messages(), &self.active_channel)
                .len()
        });
        self.on_hit(hit, state);
        messages_before.is_none_or(|before| {
            crate::projection::read(state)
                .message_rows(&self.held_messages(), &self.active_channel)
                .len()
                == before.saturating_add(1)
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
        // Re-run `sync` once after the first layout so the channel already in
        // view acquires its read receipt on open.
        std::mem::take(&mut self.read_on_present) || model_changed
    }

    fn needs_animation_frame(&self) -> bool {
        self.read_on_present
            || self.host_model.needs_frame()
            || self.resource_navigation.pending()
            || (self.thread_open.is_some() && self.thread_scroll.needs_animation_frame())
            || self.flap_animating
            || self.call_chrome_motion.animating
            || self.call.placement.is_dragging()
            || self.call.placement.is_handoff_animating()
            || self.agent_surfaces.values().any(|surface| {
                surface.placement.is_dragging() || surface.placement.is_handoff_animating()
            })
            || self.tooltips.needs_frame()
            || !self.images.is_idle()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.direct_agent
            .as_deref()
            .and_then(|agent| self.agent_tasks.next_refresh_deadline(agent))
            .into_iter()
            .chain(
                self.agent_surfaces
                    .values()
                    .filter(|surface| !surface.is_closed())
                    .filter_map(|surface| {
                        let agent = surface.view.direct_agent.as_deref()?;
                        surface.view.agent_tasks.next_refresh_deadline(agent)
                    }),
            )
            .min()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.left_flap.is_resizing()
            || self.right_flap.is_resizing()
            || self.call.placement.is_dragging()
            || self.agent_conversation_drag.is_some()
            || (self.thread_open.is_some() && self.thread_scroll.wants_pointer_capture())
    }

    /// Attachment thumbnails still decoding: a capture waits for them.
    fn has_pending_visual_work(&self) -> bool {
        !self.images.is_idle()
    }

    fn needs_redraw(&self) -> bool {
        self.canonical_dirty || (self.thread_open.is_some() && self.thread_scroll.needs_redraw())
    }
    fn mark_drawn(&mut self) {
        self.canonical_dirty = false;
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
                Hit::FocusRename => {
                    if self.rename.is_none() {
                        return false;
                    }
                    if !ui::Widget::perform_action(&mut self.rename_input, action, &[], state, path)
                    {
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
            self.left_flap.set_resize_focused(false);
            self.right_flap.set_resize_focused(false);
        }
        let handled = self.left_flap.perform_resize_action(target, action)
            || self.right_flap.perform_resize_action(target, action);
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
            (self.call.active && !self.call.channel.is_empty()).then(|| {
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
        if self.video.latest_frame().is_some() {
            self.video
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.draw_host_camera_off(scene, transform, text_renderer, theme);
        self.draw_huddle_fg(scene, transform, text_renderer, theme, hits);
    }

    /// The two drawers over (or beside) the conversation: the detail drawer
    /// (the open thread, or the workspace's people), the navigation drawer
    /// (the channel list), each over its scrim when it floats, and the
    /// sidebar toggle.
    fn draw_drawers(
        &mut self,
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
                ui::surface::region(scene, tf, page, ui::surface::Region::Content, theme);
                ChatApp::push_hit(hits, page, Hit::FocusThread);
                self.draw_thread(scene, tf, tr, theme, presence, hits);
            } else {
                let scrim = self.right_flap.scrim_rect();
                if scrim.width() > 0.5 {
                    self.right_flap.draw_scrim(scene, tf, theme);
                    ChatApp::push_hit(hits, scrim, Hit::Dismiss);
                }
                let panel = self.right_flap.sidebar_rect();
                if panel.width() > 0.5 {
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
                }
            }
        }

        // The navigation drawer: the workspace's channel list.
        // A thread pushed over a narrow window is the whole page.
        if self.thread_is_page() {
            return;
        }
        let scrim = self.left_flap.scrim_rect();
        if scrim.width() > 0.5 {
            self.left_flap.draw_scrim(scene, tf, theme);
            ChatApp::push_hit(hits, scrim, Hit::Dismiss);
        }
        let nav = self.nav_rect();
        let shown = self.left_flap_open() || self.left_flap.reveal() > 0.001;
        if shown && nav.width() > 0.5 && nav.x1 > self.rect.x0 {
            crate::paint::push_clip(scene, tf, nav);
            let local = self.left_flap.paint_panel(scene, tf, theme);
            self.draw_nav_header(scene, tf, tr, &local, hits);
            self.draw_sidebar(scene, tf, tr, &local, presence, hits);
            scene.pop_layer();
        }

        // The sidebar toggle: at the drawer's top-leading corner while it
        // shows, else at the conversation header's leading edge.
        self.draw_flap_toggles(scene, tf, tr, theme, hits);
        self.left_flap.paint_resize(scene, tf, theme);
        self.right_flap.paint_resize(scene, tf, theme);
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
mod identity_on_screen {
    use super::*;
    use ui::widget::Widget;

    /// A window's person joins the roster through `introduce`.
    #[test]
    fn a_live_peer_joins_through_the_user_action() {
        let mut app = ChatApp::new(
            crate::identity::LocalIdentity::for_participant("person-1"),
            ChatRuntime::Unavailable,
            ContentStore::memory(),
        )
        .announcing();
        app.attach_framework(ui::framework::Dispatcher::detached(
            "so.tilo.chat",
            "person-1",
        ));
        let mut state = ui::State::new();
        crate::testing::starter(&mut state.doc, "owner", "Owner", "OW");
        Widget::sync(&mut app, &state.doc, &state.root);
        app.land(&mut state.doc);
        let recorded = app
            .last_dispatched()
            .expect("the live peer is submitted as introduce");
        assert_eq!(
            serde_json::to_value(&recorded).unwrap(),
            crate::testing::operation(&crate::resource::Chat::INTRODUCE.with(
                crate::record::chat::Introduce {
                    name: app.me.name.clone(),
                    initials: app.me.initials.clone(),
                    color: u32::try_from(app.me.color_idx).unwrap(),
                }
            ))
        );
        let joined = crate::testing::users(&state.doc)
            .into_iter()
            .find(|user| user.id == app.me.id)
            .expect("the window's person is in the roster");
        assert_eq!(
            (joined.name, joined.initials, joined.status),
            (
                app.me.name.clone(),
                app.me.initials.clone(),
                crate::model::Presence::Online
            )
        );
    }

    #[test]
    fn primary_scroll_maps_bottom_anchor_and_clamps_without_editing() {
        use ui::widget::PrimaryScrollAction;
        let mut app = ChatApp::new_ephemeral();
        app.scroll_content = 1000.0;
        app.scroll_viewport = 200.0;
        app.scroll = 0.0;
        assert_eq!(app.primary_vertical_scroll().unwrap().metrics.offset, 800.0);
        app.apply_primary_vertical_scroll(PrimaryScrollAction::Start);
        assert_eq!(app.scroll, 800.0);
        assert!(!app.timeline_at_end);
        app.apply_primary_vertical_scroll(PrimaryScrollAction::Line(1));
        assert_eq!(app.scroll, 752.0);
        app.apply_primary_vertical_scroll(PrimaryScrollAction::End);
        assert_eq!(app.scroll, 0.0);
        assert!(app.timeline_at_end);
        app.scroll_content = 100.0;
        assert_eq!(
            app.primary_vertical_scroll().unwrap().metrics.max_scroll(),
            0.0
        );
    }

    fn user(id: &str, is_agent: bool) -> User {
        User {
            id: id.into(),
            name: id.into(),
            initials: "U".into(),
            color_idx: 0,
            status: crate::model::Presence::Online,
            is_agent,
        }
    }

    #[test]
    fn a_remote_saved_you_name_is_not_the_current_reader() {
        use ui::widget::Widget as _;
        let mut app = ChatApp::new_ephemeral();
        let mut state = ui::State::new();
        let stranger = "peer@9007495404254172154";
        let mut room = crate::testing::Room::default();
        crate::testing::starter(&mut room, stranger, "You", "Y");
        crate::testing::say(&mut room, stranger, "c_general", "Retained foreign message");
        state.doc = room.chat.clone();
        app.attach_room(&room);
        app.sync(&state.doc, &state.root);
        assert_eq!(
            app.user(stranger)
                .expect("observe the actual foreign author in the production roster")
                .name,
            "You"
        );
        let shown = app.user_name(stranger);
        assert_ne!(
            shown, "You",
            "another person's stored name must not read as the current reader"
        );
        assert_eq!(
            app.display_name(stranger),
            shown,
            "mentions and message headers name the same foreign person"
        );
        assert!(
            shown.contains("2154"),
            "ambiguous names must distinguish the actual participant"
        );
        assert_eq!(app.user_name(&app.me.id), crate::identity::DEMO_NAME);
        assert_eq!(
            crate::testing::users(&state.doc)[0].name,
            "You",
            "presentation must not rewrite saved person data"
        );
    }

    #[test]
    fn people_on_screen_are_named_honestly_and_you_by_your_name() {
        // Phase: roster_vocabulary_never_calls_an_agent_a_person.
        {
            let mut app = ChatApp::new_ephemeral();
            app.users = vec![user("u_me", false), user("u_scribe", true)];
            assert!(
                app.has_other_collaborator(),
                "the agent is another collaborator"
            );
            assert_eq!(app.roster_label(), "Collaborators");

            app.users = vec![user("u_me", false), user("u_aria", false)];
            assert_eq!(app.roster_label(), "People");
        }

        // Phase: an_unnamed_participant_is_not_shown_their_routing_token.
        {
            let app = ChatApp::new_ephemeral();
            let stranger = "peer@9007495404254172154";
            let shown = app.user_name(stranger);
            assert!(
                !shown.contains("9007495404254172154"),
                "the id leaked onto the screen as `{shown}`"
            );
            assert!(
                shown.starts_with("Someone"),
                "an unnamed peer should read as somebody unnamed, got `{shown}`"
            );
            assert!(
                shown.contains("2154"),
                "two unnamed peers have to be distinguishable, got `{shown}`"
            );
        }

        // Phase: your_own_name_is_known_before_the_room_has_heard_of_you.
        {
            let mut app = ChatApp::new_ephemeral();
            app.me.name = "ada".to_string();
            app.users.clear();
            app.room_names.clear();
            assert_eq!(
                app.display_name(&app.me.id.clone()),
                "ada",
                "the name this process worked out for itself must be reachable",
            );
        }

        // Phase: the_person_reading_the_screen_is_shown_their_name.
        {
            let app = ChatApp::new_ephemeral();
            let me = app.me.id.clone();
            assert_eq!(app.user_name(&me), crate::identity::DEMO_NAME);
        }

        // Phase: people_sharing_a_name_are_numbered_as_the_room_numbers_them.
        {
            use ui::permissions::UserId;
            let mut app = ChatApp::new_ephemeral();
            let me = app.me.id.clone();
            let mut presence = PresenceCtx::default();
            presence.set_self_participant(UserId(me.clone()), "Sam");
            presence.set_display_name(UserId("a-peer".into()), "sam");
            presence.set_display_name(UserId("z-peer".into()), "Kim");
            app.adopt_room(&presence);
            assert_eq!(app.user_name("a-peer"), "sam (1)");
            assert_eq!(app.user_name(&me), "Sam (2)");
            assert_eq!(app.user_name("z-peer"), "Kim");
            let offered: Vec<String> = app.participants().into_iter().map(|p| p.name).collect();
            assert!(
                offered.contains(&"sam (1)".to_owned()),
                "the @ picker offers the numbered name, got {offered:?}"
            );

            presence.set_display_name(UserId("a-peer".into()), "Alex");
            app.adopt_room(&presence);
            assert_eq!(app.user_name("a-peer"), "Alex");
            assert_eq!(
                app.user_name(&me),
                "Sam",
                "the number goes once nobody shares it"
            );
        }
    }
}

#[cfg(test)]
mod live_draft_presence {
    use super::*;
    use ui::PresencePayload;
    use ui::permissions::UserId;

    fn focused_app(text: &str) -> ChatApp {
        let mut app = ChatApp::new_ephemeral();
        app.active_channel = "c1a2b3c4-0000-4000-8000-00000000c0de".into();
        app.channels = vec![crate::model::Channel {
            id: app.active_channel.clone(),
            workspace: String::new(),
            category: String::new(),
            name: "general".into(),
            topic: String::new(),
            kind: crate::model::ChannelKind::Text,
            order: 0,
            unread: 0,
            mentions: 0,
            archived: false,
            conversation_with: None,
        }];
        app.rect = Rect::new(0.0, 0.0, 1100.0, 720.0);
        let mut doc = AutoCommit::new();
        app.composer_main.set_key(&app.active_channel);
        app.composer_main.ensure_draft(&doc);
        app.composer_main
            .insert_text(&mut ui::widget::Edits::lend(&mut doc), text);
        app.composer_main.sync(&doc);
        Widget::set_focus(&mut app, true);
        app
    }

    /// The composer tells the room that its person is writing there, while
    /// they are, and never what or where in their draft.
    #[test]
    fn focused_composer_publishes_only_that_its_person_is_writing() {
        let mut app = focused_app("hello team");
        let key = ChatApp::draft_presence_key(&app.active_channel);
        app.composer_main.select(5, 5);
        assert_eq!(
            ui::presence::capability::presence_payload(&app),
            Some((key.clone(), PresencePayload::Caret { pos: 0 }))
        );
        app.composer_main.select(0, 5);
        assert_eq!(
            ui::presence::capability::presence_payload(&app),
            Some((key, PresencePayload::Caret { pos: 0 })),
            "a selection in the draft is not the room's either"
        );
        Widget::set_focus(&mut app, false);
        assert_eq!(ui::presence::capability::presence_payload(&app), None);
        let mut idle = focused_app("");
        assert_eq!(
            ui::presence::capability::presence_payload(&idle),
            None,
            "an empty composer is not someone writing"
        );
        Widget::set_focus(&mut idle, false);
    }

    /// Someone writing in the same channel shows under the composer; someone
    /// writing in another channel does not.
    #[test]
    fn someone_writing_here_shows_as_typing() {
        let app = focused_app("hello team");
        let key = ChatApp::draft_presence_key(&app.active_channel);
        assert_eq!(
            crate::render_chat::typing_line(&PresenceCtx::empty(), &key),
            None
        );
        let mut presence = PresenceCtx::empty();
        presence.set_display_name(UserId("ada".into()), "Ada");
        presence.set_caret(&key, UserId("ada".into()), 0);
        assert_eq!(
            crate::render_chat::typing_line(&presence, &key).as_deref(),
            Some("Ada is typing…")
        );
        let mut elsewhere = PresenceCtx::empty();
        elsewhere.set_caret(
            &ChatApp::draft_presence_key("other"),
            UserId("ada".into()),
            0,
        );
        assert_eq!(crate::render_chat::typing_line(&elsewhere, &key), None);
    }
}
