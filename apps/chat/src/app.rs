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

use std::collections::{HashMap, HashSet};
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
use crate::media::MediaEngine;
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
    #[allow(
        dead_code,
        reason = "legacy CRDT scope selection remains testable for wire compatibility; production navigation uses Inbox and CurrentContext"
    )]
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
    pub author: Id,
}

impl RowSwipe {
    pub fn dx(&self) -> f64 {
        (self.origin.x - self.current.x).max(0.0)
    }
}

/// Leftward travel (px) past which a row swipe commits its delete action.
pub(crate) const SWIPE_COMMIT_DX: f64 = 88.0;

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
    /// Open the platform search over Chat.
    Search,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    /// Share-screen control. The native voice path refuses it (calls are
    /// microphone audio only); the shipped call chrome does not expose it
    /// until production has a real screen-capture producer.
    #[allow(
        dead_code,
        reason = "kept for defensive refusal handling and the voice-profile refusal test"
    )]
    CallShareScreen,
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
            Self::CallShareScreen => "chat.call-share-screen".into(),
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
                    ui::clock::Duration::ZERO
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
    /// Last resource tree with the exact inputs it was built from (committed
    /// heads and resource context). The framework rediscovers resources on
    /// every relayout, including frames that only moved the pointer.
    pub(crate) resource_memo: std::cell::RefCell<Option<(String, Vec<tilo_actions::Resource>)>>,
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
    pub(crate) room_colors: HashMap<Id, (u8, u8, u8)>,

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

    // Ids this session authored under before the room told it which
    // participant it is. See `adopt_own_participant`: what was already said
    // under one of these is still this person, so their own earlier lines go
    // on reading "You" rather than turning into a stranger mid-scroll.
    pub(crate) former_ids: Vec<Id>,

    // Engines (call sounds + media transport).
    pub(crate) media: MediaEngine,

    // The OS permission gate: routes channel participation + file-embed
    // hydration through the toolkit's permission model. Refreshed from the
    // `PermissionContext` the compositor passes into `draw`.
    pub(crate) perms: ChatPermissions,

    // Framework-owned action dispatch: every doc-mutating affordance (Send, a
    // reaction, the channel/message context menus, the Ctrl+K palette) routes
    // through this one field. It brackets the undo boundary, encodes under
    // `CHAT_ACTION_TYPE`, and applies through the executor so a peer replays the
    // byte-identical change. See [`crate::actions`].
    pub(crate) actions: crate::resource::ActionSink,

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
    /// `--shots` only: seed the real minimal starter instead of the demo, so
    /// the honest first-run empty state can be captured.
    shot_starter: bool,
    /// `--shots` only: a draft to stage into the main composer so a screenshot
    /// can show the composer mid-compose (caret + wrapped text). `None` in every
    /// live/interactive path, so production is unaffected.
    shot_composer_draft: Option<String>,
    /// True for the live binary (`ChatApp::new`). A live instance always seeds
    /// the honest empty starter and never the fabricated demo workspace — even
    /// when nothing is persisting behind it. Only the ephemeral `--shots`/test
    /// instances (`new_ephemeral`) leave this `false` and may stage the rich
    /// demo.
    pub(crate) is_live: bool,

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
    pub(crate) canonical: bool,
    canonical_dirty: bool,
    /// Document heads and pending-operation count at the last canonical reload,
    /// so an unchanged document is not reloaded and repainted every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    host_focused: bool,
    pub(crate) external_services: bool,
}

impl Default for ChatApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Native external services are explicit; browser peer transport is installed later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRuntime {
    Native,
    Unavailable,
}

/// Whether this host offers Chat's native services: attachments imported
/// from native paths into the on-disk content store, and the shell's voice
/// service. A browser page has neither.
pub(crate) fn native_services() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

impl ChatApp {
    /// Project a checked Chat record for `identity`.
    ///
    /// # Errors
    ///
    /// Returns an error when the record carries no Chat workspace schema.
    pub fn from_document(
        doc: &AutoCommit,
        identity: LocalIdentity,
        runtime: ChatRuntime,
    ) -> Result<Self, String> {
        if !ChatDoc::is_seeded(doc) {
            return Err("Chat record has no workspace schema".into());
        }
        // A native component keeps attachments in the app's content store;
        // any other host (the browser, tests) keeps them in memory.
        let content = if runtime == ChatRuntime::Native {
            ContentStore::for_app()
        } else {
            ContentStore::memory()
        };
        let mut app = Self::bare(content);
        app.canonical = true;
        app.external_services = runtime == ChatRuntime::Native;
        app.me = identity;
        app.active_ws.clear();
        app.context_ws.clear();
        app.active_channel.clear();
        app.thread_open = None;
        app.media_error = (!app.external_services).then(|| {
            "Calls and screen sharing are unsupported until a media transport is connected".into()
        });
        app.reload(doc);
        if app.ensure_selection() {
            app.reload(doc);
        }
        Ok(app)
    }
    /// The live instance: resolves this peer's real identity. The document
    /// itself is loaded and written by `app-state` (see `main`), so nothing
    /// here touches disk. The resolved identity is what makes two running
    /// instances genuinely different participants rather than two "`u_me"s`
    /// colliding.
    pub fn new() -> Self {
        let content = if native_services() {
            ContentStore::for_app()
        } else {
            ContentStore::memory()
        };
        let mut app = Self::bare(content);
        app.is_live = true;
        app.me = LocalIdentity::resolve();
        // The admitted shell channel owns native voice transport and devices.
        // Author the local view state under the real identity from the start so
        // nothing is attributed to the demo `u_me`.
        app.active_ws.clear();
        app.context_ws.clear();
        app.active_channel.clear();
        app.thread_open = None;
        app
    }

