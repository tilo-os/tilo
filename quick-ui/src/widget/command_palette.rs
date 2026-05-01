//! `CommandPalette` primitive — a fuzzy-matching searchable command list.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 9 (`CommandPalette` primitive + `GlobalSearch` using it).
//!
//! The palette holds a list of [`CommandEntry`] rows, supports a query string
//! and an optional composable filter predicate, and exposes the currently
//! visible entries via [`CommandPalette::visible_entries`]. Enter invokes the
//! selected entry's `on_select` callback with the CRDT handle and root path,
//! matching `quick-ui`'s direct-mutation event model.

use automerge::{AutoCommit, ObjId};
use std::collections::HashSet;
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{self, Icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Scope chip in the GlobalSearch filter strip — concept renders these
/// as pill toggles immediately under the prompt row
/// (`nirios-concept/src/components/GlobalSearch.tsx:184-197`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// Limit results to the active workspace.
    Workspace,
    /// Search across every workspace.
    All,
}

/// Filetype filter chip — concept paints multi-select pills next to
/// the scope chips (`GlobalSearch.tsx:201-221`). The variant set here
/// matches the four buckets surfaced in the prompt; the underlying
/// command list still supplies its own filtering predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Filetype {
    Docs,
    Code,
    Image,
    Video,
}

impl Filetype {
    pub const ALL: [Filetype; 4] =
        [Filetype::Docs, Filetype::Code, Filetype::Image, Filetype::Video];

    /// Short, all-caps label rendered inside the chip (per concept's
    /// `text-[10px] font-bold uppercase`).
    pub fn label(&self) -> &'static str {
        match self {
            Filetype::Docs => "DOCS",
            Filetype::Code => "CODE",
            Filetype::Image => "IMG",
            Filetype::Video => "VID",
        }
    }
}

/// Visual + interaction state for the filter strip painted under the
/// prompt row. Concept `GlobalSearch.tsx:184-244` — scope chips +
/// filetype chips + a full-text toggle.
///
/// Kept inside the palette widget for now; once `quick-shell::search`
/// gains its own `SearchFilter` (plan task T6 step 3), the chrome can
/// keep that as the source of truth and mirror it onto the palette
/// each frame via the existing per-frame draw path.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchFilter {
    pub scope: SearchScope,
    pub filetypes: HashSet<Filetype>,
    pub full_text: bool,
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self {
            scope: SearchScope::Workspace,
            filetypes: HashSet::new(),
            full_text: false,
        }
    }
}

impl SearchFilter {
    pub fn toggle_full_text(&mut self) { self.full_text = !self.full_text; }

    pub fn toggle_filetype(&mut self, ft: Filetype) {
        if !self.filetypes.remove(&ft) {
            self.filetypes.insert(ft);
        }
    }

    pub fn set_scope(&mut self, scope: SearchScope) { self.scope = scope; }
}

type SelectFn = dyn Fn(&mut AutoCommit, &ObjId) + Send + Sync;

/// One row in the palette.
#[derive(Clone)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    pub on_select: Option<Arc<SelectFn>>,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("on_select", &self.on_select.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl CommandEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), on_select: None }
    }

    /// Attach a callback invoked when Enter is pressed on this entry.
    pub fn on_select<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut AutoCommit, &ObjId) + Send + Sync + 'static,
    {
        self.on_select = Some(Arc::new(f));
        self
    }
}

/// Subsequence fuzzy score: `Some(match_count - gap_penalty)` when `query`
/// matches `text` as a (case-insensitive) subsequence; `None` otherwise.
/// Empty queries match everything with score `0`.
fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() { return Some(0); }
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0usize;
    let mut matches = 0i32;
    let mut gaps = 0i32;
    let mut prev_match: Option<usize> = None;
    for (ti, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            if let Some(pm) = prev_match { gaps += (ti - pm - 1) as i32; }
            prev_match = Some(ti);
            qi += 1;
            matches += 1;
            if qi == q.len() { break; }
        }
    }
    if qi < q.len() { None } else { Some(matches - gaps) }
}

