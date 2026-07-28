//! Root widget for the API tester.
//!
//! Owns the in-memory workspace (collections, environments, the active
//! request, history, and the most recent response), mirrors it into the
//! shared CRDT, and renders the three-pane Postman/Insomnia-class layout:
//!
//! ```text
//! ┌──────────┬───────────────────────────────────────────┐
//! │ sidebar  │ method ▾  [ URL bar ............ ] [Send]  │
//! │ collec-  ├───────────────────────────────────────────┤
//! │ tions    │ Params · Headers · Auth · Body            │
//! │ environ- │ <active tab editor>                        │
//! │ ments    ├───────────────────────────────────────────┤
//! │ history  │ 200 OK · 84 ms · 412 B   Pretty Raw Prev   │
//! │          │ <response headers / highlighted body>      │
//! └──────────┴───────────────────────────────────────────┘
//! ```
//!
//! The layout is a single full-window widget that paints into the Vello
//! scene; sub-regions are hit-tested in `handle_event`. The URL bar is a
//! live CRDT-bound `TextInput`, so collaborators see keystrokes converge.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use ui::widget::SplitLayout;

use crate::doc::ApiDoc;
use crate::http::{self, Outgoing};
use crate::model::{
    human_duration, human_size, Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod,
    KeyValue, Request, RequestTab, ResponseData, ResponseView, StatusClass,
};
use crate::responsive::{RailState, RAIL_WIDTH};
use crate::vars;

/// The primary rail's docked / fully-revealed width. Sourced from the
/// adaptive-chrome module so the breakpoint and the rail width stay in step.
const SIDEBAR_W: f64 = RAIL_WIDTH;
const METHOD_W: f64 = 104.0;
/// The square hamburger toggle that summons the rail in overlay mode.
const HAMBURGER_W: f64 = 36.0;
const SEND_W: f64 = 96.0;
const URLBAR_H: f64 = 56.0;
const TAB_STRIP_H: f64 = 40.0;
const ROW_H: f64 = 30.0;
const PAD: f64 = 16.0;
const JSON_LINE_H: f64 = 20.0;

/// Outcome of the background send worker.
enum SendOutcome {
    Ok(ResponseData),
    Err(String),
}

/// Root widget: the whole API tester.
pub struct ApiTesterApp {
    // --- Model ---
    collections: Vec<Collection>,
    environments: Vec<Environment>,
    active_env: Option<uuid::Uuid>,
    request: Request,
    history: Vec<HistoryEntry>,
    response: Option<ResponseData>,
    /// Set while a request is in flight.
    sending: bool,
    /// Last error string, if a send failed.
    error: Option<String>,

    // --- View state (local, per-peer; not in the CRDT) ---
    active_tab: RequestTab,
    response_view: ResponseView,
    /// JSON line indices that the user has collapsed in the Pretty view.
    collapsed: std::collections::HashSet<usize>,
    /// Whether the method dropdown is open.
    method_open: bool,
    /// Whether the environment selector is open.
    env_open: bool,
    /// Vertical scroll offset of the response body, in lines.
    body_scroll: f64,
    /// Currently selected request: (collection idx, request idx).
    selected: Option<(usize, usize)>,

    // --- Embedded live widgets ---
    url_input: TextInput,
    body_input: TextInput,

    // --- Worker plumbing ---
    tx: Sender<SendOutcome>,
    rx: Receiver<SendOutcome>,

    // --- Adaptive chrome ---
    /// Primary rail collapse state — docked beside the builder on a wide
    /// window, an overlay flap behind a scrim on a narrow one.
    rail: RailState,
    /// Wall-clock of the last rail animation tick, so the eased slide is
    /// frame-rate independent.
    last_tick: Instant,

    // --- Layout / interaction caches ---
    rect: Rect,
    /// Hit regions resolved during draw, consumed by handle_event.
    hits: Hits,
    last_cursor: Point,
    seeded: bool,
    adopted: bool,
    dirty: bool,
    /// True once the CRDT has been initialised from our seed/model.
    persisted_once: bool,
}

/// Click targets resolved during the draw pass.
#[derive(Default, Clone)]
struct Hits {
    method_btn: Rect,
    method_items: Vec<(Rect, HttpMethod)>,
    send_btn: Rect,
    env_btn: Rect,
    env_items: Vec<(Rect, Option<uuid::Uuid>)>,
    tabs: Vec<(Rect, RequestTab)>,
    views: Vec<(Rect, ResponseView)>,
    /// Sidebar collection header rows: (rect, collection idx).
    collection_rows: Vec<(Rect, usize)>,
    /// Sidebar request rows: (rect, collection idx, request idx).
    request_rows: Vec<(Rect, usize, usize)>,
    /// History rows: (rect, history idx).
    history_rows: Vec<(Rect, usize)>,
    /// JSON fold carets in the pretty view: (rect, line idx).
    fold_carets: Vec<(Rect, usize)>,
    /// Body region (for scroll wheel routing).
    body_region: Rect,
    url_rect: Rect,
    body_input_rect: Rect,
    /// Hamburger toggle that summons the rail (overlay mode only).
    hamburger: Rect,
    /// Dimming scrim behind the open overlay rail; clicking it dismisses.
    scrim: Rect,
    /// The rail's drawn rect, used to keep scrim-clicks off the rail itself.
    sidebar: Rect,
}

impl Hits {
    /// The rail's current drawn area (zero-sized when the rail isn't shown).
    fn sidebar_area(&self) -> Rect {
        self.sidebar
    }
}

impl ApiTesterApp {
    /// Build the default workspace and wire up the send worker channel.
    /// The live app: no fabricated saved collections/history and NO canned
    /// response — the response pane is empty until a real Send. It opens on a
    /// single blank starter request.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// The hermetic `--shots` / demo build: a sample collection, environments,
    /// history and a rendered example response so the screenshot is populated.
    pub fn new_demo() -> Self {
        Self::build(true)
    }

    fn build(demo: bool) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let collections = if demo { crate::seed::collections() } else { Vec::new() };
        let environments = if demo { crate::seed::environments() } else { Vec::new() };
        let active_env = environments.first().map(|e| e.id);
        // A blank starter request either way (the sample fills the URL/body only
        // in the demo); a real Send is what produces a response.
        let request = if demo {
            crate::seed::active_request()
        } else {
            crate::seed::blank_request()
        };
        let history = if demo { crate::seed::history() } else { Vec::new() };

        let mut url_input = TextInput::local();
        url_input.set_plain_chrome(true);
        url_input.set_text(&request.url);
        let mut body_input = TextInput::local();
        body_input.set_plain_chrome(true);
        body_input.set_text(&request.body_text);

