//! `BrowserWidget` — the quick-ui root widget for the browser app.
//!
//! Replaces the iced `Application` impl from Phase A. The widget owns
//! the chrome composition (URL input + reload/go buttons + Servo
//! surface), spawns the engine thread, and routes events into Servo via
//! the shared `BrowserCommand` channel.
//!
//! State model:
//!  * URL text lives in the CRDT under `browser.url` (Automerge `Text`
//!    object) — this is the path the [`TextInput`] reads/writes on
//!    every frame.
//!  * Button presses surface as boolean flags at `browser.go_clicked`
//!    and `browser.reload_clicked`. The widget polls these in `sync`
//!    after children dispatch, and on `true` issues the corresponding
//!    `BrowserCommand`, then resets the flag.
//!
//! Frame rendering:
//!  * Servo pushes RGBA into a shared `FrameSlot` via [`QuickHost`].
//!  * `ServoSurface` reads the slot on every `draw` pass.

use std::sync::{Arc, Mutex};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use crossbeam_channel::Sender;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, Button, Column, Container, IconDescriptor, Row, TextInput, Widget};

use crate::engine::{command_channel, spawn_engine_thread, BrowserCommand};
use crate::host::{BrowserHost, SharedHost};
use crate::servo_surface::{FrameData, FrameSlot, ServoSurface};

/// CRDT path for the URL text input.
const URL_PATH: &str = "browser.url";
/// CRDT path the Go button writes `true` to on click.
const GO_FLAG_PATH: &str = "browser.go_clicked";
/// CRDT path the Reload button writes `true` to on click.
const RELOAD_FLAG_PATH: &str = "browser.reload_clicked";

/// Default URL surfaced in the address bar on startup. Matches the iced
/// host so we don't change behaviour during the migration.
const DEFAULT_URL: &str = "https://duckduckgo.com";

/// Shared sink for Servo's frames + wake notifications.
pub struct QuickHost {
    /// Latest RGBA frame from Servo.
    pub frames: FrameSlot,
    /// Best-effort wake — the quick-ui App polls its widget tree every
    /// frame anyway, but signalling here lets us request a redraw
    /// proactively. v1 leaves this as a no-op; quick-ui's App pulls
    /// frames on every wakeup, and Servo's own timer drives wakeups via
    /// the engine thread's 16 ms loop.
    pub on_wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl QuickHost {
    pub fn new(frames: FrameSlot) -> Arc<Self> {
        Arc::new(Self {
            frames,
            on_wake: Mutex::new(None),
        })
    }
}

impl BrowserHost for QuickHost {
    fn submit_frame(&self, rgba: Vec<u8>, width: u32, height: u32) {
        if let Ok(mut slot) = self.frames.lock() {
            *slot = Some(FrameData { rgba, width, height });
        }
        self.wake();
    }
    fn wake(&self) {
        if let Ok(guard) = self.on_wake.lock() {
            if let Some(cb) = guard.as_ref() {
                cb();
            }
        }
    }
}

/// Root widget for the quick-ui browser app.
pub struct BrowserWidget {
    rect: Rect,
    /// Shared frame slot — written by `QuickHost::submit_frame`, read
    /// by `ServoSurface::draw`.
    frames: FrameSlot,
    /// Command channel sender — the surface and chrome buttons send
    /// here; the engine thread receives.
    cmd_tx: Sender<BrowserCommand>,
    /// Composed child tree (rebuilt on `build_layout`).
    composed: Option<Box<dyn Widget>>,
    /// True once we've populated the CRDT with the initial URL text.
    /// Subsequent `sync` calls only poll for button flags.
    bootstrap_done: bool,
    /// Last URL we asked Servo to load. Used so a Reload click without
    /// any pending Go re-sends the same URL.
    last_loaded_url: String,
    /// Last reported surface rect — when this changes we issue a
    /// Resize to Servo. Tracks the *logical* size; we multiply by
    /// scale_factor when sending.
    last_surface_size: (f64, f64),
    /// Most recent OS scale factor seen in `handle_event`.
    scale_factor: f32,
    /// Whether we've already kicked off the engine thread. The thread
    /// runs for the program's lifetime so this is one-shot.
    engine_started: bool,
    /// When true, `sync` will not spawn the Servo engine thread. Used by
    /// `--shots` so the offscreen render stays fully headless: no Servo
    /// boot, no network requests, no WebRender GL context. The chrome
    /// paints its loading/placeholder state instead of a live page.
    skip_engine: bool,
}

impl BrowserWidget {
    /// Construct a browser widget bound to a fresh frame slot + command
    /// channel. The engine thread is *not* spawned here — it's started
    /// on the first `sync` so the `--shots` path can render an empty
    /// chrome without ever booting Servo.
    pub fn new() -> Self {
        let frames: FrameSlot = Arc::new(Mutex::new(None));
        let (tx, _rx) = command_channel();
        // Note: we throw away the receiver here; `spawn_engine_thread`
        // is called from `bootstrap()` with a fresh pair. The cmd_tx
        // we store is replaced on bootstrap so children get a live one.
        Self {
            rect: Rect::ZERO,
            frames,
            cmd_tx: tx,
            composed: None,
            bootstrap_done: false,
            last_loaded_url: String::new(),
            last_surface_size: (0.0, 0.0),
            scale_factor: 1.0,
            engine_started: false,
            skip_engine: false,
        }
    }