/// Geometry split of the prompt row into its three concept-spec slots.
///
/// Concept (`nirios-concept/src/components/GlobalSearch.tsx:138-153`)
/// places a leading lucide `Search` glyph + input + trailing `⌘K` kbd
/// chip inside the same row; this struct exposes the split so both
/// `draw` and the concept-parity tests share the same layout source of
/// truth.
#[derive(Clone, Debug, PartialEq)]
pub struct InputRowLayout {
    pub icon_rect: Rect,
    pub input_rect: Rect,
    pub kbd_chip_rect: Rect,
}

pub struct CommandPalette {
    entries: Vec<CommandEntry>,
    query: String,
    filter: Option<Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>>,
    visible: Vec<usize>,
    cursor: usize,
    rect: Rect,
    is_focused: bool,
    search_filter: SearchFilter,
}

impl Default for CommandPalette {
    fn default() -> Self { Self::new() }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            query: String::new(),
            filter: None,
            visible: Vec::new(),
            cursor: 0,
            rect: Rect::ZERO,
            is_focused: false,
            search_filter: SearchFilter::default(),
        }
    }

    /// Mutable handle to the filter strip's state — callers wire chip
    /// clicks to `search_filter_mut().toggle_*()`.
    pub fn search_filter(&self) -> &SearchFilter { &self.search_filter }
    pub fn search_filter_mut(&mut self) -> &mut SearchFilter {
        &mut self.search_filter
    }

    pub fn push(&mut self, e: CommandEntry) {
        self.entries.push(e);
        self.rebuild();
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.into();
        self.rebuild();
    }

    pub fn query(&self) -> &str { &self.query }

    pub fn set_filter(&mut self, f: Box<dyn Fn(&CommandEntry) -> bool + Send + Sync>) {
        self.filter = Some(f);
        self.rebuild();
    }

    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn cursor(&self) -> usize { self.cursor }

    /// Move the cursor to `index` within the visible window. Silently
    /// clamps to the last visible row so overlay callers that do hit-
    /// testing on the post-layout rects don't have to bounds-check.
    pub fn set_cursor(&mut self, index: usize) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = index.min(self.visible.len() - 1);
        }
    }

    /// Id of the currently-selected visible entry, or `None` if the
    /// filter matched no rows.
    pub fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.entries[i].id.as_str())
    }

    /// Rect of the `i`th *visible* row, in the same coordinate space as
    /// `get_rect()`. Returns `None` if `i` is past the end of the
    /// rendered window (the palette caps at 10 rows in `build_layout`).
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.visible.len() {
            return None;
        }
        let list_top = self.list_top_y();
        let y0 = list_top + (i as f64) * ROW_HEIGHT;
        let y1 = y0 + ROW_HEIGHT;
        // The footer + its gap eat into the bottom of the card; cap at
        // the start of the footer band rather than `rect.y1 - PADDING`.
        let max_y = self.footer_rect().y0 - PADDING;
        if y1 > max_y {
            return None;
        }
        Some(Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1))
    }

    /// Y coordinate where the result list begins (immediately under the
    /// filter strip + its gap). Shared by `row_rect` and `draw`.
    fn list_top_y(&self) -> f64 {
        self.rect.y0 + PADDING + INPUT_HEIGHT + PADDING
            + FILTER_STRIP_HEIGHT + PADDING
    }

    /// Index of the visible row whose rect contains `p`, if any. Used by
    /// overlay callers that accept a click inside the palette rect and
    /// want to move the cursor to the clicked row.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        (0..self.visible.len()).find(|&i| {
            self.row_rect(i).map(|r| r.contains(p)).unwrap_or(false)
        })
    }

    /// Concept (`nirios-concept/src/components/GlobalSearch.tsx:120-150`)
    /// wraps the palette card in `rounded-3xl` (Tailwind = 24 px). Pin
    /// here so the radius is one source of truth shared by `draw` and
    /// the concept-parity tests.
    pub fn surface_radius_px(&self) -> f64 {
        24.0
    }

    /// Splits the input row into three sub-rects: a leading 24 px icon
    /// slot (lucide `Search`), the input field, and a trailing 56 px
    /// kbd-chip slot (`⌘K`). Both flanking slots are inset 12 px from
    /// the row edge, with another 12 px gap before/after the input.
    ///
    /// Concept ref: `GlobalSearch.tsx:138-153` — leading magnifier glyph
    /// + input + trailing `⌘K` kbd pill.
    pub fn compute_input_layout(&self, row: Rect) -> InputRowLayout {
        const ICON_W: f64 = 24.0;
        const CHIP_W: f64 = 48.0;
        const EDGE: f64 = 12.0;
        const GAP: f64 = 12.0;
        let cy = (row.y0 + row.y1) / 2.0;
        let icon_y0 = cy - ICON_W / 2.0;
        let icon_rect = Rect::new(
            row.x0 + EDGE,
            icon_y0,
            row.x0 + EDGE + ICON_W,
            icon_y0 + ICON_W,
        );
        let chip_h = (row.height() - EDGE).max(20.0);
        let chip_y0 = cy - chip_h / 2.0;
        let kbd_chip_rect = Rect::new(
            row.x1 - EDGE - CHIP_W,
            chip_y0,
            row.x1 - EDGE,
            chip_y0 + chip_h,
        );
        let input_rect = Rect::new(
            icon_rect.x1 + GAP,
            row.y0,
            kbd_chip_rect.x0 - GAP,
            row.y1,
        );
        InputRowLayout {
            icon_rect,
            input_rect,
            kbd_chip_rect,
        }
    }

    /// Rect of the prompt row inside the post-layout palette rect.
    /// Used by [`compute_input_layout`] callers that don't want to
    /// reconstruct the inset arithmetic at every call site.
    pub fn input_row_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + INPUT_HEIGHT,
        )
    }

    /// 40 px filter strip painted directly under the prompt row. Holds
    /// scope chips on the left, filetype chips in the middle, and a
    /// full-text toggle on the right (concept
    /// `GlobalSearch.tsx:184-244`).
    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + FILTER_STRIP_HEIGHT)
    }

    /// Footer band painted at the bottom of the palette card with
    /// `↑↓ Navigate / Enter Select` kbd-chip hints + a right-aligned
    /// scope label. Concept `GlobalSearch.tsx:274-280`.
    pub fn footer_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y1 - PADDING - FOOTER_HEIGHT,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    /// Layout split for one filter chip. Each chip is a rounded-full
    /// pill `px-2 py-1` (≈ 12 px horizontal padding) centred vertically
    /// in the strip.
    fn chip_rect(strip: Rect, x: f64, width: f64) -> Rect {
        let h = (strip.height() - 8.0).max(20.0);
        let cy = (strip.y0 + strip.y1) / 2.0;
        Rect::new(x, cy - h / 2.0, x + width, cy + h / 2.0)
    }

    /// Paint scope chips + filetype chips + full-text toggle into the
    /// filter strip. Active chip = `accent_primary` (indigo-500) fill +
    /// `text_primary` (white) text; idle = white/5 fill + `text_dim`
    /// (slate-400) text. The full-text toggle uses `accent[2]` (≈
    /// emerald-500) when on, matching concept `bg-emerald-500/20`.
    fn draw_filter_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let strip = self.filter_strip_rect();
        let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);

        let active_fill = theme.tokens.colors.accent[5];
        let idle_fill = Color::rgba8(255, 255, 255, 13);  // white/5
        let active_text = theme.tokens.colors.text_primary;
        let idle_text = theme.tokens.colors.text_dim;

        // Scope chips on the left.
        let mut x = strip.x0;
        for (scope, label) in [
            (SearchScope::Workspace, "WORKSPACE"),
            (SearchScope::All, "ALL"),
        ] {
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            let chip = Self::chip_rect(strip, x, chip_w);
            let active = self.search_filter.scope == scope;
            self.draw_chip(
                scene,
                transform,
                text_renderer,
                chip,
                label,
                chip_text_size,
                if active { active_fill } else { idle_fill },
                if active { active_text } else { idle_text },
            );
            x += chip_w + 6.0;
        }

        // Right-anchored full-text toggle.
        let ft_label = "FULL TEXT";
        let (ft_w, _) = text_renderer.measure(ft_label, chip_text_size, None);
        let ft_chip_w = ft_w as f64 + 16.0;
        let ft_chip = Self::chip_rect(strip, strip.x1 - ft_chip_w, ft_chip_w);
        let ft_on = self.search_filter.full_text;
        // Concept `GlobalSearch.tsx:227-231`: `bg-emerald-500/20
        // text-emerald-400` when on. Hex literals are pinned by the
        // plan's palette table.
        let ft_active_fill = Color::rgba8(34, 197, 94, 51);  // emerald-500/20
        let ft_active_text = Color::rgba8(0x4a, 0xde, 0x80, 0xff);  // emerald-400
        self.draw_chip(
            scene,
            transform,
            text_renderer,
            ft_chip,
            ft_label,
            chip_text_size,
            if ft_on { ft_active_fill } else { idle_fill },
            if ft_on { ft_active_text } else { idle_text },
        );

        // Filetype chips fill the gap between scope chips and the
        // full-text toggle. Right-bound stops 8 px before the toggle.
        let mut fx = x;
        let max_x = ft_chip.x0 - 8.0;
        for ft in Filetype::ALL {
            let label = ft.label();
            let (w, _) = text_renderer.measure(label, chip_text_size, None);
            let chip_w = w as f64 + 16.0;
            if fx + chip_w > max_x { break; }
            let chip = Self::chip_rect(strip, fx, chip_w);
            let active = self.search_filter.filetypes.contains(&ft);
            self.draw_chip(
                scene,
                transform,
                text_renderer,
                chip,
                label,
                chip_text_size,
                if active { active_fill } else { idle_fill },
                if active { active_text } else { idle_text },
            );
            fx += chip_w + 6.0;
        }
    }

    /// Single chip body + label. Shared by scope/filetype/full-text
    /// renders so they all pin to the same `rounded-full` shape.
    fn draw_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        chip: Rect,
        label: &str,
        text_size: f32,
        body: Color,
        text_color: Color,
    ) {
        // `rounded-full` ≈ half-height radius.
        let radius = chip.height() / 2.0;
        let shape = RoundedRect::from_rect(chip, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            body,
            None,
            &shape,
        );
        let (w, h) = text_renderer.measure(label, text_size, None);
        let tx = (chip.x0 + chip.x1) / 2.0 - w as f64 / 2.0;
        let ty = (chip.y0 + chip.y1) / 2.0 - h as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            text_size,
            text_color,
            transform * Affine::translate((tx, ty)),
            None,
        );
    }

    /// Footer paint: `↑↓ Navigate / Enter Select` on the left and the
    /// active scope label on the right, on top of a `bg-black/20` band
    /// with a 1 px slate-700 top border.
    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let footer = self.footer_rect();
        // `bg-black/20` band — 20% black over the slate surface.
        let band_body = Color::rgba8(0, 0, 0, 51);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            band_body,
            None,
            &footer,
        );
        // 1 px top border (slate-700 ≈ `border_standard`).
        let border = vello::kurbo::Line::new(
            (footer.x0, footer.y0),
            (footer.x1, footer.y0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &border,
        );

        let chip_text_size = (theme.tokens.typography.scale.sm - 2.0).max(9.0);
        let chip_body = Color::rgba8(255, 255, 255, 13);  // white/5
        let chip_border = Color::rgba8(255, 255, 255, 26);  // white/10
        let dim = theme.tokens.colors.text_dim;
        let chip_h = (footer.height() - 14.0).max(14.0);
        let cy = (footer.y0 + footer.y1) / 2.0;

        // Left cluster: kbd chip + label, repeated for ↑↓ / Enter.
        let mut x = footer.x0 + 4.0;
        for (key, label) in [("\u{2191}\u{2193}", "Navigate"), ("Enter", "Select")] {
            let (kw, kh) = text_renderer.measure(key, chip_text_size, None);
            let chip_w = kw as f64 + 10.0;
            let chip_rect =
                Rect::new(x, cy - chip_h / 2.0, x + chip_w, cy + chip_h / 2.0);
            let chip_shape = RoundedRect::from_rect(chip_rect, 4.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                chip_body,
                None,
                &chip_shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                chip_border,
                None,
                &chip_shape,
            );
            text_renderer.draw_text(
                scene,
                key,
                chip_text_size,
                dim,
                transform
                    * Affine::translate((
                        chip_rect.x0 + (chip_rect.width() - kw as f64) / 2.0,
                        chip_rect.y0 + (chip_rect.height() - kh as f64) / 2.0,
                    )),
                None,
            );
            x = chip_rect.x1 + 4.0;
            let (lw, lh) = text_renderer.measure(label, chip_text_size, None);
            text_renderer.draw_text(
                scene,
                label,
                chip_text_size,
                dim,
                transform * Affine::translate((x, cy - lh as f64 / 2.0)),
                None,
            );
            x += lw as f64 + 12.0;
        }

        // Right-anchored scope label.
        let scope_label = match self.search_filter.scope {
            SearchScope::Workspace => "in Workspace",
            SearchScope::All => "in All Workspaces",
        };
        let (sw, sh) = text_renderer.measure(scope_label, chip_text_size, None);
        text_renderer.draw_text(
            scene,
            scope_label,
            chip_text_size,
            dim,
            transform * Affine::translate((
                footer.x1 - sw as f64 - 4.0,
                cy - sh as f64 / 2.0,
            )),
            None,
        );
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(f) = &self.filter {
                if !f(e) { continue; }
            }
            if fuzzy_score(&self.query, &e.label).is_none() { continue; }
            self.visible.push(i);
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }
}