        Self {
            collections,
            environments,
            active_env,
            request,
            history,
            response: if demo { Some(http::sample_response()) } else { None },
            sending: false,
            error: None,
            active_tab: RequestTab::Params,
            response_view: ResponseView::Pretty,
            collapsed: std::collections::HashSet::new(),
            method_open: false,
            env_open: false,
            body_scroll: 0.0,
            selected: if demo { Some((0, 0)) } else { None },
            url_input,
            body_input,
            tx,
            rx,
            rail: RailState::default(),
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            hits: Hits::default(),
            last_cursor: Point::ZERO,
            seeded: false,
            adopted: false,
            dirty: true,
            persisted_once: false,
        }
    }

    /// Snap the rail to a settled open/closed state with no animation. Used by
    /// the single-frame `--shots` renderer so a headless capture shows the rail
    /// fully docked or fully collapsed rather than mid-slide.
    pub fn snap_rail(&mut self, open: bool) {
        self.rail.snap(open);
        self.dirty = true;
    }

    /// The active environment's variable map (for `{{var}}` substitution).
    fn env_vars(&self) -> HashMap<String, String> {
        match self.active_env.and_then(|id| {
            self.environments.iter().find(|e| e.id == id)
        }) {
            Some(env) => vars::var_map(&env.variables),
            None => HashMap::new(),
        }
    }

    /// Resolve the active request against the active environment.
    fn outgoing(&self) -> Outgoing {
        let mut req = self.request.clone();
        req.url = self.url_input.value().to_string();
        req.body_text = self.body_input.value().to_string();
        http::build_outgoing(&req, &self.env_vars())
    }

    /// Kick off a real request on a worker thread.
    fn send(&mut self) {
        if self.sending {
            return;
        }
        let out = self.outgoing();
        self.sending = true;
        self.error = None;
        self.dirty = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = match http::send_blocking(&out) {
                Ok(r) => SendOutcome::Ok(r),
                Err(e) => SendOutcome::Err(e),
            };
            let _ = tx.send(outcome);
        });
    }

    /// Drain any finished send result from the worker.
    fn poll_worker(&mut self) {
        while let Ok(outcome) = self.rx.try_recv() {
            self.sending = false;
            self.dirty = true;
            match outcome {
                SendOutcome::Ok(resp) => {
                    let out = self.outgoing();
                    self.history.insert(
                        0,
                        HistoryEntry {
                            id: uuid::Uuid::new_v4(),
                            method: self.request.method,
                            url: out.url.clone(),
                            status: resp.status,
                            duration_ms: resp.duration_ms,
                            when: "just now".into(),
                        },
                    );
                    self.history.truncate(50);
                    self.response = Some(resp);
                    self.response_view = ResponseView::Pretty;
                    self.collapsed.clear();
                    self.body_scroll = 0.0;
                }
                SendOutcome::Err(e) => {
                    self.error = Some(e);
                }
            }
        }
    }

    /// Load a saved request from a collection into the builder.
    fn load_request(&mut self, ci: usize, ri: usize) {
        if let Some(req) = self
            .collections
            .get(ci)
            .and_then(|c| c.requests.get(ri))
            .cloned()
        {
            self.url_input.set_text(&req.url);
            self.body_input.set_text(&req.body_text);
            self.request = req;
            self.selected = Some((ci, ri));
            self.active_tab = RequestTab::Params;
            self.dirty = true;
        }
    }

    /// Push the whole workspace into the shared CRDT.
    fn persist(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Keep the structured request snapshot in step with the live URL /
        // body buffers before serialising.
        let mut snap = self.request.clone();
        snap.url = self.url_input.value().to_string();
        snap.body_text = self.body_input.value().to_string();
        ApiDoc::set_request(state, root, &snap);
        ApiDoc::set_collections(state, root, &self.collections);
        ApiDoc::set_environments(state, root, &self.environments);
        ApiDoc::set_active_env(
            state,
            root,
            self.active_env.map(|id| id.to_string()).as_deref(),
        );
        ApiDoc::set_history(state, root, &self.history);
    }

    /// Adopt a peer's workspace from the shared CRDT.
    fn adopt(&mut self, state: &AutoCommit, root: &ObjId) {
        let cols = ApiDoc::collections(state, root);
        if !cols.is_empty() {
            self.collections = cols;
        }
        let envs = ApiDoc::environments(state, root);
        if !envs.is_empty() {
            self.environments = envs;
        }
        if let Some(id) = ApiDoc::active_env(state, root) {
            self.active_env = id.parse().ok();
        }
        if let Some(req) = ApiDoc::request(state, root) {
            self.url_input.set_text(&req.url);
            self.body_input.set_text(&req.body_text);
            self.request = req;
        }
        let hist = ApiDoc::history(state, root);
        if !hist.is_empty() {
            self.history = hist;
        }
        self.adopted = true;
        self.dirty = true;
    }
}

impl Default for ApiTesterApp {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Color helpers — every colour is read from the Theme, then tinted.
// ===========================================================================

/// The accent colour for an HTTP method, derived from the theme's
/// semantic palette so it tracks the OS theme.
fn method_color(theme: &Theme, m: HttpMethod) -> Color {
    let c = &theme.tokens.colors;
    match m {
        HttpMethod::Get => c.semantic.success,
        HttpMethod::Post => c.semantic.info,
        HttpMethod::Put => c.semantic.warning,
        HttpMethod::Patch => c.accent_hover_or_primary(),
        HttpMethod::Delete => c.semantic.danger,
        HttpMethod::Head | HttpMethod::Options => c.text_dim,
    }
}

/// Tiny extension so `method_color` can reach the accent ramp cleanly.
trait AccentPick {
    fn accent_hover_or_primary(&self) -> Color;
}
impl AccentPick for ui::tokens::colors::Colors {
    fn accent_hover_or_primary(&self) -> Color {
        self.accent[6]
    }
}

fn status_color(theme: &Theme, class: StatusClass) -> Color {
    let c = &theme.tokens.colors;
    match class {
        StatusClass::Success => c.semantic.success,
        StatusClass::Redirect => c.semantic.info,
        StatusClass::ClientError => c.semantic.warning,
        StatusClass::ServerError => c.semantic.danger,
        StatusClass::Other => c.text_dim,
    }
}

fn token_color(theme: &Theme, kind: vars::TokenKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        vars::TokenKind::Key => c.semantic.info,
        vars::TokenKind::StringValue => c.semantic.success,
        vars::TokenKind::Number => c.accent[6],
        vars::TokenKind::Keyword => c.semantic.warning,
        vars::TokenKind::Punctuation => c.text_dim,
        vars::TokenKind::Plain => c.text_primary,
    }
}

impl ApiTesterApp {
    // -------------------------------------------------------------------
    // Drawing
    // -------------------------------------------------------------------