    /// Construct a browser widget that never spawns the Servo engine.
    /// Used by the `--shots` headless render so we don't pay for a
    /// WebRender GL context just to snapshot the chrome.
    pub fn new_headless() -> Self {
        let mut w = Self::new();
        w.skip_engine = true;
        w
    }

    /// Build (or rebuild) the composed widget tree from current model
    /// state. Called on every `build_layout`.
    fn build_composition(&self) -> Box<dyn Widget> {
        // URL bar: reload | text input | go.
        let url_input = TextInput::new(vec![])
            .with_id("browser.url_input")
            .with_on_change_path(URL_PATH);
        let url_bar = Row::new()
            .push(Box::new(Button::new(
                "browser.reload",
                "Reload",
                Some(RELOAD_FLAG_PATH),
            )))
            .push(Box::new(url_input))
            .push(Box::new(Button::new(
                "browser.go",
                "Go",
                Some(GO_FLAG_PATH),
            )));

        // Live Servo surface.
        let surface = ServoSurface::new(self.frames.clone()).with_sender(self.cmd_tx.clone());

        // Root vertical composition.
        Box::new(
            Column::new()
                .push(Box::new(Container::new().with_child(Box::new(url_bar))))
                .push(Box::new(surface)),
        )
    }

    /// One-shot CRDT bootstrap: create the URL text object and the
    /// button flags so children can mutate them on first frame.
    fn bootstrap_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // URL text object — Automerge `Text`, seeded with DEFAULT_URL.
        if state.get(root, URL_PATH).ok().flatten().is_none() {
            if let Ok(obj_id) = state.put_object(root, URL_PATH, ObjType::Text) {
                let _ = state.splice_text(&obj_id, 0, 0, DEFAULT_URL);
            }
        }
        // Flag scalars — start as false so a false→true edge fires only
        // on real button clicks.
        if state.get(root, GO_FLAG_PATH).ok().flatten().is_none() {
            let _ = state.put(root, GO_FLAG_PATH, false);
        }
        if state.get(root, RELOAD_FLAG_PATH).ok().flatten().is_none() {
            let _ = state.put(root, RELOAD_FLAG_PATH, false);
        }
        self.last_loaded_url = DEFAULT_URL.to_string();
    }

    /// Spawn the Servo engine thread. Idempotent.
    fn start_engine(&mut self) {
        if self.engine_started {
            return;
        }
        let (tx, rx) = command_channel();
        self.cmd_tx = tx;
        let host: SharedHost = QuickHost::new(self.frames.clone());
        spawn_engine_thread(DEFAULT_URL.to_string(), 1024, 768, 1.0, host, rx);
        self.engine_started = true;
    }

