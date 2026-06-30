//! Root widget for the container manager.
//!
//! `ContainersApp` is a single self-laying-out widget that owns the engine
//! snapshot (containers / images / distroboxes mirrored from rootless
//! podman) and renders the whole surface — a left navigation rail, a master
//! list with inline lifecycle actions, a tabbed detail view, a create-
//! container form, and confirm/create modals. Navigation and selection state
//! live in the shared CRDT document (see [`crate::doc`]) so undo/redo and
//! live collaboration work without extra plumbing; the engine projection is
//! transient host data and stays in memory.
//!
//! The widget hit-tests in `handle_event` against rectangles it recorded
//! during `draw`, which keeps the dense, data-driven layout (variable row
//! counts, wrapped port chips, tab strips) under direct control without
//! fighting a flexbox tree.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use taffy::prelude::*;

use crate::doc::{CreateField, DetailTab, Section, ViewDoc};
use crate::draw;
use crate::model::{Container, Engine};
use crate::podman::{self, Action};

const RAIL_WIDTH: f64 = 200.0;
pub(crate) const HEADER_HEIGHT: f64 = 64.0;
pub(crate) const ROW_HEIGHT: f64 = 64.0;
pub(crate) const DETAIL_WIDTH: f64 = 420.0;
const TOAST_MS: u128 = 4000;

/// A clickable region recorded during draw and resolved in `handle_event`.
#[derive(Clone)]
pub(crate) enum Hit {
    Section(Section),
    NewContainer,
    SelectContainer(String),
    SelectImage(usize),
    Action { id: String, action: Action },
    DistroEnter(String),
    DistroCreate,
    DetailTab(DetailTab),
    CloseDetail,
    ConfirmOk,
    ConfirmCancel,
    CreateField(CreateField),
    CreateSubmit,
    CreateCancel,
    SearchFocus,
    ClearSearch,
}

pub(crate) struct HitRegion {
    pub(crate) rect: Rect,
    pub(crate) hit: Hit,
}

/// A transient confirm dialog for a destructive action.
pub(crate) struct Confirm {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) action: Action,
}

/// A transient status toast.
pub(crate) struct Toast {
    pub(crate) message: String,
    pub(crate) error: bool,
    pub(crate) born: Instant,
}

/// Background-thread message: a fresh engine probe arrived.
enum Bg {
    Snapshot(Engine),
}

pub struct ContainersApp {
    /// The live (or sample) engine projection.
    pub(crate) engine: Engine,
    rect: Rect,
    /// Mouse position in logical coords (for hover + hit-testing).
    pub(crate) cursor: Point,
    /// Click regions recorded during the last draw.
    pub(crate) hits: Vec<HitRegion>,
    /// Per-section scroll offsets (list scroll).
    pub(crate) list_scroll: f64,
    pub(crate) detail_scroll: f64,
    /// Active confirm dialog, if any.
    pub(crate) confirm: Option<Confirm>,
    /// Whether the create-container modal is open.
    pub(crate) create_open: bool,
    /// Which create field has keyboard focus (index into CreateField::ALL).
    pub(crate) create_focus: usize,
    /// Whether the search box has keyboard focus.
    pub(crate) search_focus: bool,
    /// Current toast.
    pub(crate) toast: Option<Toast>,
    /// Background refresh plumbing. `None` in headless/shots mode.
    bg_rx: Option<Receiver<Bg>>,
    bg_tx: Option<Sender<Bg>>,
    last_refresh: Instant,
    pub(crate) live: bool,
    /// In offline/demo mode, seed an initial selection on the first sync so a
    /// fresh document renders a populated detail view (notably for `--shots`).
    seed_demo: bool,
    seeded: bool,
    /// Whether a structural change happened this frame (forces relayout).
    pub(crate) structure_dirty: bool,
    /// Cached snapshot of the CRDT-backed view state. Refreshed in `sync`
    /// and at the tail of every `handle_event`; `draw` reads it because the
    /// draw pass has no document access.
    view: ViewState,
}

