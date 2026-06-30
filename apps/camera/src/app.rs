//! Root widget for the Camera app.
//!
//! A canvas-heavy app: rather than composing dozens of taffy nodes, the
//! root occupies a single layout node and paints every screen (Camera,
//! Scanner, Gallery, Settings) straight into the Vello scene, hit-testing
//! its own controls. All persisted state lives in the CRDT (see
//! [`crate::doc`]); the widget holds only render caches + click regions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Image, ImageFit, Widget};
use winit::event::WindowEvent;

use crate::doc;
use crate::frame::{self, FrameBuf};
use crate::model::*;
use crate::paint as p;
use crate::scan;

const NAV_W: f64 = 78.0;
const SCAN_PAYLOAD: &str = "WIFI:T:WPA2;S:qOS Lounge;P:welcome-aboard;H:false;;";
const SHARE_WIFI: &str = "WIFI:T:WPA2;S:qOS-Studio;P:spatial-os;H:false;;";

fn white() -> Color {
    Color::from_rgb8(255, 255, 255)
}
fn shrink(r: Rect, d: f64) -> Rect {
    Rect::new(r.x0 + d, r.y0 + d, r.x1 - d, r.y1 - d)
}

#[derive(Clone, Debug)]
enum Action {
    Nav(Screen),
    Shutter,
    CycleFacing,
    SetMode(CaptureMode),
    SetFilter(GalleryFilter),
    SelectCapture(String),
    ToggleFavSelected,
    DeleteSelected,
    DismissScan,
    ScanAction(usize),
    ShareSelected,
    CycleResolution,
    CycleFormat,
    SetTimer(Timer),
    ToggleGrid,
    ToggleLevel,
    ToggleMirror,
    ToggleGeotag,
    ToggleHdr,
    CycleDefaultFacing,
}

pub struct CameraApp {
    rect: Rect,
    node: Option<NodeId>,

    // CRDT-backed caches (refreshed every sync)
    screen: Screen,
    mode: CaptureMode,
    facing: Facing,
    settings: Settings,
    captures: Vec<Capture>,

    // ephemeral UI state
    filter: GalleryFilter,
    selected_capture: Option<String>,
    scan_dismissed: bool,
    scanned: Option<ScannedCode>,
    scan_gen: (u32, u32),
    toast: Option<String>,
    last_mouse: Point,

    // render caches
    cam_view: Option<(u32, u32, u8, Image)>,
    scan_view: Option<(u32, u32, Image)>,
    thumbs: HashMap<String, Image>,
    share_qr: Option<scan::QrMatrix>,

    regions: Vec<(Rect, Action)>,
    dirty: bool,
    seeded: bool,
}

