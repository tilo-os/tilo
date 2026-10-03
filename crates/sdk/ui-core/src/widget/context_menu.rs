//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators. Standalone owners draw it in their
//! overlay pass. `open_anchored` opts into measured, viewport-bounded geometry;
//! legacy `open_at` callers retain their point placement and fixed rows.
//!
//! Callers push `MenuItem::action(label, fn)` and optional
//! `MenuItem::separator()` entries, then call `open_at` on right-click.

use crate::widget::{AppModel, ModelPath};
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, draw_icon};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub type MenuAction = Arc<dyn Fn() + Send + Sync>;

/// Separators are purely visual and ignored by [`ContextMenu::click_item`].
#[derive(Clone)]
pub enum MenuItem {
    Action {
        label: String,
        action: MenuAction,
        /// Optional leading icon for command recognition. Icons are visual
        /// reinforcement only; `label` remains the accessible name.
        icon: Option<Icon>,
        /// `Some` reserves the leading selection gutter and publishes a
        /// semantic checked state. `None` is an ordinary command row.
        checked: Option<bool>,
        /// Destructive rows are styled as danger actions and are never the
        /// implicit keyboard target when a menu opens.
        destructive: bool,
        /// Disabled rows remain visible and announced but cannot be invoked.
        enabled: bool,
    },
    Separator,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MenuItem::Action { label, .. } => f
                .debug_struct("MenuItem::Action")
                .field("label", label)
                .field("action", &"<fn>")
                .finish(),
            MenuItem::Separator => f.write_str("MenuItem::Separator"),
        }
    }
}

impl MenuItem {
    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action {
            label: label.into(),
            action: Arc::new(action),
            icon: None,
            checked: None,
            destructive: false,
            enabled: true,
        }
    }

    /// A command row with a themed leading icon. Menus containing icon rows
    /// reserve the same gutter for every command so labels stay aligned.
    pub fn icon_action<F>(icon: Icon, label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action {
            label: label.into(),
            action: Arc::new(action),
            icon: Some(icon),
            checked: None,
            destructive: false,
            enabled: true,
        }
    }

    pub fn selection(label: impl Into<String>) -> Self {
        Self::action(label, || {})
    }

    pub fn destructive_action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action {
            label: label.into(),
            action: Arc::new(action),
            icon: None,
            checked: None,
            destructive: true,
            enabled: true,
        }
    }

    pub fn destructive_selection(label: impl Into<String>) -> Self {
        Self::destructive_action(label, || {})
    }

    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        if let MenuItem::Action {
            icon: item_icon, ..
        } = &mut self
        {
            *item_icon = Some(icon);
        }
        self
    }

    /// Set whether this command can currently be invoked.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        if let MenuItem::Action {
            enabled: item_enabled,
            ..
        } = &mut self
        {
            *item_enabled = enabled;
        }
        self
    }

    /// A persistent choice/toggle row. Checked and unchecked peers share the
    /// same leading gutter, so labels never jump as state changes.
    pub fn checked_action<F>(label: impl Into<String>, checked: bool, action: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        MenuItem::Action {
            label: label.into(),
            action: Arc::new(action),
            icon: None,
            checked: Some(checked),
            destructive: false,
            enabled: true,
        }
    }

    pub fn separator() -> Self {
        MenuItem::Separator
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            MenuItem::Action { label, .. } => Some(label.as_str()),
            MenuItem::Separator => None,
        }
    }

    /// Semantic checked state for choice/toggle rows. `None` means this is a
    /// regular command, rather than an unchecked choice.
    pub fn checked(&self) -> Option<bool> {
        match self {
            MenuItem::Action { checked, .. } => *checked,
            MenuItem::Separator => None,
        }
    }

    fn is_destructive(&self) -> bool {
        matches!(
            self,
            MenuItem::Action {
                destructive: true,
                ..
            }
        )
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, MenuItem::Action { enabled: true, .. })
    }
}

