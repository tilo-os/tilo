//! `PageSidebar` — a shared, reorderable page/slide preview rail: the PDF
//! reader's page rail and the presentation editor's slide rail both drive
//! this one widget. It owns the list geometry, scroll, drag-to-reorder
//! (via [`DragController`]), a right-click menu, and an optional "add"
//! affordance, emitting [`PageSidebarEvent`]s the host applies to its model.
//!
//! The widget does **not** know how to paint a page or slide — the host
//! passes a thumbnail-draw closure — but draws everything else (frame,
//! selection ring, index number, reflow gap, drag ghost, scrollbar, menu).
//!
//! The host is imperative (draws into a `Scene` each frame), so the primary
//! API is [`PageSidebar::begin_frame`] / `render` / pointer-
//! event methods; the [`Widget`] impl is a thin shim for the e2e harness.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::drag::{DragController, ListMetrics};

/// Something the host should do to its page/slide model in response to a
/// sidebar interaction. The ordering-changing variants flag that a save is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSidebarEvent {
    /// Make page `index` the current one (a plain click).
    Select(usize),
    /// Move the page at `from` to final index `to` (a committed drag or a
    /// "move up/down" menu action). Semantics match `Vec::remove(from)` then
    /// `Vec::insert(to, _)`.
    Reorder {
        from: usize,
        to: usize,
    },
    /// Inserting the copy right after `index`.
    Duplicate(usize),
    Delete(usize),
    /// The bottom "add" affordance was activated (import / new slide).
    Add,
}

/// Move the item at `from` to final index `to` in `v`, matching the
/// `(from, to)` a [`DragController`] commit yields. A no-op when either index
/// is out of range or they are equal. Shared so the reorder applied to the
/// model is identical to the reflow the rail previewed.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= v.len() || to >= v.len() {
        return;
    }
    let item = v.remove(from);
    v.insert(to, item);
}

/// Insert a clone of the item at `index` immediately after it. A no-op when
/// `index` is out of range.
pub fn duplicate_after<T: Clone>(v: &mut Vec<T>, index: usize) {
    let Some(copy) = v.get(index).cloned() else {
        return;
    };
    v.insert(index.saturating_add(1), copy);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Duplicate,
    MoveUp,
    MoveDown,
    Delete,
}

struct Menu {
    /// The page the menu targets.
    index: usize,
    rect: Rect,
    /// `(action, row-rect)` — separators are absent (drawn between groups).
    items: Vec<(MenuAction, Rect)>,
    hover: Option<usize>,
}

// Rail geometry. The rail lays out with a [`RailMetrics`] value: the
// constants below are the spacing scale's defaults restated, on the 4 px
// grid, and form [`RailMetrics::DEFAULT`] — what `begin_frame` uses for a
// host whose layout pass has no `Theme` in hand. `begin_frame_with_theme`
// reads the same values from `theme.tokens` ([`RailMetrics::from_theme`]).
//
//   PAD 12 (= s3), GAP 12 (= s3), NUM_GUTTER 24 (= s6), HEADER_H 32 (= s8),
//   ADD_H 36, ADD_MARGIN 8 (= s2), MENU_W 184, MENU_ROW_H 32 (= s8),
//   MENU_SEP_H 8 (= s2), MENU_PAD 8 (= s2), MENU_ROW_INSET 4 (= s1)
//
// SCROLLBAR_W 6 is the one off-grid value: it matches the toolkit's shared
// scrollbar so the rail's own bar reads as the same control.
const PAD: f64 = 12.0;
const GAP: f64 = 12.0;
const NUM_GUTTER: f64 = 24.0;
const SCROLLBAR_W: f64 = 6.0;
/// Inset of the scrollbar track from the rail's right edge.
const SCROLLBAR_INSET: f64 = 2.0;
const HEADER_H: f64 = 32.0;
const ADD_H: f64 = 36.0;
/// Gap under the add button, to the rail's bottom edge.
const ADD_MARGIN: f64 = 8.0;
const MENU_W: f64 = 184.0;
const MENU_ROW_H: f64 = 32.0;
const MENU_SEP_H: f64 = 8.0;
/// Padding above the first and below the last menu row.
const MENU_PAD: f64 = 8.0;
/// Inset of a menu row (and of the menu from the rail's edges).
const MENU_ROW_INSET: f64 = 4.0;

/// The rail's geometry, on the spacing scale. [`RailMetrics::DEFAULT`]
/// restates the scale as the constants above — what
/// [`PageSidebar::begin_frame`] has always laid out with, for a host that
/// runs its layout pass without a `Theme`. A host that has one calls
/// [`PageSidebar::begin_frame_with_theme`] and the same values are read
/// from `theme.tokens` instead, so a density change reaches the rail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RailMetrics {
    /// Inset around the list and the header label (`spacing.s3`).
    pub pad: f64,
    /// Vertical gap between thumbnails (`spacing.s3`).
    pub gap: f64,
    /// Width of the page-number gutter left of the thumbnails (`spacing.s6`).
    pub num_gutter: f64,
    /// Height of the panel header band (`spacing.s8`).
    pub header_h: f64,
    /// Height of the bottom add button (`metrics.bar_height` when themed;
    /// the restated default is 36).
    pub add_h: f64,
    /// Gap under the add button, to the rail's bottom edge (`spacing.s2`).
    pub add_margin: f64,
    /// Width of the context-menu popover (no token; the constant is kept).
    pub menu_w: f64,
    /// Height of one menu row (`spacing.s8`).
    pub menu_row_h: f64,
    /// Height of the separator before Delete (`spacing.s2`).
    pub menu_sep_h: f64,
    /// Padding above the first and below the last menu row (`spacing.s2`).
    pub menu_pad: f64,
    /// Inset of a menu row, and of the menu from the rail's edges (`spacing.s1`).
    pub menu_row_inset: f64,
}