    /// A non-persistent instance for `--shots` / tests: never reads or
    /// writes disk, so a screenshot run can't clobber a real workspace.
    /// Attachment bytes are kept in memory for the same reason.
    pub fn new_ephemeral() -> Self {
        Self::bare(ContentStore::memory())
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
            resource_memo: std::cell::RefCell::new(None),
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
            timeline_at_end: true,
            presented: false,
            read_on_present: false,
            timeline_total_height: 0.0,
            scroll_content: 0.0,
            scroll_viewport: 0.0,
            thread_open: Some("m_concept".into()),
            revealed_message: None,
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
            room_colors: HashMap::new(),
            agent_authors: HashMap::new(),
            body_views: std::cell::RefCell::new(HashMap::new()),
            body_text_inset: std::cell::Cell::new(0.0),
            mention_paint_cache: std::cell::RefCell::new(None),
            participant_labels: HashMap::new(),
            former_ids: Vec::new(),
            // Default to the deterministic local bus; the live binary
            // (`ChatApp::new`) upgrades to the real transport. This keeps
            // `--shots`/tests from binding a real iroh endpoint per instance.
            media: MediaEngine::simulated(),
            perms: ChatPermissions::permissive(),
            actions: crate::resource::ActionSink::default(),
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
            shot_starter: false,
            shot_composer_draft: None,
            is_live: false,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            media_error: None,
            canonical: false,
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
            _ => self.external_services && self.perms.allows(action),
        }
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

