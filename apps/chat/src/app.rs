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

use std::collections::HashMap;
use std::time::Duration;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::VideoPlayer;

use crate::call::{CallState, OverlayDrag, SoundEngine};
use crate::doc::ChatDoc;
use crate::identity::LocalIdentity;
use crate::media::MediaEngine;
use crate::model::{self, Channel, Id, Message, User, Workspace};
use crate::permissions::ChatPermissions;
use crate::responsive::{FlapMode, FlapState, LEFT_BREAKPOINT, RIGHT_BREAKPOINT};
use crate::storage::{self, Saver};

// --- Fixed chrome metrics (logical px) ------------------------------------
pub(crate) const RAIL_W: f64 = 68.0;
pub(crate) const SIDE_W: f64 = 248.0;
pub(crate) const HEADER_H: f64 = 60.0;
pub(crate) const THREAD_W: f64 = 360.0;
pub(crate) const COMPOSER_MIN_H: f64 = 76.0;
pub(crate) const COMPOSER_MAX_H: f64 = 200.0;
pub(crate) const AVATAR: f64 = 40.0;

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
#[derive(Clone, Debug)]
pub(crate) struct ContextMenu {
    pub target: MenuTarget,
    pub anchor: Point,
    pub items: Vec<MenuAction>,
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

/// The structural actions reachable from a context menu. The **first**
/// channel action is also what a left-swipe on the row resolves to, so the
/// gesture and the menu can never drift (swipe⇔menu parity).
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
    /// Load one of your own messages into the inline editor.
    EditMessage,
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
            MenuAction::ToggleCollapse => "Collapse / expand",
            MenuAction::EditMessage => "Edit message",
        }
    }
    pub fn is_destructive(self) -> bool {
        matches!(self, MenuAction::Delete)
    }
    /// Menu items for a channel row, in order. The first (`Delete`) is the
    /// swipe-left action.
    pub fn for_channel() -> [MenuAction; 4] {
        [MenuAction::Delete, MenuAction::Rename, MenuAction::MoveUp, MenuAction::MoveDown]
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
    /// The action a swipe-left on a channel row resolves to.
    pub fn channel_swipe_left() -> MenuAction {
        Self::for_channel()[0]
    }
    /// Menu items for one of your own messages.
    pub fn for_message() -> [MenuAction; 1] {
        [MenuAction::EditMessage]
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
    Workspace(Id),
    Channel(Id),
    /// A voice-channel member chip in the sidebar — clicking joins the call.
    VoiceChannel(Id),
    /// Toggle a category's collapsed state.
    CategoryToggle(Id),
    /// Execute a context-menu / create action.
    MenuAction { target: MenuTarget, action: MenuAction },
    /// The "+" create-channel button on a category header.
    AddChannel { workspace: Id, category: Id },
    /// The "+" create-category button in the sidebar header.
    AddCategory(Id),
    /// The "+" create-workspace button at the foot of the rail.
    AddWorkspace,
    /// Confirm the inline rename editor.
    RenameConfirm,
    Reaction { msg: Id, emoji: String },
    AddReaction { msg: Id, anchor: Point },
    OpenThread(Id),
    EditHistory { msg: Id, anchor: Point },
    /// Right-click target over one of your own messages — opens the message
    /// action menu (Edit). Left-click on it is a no-op.
    MessageMenu { msg: Id },
    /// Commit the inline message editor to the CRDT.
    EditSave,
    /// Discard the inline message editor without writing.
    EditCancel,
    EmojiChoice(String),
    FocusMain,
    FocusThread,
    Send,
    ComposerEmoji(Point),
    /// Open the workspace file picker to share an attachment.
    Attach,
    /// Pick a specific file (by path) from the open attachment picker.
    AttachPick(String),
    StartCall,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    CallDeafen,
    CallAudioOnly,
    CallExpand,
    CallLeave,
    /// Toggle the left flap (rail + channel sidebar).
    ToggleLeftFlap,
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    Dismiss, // click on a scrim closes the top-most popover
}

pub struct ChatApp {
    // CRDT projection (rebuilt every sync / after a mutation).
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) channels: Vec<Channel>,
    pub(crate) categories: Vec<crate::model::Category>,
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
    /// Voice membership per channel in the active workspace (cached from the
    /// CRDT each reload so the sidebar can show presence without re-reading).
    pub(crate) voice_members: HashMap<Id, Vec<crate::model::VoiceMember>>,
    pub(crate) draft_main: String,
    pub(crate) draft_thread: String,

    // Per-peer view state.
    pub(crate) active_ws: Id,
    pub(crate) active_channel: Id,
    pub(crate) focus: Focus,
    pub(crate) caret: usize,
    pub(crate) scroll: f64,
    pub(crate) thread_open: Option<Id>,
    pub(crate) emoji_open: bool,
    pub(crate) emoji_target: Option<Id>,
    pub(crate) emoji_anchor: Point,
    pub(crate) history_open: Option<Id>,
    pub(crate) history_anchor: Point,
    /// Inline message editor: the id of your own message being edited plus its
    /// working text. `self.caret` indexes into this text. Enter/Save writes it
    /// through [`ChatDoc::edit_message`]; Esc/Cancel discards it.
    pub(crate) editing: Option<(Id, String)>,

    // Attachment picker (browses the shared workspace for a file to share).
    pub(crate) attach_open: bool,
    pub(crate) attach_entries: Vec<crate::picker::PickEntry>,

    // Structure editing.
    pub(crate) menu: Option<ContextMenu>,
    /// Inline rename editor: the target being renamed + its working text.
    pub(crate) rename: Option<(MenuTarget, String)>,
    pub(crate) swipe: Option<RowSwipe>,

    // Call state.
    pub(crate) call: CallState,
    pub(crate) overlay_drag: Option<OverlayDrag>,

    // Responsive flaps.
    pub(crate) left_flap: FlapState,
    pub(crate) right_flap: FlapState,
    pub(crate) left_mode: FlapMode,
    pub(crate) right_mode: FlapMode,

    // The local peer's identity: how this peer authors messages, reactions,
    // voice membership, and its own read watermark in the shared document.
    // Distinct + stable per real peer (see [`crate::identity`]) so genuine
    // multi-participant chat attributes and de-duplicates correctly.
    pub(crate) me: LocalIdentity,

    // Engines (call sounds + media transport).
    pub(crate) sound: SoundEngine,
    pub(crate) media: MediaEngine,

    // The OS permission gate: routes channel participation + file-embed
    // hydration through the toolkit's permission model. Refreshed from the
    // `PermissionContext` the compositor passes into `draw`.
    pub(crate) perms: ChatPermissions,

    // Persistence (None for ephemeral / `--shots` instances).
    saver: Option<Saver>,

    // Background image loader/cache for attachment thumbnails (decodes off
    // the UI thread; real bytes come from the FUSE mount).
    pub(crate) images: ui::ImageStore,

    // Layout + render caches.
    pub(crate) rect: Rect,
    root_node: Option<NodeId>,
    video_node: Option<NodeId>,
    pub(crate) video: VideoPlayer,
    pub(crate) hits: Vec<(Rect, Hit)>,
    pub(crate) last_mouse: Point,
    relayout: bool,
    /// True while either flap is mid-slide — drives `needs_animation_frame`
    /// so the loop keeps ticking only during the transition, then idles.
    flap_animating: bool,
    /// `--shots` only: seed the real minimal starter instead of the demo, so
    /// the honest first-run empty state can be captured.
    shot_starter: bool,
}