impl Default for CameraApp {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            node: None,
            screen: Screen::Scanner,
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            captures: Vec::new(),
            filter: GalleryFilter::All,
            selected_capture: None,
            scan_dismissed: false,
            scanned: None,
            scan_gen: (0, 0),
            toast: None,
            last_mouse: Point::ZERO,
            cam_view: None,
            scan_view: None,
            thumbs: HashMap::new(),
            share_qr: None,
            regions: Vec::new(),
            dirty: true,
            seeded: false,
        }
    }

    fn viewfinder_seed(facing: Facing) -> u8 {
        match facing {
            Facing::Back => 0,
            Facing::Front => 3,
            Facing::Wide => 2,
        }
    }

    fn push_region(&mut self, rect: Rect, action: Action) {
        self.regions.push((rect, action));
    }

    fn apply(&mut self, action: Action, doc: &mut AutoCommit, root: &ObjId) {
        self.dirty = true;
        match action {
            Action::Nav(s) => {
                self.screen = s;
                doc::write_screen(doc, root, s);
            }
            Action::Shutter => {
                let (w, h) = self.settings.resolution.dims();
                let kind = match self.mode {
                    CaptureMode::Photo => CaptureKind::Photo,
                    CaptureMode::Video => CaptureKind::Video,
                };
                let cap = Capture {
                    id: format!("cap-{}", now_secs()),
                    kind,
                    ts: now_secs(),
                    w,
                    h,
                    dur_ms: if matches!(kind, CaptureKind::Video) { 7400 } else { 0 },
                    label: if matches!(kind, CaptureKind::Video) { "New clip".into() } else { "New photo".into() },
                    favorite: false,
                    facing: self.facing,
                    scene: Self::viewfinder_seed(self.facing),
                };
                doc::push_capture(doc, root, &cap);
                self.captures.insert(0, cap);
                self.toast = Some("Saved to gallery".into());
            }
            Action::CycleFacing => {
                self.facing = self.facing.next();
                doc::write_facing(doc, root, self.facing);
                self.cam_view = None;
            }
            Action::SetMode(m) => {
                self.mode = m;
                doc::write_mode(doc, root, m);
            }
            Action::SetFilter(f) => self.filter = f,
            Action::SelectCapture(id) => self.selected_capture = Some(id),
            Action::ToggleFavSelected => {
                if let Some(id) = self.selected_capture.clone() {
                    doc::toggle_favorite(doc, root, &id);
                    if let Some(c) = self.captures.iter_mut().find(|c| c.id == id) {
                        c.favorite = !c.favorite;
                    }
                }
            }
            Action::DeleteSelected => {
                if let Some(id) = self.selected_capture.clone() {
                    doc::delete_capture(doc, root, &id);
                    self.captures.retain(|c| c.id != id);
                    self.thumbs.remove(&id);
                    self.selected_capture = self.captures.first().map(|c| c.id.clone());
                }
            }
            Action::DismissScan => self.scan_dismissed = true,
            Action::ShareSelected => {
                self.toast = Some("Sharing link copied".into());
            }
            Action::ScanAction(i) => {
                if let Some(s) = self.scanned.clone() {
                    let label = s.kind.actions().get(i).copied().unwrap_or("Done");
                    self.toast = Some(format!("{label}  ·  {}", s.kind.title()));
                    doc::record_scan(doc, root, &s.raw, now_secs());
                }
            }
            Action::CycleResolution => {
                self.settings.resolution = cycle(&Resolution::ALL, self.settings.resolution);
                doc::write_settings(doc, root, &self.settings);
            }
            Action::CycleFormat => {
                self.settings.photo_format = cycle(&PhotoFormat::ALL, self.settings.photo_format);
                doc::write_settings(doc, root, &self.settings);
            }
            Action::SetTimer(tm) => {
                self.settings.timer = tm;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleGrid => {
                self.settings.grid = !self.settings.grid;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleLevel => {
                self.settings.level = !self.settings.level;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleMirror => {
                self.settings.mirror_front = !self.settings.mirror_front;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleGeotag => {
                self.settings.geotag = !self.settings.geotag;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::ToggleHdr => {
                self.settings.hdr = !self.settings.hdr;
                doc::write_settings(doc, root, &self.settings);
            }
            Action::CycleDefaultFacing => {
                self.settings.default_facing = self.settings.default_facing.next();
                doc::write_settings(doc, root, &self.settings);
            }
        }
    }

    fn hit(&mut self, pos: Point, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let found = self.regions.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, a)| a.clone());
        if let Some(action) = found {
            self.apply(action, doc, root);
            true
        } else {
            false
        }
    }

    /// Captures passing the active gallery filter.
    fn filtered(&self) -> Vec<Capture> {
        self.captures.iter().filter(|c| self.filter.matches(c)).cloned().collect()
    }
}

fn cycle<T: Copy + PartialEq>(all: &[T], cur: T) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0);
    all[(i + 1) % all.len()]
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn ago(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn frame_image(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Fill)
}

fn sample_captures() -> Vec<Capture> {
    let now = now_secs();
    let mk = |id: &str, kind: CaptureKind, off: i64, scene: u8, fav: bool, facing: Facing, label: &str, dur: i64| Capture {
        id: id.into(),
        kind,
        ts: now - off,
        w: 4032,
        h: 3024,
        dur_ms: dur,
        label: label.into(),
        favorite: fav,
        facing,
        scene,
    };
    use CaptureKind::*;
    use Facing::*;
    vec![
        mk("c1", Photo, 120, 1, true, Back, "Golden hour", 0),
        mk("c2", Photo, 1100, 2, false, Back, "City lights", 0),
        mk("c3", Video, 4200, 0, false, Back, "Coast drive", 7400),
        mk("c4", Photo, 9000, 3, true, Front, "Portrait", 0),
        mk("c5", Photo, 26000, 4, false, Wide, "Forest trail", 0),
        mk("c6", Photo, 90000, 5, false, Back, "Desert dunes", 0),
        mk("c7", Video, 180000, 2, false, Back, "Night market", 12300),
        mk("c8", Photo, 360000, 0, true, Back, "Mountain ridge", 0),
        mk("c9", Photo, 520000, 4, false, Wide, "Meadow", 0),
    ]
}

impl Widget for CameraApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            doc::seed(state, path, &sample_captures(), Screen::Scanner);
            self.seeded = true;
        }
        self.screen = doc::read_screen(state, path);
        // Dev/screenshot hook: force a screen without driving the nav.
        if let Ok(s) = std::env::var("CAMERA_SCREEN") {
            if let Some(sc) = Screen::from_str(&s) {
                self.screen = sc;
            }
        }
        self.mode = doc::read_mode(state, path);
        self.facing = doc::read_facing(state, path);
        self.settings = doc::read_settings(state, path);
        self.captures = doc::read_captures(state, path);
        if self.selected_capture.is_none() {
            self.selected_capture = self.captures.first().map(|c| c.id.clone());
        }
        if self.share_qr.is_none() {
            self.share_qr = scan::encode(SHARE_WIFI);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        self.regions.clear();
        p::fill_rect(scene, t, self.rect, theme.tokens.colors.surfaces.base);
        let content = Rect::new(self.rect.x0 + NAV_W, self.rect.y0, self.rect.x1, self.rect.y1);
        self.draw_nav(scene, t, tr, theme);
        match self.screen {
            Screen::Camera => self.draw_camera(scene, t, tr, theme, content),
            Screen::Scanner => self.draw_scanner(scene, t, tr, theme, content),
            Screen::Gallery => self.draw_gallery(scene, t, tr, theme, content),
            Screen::Settings => self.draw_settings(scene, t, tr, theme, content),
        }
        self.draw_toast(scene, t, tr, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.hit(self.last_mouse, state, path) {
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => {
                let act = match logical_key {
                    Key::Character(c) => match c.as_str() {
                        "1" => Some(Action::Nav(Screen::Camera)),
                        "2" => Some(Action::Nav(Screen::Scanner)),
                        "3" => Some(Action::Nav(Screen::Gallery)),
                        "4" => Some(Action::Nav(Screen::Settings)),
                        "g" | "G" => Some(Action::ToggleGrid),
                        "f" | "F" => Some(Action::CycleFacing),
                        "v" | "V" => Some(Action::SetMode(CaptureMode::Video)),
                        "p" | "P" => Some(Action::SetMode(CaptureMode::Photo)),
                        " " if matches!(self.screen, Screen::Camera) => Some(Action::Shutter),
                        _ => None,
                    },
                    Key::Named(NamedKey::Space) if matches!(self.screen, Screen::Camera) => Some(Action::Shutter),
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Scanner) => Some(Action::DismissScan),
                    _ => None,
                };
                if let Some(a) = act {
                    self.apply(a, state, path);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "CameraApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("camera_app")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, t, tr, theme, presence);
    }
}

// ====================================================================
// Shared chrome + control helpers
// ====================================================================

impl CameraApp {
    fn draw_nav(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let rail = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + NAV_W, self.rect.y1);
        p::fill_rect(scene, t, rail, c.surfaces.raised);
        p::line(scene, t, rail.x1, rail.y0, rail.x1, rail.y1, p::alpha(c.border_standard, 0.6), 1.0);

        // Wordmark lens.
        let lx = rail.center().x;
        let ly = rail.y0 + 32.0;
        p::circle(scene, t, lx, ly, 14.0, theme.accent_primary);
        p::circle_stroke(scene, t, lx, ly, 14.0, p::alpha(white(), 0.85), 2.0);
        p::circle(scene, t, lx, ly, 5.0, p::alpha(white(), 0.92));

        let items = [
            (Screen::Camera, "camera"),
            (Screen::Scanner, "scan"),
            (Screen::Gallery, "gallery"),
            (Screen::Settings, "settings"),
        ];
        let mut y = rail.y0 + 70.0;
        let item_h = 74.0;
        for (s, icon) in items {
            let cell = Rect::new(rail.x0 + 8.0, y, rail.x1 - 8.0, y + item_h - 8.0);
            let active = self.screen == s;
            if active {
                p::fill_round(scene, t, cell, 16.0, p::alpha(theme.accent_primary, 0.16));
                let bar = Rect::new(rail.x0, cell.y0 + 14.0, rail.x0 + 4.0, cell.y1 - 14.0);
                p::fill_round(scene, t, bar, 2.0, theme.accent_primary);
            }
            let col = if active { theme.accent_primary } else { p::alpha(c.text_primary, 0.62) };
            let icon_rect = Rect::new(cell.center().x - 15.0, cell.y0 + 11.0, cell.center().x + 15.0, cell.y0 + 41.0);
            p::icon(scene, t, icon, icon_rect, col, 2.0);
            p::text_center(scene, tr, cell.center().x, cell.y0 + 46.0, s.label(), 11.0, col);
            self.push_region(cell, Action::Nav(s));
            y += item_h;
        }

        let by = rail.y1 - 30.0;
        p::circle(scene, t, rail.center().x, by, 12.0, p::alpha(theme.status_success, 0.95));
        p::icon(scene, t, "person", Rect::new(rail.center().x - 8.0, by - 8.0, rail.center().x + 8.0, by + 8.0), white(), 1.6);
    }

    fn segmented(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, items: Vec<(String, bool, Action)>) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, rect, rect.height() / 2.0, p::alpha(c.neutral[2], 0.92));
        p::stroke_round(scene, t, rect, rect.height() / 2.0, p::alpha(c.border_standard, 0.5), 1.0);
        let n = items.len().max(1);
        let seg_w = rect.width() / n as f64;
        for (i, (label, selected, action)) in items.into_iter().enumerate() {
            let seg = Rect::new(rect.x0 + i as f64 * seg_w, rect.y0, rect.x0 + (i as f64 + 1.0) * seg_w, rect.y1);
            if selected {
                p::fill_round(scene, t, shrink(seg, 3.0), (seg.height() - 6.0) / 2.0, theme.accent_primary);
            }
            let col = if selected { white() } else { p::alpha(c.text_primary, 0.78) };
            p::text_in(scene, tr, seg, &label, 13.0, col);
            self.push_region(seg, action);
        }
    }

    fn toggle_switch(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, on: bool, action: Action) {
        let c = &theme.tokens.colors;
        let track = if on { theme.accent_primary } else { p::alpha(c.neutral[6], 0.9) };
        p::fill_round(scene, t, rect, rect.height() / 2.0, track);
        let r = rect.height() / 2.0 - 3.0;
        let kx = if on { rect.x1 - r - 3.0 } else { rect.x0 + r + 3.0 };
        p::circle(scene, t, kx, rect.center().y, r, white());
        self.push_region(rect, action);
    }

    /// A small status/info chip with optional icon. Returns the x just past it.
    fn chip(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, x: f64, y: f64, icon: Option<&str>, label: &str, bg: Color, fg: Color) -> f64 {
        let pad = 9.0;
        let icon_w = if icon.is_some() { 16.0 } else { 0.0 };
        let tw = p::text_w(tr, label, 12.0) as f64;
        let w = pad * 2.0 + icon_w + tw;
        let h = 26.0;
        let r = Rect::new(x, y, x + w, y + h);
        p::fill_round(scene, t, r, h / 2.0, bg);
        let mut cx = x + pad;
        if let Some(ic) = icon {
            p::icon(scene, t, ic, Rect::new(cx, y + 5.0, cx + 16.0, y + 21.0), fg, 1.6);
            cx += icon_w + 3.0;
        }
        p::text(scene, tr, cx, y + 6.0, label, 12.0, fg);
        x + w + 8.0
    }

    fn draw_toast(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(msg) = self.toast.clone() else { return };
        let c = &theme.tokens.colors;
        let tw = p::text_w(tr, &msg, 13.0) as f64;
        let w = tw + 56.0;
        let cx = self.rect.x0 + NAV_W + (self.rect.x1 - self.rect.x0 - NAV_W) / 2.0;
        let r = Rect::new(cx - w / 2.0, self.rect.y1 - 64.0, cx + w / 2.0, self.rect.y1 - 28.0);
        p::fill_round(scene, t, r, 18.0, p::alpha(c.neutral[1], 0.96));
        p::stroke_round(scene, t, r, 18.0, p::alpha(theme.accent_primary, 0.5), 1.0);
        p::icon(scene, t, "check", Rect::new(r.x0 + 14.0, r.center().y - 8.0, r.x0 + 30.0, r.center().y + 8.0), theme.status_success, 2.0);
        p::text(scene, tr, r.x0 + 36.0, r.center().y - 8.0, &msg, 13.0, c.text_primary);
    }
}