    fn paint(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.hits = Hits::default();
        let c = &theme.tokens.colors;

        // Whole-window background.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);

        // Resolve docked vs. overlay from the allocated width and advance the
        // eased reveal toward its target. The rail is the same width either
        // way; only its placement (inset beside vs. slid over) differs.
        let layout = self.rail.resolve(self.rect.width());
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.rail.tick(dt);

        let reveal = self.rail.reveal();
        let slide = self.rail.slide();
        let sw = SIDEBAR_W.min(self.rect.width());

        match layout {
            SplitLayout::Docked => {
                // The builder is inset by the eased rail width so showing /
                // hiding the rail reflows without ever overlapping it.
                let inset = sw * reveal;
                let sidebar = Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x0 + inset,
                    self.rect.y1,
                );
                let main = Rect::new(sidebar.x1, self.rect.y0, self.rect.x1, self.rect.y1);
                self.draw_main(scene, t, tr, theme, presence, main);
                if reveal > 0.001 {
                    self.draw_sidebar(scene, t, tr, theme, sidebar);
                }
            }
            SplitLayout::Overlay => {
                // The builder spans the full width; the rail slides in from the
                // left over a dimming scrim, on top of the builder.
                let main = self.rect;
                self.draw_main(scene, t, tr, theme, presence, main);

                if reveal > 0.001 {
                    let scrim = self.rect;
                    scene.fill(
                        Fill::NonZero,
                        t,
                        Color::from_rgba8(0, 0, 0, 255).with_alpha_factor((reveal * 0.5) as f32),
                        None,
                        &scrim,
                    );
                    self.hits.scrim = scrim;

                    let x0 = self.rect.x0 - sw * (1.0 - slide);
                    let sidebar = Rect::new(x0, self.rect.y0, x0 + sw, self.rect.y1);
                    // A soft shadow on the rail's trailing edge sells the
                    // "floating over content" depth.
                    let shadow = Rect::new(sidebar.x1, sidebar.y0, sidebar.x1 + 8.0, sidebar.y1);
                    scene.fill(
                        Fill::NonZero,
                        t,
                        Color::from_rgba8(0, 0, 0, 255).with_alpha_factor((reveal * 0.18) as f32),
                        None,
                        &shadow,
                    );
                    self.draw_sidebar(scene, t, tr, theme, sidebar);
                }
            }
        }
    }

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        self.hits.sidebar = area;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &area);
        // Right divider.
        let div = Rect::new(area.x1 - 1.0, area.y0, area.x1, area.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &div);

        let mut y = area.y0 + PAD;
        let inner_w = area.width() - PAD * 2.0;

        // App title.
        tr.draw_text_weighted(
            scene,
            "API Tester",
            theme.tokens.typography.scale.lg,
            c.text_primary,
            t * Affine::translate((area.x0 + PAD, y)),
            None,
            TextWeight::Bold,
        );
        y += 30.0;

        // Environment selector.
        let env_rect = Rect::new(area.x0 + PAD, y, area.x1 - PAD, y + 34.0);
        let env_name = self
            .active_env
            .and_then(|id| self.environments.iter().find(|e| e.id == id))
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "No Environment".into());
        self.draw_select(scene, t, tr, theme, env_rect, "ENV", &env_name, self.env_open);
        self.hits.env_btn = env_rect;
        y += 50.0;

        // --- Collections ---
        self.draw_section_label(scene, t, tr, theme, area.x0 + PAD, y, "COLLECTIONS");
        y += 24.0;
        let collections = self.collections.clone();
        for (ci, col) in collections.iter().enumerate() {
            let row = Rect::new(area.x0 + 8.0, y, area.x1 - 8.0, y + ROW_H);
            // Caret + folder + name.
            let caret = if col.expanded { "▾" } else { "▸" };
            tr.draw_text(
                scene,
                caret,
                12.0,
                c.text_dim,
                t * Affine::translate((row.x0 + 6.0, y + 7.0)),
                None,
            );
            tr.draw_text_weighted(
                scene,
                &col.name,
                theme.tokens.typography.scale.base,
                c.text_primary,
                t * Affine::translate((row.x0 + 24.0, y + 6.0)),
                Some((inner_w - 60.0) as f32),
                TextWeight::Semibold,
            );
            // Request-count badge.
            let count = col.requests.len().to_string();
            let (cw, _) = tr.measure(&count, 11.0, None);
            tr.draw_text(
                scene,
                &count,
                11.0,
                c.text_dim,
                t * Affine::translate((row.x1 - cw as f64 - 6.0, y + 8.0)),
                None,
            );
            self.hits.collection_rows.push((row, ci));
            y += ROW_H;

            if col.expanded {
                for (ri, req) in col.requests.iter().enumerate() {
                    let rrow = Rect::new(area.x0 + 8.0, y, area.x1 - 8.0, y + ROW_H);
                    let is_sel = self.selected == Some((ci, ri));
                    if is_sel {
                        let shape = RoundedRect::from_rect(
                            Rect::new(rrow.x0, rrow.y0 + 2.0, rrow.x1, rrow.y1 - 2.0),
                            theme.tokens.radii.md as f64,
                        );
                        scene.fill(
                            Fill::NonZero,
                            t,
                            theme.accent_primary.with_alpha_factor(0.18),
                            None,
                            &shape,
                        );
                    }
                    // Method tag.
                    let mcol = method_color(theme, req.method);
                    tr.draw_text_weighted(
                        scene,
                        method_short(req.method),
                        10.0,
                        mcol,
                        t * Affine::translate((rrow.x0 + 28.0, y + 9.0)),
                        None,
                        TextWeight::Bold,
                    );
                    tr.draw_text(
                        scene,
                        &req.name,
                        theme.tokens.typography.scale.sm,
                        if is_sel { c.text_primary } else { c.text_dim },
                        t * Affine::translate((rrow.x0 + 64.0, y + 8.0)),
                        Some((inner_w - 72.0) as f32),
                    );
                    self.hits.request_rows.push((rrow, ci, ri));
                    y += ROW_H;
                }
            }
        }

        y += 12.0;
        // --- History ---
        self.draw_section_label(scene, t, tr, theme, area.x0 + PAD, y, "HISTORY");
        y += 24.0;
        let history = self.history.clone();
        for (hi, h) in history.iter().enumerate() {
            if y + ROW_H > area.y1 - PAD {
                break;
            }
            let hrow = Rect::new(area.x0 + 8.0, y, area.x1 - 8.0, y + ROW_H);
            let mcol = method_color(theme, h.method);
            tr.draw_text_weighted(
                scene,
                method_short(h.method),
                10.0,
                mcol,
                t * Affine::translate((hrow.x0 + 6.0, y + 9.0)),
                None,
                TextWeight::Bold,
            );
            // Trim the URL to its path tail for readability.
            let label = short_path(&h.url);
            tr.draw_text(
                scene,
                &label,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((hrow.x0 + 46.0, y + 8.0)),
                Some((inner_w - 90.0) as f32),
            );
            let scol = status_color(theme, status_class(h.status));
            let st = h.status.to_string();
            let (sw, _) = tr.measure(&st, 11.0, None);
            tr.draw_text_weighted(
                scene,
                &st,
                11.0,
                scol,
                t * Affine::translate((hrow.x1 - sw as f64 - 6.0, y + 8.0)),
                None,
                TextWeight::Semibold,
            );
            self.hits.history_rows.push((hrow, hi));
            y += ROW_H;
        }
    }

    fn draw_section_label(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        text: &str,
    ) {
        tr.draw_text_weighted(
            scene,
            text,
            10.0,
            theme.tokens.colors.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Bold,
        );
    }

    /// A compact labelled select control (used for the env picker).
    fn draw_select(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        value: &str,
        open: bool,
    ) {
        let c = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &shape);
        let border = if open { theme.border_focus } else { c.border_standard };
        scene.stroke(&Stroke::new(1.0), t, border, None, &shape);
        tr.draw_text_weighted(
            scene,
            label,
            9.0,
            c.text_dim,
            t * Affine::translate((rect.x0 + 10.0, rect.y0 + 5.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            value,
            theme.tokens.typography.scale.sm,
            c.text_primary,
            t * Affine::translate((rect.x0 + 10.0, rect.y0 + 16.0)),
            Some((rect.width() - 36.0) as f32),
            TextWeight::Semibold,
        );
        draw_chevron(scene, t, Point::new(rect.x1 - 16.0, rect.center().y), c.text_dim, open);
    }

    fn draw_main(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;

        // --- URL bar row ---
        let bar = Rect::new(area.x0, area.y0, area.x1, area.y0 + URLBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        let bar_div = Rect::new(area.x0, bar.y1 - 1.0, area.x1, bar.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &bar_div);

        let cy0 = bar.y0 + (URLBAR_H - 36.0) / 2.0;

        // Hamburger toggle — only in overlay mode, where the rail is a flap
        // the user must summon. It leads the URL row and shifts the method
        // button right to make room. In docked mode the rail is always beside
        // the builder, so no toggle is needed.
        let mut row_x0 = area.x0 + PAD;
        if self.rail.layout() == SplitLayout::Overlay {
            let ham = Rect::new(row_x0, cy0, row_x0 + HAMBURGER_W, cy0 + 36.0);
            let shape = RoundedRect::from_rect(ham, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, c.surface_white_5, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_hamburger(scene, t, ham, c.text_primary);
            self.hits.hamburger = ham;
            row_x0 = ham.x1 + 8.0;
        }

        // Method button.
        let method_btn = Rect::new(row_x0, cy0, row_x0 + METHOD_W, cy0 + 36.0);
        let mcol = method_color(theme, self.request.method);
        let shape = RoundedRect::from_rect(method_btn, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, mcol.with_alpha_factor(0.16), None, &shape);
        scene.stroke(&Stroke::new(1.0), t, mcol.with_alpha_factor(0.5), None, &shape);
        tr.draw_text_weighted(
            scene,
            self.request.method.as_str(),
            theme.tokens.typography.scale.sm,
            mcol,
            t * Affine::translate((method_btn.x0 + 12.0, method_btn.center().y - 7.0)),
            None,
            TextWeight::Bold,
        );
        draw_chevron(
            scene,
            t,
            Point::new(method_btn.x1 - 14.0, method_btn.center().y),
            mcol,
            self.method_open,
        );
        self.hits.method_btn = method_btn;

        // Send button (right).
        let send_btn = Rect::new(area.x1 - PAD - SEND_W, cy0, area.x1 - PAD, cy0 + 36.0);
        let send_shape = RoundedRect::from_rect(send_btn, theme.tokens.radii.md as f64);
        let send_bg = if self.sending {
            theme.accent_primary.with_alpha_factor(0.5)
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, t, send_bg, None, &send_shape);
        let send_label = if self.sending { "Sending…" } else { "Send" };
        let (slw, _) = tr.measure(send_label, theme.tokens.typography.scale.sm, None);
        tr.draw_text_weighted(
            scene,
            send_label,
            theme.tokens.typography.scale.sm,
            Color::WHITE,
            t * Affine::translate((
                send_btn.center().x - slw as f64 / 2.0,
                send_btn.center().y - 7.0,
            )),
            None,
            TextWeight::Bold,
        );
        self.hits.send_btn = send_btn;

        // URL input between method and send.
        let url_rect = Rect::new(method_btn.x1 + 8.0, cy0, send_btn.x0 - 8.0, cy0 + 36.0);
        self.hits.url_rect = url_rect;
        // Field chrome.
        let url_shape = RoundedRect::from_rect(url_rect, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &url_shape);
        scene.stroke(
            &Stroke::new(if self.url_input.is_focused() { 2.0 } else { 1.0 }),
            t,
            if self.url_input.is_focused() { theme.border_focus } else { c.border_standard },
            None,
            &url_shape,
        );
        // Position the live TextInput over the field we just drew, then
        // paint it inside (plain, no own chrome). Placing it here — rather
        // than relying on a prior layout pass — means the very first frame
        // (e.g. the headless screenshot) already shows the URL.
        self.url_input.apply_layout_external(url_rect);
        self.url_input.draw(scene, t, tr, theme, presence);
        // Resolved-URL hint under the bar if the URL contains variables.
        // (kept subtle: shows the substituted preview to the right of Send
        // is too crowded, so we render it on the tab strip's right edge.)

        // --- Tab strip ---
        let strip = Rect::new(area.x0, bar.y1, area.x1, bar.y1 + TAB_STRIP_H);
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &strip);
        let mut tx = area.x0 + PAD;
        for tab in RequestTab::ALL {
            let label = tab.label();
            let (lw, _) = tr.measure(label, theme.tokens.typography.scale.base, None);
            let count = self.tab_count(tab);
            let badge_w = if count > 0 { 22.0 } else { 0.0 };
            let tab_rect = Rect::new(tx, strip.y0, tx + lw as f64 + 16.0 + badge_w, strip.y1);
            let active = self.active_tab == tab;
            // Dim the Body tab for verbs that conventionally carry no body.
            let muted = tab == RequestTab::Body && !self.request.method.has_body();
            let col = if active {
                c.text_primary
            } else if muted {
                c.text_dim.with_alpha_factor(0.5)
            } else {
                c.text_dim
            };
            tr.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.base,
                col,
                t * Affine::translate((tx + 8.0, strip.center().y - 7.0)),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            if count > 0 {
                let bx = tx + 8.0 + lw as f64 + 6.0;
                let badge = RoundedRect::from_rect(
                    Rect::new(bx, strip.center().y - 8.0, bx + 18.0, strip.center().y + 8.0),
                    8.0,
                );
                scene.fill(Fill::NonZero, t, c.surface_white_10, None, &badge);
                let cs = count.to_string();
                let (csw, _) = tr.measure(&cs, 10.0, None);
                tr.draw_text(
                    scene,
                    &cs,
                    10.0,
                    col,
                    t * Affine::translate((bx + 9.0 - csw as f64 / 2.0, strip.center().y - 6.0)),
                    None,
                );
            }
            if active {
                let underline = Rect::new(tab_rect.x0 + 4.0, strip.y1 - 2.0, tab_rect.x1 - 4.0, strip.y1);
                scene.fill(Fill::NonZero, t, theme.accent_primary, None, &underline);
            }
            self.hits.tabs.push((tab_rect, tab));
            tx = tab_rect.x1 + 4.0;
        }

        // Right of the tab strip: a hint about `{{var}}` resolution against
        // the active environment, so the user sees which placeholders are
        // (un)resolved before they send.
        let url_now = self.url_input.value().to_string();
        let refs = vars::referenced_vars(&url_now);
        if !refs.is_empty() {
            let env = self.env_vars();
            let unresolved: Vec<&String> = refs.iter().filter(|r| !env.contains_key(*r)).collect();
            let (hint, hint_col) = if unresolved.is_empty() {
                (
                    format!("{} variable(s) resolved", refs.len()),
                    c.semantic.success,
                )
            } else {
                (
                    format!("unresolved: {}", unresolved.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
                    c.semantic.warning,
                )
            };
            let (hw, _) = tr.measure(&hint, theme.tokens.typography.scale.sm, None);
            tr.draw_text(
                scene,
                &hint,
                theme.tokens.typography.scale.sm,
                hint_col,
                t * Affine::translate((area.x1 - PAD - hw as f64, strip.center().y - 7.0)),
                None,
            );
        }

        let strip_div = Rect::new(area.x0, strip.y1 - 1.0, area.x1, strip.y1);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &strip_div);

        // --- Split: editor (top) / response (bottom) ---
        let editor_h = ((area.height() - URLBAR_H - TAB_STRIP_H) * 0.42).max(150.0);
        let editor = Rect::new(area.x0, strip.y1, area.x1, strip.y1 + editor_h);
        self.draw_editor(scene, t, tr, theme, presence, editor);

        let resp_area = Rect::new(area.x0, editor.y1, area.x1, area.y1);
        self.draw_response(scene, t, tr, theme, resp_area);

        // --- Overlays (drawn last so they sit on top) ---
        if self.method_open {
            self.draw_method_menu(scene, t, tr, theme);
        }
        if self.env_open {
            self.draw_env_menu(scene, t, tr, theme);
        }
    }

    /// Number shown on a request-builder tab badge.
    fn tab_count(&self, tab: RequestTab) -> usize {
        match tab {
            RequestTab::Params => Request::enabled_count(&self.request.params),
            RequestTab::Headers => Request::enabled_count(&self.request.headers),
            RequestTab::Auth => {
                if matches!(self.request.auth, Auth::None) {
                    0
                } else {
                    1
                }
            }
            RequestTab::Body => {
                if matches!(self.request.body_mode, BodyMode::None) {
                    0
                } else {
                    1
                }
            }
        }
    }

    fn draw_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &area);
        match self.active_tab {
            RequestTab::Params => {
                let rows = self.request.params.clone();
                self.draw_kv_table(scene, t, tr, theme, area, &rows, "No query parameters");
            }
            RequestTab::Headers => {
                let rows = self.request.headers.clone();
                self.draw_kv_table(scene, t, tr, theme, area, &rows, "No headers");
            }
            RequestTab::Auth => self.draw_auth(scene, t, tr, theme, area),
            RequestTab::Body => self.draw_body(scene, t, tr, theme, presence, area),
        }
    }

    /// Draw a header-row + value-rows key/value table (params / headers).
    fn draw_kv_table(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        rows: &[KeyValue],
        empty_msg: &str,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 12.0;
        let key_x = x0 + 30.0;
        let val_x = area.x0 + area.width() * 0.5;
        // Column header.
        tr.draw_text_weighted(scene, "KEY", 10.0, c.text_dim, t * Affine::translate((key_x, y)), None, TextWeight::Bold);
        tr.draw_text_weighted(scene, "VALUE", 10.0, c.text_dim, t * Affine::translate((val_x, y)), None, TextWeight::Bold);
        y += 22.0;

        if rows.iter().all(|r| r.is_blank()) {
            tr.draw_text(
                scene,
                empty_msg,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((x0, y + 8.0)),
                None,
            );
            return;
        }

        for row in rows {
            if y + ROW_H > area.y1 {
                break;
            }
            // Checkbox.
            let cb = Rect::new(x0, y + 4.0, x0 + 16.0, y + 20.0);
            let cb_shape = RoundedRect::from_rect(cb, 4.0);
            if row.enabled {
                scene.fill(Fill::NonZero, t, theme.accent_primary, None, &cb_shape);
                draw_check(scene, t, cb, Color::WHITE);
            } else {
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &cb_shape);
            }
            let row_col = if row.enabled { c.text_primary } else { c.text_dim };
            tr.draw_text(
                scene,
                &row.key,
                theme.tokens.typography.scale.sm,
                row_col,
                t * Affine::translate((key_x, y + 7.0)),
                Some((val_x - key_x - 12.0) as f32),
            );
            // Value, with variable placeholders tinted.
            self.draw_value_with_vars(scene, t, tr, theme, &row.value, val_x, y + 7.0, area.x1 - PAD - val_x, row.enabled);
            // Row separator.
            let sep = Rect::new(x0, y + ROW_H - 1.0, area.x1 - PAD, y + ROW_H);
            scene.fill(Fill::NonZero, t, c.border_standard.with_alpha_factor(0.4), None, &sep);
            y += ROW_H;
        }

        // Trailing "add a pair" affordance — a ghost blank row that reads
        // as the next slot the user can fill in.
        if y + ROW_H <= area.y1 {
            let add = KeyValue::blank();
            let cb = Rect::new(x0, y + 4.0, x0 + 16.0, y + 20.0);
            scene.stroke(
                &Stroke::new(1.0),
                t,
                c.border_standard.with_alpha_factor(0.5),
                None,
                &RoundedRect::from_rect(cb, 4.0),
            );
            tr.draw_text(
                scene,
                if add.is_blank() { "New key" } else { &add.key },
                theme.tokens.typography.scale.sm,
                c.text_dim.with_alpha_factor(0.6),
                t * Affine::translate((key_x, y + 7.0)),
                None,
            );
            tr.draw_text(
                scene,
                "New value",
                theme.tokens.typography.scale.sm,
                c.text_dim.with_alpha_factor(0.6),
                t * Affine::translate((val_x, y + 7.0)),
                None,
            );
        }
    }

    /// Draw a value string, tinting `{{var}}` placeholders with the accent.
    #[allow(clippy::too_many_arguments)]
    fn draw_value_with_vars(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        value: &str,
        x: f64,
        y: f64,
        max_w: f64,
        enabled: bool,
    ) {
        let c = &theme.tokens.colors;
        let base_col = if enabled { c.text_primary } else { c.text_dim };
        let size = theme.tokens.typography.scale.sm;
        // Simple split on {{...}} runs.
        let mut cx = x;
        let mut rest = value;
        let limit = x + max_w;
        while !rest.is_empty() && cx < limit {
            if let Some(open) = rest.find("{{") {
                if open > 0 {
                    let plain = &rest[..open];
                    tr.draw_text(scene, plain, size, base_col, t * Affine::translate((cx, y)), None);
                    let (w, _) = tr.measure(plain, size, None);
                    cx += w as f64;
                }
                if let Some(close) = rest[open..].find("}}") {
                    let var = &rest[open..open + close + 2];
                    tr.draw_text_weighted(scene, var, size, theme.accent_primary, t * Affine::translate((cx, y)), None, TextWeight::Semibold);
                    let (w, _) = tr.measure(var, size, None);
                    cx += w as f64;
                    rest = &rest[open + close + 2..];
                    continue;
                }
            }
            tr.draw_text(scene, rest, size, base_col, t * Affine::translate((cx, y)), Some((limit - cx) as f32));
            break;
        }
    }

    fn draw_auth(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 16.0;
        tr.draw_text_weighted(scene, "AUTH TYPE", 10.0, c.text_dim, t * Affine::translate((x0, y)), None, TextWeight::Bold);
        y += 20.0;

        // Mode chips.
        let mut cx = x0;
        for label in Auth::LABELS {
            let active = self.request.auth.label() == label;
            let (lw, _) = tr.measure(label, theme.tokens.typography.scale.sm, None);
            let chip = Rect::new(cx, y, cx + lw as f64 + 24.0, y + 30.0);
            let shape = RoundedRect::from_rect(chip, theme.tokens.radii.full as f64);
            if active {
                scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
            } else {
                scene.fill(Fill::NonZero, t, c.surface_white_5, None, &shape);
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            tr.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                if active { Color::WHITE } else { c.text_dim },
                t * Affine::translate((cx + 12.0, y + 8.0)),
                None,
                TextWeight::Semibold,
            );
            cx = chip.x1 + 8.0;
        }
        y += 50.0;

        // Auth detail fields.
        let draw_field = |scene: &mut Scene, tr: &mut TextRenderer, y: f64, label: &str, value: &str, secret: bool| {
            tr.draw_text_weighted(scene, label, 10.0, c.text_dim, t * Affine::translate((x0, y)), None, TextWeight::Bold);
            let field = Rect::new(x0, y + 16.0, area.x1 - PAD, y + 50.0);
            let shape = RoundedRect::from_rect(field, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            let shown = if secret && !value.is_empty() {
                "•".repeat(value.chars().count().min(24))
            } else {
                value.to_string()
            };
            tr.draw_text(
                scene,
                &shown,
                theme.tokens.typography.scale.sm,
                c.text_primary,
                t * Affine::translate((field.x0 + 12.0, field.center().y - 7.0)),
                Some((field.width() - 24.0) as f32),
            );
        };

        match &self.request.auth {
            Auth::None => {
                tr.draw_text(
                    scene,
                    "This request does not use authentication.",
                    theme.tokens.typography.scale.sm,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            Auth::Bearer { token } => draw_field(scene, tr, y, "TOKEN", token, true),
            Auth::Basic { username, password } => {
                draw_field(scene, tr, y, "USERNAME", username, false);
                draw_field(scene, tr, y + 70.0, "PASSWORD", password, true);
            }
            Auth::ApiKey { header, value } => {
                draw_field(scene, tr, y, "HEADER NAME", header, false);
                draw_field(scene, tr, y + 70.0, "VALUE", value, true);
            }
        }
    }

    fn draw_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 12.0;

        // Body-mode chips.
        let mut cx = x0;
        for mode in BodyMode::ALL {
            let active = self.request.body_mode == mode;
            let label = mode.label();
            let (lw, _) = tr.measure(label, theme.tokens.typography.scale.sm, None);
            let chip = Rect::new(cx, y, cx + lw as f64 + 20.0, y + 28.0);
            let shape = RoundedRect::from_rect(chip, theme.tokens.radii.md as f64);
            if active {
                scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.2), None, &shape);
                scene.stroke(&Stroke::new(1.0), t, theme.accent_primary, None, &shape);
            } else {
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            tr.draw_text_weighted(
                scene,
                label,
                theme.tokens.typography.scale.sm,
                if active { c.text_primary } else { c.text_dim },
                t * Affine::translate((cx + 10.0, y + 6.0)),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            cx = chip.x1 + 8.0;
        }
        y += 40.0;

        match self.request.body_mode {
            BodyMode::None => {
                tr.draw_text(
                    scene,
                    "This request does not have a body.",
                    theme.tokens.typography.scale.sm,
                    c.text_dim,
                    t * Affine::translate((x0, y)),
                    None,
                );
            }
            BodyMode::Form => {
                let rows = self.request.form.clone();
                let sub = Rect::new(area.x0, y - 8.0, area.x1, area.y1);
                self.draw_kv_table(scene, t, tr, theme, sub, &rows, "No form fields");
            }
            BodyMode::Json | BodyMode::Raw => {
                let field = Rect::new(x0, y, area.x1 - PAD, area.y1 - 12.0);
                let shape = RoundedRect::from_rect(field, theme.tokens.radii.md as f64);
                scene.fill(Fill::NonZero, t, theme.tokens.colors.neutral[1], None, &shape);
                scene.stroke(
                    &Stroke::new(if self.body_input.is_focused() { 2.0 } else { 1.0 }),
                    t,
                    if self.body_input.is_focused() { theme.border_focus } else { c.border_standard },
                    None,
                    &shape,
                );
                self.hits.body_input_rect = field;
                self.body_input.apply_layout_external(field);
                self.body_input.draw(scene, t, tr, theme, presence);
            }
        }
    }

    fn draw_response(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &area);
        let top_div = Rect::new(area.x0, area.y0, area.x1, area.y0 + 1.0);
        scene.fill(Fill::NonZero, t, c.border_standard, None, &top_div);

        // Status row.
        let row = Rect::new(area.x0, area.y0, area.x1, area.y0 + 44.0);
        let x0 = area.x0 + PAD;
        let cy = row.center().y;

        if let Some(resp) = self.response.clone() {
            let scol = status_color(theme, resp.status_class());
            // Status pill.
            let pill_label = format!("{} {}", resp.status, resp.status_text);
            let (pw, _) = tr.measure(&pill_label, theme.tokens.typography.scale.sm, None);
            let pill = Rect::new(x0, cy - 12.0, x0 + pw as f64 + 24.0, cy + 12.0);
            let pill_shape = RoundedRect::from_rect(pill, theme.tokens.radii.full as f64);
            scene.fill(Fill::NonZero, t, scol.with_alpha_factor(0.18), None, &pill_shape);
            tr.draw_text_weighted(
                scene,
                &pill_label,
                theme.tokens.typography.scale.sm,
                scol,
                t * Affine::translate((pill.x0 + 12.0, cy - 7.0)),
                None,
                TextWeight::Bold,
            );
            // Time + size metrics.
            let meta = format!("{}   {}", human_duration(resp.duration_ms), human_size(resp.size_bytes));
            tr.draw_text(
                scene,
                &meta,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((pill.x1 + 16.0, cy - 7.0)),
                None,
            );

            // View toggle (Pretty / Raw / Preview) on the right.
            let mut vx = area.x1 - PAD;
            for view in ResponseView::ALL.into_iter().rev() {
                let label = view.label();
                let (lw, _) = tr.measure(label, theme.tokens.typography.scale.sm, None);
                let vr = Rect::new(vx - lw as f64 - 20.0, cy - 12.0, vx, cy + 12.0);
                let active = self.response_view == view;
                if active {
                    let shape = RoundedRect::from_rect(vr, theme.tokens.radii.md as f64);
                    scene.fill(Fill::NonZero, t, c.surface_white_10, None, &shape);
                }
                tr.draw_text_weighted(
                    scene,
                    label,
                    theme.tokens.typography.scale.sm,
                    if active { c.text_primary } else { c.text_dim },
                    t * Affine::translate((vr.x0 + 10.0, cy - 7.0)),
                    None,
                    if active { TextWeight::Semibold } else { TextWeight::Regular },
                );
                self.hits.views.push((vr, view));
                vx = vr.x0 - 6.0;
            }

            // Body region.
            let body_area = Rect::new(area.x0, row.y1, area.x1, area.y1);
            self.hits.body_region = body_area;
            scene.fill(Fill::NonZero, t, theme.tokens.colors.neutral[1], None, &body_area);
            let body_div = Rect::new(area.x0, row.y1, area.x1, row.y1 + 1.0);
            scene.fill(Fill::NonZero, t, c.border_standard, None, &body_div);

            match self.response_view {
                ResponseView::Pretty => {
                    if resp.is_json() {
                        self.draw_json_body(scene, t, tr, theme, &resp, body_area);
                    } else {
                        self.draw_raw_body(scene, t, tr, theme, &resp.body, body_area);
                    }
                }
                ResponseView::Raw => self.draw_raw_body(scene, t, tr, theme, &resp.body, body_area),
                ResponseView::Preview => self.draw_response_headers(scene, t, tr, theme, &resp, body_area),
            }
        } else if let Some(err) = &self.error {
            tr.draw_text_weighted(
                scene,
                "Request failed",
                theme.tokens.typography.scale.base,
                c.semantic.danger,
                t * Affine::translate((x0, cy - 7.0)),
                None,
                TextWeight::Bold,
            );
            tr.draw_text(
                scene,
                err,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                t * Affine::translate((x0, row.y1 + 12.0)),
                Some((area.width() - PAD * 2.0) as f32),
            );
        } else {
            tr.draw_text(
                scene,
                "Send a request to see the response here.",
                theme.tokens.typography.scale.base,
                c.text_dim,
                t * Affine::translate((x0, cy - 7.0)),
                None,
            );
        }
    }

    /// Draw the pretty, syntax-highlighted, foldable JSON body.
    fn draw_json_body(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        resp: &ResponseData,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let pretty = vars::pretty_json(&resp.body).unwrap_or_else(|| resp.body.clone());
        let lines = vars::tokenize_json(&pretty);
        let size = theme.tokens.typography.scale.sm;
        let gutter_w = 44.0;
        let text_x0 = area.x0 + gutter_w + 8.0;

        // Clip body to the region.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &area);

        let mut screen_y = area.y0 + 8.0 - self.body_scroll;
        let mut line_no = 0usize;
        let mut hidden_until: Option<usize> = None;

        for (idx, line) in lines.iter().enumerate() {
            // Skip lines hidden inside a collapsed block.
            if let Some(close) = hidden_until {
                if idx <= close {
                    continue;
                } else {
                    hidden_until = None;
                }
            }

            line_no += 1;
            // Only paint visible lines (cheap cull).
            if screen_y + JSON_LINE_H >= area.y0 && screen_y <= area.y1 {
                // Gutter line number.
                let ln = line_no.to_string();
                let (lnw, _) = tr.measure(&ln, 11.0, None);
                tr.draw_text(
                    scene,
                    &ln,
                    11.0,
                    c.text_dim.with_alpha_factor(0.6),
                    t * Affine::translate((area.x0 + gutter_w - lnw as f64 - 6.0, screen_y + 3.0)),
                    None,
                );

                // Fold caret for openers.
                let collapsed = self.collapsed.contains(&idx);
                if line.opens_block && line.close_line.is_some() {
                    let caret_pt = Point::new(
                        area.x0 + gutter_w - 2.0,
                        screen_y + JSON_LINE_H / 2.0,
                    );
                    draw_fold_caret(scene, t, caret_pt, c.text_dim, !collapsed);
                    let caret_rect = Rect::new(
                        area.x0 + gutter_w - 10.0,
                        screen_y,
                        area.x0 + gutter_w + 6.0,
                        screen_y + JSON_LINE_H,
                    );
                    self.hits.fold_carets.push((caret_rect, idx));
                }

                // Tokens. Advance the pen with the SAME monospace metrics
                // used to draw, so coloured runs butt up cleanly instead of
                // overlapping.
                let mut cx = text_x0;
                for tok in &line.tokens {
                    let col = token_color(theme, tok.kind);
                    tr.draw_text_styled(
                        scene,
                        &tok.text,
                        size,
                        col,
                        t * Affine::translate((cx, screen_y + 3.0)),
                        None,
                        TextWeight::Regular,
                        false,
                        true, // monospace for code-like body
                    );
                    let (w, _) = tr.measure_styled(&tok.text, size, None, TextWeight::Regular, false, true);
                    cx += w as f64;
                }

                // Collapsed summary indicator.
                if collapsed && line.opens_block {
                    tr.draw_text(
                        scene,
                        " … ",
                        size,
                        c.text_dim,
                        t * Affine::translate((cx, screen_y + 3.0)),
                        None,
                    );
                }
            }

            // If this opener is collapsed, hide everything up to its closer.
            if line.opens_block && self.collapsed.contains(&idx) {
                if let Some(close) = line.close_line {
                    hidden_until = Some(close);
                }
            }

            screen_y += JSON_LINE_H;
        }

        scene.pop_layer();
    }

    fn draw_raw_body(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: &str,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &area);
        let size = theme.tokens.typography.scale.sm;
        let mut y = area.y0 + 8.0 - self.body_scroll;
        for line in body.lines() {
            if y + JSON_LINE_H >= area.y0 && y <= area.y1 {
                tr.draw_text_styled(
                    scene,
                    line,
                    size,
                    c.text_primary,
                    t * Affine::translate((area.x0 + PAD, y + 3.0)),
                    None,
                    TextWeight::Regular,
                    false,
                    true,
                );
            }
            y += JSON_LINE_H;
        }
        scene.pop_layer();
    }

    /// The "Preview" view shows the response headers (and a content note).
    fn draw_response_headers(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        resp: &ResponseData,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let mut y = area.y0 + 12.0;
        tr.draw_text_weighted(
            scene,
            "RESPONSE HEADERS",
            10.0,
            c.text_dim,
            t * Affine::translate((x0, y)),
            None,
            TextWeight::Bold,
        );
        y += 24.0;
        let key_w = (area.width() * 0.32).min(280.0);
        for h in &resp.headers {
            if y + 26.0 > area.y1 {
                break;
            }
            tr.draw_text_weighted(
                scene,
                &h.key,
                theme.tokens.typography.scale.sm,
                c.semantic.info,
                t * Affine::translate((x0, y)),
                Some((key_w - 12.0) as f32),
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &h.value,
                theme.tokens.typography.scale.sm,
                c.text_primary,
                t * Affine::translate((x0 + key_w, y)),
                Some((area.width() - PAD * 2.0 - key_w) as f32),
            );
            let sep = Rect::new(x0, y + 22.0, area.x1 - PAD, y + 23.0);
            scene.fill(Fill::NonZero, t, c.border_standard.with_alpha_factor(0.4), None, &sep);
            y += 28.0;
        }
    }

    fn draw_method_menu(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let anchor = self.hits.method_btn;
        let item_h = 30.0;
        let menu = Rect::new(
            anchor.x0,
            anchor.y1 + 4.0,
            anchor.x0 + 160.0,
            anchor.y1 + 4.0 + item_h * HttpMethod::ALL.len() as f64,
        );
        let shape = RoundedRect::from_rect(menu, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &shape);
        let mut y = menu.y0;
        for m in HttpMethod::ALL {
            let item = Rect::new(menu.x0, y, menu.x1, y + item_h);
            if self.request.method == m {
                scene.fill(Fill::NonZero, t, c.surface_white_10, None, &item);
            }
            tr.draw_text_weighted(
                scene,
                m.as_str(),
                theme.tokens.typography.scale.sm,
                method_color(theme, m),
                t * Affine::translate((item.x0 + 14.0, y + 7.0)),
                None,
                TextWeight::Bold,
            );
            self.hits.method_items.push((item, m));
            y += item_h;
        }
        scene.pop_layer();
    }

    fn draw_env_menu(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let anchor = self.hits.env_btn;
        let item_h = 30.0;
        let n = self.environments.len() + 1; // + "No Environment"
        let menu = Rect::new(
            anchor.x0,
            anchor.y1 + 4.0,
            anchor.x1,
            anchor.y1 + 4.0 + item_h * n as f64,
        );
        let shape = RoundedRect::from_rect(menu, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &shape);
        let mut y = menu.y0;
        // None option.
        let none_item = Rect::new(menu.x0, y, menu.x1, y + item_h);
        if self.active_env.is_none() {
            scene.fill(Fill::NonZero, t, c.surface_white_10, None, &none_item);
        }
        tr.draw_text(scene, "No Environment", theme.tokens.typography.scale.sm, c.text_dim, t * Affine::translate((none_item.x0 + 12.0, y + 7.0)), None);
        self.hits.env_items.push((none_item, None));
        y += item_h;
        let envs = self.environments.clone();
        for e in envs {
            let item = Rect::new(menu.x0, y, menu.x1, y + item_h);
            if self.active_env == Some(e.id) {
                scene.fill(Fill::NonZero, t, c.surface_white_10, None, &item);
            }
            tr.draw_text(scene, &e.name, theme.tokens.typography.scale.sm, c.text_primary, t * Affine::translate((item.x0 + 12.0, y + 7.0)), None);
            self.hits.env_items.push((item, Some(e.id)));
            y += item_h;
        }
        scene.pop_layer();
    }

}