impl Default for ChatApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatApp {
    /// The persistent, live instance: resolves this peer's real identity,
    /// loads the saved workspace from disk (if any) and wires the background
    /// saver. The resolved identity is what makes two running instances
    /// genuinely different participants rather than two "u_me"s colliding.
    pub fn new() -> Self {
        let mut app = Self::bare();
        app.me = LocalIdentity::resolve();
        // Author the local view state under the real identity from the start so
        // nothing is attributed to the demo `u_me`.
        app.active_ws.clear();
        app.active_channel.clear();
        app.thread_open = None;
        if let Some(dir) = storage::data_dir() {
            app.saver = Some(Saver::spawn(dir, Duration::from_millis(400)));
        }
        app
    }

    /// A non-persistent instance for `--shots` / tests: never reads or
    /// writes disk, so a screenshot run can't clobber a real workspace.
    pub fn new_ephemeral() -> Self {
        Self::bare()
    }

    fn bare() -> Self {
        // The host call tile is a real VideoPlayer with no frame until a live
        // capture feeds one; there is no synthesised stand-in. Its honest
        // "camera off" state is painted over the empty surface.
        let video = VideoPlayer::new();
        Self {
            workspaces: Vec::new(),
            channels: Vec::new(),
            categories: Vec::new(),
            users: Vec::new(),
            messages: Vec::new(),
            voice_members: HashMap::new(),
            draft_main: String::new(),
            draft_thread: String::new(),
            active_ws: "ws_acme".into(),
            active_channel: "c_crit".into(),
            focus: Focus::Main,
            caret: 0,
            scroll: 0.0,
            thread_open: Some("m_concept".into()),
            emoji_open: false,
            emoji_target: None,
            emoji_anchor: Point::ZERO,
            history_open: None,
            history_anchor: Point::ZERO,
            editing: None,
            attach_open: false,
            attach_entries: Vec::new(),
            menu: None,
            rename: None,
            swipe: None,
            call: CallState::default(),
            overlay_drag: None,
            left_flap: FlapState::open_when_docked(),
            right_flap: FlapState::closed_by_default(),
            left_mode: FlapMode::Docked,
            right_mode: FlapMode::Docked,
            me: LocalIdentity::demo(),
            sound: SoundEngine::new(),
            media: MediaEngine::new(),
            perms: ChatPermissions::permissive(),
            saver: None,
            images: ui::ImageStore::new(crate::images::fetcher()),
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            relayout: false,
            flap_animating: false,
            shot_starter: false,
        }
    }

