//! `PdfApp` — the root widget for the PDF reader.
//!
//! It is a single full-window widget that manages its own internal layout
//! (toolbar, thumbnail rail, page canvas, comments/outline/signatures
//! sidebar, status bar, search strip) and draws each region itself. This
//! gives pixel control over the reader chrome, mirroring the custom-draw
//! approach the toolkit uses for things like the status bar, while keeping
//! all durable markup — annotations, comments, signatures — in the shared
//! CRDT so collaboration and undo come for free.

use std::collections::HashSet;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::Widget;

use crate::doc::PdfDoc;
use crate::model::{
    self, AnnoKind, Annotation, Comment, Document, Match, PtRect, Reply, Signature, Swatch,
    Tool, MARGIN, PAGE_H, PAGE_W,
};
use crate::render;

const TOOLBAR_H: f64 = 56.0;
const STATUS_H: f64 = 28.0;
const RAIL_W: f64 = 144.0;
const SIDEBAR_W: f64 = 324.0;
const SEARCH_H: f64 = 46.0;
const CANVAS_TOPPAD: f64 = 28.0;
const PAGE_GAP: f64 = 22.0;
const SIDE_PAD: f64 = 40.0;
const TODAY: &str = "2026-06-30";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Comments,
    Outline,
    Signatures,
}

#[derive(Clone, Copy, PartialEq)]
enum FitMode {
    Page,
    Width,
    Custom(f64),
}

#[derive(Clone)]
struct DragState {
    tool: Tool,
    page: usize,
    start: (f64, f64),
    cur: (f64, f64),
    points: Vec<(f64, f64)>,
}

#[derive(Clone)]
enum ToolbarHit {
    Tool(Tool),
    Swatch(Swatch),
    ZoomIn,
    ZoomOut,
    Fit,
    Search,
    ToggleSidebar,
}

#[derive(Clone)]
enum SidebarHit {
    Tab(SidebarTab),
    Outline(usize),
    Comment(String),
    Resolve(String),
    AddSignature,
    SigJump(usize),
}

pub struct PdfApp {
    doc: Document,

    // CRDT-backed caches (refreshed from the shared document each frame).
    annotations: Vec<Annotation>,
    comments: Vec<Comment>,
    signatures: Vec<Signature>,
    seeded: bool,

    // View state — local / per-peer, never in the shared doc.
    rect: Rect,
    fit: FitMode,
    zoom: f64,
    scroll_y: f64,
    current_page: usize,
    tool: Tool,
    swatch: Swatch,
    sidebar_tab: SidebarTab,
    show_sidebar: bool,
    selected_anno: Option<String>,
    signer_name: String,
    author: String,

    // Search.
    search_active: bool,
    query: String,
    matches: Vec<Match>,
    active_match: usize,

    // Interaction transients.
    last_mouse: Point,
    drag: Option<DragState>,
    created_counter: i64,
    commit_pending: bool,
    dirty: bool,

    // Region rects (computed in apply_layout).
    toolbar_rect: Rect,
    search_rect: Rect,
    rail_rect: Rect,
    canvas_rect: Rect,
    sidebar_rect: Rect,
    status_rect: Rect,

    // Sidebar content hit-targets, captured during draw.
    sidebar_hits: Vec<(SidebarHit, Rect)>,
}

