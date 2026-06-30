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

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::VideoPlayer;

use crate::doc::ChatDoc;
use crate::model::{self, Channel, Id, Message, User, Workspace};

// --- Fixed chrome metrics (logical px) ------------------------------------
pub(crate) const RAIL_W: f64 = 68.0;
pub(crate) const SIDE_W: f64 = 248.0;
pub(crate) const HEADER_H: f64 = 60.0;
pub(crate) const THREAD_W: f64 = 360.0;
pub(crate) const COMPOSER_MIN_H: f64 = 76.0;
pub(crate) const COMPOSER_MAX_H: f64 = 200.0;
pub(crate) const AVATAR: f64 = 40.0;

// --- Call video tile placement, as fractions of the window ----------------
// Chosen so the huddle PiP floats over the lower-centre of the timeline and
// the expanded host tile lands in the top-left of a 2x2 grid. Both the
// taffy node (which gives the VideoPlayer its rect) and the surrounding
// panel chrome are derived from these same fractions so they stay aligned.
pub(crate) const HUDDLE_VIDEO: (f64, f64, f64, f64) = (0.505, 0.345, 0.250, 0.250);
pub(crate) const OVERLAY_VIDEO: (f64, f64, f64, f64) = (0.075, 0.180, 0.415, 0.330);

/// Which composer currently owns keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Main,
    Thread,
}

/// Everything the user can click. Recorded during `draw`, resolved on the
/// next mouse-press.
#[derive(Clone, Debug)]
pub(crate) enum Hit {
    Workspace(Id),
    Channel(Id),
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
    CallMute,
    CallAudioOnly,
    CallFollow,
    CallExpand,
    CallLeave,
    Dismiss, // click on a scrim closes the top-most popover
}

pub struct ChatApp {
    // CRDT projection (rebuilt every sync / after a mutation).
    pub(crate) workspaces: Vec<Workspace>,
    pub(crate) channels: Vec<Channel>,
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
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

    // Call state.
    pub(crate) call_active: bool,
    pub(crate) call_expanded: bool,
    pub(crate) call_channel: Id,
    pub(crate) muted: bool,
    pub(crate) audio_only: bool,
    pub(crate) follow_host: bool,

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
    pub fn new() -> Self {
        let mut video = VideoPlayer::new();
        feed_demo_frame(&mut video);
        Self {
            workspaces: Vec::new(),
            channels: Vec::new(),
            users: Vec::new(),
            messages: Vec::new(),
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
            call_active: true,
            call_expanded: false,
            call_channel: "c_huddle".into(),
            muted: false,
            audio_only: false,
            follow_host: true,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            relayout: false,
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

    // --- region geometry --------------------------------------------------
    pub(crate) fn rail_rect(&self) -> Rect {
        Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + RAIL_W, self.rect.y1)
    }
    pub(crate) fn sidebar_rect(&self) -> Rect {
        let x0 = self.rect.x0 + RAIL_W;
        Rect::new(x0, self.rect.y0, x0 + SIDE_W, self.rect.y1)
    }
    /// Right edge of the centre column (shrinks when a thread is open).
    pub(crate) fn center_right(&self) -> f64 {
        if self.thread_open.is_some() {
            self.rect.x1 - THREAD_W
        } else {
            self.rect.x1
        }
    }
    pub(crate) fn center_left(&self) -> f64 {
        self.rect.x0 + RAIL_W + SIDE_W
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
        Rect::new(self.rect.x1 - THREAD_W, self.rect.y0, self.rect.x1, self.rect.y1)
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
        ChatDoc::seed_if_empty(doc);
        self.reload(doc);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let f = if self.call_expanded { OVERLAY_VIDEO } else { HUDDLE_VIDEO };
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
            self.video.apply_layout(taffy, vn, Point::new(self.rect.x0, self.rect.y0));
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
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        // Base canvas.
        crate::paint::fill_rect(scene, tf, self.rect, theme.tokens.colors.surfaces.base);

        self.draw_rail(scene, tf, tr, theme, &mut hits);
        self.draw_sidebar(scene, tf, tr, theme, &mut hits);
        self.draw_header(scene, tf, tr, theme, &mut hits);

        let msg_area = self.messages_rect(tr);
        crate::paint::push_clip(scene, tf, msg_area);
        self.draw_messages(scene, tf, tr, theme, msg_area, &mut hits);
        scene.pop_layer();

        self.draw_composer(scene, tf, tr, theme, &mut hits);

        if self.thread_open.is_some() {
            self.draw_thread(scene, tf, tr, theme, &mut hits);
        }

        // Call layers.
        if self.call_active && self.call_expanded {
            self.draw_call_overlay_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
            self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
        } else if self.call_active {
            self.draw_huddle_bg(scene, tf, tr, theme, &mut hits);
            self.video.draw(scene, tf, tr, theme, presence);
            self.draw_huddle_fg(scene, tf, tr, theme, &mut hits);
        }

        // Top-most popovers.
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

    fn needs_redraw(&self) -> bool {
        true
    }
}

impl ChatApp {
    /// Flag a taffy rebuild (the absolute video node's fractions changed).
    pub(crate) fn request_relayout(&mut self) {
        self.relayout = true;
    }
}
