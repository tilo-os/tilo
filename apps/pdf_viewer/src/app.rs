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

use std::time::Duration;

use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::tooltip::Tooltip;
use ui::Widget;

use crate::doc::PdfDoc;
use crate::model::{
    self, AnnoKind, Annotation, Comment, Document, Match, PtRect, Reply, SearchOptions, Signature,
    Swatch, Tool, MARGIN, PAGE_H, PAGE_W,
};
use crate::render;

const TOOLBAR_H: f64 = 56.0;
const STATUS_H: f64 = 28.0;
/// Default width of the thumbnail rail; the user can drag-resize it within
/// [`RAIL_W_MIN`, `RAIL_W_MAX`] and fully collapse it.
const RAIL_W: f64 = 144.0;
const RAIL_W_MIN: f64 = 96.0;
const RAIL_W_MAX: f64 = 280.0;
/// Width of the invisible drag strip on the rail's right edge.
const RAIL_GRIP: f64 = 6.0;
const SIDEBAR_W: f64 = 324.0;
const SEARCH_H: f64 = 46.0;
const CANVAS_TOPPAD: f64 = 28.0;
const PAGE_GAP: f64 = 22.0;
const SIDE_PAD: f64 = 40.0;
const ZOOM_MIN: f64 = 0.25;
const ZOOM_MAX: f64 = 6.0;
/// A drag shorter than this (in screen px) is treated as a click, not a pan.
const PAN_SLOP: f64 = 4.0;
/// How close (page points) a highlight drag must come to a text line before
/// it snaps to text instead of falling back to a free-form rectangle.
const SNAP_NEAR: f64 = 18.0;
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

/// An in-progress canvas pan (drag-to-pan with Select/empty space). Records
/// the screen point and scroll offsets at press so motion maps 1:1.
#[derive(Clone, Copy)]
struct PanState {
    start: Point,
    scroll_x0: f64,
    scroll_y0: f64,
    moved: bool,
}

/// The target of a right-click "Delete" context menu.
#[derive(Clone)]
enum DeleteTarget {
    Annotation(String),
    Comment(String),
    Signature(String),
}

#[derive(Clone)]
struct ContextMenu {
    at: Point,
    target: DeleteTarget,
    /// Captured during draw so the click handler knows the menu-item rect.
    item_rect: Rect,
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
    ToggleRail,
}

#[derive(Clone)]
enum SidebarHit {
    Tab(SidebarTab),
    Outline(usize),
    Comment(String),
    Resolve(String),
    DeleteComment(String),
    AddSignature,
    SigJump(usize),
    DeleteSignature(String),
}

#[derive(Clone)]
enum SearchHit {
    ToggleWholeWord,
    ToggleCaseSensitive,
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
    scroll_x: f64,
    current_page: usize,
    tool: Tool,
    swatch: Swatch,
    sidebar_tab: SidebarTab,
    show_sidebar: bool,
    /// Thumbnail rail: independently resizable + collapsible, persisted in
    /// view state so it survives across the session.
    show_rail: bool,
    rail_w: f64,
    selected_anno: Option<String>,
    /// The signature whose sidebar row is highlighted/selected (so placing a
    /// signature lands the user on the new entry).
    selected_sig: Option<String>,
    /// Vertical scroll offset of the active sidebar list, so a freshly
    /// created comment/signature can be scrolled into view.
    sidebar_scroll: f64,
    /// When set, the next sidebar draw scrolls this comment/sig id into view
    /// and clears the request.
    sidebar_reveal: Option<String>,
    signer_name: String,
    author: String,

    // Search.
    search_active: bool,
    query: String,
    matches: Vec<Match>,
    active_match: usize,
    search_whole_word: bool,
    search_case_sensitive: bool,

    // Interaction transients.
    last_mouse: Point,
    drag: Option<DragState>,
    pan: Option<PanState>,
    /// Some(pointer-x-at-press) while dragging the rail's resize grip.
    rail_drag: Option<f64>,
    context_menu: Option<ContextMenu>,
    created_counter: i64,
    commit_pending: bool,
    dirty: bool,