// ====================================================================
// Camera screen
// ====================================================================

impl CameraApp {
    fn draw_camera(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let pad = 22.0;

        // Mode bar (Photo / Video) centered up top.
        let mb_w = 210.0;
        let mb = Rect::new(content.center().x - mb_w / 2.0, content.y0 + 16.0, content.center().x + mb_w / 2.0, content.y0 + 52.0);
        self.segmented(
            scene,
            t,
            tr,
            theme,
            mb,
            vec![
                ("Photo".into(), matches!(self.mode, CaptureMode::Photo), Action::SetMode(CaptureMode::Photo)),
                ("Video".into(), matches!(self.mode, CaptureMode::Video), Action::SetMode(CaptureMode::Video)),
            ],
        );

        // Viewfinder.
        let bar_h = 138.0;
        let vf = Rect::new(content.x0 + pad, mb.y1 + 14.0, content.x1 - pad, content.y1 - bar_h);
        let vw = vf.width().max(2.0) as u32;
        let vh = vf.height().max(2.0) as u32;
        let seed = Self::viewfinder_seed(self.facing);
        let need = self.cam_view.as_ref().map(|(w, h, s, _)| (*w, *h, *s) != (vw, vh, seed)).unwrap_or(true);
        if need {
            self.cam_view = Some((vw, vh, seed, frame_image(frame::viewfinder(seed, vw, vh))));
        }
        p::push_clip_round(scene, t, vf, 20.0);
        if let Some((_, _, _, img)) = self.cam_view.as_mut() {
            img.paint_at(scene, t, vf);
        }
        // Rule-of-thirds grid.
        if self.settings.grid {
            let gc = p::alpha(white(), 0.28);
            for k in 1..3 {
                let x = vf.x0 + vf.width() * k as f64 / 3.0;
                p::line(scene, t, x, vf.y0, x, vf.y1, gc, 1.0);
                let y = vf.y0 + vf.height() * k as f64 / 3.0;
                p::line(scene, t, vf.x0, y, vf.x1, y, gc, 1.0);
            }
        }
        // Level horizon indicator.
        if self.settings.level {
            let y = vf.center().y;
            p::line(scene, t, vf.center().x - 60.0, y, vf.center().x + 60.0, y, p::alpha(theme.status_success, 0.9), 2.0);
            p::line(scene, t, vf.center().x - 90.0, y, vf.center().x - 70.0, y, p::alpha(white(), 0.6), 2.0);
            p::line(scene, t, vf.center().x + 70.0, y, vf.center().x + 90.0, y, p::alpha(white(), 0.6), 2.0);
        }
        // Focus reticle.
        let rs = 46.0;
        let fc = Point::new(vf.center().x, vf.center().y);
        let ret = Rect::new(fc.x - rs, fc.y - rs, fc.x + rs, fc.y + rs);
        p::stroke_round(scene, t, ret, 8.0, p::alpha(theme.accent_primary, 0.95), 1.6);
        p::line(scene, t, fc.x - 6.0, fc.y, fc.x + 6.0, fc.y, p::alpha(theme.accent_primary, 0.9), 1.4);
        p::line(scene, t, fc.x, fc.y - 6.0, fc.x, fc.y + 6.0, p::alpha(theme.accent_primary, 0.9), 1.4);
        p::pop(scene);

        // Top-left chips over the viewfinder.
        let chx = vf.x0 + 14.0;
        let chy = vf.y0 + 14.0;
        let bg = p::alpha(Color::from_rgb8(0, 0, 0), 0.42);
        let mut x = chx;
        x = self.chip(scene, t, tr, x, chy, Some("camera"), self.settings.resolution.label(), bg, white());
        if self.settings.hdr {
            x = self.chip(scene, t, tr, x, chy, None, "HDR", bg, p::alpha(theme.tokens.colors.semantic.warning, 1.0));
        }
        if self.settings.timer != Timer::Off {
            x = self.chip(scene, t, tr, x, chy, Some("timer"), self.settings.timer.label(), bg, white());
        }
        let _ = x;
        // Facing chip top-right.
        let fl = self.facing.short();
        let ftw = p::text_w(tr, fl, 12.0) as f64 + 22.0;
        self.chip(scene, t, tr, vf.x1 - ftw - 14.0, chy, None, fl, bg, white());

        // Control bar.
        let bar = Rect::new(content.x0, content.y1 - bar_h, content.x1, content.y1);
        let cy = bar.center().y + 4.0;

        // Last-capture thumbnail (left) — opens the gallery.
        let thumb = Rect::new(bar.x0 + 36.0, cy - 28.0, bar.x0 + 92.0, cy + 28.0);
        if let Some(cap) = self.captures.first().cloned() {
            self.ensure_thumb(&cap, 112, 112);
            p::push_clip_round(scene, t, thumb, 12.0);
            if let Some(img) = self.thumbs.get_mut(&cap.id) {
                img.paint_at(scene, t, thumb);
            }
            p::pop(scene);
            p::stroke_round(scene, t, thumb, 12.0, p::alpha(white(), 0.5), 1.5);
            // count badge
            let n = self.captures.len();
            let bw = 24.0;
            let badge = Rect::new(thumb.x1 - bw + 8.0, thumb.y0 - 8.0, thumb.x1 + 8.0, thumb.y0 + 14.0);
            p::fill_round(scene, t, badge, 11.0, theme.accent_primary);
            p::text_in(scene, tr, badge, &n.to_string(), 11.0, white());
        } else {
            p::stroke_round(scene, t, thumb, 12.0, p::alpha(c.border_standard, 0.8), 1.5);
        }
        p::text_center(scene, tr, thumb.center().x, thumb.y1 + 8.0, "Library", 10.0, p::alpha(c.text_primary, 0.6));
        self.push_region(thumb, Action::Nav(Screen::Gallery));

        // Shutter (center).
        let sc = Point::new(content.center().x, cy);
        let outer = 35.0;
        p::circle_stroke(scene, t, sc.x, sc.y, outer, p::alpha(white(), 0.92), 4.0);
        match self.mode {
            CaptureMode::Photo => p::circle(scene, t, sc.x, sc.y, outer - 8.0, white()),
            CaptureMode::Video => {
                let r = Rect::new(sc.x - 15.0, sc.y - 15.0, sc.x + 15.0, sc.y + 15.0);
                p::fill_round(scene, t, r, 7.0, theme.tokens.colors.semantic.danger);
            }
        }
        self.push_region(Rect::new(sc.x - outer, sc.y - outer, sc.x + outer, sc.y + outer), Action::Shutter);

        // Facing switch (right).
        let fb = Point::new(content.x1 - 64.0, cy);
        p::circle(scene, t, fb.x, fb.y, 26.0, p::alpha(c.neutral[3], 0.95));
        p::circle_stroke(scene, t, fb.x, fb.y, 26.0, p::alpha(c.border_standard, 0.8), 1.0);
        p::icon(scene, t, "switch", Rect::new(fb.x - 13.0, fb.y - 13.0, fb.x + 13.0, fb.y + 13.0), c.text_primary, 2.0);
        p::text_center(scene, tr, fb.x, fb.y + 30.0, self.facing.label(), 10.0, p::alpha(c.text_primary, 0.6));
        self.push_region(Rect::new(fb.x - 26.0, fb.y - 26.0, fb.x + 26.0, fb.y + 26.0), Action::CycleFacing);
    }