impl RailMetrics {
    /// The spacing scale restated, for a layout pass with no `Theme`.
    pub const DEFAULT: Self = Self {
        pad: PAD,
        gap: GAP,
        num_gutter: NUM_GUTTER,
        header_h: HEADER_H,
        add_h: ADD_H,
        add_margin: ADD_MARGIN,
        menu_w: MENU_W,
        menu_row_h: MENU_ROW_H,
        menu_sep_h: MENU_SEP_H,
        menu_pad: MENU_PAD,
        menu_row_inset: MENU_ROW_INSET,
    };

    /// The same geometry read from the theme's tokens. The add button is a
    /// bar button (`metrics.bar_height`) rather than the restated 36; the
    /// menu width has no token and keeps its constant.
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        Self {
            pad: f64::from(sp.s3),
            gap: f64::from(sp.s3),
            num_gutter: f64::from(sp.s6),
            header_h: f64::from(sp.s8),
            add_h: f64::from(theme.tokens.metrics.bar_height),
            add_margin: f64::from(sp.s2),
            menu_w: MENU_W,
            menu_row_h: f64::from(sp.s8),
            menu_sep_h: f64::from(sp.s2),
            menu_pad: f64::from(sp.s2),
            menu_row_inset: f64::from(sp.s1),
        }
    }
}