    // Per-page text-line geometry (page-point space), cached during draw so
    // the smart highlight tool can snap to text without a TextRenderer.
    text_lines: std::collections::HashMap<usize, Vec<render::TextLine>>,

    // Toolbar tooltip — a single hover-revealed popover; which button it is
    // anchored to is tracked alongside it.
    tooltip: Tooltip,
    tooltip_anchor: Rect,
    /// One-shot: on the first laid-out frame, hover a toolbar tool and reveal
    /// its tooltip so the default view (live + screenshot) shows the feature.
    demo_pending: bool,

    // Region rects (computed in apply_layout).
    toolbar_rect: Rect,
    search_rect: Rect,
    rail_rect: Rect,
    canvas_rect: Rect,
    sidebar_rect: Rect,
    status_rect: Rect,

    // Sidebar + search content hit-targets, captured during draw.
    sidebar_hits: Vec<(SidebarHit, Rect)>,
    search_hits: Vec<(SearchHit, Rect)>,
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
            scroll_x: 0.0,
            current_page: 1,
            tool: Tool::Highlight,
            swatch: Swatch::Yellow,
            sidebar_tab: SidebarTab::Comments,
            show_sidebar: true,
            show_rail: true,
            rail_w: RAIL_W,
            selected_anno: None,
            selected_sig: None,
            sidebar_scroll: 0.0,
            sidebar_reveal: None,
            signer_name: "Alice Example".to_string(),
            author,
            search_active: false,
            query: String::new(),
            matches: Vec::new(),
            active_match: 0,
            search_whole_word: false,
            search_case_sensitive: false,
            last_mouse: Point::ZERO,
            drag: None,
            pan: None,
            rail_drag: None,
            context_menu: None,
            created_counter: 100,
            commit_pending: false,
            dirty: true,
            text_lines: std::collections::HashMap::new(),
            tooltip: Tooltip::new("").with_delay(Duration::from_millis(350)),
            tooltip_anchor: Rect::ZERO,
            demo_pending: true,
            toolbar_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            rail_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            sidebar_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            sidebar_hits: Vec::new(),
            search_hits: Vec::new(),
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

        // This comment stands in for the one the user "just placed": the
        // demo view-state below selects its note and reveals it in the rail.
        let focus_comment = Comment::new_id();
        PdfDoc::add_comment(
            state,
            &Comment {
                id: focus_comment.clone(),
                anno_id: Some(note_id.clone()),
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

        // Demo view-state for the first frame (live + screenshot): zoom past
        // fit so horizontal panning is exercisable (the h-scrollbar appears),
        // land on the note's comment with the Comments rail revealed on it,
        // and arm the marker.
        self.fit = FitMode::Custom(1.6);
        self.selected_anno = Some(note_id);
        self.sidebar_tab = SidebarTab::Comments;
        self.sidebar_reveal = Some(focus_comment);
        self.tool = Tool::Highlight;
    }

    fn reload(&mut self, state: &AutoCommit) {
        self.annotations = PdfDoc::list_annotations(state);
        self.comments = PdfDoc::list_comments(state);
        self.signatures = PdfDoc::list_signatures(state);
    }

    // ---- geometry ---------------------------------------------------

    fn rail_width(&self) -> f64 {
        if self.show_rail { self.rail_w.clamp(RAIL_W_MIN, RAIL_W_MAX) } else { 0.0 }
    }

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
        let rail_w = self.rail_width();
        self.rail_rect = Rect::new(r.x0, mid_top, r.x0 + rail_w, mid_bottom);
        let sidebar_w = if self.show_sidebar { SIDEBAR_W } else { 0.0 };
        self.sidebar_rect = Rect::new(r.x1 - sidebar_w, mid_top, r.x1, mid_bottom);
        self.canvas_rect = Rect::new(r.x0 + rail_w, mid_top, r.x1 - sidebar_w, mid_bottom);
    }