    fn ensure_thumb(&mut self, cap: &Capture, w: u32, h: u32) {
        if !self.thumbs.contains_key(&cap.id) {
            let img = frame_image(frame::thumbnail(cap.scene, w, h));
            self.thumbs.insert(cap.id.clone(), img);
        }
    }
}

// ====================================================================
// Scanner screen
// ====================================================================

impl CameraApp {
    fn draw_scanner(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let pad = 22.0;
        let panel_w = 318.0;

        p::text(scene, tr, content.x0 + pad, content.y0 + 16.0, "Scan a code", 19.0, c.text_primary);
        p::text(scene, tr, content.x0 + pad, content.y0 + 42.0, "Point at any QR or barcode — detection is automatic", 12.5, c.text_dim);

        let vf = Rect::new(content.x0 + pad, content.y0 + 70.0, content.x1 - pad - panel_w - 16.0, content.y1 - pad);
        let panel = Rect::new(vf.x1 + 16.0, vf.y0, content.x1 - pad, vf.y1);

        // Generate (and decode) the viewfinder frame at exact pixel size.
        let vw = vf.width().max(2.0) as u32;
        let vh = vf.height().max(2.0) as u32;
        if self.scan_view.as_ref().map(|(w, h, _)| (*w, *h) != (vw, vh)).unwrap_or(true) {
            let (buf, scanned) = frame::scanner_frame(4, vw, vh, SCAN_PAYLOAD);
            self.scan_view = Some((vw, vh, frame_image(buf)));
            self.scanned = scanned;
            self.scan_gen = (vw, vh);
        }

        p::push_clip_round(scene, t, vf, 20.0);
        if let Some((_, _, img)) = self.scan_view.as_mut() {
            img.paint_at(scene, t, vf);
        }

        // Reticle window (centered square).
        let side = vf.width().min(vf.height()) * 0.6;
        let win = Rect::new(vf.center().x - side / 2.0, vf.center().y - side / 2.0, vf.center().x + side / 2.0, vf.center().y + side / 2.0);
        // Darken everything outside the reticle window.
        let dark = p::alpha(Color::from_rgb8(0, 0, 0), 0.5);
        p::fill_rect(scene, t, Rect::new(vf.x0, vf.y0, vf.x1, win.y0), dark);
        p::fill_rect(scene, t, Rect::new(vf.x0, win.y1, vf.x1, vf.y1), dark);
        p::fill_rect(scene, t, Rect::new(vf.x0, win.y0, win.x0, win.y1), dark);
        p::fill_rect(scene, t, Rect::new(win.x1, win.y0, vf.x1, win.y1), dark);
        // Reticle corner brackets.
        let blen = 26.0;
        let acc = theme.accent_primary;
        for &(sx, sy) in &[(-1.0_f64, -1.0_f64), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let ox = if sx < 0.0 { win.x0 } else { win.x1 };
            let oy = if sy < 0.0 { win.y0 } else { win.y1 };
            p::line(scene, t, ox, oy, ox - sx * blen, oy, acc, 3.0);
            p::line(scene, t, ox, oy, ox, oy - sy * blen, acc, 3.0);
        }
        // Scan sweep line.
        p::line(scene, t, win.x0 + 8.0, win.center().y, win.x1 - 8.0, win.center().y, p::alpha(acc, 0.7), 2.0);

        // Detected bounding box (mapped from decode corners).
        if let Some(s) = self.scanned.clone() {
            let (gw, gh) = self.scan_gen;
            let sx = vf.width() / gw.max(1) as f64;
            let sy = vf.height() / gh.max(1) as f64;
            let pt = |(x, y): (f32, f32)| Point::new(vf.x0 + x as f64 * sx, vf.y0 + y as f64 * sy);
            let mut bez = BezPath::new();
            let pts: Vec<Point> = s.corners.iter().map(|c| pt(*c)).collect();
            bez.move_to(pts[0]);
            for q in &pts[1..] {
                bez.line_to(*q);
            }
            bez.close_path();
            let green = theme.status_success;
            scene.fill(vello::peniko::Fill::NonZero, t, p::alpha(green, 0.16), None, &bez);
            scene.stroke(&vello::kurbo::Stroke::new(3.0), t, green, None, &bez);
            for q in &pts {
                p::circle(scene, t, q.x, q.y, 4.0, green);
            }
            // Symbology tag above the box.
            let cxy = s.center();
            let tag = format!("{}  detected", s.symbology);
            let tw = p::text_w(tr, &tag, 11.0) as f64 + 18.0;
            let tagx = (vf.x0 + cxy.0 as f64 * sx) - tw / 2.0;
            let top = pts.iter().map(|q| q.y).fold(f64::MAX, f64::min);
            let tagr = Rect::new(tagx, top - 26.0, tagx + tw, top - 6.0);
            p::fill_round(scene, t, tagr, 10.0, green);
            p::text_in(scene, tr, tagr, &tag, 11.0, white());
        }
        p::pop(scene);

        // Floating result card.
        if let Some(s) = self.scanned.clone() {
            if !self.scan_dismissed {
                self.draw_scan_card(scene, t, tr, theme, vf, &s);
            }
        }

        // Right info panel.
        self.draw_scan_panel(scene, t, tr, theme, panel);
    }

