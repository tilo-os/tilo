//! Root widget for the chat app.
//!
//! [`ChatApp`] owns the CRDT-derived snapshot (workspaces / channels /
//! users / messages / drafts), the per-peer view state (active channel,
//! open thread, call state, composer caret), and one real
//! [`ui::widget::VideoPlayer`] used as the host call tile. It does its own
//! manual layout: the five regions (workspace rail, channel sidebar,
//! channel header, message timeline, composer) plus floating layers
//! (thread panel, emoji picker, edit-history popover, call huddle /
//! expanded overlay) are computed from `self.rect` each frame, and every
//! clickable area is recorded into `self.hits` for the next event.
//!
//! Convergent state lives in the Automerge document; this widget is a
//! projection + an input router, in keeping with the toolkit's
//! "state lives in the CRDT" contract.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender};
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
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::tooltip::TooltipHost;
use ui::widget::{
    Collaborative, ContextMenu as SharedContextMenu, Continuable, FloatingSurfaceProvider,
    MenuItem, SearchField, SidebarDrawer, TextInput, VideoPlayer,
};

use crate::actions::{self, ChatAction};
use crate::call::CallState;
use crate::doc::ChatDoc;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentComposerIntent {
    #[default]
    Message,
    Queue,
    Steer,
}

/// Everything from the authenticated Task projection that can change the
/// retained Agent pane. This stays typed so polling does not serialize a JSON
/// tree on every idle deadline, and so adding a rendered status or receipt
/// requires the repaint comparison to account for it explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentTaskPresentationSnapshot {
    choice: ui::agent_task_frontier::AgentTaskChoice,
    pending: Vec<ui::agent_task_frontier::AgentPendingTask>,
    pending_complete: bool,
    controls: AgentTaskPresentationControls,
    discovery: ui::agent_task_frontier::AgentTaskDiscovery,
    direction: ui::agent_task_frontier::AgentDirectionState,
    queue: ui::agent_task_frontier::AgentQueueState,
    stop: ui::agent_task_frontier::AgentStopState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AgentTaskPresentationControls {
    queue: bool,
    steer: bool,
    stop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentMessageProgressPresentation {
    original_invocation: uuid::Uuid,
    request_row: String,
    progress: Option<ui::agent_message_progress::AgentMessagePublicProgress>,
    unavailable: bool,
    refreshing_unavailable: bool,
}

#[derive(Clone)]
pub(crate) struct AgentSubmission {
    pub(crate) text: String,
    pub(crate) draft_revision: u64,
    pub(crate) intent: AgentComposerIntent,
    pub(crate) message: Option<AgentMessageSubmission>,
    pub(crate) give_up_at: ui::time::Instant,
    /// Stable owner-poll anchor. Deadline getters must not move an outstanding
    /// catalogue poll into the future every time the native loop inspects it.
    pub(crate) next_poll_at: Option<ui::time::Instant>,
    pub(crate) pages_remaining: u16,
}

pub(crate) use ui::agent_message_action::{AgentMessagePoll, AgentMessageSubmission};
pub(crate) const AGENT_MESSAGE_RESOLVE_WINDOW: std::time::Duration =
    ui::agent_message_action::RESOLVE_WINDOW;
pub(crate) const AGENT_MESSAGE_MAX_PAGES: u16 = ui::agent_message_action::MAX_PAGES;
const AGENT_MESSAGE_POLL_CADENCE: std::time::Duration = std::time::Duration::from_millis(50);

type AgentSubmissionKey = (Id, Id);
type AgentMessageWakeUpdate = (AgentSubmissionKey, Option<ui::time::Instant>);
type AgentMessageAdvanceUpdate = (
    AgentSubmissionKey,
    Box<AgentMessageSubmission>,
    u16,
    Option<ui::time::Instant>,
);

enum AgentTaskSubmissionOutcome {
    Pending,
    Succeeded,
    Failed,
}

enum AgentMessageWakeDecision {
    Unchanged,
    Replace(Option<ui::time::Instant>),
}

#[derive(Clone, Debug)]
pub(crate) struct AgentConversationDrag {
    pub(crate) workspace: Id,
    pub(crate) agent: Id,
    pub(crate) channel: Id,
    pub(crate) origin: Point,
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
                MenuAction::Delete | MenuAction::DeleteMessage => Icon::Trash,
                MenuAction::MoveUp => Icon::ChevronUp,
                MenuAction::MoveDown | MenuAction::ToggleCollapse => Icon::ChevronDown,
                MenuAction::NewChannelHere
                | MenuAction::NewVoiceChannelHere
                | MenuAction::NewCategory => Icon::Plus,
                MenuAction::Archive => Icon::Archive,
                MenuAction::PinMessage => Icon::Pin,
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
    Delete,
    MoveUp,
    MoveDown,
    NewChannelHere,
    NewVoiceChannelHere,
    NewCategory,
    ToggleCollapse,
    /// Archive / unarchive a channel — hides it from the sidebar (reversible
    /// from Ctrl+K). Dispatches [`ChatAction::ArchiveChannel`](crate::actions::ChatAction).
    Archive,
    /// Load one of your own messages into the inline editor.
    EditMessage,
    /// Pin / unpin one of your own messages (shared truth every peer sees).
    PinMessage,
    /// Delete one of your own messages.
    DeleteMessage,
}

impl MenuAction {
    pub fn label(self) -> &'static str {
        match self {
            MenuAction::Rename => "Rename",
            MenuAction::Delete => "Delete",
            MenuAction::MoveUp => "Move up",
            MenuAction::MoveDown => "Move down",
            MenuAction::NewChannelHere => "New text channel",
            MenuAction::NewVoiceChannelHere => "New voice channel",
            MenuAction::NewCategory => "New category",
            MenuAction::ToggleCollapse => "Collapse category",
            MenuAction::Archive => "Archive channel",
            MenuAction::EditMessage => "Edit message",
            MenuAction::PinMessage => "Pin message",
            MenuAction::DeleteMessage => "Delete message",
        }
    }
    pub fn is_destructive(self) -> bool {
        matches!(self, MenuAction::Delete | MenuAction::DeleteMessage)
    }
    /// Menu items for a channel row, in order: the destructive one last.
    pub fn for_channel() -> [MenuAction; 5] {
        [
            MenuAction::Rename,
            MenuAction::Archive,
            MenuAction::MoveUp,
            MenuAction::MoveDown,
            MenuAction::Delete,
        ]
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
/// hostile workspace must not stall the composer click.
pub(crate) enum AttachmentPickerResult {
    Ready {
        generation: u64,
        entries: Vec<crate::picker::PickEntry>,
    },
    Failed {
        generation: u64,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct AttachmentImportPending {
    pub token: u64,
    pub channel: Id,
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
    /// Return from Inbox to channels belonging to the active Tilo workspace.
    CurrentContext,
    Channel(Id),
    /// Open a direct conversation with an Agent authenticated by the room.
    Agent(Id),
    /// Move the open direct conversation to a secondary surface, or return it.
    AgentWindow,
    /// Shared floating-surface drag handle for a direct Agent conversation.
    AgentDragHandle,
    /// Choose what the direct-agent composer submits through its canonical
    /// domain action.
    AgentIntent(AgentComposerIntent),
    /// Select the exact owner-published active frontier that was painted.
    AgentTask(Box<ui::agent_task_frontier::AgentTaskFrontier>),
    /// Open the existing Task Center, optionally selecting this exact task.
    AgentTaskCenter(Option<Id>),
    /// Request cancellation of the exact selected active run.
    AgentStop,
    /// Retry only the bounded public reply-status read for visible requests.
    AgentMessageRefresh,
    /// Expand or collapse the factual action record on an Agent reply.
    AgentWorkToggle(Id),
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
    ComposerEmoji(Point),
    /// Open the workspace file picker to share an attachment.
    Attach,
    /// Pick a specific file (by path) from the open attachment picker.
    AttachPick(String),
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
    /// Toggle the left flap (rail + channel sidebar).
    ToggleLeftFlap,
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    Dismiss, // click on a scrim closes the top-most popover
}

impl Hit {
    pub(crate) fn semantic_id(&self) -> String {
        match self {
            Self::Inbox => "chat.inbox".into(),
            Self::CurrentContext => "chat.current-context".into(),
            Self::Channel(id) => format!("chat.channel:{id}"),
            Self::Agent(id) => format!("chat.agent:{id}"),
            Self::AgentWindow => "chat.agent-window".into(),
            Self::AgentDragHandle => "chat.agent-drag-handle".into(),
            Self::AgentIntent(intent) => format!("chat.agent-intent:{intent:?}"),
            Self::AgentTask(frontier) => format!(
                "chat.agent-task:{}:{}:{}",
                frontier.task_id, frontier.run_epoch, frontier.after_revision
            ),
            Self::AgentTaskCenter(Some(id)) => format!("chat.agent-task-center:{id}"),
            Self::AgentTaskCenter(None) => "chat.agent-task-center".into(),
            Self::AgentStop => "chat.agent-stop".into(),
            Self::AgentMessageRefresh => "chat.agent-message-refresh".into(),
            Self::AgentWorkToggle(id) => format!("chat.agent-work:{id}"),
            Self::VoiceChannel(id) => format!("chat.voice-channel:{id}"),
            Self::CategoryToggle(id) => format!("chat.category-toggle:{id}"),
            Self::AddChannel {
                workspace,
                category,
            } => format!("chat.add-channel:{workspace}:{category}"),
            Self::AddCategory(id) => format!("chat.add-category:{id}"),
            Self::FocusMain => "chat.composer".into(),
            Self::FocusThread => "chat.thread-composer".into(),
            Self::FocusRename => "chat.rename".into(),
            Self::FocusEdit => "chat.message-edit".into(),
            Self::Send => "chat.send".into(),
            Self::JumpToLatest => "chat.jump-to-latest".into(),
            Self::ComposerEmoji(_) => "chat.composer-emoji".into(),
            Self::Attach => "chat.attach".into(),
            Self::AttachPick(path) => format!("chat.attach-pick:{path}"),
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
    /// Message rows intersecting the last painted timeline viewport. Render
    /// cache only; used to keep a committed parent milestone until its exact
    /// canonical reply is actually visible.
    pub(crate) visible_message_rows: HashSet<Id>,
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
    /// writes through [`ChatDoc::edit_message`].
    pub(crate) editing: Option<Id>,
    pub(crate) edit_input: crate::composer::MessageEditor,

    // Attachment picker (browses the shared workspace for a file to share).
    pub(crate) attach_open: bool,
    pub(crate) attach_entries: Vec<crate::picker::PickEntry>,
    /// Exactly one import may run at a time. This is a deliberate resource
    /// bound: selecting another file while one is reading is refused rather
    /// than creating unbounded reader threads and byte buffers.
    pub(crate) attachment_pending: Option<AttachmentImportPending>,
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
    /// Direct Agent conversations retained while shell-owned secondary
    /// surfaces present them. Keyed by their deterministic conversation id.
    pub(crate) agent_surfaces:
        HashMap<Id, crate::agent_conversation_surface::AgentConversationSurface>,
    pub(crate) agent_surface_requests: VecDeque<ui::floating_surface::FloatingSurfaceRequest>,
    pub(crate) focused_agent_surface: Option<String>,
    /// Secondary editor whose exact platform handoff still uses the primary
    /// window as its text-input source. This is distinct from native child
    /// focus: the child is intentionally unfocused while a preedit is live.
    pub(crate) ime_source_agent_surface: Option<String>,
    pub(crate) input_agent_surface: Option<String>,
    pub(crate) agent_conversation_drag: Option<AgentConversationDrag>,
    /// Exact retained Agent route represented by the primary window when no
    /// ordinary text channel exists to show after detach. This is presentation
    /// identity only: `direct_agent` stays empty and the private editor remains
    /// owned exclusively by the secondary surface.
    pub(crate) detached_agent_placeholder: Option<Id>,
    /// This instance is itself the content of one secondary conversation.
    pub(crate) conversation_surface: bool,
    pub(crate) conversation_return_requested: bool,
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
    pub(crate) room_colors: HashMap<Id, (u8, u8, u8)>,
    /// Stable bare ids of agents authenticated by the room's latest complete
    /// broker Context. Names and document rows never populate this set.
    pub(crate) room_agents: Vec<Id>,
    /// The peer-local route currently presenting a direct Agent conversation.
    /// Authority still comes from `room_agents`; this value is never read from
    /// the shared Chat document.
    pub(crate) direct_agent: Option<Id>,
    /// One owner-backed task reader per exact workspace/Agent conversation.
    /// Parent and detached presentations share these cells; only the root
    /// pumps their one shared action transport.
    pub(crate) agent_tasks:
        HashMap<(Id, Id), Rc<RefCell<ui::agent_task_frontier::AgentTaskFrontiers>>>,
    pub(crate) agent_task_actions: ui::remote_actions::RemoteActions,
    /// One bounded owner-progress reader shared by the attached and detached
    /// presentations. Only the parent pumps it; detached views render the same
    /// authenticated milestones without opening duplicate read streams.
    pub(crate) agent_message_progress:
        Rc<RefCell<ui::agent_message_progress::AgentMessageProgressReader>>,
    /// Peer-local composer intent and unsettled exact submissions, keyed like
    /// the private rich draft. They never enter the Chat CRDT.
    pub(crate) agent_intents: ui::LocalState<HashMap<(Id, Id), AgentComposerIntent>>,
    pub(crate) agent_submissions: ui::LocalState<HashMap<(Id, Id), AgentSubmission>>,
    /// Monotonic peer-local evidence that a composer intent entered its
    /// authenticated asynchronous transport. This is UI acknowledgement,
    /// never a durable owner-success claim.
    pub(crate) agent_submission_acceptances: ui::LocalState<u64>,
    /// Local acknowledgement that an explicit read-only status refresh was
    /// queued. It never represents a reply, model turn or owner mutation.
    pub(crate) agent_message_refresh_acceptances: ui::LocalState<u64>,
    pub(crate) agent_submission_errors: ui::LocalState<HashMap<(Id, Id), String>>,
    /// Peer-local disclosure state for durable Agent action records.
    pub(crate) agent_work_expanded: ui::LocalState<HashSet<Id>>,
    /// Whether this frame's empty hero carries the unavailable/read-only
    /// explanation, allowing the composer to omit its duplicate receipt row.
    pub(crate) agent_notice_in_empty_hero: bool,

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
    // this person, so their lines go on reading "You".
    pub(crate) former_ids: Vec<Id>,

    // The OS permission gate: routes channel participation + file-embed
    // hydration through the toolkit's permission model. Refreshed from the
    // `PermissionContext` the compositor passes into `draw`.
    pub(crate) perms: ChatPermissions,

    // The resource's dispatcher: every doc-mutating affordance (Send, a
    // reaction, the channel/message context menus) applies its action
    // through it, permission-checked, attributed to this window's person,
    // recorded in history and replicated. See [`crate::actions`].
    pub(crate) dispatch: Option<ui::framework::Dispatcher<crate::resource::Chat>>,
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
    pub(crate) dispatched: std::cell::RefCell<Vec<ChatAction>>,

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
    /// Stable semantic identity for `keyboard_target`. Dynamic Agent controls
    /// can enter or leave the ring between focus and a later physical Enter;
    /// the ordinal is only a frame-local cache of this identity.
    pub(crate) keyboard_target_semantic: Option<String>,
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
    canonical_dirty: bool,
    /// Document heads and pending-operation count at the last reload,
    /// so an unchanged document is not reloaded and repainted every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    host_focused: bool,
    pub(crate) host_intent: Option<ui::widget::AppHostIntent>,
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
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
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
        app
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
            voice_members: HashMap::new(),
            draft_main: String::new(),
            draft_thread: String::new(),
            composer_main: crate::composer::DraftComposer::shared(),
            composer_thread: crate::composer::DraftComposer::shared(),
            active_ws: "ws_acme".into(),
            context_ws: "ws_acme".into(),
            inbox_open: false,
            active_channel: "c_crit".into(),
            focus: Focus::Main,
            scroll: 0.0,
            thread_scroll: ui::widget::Scrollable::new_vertical().always_visible(),
            thread_body: Rect::ZERO,
            timeline_at_end: true,
            presented: false,
            read_on_present: false,
            timeline_total_height: 0.0,
            scroll_content: 0.0,
            scroll_viewport: 0.0,
            visible_message_rows: HashSet::new(),
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
            call_overlay_metrics: crate::render_call::CallOverlayMetrics::default(),
            call_overlay_metrics_key: None,
            realtime: crate::continuity::ChatRealtimeContinuity::default(),
            call_chrome_motion: CallChromeMotion::default(),
            huddle_chip_rows: 0,
            pending_floating: None,
            agent_surfaces: HashMap::new(),
            agent_surface_requests: VecDeque::new(),
            focused_agent_surface: None,
            ime_source_agent_surface: None,
            input_agent_surface: None,
            agent_conversation_drag: None,
            detached_agent_placeholder: None,
            conversation_surface: false,
            conversation_return_requested: false,
            floating_surface_bounds: Rect::ZERO,
            search_opener: ui::search::SearchOpener::new(),
            left_flap: Self::navigation_drawer(),
            right_flap: Self::detail_drawer(),
            chrome: Chrome::default(),
            layout_text_scale: 1.0,
            me: LocalIdentity::demo(),
            room_names: HashMap::new(),
            room_colors: HashMap::new(),
            room_agents: Vec::new(),
            direct_agent: None,
            agent_tasks: HashMap::new(),
            agent_task_actions: ui::remote_actions::RemoteActions::current(),
            agent_message_progress: Rc::new(RefCell::new(
                ui::agent_message_progress::AgentMessageProgressReader::default(),
            )),
            agent_intents: ui::LocalState::new(HashMap::new()),
            agent_submissions: ui::LocalState::new(HashMap::new()),
            agent_submission_acceptances: ui::LocalState::new(0),
            agent_message_refresh_acceptances: ui::LocalState::new(0),
            agent_submission_errors: ui::LocalState::new(HashMap::new()),
            agent_work_expanded: ui::LocalState::new(HashSet::new()),
            agent_notice_in_empty_hero: false,
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
            dispatch: None,
            pending_call: None,
            sharing_context: false,
            following_context: false,
            context_follow_requested: false,
            preferences: None,
            collapsed: std::collections::BTreeSet::new(),
            #[cfg(test)]
            dispatched: std::cell::RefCell::default(),
            images: ui::ImageStore::new(&crate::images::fetcher(content.clone())),
            content,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            keyboard_target: None,
            keyboard_target_semantic: None,
            last_mouse: Point::ZERO,
            call_hovered: None,
            tooltips: TooltipHost::new(),
            relayout: false,
            flap_animating: false,
            announce: None,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            media_error: None,
            canonical_dirty: true,
            synced: None,
            host_focused: true,
            host_intent: None,
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

    fn focused_send_enabled(&self) -> bool {
        if !self.can_participate() {
            return false;
        }
        if matches!(self.focus, Focus::Thread) {
            return self.thread_draft_key().is_some()
                && !self.composer_thread.value().trim().is_empty();
        }
        let draft = self.composer_main.value();
        if draft.trim().is_empty() {
            return false;
        }
        if self.direct_agent.is_none() {
            return true;
        }
        self.direct_agent_is_available()
            && match self.direct_agent_intent() {
                AgentComposerIntent::Message => true,
                AgentComposerIntent::Queue => self.direct_agent_tasks().is_some_and(|tasks| {
                    matches!(
                        tasks.borrow().discovery(),
                        ui::agent_task_frontier::AgentTaskDiscovery::Ready
                            | ui::agent_task_frontier::AgentTaskDiscovery::Incomplete { .. }
                    )
                }),
                AgentComposerIntent::Steer => self
                    .direct_agent_tasks()
                    .is_some_and(|tasks| tasks.borrow().can_steer()),
            }
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

    /// Apply one [`ChatAction`] through the resource's dispatcher — the
    /// single document-mutation path of every on-screen affordance, the
    /// same door the palette and agents use. The projection is reconciled
    /// from the document on the next sync.
    pub(crate) fn apply_action(&self, doc: &mut ui::widget::Edits<'_>, action: &ChatAction) {
        #[cfg(test)]
        self.dispatched.borrow_mut().push(action.clone());
        if let Some(dispatch) = &self.dispatch {
            // A refusal changes nothing; the dispatcher reports it.
            let _ = dispatch.apply_now_with(doc, action, actions::history_context(action));
        }
    }

    /// Submit one [`ChatAction`] through the resource's dispatcher, applied
    /// after this pass: from a pass that only reads the document (a frame's
    /// sync, a worker's answer). A refusal is reported by the dispatcher.
    pub(crate) fn submit_action(&self, action: &ChatAction) {
        #[cfg(test)]
        self.dispatched.borrow_mut().push(action.clone());
        if let Some(dispatch) = &self.dispatch {
            dispatch.apply(action);
        }
    }

    pub(crate) fn ensure_composer_draft(&self, doc: &mut ui::widget::Edits<'_>, key: &str) {
        if key.is_empty() || ChatDoc::rich_draft_is_settled(doc, key) {
            return;
        }
        self.apply_action(
            doc,
            &ChatAction::EnsureRichDraft {
                key: key.to_string(),
            },
        );
    }

    /// The last action an on-screen affordance applied, encoded as the
    /// published action that builds it would encode it.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<tilo_actions::EncodedAction> {
        self.dispatched.borrow().last().map(actions::encode)
    }

    /// Test-only: apply what this window submitted to `doc`, as the host
    /// applies it after a pass.
    #[cfg(test)]
    pub(crate) fn land(&self, doc: &mut AutoCommit) {
        if let Some(dispatch) = &self.dispatch {
            ui::framework::testing::resolve(dispatch, doc).expect("submitted edits apply");
        }
    }

    /// Edit through `dispatcher` from now on.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Chat>,
    ) {
        self.composer_main.attach(dispatcher.clone());
        self.composer_thread.attach(dispatcher.clone());
        self.dispatch = Some(dispatcher);
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
    pub(crate) fn reaction_toggle_action(&self, msg: &str, emoji: &str) -> ChatAction {
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.reacted_by(&self.me.id))
        });
        if reacting {
            actions::remove_reaction_action(&self.active_channel, msg, emoji)
        } else {
            actions::add_reaction_action(&self.active_channel, msg, emoji)
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
        }
        self.sharing_context = scenario == "live-context";
        self.stage_view(scenario);
        if scenario.starts_with("agent-lifecycle-before")
            || scenario.starts_with("agent-message-refused")
            || scenario.starts_with("agent-permission-revoked")
            || scenario.starts_with("agent-membership-removed")
            || matches!(
                scenario,
                "agent-direct"
                    | "agent-direct-compact"
                    | "agent-direction-saved"
                    | "agent-direction-saved-closed"
                    | "agent-direction-saved-compact"
                    | "agent-direction-saved-floating"
                    | "agent-stopping"
                    | "agent-stopping-compact"
                    | "agent-long-content"
                    | "agent-long-content-compact"
                    | "agent-long-content-floating"
            )
        {
            let agent_name = if scenario.starts_with("agent-long-content") {
                "Documentation and Accessibility Specialist"
            } else {
                "Scribe"
            };
            self.room_agents = vec!["scribe".into()];
            self.room_names
                .insert("agent:scribe".into(), agent_name.into());
            self.active_channel = ChatDoc::agent_conversation_channel_id("ws_acme", "scribe");
            self.direct_agent = Some("scribe".into());
            self.agent_task_context("ws_acme", "scribe");
            self.composer_main.set_agent_draft("ws_acme", "scribe");
            self.composer_main.set_placeholder(&crate::i18n::t_args(
                "chat-agent-message-placeholder",
                &[("name", agent_name.into())],
            ));
            self.stage_agent_task_controls(scenario);
            self.stage_agent_lifecycle_state(scenario);
        }
        self.reselect();
        self.request_relayout();
        self.stage_floating_call(scenario);
    }

    fn stage_agent_lifecycle_state(&mut self, scenario: &str) {
        if !scenario.starts_with("agent-lifecycle-before")
            && !scenario.starts_with("agent-message-refused")
            && !scenario.starts_with("agent-permission-revoked")
            && !scenario.starts_with("agent-membership-removed")
            && !scenario.starts_with("agent-long-content")
        {
            return;
        }
        let draft = if scenario.starts_with("agent-long-content") {
            "Preserve this private draft while checking the translated release notes, keyboard navigation, focus restoration, and high-contrast evidence before publishing."
        } else {
            "Keep the private release notes and keyboard evidence here."
        };
        self.composer_main.stage_private_text(draft);
        self.composer_main.select(9, 30);
        self.focus = Focus::Main;
        if scenario.starts_with("agent-message-refused") {
            self.agent_submission_errors.update(|errors| {
                errors.insert(
                    ("ws_acme".into(), "scribe".into()),
                    crate::i18n::t("chat-agent-message-send-failed"),
                );
            });
        } else if scenario.starts_with("agent-permission-revoked") {
            self.perms = crate::permissions::ChatPermissions::from_context(
                ui::permissions::PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), model::ME),
                    Vec::new(),
                ),
            );
            self.composer_main.set_read_only(true);
        } else if scenario.starts_with("agent-membership-removed") {
            self.room_agents.clear();
            self.composer_main.set_read_only(true);
        }
        self.set_composer_focus();
    }

    fn stage_agent_task_controls(&mut self, scenario: &str) {
        let stopping = scenario.starts_with("agent-stopping");
        let title = if scenario.starts_with("agent-long-content") {
            "Review the international release notes and verify every keyboard navigation path before publishing"
        } else {
            "Review the release"
        };
        let current_staged = self.stage_agent_task_receipt(
            "read-current-tasks",
            serde_json::json!({
                "generation": 8,
                "complete": true,
                "total": 1,
                "tasks": [{
                    "task_id": "task-release",
                    "title": title,
                    "assigned_agent": "scribe",
                    "status": "in_progress",
                    "run_epoch": 7,
                    "active_agent": "scribe",
                    "after_revision": 4,
                    "applied_revision": 4,
                    "route": "model",
                    "cancellation_requested": stopping
                }],
                "pending_complete": true,
                "pending_total": 2,
                "pending": [{
                    "task_id": "task-accessibility",
                    "title": "Check accessibility",
                    "assigned_agent": "scribe",
                    "status": "queued",
                    "route": "model"
                }, {
                    "task_id": "task-compact",
                    "title": "Inspect compact layout",
                    "assigned_agent": "scribe",
                    "status": "queued",
                    "route": "model"
                }]
            }),
        );
        if current_staged {
            self.stage_agent_task_catalogue(!scenario.starts_with("agent-permission-revoked"));
        }

        if current_staged && scenario.starts_with("agent-direction-saved") {
            self.set_direct_agent_intent(AgentComposerIntent::Steer);
            let direction_started = self.direct_agent_tasks().is_some_and(|tasks| {
                tasks
                    .borrow_mut()
                    .steer("Include keyboard evidence")
                    .is_ok()
            });
            if direction_started {
                self.stage_agent_task_receipt(
                    "direct",
                    serde_json::json!({
                        "result": {
                            "direction_added": {"task_id": "task-release", "revision": 5}
                        }
                    }),
                );
            }
        }
    }

    fn stage_agent_task_receipt(&mut self, action: &str, value: serde_json::Value) -> bool {
        use tilo_actions::ActionOutput;
        use tilo_actions::remote::{InvocationReceipt, ReceiptStatus, RemoteActionMessage};
        use ui::collaboration::{CollabEvent, CollabRequest};

        let Some(request) = self
            .agent_task_actions
            .take_collab_requests()
            .into_iter()
            .find_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(request),
                } if request.target.address.action == action => Some(request),
                _ => None,
            })
        else {
            return false;
        };
        self.agent_task_actions
            .handle_collab_event(&CollabEvent::RemoteAction {
                message: RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                }),
            });
        self.poll_agent_tasks();
        true
    }

    fn stage_agent_task_catalogue(&mut self, offered: bool) {
        use tilo_actions::remote::{CataloguePage, CatalogueResource, RemoteActionMessage};
        use tilo_actions::{
            ActionContract, ActionDescriptor, ExecutionLocus, InputSpec, OperationKind,
        };
        use ui::collaboration::{CollabEvent, CollabRequest};

        let requests = self.agent_task_actions.take_collab_requests();
        for request in requests {
            let CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { correlation, query },
            } = request
            else {
                continue;
            };
            let Some(target) = query.target else {
                continue;
            };
            let row = tilo_actions::remote::CatalogueRow {
                fresh_binding: None,
                provider: query.provider.unwrap_or_default(),
                app_id: "task-center".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: target.resource.clone(),
                    kind: "desktop.task".into(),
                    label: "Task".into(),
                    presentation: None,
                },
                action: Some(ActionDescriptor {
                    address: target,
                    semantic_identity: None,
                    resource_kind: "desktop.task".into(),
                    resource_label: "Task".into(),
                    action_label: "Task action".into(),
                    description: None,
                    locus: ExecutionLocus::ClientLocal,
                    kind: OperationKind::Document,
                    contract: ActionContract::default(),
                    input: InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }),
                has_children: false,
                visibility: None,
            };
            self.agent_task_actions
                .handle_collab_event(&CollabEvent::RemoteAction {
                    message: RemoteActionMessage::CatalogueResponse {
                        correlation,
                        result: Ok(CataloguePage {
                            revision: "shots-task-controls-1".into(),
                            rows: offered.then_some(row).into_iter().collect(),
                            exhausted: true,
                            ..CataloguePage::default()
                        }),
                    },
                });
        }
        self.poll_agent_tasks();
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
            "agent-lifecycle-before"
            | "agent-lifecycle-before-compact"
            | "agent-lifecycle-before-floating"
            | "agent-message-refused"
            | "agent-message-refused-compact"
            | "agent-message-refused-floating"
            | "agent-permission-revoked"
            | "agent-permission-revoked-compact"
            | "agent-permission-revoked-floating"
            | "agent-membership-removed"
            | "agent-membership-removed-compact"
            | "agent-membership-removed-floating"
            | "agent-direct"
            | "agent-direct-compact"
            | "agent-direction-saved"
            | "agent-direction-saved-closed"
            | "agent-direction-saved-compact"
            | "agent-direction-saved-floating"
            | "agent-stopping"
            | "agent-stopping-compact"
            | "agent-long-content"
            | "agent-long-content-compact"
            | "agent-long-content-floating" => {
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
                self.left_flap.snap(
                    !scenario.ends_with("-compact")
                        && !scenario.ends_with("-closed")
                        && !scenario.ends_with("-floating"),
                );
            }
            "people" => {
                self.thread_open = None;
                self.right_flap.snap(true);
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
            "attach-error" => {
                self.stage_view("attach");
                self.attachment_status =
                    Some("Attachment import failed: file is unavailable".into());
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
            "no-access" => {
                // A reader the workspace has not granted its files: every
                // attachment shows what it is and that it cannot open. The
                // restricted grant itself is the shot's permission context
                // (`shot_permission`).
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
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

    /// The permission context a scenario is captured under, when it is not
    /// the local unrestricted user: `no-access` is a person the workspace
    /// has granted nothing, so its attachments cannot be opened.
    #[doc(hidden)]
    pub fn shot_permission(scenario: &str) -> Option<ui::permissions::PermissionContext> {
        (scenario == "no-access").then(|| {
            ui::permissions::PermissionContext::from_capabilities(
                ui::permissions::Actor::person(None, crate::model::ME),
                Vec::new(),
            )
        })
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

    // --- model lookups ----------------------------------------------------
    pub(crate) fn user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    /// Take the room's view of the participants off the presence store.
    ///
    /// Called once per frame from `draw_with_presence`. The presence store is
    /// seeded from `CollabEvent::Context` — the broker's roster snapshot — so a
    /// name here is the room's answer, arrived over the same transport as the
    /// messages. Nothing a peer says about itself reaches this map.
    ///
    /// The room's answer **replaces** what was held rather than merging into
    /// it. Merging is how a peer who has gone keeps a name and a colour on
    /// everyone else's screen for the rest of the session: presence forgets
    /// them on leave, and anything downstream that only ever learned would go
    /// on offering a departed participant in the `@` picker and painting their
    /// colour behind a mention.
    pub fn adopt_room(&mut self, presence: &PresenceCtx) {
        self.adopt_own_participant(presence);
        self.room_agents = presence
            .names
            .keys()
            .filter_map(|id| presence.authenticated_agent_id(id).map(str::to_owned))
            .collect();
        self.room_agents.sort();
        self.room_agents.dedup();
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
    /// that spelling are still this person's, so "You" stays beside them.
    fn adopt_own_participant(&mut self, presence: &PresenceCtx) {
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

    /// What to call `id`: the room's answer first, the document's stored name
    /// as an offline fallback, and the raw id last so a label is never blank.
    ///
    /// The precedence is the point. A client that could name itself would be
    /// telling every other peer what to call it, and two peers disagreeing
    /// about a third's name is a difference nobody can settle. So the room
    /// answers, and a peer only ever supplies its id.
    pub fn display_name(&self, id: &str) -> String {
        if let Some(n) = self.room_names.get(id)
            && !n.trim().is_empty()
        {
            return if self.is_me(id) {
                n.clone()
            } else {
                crate::identity::shared_name_label(id, n)
            };
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return agent.clone();
        }
        if let Some(u) = self.users.iter().find(|u| u.id == id)
            && !u.name.trim().is_empty()
        {
            return if self.is_me(id) {
                u.name.clone()
            } else {
                crate::identity::shared_name_label(id, &u.name)
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

    /// The room-owned display name for an authenticated bare Agent id.
    /// Agent and Person namespaces remain distinct even when their bare ids
    /// happen to have the same spelling.
    pub(crate) fn agent_display_name(&self, agent: &str) -> String {
        self.room_names
            .get(&format!("agent:{agent}"))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| agent.to_owned())
    }

    pub(crate) fn open_agent_conversation(
        &mut self,
        workspace: &str,
        agent: &str,
    ) -> Result<(), String> {
        if workspace != self.context_ws
            || !self
                .workspaces
                .iter()
                .any(|candidate| candidate.id == workspace)
            || !self.room_agents.iter().any(|known| known == agent)
        {
            return Err("agent is not admitted to this workspace".into());
        }
        workspace.clone_into(&mut self.active_ws);
        self.detached_agent_placeholder = None;
        self.active_channel = ChatDoc::agent_conversation_channel_id(workspace, agent);
        self.direct_agent = Some(agent.into());
        self.agent_task_context(workspace, agent);
        self.inbox_open = false;
        self.thread_open = None;
        self.focus = Focus::Main;
        self.reset_timeline_to_latest();
        let channel = self.active_channel.clone();
        self.restore_attached_agent_surface(&channel);
        Ok(())
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
        view.room_colors.clone_from(&parent.room_colors);
        view.room_agents.clone_from(&parent.room_agents);
        view.agent_authors.clone_from(&parent.agent_authors);
        view.former_ids.clone_from(&parent.former_ids);
        view.context_ws.clone_from(&parent.context_ws);
        view.active_ws.clone_from(&parent.active_ws);
        view.active_channel = ChatDoc::agent_conversation_channel_id(&parent.active_ws, agent);
        view.direct_agent = Some(agent.into());
        view.agent_tasks.clone_from(&parent.agent_tasks);
        view.agent_task_actions = parent.agent_task_actions.clone();
        view.agent_message_progress = Rc::clone(&parent.agent_message_progress);
        view.agent_intents = parent.agent_intents.clone();
        view.agent_submissions = parent.agent_submissions.clone();
        view.agent_submission_acceptances = parent.agent_submission_acceptances.clone();
        view.agent_message_refresh_acceptances = parent.agent_message_refresh_acceptances.clone();
        view.agent_submission_errors = parent.agent_submission_errors.clone();
        view.agent_work_expanded = parent.agent_work_expanded.clone();
        view.conversation_surface = true;
        view.thread_open = None;
        view.left_flap.snap(false);
        view.right_flap.snap(false);
        view.external_services = parent.external_services;
        if let Some(dispatcher) = parent.dispatch.clone() {
            view.attach_framework(dispatcher);
        }
        view
    }

    pub(crate) fn agent_task_context(
        &mut self,
        workspace: &str,
        agent: &str,
    ) -> Rc<RefCell<ui::agent_task_frontier::AgentTaskFrontiers>> {
        let key = (workspace.to_owned(), agent.to_owned());
        let actions = self.agent_task_actions.clone();
        let context = self
            .agent_tasks
            .entry(key)
            .or_insert_with(|| {
                Rc::new(RefCell::new(
                    ui::agent_task_frontier::AgentTaskFrontiers::with_actions(actions),
                ))
            })
            .clone();
        {
            let mut tasks = context.borrow_mut();
            tasks.set_observation_active(true);
            tasks.observe(workspace, agent);
        }
        context
    }

    pub(crate) fn direct_agent_intent(&self) -> AgentComposerIntent {
        let Some(agent) = &self.direct_agent else {
            return AgentComposerIntent::Message;
        };
        self.agent_intents.with(|intents| {
            intents
                .get(&(self.active_ws.clone(), agent.clone()))
                .copied()
                .unwrap_or_default()
        })
    }

    pub(crate) fn direct_agent_tasks(
        &self,
    ) -> Option<Rc<RefCell<ui::agent_task_frontier::AgentTaskFrontiers>>> {
        let agent = self.direct_agent.as_ref()?;
        self.agent_tasks
            .get(&(self.active_ws.clone(), agent.clone()))
            .cloned()
    }

    pub(crate) fn direct_agent_intent_allowed(&self, intent: AgentComposerIntent) -> bool {
        if !self.direct_agent_is_available() {
            return false;
        }
        match intent {
            AgentComposerIntent::Message => self.can_participate(),
            AgentComposerIntent::Queue => self
                .direct_agent_tasks()
                .is_some_and(|tasks| tasks.borrow().can_queue()),
            AgentComposerIntent::Steer => self
                .direct_agent_tasks()
                .is_some_and(|tasks| tasks.borrow().can_steer()),
        }
    }

    pub(crate) fn focused_composer_can_edit(&self) -> bool {
        (matches!(self.focus, Focus::Main) && self.main_composer_can_edit())
            || (self.direct_agent.is_none() && self.can_participate())
    }

    pub(crate) fn main_composer_can_edit(&self) -> bool {
        self.direct_agent.as_ref().map_or_else(
            || self.can_participate(),
            |_| self.direct_agent_intent_allowed(self.direct_agent_intent()),
        )
    }

    fn direct_agent_task_title(&self, task_id: &str) -> Option<String> {
        let tasks = self.direct_agent_tasks()?;
        let tasks = tasks.borrow();
        tasks
            .available_tasks()
            .into_iter()
            .map(|task| (task.task_id, task.title))
            .chain(
                tasks
                    .pending_tasks()
                    .into_iter()
                    .map(|task| (task.task_id, task.title)),
            )
            .find_map(|(id, title)| (id == task_id).then_some(title))
    }

    fn direct_agent_task_accessible_status(&self, task_id: &str) -> Option<String> {
        use ui::agent_task_frontier::{AgentStopState, AgentTaskChoice};

        let tasks = self.direct_agent_tasks()?;
        let tasks = tasks.borrow();
        let current = match tasks.choice() {
            AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => task,
            AgentTaskChoice::None | AgentTaskChoice::Choose(_) => return None,
        };
        if current.task_id != task_id {
            return None;
        }
        let stopping = matches!(tasks.stop_state(), AgentStopState::Stopping { .. });
        Some(self.direct_agent_status_lines(&tasks, stopping).primary)
    }

    pub(crate) fn agent_message_awaiting_confirmation(&self, agent: &str) -> bool {
        self.agent_submissions
            .get()
            .get(&(self.active_ws.clone(), agent.to_owned()))
            .is_some_and(|submission| {
                matches!(
                    submission.message.as_ref(),
                    Some(AgentMessageSubmission::Invoke { .. })
                )
            })
    }

    pub(crate) fn direct_agent_is_available(&self) -> bool {
        self.direct_agent
            .as_ref()
            .is_some_and(|agent| self.room_agents.iter().any(|known| known == agent))
    }

    pub(crate) fn set_direct_agent_intent(&self, intent: AgentComposerIntent) {
        let Some(agent) = self.direct_agent.clone() else {
            return;
        };
        self.agent_intents
            .update(|intents| _ = intents.insert((self.active_ws.clone(), agent), intent));
    }

    pub(crate) fn poll_agent_tasks(&mut self) {
        let before = self.visible_agent_task_presentation();
        // The parent owns demand for the shared contexts. A detached view must
        // not suspend its siblings merely because they aren't its own route.
        if !self.conversation_surface {
            let mut visible = std::collections::HashSet::new();
            if let Some(agent) = &self.direct_agent {
                visible.insert((self.active_ws.clone(), agent.clone()));
            }
            for surface in self.agent_surfaces.values() {
                if !surface.is_closed()
                    && surface
                        .presentation(self.desktop_floating_bounds())
                        .is_some()
                    && let Some(agent) = &surface.view.direct_agent
                {
                    visible.insert((surface.view.active_ws.clone(), agent.clone()));
                }
            }
            for (key, context) in &self.agent_tasks {
                context
                    .borrow_mut()
                    .set_observation_active(visible.contains(key));
            }
        }
        for ((workspace, agent), context) in &self.agent_tasks {
            context.borrow_mut().observe(workspace, agent);
        }
        self.settle_agent_submissions();
        if before != self.visible_agent_task_presentation() {
            self.canonical_dirty = true;
            self.request_relayout();
        }
    }

    fn agent_message_original(message: &Message) -> Option<uuid::Uuid> {
        if !message.is_top_level()
            || message.agent_intent != Some(crate::model::AgentIntent::Message)
            || message.author.starts_with("agent:")
        {
            return None;
        }
        let encoded = message.id.strip_prefix("m_")?;
        (encoded.len() == 32)
            .then(|| uuid::Uuid::parse_str(encoded).ok())
            .flatten()
            .filter(|id| !id.is_nil())
    }

    fn presented_agent_message_original(&self, message: &Message) -> Option<uuid::Uuid> {
        if self.active_ws != self.context_ws {
            return None;
        }
        let agent = self.direct_agent.as_deref()?;
        let channel = ChatDoc::agent_conversation_channel_id(&self.active_ws, agent);
        if self.active_channel != channel || message.channel != channel {
            return None;
        }
        Self::agent_message_original(message)
    }

    pub(crate) fn visible_agent_message_originals(&self) -> Vec<(uuid::Uuid, String)> {
        const MAX_VISIBLE_ORIGINALS: usize = 32;
        let relevant = |view: &Self| {
            let Some(agent) = view.direct_agent.as_deref() else {
                return Vec::new();
            };
            let canonical_author = format!("agent:{agent}");
            let replied_parents = view
                .messages
                .iter()
                .filter(|reply| {
                    reply.channel == view.active_channel && reply.author == canonical_author
                })
                .filter_map(|reply| reply.parent.clone())
                .collect::<HashSet<_>>();
            let mut rows = view
                .messages
                .iter()
                .rev()
                .filter(|message| message.channel == view.active_channel)
                .filter_map(|message| {
                    let original = view.presented_agent_message_original(message)?;
                    let replied = replied_parents.contains(&message.id);
                    Some((replied, original, message.id.clone()))
                })
                .collect::<Vec<_>>();
            // A new unanswered request must never be starved by old history.
            // Answered rows remain demanded behind it until the owner supplies
            // the exact committed reply correlation used by the UI/snapshot.
            rows.sort_by_key(|(replied, ..)| *replied);
            rows.into_iter()
                .map(|(_, original, row)| (original, row))
                .collect::<Vec<_>>()
        };
        let mut lanes = vec![relevant(self).into_iter()];
        if !self.conversation_surface {
            for surface in self.agent_surfaces.values() {
                if !surface.is_closed()
                    && surface
                        .presentation(self.desktop_floating_bounds())
                        .is_some()
                {
                    lanes.push(relevant(&surface.view).into_iter());
                }
            }
        }
        let mut originals = std::collections::BTreeMap::new();
        while originals.len() < MAX_VISIBLE_ORIGINALS {
            let mut advanced = false;
            for lane in &mut lanes {
                if originals.len() == MAX_VISIBLE_ORIGINALS {
                    break;
                }
                if let Some((original, row)) = lane.next() {
                    advanced = true;
                    originals.entry(original).or_insert(row);
                }
            }
            if !advanced {
                break;
            }
        }
        originals.into_iter().collect()
    }

    fn agent_message_progress_presentation(&self) -> Vec<AgentMessageProgressPresentation> {
        let reader = self.agent_message_progress.borrow();
        self.visible_agent_message_originals()
            .into_iter()
            .map(|(original, request_row)| AgentMessageProgressPresentation {
                original_invocation: original,
                request_row,
                progress: reader.progress(original).cloned(),
                unavailable: reader.unavailable(original),
                refreshing_unavailable: reader.unavailable(original) && reader.refresh_in_flight(),
            })
            .collect()
    }

    fn agent_message_progress_snapshot(&self) -> Vec<serde_json::Value> {
        self.agent_message_progress_presentation()
            .into_iter()
            .map(|entry| {
                use ui::agent_message_progress::AgentMessagePublicPhase;
                let (generation, phase, reply_row) = entry.progress.map_or_else(
                    || (serde_json::Value::Null, serde_json::Value::Null, None),
                    |progress| {
                        let (phase, reply) = match progress.phase {
                            AgentMessagePublicPhase::WaitingForReply => ("waiting_for_reply", None),
                            AgentMessagePublicPhase::Publishing => ("publishing", None),
                            AgentMessagePublicPhase::NeedsAttention(_) => ("needs_attention", None),
                            AgentMessagePublicPhase::Committed { reply_row } => {
                                ("committed", Some(reply_row))
                            }
                        };
                        (
                            serde_json::json!(progress.generation),
                            serde_json::json!(phase),
                            reply,
                        )
                    },
                );
                serde_json::json!({
                    "original_invocation": entry.original_invocation,
                    "request_row": entry.request_row,
                    "generation": generation,
                    "phase": phase,
                    "unavailable": entry.unavailable,
                    "reply_row": reply_row,
                })
            })
            .collect()
    }

    pub(crate) fn poll_agent_message_progress(&mut self) {
        if self.conversation_surface {
            return;
        }
        let before = self.agent_message_progress_presentation();
        let originals = self.visible_agent_message_originals();
        let Some(room) = uuid::Uuid::parse_str(&self.context_ws)
            .ok()
            .filter(|room| !room.is_nil())
        else {
            self.agent_message_progress.borrow_mut().cancel();
            if before != self.agent_message_progress_presentation() {
                self.canonical_dirty = true;
                self.request_relayout();
            }
            return;
        };
        let mut reader = self.agent_message_progress.borrow_mut();
        if reader.set_visible(room, &originals).is_err() {
            reader.cancel();
            drop(reader);
            if before != self.agent_message_progress_presentation() {
                self.canonical_dirty = true;
                self.request_relayout();
            }
            return;
        }
        reader.refresh();
        drop(reader);
        if before != self.agent_message_progress_presentation() {
            self.canonical_dirty = true;
            self.request_relayout();
        }
    }

    pub(crate) fn agent_message_progress_label(&self, message: &Message) -> Option<String> {
        self.agent_message_progress_label_for(message, true)
    }

    pub(crate) fn agent_message_progress_layout_label(&self, message: &Message) -> Option<String> {
        self.agent_message_progress_label_for(message, false)
    }

    pub(crate) fn has_unavailable_agent_message_progress(&self) -> bool {
        let reader = self.agent_message_progress.borrow();
        self.visible_agent_message_originals()
            .iter()
            .any(|(original, _)| reader.unavailable(*original))
    }

    pub(crate) fn agent_message_refresh_in_flight(&self) -> bool {
        self.agent_message_progress.borrow().refresh_in_flight()
    }

    fn agent_message_progress_label_for(
        &self,
        message: &Message,
        hide_visible_committed: bool,
    ) -> Option<String> {
        let original = self.presented_agent_message_original(message)?;
        let reader = self.agent_message_progress.borrow();
        let Some(progress) = reader.progress(original) else {
            return reader
                .unavailable(original)
                .then(|| crate::i18n::t("chat-agent-progress-unavailable-empty"));
        };
        if let ui::agent_message_progress::AgentMessagePublicPhase::Committed { reply_row } =
            &progress.phase
        {
            let agent = self.direct_agent.as_deref()?;
            let reply_is_visible = self.messages.iter().any(|reply| {
                reply.channel == message.channel
                    && reply.id == *reply_row
                    && reply.parent.as_deref() == Some(message.id.as_str())
                    && reply.author == format!("agent:{agent}")
                    && self.visible_message_rows.contains(&reply.id)
            });
            if hide_visible_committed && reply_is_visible {
                return None;
            }
        }
        let label = match progress.phase {
            ui::agent_message_progress::AgentMessagePublicPhase::WaitingForReply => {
                crate::i18n::t("chat-agent-progress-waiting")
            }
            ui::agent_message_progress::AgentMessagePublicPhase::Publishing => {
                crate::i18n::t("chat-agent-progress-publishing")
            }
            ui::agent_message_progress::AgentMessagePublicPhase::NeedsAttention(_) => {
                crate::i18n::t("chat-agent-progress-needs-attention")
            }
            ui::agent_message_progress::AgentMessagePublicPhase::Committed { .. } => {
                crate::i18n::t("chat-agent-progress-saved")
            }
        };
        Some(if reader.unavailable(original) {
            crate::i18n::t_args(
                "chat-agent-progress-unavailable",
                &[("status", label.into())],
            )
        } else {
            label
        })
    }

    fn agent_task_presentation(view: &Self) -> Option<AgentTaskPresentationSnapshot> {
        let tasks = view.direct_agent_tasks()?;
        let tasks = tasks.borrow();
        Some(AgentTaskPresentationSnapshot {
            choice: tasks.choice(),
            pending: tasks.pending_tasks(),
            pending_complete: tasks.pending_tasks_complete(),
            controls: AgentTaskPresentationControls {
                queue: tasks.can_queue(),
                steer: tasks.can_steer(),
                stop: tasks.can_stop(),
            },
            discovery: tasks.discovery().clone(),
            direction: tasks.direction_state().clone(),
            queue: tasks.queue_state().clone(),
            stop: tasks.stop_state().clone(),
        })
    }

    fn agent_task_snapshot(view: &Self) -> serde_json::Value {
        use ui::agent_task_frontier::AgentTaskChoice;

        let Some(tasks) = view.direct_agent_tasks() else {
            return serde_json::Value::Null;
        };
        let tasks = tasks.borrow();
        let task = |task: ui::agent_task_frontier::AgentTaskFrontier| {
            let execution = task.execution.map(|execution| {
                serde_json::json!({
                    "run_epoch": execution.run_epoch,
                    "sequence": execution.sequence,
                    "phase": execution.phase,
                })
            });
            serde_json::json!({
                "task_id": task.task_id,
                "run_epoch": task.run_epoch,
                "after_revision": task.after_revision,
                "applied_revision": task.applied_revision,
                "cancellation_requested": task.cancellation_requested,
                "execution": execution,
            })
        };
        let choice = match tasks.choice() {
            AgentTaskChoice::None => serde_json::json!({"kind": "none"}),
            AgentTaskChoice::One(current) => {
                serde_json::json!({"kind": "one", "task": task(current)})
            }
            AgentTaskChoice::Selected(current) => {
                serde_json::json!({"kind": "selected", "task": task(current)})
            }
            AgentTaskChoice::Choose(choices) => serde_json::json!({
                "kind": "choose",
                "tasks": choices.into_iter().map(task).collect::<Vec<_>>(),
            }),
        };
        serde_json::json!({
            "choice": choice,
            "pending_count": tasks.pending_tasks().len(),
            "pending_complete": tasks.pending_tasks_complete(),
            "can_queue": tasks.can_queue(),
            "can_steer": tasks.can_steer(),
            "can_stop": tasks.can_stop(),
        })
    }

    fn visible_agent_task_presentation(
        &self,
    ) -> Vec<(String, Option<AgentTaskPresentationSnapshot>)> {
        let mut views = Vec::with_capacity(self.agent_surfaces.len() + 1);
        views.push(("primary".into(), Self::agent_task_presentation(self)));
        views.extend(self.agent_surfaces.values().map(|surface| {
            (
                surface.id.clone(),
                Self::agent_task_presentation(&surface.view),
            )
        }));
        views.sort_by(|left, right| left.0.cmp(&right.0));
        views
    }

    pub(crate) fn clear_keyboard_target(&mut self) {
        self.keyboard_target = None;
        self.keyboard_target_semantic = None;
    }

    pub(crate) fn resolved_keyboard_target(&self) -> Option<usize> {
        let targets = self.semantic_hits();
        match self.keyboard_target_semantic.as_deref() {
            Some(semantic) => targets
                .iter()
                .position(|(_, hit, ..)| hit.semantic_id() == semantic),
            None => self.keyboard_target.filter(|index| *index < targets.len()),
        }
    }

    pub(crate) fn reconcile_keyboard_target(&mut self) -> Option<usize> {
        let resolved = self.resolved_keyboard_target();
        self.keyboard_target = resolved;
        if resolved.is_none() {
            self.keyboard_target_semantic = None;
        }
        resolved
    }

    pub(crate) fn keyboard_owns_input(&self, input: &Hit) -> bool {
        if self.keyboard_target_semantic.is_none() && self.keyboard_target.is_none() {
            return true;
        }
        self.resolved_keyboard_target()
            .and_then(|index| {
                self.semantic_hits()
                    .get(index)
                    .map(|(_, hit, ..)| hit.semantic_id())
            })
            .is_some_and(|semantic| semantic == input.semantic_id())
    }

    fn keyboard_control_snapshot(&self) -> &'static str {
        let Some((_, hit, ..)) = self
            .resolved_keyboard_target()
            .and_then(|index| self.semantic_hits().get(index).cloned())
        else {
            return "none";
        };
        match hit {
            Hit::Send => "send",
            Hit::AgentWindow | Hit::AgentDragHandle if self.agent_window_is_return() => "return",
            Hit::AgentWindow | Hit::AgentDragHandle => "float",
            Hit::FocusMain | Hit::FocusThread => "editor",
            _ => "other",
        }
    }

    fn agent_float_frontier_snapshot(&self) -> serde_json::Value {
        let current_agent = self.direct_agent.as_deref();
        let retained = current_agent.and_then(|agent| {
            let channel = ChatDoc::agent_conversation_channel_id(&self.context_ws, agent);
            self.agent_surfaces.get(&channel)
        });
        serde_json::json!({
            "context_matches_active_workspace": self.context_ws == self.active_ws,
            "context_workspace_present": self.workspaces.iter()
                .any(|workspace| workspace.id == self.context_ws),
            "current_agent_present": current_agent.is_some(),
            "current_agent_admitted": current_agent
                .is_some_and(|agent| self.room_agents.iter().any(|known| known == agent)),
            "retained_surface_present": retained.is_some(),
            "retained_attachment_app": retained.is_some_and(|surface| matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            )),
            "pending_present_request_count": self.agent_surface_requests.len(),
        })
    }

    fn agent_message_submission_phase(message: &AgentMessageSubmission) -> &'static str {
        match message {
            AgentMessageSubmission::Providers(_) => "providers",
            AgentMessageSubmission::LaunchRows(_) => "launch_rows",
            AgentMessageSubmission::LaunchConfirm { .. } => "launch_confirm",
            AgentMessageSubmission::LaunchInvoke { .. } => "launch_invoke",
            AgentMessageSubmission::AwaitPublisher { .. } => "await_publisher",
            AgentMessageSubmission::Rows(_) => "rows",
            AgentMessageSubmission::Confirm { .. } => "confirm",
            AgentMessageSubmission::Invoke { .. } => "invoke",
        }
    }

    pub(crate) fn poll_agent_message_submission(
        &self,
        workspace: &str,
        agent: &str,
        submission: &AgentSubmission,
        message: &AgentMessageSubmission,
    ) -> AgentMessagePoll {
        ui::agent_message_action::poll(
            &self.agent_task_actions,
            workspace,
            agent,
            &submission.text,
            message,
            submission.give_up_at,
            submission.pages_remaining,
        )
    }

    fn agent_message_next_wake(submission: &AgentSubmission) -> Option<ui::time::Instant> {
        submission.next_poll_at
    }

    fn retain_agent_message_wake(
        deadline: Option<ui::time::Instant>,
        now: ui::time::Instant,
        give_up_at: ui::time::Instant,
    ) -> Option<ui::time::Instant> {
        deadline.map(|deadline| {
            if deadline <= now && now < give_up_at {
                now.checked_add(AGENT_MESSAGE_POLL_CADENCE)
                    .unwrap_or(give_up_at)
                    .min(give_up_at)
            } else {
                deadline
            }
        })
    }

    fn agent_message_wake_update(
        &self,
        submission: &AgentSubmission,
        message: &AgentMessageSubmission,
    ) -> AgentMessageWakeDecision {
        let typed = ui::agent_message_action::next_wake_deadline(
            &self.agent_task_actions,
            message,
            submission.give_up_at,
        );
        let now = ui::time::now();
        let typed = Self::retain_agent_message_wake(typed, now, submission.give_up_at);
        let retained = submission.next_poll_at;
        let replace = retained.is_some_and(|deadline| deadline <= now)
            || match (retained, typed) {
                (Some(current), Some(candidate)) => candidate < current,
                (None, Some(_)) => true,
                _ => false,
            };
        if replace {
            AgentMessageWakeDecision::Replace(typed)
        } else {
            AgentMessageWakeDecision::Unchanged
        }
    }

    fn agent_task_submission_outcome(
        &self,
        key: &AgentSubmissionKey,
        submission: &AgentSubmission,
    ) -> AgentTaskSubmissionOutcome {
        use ui::agent_task_frontier::{AgentDirectionState, AgentQueueState};

        let Some(tasks) = self.agent_tasks.get(key) else {
            return AgentTaskSubmissionOutcome::Pending;
        };
        let tasks = tasks.borrow();
        let succeeded = match submission.intent {
            AgentComposerIntent::Message => false,
            AgentComposerIntent::Queue => {
                matches!(tasks.queue_state(), AgentQueueState::Queued { .. })
            }
            AgentComposerIntent::Steer => matches!(
                tasks.direction_state(),
                AgentDirectionState::Saved { .. } | AgentDirectionState::Applied { .. }
            ),
        };
        let failed = match submission.intent {
            AgentComposerIntent::Message => false,
            AgentComposerIntent::Queue => matches!(tasks.queue_state(), AgentQueueState::Failed),
            AgentComposerIntent::Steer => matches!(
                tasks.direction_state(),
                AgentDirectionState::NeedsRefresh(_)
            ),
        };
        if succeeded {
            AgentTaskSubmissionOutcome::Succeeded
        } else if failed {
            AgentTaskSubmissionOutcome::Failed
        } else {
            AgentTaskSubmissionOutcome::Pending
        }
    }

    fn apply_agent_submission_updates(
        &self,
        settled: &[AgentSubmissionKey],
        wake_updates: Vec<AgentMessageWakeUpdate>,
        message_updates: Vec<AgentMessageAdvanceUpdate>,
    ) {
        if !message_updates.is_empty() || !wake_updates.is_empty() {
            self.agent_submissions.update(|pending| {
                for (key, next_poll_at) in wake_updates {
                    if let Some(submission) = pending.get_mut(&key) {
                        submission.next_poll_at = next_poll_at;
                    }
                }
                for (key, message, pages_remaining, next_poll_at) in message_updates {
                    if let Some(submission) = pending.get_mut(&key) {
                        submission.message = Some(*message);
                        submission.pages_remaining = pages_remaining;
                        submission.next_poll_at = next_poll_at;
                    }
                }
            });
        }
        if !settled.is_empty() {
            self.agent_submissions
                .update(|pending| pending.retain(|key, _| !settled.contains(key)));
        }
    }

    fn settle_agent_submissions(&mut self) {
        let pending = self.agent_submissions.get();
        let mut settled = Vec::new();
        let mut message_updates = Vec::new();
        let mut wake_updates = Vec::new();
        for (key, submission) in &pending {
            if submission.intent == AgentComposerIntent::Message {
                let Some(message) = submission.message.as_ref() else {
                    settled.push(key.clone());
                    continue;
                };
                match self.poll_agent_message_submission(&key.0, &key.1, submission, message) {
                    AgentMessagePoll::Waiting => {
                        if let AgentMessageWakeDecision::Replace(next_poll_at) =
                            self.agent_message_wake_update(submission, message)
                        {
                            wake_updates.push((key.clone(), next_poll_at));
                        }
                    }
                    AgentMessagePoll::Advance {
                        submission: advanced,
                        pages_remaining,
                    } => {
                        let typed = ui::agent_message_action::next_wake_deadline(
                            &self.agent_task_actions,
                            &advanced,
                            submission.give_up_at,
                        );
                        let next_poll_at = Self::retain_agent_message_wake(
                            typed,
                            ui::time::now(),
                            submission.give_up_at,
                        );
                        message_updates.push((
                            key.clone(),
                            Box::new(advanced),
                            pages_remaining,
                            next_poll_at,
                        ));
                    }
                    AgentMessagePoll::Succeeded => {
                        self.composer_main.clear_private_for_revision(
                            &key.0,
                            &key.1,
                            submission.draft_revision,
                        );
                        self.agent_submission_errors
                            .update(|errors| _ = errors.remove(key));
                        settled.push(key.clone());
                    }
                    AgentMessagePoll::Failed => {
                        self.agent_submission_errors.update(|errors| {
                            errors.insert(
                                key.clone(),
                                crate::i18n::t("chat-agent-message-send-failed"),
                            );
                        });
                        settled.push(key.clone());
                    }
                }
                continue;
            }
            match self.agent_task_submission_outcome(key, submission) {
                AgentTaskSubmissionOutcome::Succeeded => {
                    let (workspace, agent) = key;
                    self.composer_main.clear_private_for_revision(
                        workspace,
                        agent,
                        submission.draft_revision,
                    );
                    settled.push(key.clone());
                }
                AgentTaskSubmissionOutcome::Failed => settled.push(key.clone()),
                AgentTaskSubmissionOutcome::Pending => {}
            }
        }
        self.apply_agent_submission_updates(&settled, wake_updates, message_updates);
    }

    pub(crate) fn float_agent_conversation(
        &mut self,
        workspace: &str,
        agent: &str,
    ) -> Result<(), String> {
        self.open_agent_conversation(workspace, agent)?;
        if self.rect.area() <= 0.0 {
            return Err("Chat must be visible before its conversation can move".into());
        }
        let desktop = self.desktop_floating_bounds();
        let channel = self.active_channel.clone();
        if let Some(mut surface) = self.agent_surfaces.remove(&channel) {
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            ) {
                surface.move_editor_to_view(self);
                surface.placement.detach_to_desktop(surface.source);
                self.agent_surface_requests
                    .push_back(surface.request(desktop));
            }
            self.agent_surfaces.insert(channel.clone(), surface);
            self.leave_floated_agent_route(workspace, &channel);
            return Ok(());
        }
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(self, agent);
        surface.placement.detach_to_desktop(surface.source);
        self.agent_surface_requests
            .push_back(surface.request(desktop));
        self.agent_surfaces.insert(channel.clone(), surface);
        self.leave_floated_agent_route(workspace, &channel);
        Ok(())
    }

    pub(crate) fn begin_agent_conversation_drag(&mut self, origin: Point) {
        let Some(agent) = self.direct_agent.clone() else {
            return;
        };
        self.agent_conversation_drag = Some(AgentConversationDrag {
            workspace: self.active_ws.clone(),
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
                self.agent_surface_requests
                    .push_back(surface.request(desktop));
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
        self.agent_surface_requests
            .push_back(surface.request(desktop));
        self.agent_surfaces.insert(drag.channel.clone(), surface);
        self.leave_floated_agent_route(&drag.workspace, &drag.channel);
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

    fn leave_floated_agent_route(&mut self, workspace: &str, detached_channel: &str) {
        let fallback = self
            .channels
            .iter()
            .filter(|channel| {
                channel.workspace == workspace
                    && !channel.archived
                    && channel.kind == model::ChannelKind::Text
                    && channel.conversation_with.is_none()
            })
            .min_by_key(|channel| channel.order)
            .map(|channel| channel.id.clone());
        if let Some(channel) = fallback {
            self.detached_agent_placeholder = None;
            self.active_channel = channel;
            self.direct_agent = None;
            self.thread_open = None;
            self.focus = Focus::Main;
            self.reset_timeline_to_latest();
        } else {
            workspace.clone_into(&mut self.active_ws);
            detached_channel.clone_into(&mut self.active_channel);
            self.direct_agent = None;
            self.detached_agent_placeholder = Some(detached_channel.to_owned());
            self.inbox_open = false;
            self.thread_open = None;
            self.focus = Focus::Main;
            self.reset_timeline_to_latest();
        }
    }

    /// Display-only identity for the exact detached conversation represented
    /// by the primary placeholder. `true` means native pixels are adopted;
    /// false covers Prepare and Return handoffs without claiming presentation.
    pub(crate) fn detached_agent_placeholder_status(&self) -> Option<(String, bool)> {
        let channel = self.detached_agent_placeholder.as_deref()?;
        if self.direct_agent.is_some() || self.active_channel != channel {
            return None;
        }
        let surface = self.agent_surfaces.get(channel)?;
        let agent = surface.view.direct_agent.as_deref()?;
        let exact_channel = ChatDoc::agent_conversation_channel_id(&surface.view.active_ws, agent);
        if exact_channel != channel || self.active_ws != surface.view.active_ws {
            return None;
        }
        Some((
            surface.view.agent_display_name(agent),
            surface.placement.is_desktop_adopted() && !surface.placement.is_return_pending(),
        ))
    }

    pub(crate) fn agent_window_is_return(&self) -> bool {
        self.conversation_surface
            || self.detached_agent_placeholder_status().is_some()
            || self
                .agent_surfaces
                .get(&self.active_channel)
                .is_some_and(|surface| {
                    surface.placement.is_desktop_adopted() || surface.placement.is_return_pending()
                })
    }

    /// The presentation transition currently offered for this exact live
    /// Agent conversation. This is per-window state, not document state: a
    /// caller still has to invoke the row-bound View and pass the fresh
    /// checks in the corresponding handler.
    pub(crate) fn agent_presentation_is_detached(
        &self,
        workspace: &str,
        agent: &str,
    ) -> Option<bool> {
        if workspace != self.context_ws
            || !self
                .workspaces
                .iter()
                .any(|candidate| candidate.id == workspace)
            || !self.room_agents.iter().any(|known| known == agent)
        {
            return None;
        }
        let channel = ChatDoc::agent_conversation_channel_id(workspace, agent);
        if self.agent_surfaces.get(&channel).is_some_and(|surface| {
            surface.view.active_ws == workspace
                && surface.view.direct_agent.as_deref() == Some(agent)
                && surface.placement.is_desktop_adopted()
                && !surface.placement.is_return_pending()
        }) {
            return Some(true);
        }
        (self.active_ws == workspace
            && self.active_channel == channel
            && self.direct_agent.as_deref() == Some(agent)
            && self.rect.area() > 0.0)
            .then_some(false)
    }

    /// Ask the shared placement state machine to return the exact currently
    /// detached Agent conversation after revalidating the semantic caller's
    /// live room context. Local pointer/AT recovery after a membership loss
    /// retains the private editor through [`Self::settle_agent_surface_commands`]
    /// but uses the same lower-level handoff.
    pub(crate) fn request_agent_conversation_return(
        &mut self,
        workspace: &str,
        agent: &str,
    ) -> Result<(), String> {
        if self.agent_presentation_is_detached(workspace, agent) != Some(true) {
            return Err("agent conversation is not currently detached".into());
        }
        let channel = ChatDoc::agent_conversation_channel_id(workspace, agent);
        self.request_retained_agent_surface_return(&channel)
    }

    pub(crate) fn request_retained_agent_surface_return(
        &mut self,
        channel: &str,
    ) -> Result<(), String> {
        let surface = self
            .agent_surfaces
            .get_mut(channel)
            .ok_or("agent conversation is not currently detached")?;
        surface.placement.request_app_return();
        if !surface.placement.is_return_pending() {
            return Err("agent conversation return was not accepted".into());
        }
        self.request_relayout();
        Ok(())
    }

    fn settle_returned_agent_surface(&mut self, id: &str, closed: bool) {
        let Some(channel) = self
            .agent_surfaces
            .iter()
            .find_map(|(channel, surface)| (surface.id == id).then(|| channel.clone()))
        else {
            return;
        };
        let placeholder_is_active =
            self.detached_agent_placeholder.as_deref() == Some(channel.as_str());
        if !closed || placeholder_is_active {
            self.restore_retained_agent_route(&channel);
        }
        if closed && let Some(surface) = self.agent_surfaces.get_mut(&channel) {
            surface.close();
        }
        if self.focused_agent_surface.as_deref() == Some(id) {
            self.focused_agent_surface = None;
        }
        if self.ime_source_agent_surface.as_deref() == Some(id) {
            self.ime_source_agent_surface = None;
        }
        if self.input_agent_surface.as_deref() == Some(id) {
            self.input_agent_surface = None;
        }
        self.restore_attached_agent_surface(&channel);
    }

    /// Restore the route a retained secondary surface came from without
    /// re-authorising it from display strings. This is only reached from the
    /// host lifecycle for an already-created local surface. If membership was
    /// withdrawn while it floated, the private draft remains recoverable but
    /// [`Self::send_focused`] still refuses because `room_agents` no longer
    /// contains the destination.
    fn restore_retained_agent_route(&mut self, channel: &str) {
        let Some(surface) = self.agent_surfaces.get(channel) else {
            return;
        };
        let Some(agent) = surface.view.direct_agent.clone() else {
            return;
        };
        self.active_ws.clone_from(&surface.view.active_ws);
        channel.clone_into(&mut self.active_channel);
        self.direct_agent = Some(agent);
        self.detached_agent_placeholder = None;
        self.inbox_open = false;
        self.thread_open = None;
        self.focus = Focus::Main;
        self.reset_timeline_to_latest();
    }

    fn restore_attached_agent_surface(&mut self, channel: &str) {
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
        }
        if !surface.is_closed() || !route_is_active {
            self.agent_surfaces.insert(channel.to_owned(), surface);
        }
    }

    pub(crate) fn settle_agent_surface_commands(&mut self) {
        let requested = self
            .agent_surfaces
            .iter_mut()
            .find_map(|(channel, surface)| {
                std::mem::take(&mut surface.view.conversation_return_requested).then(|| {
                    (
                        channel.clone(),
                        surface.view.active_ws.clone(),
                        surface.view.direct_agent.clone(),
                    )
                })
            });
        let Some((channel, workspace, Some(agent))) = requested else {
            return;
        };
        let retained_matches = self.agent_surfaces.get(&channel).is_some_and(|surface| {
            workspace == surface.view.active_ws
                && surface.view.direct_agent.as_deref() == Some(agent.as_str())
        });
        if retained_matches {
            let _ = self.request_retained_agent_surface_return(&channel);
        }
    }

    /// What to call `id` on screen, with "You" for this session's own
    /// participant and an honest placeholder for someone the room has not
    /// named yet — nobody is ever shown their raw routing id.
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
        // id it resolved at startup, and those are still its own.
        if self.is_me(id) {
            return "You".to_string();
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

    /// Everyone the `@` picker can offer: the document's own users, plus
    /// anyone the room is talking about who has not yet written themselves
    /// into it.
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
        let child_owns_input = self.agent_surface_owns_input();
        let editing = self.editing.is_some() || self.rename.is_some() || !self.host_focused;
        let main = matches!(self.focus, Focus::Main)
            && !editing
            && self.primary_keyboard_owns_input(&Hit::FocusMain);
        let thread = matches!(self.focus, Focus::Thread)
            && self.thread_open.is_some()
            && !editing
            && self.primary_keyboard_owns_input(&Hit::FocusThread);
        self.composer_main.set_focus(main);
        self.composer_thread.set_focus(thread);
        self.edit_input
            .set_focus(self.host_focused && !child_owns_input && self.editing.is_some());
        ui::Widget::set_focus(
            &mut self.rename_input,
            self.host_focused && !child_owns_input && self.rename.is_some(),
        );
    }

    pub(crate) fn primary_keyboard_owns_input(&self, hit: &Hit) -> bool {
        !self.agent_surface_owns_input() && self.keyboard_owns_input(hit)
    }

    fn agent_surface_owns_input(&self) -> bool {
        self.focused_agent_surface
            .as_deref()
            .is_some_and(|id| self.agent_surface_accepts_input(id))
            || self
                .ime_source_agent_surface
                .as_deref()
                .is_some_and(|id| self.agent_surface_accepts_ime(id))
    }

    fn agent_surface_accepts_input(&self, id: &str) -> bool {
        self.agent_surfaces.values().any(|surface| {
            surface.id == id
                && surface.placement.is_desktop_adopted()
                && !surface.placement.is_return_pending()
                && !surface.is_closed()
        })
    }

    fn agent_surface_accepts_ime(&self, id: &str) -> bool {
        self.agent_surfaces.values().any(|surface| {
            surface.id == id
                && matches!(
                    surface.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                )
                && surface.placement.is_desktop_covered()
                && !surface.is_closed()
        })
    }

    /// Presence scope of a shared composer draft (`drafts[draft_key]`), so
    /// collaborators' carets/selections in the same draft are drawn where
    /// they type. Offsets are UTF-8 byte offsets into the draft text.
    pub(crate) fn draft_presence_key(draft_key: &str) -> ui::presence::PresenceKey {
        ui::presence::PresenceKey::Widget {
            path: vec![
                ui::presence::PathSegment::Field("chat_draft".into()),
                ui::presence::PathSegment::Field(draft_key.to_owned()),
            ],
        }
    }

    /// The focused composer's draft scope and caret/selection, when the
    /// person is typing in a composer (not an inline edit or rename).
    pub(crate) fn composer_presence(
        &self,
    ) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        if !self.host_focused
            || self.editing.is_some()
            || self.rename.is_some()
            || !self.can_participate()
        {
            return None;
        }
        let (composer, key) = match self.focus {
            Focus::Thread => (&self.composer_thread, self.thread_draft_key()?),
            Focus::Main => (&self.composer_main, self.active_channel.clone()),
        };
        if key.is_empty() {
            return None;
        }
        let payload = match composer.selection() {
            (anchor, caret) if anchor != caret => ui::PresencePayload::Selection {
                start: anchor.min(caret),
                end: anchor.max(caret),
            },
            (_, caret) => ui::PresencePayload::Caret { pos: caret },
        };
        Some((Self::draft_presence_key(&key), payload))
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
    pub(crate) fn reload(&mut self, doc: &AutoCommit) {
        self.workspaces = ChatDoc::list_workspaces(doc);
        self.channels = ChatDoc::list_channels(doc);
        self.categories = ChatDoc::list_categories(doc);
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
        // Keep the active channel valid (e.g. after a workspace switch). A
        // direct Agent route owns its deterministic channel independently;
        // the first Message creates it atomically.
        if self.direct_agent.is_none()
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
                ChatDoc::unread_count(doc, &c.id, &self.me.id)
            };
        }
        // Read every channel's messages once per reload: the open channel,
        // search and the workspace people directory all project from them
        // (reading them three times dominated typing with a long history).
        let per_channel: Vec<(Id, Vec<Message>)> = self
            .channels
            .iter()
            .map(|channel| (channel.id.clone(), ChatDoc::list_messages(doc, &channel.id)))
            .collect();
        if let Some(agent) = &self.direct_agent {
            self.active_channel = ChatDoc::agent_conversation_channel_id(&self.active_ws, agent);
        }
        self.messages = per_channel
            .iter()
            .find(|(id, _)| *id == self.active_channel)
            .map_or_else(
                || ChatDoc::list_messages(doc, &self.active_channel),
                |(_, messages)| messages.clone(),
            );
        self.search_messages = per_channel
            .iter()
            .flat_map(|(_, messages)| messages.iter().cloned())
            .collect();
        self.search_users = ChatDoc::list_users(doc);
        // Drop the body views of messages no longer loaded.
        let shown: HashSet<&str> = self.messages.iter().map(|m| m.id.as_str()).collect();
        self.body_views
            .borrow_mut()
            .retain(|(_, id), _| shown.contains(id.as_str()));
        self.agent_authors = ChatDoc::agent_authors(doc);
        // Who is in a call is projected only from the authenticated local
        // voice service's response.
        self.refresh_native_streams();
        self.users = self.workspace_people_from(doc, &per_channel);
        // Point each composer at its surface's draft; the editors read it on
        // their next sync (`sync_composers`), keeping the caret while typing.
        if let Some(agent) = &self.direct_agent {
            self.composer_main.set_agent_draft(&self.active_ws, agent);
            self.draft_main = self.composer_main.private_plain().unwrap_or_default();
            self.composer_main.set_placeholder(&crate::i18n::t_args(
                "chat-agent-message-placeholder",
                &[("name", self.agent_display_name(agent).into())],
            ));
        } else {
            self.composer_main.set_key(&self.active_channel);
            self.draft_main = ChatDoc::draft_text(doc, &self.active_channel);
            let ch_name = self
                .channel(&self.active_channel)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            self.composer_main
                .set_placeholder(&format!("Message #{ch_name}"));
        }
        self.composer_main
            .set_local_author(Some(self.me.id.clone()));
        let thread_key = self.thread_draft_key().unwrap_or_default();
        self.composer_thread.set_key(&thread_key);
        self.draft_thread = if thread_key.is_empty() {
            String::new()
        } else {
            ChatDoc::draft_text(doc, &thread_key)
        };
        self.composer_thread.set_placeholder("Reply in thread");
        self.composer_thread
            .set_local_author(Some(self.me.id.clone()));
        let read_only = !self.can_participate();
        self.composer_main.set_read_only(read_only);
        self.composer_thread.set_read_only(read_only);
        if self.detached_agent_placeholder_status().is_some() {
            self.composer_main.set_focus(false);
        } else {
            self.set_composer_focus();
        }
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
        doc: &AutoCommit,
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

        let mut people: Vec<_> = ChatDoc::list_users(doc)
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
        let status = model::Presence::Online;
        if ChatDoc::list_workspaces(doc).is_empty()
            || ChatDoc::user_is_current(doc, &me.id, &me.name, &me.initials, me.color_idx, status)
        {
            return;
        }
        self.submit_action(&actions::ChatAction::EnsureUser {
            name: me.name,
            initials: me.initials,
            color_idx: me.color_idx,
            status: status.as_str().to_string(),
        });
    }

    /// Advance the local read watermark for the channel currently in view so
    /// its unread badge stays cleared and doesn't reappear once the user
    /// navigates away. Idempotent (a no-op when nothing new arrived), so it is
    /// safe to call every frame.
    pub(crate) fn mark_active_read(&mut self, doc: &AutoCommit) {
        if self.active_channel.is_empty() || !self.timeline_at_end {
            return;
        }
        let count = ChatDoc::top_level_count(doc, &self.active_channel) as i64;
        if count <= ChatDoc::read_watermark(doc, &self.active_channel, &self.me.id) {
            return;
        }
        // Read state is a durable collaboration fact.  It is authored through
        // the same action host as palette "Mark as read", not a side write
        // that peers/history cannot account for.  The carried count makes the
        // replay independent of messages arriving after the person viewed it.
        let action = actions::mark_read_action(&self.active_channel, count);
        self.submit_action(&action);
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
        let mut in_voice: HashMap<&str, String> = HashMap::new();
        for (ch_id, members) in &self.voice_members {
            let ch_name = self
                .channel(ch_id)
                .map_or_else(|| ch_id.clone(), |c| c.name.clone());
            for m in members {
                in_voice.insert(m.user.as_str(), ch_name.clone());
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
        for agent in &self.room_agents {
            let canonical = format!("agent:{agent}");
            if rows.iter().any(|(user, _)| user.id == canonical) {
                continue;
            }
            let name = self.agent_display_name(agent);
            let color_idx = crate::identity::color_from_bytes(canonical.as_bytes());
            rows.push((
                User {
                    id: canonical,
                    initials: crate::identity::initials_for(&name),
                    color_idx,
                    name,
                    status: crate::model::Presence::Offline,
                    is_agent: true,
                },
                None,
            ));
        }
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
        rows
    }

    /// Whether this workspace has somebody besides the local participant. An
    /// agent is still a collaborator (and should surface collaboration
    /// controls), but it is never counted or labelled as a person.
    pub(crate) fn has_other_collaborator(&self) -> bool {
        self.users.iter().any(|u| !self.is_me(&u.id)) || !self.room_agents.is_empty()
    }

    /// The roster's public destination name. `People` is precise when every
    /// entry is human; once an agent is present, `Collaborators` accurately
    /// covers both and the row itself keeps the agent visibly typed.
    pub(crate) fn roster_label(&self) -> &'static str {
        if !self.room_agents.is_empty() || self.users.iter().any(|u| u.is_agent) {
            "Collaborators"
        } else {
            "People"
        }
    }

    // --- window geometry ---------------------------------------------------
    //
    // Two drawers frame the conversation: the navigation drawer (workspace
    // rail beside the channel list) at the leading edge, and the detail
    // drawer (people, or the open thread) at the trailing edge. The toolkit's
    // `SidebarDrawer` decides, from the window width, whether each docks
    // beside the conversation or floats over it behind a scrim, eases the
    // reveal and remembers the person's show/hide choice per size class.

    /// The leading navigation drawer: the workspace rail beside the channel
    /// list, at the shell's sidebar and rail metrics. It docks only while the
    /// conversation column keeps the shared detail minimum; below that it
    /// becomes a floating drawer.
    fn navigation_drawer() -> SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        // One drawer holds the rail (icon-sized, following the UI scale) and
        // the channel list (text-sized) beside it.
        SidebarDrawer::new()
            .with_rail()
            .content_min_width(f64::from(m.detail_min_width))
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

    /// The navigation drawer's panel: header row, rail and channel list.
    pub(crate) fn nav_rect(&self) -> Rect {
        self.left_flap.sidebar_rect()
    }
    /// The workspace rail: the drawer's leading column under the header row.
    pub(crate) fn rail_rect(&self) -> Rect {
        let rail = self.left_flap.rail_rect();
        let y0 = (rail.y0 + self.chrome.header_h).min(rail.y1);
        Rect::new(rail.x0, y0, rail.x1, rail.y1)
    }
    /// The channel list beside the rail, under the header row.
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
    /// Reserve the canonical editor's measured height and its action row.
    pub(crate) fn composer_input_height(&self) -> f64 {
        let chrome = &self.chrome;
        let pad = chrome.composer_min_h - chrome.body_line;
        let draft = (self.composer_main.measured_height() + pad)
            .clamp(chrome.composer_min_h, chrome.composer_max_h);
        draft
            + if self.direct_agent.is_some() {
                self.chrome.composer_min_h
            } else {
                0.0
            }
    }

    pub(crate) fn direct_agent_status_lines(
        &self,
        tasks: &ui::agent_task_frontier::AgentTaskFrontiers,
        stopping: bool,
    ) -> crate::render_chat::AgentTaskStatusLines {
        use ui::agent_task_frontier::AgentTaskChoice;

        let current = match tasks.choice() {
            AgentTaskChoice::One(task) | AgentTaskChoice::Selected(task) => Some(task),
            AgentTaskChoice::None | AgentTaskChoice::Choose(_) => None,
        };
        let mut lines = crate::render_chat::agent_task_status_lines(tasks, stopping);
        if !self.direct_agent_is_available() {
            lines.primary = current.as_ref().map_or_else(
                || crate::i18n::t("chat-agent-task-unavailable"),
                |task| {
                    crate::i18n::t_args(
                        "chat-agent-task-unavailable-last",
                        &[("title", task.title.clone().into())],
                    )
                },
            );
            lines.receipt = Some(crate::i18n::t("chat-agent-unavailable"));
            lines.queue = None;
        } else if !self.can_participate()
            && self.direct_agent_intent() == AgentComposerIntent::Message
        {
            lines.receipt = Some(crate::i18n::t("chat-agent-message-read-only"));
        } else if let Some(agent) = self.direct_agent.as_ref()
            && let Some(error) = self
                .agent_submission_errors
                .get()
                .get(&(self.active_ws.clone(), agent.clone()))
        {
            lines.receipt = Some(error.clone());
        } else if self
            .direct_agent
            .as_ref()
            .is_some_and(|agent| self.agent_message_awaiting_confirmation(agent))
        {
            lines.receipt = Some(crate::i18n::t("chat-agent-message-awaiting-confirmation"));
        }
        lines
    }

    fn composer_height_with_notice(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        hero_notice: bool,
    ) -> f64 {
        let field = self.composer_input_height();
        if self.direct_agent.is_none() {
            return field;
        }
        let rows = self.direct_agent_tasks().map_or(2, |tasks| {
            let tasks = tasks.borrow();
            let stopping = matches!(
                tasks.choice(),
                ui::agent_task_frontier::AgentTaskChoice::One(ref task)
                    | ui::agent_task_frontier::AgentTaskChoice::Selected(ref task)
                    if task.cancellation_requested
            ) || matches!(
                tasks.stop_state(),
                ui::agent_task_frontier::AgentStopState::Stopping { .. }
            );
            let choice =
                usize::from(!crate::render_chat::selectable_agent_tasks(&tasks).is_empty());
            let mut status = self.direct_agent_status_lines(&tasks, stopping);
            if hero_notice {
                status.receipt = None;
            }
            let picker = self.agent_intent_picker_rows(
                tr,
                theme,
                (self.center_right() - self.center_left() - 2.0 * self.chrome.gutter).max(0.0),
            );
            choice
                + status.count()
                + usize::from(self.has_unavailable_agent_message_progress())
                + picker
        });
        let rows = rows.max(1);
        field
            + rows as f64 * self.chrome.agent_control_h
            + rows.saturating_sub(1) as f64 * self.chrome.agent_control_gap
    }

    pub(crate) fn composer_height(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        self.composer_height_with_notice(tr, theme, self.agent_notice_in_empty_hero)
    }

    fn composer_rect_with_notice(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        hero_notice: bool,
    ) -> Rect {
        let wanted = self.composer_height_with_notice(tr, theme, hero_notice);
        self.composer_rect_for_height(wanted)
    }

    pub(crate) fn composer_rect(&self, tr: &mut TextRenderer, theme: &Theme) -> Rect {
        let wanted = self.composer_height(tr, theme);
        self.composer_rect_for_height(wanted)
    }

    fn composer_rect_for_height(&self, wanted: f64) -> Rect {
        let regular_pad = self.chrome.composer_pad;
        let regular_room = (self.rect.y1 - regular_pad)
            - (self.header_rect().y1 + regular_pad).min(self.rect.y1 - regular_pad);
        let pad = if wanted <= regular_room.max(0.0) {
            regular_pad
        } else {
            self.chrome.agent_control_gap
        };
        let bottom = self.rect.y1 - pad;
        let top_limit = (self.header_rect().y1 + pad).min(bottom);
        let h = wanted.min((bottom - top_limit).max(0.0));
        Rect::new(
            self.center_left() + self.chrome.gutter,
            bottom - h,
            self.center_right() - self.chrome.gutter,
            bottom,
        )
    }

    /// Resolve the empty-state/composer notice ownership against the larger,
    /// full-notice composer first. Omitting the duplicate row can only add
    /// message space, so this cannot oscillate between layouts.
    pub(crate) fn resolve_agent_notice_layout(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.agent_notice_in_empty_hero = false;
        let has_agent_conversation = self.direct_agent.is_some();
        let has_no_messages = self.messages.is_empty();
        let needs_notice = !self.direct_agent_is_available() || !self.can_participate();
        if !(has_agent_conversation && has_no_messages && needs_notice) {
            return;
        }
        let full = self.composer_rect_with_notice(tr, theme, false);
        let hero = Rect::new(
            self.center_left(),
            self.header_rect().y1,
            self.center_right(),
            (full.y0 - self.chrome.avatar_gap).max(self.header_rect().y1),
        );
        let agent = self.direct_agent.as_deref().unwrap_or_default();
        let name = self.agent_display_name(agent);
        let title = crate::i18n::t_args(
            if self.direct_agent_is_available() {
                "chat-agent-empty-read-only-title"
            } else {
                "chat-agent-empty-unavailable-title"
            },
            &[("name", name.into())],
        );
        self.agent_notice_in_empty_hero =
            self.agent_empty_state_will_paint(tr, theme, hero, &title);
    }

    /// The thread's composer: the same control and height rule as the main
    /// composer, at the bottom of the thread panel `panel`.
    pub(crate) fn thread_composer_rect(&self, panel: Rect) -> Rect {
        let pad = self.chrome.composer_pad;
        let chrome = &self.chrome;
        let inset = chrome.composer_min_h - chrome.body_line;
        let h = (self.composer_thread.measured_height() + inset)
            .clamp(chrome.composer_min_h, chrome.composer_max_h);
        Rect::new(
            panel.x0 + pad,
            panel.y1 - h - pad,
            panel.x1 - pad,
            panel.y1 - pad,
        )
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
        let mut ws_changed = false;
        if !self.workspaces.iter().any(|w| w.id == self.active_ws)
            && let Some(first) = self.workspaces.first()
        {
            self.active_ws = first.id.clone();
            self.context_ws = first.id.clone();
            self.active_channel.clear();
            ws_changed = true;
        }
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

    /// Warm the attachment-image cache for the current channel and an open
    /// picker, so the frame shows decoded thumbnails as soon as the
    /// background loader has them.
    pub(crate) fn request_visible_images(&self) {
        // `self.messages` already holds every message in the active channel,
        // including threaded replies and the thread parent, so one pass covers
        // the timeline and the open thread panel.
        for m in &self.messages {
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
                    Hit::Agent(agent) => (
                        crate::i18n::t_args(
                            "chat-agent-roster-label",
                            &[("name", self.agent_display_name(agent).into())],
                        ),
                        Role::ListItem,
                        self.direct_agent.as_deref() == Some(agent.as_str()),
                    ),
                    Hit::AgentWindow => (
                        if self.agent_window_is_return() {
                            crate::i18n::t("chat-agent-return")
                        } else {
                            crate::i18n::t("chat-agent-float")
                        },
                        Role::Button,
                        false,
                    ),
                    Hit::AgentDragHandle => {
                        (crate::i18n::t("chat-agent-drag"), Role::Button, false)
                    }
                    Hit::AgentIntent(intent) => (
                        crate::i18n::t(match intent {
                            AgentComposerIntent::Message => "chat-agent-mode-message",
                            AgentComposerIntent::Queue => "chat-agent-intent-queue",
                            AgentComposerIntent::Steer => "chat-agent-intent-steer",
                        }),
                        Role::Tab,
                        *intent == self.direct_agent_intent(),
                    ),
                    Hit::AgentTask(task) => (
                        crate::i18n::t_args(
                            "chat-agent-select-task",
                            &[("task", task.title.as_str().into())],
                        ),
                        Role::ListItem,
                        false,
                    ),
                    Hit::AgentTaskCenter(Some(task)) => (
                        self.direct_agent_task_accessible_status(task)
                            .or_else(|| self.direct_agent_task_title(task))
                            .map_or_else(
                                || crate::i18n::t("chat-agent-open-current-task"),
                                |title| {
                                    crate::i18n::t_args(
                                        "chat-agent-open-task",
                                        &[("title", title.into())],
                                    )
                                },
                            ),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentTaskCenter(None) => (
                        crate::i18n::t("chat-agent-open-task-center"),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentStop => (
                        crate::i18n::t("chat-agent-stop-current"),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentMessageRefresh => (
                        crate::i18n::t("chat-agent-progress-refresh"),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentWorkToggle(message) => {
                        let expanded = self
                            .agent_work_expanded
                            .with(|messages| messages.contains(message));
                        (
                            if expanded {
                                crate::i18n::t("chat-agent-work-hide-details")
                            } else {
                                crate::i18n::t("chat-agent-work-show-details")
                            },
                            Role::Button,
                            false,
                        )
                    }
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
                    Hit::AddCategory(_) => ("Add category".into(), Role::Button, false),
                    Hit::FocusRename => ("Rename field".into(), Role::TextInput, false),
                    Hit::FocusEdit => ("Edit message".into(), Role::MultilineTextInput, false),
                    Hit::FocusMain => (
                        crate::i18n::t(match self.direct_agent_intent() {
                            AgentComposerIntent::Message => "chat-agent-composer-message",
                            AgentComposerIntent::Queue => "chat-agent-composer-queue",
                            AgentComposerIntent::Steer => "chat-agent-composer-steer",
                        }),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::FocusThread => (
                        "Thread reply composer".into(),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::Send => (
                        crate::i18n::t(match self.direct_agent_intent() {
                            AgentComposerIntent::Message => "chat-agent-send-message",
                            AgentComposerIntent::Queue => "chat-agent-send-queue",
                            AgentComposerIntent::Steer => "chat-agent-send-steer",
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::JumpToLatest => ("Jump to latest message".into(), Role::Button, false),
                    Hit::ComposerEmoji(_) => {
                        (crate::i18n::t("chat-emoji-insert"), Role::Button, false)
                    }
                    Hit::Attach => ("Attach a workspace file".into(), Role::Button, false),
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
                    Hit::CopyResourceLink(_) => (
                        crate::i18n::t("chat-resource-copy-link"),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenResourceLink(_) => (
                        crate::i18n::t("chat-resource-open-link"),
                        Role::Button,
                        false,
                    ),
                    Hit::Search => ("Search Chat".into(), Role::Button, false),
                    Hit::ToggleLeftFlap => (
                        if self.left_flap_open() {
                            crate::i18n::t("chat-navigation-hide")
                        } else {
                            crate::i18n::t("chat-navigation-show")
                        },
                        Role::Button,
                        false,
                    ),
                    Hit::ToggleRightFlap => ("Show people or thread".into(), Role::Button, false),
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

    /// Resolve an accessibility target to the exact painted keyboard-ring
    /// entry that pointer and physical-key activation share.
    fn semantic_hit(&self, target: &str) -> Option<(usize, Hit)> {
        self.semantic_hits()
            .into_iter()
            .enumerate()
            .find_map(|(index, (_, hit, ..))| (hit.semantic_id() == target).then_some((index, hit)))
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
                        | Hit::Attach
                        | Hit::Send
                        | Hit::AgentDragHandle
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
    fn apply_collaboration_context(&mut self, presence: &PresenceCtx) {
        self.adopt_room(presence);
    }

    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.queue_realtime_registration();
        if self.realtime.target_media_ready && self.realtime_target_media_ready() {
            self.realtime.target_media_ready = false;
        }
        let mut requests = std::mem::take(&mut self.realtime.requests);
        requests.extend(self.resource_navigation_requests());
        requests.extend(self.agent_task_actions.take_collab_requests());
        requests.extend(
            self.agent_message_progress
                .borrow_mut()
                .take_collab_requests(),
        );
        let before = self.native_voice.snapshot.state;
        requests.extend(self.native_voice.take_requests(ui::time::now()));
        if before != self.native_voice.snapshot.state {
            self.project_native_voice();
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        let progress_before = self.agent_message_progress_presentation();
        self.agent_message_progress
            .borrow_mut()
            .handle_collab_event(event);
        if progress_before != self.agent_message_progress_presentation() {
            self.canonical_dirty = true;
            self.request_relayout();
        }
        self.agent_task_actions.handle_collab_event(event);
        self.poll_agent_tasks();
        match event {
            ui::collaboration::CollabEvent::RemoteAction { message } => {
                self.complete_resource_navigation(message);
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
            .find(|surface| surface.id == id)?
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
            .find(|surface| surface.id == id)
        else {
            return false;
        };
        let handled = surface
            .view
            .perform_semantic_action(child_target, action, state, path);
        self.settle_agent_surface_commands();
        handled
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(winit::window::ImePurpose, Rect)> {
        let surface = self
            .agent_surfaces
            .values()
            .find(|surface| surface.id == id)?;
        Some((surface.view.ime_purpose()?, surface.view.ime_cursor_rect()?))
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
            surface.view.perms = self.perms.clone();
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_colors.clone_from(&self.room_colors);
            if matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            ) || surface.placement.is_return_pending()
            {
                surface.view.rect = surface.placement.rect(surface.source, desktop);
                surface
                    .view
                    .draw(scene, transform, text_renderer, theme, presence);
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
            if !matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::Desktop { .. }
            ) && !surface.placement.is_return_pending()
            {
                return false;
            }
            surface.view.perms = self.perms.clone();
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.rect = surface.placement.rect(surface.source, desktop);
            let content_rect = surface.view.rect;
            return ui::floating_surface::paint_app_owned_content(
                scene,
                transform,
                content_rect,
                theme,
                |scene| {
                    surface
                        .view
                        .draw(scene, transform, text_renderer, theme, presence);
                    true
                },
            );
        }
        if id != format!("chat-call:{}", self.call.channel)
            || !self.call.active
            || self.call.expanded
            || (!self.call.placement.is_return_pending()
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
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
            self.focused_agent_surface = id
                .filter(|id| self.agent_surface_accepts_input(id))
                .map(str::to_owned);
        } else if self.focused_agent_surface.as_deref() == id {
            self.focused_agent_surface = None;
        }
        for surface in self.agent_surfaces.values_mut() {
            let surface_id = Some(surface.id.as_str());
            surface.view.set_focus(
                self.focused_agent_surface.as_deref() == surface_id
                    || self.ime_source_agent_surface.as_deref() == surface_id,
            );
        }
        self.set_composer_focus();
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        self.ime_source_agent_surface = id
            .filter(|id| self.agent_surface_accepts_ime(id))
            .map(str::to_owned);
        for surface in self.agent_surfaces.values_mut() {
            let surface_id = Some(surface.id.as_str());
            surface.view.set_focus(
                self.focused_agent_surface.as_deref() == surface_id
                    || self.ime_source_agent_surface.as_deref() == surface_id,
            );
        }
        self.set_composer_focus();
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

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        use ui::widget::Widget as _;

        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        self.for_each_child(&mut |child| {
            bindings.extend(ui::widget::controls::control_action_bindings(child));
        });
        for category in self.ws_categories() {
            let control_id = Hit::CategoryToggle(category.id.clone()).semantic_id();
            let resource = format!("chat.category:{}", category.id);
            for action in ["open-category", "rename", "toggle-category", "delete"] {
                bindings.push(ui::widget::ControlBindingMetadata {
                    control_id: control_id.clone(),
                    kind: ui::widget::ControlBindingKind::Action(tilo_actions::ActionAddress::new(
                        &resource, action,
                    )),
                });
            }
        }
        bindings
    }
}

impl ChatApp {
    fn conversation_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};

        if let Some((name, detached)) = self.detached_agent_placeholder_status() {
            let title = crate::i18n::t_args(
                if detached {
                    "chat-agent-detached-title"
                } else {
                    "chat-agent-moving-title"
                },
                &[("name", name.into())],
            );
            return AccessNode::new(Role::StaticText)
                .with_name(title)
                .with_description(crate::i18n::t(if detached {
                    "chat-agent-detached-message"
                } else {
                    "chat-agent-moving-message"
                }))
                .with_hashed_key("detached-agent-placeholder");
        }

        let messages = self
            .messages
            .iter()
            .filter(|message| {
                if self.direct_agent.is_some() {
                    true
                } else if self.thread_is_page() {
                    Some(&message.id) == self.thread_open.as_ref()
                        || message.parent.as_ref() == self.thread_open.as_ref()
                } else {
                    message.parent.is_none() || message.parent.as_ref() == self.thread_open.as_ref()
                }
            })
            .map(|message| {
                let author = self
                    .user(&message.author)
                    .map_or("Unknown person", |user| user.name.as_str());
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(format!("{author}: {}", message.text))
                    .with_hashed_key((&message.id, "message"));
                let mut descriptions = Vec::new();
                if let Some(work) = &message.work {
                    descriptions.push(crate::render_chat::agent_work_accessible_description(work));
                }
                if let Some(progress) = self.agent_message_progress_label(message) {
                    descriptions.push(progress);
                }
                if !descriptions.is_empty() {
                    node = node.with_description(descriptions.join(". "));
                }
                node
            })
            .collect();
        AccessNode::new(Role::List)
            .with_name("Messages")
            .with_children(messages)
            .with_hashed_key("message-list")
    }

    fn conversation_heading_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        if self.direct_agent.is_some() {
            return self.agent_accessibility_nodes();
        }
        let Some(channel) = self.channel(&self.active_channel) else {
            return Vec::new();
        };
        let mut heading = AccessNode::new(Role::Heading { level: 1 })
            .with_name(channel.kind.label(&channel.name))
            .with_bounds(self.header_rect())
            .with_hashed_key(("conversation-heading", &channel.id));
        if !channel.topic.trim().is_empty() {
            // Elision in the painted header must not hide the complete topic
            // from screen readers, including in a narrow conversation.
            heading = heading.with_description(channel.topic.clone());
        }
        vec![heading]
    }

    fn agent_accessibility_nodes(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let Some(agent) = self.direct_agent.as_deref() else {
            return Vec::new();
        };
        let mut nodes = vec![
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(crate::i18n::t_args(
                    "chat-agent-roster-label",
                    &[("name", self.agent_display_name(agent).into())],
                ))
                .with_bounds(self.header_rect())
                .with_hashed_key(("agent-conversation-heading", agent)),
        ];
        if let Some(tasks) = self.direct_agent_tasks() {
            let tasks = tasks.borrow();
            let stopping = matches!(
                tasks.choice(),
                ui::agent_task_frontier::AgentTaskChoice::One(ref task)
                    | ui::agent_task_frontier::AgentTaskChoice::Selected(ref task)
                    if task.cancellation_requested
            ) || matches!(
                tasks.stop_state(),
                ui::agent_task_frontier::AgentStopState::Stopping { .. }
            );
            let status = self.direct_agent_status_lines(&tasks, stopping);
            for (kind, value) in [
                ("task", Some(status.primary)),
                ("receipt", status.receipt),
                ("queue", status.queue),
            ] {
                if let Some(value) = value {
                    nodes.push(
                        AccessNode::new(Role::StaticText)
                            .with_name(value)
                            .with_hashed_key(("agent-task-status", kind, &self.active_ws, agent)),
                    );
                }
            }
        } else {
            let status = self
                .agent_submission_errors
                .get()
                .get(&(self.active_ws.clone(), agent.to_owned()))
                .cloned()
                .or_else(|| {
                    (!self.direct_agent_is_available())
                        .then(|| crate::i18n::t("chat-agent-unavailable"))
                })
                .or_else(|| {
                    self.agent_message_awaiting_confirmation(agent)
                        .then(|| crate::i18n::t("chat-agent-message-awaiting-confirmation"))
                })
                .or_else(|| {
                    (!self.can_participate()
                        && self.direct_agent_intent() == AgentComposerIntent::Message)
                        .then(|| crate::i18n::t("chat-agent-message-read-only"))
                });
            if let Some(status) = status {
                nodes.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(status)
                        .with_hashed_key(("agent-submission-status", &self.active_ws, agent)),
                );
            }
        }
        if !self.direct_agent_intent_allowed(self.direct_agent_intent()) {
            let draft = self.composer_main.value();
            if !draft.is_empty() {
                nodes.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(draft)
                        .with_hashed_key(("agent-read-only-draft", &self.active_ws, agent)),
                );
            }
        }
        nodes
    }
}

impl ui::undo::LocalUndo for ChatApp {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        if let Some(id) = self.input_agent_surface.as_deref() {
            let Some(surface) = self
                .agent_surfaces
                .values_mut()
                .find(|surface| surface.id == id)
            else {
                // The native runner scopes this source to one dispatched
                // event. A stale or foreign source consumes the local
                // shortcut without falling through to another private editor
                // or the shared document history.
                return true;
            };
            if !surface.placement.is_desktop_adopted()
                || surface.placement.is_return_pending()
                || surface.is_closed()
            {
                return true;
            }
            // The native event-source ID is the custody proof for this one
            // shortcut. The asynchronous focus callback may already have
            // retired while the event is in flight, so route directly to the
            // retained private editor without synthesizing view focus.
            _ = surface
                .view
                .composer_main
                .handle_owned_local_undo_request(request);
            return true;
        }
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values_mut()
                .find(|surface| surface.id == id)
        {
            return ui::undo::LocalUndo::handle_local_undo_request(surface.view.as_mut(), request);
        }
        if self.editing.is_some() {
            self.edit_input.handle_local_undo_request(request)
        } else {
            self.focused_composer_mut()
                .handle_local_undo_request(request)
        }
    }
}

impl Widget for ChatApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::undo::LocalUndo,
        dyn ui::collaboration::participants::Participants,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::view::capability::ViewState
    );

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn native_input_snapshot(
        &self,
        _state: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        let current = self
            .direct_agent_tasks()
            .and_then(|tasks| tasks.borrow().context_task())
            .map(|task| {
                let execution = task.execution.map(|execution| {
                    serde_json::json!({
                        "run_epoch": execution.run_epoch,
                        "sequence": execution.sequence,
                        "phase": execution.phase,
                    })
                });
                serde_json::json!({
                    "task_id": task.task_id,
                    "run_epoch": task.run_epoch,
                    "execution": execution,
                })
            });
        let now = ui::time::now();
        let pending_messages = self.agent_submissions.get();
        let pending_message_count = pending_messages
            .values()
            .filter(|submission| submission.intent == AgentComposerIntent::Message)
            .count();
        let message_submission = self.direct_agent.as_ref().and_then(|agent| {
            let key = (self.active_ws.clone(), agent.clone());
            let submission = pending_messages.get(&key)?;
            (submission.intent == AgentComposerIntent::Message).then(|| {
                serde_json::json!({
                    "phase": submission.message.as_ref().map_or("uninitialized", Self::agent_message_submission_phase),
                    "pending_count": pending_message_count,
                    "poll_scheduled": submission.next_poll_at.is_some(),
                    "poll_due": submission.next_poll_at.is_some_and(|deadline| deadline <= now),
                    "resolve_expired": submission.give_up_at <= now,
                    "draft_revision": submission.draft_revision,
                    "current_revision": self
                        .composer_main
                        .private_revision_for(&key.0, &key.1),
                    "draft_empty": self
                        .composer_main
                        .private_plain()
                        .is_none_or(|draft| draft.is_empty()),
                })
            })
        });
        Some(serde_json::json!({
            "app": "so.tilo.chat",
            "keyboard_control": self.keyboard_control_snapshot(),
            "composer_focused": self.composer_main.is_focused(),
            "agent_float_frontier": self.agent_float_frontier_snapshot(),
            "agent_task": current,
            "agent_task_presentation": Self::agent_task_snapshot(self),
            "agent_message_progress": self.agent_message_progress_snapshot(),
            "agent_message_submission": message_submission,
        }))
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let tasks = self
            .agent_tasks
            .values()
            .filter_map(|tasks| tasks.borrow().next_wake_deadline())
            .min();
        let messages = self.agent_submissions.with(|submissions| {
            submissions
                .values()
                .filter(|submission| submission.intent == AgentComposerIntent::Message)
                .map(|submission| {
                    submission
                        .message
                        .as_ref()
                        .map_or(submission.give_up_at, |_| {
                            Self::agent_message_next_wake(submission)
                                .unwrap_or(submission.give_up_at)
                        })
                })
                .min()
        });
        let receipts = messages.and_then(|_| self.agent_task_actions.next_observation_deadline());
        let agent_messages = self.agent_message_progress.borrow().next_wake_deadline();
        [tasks, messages, receipts, agent_messages]
            .into_iter()
            .flatten()
            .min()
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
    #[expect(
        clippy::float_cmp,
        reason = "the clamped target is compared exactly: identical means the scroll did not move"
    )]
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        use ui::widget::PrimaryScrollResult;
        let Some(region) = self.primary_vertical_scroll() else {
            return PrimaryScrollResult::Unsupported;
        };
        let Some(next) = action.offset(region.metrics) else {
            return PrimaryScrollResult::Unsupported;
        };
        if next == region.metrics.offset {
            return PrimaryScrollResult::Unchanged(region);
        }
        if region.key == "thread" {
            self.thread_scroll.set_offset_y(ui::num::f32_of_f64(next));
        } else {
            self.scroll = region.metrics.max_scroll() - next;
            self.timeline_at_end = self.scroll <= 0.5;
        }
        self.canonical_dirty = true;
        self.primary_vertical_scroll().map_or(
            PrimaryScrollResult::Unsupported,
            PrimaryScrollResult::Changed,
        )
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
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values()
                .find(|surface| surface.id == id)
        {
            return surface.view.ime_purpose();
        }
        self.host_focused
            .then_some(winit::window::ImePurpose::Normal)
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
        self.settle_attachment_import();
        {
            use automerge::transaction::Transactable as _;
            // Reload and repaint only when the document moved: committed heads
            // differ, or a transaction is or was open. Service completions run
            // before this comparison so their canonical changes are projected
            // in the same frame without making idle sync reload continuously.
            let heads = doc.base_heads();
            let pending = doc.pending_ops();
            let unchanged = self
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
        // The channel in view is read: advance its watermark (no-op when
        // nothing new arrived) so its unread badge stays cleared. Only a
        // presented instance has a viewer; a headless host worker never
        // authors read receipts.
        if self.presented {
            self.mark_active_read(doc);
        }
        self.sync_composers(doc);
        self.poll_agent_tasks();
        for surface in self.agent_surfaces.values_mut() {
            surface.view.room_agents.clone_from(&self.room_agents);
            surface.view.room_names.clone_from(&self.room_names);
            surface.view.room_colors.clone_from(&self.room_colors);
            surface.view.perms = self.perms.clone();
            surface.view.reload(doc);
            surface.view.sync_composers(doc);
        }
        self.poll_agent_message_progress();
        self.request_visible_images();
        // Native capture/transport belongs to the shell's lease-bound service.
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
                size: Size {
                    width: taffy::style_helpers::length(w),
                    height: taffy::style_helpers::length(h),
                },
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
                size: Size {
                    width: percent(f.2 as f32),
                    height: percent(f.3 as f32),
                },
                ..Default::default()
            }
        };
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let video_node = taffy.new_leaf(video_style).unwrap();
        node_map.insert(video_node, &raw mut self.video as *mut dyn Widget);
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
                    size: Size {
                        width: percent(1.0_f32),
                        height: percent(1.0_f32),
                    },
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
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        if !self.presented {
            self.presented = true;
            self.read_on_present = true;
        }
        self.rect = ui::widget::placed(layout, origin);
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
        // is painted last so it remains legible on rail, sidebar and popovers.
        let semantic = Self::deduplicate_semantic_hits(self.semantic_hits_from(&hits));
        let focused_index = match self.keyboard_target_semantic.as_deref() {
            Some(target) => semantic
                .iter()
                .position(|(_, hit, ..)| hit.semantic_id() == target),
            None => self.keyboard_target.filter(|index| *index < semantic.len()),
        };
        self.keyboard_target = focused_index;
        if focused_index.is_none() {
            self.keyboard_target_semantic = None;
        }
        if self.host_focused
            && let Some(index) = focused_index
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
                self.clear_keyboard_target();
            }
        }

        if let Some((label, anchor)) = self.tooltips.visible() {
            ui::widget::tooltip::draw_hover_tooltip(scene, tf, tr, theme, anchor, label, self.rect);
        }

        self.hits = hits;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        // Adopt the OS-provided permission context so channel participation and
        // file-embed hydration are gated by the real grant policy (the
        // compositor hands each app its context here every frame).
        self.perms = ChatPermissions::from_context(permissions.clone());
        // Adopt the room's answer for who everybody is. Names and colours are
        // the room's to give: they arrive on the collaboration context stream
        // and land in the presence store, and this is the one door an app has
        // onto that store. Taken before `draw` so the same frame that learns a
        // name paints it.
        self.adopt_room(presence);
        // Root spans the whole window — always paint.
        self.draw(scene, tf, tr, theme, presence);
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
        if self.direct_agent.is_some() && self.composer_main.is_focused() {
            let mut overlay_hits = Vec::new();
            self.draw_header(scene, tf, tr, theme, &mut overlay_hits);
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.widget.draw_overlay(scene, tf, tr, theme, presence);
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
            let app = self.rect;
            let desktop = if self.floating_surface_bounds.area() > 0.0 {
                self.floating_surface_bounds
            } else {
                app
            };
            if surface.route_drag_event(event, ctx.scale_factor, app, desktop) {
                *ctx.handled = true;
                self.canonical_dirty = true;
                self.request_relayout();
                return;
            }
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
        use winit::window::CursorIcon;

        if self.call.placement.is_dragging() || self.agent_conversation_drag.is_some() {
            return Some(CursorIcon::Grabbing);
        }
        if self.hits.iter().rev().any(|(rect, hit)| {
            rect.contains(point) && matches!(hit, Hit::AgentDragHandle | Hit::CallDragHandle)
        }) {
            return Some(CursorIcon::Grab);
        }
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
                let semantic_id = hit.semantic_id();
                let input = self.text_field_access(&hit);
                let focused = self.host_focused
                    && self.resolved_keyboard_target().map_or_else(
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
                if matches!(hit, Hit::Send) {
                    state = state.disabled(!self.focused_send_enabled());
                }
                let mut node = AccessNode::new(role)
                    .with_name(name)
                    .with_state(state)
                    .with_bounds(rect)
                    .with_action_target(semantic_id.clone());
                node = if matches!(hit, Hit::CategoryToggle(_)) {
                    node.with_hashed_key(semantic_id)
                } else {
                    node.with_key(index as u64)
                };
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
        children.extend(self.conversation_heading_accessibility());
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
        let Some((_, hit, ..)) = self.semantic_hits().get(*index).cloned() else {
            return false;
        };
        self.keyboard_target = Some(*index);
        self.keyboard_target_semantic = Some(hit.semantic_id());
        true
    }

    /// Semantic targets are the same inventory used by pointer and Tab/Enter
    /// input. Keeping this adapter at the widget boundary prevents an
    /// assistive-technology activation from taking a separate, untracked
    /// mutation path: `on_hit` builds the same `ChatAction` and delegates it
    /// to `ActionHost` for history and collaboration.
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
        let Some((hit_index, hit)) = self.semantic_hit(target) else {
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
            use ui::a11y::Action;

            // `keyboard_target` is the immediate-mode Tab ring. A semantic
            // editor action moves focus into the actual TextInput instead, so
            // let that editor be the single focused accessibility child.
            self.clear_keyboard_target();

            match action {
                Action::Focus | Action::Activate => {
                    self.on_hit(hit, state);
                    return true;
                }
                Action::SetValue(_) | Action::SetSelection { .. } => match hit {
                    Hit::FocusMain => {
                        self.focus = Focus::Main;
                        let channel = self.active_channel.clone();
                        self.ensure_composer_draft(state, &channel);
                        self.composer_main.sync(state);
                        if !self.composer_main.perform_action(action, state, path) {
                            return false;
                        }
                        self.reload(state);
                    }
                    Hit::FocusThread => {
                        let Some(key) = self.thread_draft_key() else {
                            return false;
                        };
                        self.focus = Focus::Thread;
                        self.ensure_composer_draft(state, &key);
                        self.composer_thread.sync(state);
                        if !self.composer_thread.perform_action(action, state, path) {
                            return false;
                        }
                        self.reload(state);
                    }
                    Hit::FocusRename => {
                        if self.rename.is_none() {
                            return false;
                        }
                        if !ui::Widget::perform_action(&mut self.rename_input, action, state, path)
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
            return true;
        }
        if matches!(action, ui::a11y::Action::Focus) {
            self.focus_keyboard_target(hit_index, &hit);
            return true;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        if let Hit::AgentTask(frontier) = &hit {
            self.prepare_hit_activation();
            return self.select_agent_task_frontier(frontier);
        }
        let messages_before = matches!(hit, Hit::Send)
            .then(|| ChatDoc::list_messages(state, &self.active_channel).len());
        let submission_before =
            matches!(hit, Hit::Send).then(|| self.agent_submission_acceptances.get());
        let refresh_before = matches!(hit, Hit::AgentMessageRefresh)
            .then(|| self.agent_message_refresh_acceptances.get());
        self.on_hit(hit, state);
        let send_accepted = messages_before.is_none_or(|before| {
            if ChatDoc::list_messages(state, &self.active_channel).len() == before.saturating_add(1)
            {
                return true;
            }
            submission_before.is_some_and(|before| self.agent_submission_acceptances.get() > before)
        });
        send_accepted
            && refresh_before
                .is_none_or(|before| self.agent_message_refresh_acceptances.get() > before)
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
        // Re-run `sync` once after the first layout so the channel already in
        // view acquires its read receipt on open.
        std::mem::take(&mut self.read_on_present)
    }

    fn needs_animation_frame(&self) -> bool {
        self.read_on_present
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

    fn wants_pointer_capture(&self) -> bool {
        self.left_flap.is_resizing()
            || self.right_flap.is_resizing()
            || self.call.placement.is_dragging()
            || self.agent_conversation_drag.is_some()
            || self
                .agent_surfaces
                .values()
                .any(|surface| surface.placement.is_dragging())
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
            self.clear_keyboard_target();
            self.reconcile_flaps();
            self.canonical_dirty = true;
        }
        handled
    }
}

impl ui::presence::capability::PublishesPresence for ChatApp {
    fn presence_payload(&self) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        self.composer_presence()
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
                && let Some(dispatch) = &self.dispatch
            {
                dispatch.apply(&ChatAction::NameAgent {
                    participant: participant.as_str().to_owned(),
                    name: label.to_owned(),
                });
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
    /// (rail and channel list), each over its scrim when it floats, and the
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
                    } else {
                        self.draw_members(scene, tf, tr, &local, hits);
                    }
                    scene.pop_layer();
                }
            }
        }

        // The navigation drawer: the workspace rail beside the channel list.
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
            self.draw_rail(scene, tf, tr, &local, hits);
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
    }

    #[cfg(test)]
    pub(crate) fn take_relayout_request(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }
}

#[cfg(test)]
mod identity_on_screen {
    use super::*;
    use ui::widget::Widget;

    /// A window's person joins the roster through `EnsureUser`.
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
        ChatDoc::seed_starter(&mut state.doc, "owner", "Owner", "OW");
        Widget::sync(&mut app, &state.doc, &state.root);
        app.land(&mut state.doc);
        let recorded = app
            .last_dispatched()
            .expect("the live peer is submitted as EnsureUser");
        assert_eq!(recorded.type_id.as_str(), actions::CHAT_ACTION_TYPE);
        assert!(crate::doc::ChatDoc::user_is_current(
            &state.doc,
            &app.me.id,
            &app.me.name,
            &app.me.initials,
            app.me.color_idx,
            crate::model::Presence::Online,
        ));
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
        ChatDoc::seed_starter(&mut state.doc, stranger, "You", "Y");
        ChatDoc::push_message(
            &mut state.doc,
            "c_general",
            crate::doc::NewMessage {
                author: stranger.into(),
                text: "Retained foreign message".into(),
                ..crate::doc::NewMessage::default()
            },
        );
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
        assert_eq!(app.user_name(&app.me.id), "You");
        assert_eq!(
            ChatDoc::list_users(&state.doc)[0].name,
            "You",
            "presentation must not rewrite saved person data"
        );
    }

    #[test]
    fn people_on_screen_are_named_honestly_and_you_are_you() {
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
            app.me.name = "Quinten".to_string();
            app.users.clear();
            app.room_names.clear();
            assert_eq!(
                app.display_name(&app.me.id.clone()),
                "Quinten",
                "the name this process worked out for itself must be reachable",
            );
        }

        // Phase: the_person_reading_the_screen_is_you.
        {
            let app = ChatApp::new_ephemeral();
            let me = app.me.id.clone();
            assert_eq!(app.user_name(&me), "You");
        }
    }
}

#[cfg(test)]
mod live_draft_presence {
    use super::*;
    use ui::PresencePayload;
    use ui::permissions::UserId;
    use ui::presence::SelectionRange;

    fn focused_app(text: &str) -> ChatApp {
        let mut app = ChatApp::new_ephemeral();
        app.active_channel = "c1a2b3c4-0000-4000-8000-00000000c0de".into();
        app.rect = Rect::new(0.0, 0.0, 1100.0, 720.0);
        let mut doc = AutoCommit::new();
        ChatDoc::set_draft(&mut doc, &app.active_channel, text);
        app.composer_main.set_key(&app.active_channel);
        app.composer_main.sync(&doc);
        Widget::set_focus(&mut app, true);
        app
    }

    fn composer_paths(app: &mut ChatApp, presence: &PresenceCtx) -> usize {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let mut hits = Vec::new();
        app.draw_composer(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            presence,
            &mut hits,
        );
        scene.encoding().path_tags.len()
    }

    /// The composer publishes its caret/selection in the shared draft's scope,
    /// and only while the person is typing there.
    #[test]
    fn focused_composer_publishes_caret_and_selection_in_the_draft_scope() {
        let mut app = focused_app("hello team");
        let key = ChatApp::draft_presence_key(&app.active_channel);
        app.composer_main.select(5, 5);
        assert_eq!(
            ui::presence::capability::presence_payload(&app),
            Some((key.clone(), PresencePayload::Caret { pos: 5 }))
        );
        app.composer_main.select(0, 5);
        assert_eq!(
            ui::presence::capability::presence_payload(&app),
            Some((key, PresencePayload::Selection { start: 0, end: 5 }))
        );
        Widget::set_focus(&mut app, false);
        assert_eq!(ui::presence::capability::presence_payload(&app), None);
    }

    /// A collaborator's caret and selection in the same draft are drawn in
    /// the composer; one in another channel's draft is not.
    #[test]
    fn remote_caret_and_selection_are_drawn_in_the_shared_draft() {
        let mut app = focused_app("hello team");
        let empty = composer_paths(&mut app, &PresenceCtx::empty());
        let key = ChatApp::draft_presence_key(&app.active_channel);
        let mut presence = PresenceCtx::empty();
        presence.set_display_name(UserId("ada".into()), "Ada");
        presence.set_caret(&key, UserId("ada".into()), 4);
        let caret = composer_paths(&mut app, &presence);
        assert!(caret > empty, "remote caret drawn ({empty} -> {caret})");
        presence.set_selection(
            &key,
            UserId("ada".into()),
            SelectionRange { start: 0, end: 4 },
        );
        assert!(
            composer_paths(&mut app, &presence) > caret,
            "remote selection drawn"
        );
        let mut elsewhere = PresenceCtx::empty();
        elsewhere.set_caret(
            &ChatApp::draft_presence_key("other"),
            UserId("ada".into()),
            4,
        );
        assert_eq!(composer_paths(&mut app, &elsewhere), empty);
        // An offset past the local draft (not converged yet) is clamped.
        let mut ahead = PresenceCtx::empty();
        ahead.set_caret(&key, UserId("ada".into()), 400);
        assert!(composer_paths(&mut app, &ahead) > empty);
    }
}