impl PdfApp {
    pub fn new() -> Self {
        let author = std::env::var("QUICK_APP_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "You".to_string());
        Self {
            doc: model::sample_document(),
            annotations: Vec::new(),
            comments: Vec::new(),
            signatures: Vec::new(),
            seeded: false,
            rect: Rect::ZERO,
            fit: FitMode::Page,
            zoom: 0.85,
            scroll_y: 0.0,
            current_page: 1,
            tool: Tool::Highlight,
            swatch: Swatch::Yellow,
            sidebar_tab: SidebarTab::Comments,
            show_sidebar: true,
            selected_anno: None,
            signer_name: "Alice Example".to_string(),
            author,
            search_active: false,
            query: String::new(),
            matches: Vec::new(),
            active_match: 0,
            last_mouse: Point::ZERO,
            drag: None,
            created_counter: 100,
            commit_pending: false,
            dirty: true,
            toolbar_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            rail_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            sidebar_hits: Vec::new(),
        }
    }

    // ---- seeding + cache refresh ------------------------------------

    fn seed(&mut self, state: &mut AutoCommit) {
        PdfDoc::set_meta(state, &self.doc.title, &self.doc.author);

        // A highlight over the abstract, an ink emphasis loop, a sticky
        // note, and a placed review signature — all on page 1 so the
        // default screenshot is rich.
        let highlight = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Highlight { rect: PtRect::new(64.0, 196.0, 420.0, 14.0) },
            color: Swatch::Yellow,
            author: "Bob".into(),
        };
        let highlight2 = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Highlight { rect: PtRect::new(64.0, 210.0, 300.0, 14.0) },
            color: Swatch::Yellow,
            author: "Bob".into(),
        };
        let ink = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Ink { points: ink_loop(360.0, 470.0, 150.0, 26.0) },
            color: Swatch::Pink,
            author: "Alice".into(),
        };
        let note = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Note { x: 506.0, y: 300.0 },
            color: Swatch::Blue,
            author: "Alice".into(),
        };
        let sig = Signature {
            id: Signature::new_id(),
            page: 1,
            rect: PtRect::new(318.0, 612.0, 218.0, 74.0),
            signer: "Alice Example".into(),
            date: TODAY.into(),
            style: 1,
        };

        let note_id = note.id.clone();
        let hl_id = highlight.id.clone();
        for a in [&highlight, &highlight2, &ink, &note] {
            PdfDoc::add_annotation(state, a);
        }
        PdfDoc::add_signature(state, &sig);

        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: Some(note_id),
                page: 1,
                author: "Alice".into(),
                body: "Should we spell out the relay fallback path here? mDNS only covers the local segment.".into(),
                created: 1,
                resolved: false,
                replies: vec![
                    Reply { author: "Bob".into(), body: "Good catch — folded it into section 1.2.".into() },
                    Reply { author: "Alice".into(), body: "Perfect, thanks.".into() },
                ],
            },
        );
        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: Some(hl_id),
                page: 1,
                author: "Bob".into(),
                body: "This sentence is the thesis of the whole paper — let's keep it verbatim.".into(),
                created: 2,
                resolved: false,
                replies: vec![],
            },
        );
        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: None,
                page: 1,
                author: "Carol".into(),
                body: "Typography reads cleanly at fit-page zoom. Approved.".into(),
                created: 3,
                resolved: true,
                replies: vec![],
            },
        );
    }

    fn reload(&mut self, state: &AutoCommit) {
        self.annotations = PdfDoc::list_annotations(state);
        self.comments = PdfDoc::list_comments(state);
        self.signatures = PdfDoc::list_signatures(state);
    }

    // ---- geometry ---------------------------------------------------

    fn recompute_regions(&mut self) {
        let r = self.rect;
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        self.status_rect = Rect::new(r.x0, r.y1 - STATUS_H, r.x1, r.y1);
        let mut mid_top = r.y0 + TOOLBAR_H;
        if self.search_active {
            self.search_rect = Rect::new(r.x0, mid_top, r.x1, mid_top + SEARCH_H);
            mid_top += SEARCH_H;
        } else {
            self.search_rect = Rect::ZERO;
        }
        let mid_bottom = r.y1 - STATUS_H;
        self.rail_rect = Rect::new(r.x0, mid_top, r.x0 + RAIL_W, mid_bottom);
        let sidebar_w = if self.show_sidebar { SIDEBAR_W } else { 0.0 };
        self.sidebar_rect = Rect::new(r.x1 - sidebar_w, mid_top, r.x1, mid_bottom);
        self.canvas_rect = Rect::new(r.x0 + RAIL_W, mid_top, r.x1 - sidebar_w, mid_bottom);
    }

    fn recompute_zoom(&mut self) {
        let cw = self.canvas_rect.width();
        let ch = self.canvas_rect.height();
        self.zoom = match self.fit {
            FitMode::Page => ((cw - 2.0 * SIDE_PAD) / PAGE_W)
                .min((ch - 2.0 * CANVAS_TOPPAD) / PAGE_H)
                .clamp(0.2, 4.0),
            FitMode::Width => ((cw - 2.0 * SIDE_PAD) / PAGE_W).clamp(0.2, 4.0),
            FitMode::Custom(z) => z.clamp(0.2, 4.0),
        };
    }

    fn page_step(&self) -> f64 {
        PAGE_H * self.zoom + PAGE_GAP
    }

    fn content_height(&self) -> f64 {
        let n = self.doc.page_count() as f64;
        n * PAGE_H * self.zoom + (n - 1.0).max(0.0) * PAGE_GAP + 2.0 * CANVAS_TOPPAD
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.canvas_rect.height()).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    /// Screen-space top-left origin of each page card.
    fn page_origins(&self) -> Vec<Point> {
        let pagew = PAGE_W * self.zoom;
        let x = self.canvas_rect.x0 + ((self.canvas_rect.width() - pagew) / 2.0).max(SIDE_PAD * 0.5);
        let mut out = Vec::with_capacity(self.doc.page_count());
        let mut y = self.canvas_rect.y0 + CANVAS_TOPPAD - self.scroll_y;
        for _ in 0..self.doc.page_count() {
            out.push(Point::new(x, y));
            y += self.page_step();
        }
        out
    }

    /// Map a screen point to (page-number, page-point) if it lands on a page.
    fn hit_page(&self, p: Point) -> Option<(usize, (f64, f64))> {
        let pagew = PAGE_W * self.zoom;
        let pageh = PAGE_H * self.zoom;
        for (i, origin) in self.page_origins().iter().enumerate() {
            let card = Rect::new(origin.x, origin.y, origin.x + pagew, origin.y + pageh);
            if card.contains(p) {
                let px = (p.x - origin.x) / self.zoom;
                let py = (p.y - origin.y) / self.zoom;
                return Some((i + 1, (px, py)));
            }
        }
        None
    }

    fn nearest_page(&self) -> usize {
        let target = self.canvas_rect.y0 + CANVAS_TOPPAD;
        let mut best = 1;
        let mut best_d = f64::MAX;
        for (i, o) in self.page_origins().iter().enumerate() {
            let d = (o.y - target).abs();
            if d < best_d {
                best_d = d;
                best = i + 1;
            }
        }
        best
    }

    fn go_to_page(&mut self, page: usize) {
        let page = page.clamp(1, self.doc.page_count());
        self.scroll_y = (page - 1) as f64 * self.page_step();
        self.clamp_scroll();
        self.current_page = page;
        self.dirty = true;
    }

    // ---- search -----------------------------------------------------

    fn recompute_matches(&mut self) {
        self.matches = model::search(&self.doc, &self.query);
        self.active_match = 0;
        if let Some(m) = self.matches.first() {
            self.go_to_page(m.page);
        }
        self.dirty = true;
    }

    fn step_match(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        let mut idx = self.active_match as isize + delta;
        idx = ((idx % n) + n) % n;
        self.active_match = idx as usize;
        let page = self.matches[self.active_match].page;
        self.go_to_page(page);
    }

    fn matched_blocks_for(&self, page: usize) -> (HashSet<usize>, Option<usize>) {
        let mut set = HashSet::new();
        let mut active = None;
        for (i, m) in self.matches.iter().enumerate() {
            if m.page == page {
                set.insert(m.block);
                if i == self.active_match {
                    active = Some(m.block);
                }
            }
        }
        (set, active)
    }

    // ---- annotation actions -----------------------------------------

    fn commit_drag(&mut self, state: &mut AutoCommit) {
        let Some(d) = self.drag.take() else { return };
        match d.tool {
            Tool::Highlight => {
                let r = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                if r.w > 6.0 && r.h > 4.0 {
                    let a = Annotation {
                        id: Annotation::new_id(),
                        page: d.page,
                        kind: AnnoKind::Highlight { rect: r },
                        color: self.swatch,
                        author: self.author.clone(),
                    };
                    self.selected_anno = Some(a.id.clone());
                    PdfDoc::add_annotation(state, &a);
                    self.commit_pending = true;
                }
            }
            Tool::Ink => {
                if d.points.len() >= 2 {
                    let a = Annotation {
                        id: Annotation::new_id(),
                        page: d.page,
                        kind: AnnoKind::Ink { points: d.points.clone() },
                        color: self.swatch,
                        author: self.author.clone(),
                    };
                    PdfDoc::add_annotation(state, &a);
                    self.commit_pending = true;
                }
            }
            _ => {}
        }
        self.reload(state);
        self.dirty = true;
    }

    fn create_note(&mut self, state: &mut AutoCommit, page: usize, pt: (f64, f64)) {
        let a = Annotation {
            id: Annotation::new_id(),
            page,
            kind: AnnoKind::Note { x: pt.0, y: pt.1 },
            color: self.swatch,
            author: self.author.clone(),
        };
        let anno_id = a.id.clone();
        PdfDoc::add_annotation(state, &a);
        self.created_counter += 1;
        PdfDoc::add_comment(
            state,
            &Comment {
                id: Comment::new_id(),
                anno_id: Some(anno_id.clone()),
                page,
                author: self.author.clone(),
                body: "New note — type your comment.".into(),
                created: self.created_counter,
                resolved: false,
                replies: vec![],
            },
        );
        self.selected_anno = Some(anno_id);
        self.sidebar_tab = SidebarTab::Comments;
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
    }

    fn place_signature(&mut self, state: &mut AutoCommit, page: usize, pt: (f64, f64)) {
        let w = 218.0;
        let h = 74.0;
        let rect = PtRect::new(
            (pt.0 - w / 2.0).clamp(MARGIN, PAGE_W - MARGIN - w),
            (pt.1 - h / 2.0).clamp(MARGIN, PAGE_H - MARGIN - h),
            w,
            h,
        );
        let s = Signature {
            id: Signature::new_id(),
            page,
            rect,
            signer: self.signer_name.clone(),
            date: TODAY.into(),
            style: 1,
        };
        PdfDoc::add_signature(state, &s);
        self.sidebar_tab = SidebarTab::Signatures;
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
    }

    fn select_at(&mut self, page: usize, pt: (f64, f64)) {
        // Front-to-back: notes first (small targets), then highlights.
        let mut hit: Option<String> = None;
        for a in self.annotations.iter().filter(|a| a.page == page) {
            match &a.kind {
                AnnoKind::Note { x, y } => {
                    if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 {
                        hit = Some(a.id.clone());
                    }
                }
                AnnoKind::Highlight { rect } => {
                    if rect.contains(pt.0, pt.1) && hit.is_none() {
                        hit = Some(a.id.clone());
                    }
                }
                _ => {}
            }
        }
        self.selected_anno = hit.clone();
        if let Some(id) = hit {
            // Reveal the linked comment.
            if self.comments.iter().any(|c| c.anno_id.as_deref() == Some(id.as_str())) {
                self.sidebar_tab = SidebarTab::Comments;
                self.show_sidebar = true;
            }
        }
        self.dirty = true;
    }

    fn zoom_by(&mut self, factor: f64) {
        let z = (self.zoom * factor).clamp(0.2, 4.0);
        self.fit = FitMode::Custom(z);
        self.recompute_zoom();
        self.clamp_scroll();
        self.dirty = true;
    }
}