    fn draw_scan_card(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, vf: Rect, s: &ScannedCode) {
        let c = &theme.tokens.colors;
        let cw = vf.width().min(560.0) - 36.0;
        let ch = 122.0;
        let card = Rect::new(vf.center().x - cw / 2.0, vf.y1 - ch - 18.0, vf.center().x + cw / 2.0, vf.y1 - 18.0);
        p::fill_round(scene, t, card, 16.0, p::alpha(c.neutral[1], 0.97));
        p::stroke_round(scene, t, card, 16.0, p::alpha(c.border_standard, 0.6), 1.0);

        // Type icon badge.
        let icon_name = match &s.kind {
            CodeKind::Url => "link",
            CodeKind::Wifi { .. } => "wifi",
            CodeKind::Contact { .. } => "person",
            CodeKind::Email => "copy",
            CodeKind::Phone => "phone",
            CodeKind::Geo => "map",
            CodeKind::Text => "search",
        };
        let badge = Rect::new(card.x0 + 16.0, card.y0 + 16.0, card.x0 + 60.0, card.y0 + 60.0);
        p::fill_round(scene, t, badge, 12.0, p::alpha(theme.accent_primary, 0.18));
        p::icon(scene, t, icon_name, shrink(badge, 11.0), theme.accent_primary, 2.0);

        let tx = badge.x1 + 14.0;
        p::text(scene, tr, tx, card.y0 + 16.0, s.kind.title(), 15.0, c.text_primary);
        let summ = s.kind.summary(&s.raw);
        let summ = ellipsize(tr, &summ, 13.0, card.x1 - tx - 40.0);
        p::text(scene, tr, tx, card.y0 + 39.0, &summ, 13.0, c.text_dim);

        // Dismiss X.
        let xr = Rect::new(card.x1 - 34.0, card.y0 + 12.0, card.x1 - 12.0, card.y0 + 34.0);
        p::icon(scene, t, "close", xr, p::alpha(c.text_primary, 0.6), 1.8);
        self.push_region(xr, Action::DismissScan);

        // Action buttons row.
        let actions = s.kind.actions();
        let mut bx = card.x0 + 16.0;
        let by = card.y1 - 42.0;
        for (i, label) in actions.iter().enumerate() {
            let tw = p::text_w(tr, label, 13.0) as f64;
            let bw = tw + 30.0;
            let br = Rect::new(bx, by, bx + bw, by + 30.0);
            if i == 0 {
                p::fill_round(scene, t, br, 15.0, theme.accent_primary);
                p::text_in(scene, tr, br, label, 13.0, white());
            } else {
                p::stroke_round(scene, t, br, 15.0, p::alpha(c.border_standard, 0.9), 1.2);
                p::text_in(scene, tr, br, label, 13.0, c.text_primary);
            }
            self.push_region(br, Action::ScanAction(i));
            bx += bw + 10.0;
        }
    }