// ===========================================================================
// Small drawing primitives
// ===========================================================================

fn method_short(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PTCH",
        HttpMethod::Delete => "DEL",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPT",
    }
}

fn status_class(status: u16) -> StatusClass {
    match status {
        200..=299 => StatusClass::Success,
        300..=399 => StatusClass::Redirect,
        400..=499 => StatusClass::ClientError,
        500..=599 => StatusClass::ServerError,
        _ => StatusClass::Other,
    }
}

/// Strip the scheme/host, leaving the path tail for compact history labels.
fn short_path(url: &str) -> String {
    let no_scheme = url.split("://").nth(1).unwrap_or(url);
    match no_scheme.find('/') {
        Some(i) => no_scheme[i..].to_string(),
        None => no_scheme.to_string(),
    }
}

fn draw_chevron(scene: &mut Scene, t: Affine, center: Point, color: Color, up: bool) {
    let mut p = BezPath::new();
    let (cx, cy) = (center.x, center.y);
    if up {
        p.move_to((cx - 4.0, cy + 2.0));
        p.line_to((cx, cy - 2.0));
        p.line_to((cx + 4.0, cy + 2.0));
    } else {
        p.move_to((cx - 4.0, cy - 2.0));
        p.line_to((cx, cy + 2.0));
        p.line_to((cx + 4.0, cy - 2.0));
    }
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
}