    /// Dispatch one [`ChatAction`] — the single CRDT-mutation path shared by the
    /// on-screen affordances and the Ctrl+K palette. Delegates to the framework's
    /// [`ActionHost`](ui::ActionHost), which brackets the undo boundary and applies
    /// the action through the executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// in-memory projection is reconciled from the document by the caller's
    /// `reload`. This app supplies only its domain: the action and its type.
    pub(crate) fn perform_action(&mut self, doc: &mut AutoCommit, action: &ChatAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::CHAT_ACTION_TYPE,
            action,
            actions::history_context(action),
        );
    }

    pub(crate) fn ensure_composer_draft(&mut self, doc: &mut AutoCommit, key: &str) {
        if key.is_empty() || ChatDoc::rich_draft_is_settled(doc, key) {
            return;
        }
        self.perform_action(
            doc,
            &ChatAction::EnsureRichDraft {
                key: key.to_string(),
            },
        );
    }

    /// The last [`EncodedAction`](tilo_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the affordance == [`ActionSpec`](tilo_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical action.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the rooms and channels the framework resource already holds.
    /// An empty projection does not wipe unpublished local rooms or messages.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ChatState) {
        if !state.workspaces.is_empty() {
            self.workspaces.clone_from(&state.workspaces);
        }
        if !state.channels.is_empty() {
            self.channels.clone_from(&state.channels);
        }
        // Messages stay on the live sync path — never clear them from an
        // empty resource projection that has not caught up yet.
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Chat>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// The context Ctrl+K needs to build Chat's resource tree: the active
    /// workspace, its channels (including archived, so they can be unarchived from
    /// the palette), the open channel, and that channel's messages. Derived
    /// entirely from the projection the document already holds.
    /// [`Self::chat_resource_ctx`] as agent task `participant`: authored by
    /// it, with it in the room under its label as an agent. `None` for any
    /// participant that is not an agent task (its own view is the default).
    pub(crate) fn participant_ctx(
        &self,
        participant: &tilo_actions::ParticipantId,
    ) -> Option<actions::ChatResourceCtx> {
        participant.agent_task()?;
        let mut ctx = self.chat_resource_ctx();
        Self::author_as_agent(
            &mut ctx,
            participant.as_str(),
            self.agent_label(participant.as_str()),
        );
        Some(ctx)
    }

    /// The display name the host gave agent task participant `id`.
    fn agent_label(&self, id: &str) -> String {
        self.participant_labels
            .get(id)
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| "Agent".to_owned())
    }

    /// `ctx` authored by agent participant `id`, in the room as an agent.
    fn author_as_agent(ctx: &mut actions::ChatResourceCtx, id: &str, label: String) {
        ctx.participants.retain(|p| p.id != id);
        ctx.participants.insert(
            0,
            crate::mention::Participant {
                id: id.to_owned(),
                name: label,
                is_agent: true,
            },
        );
        id.clone_into(&mut ctx.me);
    }

    pub(crate) fn chat_resource_ctx(&self) -> actions::ChatResourceCtx {
        let workspace_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let mut channels: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.workspace == self.active_ws)
            .collect();
        channels.sort_by_key(|c| c.order);
        let channels = channels
            .into_iter()
            .map(|c| actions::ChannelResource {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: c.kind,
                archived: c.archived,
            })
            .collect();
        let messages =
            self.messages
                .iter()
                .map(|m| actions::MessageResource {
                    id: m.id.clone(),
                    author: m.author.clone(),
                    label: self.message_resource_label(m),
                    pinned: m.pinned,
                    parent: m.parent.clone(),
                    attachment: m.attachment.as_ref().map(|attachment| {
                        actions::AttachmentResource {
                            name: attachment.name().to_owned(),
                            blob: attachment.blob().to_owned(),
                        }
                    }),
                    reactions: m
                        .reactions
                        .iter()
                        .map(|reaction| actions::ReactionResource {
                            emoji: reaction.emoji.clone(),
                            count: reaction.count(),
                            reacted_by_me: reaction.reacted_by(&self.me.id),
                        })
                        .collect(),
                })
                .collect();
        let mut ctx = actions::ChatResourceCtx {
            me: self.me.id.clone(),
            workspace_id: self.active_ws.clone(),
            workspace_name,
            channels,
            active_channel: self.active_channel.clone(),
            messages,
            participants: self.participants(),
        };
        // A host acting as an admitted agent task (a browser mount with an
        // agent author) authors its own tree as that agent.
        if self
            .me
            .id
            .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
            && self.participant_labels.contains_key(&self.me.id)
        {
            let me = self.me.id.clone();
            Self::author_as_agent(&mut ctx, &me, self.agent_label(&me));
        }
        ctx
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
            actions::remove_reaction_action(&self.active_channel, msg, emoji, &self.me.id)
        } else {
            actions::add_reaction_action(&self.active_channel, msg, emoji, &self.me.id)
        }
    }

    /// A palette label for a message resource: the author's name and a short
    /// snippet of the body (or the attachment name for a media-only post).
    fn message_resource_label(&self, m: &Message) -> String {
        let author = self.user_name(&m.author);
        let snippet: String = if m.text.trim().is_empty() {
            m.attachment
                .as_ref()
                .map_or_else(|| "(no text)".to_string(), |a| a.name().to_string())
        } else {
            m.text.trim().chars().take(40).collect()
        };
        format!("{author}: {snippet}")
    }

    /// Stage a named scenario for a headless screenshot against the ephemeral
    /// demo workspace. The default is an ordinary reading/conversation frame;
    /// collaboration states are explicit so a capture does not teach a floating
    /// call as Chat's resting state.
    #[allow(
        clippy::too_many_lines,
        reason = "the screenshot scenario dispatcher keeps each named visual contract explicit and adjacent"
    )]
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
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
            }
            "starter" => {
                // The real first-run empty workspace (no demo content).
                self.shot_starter = true;
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
                self.seed_shot_scenario("attach");
                // This mirrors the actual asynchronous picker state rather
                // than a hand-painted status string: no entries are usable
                // until the worker's generation-checked reply settles.
                self.attach_entries.clear();
                self.attachment_picker_pending = Some(0);
                self.attachment_status = Some("Loading attachments…".into());
            }
            "attach-error" => {
                self.seed_shot_scenario("attach");
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
                self.shot_composer_draft = Some(
                    "Agenda for Thursday's review:\nWalk through the call grid first, \
                     then the screen-share stage."
                        .to_string()
                        + "\nCan someone send me teh adress for the offsite?",
                );
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

    /// Headless visual-review hook for the real shell → app call handoff.
    ///
    /// This runs after layout so the same `FloatingPlacement` methods used by
    /// pointer input and the `Suspended` host reply see authoritative app and
    /// desktop coordinate spaces. The `returning` and `returned` frames land
    /// on identical geometry on opposite sides of the ownership boundary.
    /// Stage what a scenario needs once the window is laid out: the floating
    /// call's ownership edge, or the navigation drawer floating open over a
    /// narrow window (a choice the drawer records per size class).
    #[doc(hidden)]
    pub fn stage_after_layout_for_shot(&mut self, scenario: &str) {
        // A channel's menu opens on its row, so the channel list is out
        // (floating over a narrow window).
        if scenario == "sidebar-narrow" || scenario == "context" {
            self.reconcile_flaps();
            self.left_flap.snap(true);
            self.reconcile_flaps();
        }
        if scenario == "thread-narrow" {
            self.reconcile_flaps();
            self.right_flap.snap(true);
            self.reconcile_flaps();
        }
        self.debug_stage_floating_call_for_shot(scenario);
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

    /// Whether `scenario` stages something on the rows a first frame placed
    /// ([`Self::stage_after_draw_for_shot`]).
    #[doc(hidden)]
    pub fn stages_after_draw(&self, scenario: &str) -> bool {
        matches!(scenario, "context" | "msgmenu")
    }

    /// Open a scenario's context menu where a right-click on its row would:
    /// at the pointer, on the row the menu acts on, read from the frame that
    /// was just drawn.
    #[doc(hidden)]
    pub fn stage_after_draw_for_shot(&mut self, scenario: &str) {
        let (target, items) = match scenario {
            "context" => (
                MenuTarget::Channel("c_frontend".into()),
                MenuAction::for_channel().to_vec(),
            ),
            "msgmenu" => (
                MenuTarget::Message("m_me2".into()),
                MenuAction::for_message().to_vec(),
            ),
            _ => return,
        };
        let row = self
            .hits
            .iter()
            .find_map(|(rect, hit)| match (hit, &target) {
                (Hit::Channel(id), MenuTarget::Channel(want)) if id == want => Some(*rect),
                (Hit::MessageMenu { msg }, MenuTarget::Message(want)) if msg == want => Some(*rect),
                _ => None,
            });
        let Some(row) = row else {
            return;
        };
        // The pointer rests on the row's text, a third of the way in.
        let pointer = Point::new(row.x0 + row.width() / 3.0, row.center().y);
        self.menu = Some(self.context_menu(target, pointer, items));
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
    pub fn debug_stage_floating_call_for_shot(&mut self, scenario: &str) {
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

    /// Whether this peer is currently sharing its screen — the shared-truth
    /// flag used by the featured tile and the call-control tests. Chat has no
    /// local screen-capture producer, so production never sets it.
    #[allow(
        dead_code,
        reason = "queried by call-control tests while the production capture control is hidden"
    )]
    pub(crate) fn sharing_screen(&self) -> bool {
        self.call.sharing
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
    /// A chat author has to live in the id space the roster is keyed by,
    /// because resolving an author's name *is* a lookup in that roster.
    /// [`LocalIdentity::resolve`](crate::identity::LocalIdentity::resolve)
    /// mints an id from the signing key, while every other machine addresses
    /// this participant however the broker's roster spells it — the only
    /// spelling that appears in a roster. Authoring under the local mint
    /// alone means every remote message resolves to nobody and renders
    /// unnamed, permanently, on every screen but the sender's.
    ///
    /// A session with no room has neither an id nor a name to adopt, and
    /// keeps what it resolved for itself: there is no room-wide address to
    /// be had, and nobody else is looking.
    fn adopt_own_participant(&mut self, presence: &PresenceCtx) {
        if let Some(room_id) = presence.self_id()
            && room_id.0 != self.me.id
        {
            let former = std::mem::replace(&mut self.me.id, room_id.0.clone());
            // The colour is derived from the id, so it moves with it —
            // otherwise this peer's caret and its avatar disagree.
            self.me.color_idx = crate::identity::color_index_for(room_id.0.as_bytes());
            // What was already said under the old id is still this person: a
            // message records who spoke, and rewriting authorship after the
            // fact to tidy an id would be editing history. Remembering the id
            // instead keeps "You" beside their own earlier lines.
            if !self.former_ids.contains(&former) {
                self.former_ids.push(former);
            }
        }
        if let Some(name) = presence.self_name()
            && name != self.me.name
        {
            self.me.name = name.to_string();
            self.me.initials = crate::identity::initials_for(name);
        }
    }

    /// Whether `id` is this session's own participant, including any id it
    /// authored under before the room named it.
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
            return n.clone();
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return agent.clone();
        }
        if let Some(u) = self.users.iter().find(|u| u.id == id)
            && !u.name.trim().is_empty()
        {
            return u.name.clone();
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
            return named;
        }
        format!("Someone ({})", Self::unnamed_tag(id))
    }

    /// Enough of an unresolved id to tell two unnamed people apart.
    ///
    /// The tail rather than the head: these ids share a `peer@` prefix and
    /// differ in their digits, so the first characters are the ones that
    /// carry no information.
    fn unnamed_tag(id: &str) -> String {
        let body = id.rsplit(['@', ':']).next().unwrap_or(id);
        let tail: String = body
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tail.is_empty() {
            id.to_string()
        } else {
            tail
        }
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
        let editing = self.editing.is_some() || self.rename.is_some() || !self.host_focused;
        let main = matches!(self.focus, Focus::Main) && !editing;
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
        if self
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
        // Durable voice rows are legacy history. Current stream inventory is
        // projected only from the authenticated local service response.
        self.refresh_native_streams();
        self.users = self.workspace_people_from(doc, &per_channel);
        // Point each composer at its surface's draft; the editors read it on
        // their next sync (`sync_composers`), keeping the caret while typing.
        self.composer_main.set_key(&self.active_channel);
        self.draft_main = ChatDoc::draft_text(doc, &self.active_channel);
        let ch_name = self
            .channel(&self.active_channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.composer_main
            .set_placeholder(&format!("Message #{ch_name}"));
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
        self.set_composer_focus();
    }

    /// Bring both composers up to date with their drafts. `--shots` stages
    /// its mid-compose draft here.
    pub(crate) fn sync_composers(&mut self, doc: &mut AutoCommit) {
        if let Some(draft) = self.shot_composer_draft.take() {
            ChatDoc::set_draft(doc, &self.active_channel, &draft);
        }
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

    /// Advance the local read watermark for the channel currently in view so
    /// its unread badge stays cleared and doesn't reappear once the user
    /// navigates away. Idempotent (a no-op when nothing new arrived), so it is
    /// safe to call every frame.
    pub(crate) fn mark_active_read(&mut self, doc: &mut AutoCommit) {
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
        let action = actions::mark_read_action(&self.active_channel, &self.me.id, count);
        self.perform_action(doc, &action);
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|channel| channel.id == self.active_channel)
        {
            channel.unread = 0;
        }
    }

    fn reveal_workspace_resource(&mut self, workspace: String) -> Result<(), String> {
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

    fn reveal_category_resource(
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
    // Two drawers frame the conversation: the navigation drawer (workspace
    // rail beside the channel list) at the leading edge, and the detail
    // drawer (people, or the open thread) at the trailing edge. The toolkit's
    // `SidebarDrawer` decides, from the window width, whether each docks
    // beside the conversation or floats over it behind a scrim, eases the
    // reveal and remembers the person's show/hide choice per size class.

    /// The leading navigation drawer: the workspace rail beside the channel
    /// list, at the shell's sidebar and rail metrics. It docks only while the
    /// conversation beside it keeps the content minimum for its text after
    /// the avatar column; below that it becomes a floating drawer.
    fn navigation_drawer() -> SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        let s = ui::tokens::Spacing::BASE;
        let docked = m.rail_width + m.sidebar_width;
        let avatar_column = s.s6 + m.min_pointer_target + s.s3;
        SidebarDrawer::new()
            .docked_width(f64::from(docked))
            .overlay_width(f64::from(m.rail_width + m.drawer_width))
            .breakpoint(f64::from(docked + avatar_column + m.content_min_width))
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
        let nav = self.nav_rect();
        let y0 = (nav.y0 + self.chrome.header_h).min(nav.y1);
        Rect::new(
            nav.x0,
            y0,
            (nav.x0 + self.chrome.rail_w).min(nav.x1),
            nav.y1,
        )
    }
    /// The channel list beside the rail, under the header row.
    pub(crate) fn sidebar_rect(&self) -> Rect {
        let nav = self.nav_rect();
        let y0 = (nav.y0 + self.chrome.header_h).min(nav.y1);
        Rect::new(
            (nav.x0 + self.chrome.rail_w).min(nav.x1),
            y0,
            nav.x1,
            nav.y1,
        )
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
    /// The composer field's height for a draft `text` wrapped at `width`:
    /// one line at rest, growing with the draft up to the cap.
    fn composer_field_height(&self, tr: &mut TextRenderer, text: &str, width: f64) -> f64 {
        let chrome = &self.chrome;
        let wrap = width.max(40.0) as f32;
        let probe = if text.is_empty() { "Ag" } else { text };
        let (_, h) = tr.measure_style(probe, Some(wrap), &chrome.body);
        let pad = chrome.composer_min_h - chrome.body_line;
        (f64::from(h) + pad).clamp(chrome.composer_min_h, chrome.composer_max_h)
    }
    /// The width the composer's draft wraps at inside a field `field_w`
    /// wide: the field less its buttons (attach, emoji, send).
    fn composer_text_width(&self, field_w: f64) -> f64 {
        field_w - 4.0 * self.chrome.composer_min_h
    }
    pub(crate) fn composer_height(&self, tr: &mut TextRenderer) -> f64 {
        let field_w = self.center_right() - self.center_left() - 2.0 * self.chrome.gutter;
        let text_w = self.composer_text_width(field_w);
        self.composer_field_height(tr, &self.draft_main, text_w)
    }
    pub(crate) fn composer_rect(&self, tr: &mut TextRenderer) -> Rect {
        let h = self.composer_height(tr);
        let pad = self.chrome.composer_pad;
        Rect::new(
            self.center_left() + self.chrome.gutter,
            self.rect.y1 - h - pad,
            self.center_right() - self.chrome.gutter,
            self.rect.y1 - pad,
        )
    }

    /// The thread's composer: the same control and height rule as the main
    /// composer, at the bottom of the thread panel `panel`.
    pub(crate) fn thread_composer_rect(&self, tr: &mut TextRenderer, panel: Rect) -> Rect {
        let pad = self.chrome.composer_pad;
        let field_w = panel.width() - 2.0 * pad;
        let text_w = self.composer_text_width(field_w);
        let h = self.composer_field_height(tr, &self.composer_thread.value(), text_w);
        Rect::new(
            panel.x0 + pad,
            panel.y1 - h - pad,
            panel.x1 - pad,
            panel.y1 - pad,
        )
    }
    /// The timeline: between the header row and the composer.
    pub(crate) fn messages_rect(&self, tr: &mut TextRenderer) -> Rect {
        let composer = self.composer_rect(tr);
        Rect::new(
            self.center_left(),
            self.rect.y0 + self.chrome.header_h,
            self.center_right(),
            composer.y0 - self.chrome.avatar_gap,
        )
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

    /// Synchronously warm the attachment-image cache for the current channel,
    /// blocking briefly so a single offscreen `--shots` frame paints the real
    /// decoded images. Never call this on the live UI thread — there the store
    /// loads in the background and surfaces on a later frame.
    pub fn prewarm_images(&self) {
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
        self.images
            .block_until_idle(ui::clock::Duration::from_secs(3));
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
                    Hit::FocusMain => ("Message composer".into(), Role::MultilineTextInput, false),
                    Hit::FocusThread => (
                        "Thread reply composer".into(),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::Send => ("Send message".into(), Role::Button, false),
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
                    Hit::Search => ("Search Chat".into(), Role::Button, false),
                    Hit::ToggleLeftFlap => {
                        ("Show conversation navigation".into(), Role::Button, false)
                    }
                    Hit::ToggleRightFlap => ("Show people or thread".into(), Role::Button, false),
                    Hit::OpenThread(_) => ("Open message thread".into(), Role::Button, false),
                    Hit::CloseThread => ("Close thread".into(), Role::Button, false),
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
                    Hit::CallShareScreen => {
                        ("Share screen".into(), Role::ToggleButton, self.call.sharing)
                    }
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

impl ChatApp {
    /// Perform one of the operations Chat's resources export; `None` for an
    /// operation that is not Chat's.
    ///
    /// # Errors
    ///
    /// Refuses an operation of the wrong kind, for a conversation or message
    /// that no longer exists, or that its local service rejects.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive operation dispatcher preserves the resource contract at a single boundary"
    )]
    pub(crate) fn perform_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        match &operation {
            crate::actions::ChatOperation::NativeVoice { request, .. } => {
                if kind != tilo_actions::OperationKind::Effect
                    || !request.valid()
                    || request.intent != 0
                {
                    return Err("Call controls require a valid local Effect".into());
                }
            }
            crate::actions::ChatOperation::ImportAttachment { .. } => {
                if !self.external_services {
                    return Err("attachment import unsupported: no host asset transport".into());
                }
                if kind != tilo_actions::OperationKind::Effect {
                    return Err("attachment import requires an Effect operation".into());
                }
            }
            _ if !chat_navigation_kind(kind)? => return Ok(None),
            _ => {}
        }
        match operation {
            crate::actions::ChatOperation::NativeVoice { channel, request } => {
                self.native_voice_command(&channel, request.command)?;
            }
            crate::actions::ChatOperation::OpenWorkspace { workspace } => {
                self.reveal_workspace_resource(workspace)?;
            }
            crate::actions::ChatOperation::OpenChannel { workspace, channel } => {
                if !self
                    .channels
                    .iter()
                    .any(|item| item.id == channel && item.workspace == workspace)
                {
                    return Err("conversation no longer exists".into());
                }
                self.active_ws = workspace;
                self.active_channel = channel;
                self.inbox_open = false;
                self.thread_open = None;
                self.revealed_message = None;
                self.reveal_scroll_pending = false;
                self.revealed_person = None;
                self.reset_timeline_to_latest();
                self.focus = Focus::Main;
            }
            crate::actions::ChatOperation::OpenCategory {
                workspace,
                category,
            } => self.reveal_category_resource(workspace, &category)?,
            crate::actions::ChatOperation::RevealMessage {
                workspace,
                channel,
                message,
                parent,
            } => {
                if !self
                    .channels
                    .iter()
                    .any(|item| item.id == channel && item.workspace == workspace)
                {
                    return Err("conversation no longer exists".into());
                }
                if !ChatDoc::list_messages(doc, &channel)
                    .iter()
                    .any(|item| item.id == message)
                {
                    return Err("message no longer exists".into());
                }
                self.active_ws = workspace;
                self.active_channel = channel;
                self.inbox_open = false;
                self.thread_open = parent;
                self.revealed_message = Some(message);
                self.reveal_scroll_pending = true;
                self.revealed_person = None;
                self.timeline_total_height = 0.0;
                self.timeline_at_end = false;
                self.focus = if self.thread_open.is_some() {
                    Focus::Thread
                } else {
                    Focus::Main
                };
                if self.thread_open.is_some() {
                    self.right_flap.open();
                }
            }
            crate::actions::ChatOperation::OpenPerson { user } => {
                if !ChatDoc::list_users(doc).iter().any(|item| item.id == user) {
                    return Err("person no longer exists".into());
                }
                // Prefer the current workspace when it already evidences this
                // person; otherwise open the first workspace where they wrote.
                if !self.users.iter().any(|item| item.id == user)
                    && let Some(channel) = self.channels.iter().find(|channel| {
                        ChatDoc::list_messages(doc, &channel.id)
                            .iter()
                            .any(|message| {
                                message.author == user
                                    || message.mentions.iter().any(|mention| mention.user == user)
                                    || message
                                        .reactions
                                        .iter()
                                        .any(|reaction| reaction.users.iter().any(|id| id == &user))
                            })
                    })
                {
                    self.active_ws = channel.workspace.clone();
                    self.active_channel = channel.id.clone();
                }
                self.inbox_open = false;
                self.thread_open = None;
                self.revealed_message = None;
                self.revealed_person = Some(user);
                self.focus = Focus::Main;
                self.right_flap.open();
            }
            crate::actions::ChatOperation::ImportAttachment { channel, path } => {
                if path.is_empty() {
                    return Err("attachment path is empty".into());
                }
                if !self.channels.iter().any(|item| item.id == channel) {
                    return Err("conversation no longer exists".into());
                }
                if !self.begin_attachment_import_into(&channel, std::path::Path::new(&path)) {
                    return Err("attachment import was not accepted".into());
                }
            }
            crate::actions::ChatOperation::ReadMessage { message } => {
                return self.read_message(doc, &message).map(|value| {
                    Some(tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    })
                });
            }
        }
        Ok(Some(tilo_actions::ActionOutput::default()))
    }

    /// One message as a value: who wrote it, whether that is an agent, its
    /// whole text and the agent's record of its work.
    fn read_message(&self, doc: &AutoCommit, id: &str) -> Result<serde_json::Value, String> {
        let message = self
            .channels
            .iter()
            .find_map(|channel| {
                ChatDoc::list_messages(doc, &channel.id)
                    .into_iter()
                    .find(|message| message.id == id)
            })
            .ok_or("message no longer exists")?;
        // From the document, not the last projection: a reply this mount
        // just posted is already the agent's.
        let agent_name = ChatDoc::agent_authors(doc).remove(&message.author);
        let agent = agent_name.is_some()
            || self
                .users
                .iter()
                .any(|user| user.id == message.author && user.is_agent);
        Ok(serde_json::json!({
            "id": message.id,
            "channel": message.channel,
            "parent": message.parent,
            "author": message.author,
            "authorName": agent_name.unwrap_or_else(|| self.display_name(&message.author)),
            "agent": agent,
            "mine": self.is_me(&message.author),
            "ts": message.ts,
            "edited": message.edited,
            "text": message.text,
            "work": message.work,
        }))
    }
}

fn chat_navigation_kind(kind: tilo_actions::OperationKind) -> Result<bool, String> {
    match kind {
        tilo_actions::OperationKind::Document => Ok(false),
        tilo_actions::OperationKind::View => Ok(true),
        tilo_actions::OperationKind::Effect => {
            Err("Chat navigation requires a View operation".into())
        }
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
        state: &mut AutoCommit,
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
        let before = self.native_voice.snapshot.state;
        requests.extend(self.native_voice.take_requests(ui::time::now()));
        if before != self.native_voice.snapshot.state {
            self.project_native_voice();
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        match event {
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
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.pending_floating.take()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
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

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        let returning_to_app = self.call.placement.is_return_pending();
        if !self.call.active
            || self.call.expanded
            || (!returning_to_app
                && !matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::Desktop { .. }
                ))
        {
            return Vec::new();
        }
        vec![ui::floating_surface::FloatingSurfacePresentation {
            id: format!("chat-call:{}", self.call.channel),
            content_rect: self.call_video_rect(),
            window_rect: self.huddle_panel().inflate(8.0, 8.0),
            returning_to_app,
        }]
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.call
            .active
            .then(|| format!("chat-call:{}", self.call.channel))
            .into_iter()
            .collect()
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.floating_surface_bounds = bounds;
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
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
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::collaboration::participants::Participants,
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::widget::controls::ActionControls
    );

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
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
        self.scroll = region.metrics.max_scroll() - next;
        self.timeline_at_end = self.scroll <= 0.5;
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
        self.canonical_dirty = true;
        self.set_composer_focus();
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.host_focused
            .then_some(winit::window::ImePurpose::Normal)
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
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

    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document(doc, path);
        self.apply_realtime_stop(doc);
        self.establish_realtime_target();
        if !self.canonical && self.is_live {
            let status = model::Presence::Online;
            // A brand-new live record still needs the honest first workspace.
            // SeedStarter owns that write; EnsureUser only joins the roster.
            if ChatDoc::list_workspaces(doc).is_empty() {
                self.perform_action(
                    doc,
                    &actions::ChatAction::SeedStarter {
                        id: self.me.id.clone(),
                        name: self.me.name.clone(),
                        initials: self.me.initials.clone(),
                        color_idx: self.me.color_idx,
                    },
                );
            } else if !ChatDoc::user_is_current(
                doc,
                &self.me.id,
                &self.me.name,
                &self.me.initials,
                self.me.color_idx,
                status,
            ) {
                self.perform_action(
                    doc,
                    &actions::ChatAction::EnsureUser {
                        id: self.me.id.clone(),
                        name: self.me.name.clone(),
                        initials: self.me.initials.clone(),
                        color_idx: self.me.color_idx,
                        status: status.as_str().to_string(),
                    },
                );
            }
        } else if !self.canonical && self.shot_starter {
            // `--shots` capture of the honest first-run empty workspace.
            ChatDoc::seed_starter(doc, &self.me.id, &self.me.name, &self.me.initials);
        } else if !self.canonical {
            // Ephemeral `--shots`/test instance: the rich demo workspace.
            ChatDoc::seed_if_empty(doc);
        }
        self.settle_attachment_picker();
        self.settle_attachment_import(doc);
        if self.canonical {
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
        } else {
            self.reload(doc);
        }
        if self.ensure_selection() {
            // Re-picking the workspace changes which channels/messages apply.
            self.canonical_dirty = true;
            self.reload(doc);
        }
        // The channel in view is read: advance its watermark (no-op when
        // nothing new arrived) so its unread badge stays cleared. Only a
        // presented instance has a viewer; a headless host worker never
        // authors read receipts.
        if self.presented {
            self.mark_active_read(doc);
        }
        self.sync_composers(doc);
        // Native capture/transport belongs to the shell's lease-bound service.
        // Durable voice rows never start the legacy Iroh media pump.
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
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
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
        self.draw_header(scene, tf, tr, theme, &mut hits);
        let msg_area = self.messages_rect(tr);
        crate::paint::push_clip(scene, tf, msg_area);
        self.draw_messages(
            ui::paint::PaintCtx::new(scene, tf, tr, theme),
            presence,
            msg_area,
            &mut hits,
        );
        scene.pop_layer();
        self.draw_composer(scene, tf, tr, theme, presence, &mut hits);

        self.draw_drawers(scene, tf, tr, theme, presence, &mut hits);

        // Call layers. The host tile is a real VideoPlayer, painted only once
        // a real capture has fed it a frame; otherwise the honest "camera off"
        // placeholder (drawn from live geometry) covers the host tile.
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
        } else if self.call.active
            // At enlarged text, the labelled Call control remains reachable
            // in the header; a fixed floating huddle would cover the thread
            // and composer instead of forming a usable secondary surface.
            && theme.text_scale <= 1.25
            && ((!self.call.placement.is_return_pending()
                && matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::App { .. }
                ))
                || !self.call.placement.is_desktop_covered())
        {
            self.draw_live_huddle(scene, tf, tr, theme, presence, &mut hits);
        }

        // Top-most popovers.
        if let Some(mut menu) = self.menu.take() {
            menu.widget.draw(scene, tf, tr, theme, presence);
            self.menu = Some(menu);
        }
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Embedded inputs request focus with a temporary layout node. Map
        // that actual request to Chat's retained keyboard/IME owner.
        let was_handled = *ctx.handled;
        let requested = self.dispatch_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            ctx.state,
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

        let conversation = self
            .messages
            .iter()
            .filter(|message| message.parent.is_none())
            .map(|message| {
                let author = self
                    .user(&message.author)
                    .map_or("Unknown person", |user| user.name.as_str());
                AccessNode::new(Role::ListItem)
                    .with_name(format!("{author}: {}", message.text))
                    .with_hashed_key((&message.id, "message"))
            })
            .collect();
        let conversation = AccessNode::new(Role::List)
            .with_name("Messages")
            .with_children(conversation)
            .with_hashed_key("message-list");

        let mut children: Vec<AccessNode> = controls;
        if let Some(channel) = self.channel(&self.active_channel) {
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
        Some(
            AccessNode::new(Role::Group)
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
        self.semantic_hits().get(*index).map(|(rect, ..)| *rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.semantic_hits().len() {
            return false;
        }
        self.keyboard_target = Some(*index);
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("context-menu:item:") {
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
            return menu_handled;
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
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let messages_before = matches!(hit, Hit::Send)
            .then(|| ChatDoc::list_messages(state, &self.active_channel).len());
        self.on_hit(hit, state);
        messages_before.is_none_or(|before| {
            ChatDoc::list_messages(state, &self.active_channel).len() == before.saturating_add(1)
        })
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CHAT_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .perform_operation(request.kind, request.type_id, request.payload, doc)?
            .map(ui::widget::ResourceOperationStart::Completed))
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
    fn update(&mut self, _doc: &mut AppModel, _path: &ModelPath) -> bool {
        // A bound media endpoint wakes the event loop; sync adopts it.
        let media_bound = self.media.poll_binding();
        // Re-run `sync` once after the first layout so the channel already in
        // view acquires its read receipt on open.
        std::mem::take(&mut self.read_on_present) || media_bound
    }

    fn needs_animation_frame(&self) -> bool {
        self.read_on_present
            || self.flap_animating
            || self.call_chrome_motion.animating
            || self.call.placement.is_dragging()
            || self.call.placement.is_handoff_animating()
            || self.tooltips.needs_frame()
            || !self.images.is_idle()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.call.placement.is_dragging()
    }

    fn needs_redraw(&self) -> bool {
        !self.canonical || self.canonical_dirty
    }
    fn mark_drawn(&mut self) {
        self.canonical_dirty = false;
        self.composer_main.mark_drawn();
        self.composer_thread.mark_drawn();
    }
}

impl ui::presence::capability::PublishesPresence for ChatApp {
    fn presence_payload(&self) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        self.composer_presence()
    }
}

impl ui::search::capability::SearchSource for ChatApp {
    /// Fold conversations, full-text messages, and people into global search.
    /// Every result names an exported `View` action; there is no app-private
    /// activation path.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::ChatSearchProvider::new(
            &self.categories,
            &self.channels,
            &self.workspaces,
            &self.search_messages,
            &self.search_users,
        ))]
    }
}