    fn draw_scan_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, panel: Rect) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, panel, 18.0, c.surfaces.raised);
        p::stroke_round(scene, t, panel, 18.0, p::alpha(c.border_standard, 0.5), 1.0);
        let px = panel.x0 + 18.0;
        p::text(scene, tr, px, panel.y0 + 18.0, "Recent scans", 14.0, c.text_primary);

        let history = [
            ("link", "qos.dev/spatial", "2m ago"),
            ("wifi", "Café Helvetica", "1h ago"),
            ("person", "Ada Lovelace", "Yesterday"),
            ("map", "Studio HQ", "3d ago"),
        ];
        let mut y = panel.y0 + 46.0;
        for (icon, label, time) in history {
            let row = Rect::new(panel.x0 + 8.0, y, panel.x1 - 8.0, y + 44.0);
            p::icon(scene, t, icon, Rect::new(row.x0 + 8.0, row.center().y - 9.0, row.x0 + 26.0, row.center().y + 9.0), p::alpha(theme.accent_primary, 0.9), 1.8);
            p::text(scene, tr, row.x0 + 36.0, row.center().y - 14.0, label, 13.0, c.text_primary);
            p::text(scene, tr, row.x0 + 36.0, row.center().y + 2.0, time, 11.0, c.text_dim);
            y += 46.0;
        }

        // Divider.
        p::line(scene, t, px, y + 6.0, panel.x1 - 18.0, y + 6.0, p::alpha(c.border_standard, 0.5), 1.0);
        y += 22.0;
        p::text(scene, tr, px, y, "Share this Wi-Fi", 14.0, c.text_primary);
        y += 26.0;
        // Render the share QR.
        let qside = (panel.width() - 120.0).min(panel.y1 - y - 50.0).max(80.0);
        let qr_rect = Rect::new(panel.center().x - qside / 2.0, y, panel.center().x + qside / 2.0, y + qside);
        if let Some(m) = self.share_qr.clone() {
            p::fill_round(scene, t, shrink(qr_rect, -8.0), 12.0, white());
            self.draw_qr(scene, t, &m, qr_rect);
        }
        p::text_center(scene, tr, panel.center().x, qr_rect.y1 + 16.0, "qOS-Studio · WPA2", 11.5, c.text_dim);
    }

    fn draw_qr(&self, scene: &mut Scene, t: Affine, m: &scan::QrMatrix, rect: Rect) {
        let n = m.size as f64;
        let cell = rect.width() / n;
        let dark = Color::from_rgb8(17, 17, 17);
        for y in 0..m.size {
            for x in 0..m.size {
                if m.is_dark(x, y) {
                    let rx = rect.x0 + x as f64 * cell;
                    let ry = rect.y0 + y as f64 * cell;
                    p::fill_rect(scene, t, Rect::new(rx, ry, rx + cell + 0.6, ry + cell + 0.6), dark);
                }
            }
        }
    }
}

// ====================================================================
// Gallery screen
// ====================================================================

impl CameraApp {
    fn draw_gallery(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let pad = 24.0;
        let now = now_secs();

        // Header.
        p::text(scene, tr, content.x0 + pad, content.y0 + 16.0, "Library", 20.0, c.text_primary);
        let count = format!("{} items", self.captures.len());
        p::text(scene, tr, content.x0 + pad, content.y0 + 44.0, &count, 12.5, c.text_dim);

        let seg_w = 320.0;
        let seg = Rect::new(content.x1 - pad - seg_w, content.y0 + 16.0, content.x1 - pad, content.y0 + 48.0);
        let items: Vec<(String, bool, Action)> = GalleryFilter::ALL
            .iter()
            .map(|f| (f.label().to_string(), *f == self.filter, Action::SetFilter(*f)))
            .collect();
        self.segmented(scene, t, tr, theme, seg, items);

        let filtered = self.filtered();
        let has_sel = self.selected_capture.as_ref().map(|id| filtered.iter().any(|c| &c.id == id)).unwrap_or(false);
        let detail_w = if has_sel { 332.0 } else { 0.0 };
        let detail_gap = if has_sel { 18.0 } else { 0.0 };
        let grid = Rect::new(content.x0 + pad, content.y0 + 64.0, content.x1 - pad - detail_w - detail_gap, content.y1 - pad);

        if filtered.is_empty() {
            p::text_center(scene, tr, grid.center().x, grid.center().y - 10.0, "No captures yet", 15.0, c.text_dim);
            return;
        }

        // Grid layout.
        let card_w = 196.0;
        let card_h = 150.0;
        let gap = 14.0;
        let cols = (((grid.width() + gap) / (card_w + gap)).floor() as usize).max(1);
        let cell_w = (grid.width() - gap * (cols - 1) as f64) / cols as f64;
        p::push_clip_rect(scene, t, grid);
        for (i, cap) in filtered.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = grid.x0 + col as f64 * (cell_w + gap);
            let y0 = grid.y0 + row as f64 * (card_h + gap);
            if y0 > grid.y1 {
                break;
            }
            let card = Rect::new(x0, y0, x0 + cell_w, y0 + card_h);
            self.draw_gallery_card(scene, t, tr, theme, card, cap, now);
        }
        p::pop(scene);

