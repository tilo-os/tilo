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

use crate::call::{AllowAll, CallPermissions, CallState, OverlayDrag, SoundEngine};
use crate::doc::ChatDoc;
use crate::media::MediaEngine;
use crate::model::{self, Channel, Id, Message, User, Workspace};
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

// --- Floating call overlay home placement (fractions of the window) -------
// The docked overlay's resting rect before the user drags it, and the
// expanded host-tile rect. The VideoPlayer's taffy node and the surrounding
// chrome derive from these so they stay aligned; the drag offset is applied
// on top in `apply_layout`.
pub(crate) const HUDDLE_VIDEO: (f64, f64, f64, f64) = (0.505, 0.300, 0.250, 0.230);
pub(crate) const OVERLAY_VIDEO: (f64, f64, f64, f64) = (0.075, 0.180, 0.415, 0.330);

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
    /// Open a context menu for a channel/category/workspace target.
    ContextMenu { target: MenuTarget, anchor: Point },
    /// Execute a context-menu / create action.
    MenuAction { target: MenuTarget, action: MenuAction },
    /// The "+" create-channel button on a category header.
    AddChannel { workspace: Id, category: Id },
    /// The "+" create-category button in the sidebar header.
    AddCategory(Id),
    /// Confirm the inline rename editor.
    RenameConfirm,
    Reaction { msg: Id, emoji: String },
    AddReaction { msg: Id, anchor: Point },
    OpenThread(Id),
    EditHistory { msg: Id, anchor: Point },
    EmojiChoice(String),
    FocusMain,
    FocusThread,
    Send,
    ComposerEmoji(Point),
    Attach,
    StartCall,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    CallDeafen,
    CallAudioOnly,
    CallFollow,
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

    // Engines (call sounds + media transport + the call permission seam).
    pub(crate) sound: SoundEngine,
    pub(crate) media: MediaEngine,
    pub(crate) permissions: AllowAll,

    // Persistence (None for ephemeral / `--shots` instances).
    saver: Option<Saver>,

    // Layout + render caches.
    pub(crate) rect: Rect,
    root_node: Option<NodeId>,
    video_node: Option<NodeId>,
    pub(crate) video: VideoPlayer,
    pub(crate) hits: Vec<(Rect, Hit)>,
    pub(crate) last_mouse: Point,
    relayout: bool,
}