impl Default for RailMetrics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Renderer-relative size of every label the rail paints (header, page
/// numbers, add button, menu rows): `scale.sm`, with the theme's text scale
/// divided once by the renderer's text scale because the renderer applies
/// it again.
fn label_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

/// Widens a row/action count to `f64` for layout math, through `u32` (exact
/// in `f64`) with a saturating fallback rather than an `as` cast — a page
/// rail never approaches `u32::MAX` rows.
fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` is pure
/// `f64` matrix math with no overflow trap, so it cannot panic regardless of
/// operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(base: Affine, x: f64, y: f64) -> Affine {
    base * Affine::translate((x, y))
}

/// Offsets a rect vertically. `Rect`'s `Add<Vec2>` is pure `f64` geometry
/// with no overflow trap, so it cannot panic regardless of operand
/// magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Rect + Vec2 is float geometry translation and cannot panic"
)]
fn shifted(rect: Rect, dy: f64) -> Rect {
    rect + Vec2::new(0.0, dy)
}

pub struct PageSidebar {
    // ---- configuration ----
    /// Thumbnail height as a multiple of its width (`page_h / page_w`).
    aspect: f64,
    header: String,
    add_label: String,
    show_add: bool,
    /// When true the widget paints its own panel background + header + divider
    /// (the PDF reader owns a full left column); when false it paints only the
    /// thumbnail list inside `rect` (the slides rail supplies its own panel).
    draw_panel: bool,
    /// Geometry this frame lays out and paints with; see [`RailMetrics`].
    m: RailMetrics,

    // ---- per-frame inputs ----
    count: usize,
    current: usize,
    rect: Rect,

    // ---- derived geometry (recomputed in begin_frame) ----
    list_vp: Rect,
    thumb_w: f64,
    thumb_h: f64,
    list_top: f64,
    max_scroll: f64,
    add_rect: Rect,

    // ---- interaction state ----
    scroll: f64,
    drag: DragController<usize>,
    /// Index pressed this gesture (used to distinguish a click from a drag).
    press_idx: Option<usize>,
    last_pointer: Point,
    menu: Option<Menu>,
    pending: Vec<PageSidebarEvent>,
    /// Pages that carry a mark (annotations, comments).
    marked: Vec<usize>,
}

/// The hairline that seats a page thumbnail on the rail.
const PAGE_EDGE_ALPHA: u8 = 26;

impl PageSidebar {
    /// A rail whose thumbnails have aspect ratio `page_h / page_w`
    /// (e.g. `792.0/612.0` for US-Letter PDF pages, `720.0/1280.0` for slides).
    pub fn new(aspect: f64) -> Self {
        Self {
            aspect: aspect.max(0.05),
            header: "Pages".to_string(),
            add_label: String::new(),
            show_add: false,
            draw_panel: true,
            m: RailMetrics::DEFAULT,
            count: 0,
            current: 0,
            rect: Rect::ZERO,
            list_vp: Rect::ZERO,
            thumb_w: 1.0,
            thumb_h: 1.0,
            list_top: 0.0,
            max_scroll: 0.0,
            add_rect: Rect::ZERO,
            scroll: 0.0,
            drag: DragController::vertical(),
            press_idx: None,
            last_pointer: Point::ZERO,
            menu: None,
            pending: Vec::new(),
            marked: Vec::new(),
        }
    }

    /// The pages that carry a mark (annotations, comments): a dot inside
    /// the thumbnail's corner.
    pub fn set_marked(&mut self, pages: Vec<usize>) {
        self.marked = pages;
    }

    #[must_use]
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header = header.into();
        self
    }

    /// Show a bottom "add" button carrying `label` (e.g. "Import…", "New slide").
    #[must_use]
    pub fn with_add(mut self, label: impl Into<String>) -> Self {
        self.add_label = label.into();
        self.show_add = true;
        self
    }

    /// Set false when the host already draws the panel chrome around the list.
    #[must_use]
    pub fn with_panel(mut self, draw_panel: bool) -> Self {
        self.draw_panel = draw_panel;
        self
    }

    pub fn set_aspect(&mut self, aspect: f64) {
        self.aspect = aspect.max(0.05);
    }

    pub fn scroll(&self) -> f64 {
        self.scroll
    }

    pub fn set_scroll(&mut self, s: f64) {
        self.scroll = s.clamp(0.0, self.max_scroll);
    }

    /// Whether a reorder drag is live (ghost + gap showing) — the host wires
    /// this to its redraw/pointer-capture logic.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_active()
    }

    pub fn is_pending_drag(&self) -> bool {
        self.drag.is_pending()
    }

    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Maximum scroll offset for the current frame's geometry (0 when the
    /// whole list fits).
    pub fn max_scroll(&self) -> f64 {
        self.max_scroll
    }

    /// The live target index a held reorder drag would land at (`0..=count-1`),
    /// or `None` when no drag is active. Passthrough of the shared
    /// [`DragController`] so a host can preview / assert the drop row.
    pub fn drag_insertion(&self) -> Option<usize> {
        self.drag.insertion()
    }

    /// The original index of the slide being dragged, once the drag is active.
    pub fn drag_from(&self) -> Option<usize> {
        self.drag.from_index()
    }

    pub fn take_events(&mut self) -> Vec<PageSidebarEvent> {
        std::mem::take(&mut self.pending)
    }

    // ---- geometry ----------------------------------------------------

    /// [`Self::begin_frame`] with the geometry read from `theme`
    /// ([`RailMetrics::from_theme`]) rather than the restated scale — the
    /// variant for a host whose layout pass has a `Theme` in hand.
    pub fn begin_frame_with_theme(
        &mut self,
        theme: &Theme,
        rect: Rect,
        count: usize,
        current: usize,
    ) {
        self.m = RailMetrics::from_theme(theme);
        self.begin_frame(rect, count, current);
    }

    /// Recompute list geometry for this frame and clamp the scroll. Call once
    /// per frame before [`render`](Self::render) and before hit-testing.
    /// Lays out with whatever [`RailMetrics`] the rail holds:
    /// [`RailMetrics::DEFAULT`] unless [`Self::begin_frame_with_theme`] has
    /// handed it the theme's.
    pub fn begin_frame(&mut self, rect: Rect, count: usize, current: usize) {
        self.rect = rect;
        self.count = count;
        self.current = current.min(count.saturating_sub(1));

        let header_h = if self.draw_panel {
            self.m.header_h
        } else {
            0.0
        };
        let add_h = if self.show_add {
            self.m.add_h + self.m.add_margin
        } else {
            0.0
        };
        let top = rect.y0 + header_h;
        // The add button keeps a margin above it too, so the last thumbnail
        // never runs under it.
        let bottom = (rect.y1
            - add_h
            - if self.show_add {
                self.m.add_margin
            } else {
                0.0
            })
        .max(top);
        self.list_vp = Rect::new(rect.x0, top, rect.x1, bottom);

        let usable_w = self.list_vp.width() - 2.0 * self.m.pad - self.m.num_gutter - SCROLLBAR_W;
        self.thumb_w = usable_w.max(1.0);
        self.thumb_h = self.thumb_w * self.aspect;
        self.list_top = self.list_vp.y0 + self.m.pad;

        let content_h = if count == 0 {
            0.0
        } else {
            count_f64(count) * self.thumb_h + (count_f64(count) - 1.0) * self.m.gap
        };
        let view_h = (self.list_vp.height() - 2.0 * self.m.pad).max(0.0);
        self.max_scroll = (content_h - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);

        self.add_rect = if self.show_add {
            Rect::new(
                rect.x0 + self.m.pad,
                rect.y1 - self.m.add_margin - self.m.add_h,
                rect.x1 - self.m.pad,
                rect.y1 - self.m.add_margin,
            )
        } else {
            Rect::ZERO
        };

        // Keep the drag gap pinned to the ghost as the list scrolls.
        self.drag.sync_insertion(&self.list_metrics());
    }

    fn list_metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.list_top - self.scroll,
            self.thumb_h,
            self.m.gap,
            self.count,
        )
    }

    /// The un-shifted screen rect of thumbnail `i` this frame.
    fn thumb_rect(&self, i: usize) -> Rect {
        let x0 = self.list_vp.x0 + self.m.pad + self.m.num_gutter;
        let top = self.list_top - self.scroll + count_f64(i) * (self.thumb_h + self.m.gap);
        Rect::new(x0, top, x0 + self.thumb_w, top + self.thumb_h)
    }

    /// Every thumbnail slot as `(index, rect)` — exposed for the host (and
    /// tests) so it can, e.g., pre-rasterise only visible pages.
    pub fn thumb_rects(&self) -> Vec<(usize, Rect)> {
        (0..self.count).map(|i| (i, self.thumb_rect(i))).collect()
    }

    pub fn scroll_to(&mut self, i: usize) {
        if self.count == 0 {
            return;
        }
        let top = count_f64(i) * (self.thumb_h + self.m.gap);
        let bottom = top + self.thumb_h;
        let view_h = (self.list_vp.height() - 2.0 * self.m.pad).max(0.0);
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + view_h {
            self.scroll = (bottom - view_h).max(0.0);
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
    }

    fn thumb_at(&self, p: Point) -> Option<usize> {
        if !self.list_vp.contains(p) {
            return None;
        }
        (0..self.count).find(|&i| self.thumb_rect(i).contains(p))
    }

    // ---- pointer input ----------------------------------------------

    /// A left-button press at logical point `p`. Returns true when the rail
    /// consumed it (so the host stops routing). Opens/uses the context menu,
    /// the add button, or begins a select + potential reorder drag.
    pub fn on_press(&mut self, p: Point) -> bool {
        self.last_pointer = p;

        // A menu is modal while open: a click either picks a row or dismisses.
        if let Some(menu) = self.menu.take() {
            for (act, row) in &menu.items {
                if row.contains(p) {
                    self.apply_menu(*act, menu.index);
                    return true;
                }
            }
            // click off the menu — swallow the dismiss.
            return true;
        }

        if self.show_add && self.add_rect.contains(p) {
            self.pending.push(PageSidebarEvent::Add);
            return true;
        }

        if let Some(i) = self.thumb_at(p) {
            self.press_idx = Some(i);
            self.pending.push(PageSidebarEvent::Select(i));
            let grab = p.y - self.list_metrics().slot_lead(i);
            self.drag.begin(i, i, grab, p);
            return true;
        }

        // A click elsewhere in the rail column is still consumed by the panel.
        self.list_vp.contains(p) || (self.draw_panel && self.rect.contains(p))
    }

    /// A right-button press: select the page under the cursor and open the
    /// duplicate / move / delete menu on it. Returns true when consumed.
    pub fn on_right_press(&mut self, p: Point) -> bool {
        self.last_pointer = p;
        self.menu = None;
        let Some(i) = self.thumb_at(p) else {
            return self.list_vp.contains(p);
        };
        self.pending.push(PageSidebarEvent::Select(i));
        self.open_menu(i, p);
        true
    }

    /// Pointer motion (logical). Feeds the reorder drag and menu hover.
    /// Returns true when the visible state changed.
    pub fn on_motion(&mut self, p: Point) -> bool {
        self.last_pointer = p;
        let mut changed = false;
        if let Some(menu) = self.menu.as_mut() {
            let hover = menu.items.iter().position(|(_, r)| r.contains(p));
            if hover != menu.hover {
                menu.hover = hover;
                changed = true;
            }
        }
        if self.drag.is_pending() {
            changed |= self.drag.pointer_moved(p, &self.list_metrics());
        }
        changed
    }

    /// Button release. Commits a reorder drag if one moved. Returns true when
    /// the visible state changed (so the host redraws).
    pub fn on_release(&mut self) -> bool {
        self.press_idx = None;
        let was_active = self.drag.is_active();
        if let Some((from, to)) = self.drag.commit() {
            self.pending.push(PageSidebarEvent::Reorder { from, to });
            return true;
        }
        was_active
    }

    /// A wheel notch of `dy` logical px (positive scrolls the list down).
    pub fn on_wheel(&mut self, dy: f64) -> bool {
        if self.max_scroll <= 0.0 {
            return false;
        }
        let before = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll);
        (self.scroll - before).abs() > f64::EPSILON
    }

    /// Escape: cancel a live drag or close the menu. Returns true if either
    /// was in progress.
    pub fn on_escape(&mut self) -> bool {
        let mut changed = self.drag.cancel();
        if self.menu.take().is_some() {
            changed = true;
        }
        changed
    }

    fn open_menu(&mut self, index: usize, at: Point) {
        let mut actions = vec![MenuAction::Duplicate];
        if index > 0 {
            actions.push(MenuAction::MoveUp);
        }
        if index.saturating_add(1) < self.count {
            actions.push(MenuAction::MoveDown);
        }
        actions.push(MenuAction::Delete);

        // Height: one separator sits before Delete.
        let h = count_f64(actions.len()) * self.m.menu_row_h
            + self.m.menu_sep_h
            + 2.0 * self.m.menu_pad;
        let x0 =
            at.x.min(self.rect.x1 - self.m.menu_w - self.m.menu_row_inset)
                .max(self.rect.x0 + self.m.menu_row_inset);
        let y0 =
            at.y.min(self.rect.y1 - h - self.m.menu_row_inset)
                .max(self.rect.y0 + self.m.menu_row_inset);
        let rect = Rect::new(x0, y0, x0 + self.m.menu_w, y0 + h);

        let mut items = Vec::new();
        let mut y = y0 + self.m.menu_pad;
        for act in actions {
            if act == MenuAction::Delete {
                y += self.m.menu_sep_h;
            }
            let row = Rect::new(
                x0 + self.m.menu_row_inset,
                y,
                x0 + self.m.menu_w - self.m.menu_row_inset,
                y + self.m.menu_row_h,
            );
            items.push((act, row));
            y += self.m.menu_row_h;
        }
        self.menu = Some(Menu {
            index,
            rect,
            items,
            hover: None,
        });
    }

    fn apply_menu(&mut self, act: MenuAction, index: usize) {
        match act {
            MenuAction::Duplicate => self.pending.push(PageSidebarEvent::Duplicate(index)),
            MenuAction::MoveUp if index > 0 => self.pending.push(PageSidebarEvent::Reorder {
                from: index,
                to: index.saturating_sub(1),
            }),
            MenuAction::MoveDown if index.saturating_add(1) < self.count => {
                self.pending.push(PageSidebarEvent::Reorder {
                    from: index,
                    to: index.saturating_add(1),
                });
            }
            MenuAction::Delete => self.pending.push(PageSidebarEvent::Delete(index)),
            _ => {}
        }
    }

    // ---- drawing -----------------------------------------------------

    /// Paint the rail. `thumb` draws the host's page/slide content into the
    /// given slot rect (the widget frames it, rings it, numbers it, and
    /// handles the reflow gap + drag ghost around it).
    pub fn render<F>(
        &mut self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        mut thumb: F,
    ) where
        F: FnMut(&mut Scene, &mut TextRenderer, Affine, usize, Rect),
    {
        // A rail that paints its own panel is the window's sidebar region:
        // the framework's tier, and everything on it drawn for that ground.
        let region_theme;
        let theme = if self.draw_panel {
            crate::surface::region(
                scene,
                base,
                self.rect,
                crate::surface::Region::Sidebar,
                theme,
            );
            region_theme = theme.in_region(crate::surface::Region::Sidebar);
            &region_theme
        } else {
            theme
        };
        let c = &theme.tokens.colors;

        if self.draw_panel {
            // The header is a section label, sentence case, centred on its
            // own measured line in the header band.
            let style = label_style(theme);
            let (_, lh) = tr.measure_style(
                &self.header,
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
            let hy = self.rect.y0 + (self.m.header_h - f64::from(lh)) / 2.0;
            tr.draw_style(
                scene,
                &self.header,
                c.text_dim,
                translated(base, self.rect.x0 + self.m.pad, hy),
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
        }

        let metrics = self.list_metrics();
        let drag_from = self.drag.from_index();

        // Thumbnail list, clipped to the viewport so it never bleeds over the
        // header or the add button.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            base,
            &self.list_vp,
        );
        for i in 0..self.count {
            if Some(i) == drag_from {
                continue; // lifted out — the floating ghost is drawn below.
            }
            let shift = self.drag.item_shift(i, &metrics);
            let rect = shifted(self.thumb_rect(i), shift);
            // Skip rows fully outside the viewport (cheap virtualisation).
            if rect.y1 < self.list_vp.y0 - 4.0 || rect.y0 > self.list_vp.y1 + 4.0 {
                continue;
            }
            self.draw_slot(scene, base, tr, theme, i, rect, &mut thumb);
        }
        scene.pop_layer();

        if let (Some(from), Some(ghost)) = (
            drag_from,
            self.drag.ghost_rect_sized(
                self.list_vp.x0 + self.m.pad + self.m.num_gutter,
                vello::kurbo::Size::new(self.thumb_w, self.thumb_h),
            ),
        ) {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                base,
                &self.list_vp,
            );
            // A dragged page is genuinely lifted, so it casts the ramp's
            // dragged-object shadow, shaped to its own corners (HIG §9.3),
            // rather than a black wash nudged down four pixels.
            let radius = f64::from(theme.tokens.radii.control);
            crate::surface::shadow(scene, base, ghost, radius, &theme.tokens.elevation.floating);
            thumb(scene, tr, base, from, ghost);
            // The page you are moving carries your colour, as every drop
            // and insertion indicator does.
            scene.stroke(
                &Stroke::new(2.0),
                base,
                theme.tokens.colors.intent.you,
                None,
                &RoundedRect::from_rect(ghost, radius),
            );
            scene.pop_layer();
        }

        self.draw_scrollbar(scene, base, theme);

        if self.show_add {
            self.draw_add_button(scene, base, tr, theme);
        }

        if self.menu.is_some() {
            self.draw_menu(scene, base, tr, theme);
        }
    }

    /// The rail's own scrollbar (track + thumb), a no-op when the list fits
    /// (`max_scroll <= 0.0`).
    fn draw_scrollbar(&self, scene: &mut Scene, base: Affine, theme: &Theme) {
        if self.max_scroll <= 0.0 {
            return;
        }
        let view_h = (self.list_vp.height() - 2.0 * self.m.pad).max(1.0);
        let content_h = view_h + self.max_scroll;
        let track = Rect::new(
            self.list_vp.x1 - SCROLLBAR_W - SCROLLBAR_INSET,
            self.list_top,
            self.list_vp.x1 - SCROLLBAR_INSET,
            self.list_top + view_h,
        );
        let frac = (view_h / content_h).clamp(0.08, 1.0);
        let thumb_h = (view_h * frac).max(f64::from(theme.tokens.spacing.s6));
        let travel = view_h - thumb_h;
        let t = (self.scroll / self.max_scroll).clamp(0.0, 1.0);
        let ty = track.y0 + travel * t;
        // Capsules: the radius is half the bar's width, not a literal.
        let radius = SCROLLBAR_W / 2.0;
        // The shared scrollbar tiers, so every rail's bar matches Scrollable's.
        scene.fill(
            Fill::NonZero,
            base,
            crate::widget::scrollbar::track_color(theme),
            None,
            &RoundedRect::from_rect(track, radius),
        );
        scene.fill(
            Fill::NonZero,
            base,
            crate::widget::scrollbar::thumb_color(theme, false),
            None,
            &RoundedRect::from_rect(Rect::new(track.x0, ty, track.x1, ty + thumb_h), radius),
        );
    }

    fn draw_slot<F>(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        i: usize,
        rect: Rect,
        thumb: &mut F,
    ) where
        F: FnMut(&mut Scene, &mut TextRenderer, Affine, usize, Rect),
    {
        let c = &theme.tokens.colors;
        let is_current = i == self.current;
        let radius = f64::from(theme.tokens.radii.control);

        // The current page is the selection lift (HIG §9.3): one plate
        // behind its number and thumbnail, never a coloured ring.
        if is_current {
            let inset = f64::from(theme.tokens.spacing.s1);
            let plate = Rect::new(
                self.list_vp.x0 + inset,
                rect.y0 - inset,
                rect.x1 + inset,
                rect.y1 + inset,
            );
            crate::chrome::paint_row_selection(
                scene,
                base,
                plate,
                crate::chrome::SelectionState::active(true),
                c.surfaces.content,
                theme,
            );
        }

        thumb(scene, tr, base, i, rect);

        // Every page is seated on the rail by a hairline derived from the
        // ground, following the thumbnail's corners.
        scene.stroke(
            &Stroke::new(1.0),
            base,
            crate::tokens::colors::Colors::hairline_on(c.surfaces.content, PAGE_EDGE_ALPHA),
            None,
            &crate::surface::edge_outline(rect, radius),
        );
        // A marked page (annotated, commented) carries a dot inside its own
        // corner, in your colour.
        if self.marked.contains(&i) {
            let d = f64::from(theme.tokens.spacing.s2);
            let inset = f64::from(theme.tokens.spacing.s1) + d / 2.0;
            let centre = Point::new(rect.x1 - inset, rect.y0 + inset);
            scene.fill(
                Fill::NonZero,
                base,
                c.surfaces.content,
                None,
                &vello::kurbo::Circle::new(centre, d / 2.0 + 1.0),
            );
            scene.fill(
                Fill::NonZero,
                base,
                c.intent.you,
                None,
                &vello::kurbo::Circle::new(centre, d / 2.0),
            );
        }

        // Right-aligned in the left gutter, one `s2` short of the
        // thumbnail, vertically centred on the measured line.
        let label = format!("{}", i.saturating_add(1));
        let style = label_style(theme);
        let weight = if is_current {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let (lw, lh) = tr.measure_style(&label, None, &style.clone().with_weight(weight));
        let lx = rect.x0 - f64::from(theme.tokens.spacing.s2) - f64::from(lw);
        let ly = rect.y0 + (rect.height() - f64::from(lh)) / 2.0;
        let col = if is_current {
            c.text_primary
        } else {
            c.text_dim
        };
        tr.draw_style(
            scene,
            &label,
            col,
            translated(base, lx.max(self.list_vp.x0 + SCROLLBAR_INSET), ly),
            None,
            &style.clone().with_weight(weight),
        );
    }

    fn draw_add_button(
        &self,
        scene: &mut Scene,
        base: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let r = self.add_rect;
        let hovered = r.contains(self.last_pointer);
        // A Neutral control on the rail: its own surface with a derived
        // edge (HIG §9.5); hover is the shared hover face, not an accent wash.
        let bg = if hovered {
            c.lift_on(c.surfaces.raised, 0.5)
        } else {
            c.surfaces.raised
        };
        crate::surface::card(scene, base, r, f64::from(theme.tokens.radii.control), bg);
        let style = label_style(theme).with_weight(TextWeight::Semibold);
        let (tw, th) = tr.measure_style(&self.add_label, None, &style);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let cluster = icon + gap + f64::from(tw);
        let x0 =
            (r.x0 + (r.width() - cluster) / 2.0).max(r.x0 + f64::from(theme.tokens.spacing.s2));
        crate::icons::Icon::Plus.draw(
            scene,
            base,
            Rect::from_center_size(Point::new(x0 + icon / 2.0, r.center().y), (icon, icon)),
            c.text_primary,
        );
        let ty = r.y0 + (r.height() - f64::from(th)) / 2.0;
        tr.draw_style(
            scene,
            &self.add_label,
            c.text_primary,
            translated(base, x0 + icon + gap, ty),
            None,
            &style,
        );
    }

    fn draw_menu(&self, scene: &mut Scene, base: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let c = &theme.tokens.colors;
        // A popover over content: `radii.control`, the floating-panel shadow
        // shaped to its corners, hairline edge; rows are `radii.detail` details
        // inside it, labels on the standard padding.
        let radius = f64::from(theme.tokens.radii.control);
        let rr = RoundedRect::from_rect(menu.rect, radius);
        crate::surface::shadow(
            scene,
            base,
            menu.rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        // Panel role (HIG §9.1/§9.5): the panel ground with its derived
        // border; the hovered row is the hover lift, never an accent wash.
        scene.fill(
            Fill::NonZero,
            base,
            crate::chrome::panel_ground(theme),
            None,
            &rr,
        );
        scene.stroke(
            &Stroke::new(1.0),
            base,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(menu.rect, radius),
        );
        let style = label_style(theme);
        for (idx, (act, row)) in menu.items.iter().enumerate() {
            if menu.hover == Some(idx) {
                let tier = crate::chrome::selection_tier(
                    theme,
                    crate::chrome::SelectionState::new(false, true, false),
                );
                scene.fill(
                    Fill::NonZero,
                    base,
                    tier.fill,
                    None,
                    &RoundedRect::from_rect(*row, f64::from(theme.tokens.radii.detail)),
                );
            }
            let (label, col) = match act {
                MenuAction::Duplicate => ("Duplicate", c.text_primary),
                MenuAction::MoveUp => ("Move up", c.text_primary),
                MenuAction::MoveDown => ("Move down", c.text_primary),
                MenuAction::Delete => ("Delete", c.semantic.danger),
            };
            let (_, th) = tr.measure_style(label, None, &style);
            let ty = row.y0 + (row.height() - f64::from(th)) / 2.0;
            tr.draw_style(
                scene,
                label,
                col,
                translated(base, row.x0 + f64::from(theme.padding_standard), ty),
                None,
                &style,
            );
        }
    }
}

impl Widget for PageSidebar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "PageSidebar"
    }
    fn test_id(&self) -> Option<&str> {
        Some("page_sidebar")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rail(count: usize) -> PageSidebar {
        let mut s = PageSidebar::new(792.0 / 612.0).with_add("Import…");
        s.begin_frame(Rect::new(0.0, 0.0, 200.0, 800.0), count, 0);
        s
    }

    #[test]
    fn page_list_operations_reorder_duplicate_and_measure() {
        // Case `move_item_matches_remove_insert`.
        {
            let mut v = vec![0, 1, 2, 3];
            move_item(&mut v, 0, 3);
            assert_eq!(v, vec![1, 2, 3, 0]);
            let mut v = vec![0, 1, 2, 3];
            move_item(&mut v, 3, 0);
            assert_eq!(v, vec![3, 0, 1, 2]);
            // out of range / equal are no-ops.
            let mut v = vec![0, 1];
            move_item(&mut v, 0, 0);
            assert_eq!(v, vec![0, 1]);
            move_item(&mut v, 5, 0);
            assert_eq!(v, vec![0, 1]);
        }

        // Case `duplicate_after_inserts_clone`.
        {
            let mut v = vec!['a', 'b', 'c'];
            duplicate_after(&mut v, 1);
            assert_eq!(v, vec!['a', 'b', 'b', 'c']);
            duplicate_after(&mut v, 99);
            assert_eq!(v, vec!['a', 'b', 'b', 'c']);
        }

        // Case `themed_metrics_read_the_spacing_scale_the_constants_restate`.
        // The themed metrics are the restated scale read from the tokens: the
        // same numbers at the default density, except the add button, which
        // becomes a bar button, and the menu width, which has no token. A
        // themed frame lays the rail out with them; a plain frame keeps the
        // defaults, so the existing hosts are untouched.
        {
            let theme = Theme::dark();
            let themed = RailMetrics::from_theme(&theme);
            let d = RailMetrics::DEFAULT;
            assert_eq!(
                (themed.pad, themed.gap, themed.num_gutter, themed.header_h),
                (d.pad, d.gap, d.num_gutter, d.header_h)
            );
            assert_eq!(
                (
                    themed.add_margin,
                    themed.menu_row_h,
                    themed.menu_sep_h,
                    themed.menu_pad,
                    themed.menu_row_inset
                ),
                (
                    d.add_margin,
                    d.menu_row_h,
                    d.menu_sep_h,
                    d.menu_pad,
                    d.menu_row_inset
                )
            );
            assert_eq!(themed.menu_w, d.menu_w);
            assert_eq!(themed.add_h, f64::from(theme.tokens.metrics.bar_height));

            let rect = Rect::new(0.0, 0.0, 200.0, 800.0);
            let mut s = PageSidebar::new(1.0).with_add("Import…");
            s.begin_frame_with_theme(&theme, rect, 3, 0);
            assert_eq!(s.add_rect.height(), themed.add_h);
            assert_eq!(s.thumb_rects()[0].1.x0, themed.pad + themed.num_gutter);
            let mut plain = PageSidebar::new(1.0).with_add("Import…");
            plain.begin_frame(rect, 3, 0);
            assert_eq!(plain.add_rect.height(), d.add_h);
            assert_eq!(plain.thumb_rects()[0].1.x0, d.pad + d.num_gutter);
        }

        // Case `thumb_rects_are_ordered_and_sized`.
        {
            let s = rail(4);
            let rects = s.thumb_rects();
            assert_eq!(rects.len(), 4);
            for (_, r) in &rects {
                assert!(
                    r.height() > r.width(),
                    "portrait pages are taller than wide"
                );
            }
            assert!(rects[1].1.y0 > rects[0].1.y1, "slot 1 sits below slot 0");
        }
    }

    #[test]
    fn thumbnail_pointer_selects_drags_and_scrolls() {
        // Case `press_on_thumbnail_selects_it`.
        {
            let mut s = rail(4);
            let (_, r2) = s.thumb_rects()[2];
            assert!(s.on_press(r2.center()));
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Select(2)]);
        }

        // Case `drag_reorder_emits_reorder_on_release`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            s.on_press(rects[0].1.center());
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Select(0)]);
            let far_down = Point::new(rects[3].1.center().x, rects[3].1.center().y + 400.0);
            s.on_motion(far_down);
            assert!(s.is_dragging(), "cleared the drag threshold");
            assert!(s.on_release());
            assert_eq!(
                s.take_events(),
                vec![PageSidebarEvent::Reorder { from: 0, to: 3 }]
            );
        }

        // Case `tiny_move_stays_a_click_not_a_reorder`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            s.on_press(rects[1].1.center());
            s.take_events(); // clear the Select
            s.on_motion(rects[1].1.center() + Vec2::new(1.0, 1.0));
            assert!(!s.is_dragging());
            assert!(!s.on_release());
            assert!(s.take_events().is_empty(), "a click produces no reorder");
        }

        // Case `escape_cancels_a_live_drag`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            s.on_press(rects[0].1.center());
            s.on_motion(rects[3].1.center());
            assert!(s.is_dragging());
            assert!(s.on_escape());
            assert!(!s.is_dragging());
            assert!(!s.on_release());
            // No reorder survives a cancel.
            assert!(
                !s.take_events()
                    .iter()
                    .any(|e| matches!(e, PageSidebarEvent::Reorder { .. }))
            );
        }

        // Case `wheel_scrolls_and_clamps`.
        {
            // Many pages so the content overflows the viewport.
            let mut s = rail(40);
            assert!(s.max_scroll > 0.0);
            assert!(s.on_wheel(120.0));
            assert!(s.scroll() > 0.0);
            s.set_scroll(0.0);
            assert!(!s.on_wheel(-50.0), "already at the top");
            assert_eq!(s.scroll(), 0.0);
        }

        // Case `add_button_emits_add`.
        {
            let mut s = rail(4);
            assert!(s.on_press(s.add_rect.center()));
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Add]);
        }
    }

    #[test]
    fn page_menu_duplicates_deletes_and_moves_within_bounds() {
        // Case `right_click_menu_duplicate_and_delete`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            assert!(s.on_right_press(rects[1].1.center()));
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Select(1)]);
            assert!(s.menu_open());
            let dup_row = s.menu.as_ref().unwrap().items[0].1.center();
            assert!(s.on_press(dup_row));
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Duplicate(1)]);
            assert!(!s.menu_open(), "picking a row closes the menu");

            s.on_right_press(rects[2].1.center());
            s.take_events();
            let menu = s.menu.as_ref().unwrap();
            let del = menu
                .items
                .iter()
                .find(|(a, _)| *a == MenuAction::Delete)
                .unwrap()
                .1
                .center();
            assert!(s.on_press(del));
            assert_eq!(s.take_events(), vec![PageSidebarEvent::Delete(2)]);
        }

        // Case `menu_move_up_down_map_to_reorder`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            s.on_right_press(rects[2].1.center());
            s.take_events();
            let up = s
                .menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .find(|(a, _)| *a == MenuAction::MoveUp)
                .unwrap()
                .1
                .center();
            s.on_press(up);
            assert_eq!(
                s.take_events(),
                vec![PageSidebarEvent::Reorder { from: 2, to: 1 }]
            );
        }

        // Case `first_page_menu_has_no_move_up`.
        {
            let mut s = rail(4);
            let rects = s.thumb_rects();
            s.on_right_press(rects[0].1.center());
            let has_up = s
                .menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|(a, _)| *a == MenuAction::MoveUp);
            assert!(!has_up, "the first page cannot move up");
        }
    }
}