        // Detail panel.
        if has_sel {
            let id = self.selected_capture.clone().unwrap();
            if let Some(cap) = filtered.iter().find(|c| c.id == id).cloned() {
                let panel = Rect::new(grid.x1 + detail_gap, content.y0 + 64.0, content.x1 - pad, content.y1 - pad);
                self.draw_gallery_detail(scene, t, tr, theme, panel, &cap, now);
            }
        }
    }

    fn draw_gallery_card(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, card: Rect, cap: &Capture, now: i64) {
        let c = &theme.tokens.colors;
        let selected = self.selected_capture.as_deref() == Some(cap.id.as_str());
        self.ensure_thumb(cap, 224, 168);
        p::push_clip_round(scene, t, card, 14.0);
        if let Some(img) = self.thumbs.get_mut(&cap.id) {
            img.paint_at(scene, t, card);
        }
        // Bottom gradient strip for label legibility.
        let strip = Rect::new(card.x0, card.y1 - 44.0, card.x1, card.y1);
        p::fill_rect(scene, t, strip, p::alpha(Color::from_rgb8(0, 0, 0), 0.45));
        p::text(scene, tr, card.x0 + 10.0, card.y1 - 34.0, &cap.label, 12.5, white());
        p::text(scene, tr, card.x0 + 10.0, card.y1 - 18.0, &ago(cap.ts, now), 10.5, p::alpha(white(), 0.8));
        p::pop(scene);

        // Video badge.
        if cap.is_video() {
            let pl = Rect::new(card.x0 + 8.0, card.y0 + 8.0, card.x0 + 30.0, card.y0 + 30.0);
            p::circle(scene, t, pl.center().x, pl.center().y, 11.0, p::alpha(Color::from_rgb8(0, 0, 0), 0.5));
            p::icon(scene, t, "play", shrink(pl, 5.0), white(), 1.5);
            let dur = fmt_dur(cap.dur_ms);
            let dw = p::text_w(tr, &dur, 11.0) as f64 + 14.0;
            let dr = Rect::new(card.x1 - dw - 8.0, card.y0 + 8.0, card.x1 - 8.0, card.y0 + 30.0);
            p::fill_round(scene, t, dr, 8.0, p::alpha(Color::from_rgb8(0, 0, 0), 0.5));
            p::text_in(scene, tr, dr, &dur, 11.0, white());
        } else if cap.favorite {
            let st = Rect::new(card.x1 - 30.0, card.y0 + 8.0, card.x1 - 8.0, card.y0 + 30.0);
            p::icon(scene, t, "star", st, theme.tokens.colors.semantic.warning, 1.5);
        }

        let ring = if selected { theme.accent_primary } else { p::alpha(c.border_standard, 0.0) };
        p::stroke_round(scene, t, card, 14.0, ring, 2.5);
        self.push_region(card, Action::SelectCapture(cap.id.clone()));
    }

    fn draw_gallery_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, panel: Rect, cap: &Capture, now: i64) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, panel, 18.0, c.surfaces.raised);
        p::stroke_round(scene, t, panel, 18.0, p::alpha(c.border_standard, 0.5), 1.0);

        let preview = Rect::new(panel.x0 + 16.0, panel.y0 + 16.0, panel.x1 - 16.0, panel.y0 + 16.0 + 188.0);
        self.ensure_thumb(cap, 360, 240);
        p::push_clip_round(scene, t, preview, 14.0);
        if let Some(img) = self.thumbs.get_mut(&cap.id) {
            img.paint_at(scene, t, preview);
        }
        if cap.is_video() {
            p::circle(scene, t, preview.center().x, preview.center().y, 22.0, p::alpha(Color::from_rgb8(0, 0, 0), 0.45));
            p::icon(scene, t, "play", Rect::new(preview.center().x - 11.0, preview.center().y - 11.0, preview.center().x + 11.0, preview.center().y + 11.0), white(), 2.0);
        }
        p::pop(scene);

        let mut y = preview.y1 + 16.0;
        p::text(scene, tr, panel.x0 + 18.0, y, &cap.label, 17.0, c.text_primary);
        y += 24.0;
        p::text(scene, tr, panel.x0 + 18.0, y, &ago(cap.ts, now), 12.0, c.text_dim);
        y += 28.0;

        let rows = [
            ("Type", if cap.is_video() { "Video".to_string() } else { "Photo".to_string() }),
            ("Resolution", format!("{} × {}", cap.w, cap.h)),
            ("Camera", cap.facing.label().to_string()),
            ("Format", self.settings.photo_format.label().to_string()),
            ("Size", format!("{:.1} MB", (cap.w as f64 * cap.h as f64 * 1.5) / 1_000_000.0)),
        ];
        for (k, v) in rows {
            p::text(scene, tr, panel.x0 + 18.0, y, k, 12.5, c.text_dim);
            p::text_right(scene, tr, panel.x1 - 18.0, y, &v, 12.5, c.text_primary);
            y += 26.0;
        }

        // Action buttons.
        let by = panel.y1 - 52.0;
        let bw = (panel.width() - 36.0 - 20.0) / 3.0;
        let labels = [("star", "Favorite", Action::ToggleFavSelected), ("share", "Share", Action::ShareSelected), ("trash", "Delete", Action::DeleteSelected)];
        for (i, (icon, label, action)) in labels.into_iter().enumerate() {
            let x0 = panel.x0 + 18.0 + i as f64 * (bw + 10.0);
            let br = Rect::new(x0, by, x0 + bw, by + 38.0);
            let danger = icon == "trash";
            let on = icon == "star" && cap.favorite;
            let col = if danger { theme.tokens.colors.semantic.danger } else if on { theme.tokens.colors.semantic.warning } else { c.text_primary };
            p::fill_round(scene, t, br, 12.0, p::alpha(c.neutral[2], 0.9));
            p::icon(scene, t, icon, Rect::new(br.center().x - 30.0, br.center().y - 8.0, br.center().x - 14.0, br.center().y + 8.0), col, 1.8);
            p::text(scene, tr, br.center().x - 8.0, br.center().y - 7.0, label, 11.5, col);
            self.push_region(br, action);
        }
    }
}

// ====================================================================
// Settings screen
// ====================================================================

impl CameraApp {
    fn draw_settings(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content: Rect) {
        let c = &theme.tokens.colors;
        let colw = 660.0_f64.min(content.width() - 48.0);
        let x0 = content.x0 + (content.width() - colw) / 2.0;
        let mut y = content.y0 + 22.0;
        p::text(scene, tr, x0, y, "Settings", 22.0, c.text_primary);
        y += 44.0;

        // Capture group.
        y = self.group_title(scene, tr, theme, x0, y, "Capture");
        let cap_rows = 4;
        let cap_rect = Rect::new(x0, y, x0 + colw, y + cap_rows as f64 * 52.0);
        p::fill_round(scene, t, cap_rect, 14.0, c.surfaces.raised);
        p::stroke_round(scene, t, cap_rect, 14.0, p::alpha(c.border_standard, 0.5), 1.0);
        let mut ry = y;
        ry = self.value_row(scene, t, tr, theme, x0, ry, colw, "Resolution", self.settings.resolution.label(), Action::CycleResolution);
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.value_row(scene, t, tr, theme, x0, ry, colw, "Photo format", self.settings.photo_format.label(), Action::CycleFormat);
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(scene, t, tr, theme, x0, ry, colw, "HDR", self.settings.hdr, Action::ToggleHdr);
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.timer_row(scene, t, tr, theme, x0, ry, colw);
        y = ry + 20.0;

        // Composition group.
        y = self.group_title(scene, tr, theme, x0, y, "Composition");
        let comp_rect = Rect::new(x0, y, x0 + colw, y + 3.0 * 52.0);
        p::fill_round(scene, t, comp_rect, 14.0, c.surfaces.raised);
        p::stroke_round(scene, t, comp_rect, 14.0, p::alpha(c.border_standard, 0.5), 1.0);
        ry = y;
        ry = self.toggle_row(scene, t, tr, theme, x0, ry, colw, "Grid (rule of thirds)", self.settings.grid, Action::ToggleGrid);
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(scene, t, tr, theme, x0, ry, colw, "Level guide", self.settings.level, Action::ToggleLevel);
        self.row_sep(scene, t, theme, x0, ry, colw);
        ry = self.toggle_row(scene, t, tr, theme, x0, ry, colw, "Mirror front camera", self.settings.mirror_front, Action::ToggleMirror);
        y = ry + 20.0;

        // Privacy + Camera groups side by side conceptually; keep stacked.
        y = self.group_title(scene, tr, theme, x0, y, "Privacy & camera");
        let pc_rect = Rect::new(x0, y, x0 + colw, y + 2.0 * 52.0);
        p::fill_round(scene, t, pc_rect, 14.0, c.surfaces.raised);
        p::stroke_round(scene, t, pc_rect, 14.0, p::alpha(c.border_standard, 0.5), 1.0);
        ry = y;
        ry = self.toggle_row(scene, t, tr, theme, x0, ry, colw, "Location tags", self.settings.geotag, Action::ToggleGeotag);
        self.row_sep(scene, t, theme, x0, ry, colw);
        let _ = self.value_row(scene, t, tr, theme, x0, ry, colw, "Default camera", self.settings.default_facing.label(), Action::CycleDefaultFacing);
    }