impl Default for PdfApp {
    fn default() -> Self {
        Self::new()
    }
}

// Generate a loose ink "emphasis loop" — an ellipse-ish polyline.
fn ink_loop(cx: f64, cy: f64, rx: f64, ry: f64) -> Vec<(f64, f64)> {
    let mut pts = Vec::new();
    let n = 22;
    for i in 0..=n {
        let t = i as f64 / n as f64 * std::f64::consts::TAU * 1.05;
        let wobble = 1.0 + 0.06 * (t * 3.0).sin();
        pts.push((cx + rx * wobble * t.cos(), cy + ry * wobble * t.sin()));
    }
    pts
}

include!("app_draw.rs");
include!("app_events.rs");

impl Widget for PdfApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if !PdfDoc::is_seeded(state) {
                self.seed(state);
            }
            self.seeded = true;
        }
        self.reload(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.recompute_regions();
        self.recompute_zoom();
        self.clamp_scroll();
        self.current_page = self.nearest_page();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_all(scene, transform, text_renderer, theme);
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
        self.on_event(event, scale_factor, modifiers, state, handled);
    }

    fn name(&self) -> &str {
        "PdfApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("pdf_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_commit(&self) -> bool {
        self.commit_pending
    }

    fn on_commit(&mut self) {
        self.commit_pending = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.drag.is_some()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
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
        // Root fills the whole window — never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_state() -> (PdfApp, ui::State) {
        let mut app = PdfApp::new();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        // Give it a viewport so geometry is meaningful.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.recompute_zoom();
        (app, st)
    }

    #[test]
    fn sync_seeds_markup_once() {
        let (app, _st) = app_with_state();
        assert!(app.seeded);
        assert!(!app.annotations.is_empty(), "seed should add annotations");
        assert!(!app.comments.is_empty(), "seed should add comments");
        assert!(!app.signatures.is_empty(), "seed should add a signature");
        // The note annotation has a linked comment thread.
        let note = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .expect("a sticky note");
        assert!(app
            .comments
            .iter()
            .any(|c| c.anno_id.as_deref() == Some(note.id.as_str())));
    }

    #[test]
    fn second_sync_does_not_duplicate() {
        let (mut app, mut st) = app_with_state();
        let n = app.annotations.len();
        app.sync(&mut st.doc, &st.root);
        assert_eq!(app.annotations.len(), n, "re-sync must not re-seed");
    }

    #[test]
    fn search_finds_and_jumps() {
        let (mut app, _st) = app_with_state();
        app.query = "filesystem".into();
        app.recompute_matches();
        assert!(!app.matches.is_empty());
        // Active match drives the current page.
        let p = app.matches[0].page;
        assert_eq!(app.current_page, p);
        // Stepping wraps around.
        let first = app.active_match;
        app.step_match(1);
        if app.matches.len() > 1 {
            assert_ne!(app.active_match, first);
        }
    }

    #[test]
    fn create_note_adds_annotation_and_comment() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        let c0 = app.comments.len();
        app.create_note(&mut st.doc, 2, (120.0, 200.0));
        assert_eq!(app.annotations.len(), a0 + 1);
        assert_eq!(app.comments.len(), c0 + 1);
        assert!(app.commit_pending);
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
    }

    #[test]
    fn highlight_drag_commits_annotation() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (64.0, 100.0),
            cur: (300.0, 116.0),
            points: vec![],
        });
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0 + 1, "drag should add one highlight");
    }

    #[test]
    fn tiny_highlight_drag_is_ignored() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (64.0, 100.0),
            cur: (66.0, 101.0),
            points: vec![],
        });
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0, "sub-threshold drag adds nothing");
    }

    #[test]
    fn place_signature_clamps_into_page() {
        let (mut app, mut st) = app_with_state();
        let s0 = app.signatures.len();
        app.signer_name = "Tester".into();
        app.place_signature(&mut st.doc, 3, (10.0, 10.0)); // near corner
        assert_eq!(app.signatures.len(), s0 + 1);
        let last = app.signatures.iter().find(|s| s.page == 3).unwrap();
        assert!(last.rect.x >= MARGIN - 0.001);
        assert!(last.rect.y >= MARGIN - 0.001);
    }

    #[test]
    fn page_navigation_clamps() {
        let (mut app, _st) = app_with_state();
        app.go_to_page(999);
        assert_eq!(app.current_page, app.doc.page_count());
        app.go_to_page(0);
        assert_eq!(app.current_page, 1);
    }

    #[test]
    fn zoom_switches_to_custom() {
        let (mut app, _st) = app_with_state();
        app.zoom_by(1.25);
        assert!(matches!(app.fit, FitMode::Custom(_)));
    }
}