impl ContainersApp {
    /// Construct the manager. `live` enables the background podman poller and
    /// real lifecycle actions; `false` (used by `--shots`) renders the
    /// bundled sample snapshot deterministically with no process spawning.
    pub fn new(live: bool) -> Self {
        let engine = if live {
            podman::probe().engine
        } else {
            crate::model::sample_engine()
        };
        let (bg_tx, bg_rx) = if live {
            let (tx, rx) = std::sync::mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        Self {
            engine,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            hits: Vec::new(),
            list_scroll: 0.0,
            detail_scroll: 0.0,
            confirm: None,
            create_open: false,
            create_focus: 0,
            search_focus: false,
            toast: None,
            bg_rx,
            bg_tx,
            last_refresh: Instant::now(),
            live,
            seed_demo: !live,
            seeded: false,
            structure_dirty: false,
            view: ViewState::default(),
        }
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast { message: message.into(), error, born: Instant::now() });
    }

    /// Kick a background podman re-probe so stats stay fresh without blocking
    /// the UI thread. No-op offline.
    fn spawn_refresh(&mut self) {
        if !self.live {
            return;
        }
        let Some(tx) = self.bg_tx.clone() else { return };
        self.last_refresh = Instant::now();
        std::thread::spawn(move || {
            let engine = podman::probe().engine;
            let _ = tx.send(Bg::Snapshot(engine));
        });
    }

    /// Drain any background snapshot, preserving per-container detail
    /// (logs/env/mounts) for the selected container that `ps` doesn't carry.
    fn drain_bg(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = &self.bg_rx else { return };
        let mut latest = None;
        while let Ok(Bg::Snapshot(e)) = rx.try_recv() {
            latest = Some(e);
        }
        if let Some(mut fresh) = latest {
            // Carry over the selected container's hydrated detail so the
            // detail tabs don't flicker empty between probes.
            if let Some(sel) = ViewDoc::selected(doc) {
                if let Some(old) = self.engine.find(&sel).cloned() {
                    if let Some(newc) = fresh.containers.iter_mut().find(|c| c.id == sel) {
                        if newc.logs.is_empty() {
                            newc.logs = old.logs;
                        }
                        if newc.env.is_empty() {
                            newc.env = old.env;
                        }
                        if newc.mounts.is_empty() {
                            newc.mounts = old.mounts;
                        }
                    }
                }
            }
            self.engine = fresh;
        }
    }

    /// Hydrate the selected container's detail (logs + inspect) on demand.
    fn hydrate_selection(&mut self, doc: &mut AutoCommit) {
        let Some(sel) = ViewDoc::selected(doc) else { return };
        let needs = self
            .engine
            .containers
            .iter()
            .find(|c| c.id == sel)
            .map(|c| c.logs.is_empty() && c.env.is_empty())
            .unwrap_or(false);
        if !needs || !self.live {
            return;
        }
        let logs = podman::fetch_logs(&sel, 200);
        if let Some(c) = self.engine.containers.iter_mut().find(|c| c.id == sel) {
            podman::inspect_into(c);
            if !logs.is_empty() {
                c.logs = logs;
            }
        }
    }

    /// Apply a lifecycle action, then re-probe. Destructive ones come through
    /// the confirm dialog so this is only ever invoked post-confirmation.
    pub(crate) fn apply_action(&mut self, action: Action, id: &str) {
        let name = self
            .engine
            .find(id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| id.to_string());
        if self.live {
            match podman::run_action(action, id) {
                Ok(()) => self.show_toast(format!("{} {}", action.label(), name), false),
                Err(e) => self.show_toast(format!("{} failed: {}", action.label(), e), true),
            }
            self.spawn_refresh();
        } else {
            // Offline: reflect the action in the local snapshot so the
            // sample surface still responds to clicks.
            self.apply_action_offline(action, id);
            self.show_toast(format!("{} {}", action.label(), name), false);
        }
    }