pub struct ContextMenu {
    items: Vec<MenuItem>,
    open: bool,
    position: Option<[f32; 2]>,
    rect: Rect,
    /// `None` when the cursor is off the menu or on a separator.
    hovered: Option<usize>,
    /// `MouseInput` carries no position, so this is tracked from
    /// `CursorMoved` to map a press to a row.
    last_cursor: Point,
    /// Set by `handle_event`, drained by [`ContextMenu::take_selection`].
    /// The `&mut self` + CRDT-friendly action channel: instead of forcing
    /// every menu action through an `Arc<dyn Fn()>` (which can't splice
    /// into an Automerge doc the host owns), the host polls
    /// `take_selection()` after dispatching the event and mutates itself.
    /// The legacy `Arc` callback still fires too.
    selection: Option<usize>,
    bounds: Option<(Rect, Rect)>,
    row_height: f64,
    measured_width: f64,
    scroll_offset: f64,
    active_touch: Option<u64>,
    text_scale: f64,
    viewport: Option<Rect>,
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            open: false,
            position: None,
            rect: Rect::ZERO,
            hovered: None,
            last_cursor: Point::ZERO,
            selection: None,
            bounds: None,
            row_height: ROW_HEIGHT,
            measured_width: MENU_WIDTH,
            scroll_offset: 0.0,
            active_touch: None,
            text_scale: 1.0,
            viewport: None,
        }
    }

    pub fn push(&mut self, item: MenuItem) {
        self.items.push(item);
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    pub fn set_item_enabled(&mut self, index: usize, enabled: bool) {
        let Some(MenuItem::Action {
            enabled: item_enabled,
            ..
        }) = self.items.get_mut(index)
        else {
            return;
        };
        *item_enabled = enabled;
        if !enabled && self.hovered == Some(index) {
            self.hovered = None;
            self.move_hover(1);
        }
    }

    /// Also computes `rect` immediately (via
    /// [`layout_at_position`](Self::layout_at_position)) so
    /// [`item_at`](Self::item_at) / [`contains`](Self::contains) are usable
    /// without first running a `draw`/`apply_layout` pass — needed by
    /// consumers (like `TextInput`) that route a click to a row in the same
    /// input batch the menu opened in.
    pub fn open_at(&mut self, position: [f32; 2]) {
        self.bounds = None;
        self.scroll_offset = 0.0;
        self.open = true;
        self.position = Some(position);
        self.measured_width = MENU_WIDTH * self.text_scale;
        self.row_height = ROW_HEIGHT * self.text_scale;
        if let Some(viewport) = self.viewport {
            self.constrain_to(viewport);
        }
        self.hovered = self
            .items
            .iter()
            .find(|item| item.is_enabled())
            .filter(|item| item.is_destructive())
            .and_then(|_| {
                self.items
                    .iter()
                    .position(|item| item.is_enabled() && !item.is_destructive())
            });
        self.layout_at_position();
    }

    pub fn open_at_clamped(&mut self, position: [f32; 2], viewport: Rect) {
        self.set_viewport(Some(viewport));
        self.open_at(position);
    }

    /// Update menu geometry for the system text scale.
    ///
    /// This is intentionally public for overlay-owning widgets: a menu can be
    /// opened and clicked between layout passes, so merely reading the theme in
    /// [`Widget::draw`] would leave its hit targets at the previous scale.
    pub fn set_text_scale(&mut self, text_scale: f32) {
        let scale = f64::from(text_scale.clamp(0.5, 4.0));
        if (self.text_scale - scale).abs() > f64::EPSILON {
            self.text_scale = scale;
            self.measured_width = MENU_WIDTH * scale;
            self.row_height = ROW_HEIGHT * scale;
            self.layout_at_position();
        }
    }

    /// Keep the menu inside an owner-provided viewport. It opens on the side
    /// of its anchor with more room, then wheel and keyboard navigation scroll
    /// rows internally when neither side can fit them. The accessibility tree
    /// still exposes every complete label.
    pub fn set_viewport(&mut self, viewport: Option<Rect>) {
        self.viewport = viewport.filter(|rect| rect.width() > 0.0 && rect.height() > 0.0);
        if let Some(viewport) = self.viewport {
            self.constrain_to(viewport);
        }
        self.layout_at_position();
    }

    /// Open a shared menu inside a host viewport. Geometry is immediately usable;
    /// call `layout_within` with the renderer before painting to settle text metrics.
    pub fn open_anchored(&mut self, anchor: Rect, viewport: Rect) {
        self.open = true;
        self.hovered = None;
        self.scroll_offset = 0.0;
        self.reanchor(anchor, viewport);
    }

    pub fn reanchor(&mut self, anchor: Rect, viewport: Rect) {
        self.bounds = Some((anchor, viewport));
        self.layout_at_position();
    }

    /// Keep a standalone menu inside its owner's current viewport, preserving
    /// its opening anchor across resize and text-metric changes.
    pub fn constrain_to(&mut self, viewport: Rect) {
        let anchor = self.bounds.map(|(anchor, _)| anchor).or_else(|| {
            self.position
                .map(|[x, y]| Rect::new(f64::from(x), f64::from(y), f64::from(x), f64::from(y)))
        });
        if let Some(anchor) = anchor {
            self.reanchor(anchor, viewport);
        }
    }

    fn has_leading_gutter(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                MenuItem::Action {
                    checked: Some(_),
                    ..
                } | MenuItem::Action { icon: Some(_), .. }
            )
        })
    }

    pub fn layout_within(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.set_text_scale(theme.text_scale);
        let style = text.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
        let mut width: f64 = 0.0;
        let mut height: f64 = 0.0;
        let has_leading_gutter = self.has_leading_gutter();
        for item in &self.items {
            if let Some(label) = item.label() {
                let (w, h) = text.measure_style(label, None, &style);
                let check_width = if has_leading_gutter {
                    f64::from(theme.tokens.typography.scale.sm + theme.tokens.spacing.s2)
                } else {
                    0.0
                };
                width = width.max(f64::from(w) + check_width);
                height = height.max(f64::from(h));
            }
        }
        // The label's inset on both sides, and `s3` of air around the line.
        self.measured_width = width + 2.0 * label_inset(theme);
        self.row_height =
            (height + f64::from(theme.tokens.spacing.s3)).max(ROW_HEIGHT * self.text_scale);
        self.layout_at_position();
        self.ensure_hover_visible();
    }

    /// Height of one row. Action rows take the measured height once a paint
    /// has measured the labels (`ROW_HEIGHT` until then, and never less), in
    /// both the anchored and the point-placed mode — a point-placed menu at
    /// a large text setting otherwise paints two-line-tall glyphs into
    /// 28 px rows.
    fn item_height(&self, item: &MenuItem) -> f64 {
        match item {
            MenuItem::Action { .. } => self.row_height,
            MenuItem::Separator => SEPARATOR_HEIGHT,
        }
    }

    /// Width of a point-placed (`open_at`) menu: the fixed reference width,
    /// grown to fit the measured labels so nothing is clipped at a large
    /// text setting. Anchored menus hug their labels instead.
    fn legacy_width(&self) -> f64 {
        self.measured_width.max(MENU_WIDTH)
    }

    fn content_height(&self) -> f64 {
        self.items.iter().map(|item| self.item_height(item)).sum()
    }

    fn body_rect(&self) -> Rect {
        let inset = if self.bounds.is_some() {
            PADDING.min(self.rect.height() / 4.0)
        } else {
            PADDING
        };
        Rect::new(
            self.rect.x0,
            self.rect.y0 + inset,
            self.rect.x1,
            self.rect.y1 - inset,
        )
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.body_rect().height()).max(0.0)
    }

    /// Painted (clipped) geometry of an action row, in host coordinates.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        if !self.open || !matches!(self.items.get(index), Some(MenuItem::Action { .. })) {
            return None;
        }
        let body = self.body_rect();
        let top = body.y0 - self.scroll_offset
            + self
                .items
                .iter()
                .take(index)
                .map(|item| self.item_height(item))
                .sum::<f64>();
        let rect = Rect::new(
            body.x0,
            top,
            body.x1,
            top + self.item_height(self.items.get(index)?),
        )
        .intersect(body);
        (!rect.is_zero_area()).then_some(rect)
    }

    pub fn focus_item(&mut self, index: usize) -> bool {
        if !matches!(self.items.get(index), Some(MenuItem::Action { .. })) {
            return false;
        }
        self.hovered = Some(index);
        self.ensure_hover_visible();
        true
    }

    pub fn focus_first_action(&mut self) {
        self.hovered = None;
        self.move_hover(1);
    }

    fn ensure_hover_visible(&mut self) {
        if self.bounds.is_none() {
            return;
        }
        if let Some(index) = self.hovered {
            let top = self
                .items
                .iter()
                .take(index)
                .map(|item| self.item_height(item))
                .sum::<f64>();
            let height = self
                .items
                .get(index)
                .map_or(0.0, |item| self.item_height(item));
            let body_height = self.body_rect().height();
            if top < self.scroll_offset {
                self.scroll_offset = top;
            } else if top + height > self.scroll_offset + body_height {
                self.scroll_offset = top + height.min(body_height) - body_height;
            }
        }
        self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll());
    }

    /// Mirrors the `build_layout` + `apply_layout` geometry without a taffy
    /// round-trip. A no-op when the menu has never been positioned. Lets a
    /// standalone consumer (one that owns a `ContextMenu` as a field rather
    /// than a tree node) hit-test and draw the menu directly.
    pub fn layout_at_position(&mut self) {
        if let Some((anchor, viewport)) = self.bounds {
            let width = self.measured_width.min(viewport.width().max(0.0));
            let height = (self.content_height() + PADDING * 2.0).min(viewport.height().max(0.0));
            let x = anchor
                .x0
                .clamp(viewport.x0, (viewport.x1 - width).max(viewport.x0));
            let preferred_y = if anchor.y1 + height <= viewport.y1 {
                anchor.y1
            } else {
                anchor.y0 - height
            };
            let y = preferred_y.clamp(viewport.y0, (viewport.y1 - height).max(viewport.y0));
            self.rect = Rect::new(x, y, x + width, y + height);
            self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll());
            return;
        }
        let Some([px, py]) = self.position else {
            return;
        };
        let height = PADDING * 2.0 + self.content_height().max(ROW_HEIGHT);
        let x0 = f64::from(px);
        let y0 = f64::from(py);
        self.rect = Rect::new(x0, y0, x0 + self.legacy_width(), y0 + height);
    }

    /// Safe to call when already closed.
    pub fn close(&mut self) {
        self.open = false;
        self.active_touch = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Preserved across `close()` so callers can re-open at the same spot.
    /// `None` before the menu has ever been opened.
    pub fn position(&self) -> Option<[f32; 2]> {
        self.position
    }

    /// Lets a consumer route a left-click to
    /// [`click_item`](Self::click_item) without re-deriving the row layout.
    pub fn item_at(&self, point: [f64; 2]) -> Option<usize> {
        if !self.open {
            return None;
        }
        let point = Point::new(point[0], point[1]);
        for idx in 0..self.items.len() {
            if self.item_rect(idx).is_some_and(|rect| rect.contains(point)) {
                return Some(idx);
            }
        }
        None
    }

    /// For dismiss-on-click-outside routing.
    pub fn contains(&self, point: [f64; 2]) -> bool {
        self.open
            && self
                .rect
                .contains(vello::kurbo::Point::new(point[0], point[1]))
    }

    /// No-op for separators / out-of-range indices. Records the index in
    /// `Self::selection` so a `&mut self` / CRDT host can react via
    /// [`Self::take_selection`].
    pub fn click_item(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else {
            return;
        };
        match item {
            MenuItem::Action {
                action,
                enabled: true,
                ..
            } => {
                let cb = action.clone();
                cb();
                self.selection = Some(idx);
                self.open = false;
            }
            MenuItem::Action { .. } | MenuItem::Separator => {}
        }
    }

    /// Returns `Some(idx)` exactly once per selection, then `None`. The
    /// CRDT-friendly action channel — poll it after dispatching an event to
    /// perform a `&mut self` mutation the `Arc<dyn Fn()>` model can't express.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.selection.take()
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    fn row_height(&self) -> f64 {
        self.row_height
    }
    #[cfg(test)]
    fn padding(&self) -> f64 {
        PADDING
    }
    #[cfg(test)]
    fn menu_width(&self) -> f64 {
        self.legacy_width()
    }
    fn rows_viewport(&self) -> Rect {
        self.body_rect()
    }
    fn has_overflow(&self) -> bool {
        self.max_scroll() > 0.0
    }
    fn scroll_by(&mut self, delta: f64) {
        self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, self.max_scroll());
    }

    fn draw_scroll_affordance(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.has_overflow() {
            return;
        }
        let rows_viewport = self.rows_viewport();
        let viewport_height = rows_viewport.height();
        let rows_height = self.content_height().max(1.0);
        let track_width = (2.0 * self.text_scale).max(2.0);
        let track_x1 = self.rect.x1 - 4.0 * self.text_scale;
        let track = Rect::new(
            track_x1 - track_width,
            rows_viewport.y0,
            track_x1,
            rows_viewport.y1,
        );
        let min_thumb_height = (8.0 * self.text_scale).min(viewport_height);
        let thumb_height = (viewport_height * viewport_height / rows_height)
            .clamp(min_thumb_height, viewport_height);
        let progress = self.scroll_offset / self.max_scroll();
        let thumb_y0 = rows_viewport.y0 + (viewport_height - thumb_height) * progress;
        let thumb = Rect::new(track.x0, thumb_y0, track.x1, thumb_y0 + thumb_height);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.overlay, 26),
            None,
            &track,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.border_standard,
            None,
            &thumb,
        );
    }

    /// Skips separators. `dir > 0` moves down, `dir < 0` up.
    fn move_hover(&mut self, dir: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let down = dir > 0;
        // With nothing hovered, entering from above starts at the first row
        // and entering from below at the last; otherwise step off the row we
        // are on. Either way the walk then visits at most `n` candidates.
        let mut idx = match self.hovered {
            Some(i) => wrapping_step(i, n, down),
            None if down => 0,
            None => n.saturating_sub(1),
        };
        for _ in 0..n {
            if self.items.get(idx).is_some_and(MenuItem::is_enabled) {
                self.hovered = Some(idx);
                self.ensure_hover_visible();
                return;
            }
            idx = wrapping_step(idx, n, down);
        }
    }
}