    fn group_title(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, label: &str) -> f64 {
        p::text(scene, tr, x + 4.0, y, &label.to_uppercase(), 11.0, p::alpha(theme.tokens.colors.text_dim, 0.9));
        y + 24.0
    }

    fn row_sep(&self, scene: &mut Scene, t: Affine, theme: &Theme, x: f64, y: f64, w: f64) {
        p::line(scene, t, x + 16.0, y, x + w - 16.0, y, p::alpha(theme.tokens.colors.border_standard, 0.35), 1.0);
    }

    fn toggle_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, w: f64, label: &str, on: bool, action: Action) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(scene, tr, x + 18.0, row.center().y - 8.0, label, 14.0, c.text_primary);
        let sw = Rect::new(row.x1 - 18.0 - 46.0, row.center().y - 13.0, row.x1 - 18.0, row.center().y + 13.0);
        self.toggle_switch(scene, t, theme, sw, on, action);
        y + 52.0
    }

    fn value_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, w: f64, label: &str, value: &str, action: Action) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(scene, tr, x + 18.0, row.center().y - 8.0, label, 14.0, c.text_primary);
        let chev = Rect::new(row.x1 - 30.0, row.center().y - 9.0, row.x1 - 16.0, row.center().y + 9.0);
        p::icon(scene, t, "chevron", chev, p::alpha(c.text_dim, 0.9), 1.8);
        p::text_right(scene, tr, chev.x0 - 8.0, row.center().y - 8.0, value, 13.5, theme.accent_primary);
        self.push_region(row, action);
        y + 52.0
    }

    fn timer_row(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, w: f64) -> f64 {
        let c = &theme.tokens.colors;
        let row = Rect::new(x, y, x + w, y + 52.0);
        p::text(scene, tr, x + 18.0, row.center().y - 8.0, "Self-timer", 14.0, c.text_primary);
        let seg = Rect::new(row.x1 - 18.0 - 168.0, row.center().y - 15.0, row.x1 - 18.0, row.center().y + 15.0);
        let items: Vec<(String, bool, Action)> = Timer::ALL
            .iter()
            .map(|tm| (tm.label().to_string(), *tm == self.settings.timer, Action::SetTimer(*tm)))
            .collect();
        self.segmented(scene, t, tr, theme, seg, items);
        y + 52.0
    }
}

/// Truncate `s` with an ellipsis so it fits in `max_w` pixels at `size`.
fn ellipsize(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if p::text_w(tr, s, size) as f64 <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let trial = format!("{out}{ch}…");
        if p::text_w(tr, &trial, size) as f64 > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (CameraApp, AutoCommit, ObjId) {
        let mut app = CameraApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc, root)
    }

    #[test]
    fn seeds_samples_and_default_screen() {
        let (app, _doc, _root) = fresh();
        assert_eq!(app.screen, Screen::Scanner);
        assert_eq!(app.captures.len(), 9);
        assert!(app.selected_capture.is_some());
    }

    #[test]
    fn nav_action_switches_screen_in_doc() {
        let (mut app, mut doc, root) = fresh();
        app.apply(Action::Nav(Screen::Gallery), &mut doc, &root);
        assert_eq!(app.screen, Screen::Gallery);
        assert_eq!(doc::read_screen(&doc, &root), Screen::Gallery);
    }

    #[test]
    fn shutter_appends_capture() {
        let (mut app, mut doc, root) = fresh();
        let before = app.captures.len();
        app.apply(Action::Shutter, &mut doc, &root);
        assert_eq!(app.captures.len(), before + 1);
        assert_eq!(doc::read_captures(&doc, &root).len(), before + 1);
        assert!(app.toast.is_some());
    }

    #[test]
    fn cycle_facing_and_persist() {
        let (mut app, mut doc, root) = fresh();
        assert_eq!(app.facing, Facing::Back);
        app.apply(Action::CycleFacing, &mut doc, &root);
        assert_eq!(app.facing, Facing::Front);
        assert_eq!(doc::read_facing(&doc, &root), Facing::Front);
    }

    #[test]
    fn toggle_grid_persists_to_settings() {
        let (mut app, mut doc, root) = fresh();
        let before = app.settings.grid;
        app.apply(Action::ToggleGrid, &mut doc, &root);
        assert_eq!(app.settings.grid, !before);
        assert_eq!(doc::read_settings(&doc, &root).grid, !before);
    }

    #[test]
    fn delete_selected_removes_and_reselects() {
        let (mut app, mut doc, root) = fresh();
        let id = app.selected_capture.clone().unwrap();
        app.apply(Action::DeleteSelected, &mut doc, &root);
        assert!(!app.captures.iter().any(|c| c.id == id));
        assert_eq!(app.captures.len(), 8);
        assert!(app.selected_capture.is_some());
    }

    #[test]
    fn resolution_cycles() {
        let (mut app, mut doc, root) = fresh();
        assert_eq!(app.settings.resolution, Resolution::R12MP);
        app.apply(Action::CycleResolution, &mut doc, &root);
        assert_eq!(app.settings.resolution, Resolution::R4K);
    }
}