    /// Whether a call action is permitted, routed through the real OS
    /// permission gate for the active workspace. A View-only / ungranted peer
    /// is blocked from join/speak/camera/screen-share; leaving is always
    /// allowed so a downgraded peer can drop out.
    pub(crate) fn call_allows(&self, action: crate::call::CallAction) -> bool {
        self.perms.allows(&self.active_ws, action)
    }

    /// Whether the local peer may post / react / start a call in the active
    /// workspace (an Edit grant). The permissive default allows; a restricted
    /// context gates writes while still letting the peer read.
    pub(crate) fn can_participate(&self) -> bool {
        self.perms.can_participate(&self.active_ws)
    }

    /// Whether the local peer may hydrate + render the bytes behind an
    /// attachment. An ungranted embed shows a locked card and never touches
    /// the file — the permission boundary for a projected FUSE share.
    pub(crate) fn embed_allowed(&self, path: &str) -> bool {
        self.perms.can_embed(path)
    }

    /// Persist the current document. Called after every mutation; a no-op
    /// for ephemeral instances.
    pub(crate) fn persist(&self, doc: &mut AutoCommit) {
        if let Some(saver) = &self.saver {
            saver.request(doc.save());
        }
    }

    /// Stage a named scenario for a headless screenshot against the ephemeral
    /// demo workspace. The default (empty) scenario is the rich desktop frame:
    /// an open thread beside the conversation and a live call huddle.
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
            "compact" => {
                // Medium window: docked channel list, conversation reflowed,
                // members/thread summoned on demand (not forced open), no call.
                self.thread_open = None;
                self.right_flap.snap(false);
                self.call = CallState::default();
            }
            "starter" => {
                // The real first-run empty workspace (no demo content).
                self.shot_starter = true;
                self.thread_open = None;
                self.call = CallState::default();
            }
            "context" => {
                self.menu = Some(ContextMenu {
                    target: MenuTarget::Channel("c_frontend".into()),
                    anchor: Point::new(240.0, 230.0),
                    items: MenuAction::for_channel().to_vec(),
                });
            }
            "call" => {
                // The expanded full-window call overlay against the demo
                // huddle (host + three remotes), so the tile grid and the
                // wrapped controls bar can be captured headless at any width.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call.join("c_huddle");
                self.call.expanded = true;
            }
            "attach" => {
                // Show the workspace file picker with real, decodable
                // thumbnails (the demo scheme drives the same decode path a
                // projected FUSE image would). Ephemeral shot only.
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
            "edit" => {
                // The inline message editor open on your own last message,
                // with an earlier message still carrying the static "(edited)"
                // marker — the whole edit story in one frame. Ephemeral shot
                // only: at seed time the messages aren't loaded yet, so the
                // working text is set directly and the caret is clamped to its
                // end by `reload`.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.editing = Some((
                    "m_me2".into(),
                    "That's the one. Starting a huddle to walk through the grid — jump in. Moving it to 3pm so the west-coast folks can make it."
                        .into(),
                ));
                self.focus = Focus::Main;
                self.caret = usize::MAX; // clamped to the working text length on reload
            }
            "msgmenu" => {
                // The right-click action menu over one of your own messages,
                // offering Edit — the discoverable entry point to editing.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.menu = Some(ContextMenu {
                    target: MenuTarget::Message("m_me2".into()),
                    anchor: Point::new(360.0, 300.0),
                    items: MenuAction::for_message().to_vec(),
                });
            }
            _ => {
                // Desktop demo: reveal the thread beside the conversation and
                // put this peer in the Design Huddle so the call surface shows.
                self.right_flap.open();
                self.call.join("c_huddle");
            }
        }
    }

    // --- model lookups ----------------------------------------------------
    pub(crate) fn user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }
    pub(crate) fn user_name(&self, id: &str) -> String {
        self.user(id).map(|u| u.name.clone()).unwrap_or_else(|| id.to_string())
    }
    pub(crate) fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
    pub(crate) fn message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// Channels in the active workspace, ordered.
    pub(crate) fn ws_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> =
            self.channels.iter().filter(|c| c.workspace == self.active_ws).collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// Categories in the active workspace, ordered.
    pub(crate) fn ws_categories(&self) -> Vec<&crate::model::Category> {
        let mut v: Vec<&crate::model::Category> =
            self.categories.iter().filter(|c| c.workspace == self.active_ws).collect();
        v.sort_by_key(|c| c.order);
        v
    }

    pub(crate) fn category(&self, id: &str) -> Option<&crate::model::Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Cached voice members of `channel` (empty when nobody is connected).
    pub(crate) fn voice_in(&self, channel: &str) -> &[crate::model::VoiceMember] {
        self.voice_members.get(channel).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The media routing for the current call, chosen from the live participant
    /// count: peer-to-peer below the mesh threshold, host-relay above it. A
    /// call transparently promotes from P2P to host-based as people join.
    pub(crate) fn call_session(&self) -> crate::model::CallSession {
        crate::model::CallSession::select(self.voice_in(&self.call.channel).len().max(1))
    }

    /// Channels in `category` within the active workspace, ordered.
    pub(crate) fn category_channels(&self, category: &str) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.workspace == self.active_ws && c.category == category)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    pub(crate) fn focused_draft(&self) -> &str {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &self.draft_thread,
            _ => &self.draft_main,
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
        self.users = ChatDoc::list_users(doc);
        // Keep the active channel valid (e.g. after a workspace switch).
        if self.channel(&self.active_channel).map(|c| c.workspace.clone())
            != Some(self.active_ws.clone())
        {
            if let Some(first) = self
                .ws_channels()
                .into_iter()
                .find(|c| c.kind == model::ChannelKind::Text)
                .map(|c| c.id.clone())
            {
                self.active_channel = first;
            }
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
        self.messages = ChatDoc::list_messages(doc, &self.active_channel);
        // Cache voice membership for every voice channel in this workspace.
        self.voice_members.clear();
        for ch in self.channels.iter().filter(|c| c.kind == model::ChannelKind::Voice) {
            let members = ChatDoc::list_voice_members(doc, &ch.id);
            if !members.is_empty() {
                self.voice_members.insert(ch.id.clone(), members);
            }
        }
        self.draft_main = ChatDoc::draft_text(doc, &self.active_channel);
        self.draft_thread = match self.thread_draft_key() {
            Some(k) => ChatDoc::draft_text(doc, &k),
            None => String::new(),
        };
        // Clamp the caret to whatever text it currently indexes: the inline
        // message editor when one is open, otherwise the focused draft.
        let max = match &self.editing {
            Some((_, text)) => text.chars().count(),
            None => self.focused_draft().chars().count(),
        };
        if self.caret > max {
            self.caret = max;
        }
    }

    /// Advance the local read watermark for the channel currently in view so
    /// its unread badge stays cleared and doesn't reappear once the user
    /// navigates away. Idempotent (a no-op when nothing new arrived), so it is
    /// safe to call every frame.
    pub(crate) fn mark_active_read(&self, doc: &mut AutoCommit) {
        if !self.active_channel.is_empty() {
            ChatDoc::mark_read(doc, &self.active_channel, &self.me.id);
        }
    }

    /// The members roster shown in the right flap when no thread is open: every
    /// user in the workspace paired with the voice channel they're currently in
    /// (if any). Sorted in-call first, then by presence, then by name. Real
    /// projection of the shared user list + voice membership — no placeholders.
    pub(crate) fn roster_rows(&self) -> Vec<(User, Option<String>)> {
        // user id → the name of the voice channel they're connected to.
        let mut in_voice: HashMap<&str, String> = HashMap::new();
        for (ch_id, members) in &self.voice_members {
            let ch_name =
                self.channel(ch_id).map(|c| c.name.clone()).unwrap_or_else(|| ch_id.clone());
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
            rank(&a.0, &a.1).cmp(&rank(&b.0, &b.1)).then_with(|| a.0.name.cmp(&b.0.name))
        });
        rows
    }

    // --- responsive flap geometry ----------------------------------------
    //
    // Each flap's *physical* pane width is fixed; what changes with the mode
    // is whether the content is inset by it (docked) or whether the pane
    // floats over the content behind a scrim (overlay). The eased `slide`
    // animates the float in/out.

    /// Total width occupied by the left navigation pane (rail + sidebar).
    pub(crate) fn left_pane_w(&self) -> f64 {
        RAIL_W + SIDE_W
    }

    /// True when the left flap currently shows (docked, or revealed overlay).
    pub(crate) fn left_flap_open(&self) -> bool {
        self.left_flap.is_open(self.left_mode)
    }
    /// True when the right flap (members / thread) currently shows.
    pub(crate) fn right_flap_open(&self) -> bool {
        self.right_flap.is_open(self.right_mode)
    }

    /// X where the left pane currently starts. In overlay mode it slides in
    /// from off the left edge as `reveal` rises.
    pub(crate) fn left_pane_x0(&self) -> f64 {
        match self.left_mode {
            FlapMode::Docked => self.rect.x0,
            FlapMode::Overlay => self.rect.x0 - self.left_pane_w() * (1.0 - self.left_flap.slide()),
        }
    }

    /// X where the right pane currently starts (its left edge).
    pub(crate) fn right_pane_x0(&self) -> f64 {
        let w = THREAD_W;
        match self.right_mode {
            FlapMode::Docked => self.rect.x1 - w,
            FlapMode::Overlay => self.rect.x1 - w * self.right_flap.slide(),
        }
    }

    // --- region geometry --------------------------------------------------
    pub(crate) fn rail_rect(&self) -> Rect {
        let x0 = self.left_pane_x0();
        Rect::new(x0, self.rect.y0, x0 + RAIL_W, self.rect.y1)
    }
    pub(crate) fn sidebar_rect(&self) -> Rect {
        let x0 = self.left_pane_x0() + RAIL_W;
        Rect::new(x0, self.rect.y0, x0 + SIDE_W, self.rect.y1)
    }
    /// Right edge of the centre column. In docked mode it yields room to a
    /// revealed right flap; in overlay mode the content spans full width and
    /// the flap floats over it.
    pub(crate) fn center_right(&self) -> f64 {
        if self.right_mode == FlapMode::Docked && self.right_flap_open() {
            self.rect.x1 - THREAD_W
        } else {
            self.rect.x1
        }
    }
    pub(crate) fn center_left(&self) -> f64 {
        if self.left_mode == FlapMode::Docked && self.left_flap_open() {
            self.rect.x0 + self.left_pane_w()
        } else {
            // Overlay (or hidden docked): content reclaims the pane width;
            // leave room for the floating flap toggle button.
            self.rect.x0
        }
    }
    pub(crate) fn header_rect(&self) -> Rect {
        Rect::new(self.center_left(), self.rect.y0, self.center_right(), self.rect.y0 + HEADER_H)
    }
    pub(crate) fn composer_height(&self, tr: &mut TextRenderer) -> f64 {
        let w = (self.center_right() - self.center_left() - 48.0).max(40.0) as f32;
        let (_, h) = tr.measure(self.draft_main.as_str(), 15.0, Some(w));
        (COMPOSER_MIN_H + (h as f64 - 19.0).max(0.0)).clamp(COMPOSER_MIN_H, COMPOSER_MAX_H)
    }
    pub(crate) fn composer_rect(&self, tr: &mut TextRenderer) -> Rect {
        let h = self.composer_height(tr);
        Rect::new(
            self.center_left() + 14.0,
            self.rect.y1 - h - 12.0,
            self.center_right() - 14.0,
            self.rect.y1 - 12.0,
        )
    }
    pub(crate) fn messages_rect(&self, tr: &mut TextRenderer) -> Rect {
        let composer = self.composer_rect(tr);
        Rect::new(self.center_left(), self.rect.y0 + HEADER_H, self.center_right(), composer.y0 - 6.0)
    }
    pub(crate) fn thread_rect(&self) -> Rect {
        let x0 = self.right_pane_x0();
        Rect::new(x0, self.rect.y0, x0 + THREAD_W, self.rect.y1)
    }

    /// Resolve flap modes from the current width and advance their eased
    /// reveals. Called once per frame at the top of `draw`. Returns true
    /// while either flap is still animating (so the host keeps redrawing).
    pub(crate) fn reconcile_flaps(&mut self) -> bool {
        let w = self.rect.width();
        // Ignore a degenerate pre-layout frame: resolving the flap mode from a
        // zero width would snap it to the overlay default before the real
        // window size is known. Wait for a genuine allocation so the first
        // reconcile lands on the actual breakpoint.
        if w < 1.0 {
            return false;
        }
        self.left_mode = FlapMode::from_width(w, LEFT_BREAKPOINT);
        self.right_mode = FlapMode::from_width(w, RIGHT_BREAKPOINT);
        self.left_flap.reconcile(self.left_mode);
        self.right_flap.reconcile(self.right_mode);
        let a = self.left_flap.tick(self.left_mode);
        let b = self.right_flap.tick(self.right_mode);
        self.flap_animating = a || b;
        a || b
    }

    /// Keep the active workspace / channel / thread pointing at something that
    /// exists — used after seeding a fresh (real) starter whose ids differ
    /// from the demo defaults, and after any structural delete. Returns true
    /// when the active workspace changed (the caller must reload so the
    /// channel + message lists refresh).
    pub(crate) fn ensure_selection(&mut self) -> bool {
        let mut ws_changed = false;
        if !self.workspaces.iter().any(|w| w.id == self.active_ws) {
            if let Some(first) = self.workspaces.first() {
                self.active_ws = first.id.clone();
                self.active_channel.clear();
                ws_changed = true;
            }
        }
        // Drop a stale open thread whose parent isn't in this workspace.
        if let Some(t) = self.thread_open.clone() {
            if self.message(&t).is_none() {
                self.thread_open = None;
                if self.focus == Focus::Thread {
                    self.focus = Focus::Main;
                }
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
            if let Some(crate::model::Attachment::Image { path, .. }) = &m.attachment {
                // Never hydrate the bytes of an embed this peer isn't granted.
                if self.embed_allowed(path) {
                    let _ = self.images.get(path);
                }
            }
        }
        // Also warm any thumbnails visible in an open attachment picker.
        for e in &self.attach_entries {
            let _ = self.images.get(&e.key());
        }
        self.images.block_until_idle(std::time::Duration::from_secs(3));
    }

    /// The host-tile video rect for the current call mode, including the
    /// dragged overlay offset (docked huddle only — the expanded grid is
    /// anchored). The single source of truth for both the VideoPlayer node
    /// placement and the surrounding call chrome.
    pub(crate) fn call_video_rect(&self) -> Rect {
        if self.call.expanded {
            // The host is the first cell of the responsive participant grid,
            // so the tile (and the VideoPlayer surface beneath it) fills its
            // share of the surface at every width — never a fixed fraction
            // that leaves the rest of the overlay empty.
            self.overlay_host_rect()
        } else {
            frac_rect(self.rect, HUDDLE_VIDEO) + self.call.overlay_offset
        }
    }

    pub(crate) fn push_hit(hits: &mut Vec<(Rect, Hit)>, rect: Rect, hit: Hit) {
        hits.push((rect, hit));
    }
}