impl ui::resources::PublishesResources for ChatApp {
    /// Standalone / `--shots` keeps the full hand-built door. When hosted,
    /// channel/message Document ops ride [`tilo_resource::publish`]; this
    /// returns only live extras — never a second `chat.workspace` Document door.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        // The framework root already owns the workspace door.
        if self.is_hosted() && id == "chat.workspace" {
            return None;
        }
        self.publish_resource(doc, id)
    }
    /// The tree a delivered invocation resolves against for an agent task
    /// participant: the same resources, authored as that participant (its
    /// posts carry it as author and its display name). When hosted, Document
    /// ops stay on `ResourceRoot`'s publish — do not republish a second chat root.
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        match participant.and_then(|p| self.participant_ctx(p)) {
            Some(ctx) => crate::actions::provide_resources(&ctx, doc),
            None => ui::resources::resources(self, doc),
        }
    }
    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "chat.workspace" {
            return None;
        }
        match participant.and_then(|p| self.participant_ctx(p)) {
            Some(ctx) => crate::actions::resolve_resource(&ctx, doc, id),
            None => ui::resources::resource(self, doc, id),
        }
    }
}

impl ui::undo::CommittedActions for ChatApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing the event —
    /// the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
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
    }

    /// Flag a taffy rebuild (kept for parity; the video rect is now set in
    /// `apply_layout` so most state changes don't need it, but call expand
    /// still requests a clean relayout).
    pub(crate) fn request_relayout(&mut self) {
        self.relayout = true;
    }
}