impl Default for ChatApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatApp {
    /// The persistent, live instance: loads the saved workspace from disk
    /// (if any) and wires the background saver.
    pub fn new() -> Self {
        let mut app = Self::bare();
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
        let mut video = VideoPlayer::new();
        feed_demo_frame(&mut video);
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
            menu: None,
            rename: None,
            swipe: None,
            call: CallState::default(),
            overlay_drag: None,
            left_flap: FlapState::open_when_docked(),
            right_flap: FlapState::open_when_docked(),
            left_mode: FlapMode::Docked,
            right_mode: FlapMode::Docked,
            sound: SoundEngine::new(),
            media: MediaEngine::new(),
            permissions: AllowAll,
            saver: None,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            relayout: false,
        }
    }

    /// Whether a call action is permitted by the (currently default-allow)
    /// permission seam. The single gate the call UI consults before a
    /// privileged action; swap [`AllowAll`] for the real OS checker later.
    pub(crate) fn call_allows(&self, action: crate::call::CallAction) -> bool {
        self.permissions.allows(action)
    }

    /// Persist the current document. Called after every mutation; a no-op
    /// for ephemeral instances.
    pub(crate) fn persist(&self, doc: &mut AutoCommit) {
        if let Some(saver) = &self.saver {
            saver.request(doc.save());
        }
    }

    /// Stage a named scenario for a headless screenshot. Unknown names leave
    /// the default frame.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "narrow" => {
                // Snap both flaps closed so the single-frame narrow capture
                // shows the overlay-collapsed conversation (not a mid-slide).
                // Modes resolve to overlay at the narrow viewport during layout.
                self.left_flap.snap(false);
                self.right_flap.snap(false);
                self.thread_open = None;
            }
            "context" => {
                // Open a channel context menu for the screenshot.
                self.menu = Some(ContextMenu {
                    target: MenuTarget::Channel("c_frontend".into()),
                    anchor: Point::new(240.0, 230.0),
                    items: MenuAction::for_channel().to_vec(),
                });
            }
            _ => {}
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
        let max = self.focused_draft().chars().count();
        if self.caret > max {
            self.caret = max;
        }
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
        self.left_mode = FlapMode::from_width(w, LEFT_BREAKPOINT);
        self.right_mode = FlapMode::from_width(w, RIGHT_BREAKPOINT);
        self.left_flap.reconcile(self.left_mode);
        self.right_flap.reconcile(self.right_mode);
        let a = self.left_flap.tick(self.left_mode);
        let b = self.right_flap.tick(self.right_mode);
        a || b
    }

    /// The host-tile video rect for the current call mode, including the
    /// dragged overlay offset (docked huddle only — the expanded grid is
    /// anchored). The single source of truth for both the VideoPlayer node
    /// placement and the surrounding call chrome.
    pub(crate) fn call_video_rect(&self) -> Rect {
        if self.call.expanded {
            frac_rect(self.rect, OVERLAY_VIDEO)
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

/// Synthesise one colourful H.264 frame and hand it to the player so the
/// host call tile shows a live-looking surface offline. Silently no-ops if
/// the in-tree openh264 encoder is unavailable — the tile then falls back
/// to the themed letterbox fill.
fn feed_demo_frame(video: &mut VideoPlayer) {
    let _ = (|| -> Option<()> {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;

        let w = 320usize;
        let h = 192usize;
        let (cw, ch) = (w / 2, h / 2);
        let y_len = w * h;
        let uv_len = cw * ch;
        let mut plane = vec![0u8; y_len + 2 * uv_len];

        // Luma: soft vertical gradient + a centred highlight blob to read as
        // a face/light in a dim room.
        for y in 0..h {
            for x in 0..w {
                let base = 46.0 + (y as f32 / h as f32) * 70.0;
                let dx = (x as f32 - w as f32 * 0.5) / (w as f32 * 0.5);
                let dy = (y as f32 - h as f32 * 0.42) / (h as f32 * 0.5);
                let blob = (1.0 - (dx * dx + dy * dy)).max(0.0) * 95.0;
                plane[y * w + x] = (base + blob).clamp(0.0, 255.0) as u8;
            }
        }
        // Chroma: cool indigo/teal sweep.
        for cy in 0..ch {
            for cx in 0..cw {
                let u = 150.0 + (cx as f32 / cw as f32) * 24.0;
                let v = 96.0 + (cy as f32 / ch as f32) * 28.0;
                plane[y_len + cy * cw + cx] = u.clamp(0.0, 255.0) as u8;
                plane[y_len + uv_len + cy * cw + cx] = v.clamp(0.0, 255.0) as u8;
            }
        }

        let yuv = YUVBuffer::from_vec(plane, w, h);
        let mut enc = Encoder::new().ok()?;
        let bitstream = enc.encode(&yuv).ok()?;
        let bytes = bitstream.to_vec();
        if !bytes.is_empty() {
            video.feed_frame(&bytes);
            video.play();
        }
        Some(())
    })();
}

impl Widget for ChatApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        // First-run adoption: load a persisted workspace into the shared doc
        // before seeding, so a restart restores prior messages/structure
        // rather than re-seeding the demo. Only the live (persistent)
        // instance reads disk; ephemeral `--shots` instances seed fresh.
        if self.saver.is_some() && !ChatDoc::is_seeded(doc) {
            if let Some(dir) = storage::data_dir() {
                if let Ok(Some(bytes)) = storage::load_bytes(&dir) {
                    if let Ok(loaded) = AutoCommit::load(&bytes) {
                        let _ = doc.merge(&mut loaded.clone());
                    }
                }
            }
        }
        ChatDoc::seed_if_empty(doc);
        self.reload(doc);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host video tile is an absolute leaf positioned by *fractional*
        // insets against the root, so it sizes correctly even on the first
        // layout pass (when `self.rect` isn't known yet). The dragged overlay
        // offset is applied as an origin shift in `apply_layout`, which keeps
        // the tile correct without a relayout on every drag frame.
        let f = if self.call.expanded { OVERLAY_VIDEO } else { HUDDLE_VIDEO };
        let video_node = taffy
            .new_leaf(Style {
                position: Position::Absolute,
                inset: taffy::Rect {
                    left: percent(f.0 as f32),
                    top: percent(f.1 as f32),
                    right: LengthPercentageAuto::auto(),
                    bottom: LengthPercentageAuto::auto(),
                },
                size: Size { width: percent(f.2 as f32), height: percent(f.3 as f32) },
                ..Default::default()
            })
            .unwrap();
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
            self.draw_thread(scene, tf, tr, theme, &mut hits);
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

        // Call layers.
        if self.call.active && self.call.expanded {
            self.draw_call_overlay_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
            self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
        } else if self.call.active {
            self.draw_huddle_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
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

        self.hits = hits;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
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

    /// Keep redrawing while a call is live (active-speaker pulse / video) or
    /// a flap is mid-slide, so animations are smooth.
    fn needs_animation_frame(&self) -> bool {
        self.call.active
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