/// Three stacked bars — the hamburger toggle that summons the overlay rail.
fn draw_hamburger(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let half = 8.0;
    for dy in [-6.0, 0.0, 6.0] {
        let bar = Rect::new(cx - half, cy + dy - 1.0, cx + half, cy + dy + 1.0);
        scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(bar, 1.0));
    }
}

fn draw_check(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    let mut p = BezPath::new();
    p.move_to((rect.x0 + 3.5, rect.center().y));
    p.line_to((rect.x0 + 6.5, rect.y1 - 4.0));
    p.line_to((rect.x1 - 3.5, rect.y0 + 4.0));
    scene.stroke(&Stroke::new(1.8), t, color, None, &p);
}

/// A triangle fold caret: pointing down when `expanded`, right when not.
fn draw_fold_caret(scene: &mut Scene, t: Affine, center: Point, color: Color, expanded: bool) {
    let mut p = BezPath::new();
    let (cx, cy) = (center.x, center.y);
    if expanded {
        p.move_to((cx - 3.0, cy - 2.0));
        p.line_to((cx + 3.0, cy - 2.0));
        p.line_to((cx, cy + 3.0));
    } else {
        p.move_to((cx - 2.0, cy - 3.0));
        p.line_to((cx + 3.0, cy));
        p.line_to((cx - 2.0, cy + 3.0));
    }
    p.close_path();
    scene.fill(Fill::NonZero, t, color, None, &p);
}