#[cfg(test)]
mod identity_on_screen {
    use super::*;
    use ui::widget::Widget;

    /// A live peer joins the roster through `EnsureUser`, hosted or standalone.
    #[test]
    fn a_live_peer_joins_through_the_user_action() {
        let mut app = ChatApp::new_ephemeral();
        app.is_live = true;
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
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
        app.composer_main.sync(&mut doc);
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

/// Agent attribution: an agent task's `post` (the same operation a person
/// uses) is authored by the invoking participant and rendered for every member
/// as "<agent name> (agent)"; a person's post is unchanged.
#[cfg(test)]
mod agent_attribution_tests {
    use super::*;
    use automerge::ROOT;
    use tilo_actions::{ActionInput, LocalTransport, ParticipantId};

    fn post_via(resources: &[tilo_actions::Resource], text: &str) -> tilo_actions::EncodedAction {
        fn find<'a>(
            resources: &'a [tilo_actions::Resource],
            id: &str,
        ) -> Option<&'a tilo_actions::Resource> {
            resources.iter().find_map(|r| {
                if r.id.as_str() == id {
                    Some(r)
                } else {
                    find(&r.children, id)
                }
            })
        }
        let general = find(resources, "chat.channel:c_general").expect("general published");
        let post = general.actions.iter().find(|a| a.id == "post").unwrap();
        match &post.input {
            ActionInput::Prompted { build, .. } => build(text),
            _ => panic!("post is prompted"),
        }
    }