    fn recompute_zoom(&mut self) {
        let cw = self.canvas_rect.width();
        let ch = self.canvas_rect.height();
        self.zoom = match self.fit {
            FitMode::Page => ((cw - 2.0 * SIDE_PAD) / PAGE_W)
                .min((ch - 2.0 * CANVAS_TOPPAD) / PAGE_H)
                .clamp(ZOOM_MIN, ZOOM_MAX),
            FitMode::Width => ((cw - 2.0 * SIDE_PAD) / PAGE_W).clamp(ZOOM_MIN, ZOOM_MAX),
            FitMode::Custom(z) => z.clamp(ZOOM_MIN, ZOOM_MAX),
        };
    }

    fn page_step(&self) -> f64 {
        PAGE_H * self.zoom + PAGE_GAP
    }

    fn content_height(&self) -> f64 {
        let n = self.doc.page_count() as f64;
        n * PAGE_H * self.zoom + (n - 1.0).max(0.0) * PAGE_GAP + 2.0 * CANVAS_TOPPAD
    }

    /// Full width the zoomed page occupies, plus side padding. When this
    /// exceeds the canvas width the user can pan horizontally to reach it.
    fn content_width(&self) -> f64 {
        PAGE_W * self.zoom + 2.0 * SIDE_PAD
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.canvas_rect.height()).max(0.0)
    }

    fn max_scroll_x(&self) -> f64 {
        (self.content_width() - self.canvas_rect.width()).max(0.0)
    }

    /// Whether the zoomed page is wider than the viewport — drives whether
    /// the arrow keys pan horizontally and whether the h-scrollbar shows.
    fn is_overflowing_x(&self) -> bool {
        self.max_scroll_x() > 1.0
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
        self.scroll_x = self.scroll_x.clamp(0.0, self.max_scroll_x());
    }

    /// Screen-space top-left origin of each page card. When the page fits
    /// horizontally it is centred; once it overflows it is left-anchored and
    /// shifted by `scroll_x` so panning can reach the full zoomed width.
    fn page_origins(&self) -> Vec<Point> {
        let pagew = PAGE_W * self.zoom;
        let x = if self.is_overflowing_x() {
            self.canvas_rect.x0 + SIDE_PAD - self.scroll_x
        } else {
            self.canvas_rect.x0 + ((self.canvas_rect.width() - pagew) / 2.0).max(SIDE_PAD * 0.5)
        };
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

    fn search_options(&self) -> SearchOptions {
        SearchOptions {
            whole_word: self.search_whole_word,
            case_sensitive: self.search_case_sensitive,
        }
    }

    fn recompute_matches(&mut self) {
        self.matches = model::search(&self.doc, &self.query, self.search_options());
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

    /// Smart text marker: snap a highlight drag onto the text lines under it.
    ///
    /// Returns one [`PtRect`] per text line the drag covers, each clipped to
    /// the line's glyph extent so the band looks like a real text selection.
    /// Returns an empty vec when the drag isn't near any text — the caller
    /// then falls back to a free-form rectangle. The decision is purely a
    /// proximity test, so the user never toggles a mode.
    fn snap_highlights(&self, page: usize, drag: PtRect) -> Vec<PtRect> {
        let Some(lines) = self.text_lines.get(&page) else { return Vec::new() };
        let drag_bottom = drag.y + drag.h;
        let drag_right = drag.x + drag.w;
        let mut bands = Vec::new();
        for line in lines {
            let lr = line.rect();
            // Vertical proximity: the line's band overlaps (or is within
            // SNAP_NEAR of) the drag's vertical span.
            let v_gap = (lr.y0 - drag_bottom).max(drag.y - lr.y1).max(0.0);
            if v_gap > SNAP_NEAR {
                continue;
            }
            // Horizontal overlap with the line's glyph extent.
            let x0 = drag.x.max(lr.x0);
            let x1 = drag_right.min(lr.x1);
            // Near-horizontal proximity still counts so a slightly-off drag
            // snaps to the whole line rather than dropping it.
            let h_overlap = x1 - x0;
            let h_gap = (lr.x0 - drag_right).max(drag.x - lr.x1).max(0.0);
            if h_overlap <= 1.0 && h_gap > SNAP_NEAR {
                continue;
            }
            let (bx0, bx1) = if h_overlap > 1.0 { (x0, x1) } else { (lr.x0, lr.x1) };
            bands.push(PtRect::new(bx0, line.y, (bx1 - bx0).max(2.0), line.h));
        }
        bands
    }

    fn commit_drag(&mut self, state: &mut AutoCommit) {
        let Some(d) = self.drag.take() else { return };
        match d.tool {
            Tool::Highlight => {
                let drag_rect = PtRect::from_corners(d.start.0, d.start.1, d.cur.0, d.cur.1);
                if drag_rect.w <= 6.0 && drag_rect.h <= 4.0 {
                    // Sub-threshold: nothing meaningful was dragged.
                    self.reload(state);
                    self.dirty = true;
                    return;
                }
                // Default: stick to text. Fall back to free-form only when
                // the drag isn't near any text line.
                let snapped = self.snap_highlights(d.page, drag_rect);
                let rects = if snapped.is_empty() { vec![drag_rect] } else { snapped };
                let mut last_id = None;
                for r in rects {
                    let a = Annotation {
                        id: Annotation::new_id(),
                        page: d.page,
                        kind: AnnoKind::Highlight { rect: r },
                        color: self.swatch,
                        author: self.author.clone(),
                    };
                    last_id = Some(a.id.clone());
                    PdfDoc::add_annotation(state, &a);
                }
                self.selected_anno = last_id;
                self.commit_pending = true;
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
        let comment_id = Comment::new_id();
        PdfDoc::add_comment(
            state,
            &Comment {
                id: comment_id.clone(),
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
        self.selected_sig = None;
        // Open the Comments rail and scroll the brand-new thread into view.
        self.focus_sidebar(SidebarTab::Comments, comment_id);
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
    }

    /// Reveal a freshly-created item: ensure the sidebar is open on `tab` and
    /// request the next draw scroll `id` into view + select it.
    fn focus_sidebar(&mut self, tab: SidebarTab, id: String) {
        self.sidebar_tab = tab;
        if !self.show_sidebar {
            self.show_sidebar = true;
            self.reflow();
        }
        self.sidebar_reveal = Some(id);
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
        let sig_id = Signature::new_id();
        let s = Signature {
            id: sig_id.clone(),
            page,
            rect,
            signer: self.signer_name.clone(),
            date: TODAY.into(),
            style: 1,
        };
        PdfDoc::add_signature(state, &s);
        self.selected_sig = Some(sig_id.clone());
        self.selected_anno = None;
        // Land the user on the Signatures rail, scrolled to the new field.
        self.focus_sidebar(SidebarTab::Signatures, sig_id);
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
        self.selected_sig = None;
        if let Some(id) = hit {
            // Reveal the linked comment.
            if let Some(c) = self.comments.iter().find(|c| c.anno_id.as_deref() == Some(id.as_str())) {
                let cid = c.id.clone();
                self.focus_sidebar(SidebarTab::Comments, cid);
            }
        }
        self.dirty = true;
    }

    /// Zoom keeping the viewport centre fixed (used by the +/- buttons and
    /// Ctrl +/-). Delegates to the focal-point path with the canvas centre.
    fn zoom_by(&mut self, factor: f64) {
        let c = self.canvas_rect.center();
        self.zoom_at(factor, Point::new(c.x, c.y));
    }

    /// Zoom by `factor` while holding the document point under `focal`
    /// (a screen-space point) stationary, so Ctrl+wheel zooms toward the
    /// cursor and the content under it doesn't drift.
    fn zoom_at(&mut self, factor: f64, focal: Point) {
        let old_zoom = self.zoom;
        let new_zoom = (old_zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        if (new_zoom - old_zoom).abs() < 1e-9 {
            return;
        }

        // Document coordinate (in page-point space along each axis) currently
        // under the focal point, derived from the present origins/scroll.
        let canvas = self.canvas_rect;
        // Vertical: distance from the canvas content-top, in zoom-independent
        // "scroll units" measured at the page-point scale.
        let doc_y = (focal.y - (canvas.y0 + CANVAS_TOPPAD) + self.scroll_y) / old_zoom;
        // Horizontal: only meaningful when overflowing; otherwise the page is
        // centred and scroll_x stays zero.
        let was_overflow = self.is_overflowing_x();
        let left0 = if was_overflow {
            canvas.x0 + SIDE_PAD - self.scroll_x
        } else {
            canvas.x0 + ((canvas.width() - PAGE_W * old_zoom) / 2.0).max(SIDE_PAD * 0.5)
        };
        let doc_x = (focal.x - left0) / old_zoom;

        self.fit = FitMode::Custom(new_zoom);
        self.recompute_zoom(); // clamps + applies new_zoom
        let z = self.zoom;

        // Re-derive scroll so the same document point lands back under focal.
        self.scroll_y = doc_y * z - (focal.y - (canvas.y0 + CANVAS_TOPPAD));
        if self.max_scroll_x() > 1.0 {
            let left_target = focal.x - doc_x * z;
            self.scroll_x = canvas.x0 + SIDE_PAD - left_target;
        } else {
            self.scroll_x = 0.0;
        }
        self.clamp_scroll();
        self.current_page = self.nearest_page();
        self.dirty = true;
    }

    fn reset_zoom(&mut self) {
        self.fit = FitMode::Page;
        self.recompute_zoom();
        self.scroll_x = 0.0;
        self.clamp_scroll();
        self.dirty = true;
    }

    // ---- deletion ---------------------------------------------------

    /// Remove a markup item by id from the shared CRDT, drop any local
    /// selection that pointed at it, and force a redraw. Used by the Delete
    /// key, the sidebar item buttons, and the right-click context menu — so
    /// every delete affordance lands on the same model mutation.
    fn delete_target(&mut self, target: &DeleteTarget, state: &mut AutoCommit) {
        match target {
            DeleteTarget::Annotation(id) => {
                PdfDoc::delete_annotation(state, id);
                if self.selected_anno.as_deref() == Some(id.as_str()) {
                    self.selected_anno = None;
                }
            }
            DeleteTarget::Comment(id) => {
                // Deleting a comment also removes the annotation it pins, so
                // the orphaned note marker doesn't linger on the page.
                if let Some(anno) = self
                    .comments
                    .iter()
                    .find(|c| &c.id == id)
                    .and_then(|c| c.anno_id.clone())
                {
                    PdfDoc::delete_annotation(state, &anno);
                    if self.selected_anno.as_deref() == Some(anno.as_str()) {
                        self.selected_anno = None;
                    }
                }
                PdfDoc::delete_comment(state, id);
            }
            DeleteTarget::Signature(id) => {
                PdfDoc::delete_signature(state, id);
                if self.selected_sig.as_deref() == Some(id.as_str()) {
                    self.selected_sig = None;
                }
            }
        }
        self.context_menu = None;
        self.commit_pending = true;
        self.reload(state);
        self.dirty = true;
    }

    /// Delete whichever markup the user currently has selected (Delete key).
    fn delete_selected(&mut self, state: &mut AutoCommit) -> bool {
        if let Some(id) = self.selected_anno.clone() {
            self.delete_target(&DeleteTarget::Annotation(id), state);
            true
        } else if let Some(id) = self.selected_sig.clone() {
            self.delete_target(&DeleteTarget::Signature(id), state);
            true
        } else {
            false
        }
    }

    /// Pick a deletable markup item under a right-click on the canvas.
    fn delete_target_at(&self, page: usize, pt: (f64, f64)) -> Option<DeleteTarget> {
        // Signatures first (largest, but explicit), then notes, highlights.
        for s in self.signatures.iter().filter(|s| s.page == page) {
            if s.rect.contains(pt.0, pt.1) {
                return Some(DeleteTarget::Signature(s.id.clone()));
            }
        }
        for a in self.annotations.iter().filter(|a| a.page == page) {
            match &a.kind {
                AnnoKind::Note { x, y } => {
                    if (pt.0 - x).abs() < 24.0 && (pt.1 - y).abs() < 24.0 {
                        return Some(DeleteTarget::Annotation(a.id.clone()));
                    }
                }
                AnnoKind::Highlight { rect } => {
                    if rect.contains(pt.0, pt.1) {
                        return Some(DeleteTarget::Annotation(a.id.clone()));
                    }
                }
                _ => {}
            }
        }
        None
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

        // First laid-out frame: hover the active tool so its tooltip shows in
        // the default view. Cleared after one shot; live hover takes over.
        if self.demo_pending && self.toolbar_rect.area() > 1.0 {
            self.demo_pending = false;
            let lay = self.toolbar_layout();
            if let Some((_, r)) = lay.tools.iter().find(|(t, _)| *t == self.tool) {
                self.last_mouse = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
                let label = format!("{}  ({})", self.tool.label(), self.tool.hotkey());
                self.tooltip = Tooltip::new(label).with_delay(Duration::from_millis(350));
                // Seed past the delay so the popover is visible immediately.
                self.tooltip.on_hover_enter(Duration::from_millis(400));
                self.tooltip_anchor = *r;
            }
        }
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
        self.dirty || self.drag.is_some() || self.pan.is_some() || self.rail_drag.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep ticking while a toolbar button is hovered so the tooltip can
        // cross its reveal delay, and while the tooltip is showing.
        self.toolbar_rect.contains(self.last_mouse) || self.tooltip.is_visible()
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

    #[test]
    fn zoom_at_keeps_focal_point_stationary() {
        let (mut app, _st) = app_with_state();
        // Start already overflowing and mid-scrolled so neither axis clamps
        // during the zoom — the focal point can then stay exactly put.
        app.fit = FitMode::Custom(2.0);
        app.recompute_zoom();
        app.scroll_x = app.max_scroll_x() / 2.0;
        app.go_to_page(1);
        app.scroll_x = app.max_scroll_x() / 2.0;
        app.clamp_scroll();
        // Build a focal point that demonstrably lands inside page 1's card so
        // neither axis is at a scroll clamp.
        let origin = app.page_origins()[0];
        let focal = Point::new(origin.x + 300.0 * app.zoom, origin.y + 200.0 * app.zoom);
        let before = app.hit_page(focal).expect("focal lands on a page");
        app.zoom_at(1.3, focal);
        let after = app.hit_page(focal).expect("focal still on a page");
        assert_eq!(before.0, after.0, "same page stays under the cursor");
        assert!((before.1 .0 - after.1 .0).abs() < 2.0, "x focal drift {} too large", (before.1 .0 - after.1 .0).abs());
        assert!((before.1 .1 - after.1 .1).abs() < 2.0, "y focal drift {} too large", (before.1 .1 - after.1 .1).abs());
    }

    #[test]
    fn zoom_respects_min_max_bounds() {
        let (mut app, _st) = app_with_state();
        for _ in 0..40 {
            app.zoom_by(1.5);
        }
        assert!(app.zoom <= ZOOM_MAX + 1e-6);
        for _ in 0..40 {
            app.zoom_by(0.5);
        }
        assert!(app.zoom >= ZOOM_MIN - 1e-6);
    }

    #[test]
    fn horizontal_pan_reaches_full_width_when_zoomed() {
        let (mut app, _st) = app_with_state();
        app.fit = FitMode::Custom(3.0);
        app.recompute_zoom();
        app.clamp_scroll();
        assert!(app.is_overflowing_x(), "3x zoom must overflow the viewport");
        app.scroll_x = f64::MAX;
        app.clamp_scroll();
        // The full reachable width is content_width - canvas width.
        let expected = (app.content_width() - app.canvas_rect.width()).max(0.0);
        assert!((app.scroll_x - expected).abs() < 1.0, "pan should reach the full zoomed width");
    }

    #[test]
    fn highlight_snaps_to_text_when_lines_present() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        // Seed a few text-line bands spanning two lines and drag across both.
        app.text_lines.insert(
            1,
            vec![
                render::TextLine { block: 5, x: 64.0, x_end: 480.0, y: 196.0, h: 13.0 },
                render::TextLine { block: 5, x: 64.0, x_end: 420.0, y: 210.0, h: 13.0 },
            ],
        );
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (80.0, 198.0),
            cur: (300.0, 220.0),
            points: vec![],
        });
        app.commit_drag(&mut st.doc);
        // Two covered lines -> two text-shaped bands (not one free rectangle).
        assert_eq!(app.annotations.len(), a0 + 2, "drag over two lines snaps to two bands");
    }

    #[test]
    fn highlight_falls_back_to_freeform_without_text() {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        // No text-line cache near the drag -> free-form rectangle (one anno).
        app.text_lines.insert(
            1,
            vec![render::TextLine { block: 5, x: 64.0, x_end: 480.0, y: 196.0, h: 13.0 }],
        );
        app.tool = Tool::Highlight;
        // Drag far below any seeded line.
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (80.0, 500.0),
            cur: (300.0, 540.0),
            points: vec![],
        });
        app.commit_drag(&mut st.doc);
        assert_eq!(app.annotations.len(), a0 + 1, "no nearby text -> single free-form band");
    }

    #[test]
    fn delete_selected_annotation_removes_it() {
        let (mut app, mut st) = app_with_state();
        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Highlight { .. }))
            .unwrap()
            .clone();
        let n = app.annotations.len();
        app.selected_anno = Some(hl.id.clone());
        assert!(app.delete_selected(&mut st.doc));
        assert_eq!(app.annotations.len(), n - 1);
        assert!(!app.annotations.iter().any(|a| a.id == hl.id));
        assert!(app.selected_anno.is_none());
    }

    #[test]
    fn delete_signature_target_removes_it() {
        let (mut app, mut st) = app_with_state();
        let sig = app.signatures[0].clone();
        let n = app.signatures.len();
        app.delete_target(&DeleteTarget::Signature(sig.id.clone()), &mut st.doc);
        assert_eq!(app.signatures.len(), n - 1);
        assert!(!app.signatures.iter().any(|s| s.id == sig.id));
    }

    #[test]
    fn delete_comment_target_also_drops_its_annotation() {
        let (mut app, mut st) = app_with_state();
        // The note's linked comment: deleting it must drop the note marker too.
        let note = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .unwrap()
            .clone();
        let comment = app
            .comments
            .iter()
            .find(|c| c.anno_id.as_deref() == Some(note.id.as_str()))
            .unwrap()
            .clone();
        app.delete_target(&DeleteTarget::Comment(comment.id.clone()), &mut st.doc);
        assert!(!app.comments.iter().any(|c| c.id == comment.id));
        assert!(!app.annotations.iter().any(|a| a.id == note.id), "orphan note marker removed");
    }

    #[test]
    fn placing_signature_focuses_signatures_sidebar() {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Comments;
        app.place_signature(&mut st.doc, 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Signatures as u8);
        assert!(app.show_sidebar);
        assert!(app.selected_sig.is_some(), "new signature is selected");
        assert!(app.sidebar_reveal.is_some(), "reveal request queued for the new entry");
    }

    #[test]
    fn creating_note_focuses_comments_sidebar_on_new_thread() {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Signatures;
        app.create_note(&mut st.doc, 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
        assert!(app.show_sidebar);
        assert!(app.sidebar_reveal.is_some(), "new comment queued to scroll into view");
    }

    #[test]
    fn rail_toggle_and_resize_reclaim_space() {
        let (mut app, _st) = app_with_state();
        let canvas_with_rail = app.canvas_rect.width();
        app.toggle_rail();
        assert!(!app.show_rail);
        assert!(app.canvas_rect.width() > canvas_with_rail, "hiding the rail reclaims space");
        app.toggle_rail();
        assert!(app.show_rail);
        // Resize within bounds is honoured.
        app.rail_w = 999.0;
        app.recompute_regions();
        assert!((app.rail_width() - RAIL_W_MAX).abs() < 1e-6, "rail width clamps to max");
    }

    #[test]
    fn search_toggles_change_match_logic() {
        let (mut app, _st) = app_with_state();
        app.query = "the".into();
        app.search_case_sensitive = false;
        app.search_whole_word = false;
        app.recompute_matches();
        let loose = app.matches.len();
        app.search_whole_word = true;
        app.recompute_matches();
        let strict = app.matches.len();
        assert!(strict <= loose, "whole-word never yields more matches than substring");
    }
}
