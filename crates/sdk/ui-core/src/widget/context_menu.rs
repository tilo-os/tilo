//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators, backed visually by `overlay::Popover`
//! so it participates in the overlay stack, scrim dismissal, and focus
//! contract.
//!
//! Callers push `MenuItem::action(label, fn)` and optional
//! `MenuItem::separator()` entries, then call `open_at` on right-click.

use automerge::{AutoCommit, ObjId};
use std::sync::Arc;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, draw_icon};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
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
        /// Disabled rows remain visible and announced but reject pointer,
        /// keyboard, touch, and semantic activation without dismissing the
        /// menu.
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

    pub fn is_enabled(&self) -> bool {
        matches!(self, MenuItem::Action { enabled: true, .. })
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
    /// The finger currently owning menu interaction. Touch is captured from
    /// press through release so an underlying row cannot also activate.
    active_touch: Option<u64>,
    /// The live system text scale.  Menus are painted outside the normal
    /// widget tree in several consumers, so retain it here for immediate
    /// open-and-click geometry as well as the regular layout pass.
    text_scale: f64,
    /// Optional owner-provided viewport. A menu prefers opening downward from
    /// its anchor, flips above it when that gives more room, and never paints
    /// outside this rectangle. Rows scroll when neither side can fit them.
    viewport: Option<Rect>,
    /// Scroll offset into the rows, in logical pixels.
    scroll_y: f64,
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
            active_touch: None,
            text_scale: 1.0,
            viewport: None,
            scroll_y: 0.0,
        }
    }

    pub fn push(&mut self, item: MenuItem) {
        self.items.push(item);
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    /// Refresh a row's availability while a menu is open. If the row owned
    /// keyboard focus when it became disabled, move focus to the next
    /// available command.
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
            // Keep the old index as the navigation origin so disabling a
            // focused middle row advances to its next peer instead of
            // restarting at the first command.
            self.move_hover(1);
            if self.hovered == Some(index) {
                self.hovered = None;
            }
        }
    }

    /// Also computes `rect` immediately (via
    /// [`layout_at_position`](Self::layout_at_position)) so
    /// [`item_at`](Self::item_at) / [`contains`](Self::contains) are usable
    /// without first running a `draw`/`apply_layout` pass — needed by
    /// consumers (like `TextInput`) that route a click to a row in the same
    /// input batch the menu opened in.
    pub fn open_at(&mut self, position: [f32; 2]) {
        self.open = true;
        self.position = Some(position);
        self.scroll_y = 0.0;
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
            self.layout_at_position();
        }
    }

    /// Keep the menu inside an owner-provided viewport. It opens on the side
    /// of its anchor with more room, then wheel and keyboard navigation scroll
    /// rows internally when neither side can fit them. The accessibility tree
    /// still exposes every complete label.
    pub fn set_viewport(&mut self, viewport: Option<Rect>) {
        self.viewport = viewport.filter(|rect| rect.width() > 0.0 && rect.height() > 0.0);
        self.layout_at_position();
    }

    /// Mirrors the `build_layout` + `apply_layout` geometry without a taffy
    /// round-trip. A no-op when the menu has never been positioned. Lets a
    /// standalone consumer (one that owns a `ContextMenu` as a field rather
    /// than a tree node) hit-test and draw the menu directly.
    pub fn layout_at_position(&mut self) {
        let Some([px, py]) = self.position else {
            return;
        };
        let anchor = Point::new(f64::from(px), f64::from(py));
        let (width, height, x0, y0) = self.viewport.map_or_else(
            || (self.menu_width(), self.content_height(), anchor.x, anchor.y),
            |viewport| {
                let width = self.menu_width().min(viewport.width());
                let anchor_y = anchor.y.clamp(viewport.y0, viewport.y1);
                let desired_height = self.content_height().min(viewport.height());
                let below = viewport.y1 - anchor_y;
                let above = anchor_y - viewport.y0;
                let (height, y0) = if below >= desired_height {
                    (desired_height, anchor_y)
                } else if above >= desired_height {
                    (desired_height, anchor_y - desired_height)
                } else if self.content_height() >= viewport.height() {
                    // An oversized menu gets the entire viewport. This keeps
                    // its scrollable surface useful even at a bottom-edge
                    // anchor where neither side has a full row available.
                    (viewport.height(), viewport.y0)
                } else if below >= above {
                    (below, anchor_y)
                } else {
                    (above, viewport.y0)
                };
                let x0 = anchor
                    .x
                    .clamp(viewport.x0, (viewport.x1 - width).max(viewport.x0));
                (width, height, x0, y0)
            },
        );
        self.rect = Rect::new(x0, y0, x0 + width, y0 + height);
        self.clamp_scroll();
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
        let [px, py] = point;
        if px < self.rect.x0
            || px > self.rect.x1
            || py < self.rows_viewport().y0
            || py > self.rows_viewport().y1
        {
            return None;
        }
        let mut y = self.rect.y0 + self.padding() - self.scroll_y;
        for (idx, item) in self.items.iter().enumerate() {
            let h = self.item_height(item);
            if matches!(item, MenuItem::Action { .. }) && py >= y && py < y + h {
                return Some(idx);
            }
            y += h;
        }
        None
    }

    /// The exact painted rectangle of an actionable row, if the menu is open.
    /// Callers that synthesize pointer input (including app-level accessibility
    /// tests) can use this instead of duplicating menu padding and row-height
    /// constants that drift when the system text scale changes.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        if !self.open {
            return None;
        }
        let mut y = self.rect.y0 + self.padding() - self.scroll_y;
        for (idx, item) in self.items.iter().enumerate() {
            let h = self.item_height(item);
            if idx == index {
                return matches!(item, MenuItem::Action { .. }).then(|| {
                    Rect::new(self.rect.x0, y, self.rect.x1, y + h).intersect(self.rows_viewport())
                });
            }
            y += h;
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
    /// [`Self::selection`] so a `&mut self` / CRDT host can react via
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
        // TextRenderer applies the system scale while drawing. Its caller also
        // supplies scaled type tokens, so custom overlay rows need the same
        // effective (squared) vertical scale to avoid glyphs colliding.
        ROW_HEIGHT * self.text_scale * self.text_scale
    }

    fn separator_height(&self) -> f64 {
        SEPARATOR_HEIGHT * self.text_scale * self.text_scale
    }

    fn menu_width(&self) -> f64 {
        MENU_WIDTH * self.text_scale
    }

    fn padding(&self) -> f64 {
        PADDING * self.text_scale * self.text_scale
    }

    fn item_height(&self, item: &MenuItem) -> f64 {
        match item {
            MenuItem::Action { .. } => self.row_height(),
            MenuItem::Separator => self.separator_height(),
        }
    }

    fn content_height(&self) -> f64 {
        self.padding() * 2.0
            + self
                .items
                .iter()
                .map(|item| self.item_height(item))
                .sum::<f64>()
                .max(self.row_height())
    }

    fn rows_viewport_height(&self) -> f64 {
        self.rows_viewport().height()
    }

    fn rows_viewport(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.padding(),
            self.rect.x1,
            (self.rect.y1 - self.padding()).max(self.rect.y0 + self.padding()),
        )
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.rect.height()).max(0.0)
    }

    fn has_overflow(&self) -> bool {
        self.max_scroll() > 0.0
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    fn ensure_item_visible(&mut self, index: usize) {
        let mut top = 0.0;
        for item in self.items.iter().take(index) {
            top += self.item_height(item);
        }
        let Some(item) = self.items.get(index) else {
            return;
        };
        let bottom = top + self.item_height(item);
        let viewport = self.rows_viewport_height();
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if bottom > self.scroll_y + viewport {
            self.scroll_y = bottom - viewport;
        }
        self.clamp_scroll();
    }

    fn scroll_by(&mut self, delta: f64) {
        self.scroll_y += delta;
        self.clamp_scroll();
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the popup width is constrained to the logical viewport"
    )]
    fn rendered_label(
        &self,
        label: &str,
        text_renderer: &mut TextRenderer,
        text_size: f32,
        label_x: f64,
    ) -> String {
        // Keep painting within the popup even when a locale has an unusually
        // long label. The unabridged `label` remains the accessibility name.
        let usable_width = (self.rect.x1 - label_x - 10.0 * self.text_scale).max(1.0) as f32;
        text_renderer.elide_to_width(label, text_size, usable_width)
    }

    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let text_size = theme.tokens.typography.scale.sm;
        let rendered_text_size = f64::from(text_size) * f64::from(text_renderer.text_scale());
        let rows_viewport = self.rows_viewport();
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &rows_viewport,
        );
        let mut y = self.rect.y0 + self.padding() - self.scroll_y;
        let has_leading_gutter = self.items.iter().any(|item| {
            matches!(item, MenuItem::Action { icon: Some(_), .. }) || item.checked().is_some()
        });
        for (idx, item) in self.items.iter().enumerate() {
            match item {
                MenuItem::Action {
                    label,
                    icon,
                    checked,
                    destructive,
                    enabled,
                    ..
                } => {
                    if *enabled && self.hovered == Some(idx) {
                        let hl = Rect::new(
                            self.rect.x0 + 4.0 * self.text_scale,
                            y + self.text_scale,
                            self.rect.x1 - 4.0 * self.text_scale,
                            y + self.row_height() - self.text_scale,
                        );
                        let hl_shape = RoundedRect::from_rect(hl, f64::from(theme.tokens.radii.sm));
                        scene.fill(
                            vello::peniko::Fill::NonZero,
                            transform,
                            theme.tokens.colors.surface_white_10,
                            None,
                            &hl_shape,
                        );
                    }
                    let label_y = y + (self.row_height() - rendered_text_size) / 2.0;
                    let label_x = if has_leading_gutter {
                        let icon_size = 14.0 * self.text_scale;
                        let icon_rect = Rect::new(
                            self.rect.x0 + 10.0 * self.text_scale,
                            y + (self.row_height() - icon_size) / 2.0,
                            self.rect.x0 + 24.0 * self.text_scale,
                            y + f64::midpoint(self.row_height(), icon_size),
                        );
                        let leading_icon =
                            (*icon).or_else(|| (*checked == Some(true)).then_some(Icon::Check));
                        if let Some(icon) = leading_icon {
                            let icon_color = if *enabled {
                                theme.tokens.colors.text_primary
                            } else {
                                theme
                                    .tokens
                                    .colors
                                    .on_surface_at(
                                        theme.tokens.colors.surfaces.overlay,
                                        crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST,
                                    )
                                    .ink_dim
                            };
                            draw_icon(
                                scene,
                                transform,
                                icon_rect,
                                icon,
                                icon_color,
                                1.8 * self.text_scale,
                            );
                        }
                        self.rect.x0 + 32.0 * self.text_scale
                    } else {
                        self.rect.x0 + 10.0 * self.text_scale
                    };
                    let rendered_label =
                        self.rendered_label(label, text_renderer, text_size, label_x);
                    let label_color = if !*enabled {
                        theme
                            .tokens
                            .colors
                            .on_surface_at(
                                theme.tokens.colors.surfaces.overlay,
                                crate::tokens::colors::ON_CARD_SMALL_TEXT_CONTRAST,
                            )
                            .ink_dim
                    } else if *destructive {
                        theme.tokens.colors.semantic.danger
                    } else {
                        theme.tokens.colors.text_primary
                    };
                    text_renderer.draw_text(
                        scene,
                        &rendered_label,
                        text_size,
                        label_color,
                        transform.pre_translate(Vec2::new(label_x, label_y)),
                        None,
                    );
                    y += self.row_height();
                }
                MenuItem::Separator => {
                    let sep_y = y + self.separator_height() / 2.0;
                    let line = Rect::new(
                        self.rect.x0 + 8.0 * self.text_scale,
                        sep_y - 0.5 * self.text_scale,
                        self.rect.x1 - 8.0 * self.text_scale,
                        sep_y + 0.5 * self.text_scale,
                    );
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        theme.tokens.colors.border_standard,
                        None,
                        &line,
                    );
                    y += self.separator_height();
                }
            }
        }
        scene.pop_layer();
    }

    fn draw_scroll_affordance(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.has_overflow() {
            return;
        }
        let rows_viewport = self.rows_viewport();
        let viewport_height = rows_viewport.height();
        let rows_height = (self.content_height() - self.padding() * 2.0).max(1.0);
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
        let progress = self.scroll_y / self.max_scroll();
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
                self.ensure_item_visible(idx);
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

impl Widget for ContextMenu {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
                width: Dimension::length(self.menu_width() as f32),
                height: Dimension::length(height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
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

        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
        // The near-black overlay tier, so the menu reads as an opaque
        // raised sheet, never bleeding the content beneath.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
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

        self.draw_rows(scene, transform, text_renderer, theme);
        // A static token-driven thumb makes hidden rows discoverable to
        // pointer users without relying on motion.
        self.draw_scroll_affordance(scene, transform, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    MouseScrollDelta::PixelDelta(position) => -position.y / scale_factor,
                };
                self.scroll_by(amount);
                self.hovered = self
                    .item_at([self.last_cursor.x, self.last_cursor.y])
                    .filter(|index| self.items[*index].is_enabled());
                *handled = true;
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
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.move_hover(1);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.move_hover(-1);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(idx) = self.hovered {
                        self.click_item(idx);
                    }
                    *handled = true;
                }
                _ => {}
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.item_at([self.last_cursor.x, self.last_cursor.y]) {
                    self.click_item(idx);
                    *handled = true;
                } else {
                    // An open menu is modal-ish: swallow the dismiss click
                    // so it doesn't also activate whatever sits underneath.
                    self.close();
                    *handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / scale_factor,
                    touch.location.y / scale_factor,
                );
                match touch.phase {
                    winit::event::TouchPhase::Started if self.active_touch.is_none() => {
                        self.active_touch = Some(touch.id);
                        self.last_cursor = point;
                        self.hovered = self
                            .item_at([point.x, point.y])
                            .filter(|index| self.items[*index].is_enabled());
                        *handled = true;
                    }
                    winit::event::TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                        self.last_cursor = point;
                        self.hovered = self
                            .item_at([point.x, point.y])
                            .filter(|index| self.items[*index].is_enabled());
                        *handled = true;
                    }
                    winit::event::TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        if let Some(idx) = self.item_at([point.x, point.y]) {
                            self.click_item(idx);
                        } else {
                            self.close();
                        }
                        *handled = true;
                    }
                    winit::event::TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        self.close();
                        *handled = true;
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
        _state: &mut AutoCommit,
        _path: &ObjId,
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

    #[test]
    fn accessibility_open_menu_is_expanded_and_visible() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        menu.open_at([10.0, 20.0]);
        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.state.expanded, Some(true), "open menu is expanded");
        assert!(!node.state.hidden, "open menu is visible");
    }

    #[test]
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

    #[test]
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

    #[test]
    fn accessibility_exposes_disabled_rows_as_unfocusable() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::selection("Paste").with_enabled(false));
        menu.open_at([10.0, 20.0]);

        let node = Widget::accessibility(&menu).unwrap();
        assert!(node.children[0].state.disabled);
        assert!(!node.children[0].state.focusable);
        assert!(!Widget::perform_semantic_action(
            &mut menu,
            "context-menu:item:0",
            &crate::a11y::Action::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert!(
            menu.is_open(),
            "a denied semantic action keeps the menu open"
        );
        assert!(menu.take_selection().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    #[test]
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

    #[test]
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

    #[test]
    fn esc_dismisses() {
        let (mut m, _f) = open_menu();
        let handled = dispatch(&mut m, &key(NamedKey::Escape));
        assert!(handled);
        assert!(!m.is_open(), "Esc closes the menu");
    }

    #[test]
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

    #[test]
    fn disabled_rows_reject_pointer_and_keyboard_without_dismissing() {
        let fired = Arc::new(AtomicU32::new(0));
        let denied_fired = fired.clone();
        let allowed_fired = fired.clone();
        let mut menu = ContextMenu::new();
        menu.push(
            MenuItem::action("Denied", move || {
                denied_fired.fetch_add(1, Ordering::SeqCst);
            })
            .with_enabled(false),
        );
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Allowed", move || {
            allowed_fired.fetch_add(10, Ordering::SeqCst);
        }));
        menu.open_at([100.0, 100.0]);
        lay_out(&mut menu);

        let denied = menu.item_rect(0).unwrap().center();
        dispatch(&mut menu, &cursor_at(denied));
        assert_eq!(menu.hovered(), None, "disabled rows do not highlight");
        assert!(dispatch(&mut menu, &left_press()));
        assert!(menu.is_open(), "a disabled click does not dismiss the menu");
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        assert!(menu.take_selection().is_none());

        dispatch(&mut menu, &key(NamedKey::ArrowDown));
        assert_eq!(menu.hovered(), Some(2), "keyboard skips disabled rows");
        dispatch(&mut menu, &key(NamedKey::Enter));
        assert_eq!(fired.load(Ordering::SeqCst), 10);
        assert_eq!(menu.take_selection(), Some(2));
    }

    #[test]
    fn disabling_the_focused_middle_row_advances_to_its_next_peer() {
        let mut menu = ContextMenu::new();
        for label in ["First", "Second", "Third"] {
            menu.push(MenuItem::selection(label));
        }
        menu.open_at([10.0, 20.0]);
        menu.move_hover(1);
        menu.move_hover(1);
        assert_eq!(menu.hovered(), Some(1));

        menu.set_item_enabled(1, false);

        assert_eq!(menu.hovered(), Some(2));
        assert!(!menu.items()[1].is_enabled());
    }

    #[test]
    fn destructive_first_row_is_not_the_implicit_keyboard_target() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::destructive_selection("Delete"));
        menu.push(MenuItem::selection("Archive"));
        menu.open_at([10.0, 20.0]);

        assert_eq!(menu.hovered(), Some(1));
    }

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
    fn text_scale_updates_layout_and_hit_testing_together() {
        let (mut m, _fired) = open_menu();
        let before = m.get_rect();
        m.set_text_scale(2.0);
        let after = m.get_rect();
        assert_eq!(after.width(), before.width() * 2.0);
        assert_eq!(after.height(), before.height() * 4.0);

        let second_action = m.item_rect(2).expect("second action row");
        let p = Point::new(
            f64::midpoint(second_action.x0, second_action.x1),
            second_action.y0 + 1.0,
        );
        assert_eq!(m.item_at([p.x, p.y]), Some(2));
    }

    #[test]
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

    #[test]
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

    #[test]
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
}