/// Compute an absolute rect from window-relative fractions.
pub(crate) fn frac_rect(win: Rect, f: (f64, f64, f64, f64)) -> Rect {
    let x0 = win.x0 + f.0 * win.width();
    let y0 = win.y0 + f.1 * win.height();
    Rect::new(x0, y0, x0 + f.2 * win.width(), y0 + f.3 * win.height())
}

impl Widget for ChatApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        if self.saver.is_some() {
            // Live instance. Restore a persisted workspace if one exists so a
            // restart keeps prior messages/structure; otherwise scaffold a
            // real, empty starter workspace for this user — never the demo.
            if !ChatDoc::is_seeded(doc) {
                if let Some(dir) = storage::data_dir() {
                    if let Ok(Some(bytes)) = storage::load_bytes(&dir) {
                        if let Ok(loaded) = AutoCommit::load(&bytes) {
                            let _ = doc.merge(&mut loaded.clone());
                        }
                    }
                }
            }
            if !ChatDoc::is_seeded(doc) {
                ChatDoc::seed_starter(doc, &self.me.id, &self.me.name, &self.me.initials);
            }
            // Announce this peer into the shared roster so every other
            // participant sees its real name/colour — whether we seeded the
            // workspace or adopted a peer's shared document. Guarded so it
            // writes once and doesn't churn the doc (which would spam empty
            // collab deltas) on subsequent frames.
            if ChatDoc::ensure_user(
                doc,
                &self.me.id,
                &self.me.name,
                &self.me.initials,
                self.me.color_idx,
                model::Presence::Online,
            ) {
                self.persist(doc);
            }
        } else if self.shot_starter {
            // `--shots` capture of the honest first-run empty workspace.
            ChatDoc::seed_starter(doc, &self.me.id, &self.me.name, &self.me.initials);
        } else {
            // Ephemeral `--shots`/test instance: the rich demo workspace.
            ChatDoc::seed_if_empty(doc);
        }
        self.reload(doc);
        if self.ensure_selection() {
            // Re-picking the workspace changes which channels/messages apply.
            self.reload(doc);
        }
        // The channel in view is read: advance its watermark (no-op when
        // nothing new arrived) so its unread badge stays cleared.
        self.mark_active_read(doc);
        // While in a call, keep the media signalling handshake current: dial
        // any peer that has joined since the last frame (idempotent).
        if self.call.active {
            self.connect_call_peers(doc);
        }
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
                size: Size { width: percent(f.2 as f32), height: percent(f.3 as f32) },
                ..Default::default()
            }
        };
        let video_node = taffy.new_leaf(video_style).unwrap();
        node_map.insert(video_node, &mut self.video as *mut VideoPlayer as *mut dyn Widget);
        self.video_node = Some(video_node);

        let root = taffy
            .new_with_children(
                Style {
                    size: Size { width: percent(1.0), height: percent(1.0) },
                    ..Default::default()
                },
                &[video_node],
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        self.root_node = Some(root);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if let Some(vn) = self.video_node {
            // Shift the video by the dragged overlay offset (huddle only; the
            // expanded grid is anchored). The taffy node holds the fractional
            // home; the origin adds the drag delta.
            let off = if self.call.expanded {
                Point::new(self.rect.x0, self.rect.y0)
            } else {
                Point::new(self.rect.x0 + self.call.overlay_offset.x, self.rect.y0 + self.call.overlay_offset.y)
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
        // Resolve flap modes + advance their eased reveals for this frame.
        self.reconcile_flaps();

        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let c = &theme.tokens.colors;

        // Base canvas.
        crate::paint::fill_rect(scene, tf, self.rect, c.surfaces.base);

        // Header + conversation occupy the centre column.
        self.draw_header(scene, tf, tr, theme, &mut hits);
        let msg_area = self.messages_rect(tr);
        crate::paint::push_clip(scene, tf, msg_area);
        self.draw_messages(scene, tf, tr, theme, msg_area, &mut hits);
        scene.pop_layer();
        self.draw_composer(scene, tf, tr, theme, &mut hits);

        // Right flap (members / thread). In overlay mode a scrim dims the
        // content behind the floating panel.
        if self.right_mode == FlapMode::Overlay && self.right_flap.reveal() > 0.001 {
            let a = (self.right_flap.reveal() * 0.55) as f32;
            crate::paint::fill_rect(scene, tf, self.rect, c.surfaces.overlay.with_alpha_factor(a));
            ChatApp::push_hit(&mut hits, self.rect, Hit::Dismiss);
        }
        if self.right_flap_open() || self.right_flap.reveal() > 0.001 {
            // The right flap is "members / thread": it shows the open thread
            // when one is up, otherwise the workspace members roster (so the
            // "👥 N" pill always reveals a real panel, never a blank column).
            if self.thread_open.is_some() {
                self.draw_thread(scene, tf, tr, theme, &mut hits);
            } else {
                self.draw_members(scene, tf, tr, theme, &mut hits);
            }
        }

        // Left flap (rail + channel sidebar). Overlay scrim first.
        if self.left_mode == FlapMode::Overlay && self.left_flap.reveal() > 0.001 {
            let a = (self.left_flap.reveal() * 0.55) as f32;
            crate::paint::fill_rect(scene, tf, self.rect, c.surfaces.overlay.with_alpha_factor(a));
            ChatApp::push_hit(&mut hits, self.rect, Hit::Dismiss);
        }
        if self.left_flap_open() || self.left_flap.reveal() > 0.001 {
            self.draw_rail(scene, tf, tr, theme, &mut hits);
            self.draw_sidebar(scene, tf, tr, theme, &mut hits);
        }

        // Flap toggle buttons (hamburger / members) float over the header in
        // overlay mode (or whenever a flap is hidden).
        self.draw_flap_toggles(scene, tf, tr, theme, &mut hits);

        // Call layers. The host tile is a real VideoPlayer; with no live
        // capture feeding it (there is no camera here) it paints an honest
        // "camera off" placeholder rather than any fabricated frame.
        if self.call.active && self.call.expanded {
            self.draw_call_overlay_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
            self.draw_host_camera_off(scene, tf, tr, theme);
            self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
        } else if self.call.active {
            self.draw_huddle_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
            self.draw_host_camera_off(scene, tf, tr, theme);
            self.draw_huddle_fg(scene, tf, tr, theme, &mut hits);
        }

        // Top-most popovers.
        if let Some(menu) = self.menu.clone() {
            self.draw_context_menu(scene, tf, tr, theme, &menu, &mut hits);
        }
        if self.emoji_open {
            self.draw_emoji_picker(scene, tf, tr, theme, &mut hits);
        }
        if self.history_open.is_some() {
            self.draw_history_popover(scene, tf, tr, theme, &mut hits);
        }
        if self.attach_open {
            self.draw_attach_picker(scene, tf, tr, theme, &mut hits);
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
        // Root spans the whole window — always paint.
        self.draw(scene, tf, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        self.dispatch_event(event, scale_factor, modifiers, state, handled);
    }

    fn name(&self) -> &str {
        "ChatApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }

    /// Keep the loop ticking only while something is actually moving: a flap
    /// mid-slide, an overlay being dragged, or attachment images still
    /// decoding in the background. A live-but-static call does **not** force
    /// continuous redraws (the roster/rings are static), so an idle call
    /// no longer pins a core.
    fn needs_animation_frame(&self) -> bool {
        self.flap_animating || self.overlay_drag.is_some() || !self.images.is_idle()
    }

    fn needs_redraw(&self) -> bool {
        true
    }
}

impl ChatApp {
    /// Flag a taffy rebuild (kept for parity; the video rect is now set in
    /// `apply_layout` so most state changes don't need it, but call expand
    /// still requests a clean relayout).
    pub(crate) fn request_relayout(&mut self) {
        self.relayout = true;
    }
}