const ROW_HEIGHT: f64 = 32.0;
const INPUT_HEIGHT: f64 = 36.0;
const PADDING: f64 = 8.0;
/// Concept `GlobalSearch.tsx:184` — a `py-2` strip (≈ 40 px) wrapping
/// the chip row.
const FILTER_STRIP_HEIGHT: f64 = 40.0;
/// Concept `GlobalSearch.tsx:274-280` — a `p-3` band (≈ 36 px) holding
/// the kbd-chip hints + scope label.
const FOOTER_HEIGHT: f64 = 36.0;

impl Widget for CommandPalette {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rows = self.visible.len().min(10).max(1) as f32;
        // padding (top) + input row + gap + filter strip + gap + list +
        // gap + footer + padding (bottom). The `gap` between sections
        // reuses `PADDING` so the surface stays a single visual rhythm.
        let height = (PADDING as f32) * 2.0
            + INPUT_HEIGHT as f32
            + PADDING as f32
            + FILTER_STRIP_HEIGHT as f32
            + PADDING as f32
            + rows * ROW_HEIGHT as f32
            + PADDING as f32
            + FOOTER_HEIGHT as f32;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(360.0),
                    height: Dimension::length(height),
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Popover-shaped surface with a soft border. Concept's
        // `rounded-3xl` (= 24 px) lives in [`surface_radius_px`] so the
        // value is pinned by the radius test and reused here.
        let shape = RoundedRect::from_rect(self.rect, self.surface_radius_px());
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );

        // Split the prompt row into leading-icon / input / trailing
        // kbd-chip slots — concept `GlobalSearch.tsx:138-153`.
        let row_rect = self.input_row_rect();
        let layout = self.compute_input_layout(row_rect);
        let input_shape =
            RoundedRect::from_rect(layout.input_rect, theme.tokens.radii.sm as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &input_shape,
        );
        let border_color = if self.is_focused {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        scene.stroke(
            &Stroke::new(if self.is_focused { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input_shape,
        );

        // Leading lucide `Search` glyph (slate-400 = `text_dim`). Drawn
        // unconditionally whenever the palette is open — concept never
        // hover-gates this affordance.
        icons::draw_icon(
            scene,
            transform,
            layout.icon_rect,
            Icon::Search,
            theme.tokens.colors.text_dim,
            1.5,
        );

        // Trailing `⌘K` kbd chip — `bg-white/5 border border-white/10
        // text-slate-300 font-mono rounded-md`. Body fill + border
        // stroke + small mono label.
        let chip_shape = RoundedRect::from_rect(layout.kbd_chip_rect, 6.0);
        // Concept: `bg-white/5 border border-white/10` — semi-transparent
        // white at 5% / 10% opacity over the slate-900 surface tint.
        let chip_body = Color::rgba8(255, 255, 255, 13);
        let chip_border = Color::rgba8(255, 255, 255, 26);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            chip_body,
            None,
            &chip_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            chip_border,
            None,
            &chip_shape,
        );
        let chip_text = "\u{2318}K"; // ⌘K
        let chip_text_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let (cw, ch) = text_renderer.measure(chip_text, chip_text_size, None);
        let chip_text_x = (layout.kbd_chip_rect.x0 + layout.kbd_chip_rect.x1) / 2.0
            - cw as f64 / 2.0;
        let chip_text_y = (layout.kbd_chip_rect.y0 + layout.kbd_chip_rect.y1) / 2.0
            - ch as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            chip_text,
            chip_text_size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((chip_text_x, chip_text_y)),
            None,
        );

        // Query / placeholder text inside the input slot.
        let display = if self.query.is_empty() { "Search\u{2026}" } else { &self.query };
        let text_color = if self.query.is_empty() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        let text_size = theme.tokens.typography.scale.sm;
        let text_y = layout.input_rect.y0
            + (layout.input_rect.height() - text_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            display,
            text_size,
            text_color,
            transform * Affine::translate((layout.input_rect.x0 + 8.0, text_y)),
            None,
        );

        // Filter strip — scope chips, filetype chips, full-text toggle.
        // Concept `nirios-concept/src/components/GlobalSearch.tsx:184-244`.
        self.draw_filter_strip(scene, transform, text_renderer, theme);

        // Vertical list of visible entries.
        let list_top = self.list_top_y();
        let footer_top = self.footer_rect().y0;
        for (i, &entry_idx) in self.visible.iter().enumerate() {
            let y0 = list_top + (i as f64) * ROW_HEIGHT;
            let y1 = y0 + ROW_HEIGHT;
            if y1 > footer_top - PADDING {
                break;
            }
            let entry_rect =
                Rect::new(self.rect.x0 + PADDING, y0, self.rect.x1 - PADDING, y1);
            if i == self.cursor {
                let highlight =
                    RoundedRect::from_rect(entry_rect, theme.tokens.radii.sm as f64);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[4],
                    None,
                    &highlight,
                );
            }
            let label = &self.entries[entry_idx].label;
            let label_y = entry_rect.y0 + (entry_rect.height() - text_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                text_size,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((entry_rect.x0 + 8.0, label_y)),
                None,
            );
        }

        // Footer band: kbd-chip hints on the left + scope label on the
        // right. Concept `GlobalSearch.tsx:274-280`.
        self.draw_footer(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.is_focused { return; }
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: element_state, .. },
            ..
        } = event
        {
            if *element_state != ElementState::Pressed { return; }
            match logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    if !self.visible.is_empty() {
                        self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::ArrowUp) => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(&idx) = self.visible.get(self.cursor) {
                        if let Some(cb) = self.entries[idx].on_select.clone() {
                            cb(state, path);
                        }
                        *handled = true;
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    self.is_focused = false;
                    *handled = true;
                }
                Key::Named(NamedKey::Backspace) => {
                    if self.query.pop().is_some() {
                        self.rebuild();
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::Space) => {
                    self.query.push(' ');
                    self.rebuild();
                    *handled = true;
                }
                Key::Character(s) => {
                    // Same shape as `overlay_host.rs:1383-1395` — push
                    // every non-control char carried by the smol-str
                    // payload, then rebuild + consume the event.
                    let mut pushed = false;
                    for c in s.chars() {
                        if !c.is_control() {
                            self.query.push(c);
                            pushed = true;
                        }
                    }
                    if pushed {
                        self.rebuild();
                        *handled = true;
                    }
                }
                _ => {}
            }
        }
    }

    fn name(&self) -> &str { "CommandPalette" }

    fn get_rect(&self) -> Rect { self.rect }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::TaffyTree;
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    /// Synthesise a `WindowEvent::KeyboardInput` carrying `logical_key`
    /// in the `Pressed` state. Mirrors the zero-init workaround in
    /// `quick-shell/tests/e2e_shell_flows.rs:1287-1307` — winit 0.30
    /// keeps `KeyEventExtra` `pub(crate)` so we can't construct a
    /// `KeyEvent` through a struct literal.
    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: all pub fields of `KeyEvent` tolerate the all-zero bit
        // pattern; `handle_event` only inspects `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    fn fuzzy_score_empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_subsequence_matches() {
        assert!(fuzzy_score("ren", "Rename Workspace").is_some());
    }

    #[test]
    fn fuzzy_score_non_subsequence_is_none() {
        assert_eq!(fuzzy_score("xyz", "Rename Workspace"), None);
    }

    #[test]
    fn set_cursor_clamps_to_visible_len() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_cursor(99);
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn selected_id_reflects_cursor_position() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        assert_eq!(p.selected_id(), Some("a"));
        p.set_cursor(1);
        assert_eq!(p.selected_id(), Some("b"));
    }

    #[test]
    fn selected_id_is_none_when_filter_removes_everything() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.set_filter(Box::new(|_| false));
        assert_eq!(p.selected_id(), None);
    }

    #[test]
    fn palette_card_radius_is_24_px() {
        let palette = CommandPalette::new();
        assert_eq!(
            palette.surface_radius_px(),
            24.0,
            "concept palette uses rounded-3xl = 24 px"
        );
    }

    #[test]
    fn input_row_layout_reserves_leading_icon_and_kbd_chip() {
        let palette = CommandPalette::new();
        let layout = palette.compute_input_layout(Rect::new(0.0, 0.0, 640.0, 56.0));
        assert!(
            layout.icon_rect.width() >= 24.0 && layout.icon_rect.width() <= 40.0,
            "leading icon slot must be 24..=40 px wide; got {}",
            layout.icon_rect.width()
        );
        assert!(
            layout.kbd_chip_rect.width() >= 48.0,
            "Cmd+K chip needs at least 48 px; got {}",
            layout.kbd_chip_rect.width()
        );
        assert!(
            layout.kbd_chip_rect.x1 < 640.0,
            "kbd chip must be inside the row"
        );
        assert!(
            layout.input_rect.x0 > layout.icon_rect.x1,
            "input must start after the icon"
        );
        assert!(
            layout.input_rect.x1 < layout.kbd_chip_rect.x0,
            "input must end before the kbd chip"
        );
    }

    #[test]
    fn row_at_finds_visible_row_inside_the_post_layout_rect() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        // Seat the palette rect so row_rect has meaningful coordinates.
        // Height must accommodate input row + filter strip + 2 result
        // rows + footer band — see `build_layout`.
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let mid_row_1 = p.row_rect(1).unwrap().center();
        assert_eq!(p.row_at(mid_row_1), Some(1));
        // Clicks outside the rect return None.
        assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
    }

    // -------------------------------------------------------------------
    // Task 6 — filter strip + footer hints
    //
    // Failing tests added FIRST (per the plan's TDD discipline). They
    // pin the SearchFilter state machine and the filter-strip / footer
    // layout split. Concept ref:
    // `nirios-concept/src/components/GlobalSearch.tsx:184-244,274-280`.
    // -------------------------------------------------------------------

    #[test]
    fn search_filter_full_text_toggles() {
        let mut f = SearchFilter::default();
        assert!(!f.full_text);
        f.toggle_full_text();
        assert!(f.full_text);
        f.toggle_full_text();
        assert!(!f.full_text);
    }

    #[test]
    fn search_filter_filetypes_multi_select() {
        let mut f = SearchFilter::default();
        f.toggle_filetype(Filetype::Code);
        f.toggle_filetype(Filetype::Docs);
        assert!(f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
        // Toggling again removes the entry — multi-select acts like a set.
        f.toggle_filetype(Filetype::Code);
        assert!(!f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
    }

    #[test]
    fn search_filter_default_scope_is_workspace() {
        let f = SearchFilter::default();
        assert_eq!(f.scope, SearchScope::Workspace);
    }

    #[test]
    fn filter_strip_sits_below_input_row_and_above_results() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let input = p.input_row_rect();
        let strip = p.filter_strip_rect();
        let row0 = p.row_rect(0).expect("seeded row 0 must lay out");
        assert!(
            strip.y0 >= input.y1,
            "filter strip must start at or below input row (input.y1={}, strip.y0={})",
            input.y1,
            strip.y0,
        );
        assert!(
            strip.y1 <= row0.y0,
            "results list must start at or below filter strip (strip.y1={}, row0.y0={})",
            strip.y1,
            row0.y0,
        );
        assert!(
            strip.height() >= 32.0,
            "filter strip must be tall enough for chip pills (got {})",
            strip.height(),
        );
    }

    #[test]
    fn typing_filters_visible_entries() {
        // B-1: typing into a focused palette must filter the visible
        // result list and update the query string. Drives the
        // production `handle_event` code path with a real
        // `WindowEvent::KeyboardInput { Key::Character("h") }`.
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("h", "Hotel"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_focus(true);

        let total = p.entries.len();
        // Sanity: with no query, all entries are visible.
        assert_eq!(p.visible_entries().len(), total);

        let event = key_press(Key::Character(SmolStr::new("h")));
        let mut doc = AutoCommit::new();
        let doc_root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();

        p.handle_event(
            &event,
            1.0,
            &modifiers,
            &mut doc,
            &doc_root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        assert_eq!(p.query(), "h", "typing must update the query string");
        assert!(
            p.visible_entries().len() < total,
            "typing 'h' must filter out non-matching entries (visible={}, total={})",
            p.visible_entries().len(),
            total,
        );
        assert!(handled, "the palette must consume the keystroke");
    }

    #[test]
    fn footer_sits_below_results_at_card_bottom() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let row0 = p.row_rect(0).expect("seeded row 0 must lay out");
        let footer = p.footer_rect();
        assert!(
            footer.y0 >= row0.y1,
            "footer must sit below the result list (row0.y1={}, footer.y0={})",
            row0.y1,
            footer.y0,
        );
        assert!(
            footer.y1 <= p.rect.y1,
            "footer must stay inside the card (rect.y1={}, footer.y1={})",
            p.rect.y1,
            footer.y1,
        );
        assert!(
            footer.height() >= 24.0,
            "footer must be tall enough for kbd chips (got {})",
            footer.height(),
        );
    }
}