    /// Read the current URL out of the CRDT. Returns `None` if the
    /// path doesn't yet exist (pre-bootstrap).
    fn read_url(state: &AutoCommit, root: &ObjId) -> Option<String> {
        if let Ok(Some((Value::Object(ObjType::Text), obj_id))) = state.get(root, URL_PATH) {
            state.text(&obj_id).ok()
        } else {
            None
        }
    }

    /// Read a boolean flag, returning `true` only when the path holds
    /// a true scalar.
    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        )
    }

    /// Normalise a user-typed URL (prepend `https://` if missing).
    fn normalise_url(input: &str) -> String {
        if input.starts_with("http://") || input.starts_with("https://") {
            input.to_string()
        } else {
            format!("https://{}", input)
        }
    }
}

impl Default for BrowserWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for BrowserWidget {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, path);
            // Spawn the engine on first sync unless headless (--shots).
            if !self.skip_engine {
                self.start_engine();
            }
            self.bootstrap_done = true;
        }

        // Detect button flags raised since last sync, and translate
        // them into BrowserCommands. We *reset* the flag right after
        // handling so a single click fires exactly one command.
        let go = Self::read_flag(state, path, GO_FLAG_PATH);
        let reload = Self::read_flag(state, path, RELOAD_FLAG_PATH);
        if go {
            if let Some(url) = Self::read_url(state, path) {
                let normalised = Self::normalise_url(&url);
                self.last_loaded_url = normalised.clone();
                let _ = self.cmd_tx.send(BrowserCommand::LoadUrl(normalised));
                let _ = self.cmd_tx.send(BrowserCommand::Focus);
            }
            let _ = state.put(path, GO_FLAG_PATH, false);
        }
        if reload {
            if !self.last_loaded_url.is_empty() {
                let _ = self
                    .cmd_tx
                    .send(BrowserCommand::LoadUrl(self.last_loaded_url.clone()));
            }
            let _ = state.put(path, RELOAD_FLAG_PATH, false);
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&child_node) = children.first() {
                    c.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }

        // After children are laid out, the Servo surface has its real
        // rect — push a resize to the engine if it changed. We dig the
        // surface out of the composed tree via the children iterator;
        // it's the last leaf in the composition order.
        let surface_rect = self.find_surface_rect();
        if let Some(r) = surface_rect {
            let new_logical = (r.width(), r.height());
            if (new_logical.0 - self.last_surface_size.0).abs() > 0.5
                || (new_logical.1 - self.last_surface_size.1).abs() > 0.5
            {
                self.last_surface_size = new_logical;
                if new_logical.0 > 0.0 && new_logical.1 > 0.0 {
                    let pw = (new_logical.0 as f32 * self.scale_factor) as u32;
                    let ph = (new_logical.1 as f32 * self.scale_factor) as u32;
                    let _ = self.cmd_tx.send(BrowserCommand::Resize(pw.max(1), ph.max(1)));
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track scale factor so we can emit physical-pixel resizes.
        if (self.scale_factor as f64 - scale_factor).abs() > f64::EPSILON {
            self.scale_factor = scale_factor as f32;
            let _ = self
                .cmd_tx
                .send(BrowserCommand::ScaleFactor(self.scale_factor));
        }

        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        child_node,
                        taffy,
                    );
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "BrowserWidget"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn test_id(&self) -> Option<&str> {
        Some("browser.root")
    }
}

impl BrowserWidget {
    /// Walk the composed tree and return the [`ServoSurface`]'s rect if
    /// one is present. The surface is identified by `test_id ==
    /// "browser.surface"`.
    fn find_surface_rect(&self) -> Option<Rect> {
        fn walk(w: &dyn Widget) -> Option<Rect> {
            if w.test_id() == Some("browser.surface") {
                return Some(w.get_rect());
            }
            for c in w.children() {
                if let Some(r) = walk(c) {
                    return Some(r);
                }
            }
            None
        }
        self.composed.as_ref().and_then(|c| walk(c.as_ref()))
    }
}

impl AppWidget for BrowserWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        "Browser"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{F02AC}") // mdi-web (browser globe glyph)
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}
