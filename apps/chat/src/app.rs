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
use ui::widget::{TextInput, VideoPlayer};

use crate::actions::{self, ChatAction};
use crate::call::{CallState, OverlayDrag, SoundEngine};
use crate::doc::ChatDoc;
use crate::identity::LocalIdentity;
use crate::media::MediaEngine;
use crate::model::{self, Channel, Id, Message, User, Workspace};
use crate::permissions::ChatPermissions;
use crate::responsive::{FlapMode, FlapState, LEFT_BREAKPOINT, RIGHT_BREAKPOINT};

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
            MenuAction::ToggleCollapse => "Collapse / expand",
            MenuAction::Archive => "Archive / unarchive",
            MenuAction::EditMessage => "Edit message",
            MenuAction::PinMessage => "Pin / unpin",
            MenuAction::DeleteMessage => "Delete message",
        }
    }
    pub fn is_destructive(self) -> bool {
        matches!(self, MenuAction::Delete | MenuAction::DeleteMessage)
    }
    /// Menu items for a channel row, in order. The first (`Delete`) is the
    /// swipe-left action.
    pub fn for_channel() -> [MenuAction; 5] {
        [
            MenuAction::Delete,
            MenuAction::Archive,
            MenuAction::Rename,
            MenuAction::MoveUp,
            MenuAction::MoveDown,
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
    /// The action a swipe-left on a channel row resolves to.
    pub fn channel_swipe_left() -> MenuAction {
        Self::for_channel()[0]
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
    MenuAction {
        target: MenuTarget,
        action: MenuAction,
    },
    /// The "+" create-channel button on a category header.
    AddChannel {
        workspace: Id,
        category: Id,
    },
    /// The "+" create-category button in the sidebar header.
    AddCategory(Id),
    /// The "+" create-workspace button at the foot of the rail.
    AddWorkspace,
    /// Confirm the inline rename editor.
    RenameConfirm,
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
    /// Start/stop sharing this peer's screen (portal `ScreenCast`).
    CallShareScreen,
    CallExpand,
    CallLeave,
    /// Toggle the left flap (rail + channel sidebar).
    ToggleLeftFlap,
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    Dismiss, // click on a scrim closes the top-most popover
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
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
    /// Voice membership per channel in the active workspace (cached from the
    /// CRDT each reload so the sidebar can show presence without re-reading).
    pub(crate) voice_members: HashMap<Id, Vec<crate::model::VoiceMember>>,
    /// Cached plain text of the focused drafts, mirrored from the composer
    /// widgets each `reload`. Kept only so the composer's auto-grow height
    /// (`composer_height`) and the tests can read the draft without reaching
    /// into the widget; the editable source of truth is the `TextInput`.
    pub(crate) draft_main: String,
    pub(crate) draft_thread: String,

    /// The message composers — shared multiline [`TextInput`]s (Enter sends,
    /// Shift+Enter inserts a newline) so the composer gets real caret,
    /// selection, clipboard (Ctrl+C/V/X), focus ring, a11y, and theming for
    /// free instead of a hand-rolled `String` + caret index. One per surface:
    /// the main timeline and the open thread. Both are local-mode; each
    /// keystroke is written back into the per-channel/thread draft `Text` in
    /// the CRDT (diff-spliced) so a draft stays collaborative + persistent.
    pub(crate) composer_main: TextInput,
    pub(crate) composer_thread: TextInput,

    // Per-peer view state.
    pub(crate) active_ws: Id,
    pub(crate) active_channel: Id,
    pub(crate) focus: Focus,
    /// Caret index for the in-memory inline editors only — the message-edit
    /// working text (`editing`) and the inline rename (`rename`). The
    /// composers own their own caret inside their `TextInput`.
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

    // The room's answer for who everybody is, refreshed every frame from
    // presence (see `adopt_room`). A peer never writes its own name into
    // this map — only what the room says about a participant lands here.
    pub(crate) room_names: HashMap<Id, String>,
    pub(crate) room_colors: HashMap<Id, (u8, u8, u8)>,

    // Ids this session authored under before the room told it which
    // participant it is. See `adopt_own_participant`: what was already said
    // under one of these is still this person, so their own earlier lines go
    // on reading "You" rather than turning into a stranger mid-scroll.
    pub(crate) former_ids: Vec<Id>,

    // Engines (call sounds + media transport).
    pub(crate) sound: SoundEngine,
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
    pub(crate) actions: ui::ActionHost,

    // Persistence (None for ephemeral / `--shots` instances).

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
    /// tile (camera or screen-share). Populated only by the live media pump; in
    /// the default build it stays empty, so a tile shows the honest placeholder
    /// until a real decoded frame arrives — it never fabricates a picture.
    pub(crate) remote_frames: HashMap<String, vello::peniko::ImageData>,

    /// Latest decoded RGBA **screen-share** frame per remote peer id, kept
    /// separate from their camera frame so a peer can share their screen and
    /// keep their camera on at once — the screen fills the featured stage tile,
    /// the camera stays in that peer's filmstrip cell. Populated only by the
    /// live media pump; empty in the default build, where the featured tile
    /// shows the honest "connecting" placeholder rather than a fabricated feed.
    pub(crate) remote_screens: HashMap<String, vello::peniko::ImageData>,

    /// The screen-capture source seam. Consulted before a share starts so the
    /// real xdg-desktop-portal `ScreenCast` flow (behind `live-media`) can be
    /// swapped for an injected fake in tests. The default [`SystemPortal`]
    /// returns `None` in the default build — honest gating, never a faked share.
    pub(crate) portal: Box<dyn crate::call::ScreenPortal>,

    // Live capture/encode/playback pipelines — present only under `live-media`
    // and only while in a call. All `None`/empty until the call opens a device;
    // torn down when the call ends or a device is unavailable (honest silence).
    #[cfg(feature = "live-media")]
    mic: Option<crate::audio::MicCapture>,
    #[cfg(feature = "live-media")]
    speaker: Option<crate::audio::SpeakerPlayback>,
    #[cfg(feature = "live-media")]
    camera: Option<crate::video::CameraCapture>,
    // Camera encode reuses the shared qOS video stack's bare H.264 encoder
    // (raw NALUs: the local self-preview feeds them to `ui`'s VideoPlayer,
    // and they publish as the camera track). No second in-app codec.
    #[cfg(feature = "live-media")]
    cam_encoder: Option<qos_callstream::H264Encoder>,
    #[cfg(feature = "live-media")]
    screen: Option<crate::video::ScreenCapture>,
    // Screen-share encode + framing runs the shared stack's throttle +
    // encoder → `SurfaceFrame` sender seam — the same composition the
    // compositor's `SharedFlatpakSession` performs for a Wayland surface.
    #[cfg(feature = "live-media")]
    screen_session: Option<qos_callstream::VideoStreamSender>,
    // Remote **camera** decoders (raw-NALU H.264 → RGBA), keyed by sender.
    #[cfg(feature = "live-media")]
    remote_decoders: HashMap<String, qos_callstream::H264Decoder>,
    // Remote **screen-share** receivers (reframe + decode) → featured tile,
    // keyed by sender. The receive half of the shared stack.
    #[cfg(feature = "live-media")]
    remote_screen_rx: HashMap<String, qos_callstream::VideoStreamReceiver>,
    #[cfg(feature = "live-media")]
    media_seq: u64,
}

impl Default for ChatApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatApp {
    /// The live instance: resolves this peer's real identity. The document
    /// itself is loaded and written by `app-state` (see `main`), so nothing
    /// here touches disk. The resolved identity is what makes two running
    /// instances genuinely different participants rather than two "`u_me"s`
    /// colliding.
    pub fn new() -> Self {
        let mut app = Self::bare();
        app.is_live = true;
        app.me = LocalIdentity::resolve();
        // The live binary uses the real transport (a bound iroh endpoint on a
        // real session; the simulator otherwise). Tests/`--shots` keep the
        // deterministic local bus from `bare`.
        app.media = MediaEngine::new();
        // Author the local view state under the real identity from the start so
        // nothing is attributed to the demo `u_me`.
        app.active_ws.clear();
        app.active_channel.clear();
        app.thread_open = None;
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
            composer_main: Self::new_composer(),
            composer_thread: Self::new_composer(),
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
            room_names: HashMap::new(),
            room_colors: HashMap::new(),
            former_ids: Vec::new(),
            sound: SoundEngine::new(),
            // Default to the deterministic local bus; the live binary
            // (`ChatApp::new`) upgrades to the real transport. This keeps
            // `--shots`/tests from binding a real iroh endpoint per instance.
            media: MediaEngine::simulated(),
            perms: ChatPermissions::permissive(),
            actions: ui::ActionHost::new(),
            images: ui::ImageStore::new(&crate::images::fetcher()),
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            relayout: false,
            flap_animating: false,
            shot_starter: false,
            shot_composer_draft: None,
            is_live: false,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            portal: Box::new(crate::call::SystemPortal),
            #[cfg(feature = "live-media")]
            mic: None,
            #[cfg(feature = "live-media")]
            speaker: None,
            #[cfg(feature = "live-media")]
            camera: None,
            #[cfg(feature = "live-media")]
            cam_encoder: None,
            #[cfg(feature = "live-media")]
            screen: None,
            #[cfg(feature = "live-media")]
            screen_session: None,
            #[cfg(feature = "live-media")]
            remote_decoders: HashMap::new(),
            #[cfg(feature = "live-media")]
            remote_screen_rx: HashMap::new(),
            #[cfg(feature = "live-media")]
            media_seq: 0,
        }
    }

    /// Whether a call action is permitted, routed through the real OS
    /// permission gate for the active workspace. A View-only / ungranted peer
    /// is blocked from join/speak/camera/screen-share; leaving is always
    /// allowed so a downgraded peer can drop out.
    pub(crate) fn call_allows(&self, action: crate::call::CallAction) -> bool {
        self.perms.allows(action)
    }

    /// Whether the local peer may post / react / start a call in the active
    /// workspace (an Edit grant). The permissive default allows; a restricted
    /// context gates writes while still letting the peer read.
    pub(crate) fn can_participate(&self) -> bool {
        self.perms.can_participate()
    }

    /// Whether the local peer may hydrate + render the bytes behind an
    /// attachment. An ungranted embed shows a locked card and never touches
    /// the file — the permission boundary for a projected FUSE share.
    pub(crate) fn embed_allowed(&self, path: &str) -> bool {
        self.perms.can_embed(path)
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
        self.actions
            .dispatch(doc, actions::CHAT_ACTION_TYPE, action);
    }

    /// The last [`EncodedAction`](qos_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the affordance == [`ActionSpec`](qos_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical action.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The context Ctrl+K needs to build Chat's resource tree: the active
    /// workspace, its channels (including archived, so they can be unarchived from
    /// the palette), the open channel, and that channel's messages. Derived
    /// entirely from the projection the document already holds.
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
        let messages = self
            .messages
            .iter()
            .map(|m| actions::MessageResource {
                id: m.id.clone(),
                label: self.message_resource_label(m),
                pinned: m.pinned,
            })
            .collect();
        actions::ChatResourceCtx {
            me: self.me.id.clone(),
            workspace_id: self.active_ws.clone(),
            workspace_name,
            channels,
            active_channel: self.active_channel.clone(),
            messages,
            participants: self.participants(),
        }
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
            "composer" => {
                // The composer mid-compose: a multi-line draft with the caret,
                // so the shared TextInput's text + caret + focus ring can be
                // graded. Thread/call closed to keep the composer prominent.
                self.right_flap.snap(false);
                self.thread_open = None;
                self.call = CallState::default();
                self.focus = Focus::Main;
                self.shot_composer_draft = Some(
                    "Shipping the composer on the shared TextInput — real caret, \
                     selection, and Ctrl+C/V now.\nEnter sends, Shift+Enter for a new line."
                        .into(),
                );
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

    /// The live media pump: run once per frame while the engine is live. Pulls
    /// captured microphone/camera/screen media, encodes it and publishes it over
    /// the transport, then decodes and plays whatever arrived from peers. Every
    /// device/track honours the call permission gate; when a device is absent
    /// the corresponding pipeline is simply `None` and that medium stays silent.
    #[cfg(feature = "live-media")]
    fn pump_media(&mut self) {
        if !self.call.active {
            self.pump_media_reset();
            return;
        }
        let me = self.me.id.clone();
        self.pump_mic(&me);
        self.pump_camera(&me);
        self.pump_screen(&me);
        self.pump_receive(&me);
    }

    /// Release all devices + remote state between calls.
    #[cfg(feature = "live-media")]
    fn pump_media_reset(&mut self) {
        self.mic = None;
        self.speaker = None;
        self.camera = None;
        self.cam_encoder = None;
        self.screen = None;
        self.screen_session = None;
        self.remote_decoders.clear();
        self.remote_screen_rx.clear();
        self.remote_frames.clear();
        self.remote_screens.clear();
    }

    /// Microphone → Opus → publish (only while transmitting).
    #[cfg(feature = "live-media")]
    fn pump_mic(&mut self, me: &str) {
        use crate::call::CallAction;
        use crate::media::{MediaObject, TrackKind};
        use std::time::Duration;

        let transmit =
            !self.call.muted && !self.call.deafened && self.call_allows(CallAction::Speak);
        if !transmit {
            self.mic = None;
            return;
        }
        if self.mic.is_none() {
            self.mic = crate::audio::MicCapture::new();
        }
        let packets = self
            .mic
            .as_mut()
            .map(super::audio::MicCapture::poll)
            .unwrap_or_default();
        for pkt in packets {
            let seq = self.next_media_seq();
            self.media.transport_mut().send(MediaObject {
                sender: me.to_string(),
                kind: TrackKind::Audio,
                seq,
                pts: Duration::from_millis(seq * 20),
                payload: pkt,
            });
        }
    }

    /// Camera → H.264 → local preview + publish (only with camera on).
    #[cfg(feature = "live-media")]
    fn pump_camera(&mut self, me: &str) {
        use crate::call::CallAction;
        use crate::media::{MediaObject, TrackKind};
        use std::time::Duration;

        let camera_on = !self.call.audio_only && self.call_allows(CallAction::Camera);
        if !camera_on {
            self.camera = None;
            self.cam_encoder = None;
            return;
        }
        if self.camera.is_none() {
            self.camera =
                crate::video::CameraCapture::new(crate::video::DEFAULT_W, crate::video::DEFAULT_H);
            // Camera encode reuses the shared stack's H.264 encoder.
            self.cam_encoder = qos_callstream::H264Encoder::new(
                crate::video::DEFAULT_W,
                crate::video::DEFAULT_H,
                crate::video::DEFAULT_FPS,
            )
            .ok();
        }
        let nalus = match (self.camera.as_mut(), self.cam_encoder.as_mut()) {
            (Some(cam), Some(enc)) => cam.poll().map(|rgba| enc.encode(&rgba).unwrap_or_default()),
            _ => None,
        };
        let Some(nalus) = nalus else {
            return;
        };
        if nalus.is_empty() {
            return;
        }
        // Feed the host tile's VideoPlayer so this peer sees its own
        // camera (the surface yields its placeholder to a real frame).
        self.video.feed_frame(&nalus);
        let seq = self.next_media_seq();
        self.media.transport_mut().send(MediaObject {
            sender: me.to_string(),
            kind: TrackKind::Video,
            seq,
            pts: Duration::from_millis(seq * 33),
            payload: nalus,
        });
    }

    /// Screen capture → shared stack (throttle + encode → `SurfaceFrame`) →
    /// publish (started via `share_screen`). The `VideoStreamSender` seam
    /// does the throttling + encode + framing, so a returned `Some` is
    /// already a wire-ready, rate-capped frame.
    #[cfg(feature = "live-media")]
    fn pump_screen(&mut self, me: &str) {
        use crate::media::{MediaObject, TrackKind};
        use std::time::Duration;

        let screen_wire = match (self.screen.as_mut(), self.screen_session.as_mut()) {
            (Some(scr), Some(session)) => scr
                .poll()
                .and_then(|rgba| session.offer_frame(&rgba, std::time::Instant::now())),
            _ => None,
        };
        let Some(wire) = screen_wire else {
            return;
        };
        let seq = self.next_media_seq();
        self.media.transport_mut().send(MediaObject {
            sender: me.to_string(),
            kind: TrackKind::Screen,
            seq,
            pts: Duration::from_millis(seq * 33),
            payload: wire,
        });
    }

    /// Receive: decode + play whatever arrived from peers.
    #[cfg(feature = "live-media")]
    fn pump_receive(&mut self, me: &str) {
        use crate::media::TrackKind;

        let objs = self.media.transport_mut().poll();
        for o in objs {
            if o.sender == me {
                continue;
            }
            match o.kind {
                TrackKind::Audio => {
                    if !self.call.deafened {
                        if self.speaker.is_none() {
                            self.speaker = crate::audio::SpeakerPlayback::new();
                        }
                        if let Some(sp) = self.speaker.as_mut() {
                            sp.play(&o.payload);
                        }
                    }
                }
                TrackKind::Video => {
                    // Remote camera: a raw-NALU H.264 stream decoded through
                    // the shared stack's decoder → the peer's filmstrip cell.
                    let frame = self
                        .remote_decoders
                        .entry(o.sender.clone())
                        .or_default()
                        .decode(&o.payload);
                    if let Some(frame) = frame {
                        self.remote_frames
                            .insert(o.sender.clone(), rgba_to_image(&frame));
                    }
                }
                TrackKind::Screen => {
                    // Remote screen-share: reframe + decode through the shared
                    // stack's receiver (the same `SurfaceFrame` framing the
                    // compositor's PeerView consumes) → the featured stage tile.
                    let frames = self
                        .remote_screen_rx
                        .entry(o.sender.clone())
                        .or_default()
                        .push(&o.payload);
                    if let Some(frame) = frames.last() {
                        self.remote_screens
                            .insert(o.sender.clone(), rgba_to_image(frame));
                    }
                }
            }
        }
    }

    #[cfg(feature = "live-media")]
    fn next_media_seq(&mut self) -> u64 {
        let s = self.media_seq;
        self.media_seq = self.media_seq.wrapping_add(1);
        s
    }

    /// Acquire a screen-share source through the portal seam: check the
    /// `ShareScreen` permission, then ask [`self.portal`](Self) for a source
    /// (the real xdg-desktop-portal `ScreenCast` picker behind `live-media`; an
    /// honest `None` in the default build; an injected fake in tests). On a
    /// grant, open the `PipeWire` capture + a shared-stack video sender and
    /// publish the screen track (behind `live-media`) and return `true`.
    /// Returns `false` — never fabricating a share — when permission is denied
    /// or the portal grants nothing. The caller flips `call.sharing` + the
    /// shared voice membership.
    ///
    /// The source selection stays the xdg-desktop-portal `ScreenCast` picker;
    /// the encode + framing route through [`qos_callstream::VideoStreamSender`]
    /// — the same throttle+encoder+`SurfaceFrame` composition the compositor's
    /// `SharedFlatpakSession` performs — instead of a second in-app codec.
    pub(crate) fn acquire_screen(&mut self) -> bool {
        if !self.call_allows(crate::call::CallAction::ShareScreen) {
            return false;
        }
        let Some(src) = self.portal.request() else {
            return false;
        };
        #[cfg(feature = "live-media")]
        {
            if let Some(scr) = crate::video::ScreenCapture::new(src.node, src.width, src.height) {
                self.screen = Some(scr);
                self.screen_session = qos_callstream::VideoStreamSender::new(
                    src.width,
                    src.height,
                    crate::video::DEFAULT_FPS,
                )
                .ok();
                self.media
                    .transport_mut()
                    .publish(crate::media::TrackKind::Screen);
            }
        }
        let _ = src;
        true
    }

    /// Whether this peer is currently sharing its screen — the shared-truth flag
    /// the featured tile and the Share/Stop control key off. True only after the
    /// portal granted a source (see [`Self::acquire_screen`]).
    pub(crate) fn sharing_screen(&self) -> bool {
        self.call.sharing
    }

    /// Tear down the local screen capture and stop publishing the screen track.
    /// The caller clears `call.sharing` + the shared voice membership.
    pub(crate) fn release_screen(&mut self) {
        #[cfg(feature = "live-media")]
        {
            self.screen = None;
            self.screen_session = None;
            self.media
                .transport_mut()
                .unpublish(crate::media::TrackKind::Screen);
        }
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
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // room's address for itself still wrote its earlier lines under the
        // id it resolved at startup, and those are still its own.
        if self.is_me(id) {
            return "You".to_string();
        }
        let named = self.display_name(id);
        if named != id && !named.trim().is_empty() {
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
        let s = ui::widget::Avatar::initials_for_name(&name);
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

    /// The media routing for the current call, chosen from the live participant
    /// count: peer-to-peer below the mesh threshold, host-relay above it. A
    /// call transparently promotes from P2P to host-based as people join.
    pub(crate) fn call_session(&self) -> crate::model::CallSession {
        crate::model::CallSession::select(self.voice_in(&self.call.channel).len().max(1))
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

    /// Build a composer `TextInput`: multiline (Enter sends, Shift+Enter adds a
    /// newline), plain chrome (the composer draws its own rounded box around the
    /// text), sized to match the surrounding chat body.
    fn new_composer() -> TextInput {
        let mut t = TextInput::local()
            .multiline(true)
            .with_enter_submits(true)
            .with_font_size(15.0);
        t.set_plain_chrome(true);
        t
    }

    /// The composer for the surface the user is typing in.
    pub(crate) fn focused_composer(&self) -> &TextInput {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &self.composer_thread,
            _ => &self.composer_main,
        }
    }

    pub(crate) fn focused_composer_mut(&mut self) -> &mut TextInput {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &mut self.composer_thread,
            _ => &mut self.composer_main,
        }
    }

    /// Point keyboard focus at the composer for the current surface — but only
    /// when no inline editor / rename is capturing input. Idempotent; called
    /// from `reload` and after any focus change.
    pub(crate) fn set_composer_focus(&mut self) {
        let editing = self.editing.is_some() || self.rename.is_some();
        let main = matches!(self.focus, Focus::Main) && !editing;
        let thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some() && !editing;
        ui::Widget::set_focus(&mut self.composer_main, main);
        ui::Widget::set_focus(&mut self.composer_thread, thread);
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
        self.messages = ChatDoc::list_messages(doc, &self.active_channel);
        // Cache voice membership for every voice channel in this workspace.
        self.voice_members.clear();
        for ch in self
            .channels
            .iter()
            .filter(|c| c.kind == model::ChannelKind::Voice)
        {
            let members = ChatDoc::list_voice_members(doc, &ch.id);
            if !members.is_empty() {
                self.voice_members.insert(ch.id.clone(), members);
            }
        }
        // Load the per-surface drafts from the CRDT into the composer widgets.
        // `set_text` is a no-op when the value is unchanged, so this preserves
        // the caret/selection while typing (the draft we just wrote back equals
        // the widget's value) and only reloads when the surface actually
        // changed (a channel/thread switch), where a caret reset is correct.
        self.draft_main = ChatDoc::draft_text(doc, &self.active_channel);
        self.composer_main.set_text(&self.draft_main);
        let ch_name = self
            .channel(&self.active_channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.composer_main
            .set_placeholder(&format!("Message #{ch_name}"));
        self.composer_main
            .set_local_author(Some(self.me.id.clone()));
        self.draft_thread = match self.thread_draft_key() {
            Some(k) => ChatDoc::draft_text(doc, &k),
            None => String::new(),
        };
        self.composer_thread.set_text(&self.draft_thread);
        self.composer_thread.set_placeholder("Reply…");
        self.composer_thread
            .set_local_author(Some(self.me.id.clone()));
        // `--shots` only: stage a mid-compose draft into the main composer so a
        // screenshot shows real text + caret. Never set in a live/interactive
        // run, so this is inert in production.
        if let Some(draft) = self.shot_composer_draft.clone() {
            self.composer_main.set_text(&draft);
            self.composer_main.set_cursor_pos(draft.len());
            self.draft_main = draft;
        }
        self.set_composer_focus();
        // Clamp the caret to whatever text the inline editor currently indexes
        // (message-edit working text, or the rename buffer). The composers own
        // their own caret, so it is not clamped here.
        let max = match &self.editing {
            Some((_, text)) => text.chars().count(),
            None => 0,
        };
        if self.editing.is_some() && self.caret > max {
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

    // --- responsive flap geometry ----------------------------------------
    //
    // Each flap's *physical* pane width is fixed; what changes with the mode
    // is whether the content is inset by it (docked) or whether the pane
    // floats over the content behind a scrim (overlay). The eased `slide`
    // animates the float in/out.

    /// Total width occupied by the left navigation pane (rail + sidebar).
    pub(crate) fn left_pane_w() -> f64 {
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
            FlapMode::Overlay => {
                self.rect.x0 - Self::left_pane_w() * (1.0 - self.left_flap.slide())
            }
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
            self.rect.x0 + Self::left_pane_w()
        } else {
            // Overlay (or hidden docked): content reclaims the pane width;
            // leave room for the floating flap toggle button.
            self.rect.x0
        }
    }
    pub(crate) fn header_rect(&self) -> Rect {
        Rect::new(
            self.center_left(),
            self.rect.y0,
            self.center_right(),
            self.rect.y0 + HEADER_H,
        )
    }
    pub(crate) fn composer_height(&self, tr: &mut TextRenderer) -> f64 {
        let w = (self.center_right() - self.center_left() - 48.0).max(40.0) as f32;
        let (_, h) = tr.measure(self.draft_main.as_str(), 15.0, Some(w));
        (COMPOSER_MIN_H + (f64::from(h) - 19.0).max(0.0)).clamp(COMPOSER_MIN_H, COMPOSER_MAX_H)
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
        Rect::new(
            self.center_left(),
            self.rect.y0 + HEADER_H,
            self.center_right(),
            composer.y0 - 6.0,
        )
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
        if !self.workspaces.iter().any(|w| w.id == self.active_ws)
            && let Some(first) = self.workspaces.first()
        {
            self.active_ws = first.id.clone();
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
        self.images
            .block_until_idle(std::time::Duration::from_secs(3));
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
        if self.is_live {
            // Live instance. `app-state` has already restored the previous
            // session's document by now, so an unseeded document really is a
            // first run: scaffold a real, empty starter workspace for this
            // user — never the demo.
            // Keyed off `is_live` (not on whether anything persisted) so a
            // live run with no data dir still gets the honest empty starter
            // rather than the demo.
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
            ) {}
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
        // Move real media: capture mic/camera/screen → encode → publish, and
        // decode/play whatever arrived from peers. No-op without `live-media`.
        #[cfg(feature = "live-media")]
        self.pump_media();
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
        let video_node = taffy.new_leaf(video_style).unwrap();
        node_map.insert(video_node, &raw mut self.video as *mut dyn Widget);
        self.video_node = Some(video_node);

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
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(vn) = self.video_node {
            // Shift the video by the dragged overlay offset (huddle only; the
            // expanded grid is anchored). The taffy node holds the fractional
            // home; the origin adds the drag delta.
            let off = if self.call.expanded {
                Point::new(self.rect.x0, self.rect.y0)
            } else {
                Point::new(
                    self.rect.x0 + self.call.overlay_offset.x,
                    self.rect.y0 + self.call.overlay_offset.y,
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
        self.draw_composer(scene, tf, tr, theme, presence, &mut hits);

        // Right flap (members / thread). In overlay mode a scrim dims the
        // content behind the floating panel.
        if self.right_mode == FlapMode::Overlay && self.right_flap.reveal() > 0.001 {
            let a = (self.right_flap.reveal() * 0.55) as f32;
            crate::paint::fill_rect(
                scene,
                tf,
                self.rect,
                c.surfaces.overlay.with_alpha_factor(a),
            );
            ChatApp::push_hit(&mut hits, self.rect, Hit::Dismiss);
        }
        if self.right_flap_open() || self.right_flap.reveal() > 0.001 {
            // The right flap is "members / thread": it shows the open thread
            // when one is up, otherwise the workspace members roster (so the
            // "👥 N" pill always reveals a real panel, never a blank column).
            if self.thread_open.is_some() {
                self.draw_thread(scene, tf, tr, theme, presence, &mut hits);
            } else {
                self.draw_members(scene, tf, tr, theme, &mut hits);
            }
        }

        // Left flap (rail + channel sidebar). Overlay scrim first.
        if self.left_mode == FlapMode::Overlay && self.left_flap.reveal() > 0.001 {
            let a = (self.left_flap.reveal() * 0.55) as f32;
            crate::paint::fill_rect(
                scene,
                tf,
                self.rect,
                c.surfaces.overlay.with_alpha_factor(a),
            );
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
        // The host tile's VideoPlayer surface is painted only once a real
        // capture has fed it a frame; otherwise the honest "camera off"
        // placeholder (drawn from live geometry) fully covers the host tile. The
        // guard also avoids the surface node — whose taffy geometry lags a beat
        // behind `self.rect` on a single headless `--shots` frame — painting a
        // stray mark at its stale origin before the placeholder lands.
        let host_has_frame = self.video.latest_frame().is_some();
        if self.call.active && self.call.expanded {
            self.draw_call_overlay_bg(scene, tf, tr, theme);
            if host_has_frame {
                self.video.draw(scene, tf, tr, theme, presence);
            }
            self.draw_host_camera_off(scene, tf, tr, theme);
            self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
        } else if self.call.active {
            self.draw_huddle_bg(scene, tf, tr, theme, &mut hits);
            if host_has_frame {
                self.video.draw(scene, tf, tr, theme, presence);
            }
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
        // Adopt the room's answer for who everybody is. Names and colours are
        // the room's to give: they arrive on the collaboration context stream
        // and land in the presence store, and this is the one door an app has
        // onto that store. Taken before `draw` so the same frame that learns a
        // name paints it.
        self.adopt_room(presence);
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
        self.dispatch_event(event, scale_factor, *modifiers, state, handled);
    }

    /// The resource tree Chat currently has open — the active workspace, its
    /// channels, and the open channel's messages — each exporting the actions
    /// Ctrl+K offers. Delegates to the [`ResourceProvider`](qos_actions::ResourceProvider)
    /// impl in [`crate::actions`], the same builder the affordance covenant tests
    /// drive. Recomputed each frame; empty until a workspace is selected.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing the event —
    /// the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the conversations into the unified Ctrl+K palette: a snapshot
    /// [`ChannelSearchProvider`](crate::search_provider::ChannelSearchProvider)
    /// makes every text channel findable there by name or topic, alongside the
    /// actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::ChannelSearchProvider::new(&self.channels, &self.workspaces),
        )]
    }

    /// Open the conversation a chosen palette row identifies: switch to its
    /// workspace and make it the active channel (the next `reload` loads its
    /// messages). View state is per-peer presentation, so the doc args are
    /// unused — the same fields the sidebar's own channel click sets.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::ChannelSearchProvider::parse_action_id(action_id)
            && let Some(ws) = self.channel(&id).map(|c| c.workspace.clone())
        {
            self.active_ws = ws;
            self.active_channel = id;
            self.thread_open = None;
            self.scroll = 0.0;
            self.focus = Focus::Main;
            self.caret = 0;
        }
    }

    fn name(&self) -> &'static str {
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

/// Wrap a decoded RGBA frame as a `peniko` image for painting into a call tile.
#[cfg(feature = "live-media")]
fn rgba_to_image(frame: &qos_callstream::DecodedFrame) -> vello::peniko::ImageData {
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    ImageData {
        data: Blob::new(std::sync::Arc::new(frame.rgba.clone())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: frame.width,
        height: frame.height,
    }
}

/// Screen-share reuse proof: a remote peer's screen frame, encoded through the
/// shared qOS video-streaming stack, must travel the real receive path into
/// the featured-tile map. Gated on `live-media` because the video stack (and
/// the featured-tile decode) only compile there — the default/`--shots` build
/// carries no codec and shows the honest placeholder.
#[cfg(all(test, feature = "live-media"))]
mod screenshare_reuse_tests {
    use super::*;
    use crate::media::{MediaEngine, MediaObject, TrackKind};
    use qos_callstream::VideoStreamSender;
    use std::time::{Duration, Instant};

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&[
                    ((x * 255) / width.max(1)) as u8,
                    ((y * 255) / height.max(1)) as u8,
                    128,
                    255,
                ]);
            }
        }
        out
    }

    /// A remote screen-share frame encoded on the shared `VideoStreamSender`
    /// (throttle + H.264 + `SurfaceFrame` framing), delivered as a
    /// `TrackKind::Screen` `MediaObject`, must decode through the app's real
    /// `pump_media` receive path and land in `remote_screens` — the featured
    /// stage tile. This asserts on the production receive path (the same
    /// `VideoStreamReceiver` + decoder the app runs), proving a decoded frame
    /// reaches the tile with no second in-app codec.
    #[test]
    fn remote_screen_share_frame_reaches_the_featured_tile() {
        const W: u32 = 96;
        const H: u32 = 64;

        let mut app = ChatApp::new_ephemeral();
        // Deterministic in-process loopback transport; no network/devices.
        app.media = MediaEngine::simulated();
        app.call.active = true;
        app.call.audio_only = true; // no camera capture in the pump
        app.call.muted = true; // no mic capture in the pump

        // A remote peer encodes a screen frame through the SHARED stack.
        let mut sender = VideoStreamSender::new(W, H, 30).expect("shared-stack sender");
        let wire = sender
            .offer_frame(&gradient(W, H), Instant::now())
            .expect("throttle admits the first frame + encoder emits a NALU");
        app.media.transport_mut().send(MediaObject {
            sender: "u_remote".into(),
            kind: TrackKind::Screen,
            seq: 0,
            pts: Duration::from_millis(0),
            payload: wire,
        });

        // Honest gating: nothing in the tile until a real frame is pumped.
        assert!(
            app.remote_screens.is_empty(),
            "no fabricated featured-tile frame before the pump"
        );

        app.pump_media();

        let img = app
            .remote_screens
            .get("u_remote")
            .expect("the decoded screen frame reached the featured tile");
        assert_eq!(
            (img.width, img.height),
            (W, H),
            "featured-tile image carries the decoded frame's dimensions"
        );
    }
}

#[cfg(test)]
mod identity_on_screen {
    use super::*;

    /// Nobody is ever shown the token the system uses to route to them.
    ///
    /// A participant id is `peer@9007495404254172154` — a hash of a public
    /// key. It appeared beside messages and in the roster whenever the room
    /// had not named somebody yet, which is most of the time a peer has just
    /// arrived. It is not a name; it is the thing a name resolves from.
    #[test]
    fn an_unnamed_participant_is_not_shown_their_routing_token() {
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

    /// You have a name, and this process knows it.
    ///
    /// `LocalIdentity::resolve` derives one at startup and puts it on
    /// `me`. `display_name` consulted the room document and the roster
    /// and neither knows you until you have spoken, so the one name that
    /// was certain was the one it fell through on — and a person saw
    /// their own routing hash in the roster and in the `@` picker.
    ///
    /// This is what "referred to as You" rests on: second-person is a
    /// rendering choice, not a substitute for having a name.
    #[test]
    fn your_own_name_is_known_before_the_room_has_heard_of_you() {
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

    /// And you are "You".
    ///
    /// Reading your own id — or your own name — back at you is how a chat
    /// tells you it does not know who you are.
    #[test]
    fn the_person_reading_the_screen_is_you() {
        let app = ChatApp::new_ephemeral();
        let me = app.me.id.clone();
        assert_eq!(app.user_name(&me), "You");
    }
}