    fn dispatch(doc: &mut AutoCommit, encoded: tilo_actions::EncodedAction) {
        let mut registry = tilo_actions::ActionRegistry::new();
        crate::actions::register(&mut registry);
        let decoded = registry.decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    #[test]
    fn agent_posts_are_authored_by_the_invoking_agent_for_every_member() {
        let mut app = ChatApp::new_ephemeral();
        let mut doc = AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut doc);
        app.sync(&mut doc, &ROOT);
        let agent = ParticipantId::for_agent_task("11111111-1111-4111-8111-111111111111").unwrap();
        ui::collaboration::participants::join_participant(&mut app, &agent);
        ui::collaboration::participants::set_participant_label(&mut app, &agent, "Tide");

        let resources = ui::resources::resources_for(&app, Some(&agent), &doc);
        let encoded = post_via(&resources, "hello from the agent");
        let action: crate::actions::ChatAction = serde_json::from_slice(&encoded.payload).unwrap();
        assert!(
            matches!(&action, crate::actions::ChatAction::AgentAuthored { name, .. } if name == "Tide")
        );
        dispatch(&mut doc, encoded);

        // A person's post in the same tree shape is the plain send, as before.
        let own = ui::resources::resources_for(&app, None, &doc);
        let encoded = post_via(&own, "hello from a person");
        let action: crate::actions::ChatAction = serde_json::from_slice(&encoded.payload).unwrap();
        assert!(matches!(
            action,
            crate::actions::ChatAction::SendMessage { .. }
        ));
        dispatch(&mut doc, encoded);

        let messages = crate::doc::ChatDoc::list_messages(&doc, "c_general");
        let by_agent = messages
            .iter()
            .find(|m| m.text == "hello from the agent")
            .unwrap();
        assert_eq!(by_agent.author, agent.as_str());
        let by_person = messages
            .iter()
            .find(|m| m.text == "hello from a person")
            .unwrap();
        assert_eq!(by_person.author, app.me.id);

        // Any member (a fresh peer replaying the document) renders the agent.
        let mut member = ChatApp::new_ephemeral();
        member.sync(&mut doc, &ROOT);
        assert_eq!(member.user_name(agent.as_str()), "Tide (agent)");
    }

    /// The label is applied only to the message's own agent-task author: a
    /// wrapper naming a person, or a message it did not write, labels nothing.
    #[test]
    fn agent_labels_bind_only_agent_task_authors_of_their_own_message() {
        let mut doc = AutoCommit::new();
        crate::doc::ChatDoc::seed_if_empty(&mut doc);
        let send = crate::actions::send_message_action(
            "c_general",
            "m_person_1",
            "person-1",
            1,
            "hi",
            None,
            None,
            Vec::new(),
        );
        dispatch(
            &mut doc,
            crate::actions::encode(&crate::actions::ChatAction::AgentAuthored {
                name: "Impostor".into(),
                send: Box::new(send),
            }),
        );
        assert!(
            crate::doc::ChatDoc::list_messages(&doc, "c_general")
                .iter()
                .any(|m| m.id == "m_person_1")
        );
        assert!(crate::doc::ChatDoc::agent_authors(&doc).is_empty());
    }
}