// ===========================================================================
// Widget impl
// ===========================================================================

impl Widget for ApiTesterApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Seed the shared document the first time, or adopt a peer's.
        if !self.adopted {
            if ApiDoc::is_empty(state, path) {
                if !self.seeded {
                    self.persist(state, path);
                    // Also seed the live URL / body Text objects.
                    let url = self.url_input.value().to_string();
                    let body = self.body_input.value().to_string();
                    ApiDoc::set_text(state, path, "url", &url);
                    ApiDoc::set_text(state, path, "body", &body);
                    self.seeded = true;
                    self.adopted = true;
                    self.persisted_once = true;
                }
            } else {
                self.adopt(state, path);
            }
        }
        // Bind the live URL / body inputs to their CRDT Text objects so
        // edits converge between collaborators.
        let url_obj = ApiDoc::url_text(state, path);
        self.url_input.bind_text(url_obj);
        let body_obj = ApiDoc::body_text(state, path);
        self.body_input.bind_text(body_obj);
        self.url_input.sync(state, path);
        self.body_input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app is a single full-window leaf; sub-regions are drawn and
        // hit-tested manually. The embedded inputs get their own nodes so
        // taffy tracks them, but we override their rects in apply_layout.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let _ = taffy;
        // Place the embedded inputs over the chrome rects computed last
        // frame (hits is populated during draw). On the very first frame
        // hits is empty; the inputs settle by the next frame.
        self.place_inputs();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.poll_worker();
        // Re-place inputs so they sit on the chrome the previous frame drew.
        self.place_inputs();
        self.paint(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Route keystrokes / clicks to a focused embedded input first.
        if self.url_input.is_focused() {
            self.url_input.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            if self.url_input.needs_commit() {
                self.url_input.on_commit();
            }
            if *handled {
                self.dirty = true;
                self.sync_request_from_inputs();
                self.persist(state, path);
                return;
            }
        }
        if self.body_input.is_focused() {
            self.body_input.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            if *handled {
                self.dirty = true;
                self.sync_request_from_inputs();
                self.persist(state, path);
                return;
            }
        }

        match event {
            // Esc dismisses the open overlay rail (after focused inputs have
            // had their chance to consume it above).
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.rail.layout() == SplitLayout::Overlay && self.rail.is_open() {
                    self.rail.close();
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.hits.body_region.contains(self.last_cursor) {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => *y as f64 * -40.0,
                        MouseScrollDelta::PixelDelta(p) => -p.y,
                    };
                    self.body_scroll = (self.body_scroll + dy).max(0.0);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                self.on_click(p, state, path, handled, focus_target, node_id);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ApiTesterApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.sending
            || self.rail.is_animating()
            || self.url_input.needs_redraw()
            || self.body_input.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the host pumping redraws while the rail slide / scrim fade is
        // still settling, so a toggle eases smoothly instead of jumping a
        // single frame and freezing.
        self.rail.is_animating()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.url_input.mark_drawn();
        self.body_input.mark_drawn();
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep the App pumping while a request is in flight so the worker's
        // result surfaces, and until we've adopted/seeded the shared doc.
        self.sending || !self.adopted
    }
}

impl ApiTesterApp {
    /// Place the embedded URL / body inputs over the chrome rects that the
    /// last draw resolved, so each input hit-tests and paints exactly
    /// inside the field the app drew for it.
    fn place_inputs(&mut self) {
        if self.hits.url_rect.width() > 0.0 {
            self.url_input.apply_layout_external(self.hits.url_rect);
        }
        if self.hits.body_input_rect.width() > 0.0 {
            self.body_input.apply_layout_external(self.hits.body_input_rect);
        }
    }

    /// Mirror the live URL / body buffers back into the structured request.
    fn sync_request_from_inputs(&mut self) {
        self.request.url = self.url_input.value().to_string();
        self.request.body_text = self.body_input.value().to_string();
    }

    /// Resolve a left-click against the resolved hit regions.
    fn on_click(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        // 1. Open menus consume clicks first.
        if self.method_open {
            for (rect, m) in self.hits.method_items.clone() {
                if rect.contains(p) {
                    self.request.method = m;
                    self.method_open = false;
                    *handled = true;
                    self.dirty = true;
                    self.persist(state, path);
                    return;
                }
            }
            self.method_open = false;
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.env_open {
            for (rect, id) in self.hits.env_items.clone() {
                if rect.contains(p) {
                    self.active_env = id;
                    self.env_open = false;
                    *handled = true;
                    self.dirty = true;
                    self.persist(state, path);
                    return;
                }
            }
            self.env_open = false;
            self.dirty = true;
            *handled = true;
            return;
        }

        // 1b. Adaptive chrome: the hamburger toggles the rail; clicking the
        // scrim outside the open rail dismisses it. Both run before the rest
        // so a scrim click can't fall through to the builder beneath.
        if self.hits.hamburger.width() > 0.0 && self.hits.hamburger.contains(p) {
            self.rail.toggle();
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.rail.layout() == SplitLayout::Overlay && self.rail.is_open() {
            let on_rail = self.hits.sidebar_area().contains(p);
            if !on_rail && self.hits.scrim.width() > 0.0 && self.hits.scrim.contains(p) {
                self.rail.close();
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 2. Method / env / send buttons.
        if self.hits.method_btn.contains(p) {
            self.method_open = true;
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.hits.env_btn.contains(p) {
            self.env_open = true;
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.hits.send_btn.contains(p) {
            self.send();
            *handled = true;
            return;
        }

        // 3. URL field focus.
        if self.hits.url_rect.contains(p) {
            self.url_input.set_focus(true);
            self.body_input.set_focus(false);
            *focus_target = Some(node_id);
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.hits.body_input_rect.width() > 0.0 && self.hits.body_input_rect.contains(p) {
            self.body_input.set_focus(true);
            self.url_input.set_focus(false);
            *focus_target = Some(node_id);
            self.dirty = true;
            *handled = true;
            return;
        }

        // 4. Request-builder tabs.
        for (rect, tab) in self.hits.tabs.clone() {
            if rect.contains(p) {
                self.active_tab = tab;
                self.url_input.set_focus(false);
                self.body_input.set_focus(false);
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 5. Response view toggle.
        for (rect, view) in self.hits.views.clone() {
            if rect.contains(p) {
                self.response_view = view;
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 6. JSON fold carets.
        for (rect, idx) in self.hits.fold_carets.clone() {
            if rect.contains(p) {
                if self.collapsed.contains(&idx) {
                    self.collapsed.remove(&idx);
                } else {
                    self.collapsed.insert(idx);
                }
                self.dirty = true;
                *handled = true;
                return;
            }
        }

        // 7. Sidebar: collection headers toggle expand.
        for (rect, ci) in self.hits.collection_rows.clone() {
            if rect.contains(p) {
                if let Some(col) = self.collections.get_mut(ci) {
                    col.expanded = !col.expanded;
                }
                self.dirty = true;
                *handled = true;
                self.persist(state, path);
                return;
            }
        }

        // 8. Sidebar: request rows load into the builder.
        for (rect, ci, ri) in self.hits.request_rows.clone() {
            if rect.contains(p) {
                self.load_request(ci, ri);
                self.url_input.set_focus(false);
                self.body_input.set_focus(false);
                *handled = true;
                self.persist(state, path);
                return;
            }
        }

        // 9. History rows: replay method + URL into the builder.
        for (rect, hi) in self.hits.history_rows.clone() {
            if rect.contains(p) {
                if let Some(h) = self.history.get(hi).cloned() {
                    self.request.method = h.method;
                    self.url_input.set_text(&h.url);
                    self.request.url = h.url;
                    self.dirty = true;
                }
                *handled = true;
                self.persist(state, path);
                return;
            }
        }

        // Click elsewhere: drop focus.
        self.url_input.set_focus(false);
        self.body_input.set_focus(false);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that exercise saved collections / environments / a request use the
    /// demo build (the live app starts empty; see `live_app_starts_empty`).
    fn app() -> ApiTesterApp {
        ApiTesterApp::new_demo()
    }

    #[test]
    fn demo_app_has_seeded_workspace() {
        let a = ApiTesterApp::new_demo();
        assert!(!a.collections.is_empty());
        assert!(!a.environments.is_empty());
        assert!(a.active_env.is_some());
        assert!(a.response.is_some());
    }

    #[test]
    fn live_app_starts_empty() {
        // The live app fabricates nothing: no saved collections/environments/
        // history and NO canned response — the response pane is empty until a
        // real Send.
        let a = ApiTesterApp::new();
        assert!(a.collections.is_empty(), "no fabricated collections");
        assert!(a.environments.is_empty(), "no fabricated environments");
        assert!(a.history.is_empty(), "no fabricated history");
        assert!(a.response.is_none(), "no canned response before a real Send");
    }

    #[test]
    fn env_vars_resolve_from_active_environment() {
        let a = app();
        let vars = a.env_vars();
        assert_eq!(
            vars.get("base_url").map(String::as_str),
            Some("https://api.example.com/v2")
        );
    }

    #[test]
    fn outgoing_substitutes_environment_into_url() {
        let a = app();
        let out = a.outgoing();
        assert!(out.url.starts_with("https://api.example.com/v2/users"));
        // Enabled params land in the query string.
        assert!(out.url.contains("page=1"));
        assert!(out.url.contains("per_page=25"));
        // Disabled param (sort) is excluded.
        assert!(!out.url.contains("sort="));
    }

    #[test]
    fn loading_a_saved_request_swaps_the_builder() {
        let mut a = app();
        // Load "Create user" (collection 0, request 2).
        a.load_request(0, 2);
        assert_eq!(a.request.method, HttpMethod::Post);
        assert_eq!(a.url_input.value(), "{{base_url}}/users");
        assert_eq!(a.request.body_mode, BodyMode::Json);
        assert_eq!(a.selected, Some((0, 2)));
    }

    #[test]
    fn tab_count_reflects_enabled_rows() {
        let a = app();
        // Two enabled params (page, per_page); sort is disabled.
        assert_eq!(a.tab_count(RequestTab::Params), 2);
        // Two headers.
        assert_eq!(a.tab_count(RequestTab::Headers), 2);
        // Bearer auth → 1.
        assert_eq!(a.tab_count(RequestTab::Auth), 1);
    }

    #[test]
    fn persist_then_adopt_round_trips_through_crdt() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        a.persist(&mut doc, &root);

        let mut b = ApiTesterApp::new();
        // Clear b's model so adoption is observable.
        b.collections.clear();
        b.adopt(&doc, &root);
        assert!(!b.collections.is_empty());
        assert_eq!(b.collections.len(), a.collections.len());
    }
}