/// One step through `n` items, wrapping past either end. Kept in `usize`
/// so a menu index can never go negative or wrap through a cast.
fn wrapping_step(i: usize, n: usize, down: bool) -> usize {
    if down {
        let next = i.saturating_add(1);
        if next >= n { 0 } else { next }
    } else {
        i.checked_sub(1).unwrap_or_else(|| n.saturating_sub(1))
    }
}

/// Menus are shared by pointer and touch surfaces; never shrink an action
/// below the platform's 44 logical-pixel minimum target.
const ROW_HEIGHT: f64 = 44.0;
const _: () = assert!(ROW_HEIGHT >= 44.0);
const SEPARATOR_HEIGHT: f64 = 9.0;
const MENU_WIDTH: f64 = 220.0;
const PADDING: f64 = 6.0;
/// Air between the armed row's fill edge and the first glyph. The fill is
/// inset `spacing.s1` from the card ([`label_inset`] adds this to it), so the
/// label keeps this gap inside its highlight at every `ui_scale` instead of
/// drifting into the fill's edge as the token inset grows.
const LABEL_GAP: f64 = 6.0;

/// Distance from the card's edge to a label's first glyph: the highlight
/// inset (`spacing.s1`) plus [`LABEL_GAP`] — 10 px at 1×.
fn label_inset(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1) + LABEL_GAP
}