    /// Mutate the in-memory snapshot to mirror a lifecycle verb when no
    /// engine is present. Keeps the demo interactive.
    fn apply_action_offline(&mut self, action: Action, id: &str) {
        use crate::model::ContainerStatus as S;
        match action {
            Action::Remove => self.engine.containers.retain(|c| c.id != id),
            other => {
                if let Some(c) = self.engine.containers.iter_mut().find(|c| c.id == id) {
                    c.status = match other {
                        Action::Start | Action::Restart | Action::Unpause => S::Running,
                        Action::Stop => S::Exited,
                        Action::Pause => S::Paused,
                        Action::Remove => c.status,
                    };
                    c.status_detail = match c.status {
                        S::Running => "Up just now".into(),
                        S::Exited => "Exited (0) just now".into(),
                        S::Paused => "Paused".into(),
                        _ => c.status_detail.clone(),
                    };
                    if !c.status.is_running() {
                        c.stats = crate::model::Stats::default();
                    }
                }
            }
        }
        self.structure_dirty = true;
    }

    pub(crate) fn submit_create(&mut self, doc: &mut AutoCommit) {
        let args = ViewDoc::create_run_args(doc);
        let Some(args) = args else {
            self.show_toast("Image is required to create a container", true);
            return;
        };
        let name = ViewDoc::create_field(doc, CreateField::Image);
        if self.live {
            let out = std::process::Command::new("podman").args(&args).output();
            match out {
                Ok(o) if o.status.success() => {
                    self.show_toast(format!("Created container from {}", name), false);
                    ViewDoc::clear_create(doc);
                    self.create_open = false;
                    self.spawn_refresh();
                }
                Ok(o) => self.show_toast(
                    format!("Create failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
                    true,
                ),
                Err(e) => self.show_toast(format!("Create failed: {e}"), true),
            }
        } else {
            self.show_toast(format!("podman {}", args.join(" ")), false);
            ViewDoc::clear_create(doc);
            self.create_open = false;
        }
    }

    /// Filtered containers for the current search query, sorted running-first.
    pub(crate) fn visible_containers(&self, query: &str) -> Vec<&Container> {
        let mut v: Vec<&Container> =
            self.engine.containers.iter().filter(|c| c.matches(query)).collect();
        v.sort_by(|a, b| {
            b.status
                .is_running()
                .cmp(&a.status.is_running())
                .then_with(|| a.name.cmp(&b.name))
        });
        v
    }
}

// =====================================================================
// Widget impl
// =====================================================================

impl Widget for ContainersApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        self.drain_bg(doc);
        // Seed a representative selection on the very first sync of a demo /
        // shots session so the detail view is populated out of the box.
        if self.seed_demo && !self.seeded {
            self.seeded = true;
            // An optional env hook selects a demo scene for screenshots
            // (`containers` / `images` / `distrobox` / `stats` / `inspect`).
            let scene = std::env::var("QOS_CONTAINERS_DEMO").unwrap_or_default();
            match scene.as_str() {
                "images" => ViewDoc::set_section(doc, Section::Images),
                "distrobox" => ViewDoc::set_section(doc, Section::Distrobox),
                "create" => {
                    self.create_open = true;
                    self.create_focus = 1;
                    ViewDoc::set_create_field(doc, CreateField::Name, "web-frontend");
                    ViewDoc::set_create_field(doc, CreateField::Image, "docker.io/library/nginx:alpine");
                    ViewDoc::set_create_field(doc, CreateField::Ports, "8080:80, 8443:443");
                }
                _ => {}
            }
            let tab = match scene.as_str() {
                "stats" => DetailTab::Stats,
                "inspect" => DetailTab::Inspect,
                "terminal" => DetailTab::Terminal,
                _ => DetailTab::Logs,
            };
            if scene != "images" && scene != "distrobox" && ViewDoc::selected(doc).is_none() {
                if let Some(first) = self
                    .engine
                    .containers
                    .iter()
                    .find(|c| c.status.is_running())
                    .or_else(|| self.engine.containers.first())
                {
                    ViewDoc::set_selected(doc, Some(&first.id));
                    ViewDoc::set_tab(doc, tab);
                }
            }
        }
        // Periodically re-probe the engine for fresh stats / state.
        if self.live && self.last_refresh.elapsed() >= podman::REFRESH_INTERVAL {
            self.spawn_refresh();
        }
        self.hydrate_selection(doc);
        self.refresh_view(doc);
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while live so the background poller's snapshots and
        // the toast timeout are picked up without needing input.
        self.live || self.toast.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The widget self-lays-out in apply_layout/draw, so it only needs a
        // single full-bleed leaf node.
        let node = taffy
            .new_leaf(Style {
                size: Size { width: percent(1.0), height: percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
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

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Install the host's DPI transform so every draw helper this frame
        // composes onto it (HiDPI-correct without threading it everywhere).
        draw::set_base(transform);
        // Expire the toast.
        if let Some(t) = &self.toast {
            if t.born.elapsed().as_millis() > TOAST_MS {
                self.toast = None;
            }
        }
        self.hits.clear();
        let r = self.rect;
        let c = &theme.tokens.colors;

        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        // The draw pass has no document access; it renders from the view
        // snapshot refreshed in `sync` / `handle_event`.
        let view = self.view.clone();

        // --- Navigation rail ---
        let rail = Rect::new(r.x0, r.y0, r.x0 + RAIL_WIDTH, r.y1);
        self.draw_rail(scene, tr, theme, rail, &view);

        // --- Content area ---
        let content = Rect::new(rail.x1, r.y0, r.x1, r.y1);
        match view.section {
            Section::Containers => self.draw_containers(scene, tr, theme, content, &view),
            Section::Images => self.draw_images(scene, tr, theme, content, &view),
            Section::Distrobox => self.draw_distrobox(scene, tr, theme, content, &view),
        }

        // --- Overlays ---
        if self.create_open {
            self.draw_create_modal(scene, tr, theme, r, &view);
        }
        if let Some(confirm) = &self.confirm {
            Self::draw_confirm(scene, tr, theme, r, confirm, &mut self.hits);
        }
        if let Some(toast) = &self.toast {
            Self::draw_toast(scene, tr, theme, r, toast);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Hover changes button shading — request a redraw.
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (*y as f64) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                // Scroll the panel under the cursor.
                if self.cursor.x > self.rect.x0 + RAIL_WIDTH + self.list_area_width(doc) {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                } else {
                    self.list_scroll = (self.list_scroll - dy).max(0.0);
                }
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.on_click(doc, handled);
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                self.on_key(doc, key_event, modifiers, handled);
            }
            _ => {}
        }
        // Refresh the cached view state after any mutation so the next draw
        // reflects it (draw has no doc access).
        self.refresh_view(doc);
    }

    fn name(&self) -> &str {
        "ContainersApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

// =====================================================================
// Cached view state
//
// `draw` has no access to the CRDT document, so the app keeps a small
// snapshot of the doc-backed view state refreshed at the tail of every
// `handle_event` and in `sync`.
// =====================================================================

#[derive(Clone)]
pub(crate) struct ViewState {
    pub section: Section,
    pub selected: Option<String>,
    pub tab: DetailTab,
    pub search: String,
    pub create: [String; 4],
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            section: Section::Containers,
            selected: None,
            tab: DetailTab::Logs,
            search: String::new(),
            create: [String::new(), String::new(), String::new(), String::new()],
        }
    }
}

impl ContainersApp {
    pub(crate) fn refresh_view(&mut self, doc: &mut AutoCommit) {
        self.view = ViewState {
            section: ViewDoc::section(doc),
            selected: ViewDoc::selected(doc),
            tab: ViewDoc::tab(doc),
            search: ViewDoc::search(doc),
            create: [
                ViewDoc::create_field(doc, CreateField::Name),
                ViewDoc::create_field(doc, CreateField::Image),
                ViewDoc::create_field(doc, CreateField::Ports),
                ViewDoc::create_field(doc, CreateField::Command),
            ],
        };
    }

    /// Width of the list panel (full content width when no detail is open,
    /// otherwise content minus the detail column).
    fn list_area_width(&self, _doc: &AutoCommit) -> f64 {
        let content_w = (self.rect.width() - RAIL_WIDTH).max(0.0);
        if self.view.selected.is_some() && self.view.section == Section::Containers {
            (content_w - DETAIL_WIDTH).max(240.0)
        } else {
            content_w
        }
    }
}