impl Widget for ContextMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.set_text_scale(theme.text_scale);
        let height = if self.open { self.rect.height() } else { 0.0 };
        // Both are small logical-pixel menu dimensions: MENU_WIDTH is a
        // compile-time constant and `height` is a row-count multiple of
        // ROW_HEIGHT, so neither comes close to f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel menu dimensions; f32 precision is sufficient"
        )]
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.legacy_width() as f32),
                height: Dimension::length(height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if self.bounds.is_some() {
            self.layout_at_position();
            return;
        }
        // The caller-provided anchor wins when open; otherwise the
        // taffy-assigned position keeps offscreen/closed menus sensible.
        if self.open && self.position.is_some() {
            self.layout_at_position();
            return;
        }
        let l = taffy.layout(node).copied().unwrap_or_default();
        let (x0, y0) = if let (true, Some([px, py])) = (self.open, self.position) {
            (f64::from(px), f64::from(py))
        } else {
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            )
        };
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
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
        self.set_text_scale(theme.text_scale);
        if !self.open {
            return;
        }

        // Measure in both modes: an anchored menu hugs its labels, a
        // point-placed one keeps its origin and reference width but grows
        // its rows (and, past the reference width, its width) to the text.
        self.layout_within(text_renderer, theme);
        let radius = f64::from(theme.tokens.radii.md);
        let shape = RoundedRect::from_rect(self.rect, radius);
        // The near-black overlay tier, so the menu reads as an opaque
        // raised sheet, never bleeding the content beneath.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        // Panel role (HIG §9.1): the edge is derived against the ground,
        // painted as a crisp 1 px inset stroke. The declared
        // `border_standard` sat three shades off the overlay tier on the
        // dark preset and read as no edge at all.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        // One style for measuring (`layout_within`) and painting, in both
        // modes: the point-placed path used to paint the theme's already
        // text-scaled size through the renderer's own multiplier, so its
        // glyphs came out at 4× on a 200 % setting while its rows were
        // measured at 2×.
        let style =
            text_renderer.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm));
        let label_x = self.rect.x0 + label_inset(theme);
        // HIG §9.5: a separator inside a Panel is inset `spacing.s3` from
        // the panel edge, never running into the corner radius.
        let separator_inset = f64::from(theme.tokens.spacing.s3);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.body_rect(),
        );
        let mut y = self.body_rect().y0 - self.scroll_offset;
        let has_leading_gutter = self.has_leading_gutter();
        for (idx, item) in self.items.iter().enumerate() {
            let row_height = self.item_height(item);
            match item {
                MenuItem::Action {
                    label,
                    icon,
                    checked,
                    destructive,
                    enabled,
                    ..
                } => {
                    // The row Enter / a click would activate. Painted the way
                    // every other menu-like list in the toolkit paints its
                    // armed row (`Popover`, `Dropdown`, `SlashMenu`,
                    // `PageJumpPalette`): a solid accent fill with ink chosen
                    // against it. The previous 10 % veneer measured only
                    // ΔL* ≈ 11 on the near-black `surfaces.overlay` ground,
                    // which reads as "no highlight" on a dark desktop
                    // (2026-09-16 user report); see
                    // `tests/menu_highlight_render.rs`.
                    let armed = *enabled && self.hovered == Some(idx);
                    if armed {
                        let inset = f64::from(theme.tokens.spacing.s1);
                        let hl = Rect::new(
                            self.rect.x0 + inset,
                            y + 1.0,
                            self.rect.x1 - inset,
                            y + row_height - 1.0,
                        );
                        let hl_shape = RoundedRect::from_rect(hl, f64::from(theme.tokens.radii.sm));
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
                            None,
                            &hl_shape,
                        );
                    }
                    let ink = if !*enabled {
                        theme
                            .tokens
                            .colors
                            .on_surface_at(
                                theme.tokens.colors.surfaces.overlay,
                                crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST,
                            )
                            .ink_dim
                    } else if armed {
                        theme
                            .tokens
                            .colors
                            .on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST)
                    } else if *destructive {
                        theme.tokens.colors.semantic.danger
                    } else {
                        theme.tokens.colors.text_primary
                    };
                    let label_x = if has_leading_gutter {
                        let icon_size = f64::from(theme.tokens.typography.scale.sm);
                        if let Some(icon) =
                            (*icon).or_else(|| (*checked == Some(true)).then_some(Icon::Check))
                        {
                            draw_icon(
                                scene,
                                transform,
                                Rect::from_origin_size(
                                    (label_x, y + (row_height - icon_size) / 2.0),
                                    (icon_size, icon_size),
                                ),
                                icon,
                                ink,
                                1.8,
                            );
                        }
                        label_x + icon_size + f64::from(theme.tokens.spacing.s2)
                    } else {
                        label_x
                    };
                    let label = text_renderer.elide_to_width(
                        label,
                        style.size,
                        (self.rect.x1 - label_x - label_inset(theme)).max(1.0) as f32,
                    );
                    let (_, label_height) = text_renderer.measure_style(&label, None, &style);
                    let label_y = y + (row_height - f64::from(label_height)) / 2.0;
                    text_renderer.draw_style(
                        scene,
                        &label,
                        ink,
                        transform.pre_translate(Vec2::new(label_x, label_y)),
                        None,
                        &style,
                    );
                    y += row_height;
                }
                MenuItem::Separator => {
                    let sep_y = y + SEPARATOR_HEIGHT / 2.0;
                    crate::surface::separator(
                        scene,
                        transform,
                        self.rect.x0,
                        self.rect.x1,
                        sep_y,
                        separator_inset,
                        theme.tokens.colors.surfaces.overlay,
                    );
                    y += SEPARATOR_HEIGHT;
                }
            }
        }
        scene.pop_layer();
        self.draw_scroll_affordance(scene, transform, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self
                    .item_at([self.last_cursor.x, self.last_cursor.y])
                    .filter(|index| self.items[*index].is_enabled());
                // Not marked handled (so cursor presence still flows) —
                // only presses are.
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.contains([self.last_cursor.x, self.last_cursor.y]) =>
            {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * self.row_height(),
                    MouseScrollDelta::PixelDelta(position) => -position.y / ctx.scale_factor,
                };
                self.scroll_by(amount);
                self.hovered = self
                    .item_at([self.last_cursor.x, self.last_cursor.y])
                    .filter(|index| self.items[*index].is_enabled());
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match key {
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.move_hover(1);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.move_hover(-1);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(idx) = self.hovered {
                        self.click_item(idx);
                    }
                    *ctx.handled = true;
                }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } if self.bounds.is_some() => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.row_height,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll_offset = (self.scroll_offset - delta).clamp(0.0, self.max_scroll());
                self.hovered = self.item_at([self.last_cursor.x, self.last_cursor.y]);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.item_at([self.last_cursor.x, self.last_cursor.y]) {
                    self.click_item(idx);
                    *ctx.handled = true;
                } else {
                    // An open menu is modal-ish: swallow the dismiss click
                    // so it doesn't also activate whatever sits underneath.
                    self.close();
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                match touch.phase {
                    winit::event::TouchPhase::Started if self.active_touch.is_none() => {
                        self.active_touch = Some(touch.id);
                        self.last_cursor = point;
                        self.hovered = self
                            .item_at([point.x, point.y])
                            .filter(|index| self.items[*index].is_enabled());
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                        self.last_cursor = point;
                        self.hovered = self
                            .item_at([point.x, point.y])
                            .filter(|index| self.items[*index].is_enabled());
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        if let Some(idx) = self.item_at([point.x, point.y]) {
                            self.click_item(idx);
                        } else {
                            self.close();
                        }
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        self.close();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ContextMenu"
    }

    fn traps_focus(&self) -> bool {
        self.open
    }

    /// Participates in the App's modal-capture contract — suppressing focus
    /// shortcuts while up.
    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Menu items are painted rows rather than child widgets, so publish
        // virtual children. A container-only node made the menu visible to an
        // assistive technology but hid every action inside it.
        let action_count = self
            .items
            .iter()
            .filter(|item| matches!(item, MenuItem::Action { .. }))
            .count();
        let mut action_position: usize = 0;
        let children = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let MenuItem::Action {
                    label,
                    checked,
                    enabled,
                    ..
                } = item
                else {
                    return None;
                };
                action_position = action_position.saturating_add(1);
                Some(
                    AccessNode::new(Role::MenuItem)
                        .with_name(label.clone())
                        .with_action_target(format!("context-menu:item:{index}"))
                        .with_bounds(self.item_rect(index).unwrap_or(Rect::ZERO))
                        .with_position(action_position, action_count)
                        .with_state(AccessState {
                            focusable: *enabled,
                            focused: self.open && self.hovered == Some(index),
                            hidden: !self.open,
                            checked: *checked,
                            disabled: !*enabled,
                            ..Default::default()
                        }),
                )
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_state(AccessState {
                    expanded: Some(self.open),
                    hidden: !self.open,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !self.open || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(index) = target
            .strip_prefix("context-menu:item:")
            .and_then(|index| index.parse::<usize>().ok())
        else {
            return false;
        };
        if !self.items.get(index).is_some_and(MenuItem::is_enabled) {
            return false;
        }
        self.click_item(index);
        true
    }
}

#[cfg(test)]
mod a11y_tests {
    use super::*;
    use crate::widget::Widget;

    #[test]
    fn menu_accessibility_reports_state_rows_and_checks() {
        fn accessibility_reports_menu_role_and_collapsed_when_closed() {
            use crate::a11y::Role;
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::action("Open", || {}));
            let node = Widget::accessibility(&menu).expect("menu is not presentational");
            assert_eq!(node.role, Role::Menu);
            assert_eq!(
                node.state.expanded,
                Some(false),
                "closed menu is not expanded"
            );
            assert!(node.state.hidden, "closed menu is hidden from the tree");
        }

        fn accessibility_open_menu_is_expanded_and_visible() {
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::action("Open", || {}));
            menu.open_at([10.0, 20.0]);
            let node = Widget::accessibility(&menu).unwrap();
            assert_eq!(node.state.expanded, Some(true), "open menu is expanded");
            assert!(!node.state.hidden, "open menu is visible");
        }

        fn accessibility_exposes_named_action_rows_and_skips_separators() {
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::action("Keep Offline", || {}));
            menu.push(MenuItem::separator());
            menu.push(MenuItem::action("Sharing & Access…", || {}));
            menu.open_at([10.0, 20.0]);

            let node = Widget::accessibility(&menu).unwrap();
            assert_eq!(node.children.len(), 2);
            assert_eq!(node.children[0].role, crate::a11y::Role::MenuItem);
            assert_eq!(node.children[0].name.as_deref(), Some("Keep Offline"));
            assert_eq!(
                node.children[0].action_target.as_deref(),
                Some("context-menu:item:0")
            );
            assert_eq!(node.children[0].pos_in_set, Some(1));
            assert_eq!(node.children[0].set_size, Some(2));
            assert_eq!(node.children[1].name.as_deref(), Some("Sharing & Access…"));
            assert_eq!(
                node.children[1].action_target.as_deref(),
                Some("context-menu:item:2")
            );
            assert!(node.children.iter().all(|child| child.state.focusable));
        }

        fn accessibility_exposes_checked_state_without_polluting_the_label() {
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::checked_action("List", true, || {}));
            menu.push(MenuItem::checked_action("Grid", false, || {}));
            menu.open_at([10.0, 20.0]);

            let node = Widget::accessibility(&menu).unwrap();
            assert_eq!(node.children[0].name.as_deref(), Some("List"));
            assert_eq!(node.children[0].state.checked, Some(true));
            assert_eq!(node.children[1].name.as_deref(), Some("Grid"));
            assert_eq!(node.children[1].state.checked, Some(false));
        }

        accessibility_reports_menu_role_and_collapsed_when_closed();
        accessibility_open_menu_is_expanded_and_visible();
        accessibility_exposes_named_action_rows_and_skips_separators();
        accessibility_exposes_checked_state_without_polluting_the_label();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use winit::event::{DeviceId, TouchPhase};

    /// Callbacks must be `Send + Sync` (the `MenuAction` bound), so the
    /// counter is an `Arc<AtomicU32>`.
    fn open_menu() -> (ContextMenu, Arc<AtomicU32>) {
        let fired = Arc::new(AtomicU32::new(0));
        let f1 = fired.clone();
        let f2 = fired.clone();
        let mut m = ContextMenu::new();
        m.push(MenuItem::action("Cut", move || {
            f1.fetch_add(1, Ordering::SeqCst);
        }));
        m.push(MenuItem::separator());
        m.push(MenuItem::action("Copy", move || {
            f2.fetch_add(100, Ordering::SeqCst);
        }));
        m.open_at([100.0, 100.0]);
        lay_out(&mut m);
        (m, fired)
    }

    fn lay_out(m: &mut ContextMenu) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = m.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        m.apply_layout(&taffy, node, Point::ZERO);
    }

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn touch(phase: TouchPhase, id: u64, point: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: synth_device(),
            phase,
            location: winit::dpi::PhysicalPosition::new(point.x, point.y),
            force: None,
            id,
        })
    }
    fn wheel_lines(y: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: synth_device(),
            delta: MouseScrollDelta::LineDelta(0.0, y),
            phase: TouchPhase::Moved,
        }
    }
    fn key(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn dispatch(m: &mut ContextMenu, event: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        m.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn pointer_activation_fires_actions_and_dismisses() {
        fn click_on_row_fires_action_and_records_selection() {
            let (mut m, fired) = open_menu();
            // First action row centre: y0 + PADDING + ROW_HEIGHT/2.
            let r = m.get_rect();
            let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
            let p = Point::new(r.x0 + 20.0, row0_y);
            dispatch(&mut m, &cursor_at(p));
            let handled = dispatch(&mut m, &left_press());
            assert!(handled, "a row click is consumed");
            assert_eq!(
                fired.load(Ordering::SeqCst),
                1,
                "the first action's callback fired"
            );
            assert_eq!(m.take_selection(), Some(0), "selection index is exposed");
            assert!(!m.is_open(), "the menu closes after a selection");
            assert_eq!(m.take_selection(), None, "selection drains exactly once");
        }

        fn click_outside_dismisses_and_is_swallowed() {
            let (mut m, fired) = open_menu();
            let r = m.get_rect();
            let p = Point::new(r.x1 + 50.0, r.y1 + 50.0);
            dispatch(&mut m, &cursor_at(p));
            let handled = dispatch(&mut m, &left_press());
            assert!(handled, "outside click is swallowed (not passed through)");
            assert!(!m.is_open(), "outside click dismisses the menu");
            assert_eq!(
                fired.load(Ordering::SeqCst),
                0,
                "no action fires on an outside click"
            );
        }

        fn esc_dismisses() {
            let (mut m, _f) = open_menu();
            let handled = dispatch(&mut m, &key(NamedKey::Escape));
            assert!(handled);
            assert!(!m.is_open(), "Esc closes the menu");
        }

        fn hover_tracks_cursor_row() {
            let (mut m, _f) = open_menu();
            let r = m.get_rect();
            let row0_y = r.y0 + PADDING + ROW_HEIGHT / 2.0;
            dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, row0_y)));
            assert_eq!(m.hovered(), Some(0), "cursor over row 0 highlights it");
            // Move onto the separator → no hover.
            let sep_y = r.y0 + PADDING + ROW_HEIGHT + SEPARATOR_HEIGHT / 2.0;
            dispatch(&mut m, &cursor_at(Point::new(r.x0 + 10.0, sep_y)));
            assert_eq!(m.hovered(), None, "cursor over a separator clears hover");
        }

        fn touch_release_activates_once_and_keeps_the_capture() {
            let (mut menu, fired) = open_menu();
            let rect = menu.get_rect();
            let point = Point::new(rect.x0 + 20.0, rect.y0 + PADDING + ROW_HEIGHT / 2.0);

            assert!(dispatch(&mut menu, &touch(TouchPhase::Started, 7, point)));
            assert!(dispatch(&mut menu, &touch(TouchPhase::Ended, 7, point)));
            assert_eq!(fired.load(Ordering::SeqCst), 1);
            assert_eq!(menu.take_selection(), Some(0));
            assert!(!menu.is_open());
        }

        fn modal_capture_when_open() {
            let (m, _f) = open_menu();
            assert!(Widget::traps_focus(&m), "open menu traps keyboard focus");
            assert!(
                Widget::modal_capture(&m).is_some(),
                "open menu reports capture"
            );
            let closed = ContextMenu::new();
            assert!(!Widget::traps_focus(&closed));
            assert!(Widget::modal_capture(&closed).is_none());
        }

        click_on_row_fires_action_and_records_selection();
        click_outside_dismisses_and_is_swallowed();
        esc_dismisses();
        hover_tracks_cursor_row();
        touch_release_activates_once_and_keeps_the_capture();
        modal_capture_when_open();
    }

    #[test]
    fn disabled_and_destructive_rows_are_not_implicit_targets() {
        fn keyboard_nav_skips_separator_and_enter_selects() {
            let (mut m, fired) = open_menu();
            // Down → first action (idx 0).
            dispatch(&mut m, &key(NamedKey::ArrowDown));
            assert_eq!(m.hovered(), Some(0));
            // Down again → must skip the separator (idx 1) and land on idx 2.
            dispatch(&mut m, &key(NamedKey::ArrowDown));
            assert_eq!(m.hovered(), Some(2), "ArrowDown skips the separator");
            // Enter selects the hovered row.
            dispatch(&mut m, &key(NamedKey::Enter));
            assert_eq!(
                fired.load(Ordering::SeqCst),
                100,
                "Enter fired the second action"
            );
            assert_eq!(m.take_selection(), Some(2));
        }

        fn disabled_rows_reject_pointer_activation() {
            let (mut menu, fired) = open_menu();
            menu.set_item_enabled(0, false);
            let rect = menu.item_rect(0).expect("first action row");
            let point = rect.center();

            dispatch(&mut menu, &cursor_at(point));
            assert_eq!(menu.hovered(), None, "disabled rows are not hover targets");
            assert!(dispatch(&mut menu, &left_press()));
            assert_eq!(fired.load(Ordering::SeqCst), 0);
            assert_eq!(menu.take_selection(), None);
            assert!(menu.is_open(), "a rejected activation keeps the menu open");
        }

        fn keyboard_navigation_skips_disabled_rows() {
            let (mut menu, fired) = open_menu();
            menu.set_item_enabled(0, false);

            dispatch(&mut menu, &key(NamedKey::ArrowDown));
            assert_eq!(menu.hovered(), Some(2));
            dispatch(&mut menu, &key(NamedKey::Enter));

            assert_eq!(fired.load(Ordering::SeqCst), 100);
            assert_eq!(menu.take_selection(), Some(2));
        }

        fn destructive_first_row_is_not_the_implicit_keyboard_target() {
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::destructive_selection("Delete"));
            menu.push(MenuItem::selection("Archive"));
            menu.open_at([10.0, 20.0]);

            assert_eq!(menu.hovered(), Some(1));
        }

        fn item_builders_preserve_icon_and_destructive_intent() {
            let item = MenuItem::destructive_selection("Delete").with_icon(Icon::Trash);
            let MenuItem::Action {
                icon, destructive, ..
            } = item
            else {
                panic!("selection builder must produce an action");
            };

            assert_eq!(icon, Some(Icon::Trash));
            assert!(destructive);
        }

        fn semantic_activation_uses_the_same_selection_channel() {
            let (mut menu, fired) = open_menu();
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_semantic_action(
                &mut menu,
                "context-menu:item:2",
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(fired.load(Ordering::SeqCst), 100);
            assert_eq!(menu.take_selection(), Some(2));
            assert!(!menu.is_open());
        }

        fn semantic_activation_rejects_disabled_rows() {
            let (mut menu, fired) = open_menu();
            menu.set_item_enabled(2, false);
            let mut doc = AutoCommit::new();

            assert!(!Widget::perform_semantic_action(
                &mut menu,
                "context-menu:item:2",
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(fired.load(Ordering::SeqCst), 0);
            assert_eq!(menu.take_selection(), None);
            assert!(menu.is_open());
        }

        keyboard_nav_skips_separator_and_enter_selects();
        disabled_rows_reject_pointer_activation();
        keyboard_navigation_skips_disabled_rows();
        destructive_first_row_is_not_the_implicit_keyboard_target();
        item_builders_preserve_icon_and_destructive_intent();
        semantic_activation_uses_the_same_selection_channel();
        semantic_activation_rejects_disabled_rows();
    }

    #[test]
    fn bounded_menu_scrolls_and_keeps_targets_reachable() {
        fn bounded_wheel_hit_testing_and_keyboard_activation_share_scroll_offset() {
            let mut menu = ContextMenu::new();
            for index in 0..20 {
                menu.push(MenuItem::action(format!("Command {index}"), || {}));
            }
            menu.open_anchored(
                Rect::new(10.0, 10.0, 30.0, 30.0),
                Rect::new(0.0, 0.0, 180.0, 100.0),
            );
            menu.layout_within(&mut TextRenderer::new(), &Theme::dark());
            let point = menu.get_rect().center();
            dispatch(&mut menu, &cursor_at(point));
            let wheel = |y| WindowEvent::MouseWheel {
                device_id: synth_device(),
                delta: MouseScrollDelta::LineDelta(0.0, y),
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(dispatch(&mut menu, &wheel(-100.0)));
            assert_eq!(menu.scroll_offset, menu.max_scroll());
            let last = menu.item_rect(19).unwrap().center();
            assert_eq!(menu.item_at([last.x, last.y]), Some(19));
            dispatch(&mut menu, &cursor_at(last));
            dispatch(&mut menu, &left_press());
            assert_eq!(menu.take_selection(), Some(19));
            assert_eq!(menu.take_selection(), None);
            menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
            menu.focus_first_action();
            for _ in 0..19 {
                dispatch(&mut menu, &key(NamedKey::ArrowDown));
            }
            assert_eq!(menu.hovered(), Some(19));
            assert!(menu.item_rect(19).is_some());
            dispatch(&mut menu, &key(NamedKey::Enter));
            assert_eq!(menu.take_selection(), Some(19));
            menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
            dispatch(&mut menu, &wheel(100.0));
            assert_eq!(menu.scroll_offset, 0.0);
        }

        fn viewport_bounds_menu_and_keeps_keyboard_target_reachable() {
            let mut menu = ContextMenu::new();
            for label in ["First", "Second", "Third", "Fourth"] {
                menu.push(MenuItem::action(label, || {}));
            }
            menu.set_text_scale(2.0);
            // Two rows fit in this viewport. A bottom-right invocation flips the
            // popup upward, so the remaining rows stay reachable via scrolling.
            let viewport = Rect::new(
                0.0,
                0.0,
                menu.menu_width(),
                menu.padding() * 2.0 + menu.row_height() * 2.0,
            );
            menu.set_viewport(Some(viewport));
            menu.open_at([390.0, 160.0]);
            assert!(
                menu.has_overflow(),
                "clipped rows expose a scroll affordance"
            );
            assert_eq!(menu.get_rect().x1, viewport.x1);
            assert_eq!(menu.get_rect().y0, viewport.y0);
            assert_eq!(menu.get_rect().y1, viewport.y1);
            for _ in 0..4 {
                menu.move_hover(1);
            }
            assert_eq!(menu.hovered(), Some(3));
            let last = menu.item_rect(3).expect("last action is laid out");
            assert!(last.y0 >= menu.rect.y0 + menu.padding());
            assert!(last.y1 <= menu.rect.y1 - menu.padding());
        }

        fn wheel_scrolls_rows_inside_a_bounded_menu() {
            let mut menu = ContextMenu::new();
            for label in ["First", "Second", "Third", "Fourth"] {
                menu.push(MenuItem::action(label, || {}));
            }
            let viewport = Rect::new(0.0, 0.0, menu.menu_width(), 68.0);
            menu.set_viewport(Some(viewport));
            menu.open_at([0.0, 0.0]);
            let pointer = Point::new(
                16.0,
                menu.rect.y0 + menu.padding() + menu.row_height() / 2.0,
            );
            dispatch(&mut menu, &cursor_at(pointer));
            assert!(dispatch(&mut menu, &wheel_lines(-2.0)));
            assert_eq!(menu.item_at([pointer.x, pointer.y]), Some(2));
        }

        fn text_scale_updates_layout_and_hit_testing_together() {
            let (mut m, _fired) = open_menu();
            let before = m.get_rect();
            m.set_text_scale(2.0);
            let after = m.get_rect();
            assert_eq!(after.width(), before.width() * 2.0);
            assert_eq!(
                after.height(),
                PADDING * 2.0 + ROW_HEIGHT * 4.0 + SEPARATOR_HEIGHT
            );

            let second_action = m.item_rect(2).expect("second action row");
            let p = Point::new(
                f64::midpoint(second_action.x0, second_action.x1),
                second_action.y0 + 1.0,
            );
            assert_eq!(m.item_at([p.x, p.y]), Some(2));
        }

        bounded_wheel_hit_testing_and_keyboard_activation_share_scroll_offset();
        viewport_bounds_menu_and_keeps_keyboard_target_reachable();
        wheel_scrolls_rows_inside_a_bounded_menu();
        text_scale_updates_layout_and_hit_testing_together();
    }

    #[test]
    fn legacy_open_at_geometry_scales_with_text() {
        /// At 1× a point-placed menu paints exactly where `open_at` put it, at
        /// the reference width and row height: measuring the labels changes
        /// nothing a reference-scale menu already had.
        fn legacy_open_at_geometry_is_unchanged_at_1x_after_paint() {
            let (mut m, _f) = open_menu();
            let before = m.get_rect();
            let mut scene = Scene::new();
            m.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::default(),
            );
            assert_eq!(m.get_rect(), before);
            assert_eq!(m.row_height, ROW_HEIGHT);
        }

        /// A point-placed (`open_at`) menu keeps its origin and never shrinks
        /// below the reference width, but its rows follow the text. At a 200 %
        /// text setting the labels used to be painted at 4× — the theme's
        /// already-scaled size sent through the renderer's own multiplier —
        /// into fixed 28 px rows (file-explorer `context-menu` at
        /// `--text-scale 2`); now every glyph line lands inside its own row.
        fn legacy_open_at_rows_grow_with_the_text_scale_and_glyphs_stay_in_their_rows() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut m = ContextMenu::new();
            m.push(MenuItem::action("Neuer Ordner", || {}));
            m.push(MenuItem::separator());
            m.push(MenuItem::action("Verborgene Dateien anzeigen", || {}));
            m.push(MenuItem::action("Nach Name sortieren", || {}));
            m.open_at([100.0, 100.0]);
            let before = m.get_rect();
            let mut scene = Scene::new();
            m.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let after = m.get_rect();
            assert_eq!((after.x0, after.y0), (before.x0, before.y0), "origin kept");
            assert!(
                after.width() >= MENU_WIDTH,
                "never narrower than the reference"
            );
            assert!(
                m.row_height > ROW_HEIGHT,
                "rows grew to the text: {}",
                m.row_height
            );

            let rows: Vec<Rect> = [0, 2, 3].iter().filter_map(|&i| m.item_rect(i)).collect();
            assert_eq!(rows.len(), 3, "every action row is painted");
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(!runs.is_empty(), "labels were painted");
            // Measure the painted size with an unscaled renderer: `font_size`
            // is the final glyph size, and it must be the theme's 2× — not 4×.
            let mut probe = TextRenderer::new();
            for run in runs {
                assert!(
                    (run.font_size - theme.tokens.typography.scale.sm).abs() < 0.01,
                    "glyphs painted at {} px; the theme's scaled size is {}",
                    run.font_size,
                    theme.tokens.typography.scale.sm,
                );
                let top = f64::from(run.transform.translation[1]);
                let (_, h) = probe.measure("Ag", run.font_size, None);
                assert!(
                    rows.iter()
                        .any(|r| top >= r.y0 - 0.01 && top + f64::from(h) <= r.y1 + 0.01),
                    "a glyph line at y={top} (h={h}) escapes every row: {rows:?}",
                );
            }
        }

        legacy_open_at_geometry_is_unchanged_at_1x_after_paint();
        legacy_open_at_rows_grow_with_the_text_scale_and_glyphs_stay_in_their_rows();
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    #[test]
    fn bounded_menu_measures_and_clamps_in_every_viewport() {
        fn bounded_menu_measures_scales_and_clamps_at_all_edges() {
            let viewport = Rect::new(10.0, 20.0, 110.0, 95.0);
            for anchor in [
                Rect::new(10.0, 20.0, 30.0, 40.0),
                Rect::new(90.0, 20.0, 110.0, 40.0),
                Rect::new(10.0, 75.0, 30.0, 95.0),
                Rect::new(90.0, 75.0, 110.0, 95.0),
            ] {
                let mut menu = ContextMenu::new();
                for _ in 0..8 {
                    menu.push(MenuItem::action(
                        "Sehr lange übersetzte Aktion 共有 العربية",
                        || {},
                    ));
                }
                menu.open_anchored(anchor, viewport);
                let mut text = TextRenderer::new();
                text.set_text_scale(2.0);
                menu.layout_within(&mut text, &Theme::dark().with_text_scale(2.0));
                assert_eq!(menu.get_rect().intersect(viewport), menu.get_rect());
                assert!(menu.row_height > ROW_HEIGHT);
                menu.focus_item(7);
                assert!(menu.scroll_offset > 0.0);
                let last = menu.item_rect(7).unwrap();
                assert_eq!(menu.item_at([last.center().x, last.center().y]), Some(7));
                assert!(menu.item_rect(0).is_none());
                menu.move_hover(1);
                assert_eq!(menu.hovered(), Some(0));
                assert_eq!(menu.scroll_offset, 0.0);
            }
        }

        fn width_uses_shaping_and_legacy_open_resets_bounded_geometry() {
            let mut narrow = ContextMenu::new();
            narrow.push(MenuItem::action("iii", || {}));
            let mut wide = ContextMenu::new();
            wide.push(MenuItem::action("WWW", || {}));
            let mut text = TextRenderer::new();
            for menu in [&mut narrow, &mut wide] {
                menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 1000.0, 1000.0));
                menu.layout_within(&mut text, &Theme::dark());
            }
            assert!(wide.get_rect().width() > narrow.get_rect().width());
            wide.open_at([900.0, 950.0]);
            assert_eq!(
                wide.get_rect(),
                Rect::new(
                    900.0,
                    950.0,
                    900.0 + MENU_WIDTH,
                    950.0 + ROW_HEIGHT + 2.0 * PADDING
                )
            );
            assert_eq!(wide.scroll_offset, 0.0);
        }

        fn tiny_viewport_keeps_focused_rows_reachable() {
            let mut menu = ContextMenu::new();
            menu.push(MenuItem::action("First", || {}));
            menu.push(MenuItem::separator());
            menu.push(MenuItem::action("Last", || {}));
            let viewport = Rect::new(0.0, 0.0, 4.0, 4.0);
            menu.open_anchored(viewport, viewport);
            menu.layout_within(&mut TextRenderer::new(), &Theme::dark());
            menu.focus_item(2);
            assert_eq!(menu.get_rect(), viewport);
            let last = menu.item_rect(2).unwrap();
            assert_eq!(menu.item_at([last.center().x, last.center().y]), Some(2));
        }

        bounded_menu_measures_scales_and_clamps_at_all_edges();
        width_uses_shaping_and_legacy_open_resets_bounded_geometry();
        tiny_viewport_keeps_focused_rows_reachable();
    }
}
