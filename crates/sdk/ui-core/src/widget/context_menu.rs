//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators. Standalone owners draw it in their
//! overlay pass. `open_anchored` opts into measured, viewport-bounded geometry;
//! legacy `open_at` callers retain their point placement and fixed rows.
//!
//! Callers push `MenuItem::action(label, fn)` and optional
//! `MenuItem::separator()` entries, then call `open_at` on right-click.

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, draw_icon};
use crate::platform::{PlatformConventions, Shortcut};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::floating_surface::{self, PanelAlign, PopoverSide};

/// What a menu row runs. Menus live on the UI thread with the widgets that
/// open them, so a row takes the toolkit's usual single-threaded callback:
/// it may capture `Rc` state like any `Button::on_click`.
pub type MenuAction = std::rc::Rc<dyn Fn()>;

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
        /// The keyboard shortcut that runs the same command, shown at the
        /// row's trailing edge in the platform's spelling.
        shortcut: Option<Shortcut>,
        /// `Some` makes the row open a nested menu (with a chevron) instead
        /// of running `action`.
        submenu: Option<Vec<MenuItem>>,
    },
    Separator,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MenuItem::Action { label, submenu, .. } => f
                .debug_struct("MenuItem::Action")
                .field("label", label)
                .field("action", &"<fn>")
                .field("submenu", submenu)
                .finish(),
            MenuItem::Separator => f.write_str("MenuItem::Separator"),
        }
    }
}

impl MenuItem {
    fn command(label: String, action: MenuAction) -> Self {
        MenuItem::Action {
            label,
            action,
            icon: None,
            checked: None,
            destructive: false,
            enabled: true,
            shortcut: None,
            submenu: None,
        }
    }

    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        Self::command(label.into(), std::rc::Rc::new(action))
    }

    /// A command row with a themed leading icon. Menus containing icon rows
    /// reserve the same gutter for every command so labels stay aligned.
    pub fn icon_action<F>(icon: Icon, label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        Self::action(label, action).with_icon(icon)
    }

    pub fn selection(label: impl Into<String>) -> Self {
        Self::action(label, || {})
    }

    pub fn destructive_action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        let mut item = Self::action(label, action);
        if let MenuItem::Action { destructive, .. } = &mut item {
            *destructive = true;
        }
        item
    }

    pub fn destructive_selection(label: impl Into<String>) -> Self {
        Self::destructive_action(label, || {})
    }

    /// A row that opens a nested menu of `items` beside it: on hover, a
    /// click, Enter or the arrow towards it. Its chevron says so.
    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        let mut item = Self::command(label.into(), std::rc::Rc::new(|| {}));
        if let MenuItem::Action { submenu, .. } = &mut item {
            *submenu = Some(items);
        }
        item
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

    /// Show the keyboard shortcut that runs the same command at the row's
    /// trailing edge (`Shortcut::command("S")`, `Shortcut::key("Delete")`).
    #[must_use]
    pub fn with_shortcut(mut self, shortcut: impl Into<Shortcut>) -> Self {
        if let MenuItem::Action {
            shortcut: item_shortcut,
            ..
        } = &mut self
        {
            *item_shortcut = Some(shortcut.into());
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

    /// Make this a state row ("Copy on select", a sort order): a check mark
    /// when `checked`, and a semantic checked state either way. Checked and
    /// unchecked peers share the same leading gutter, so labels never jump
    /// as state changes.
    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        if let MenuItem::Action {
            checked: item_checked,
            ..
        } = &mut self
        {
            *item_checked = Some(checked);
        }
        self
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

    /// The shortcut shown on the row, if any.
    pub fn shortcut(&self) -> Option<&Shortcut> {
        match self {
            MenuItem::Action { shortcut, .. } => shortcut.as_ref(),
            MenuItem::Separator => None,
        }
    }

    /// The nested menu's rows, for a row that opens one.
    pub fn submenu_items(&self) -> Option<&[MenuItem]> {
        match self {
            MenuItem::Action { submenu, .. } => submenu.as_deref(),
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
    /// Where the menu was opened at the pointer, until the pointer moves
    /// away from there. A host repeats a still pointer's position after a
    /// rebuild ([`crate::pointer_replay`]); that repeat is not the person
    /// pointing at a row, so it leaves the keyboard's row where it is.
    opened_under: Option<Point>,
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
    /// The row the menu acts on, ringed while the menu is open.
    anchor_row: Option<Rect>,
    /// Which edge of its anchor the menu lines up with.
    align: PanelAlign,
    /// Which side of its anchor the menu opens on: below a control, beside
    /// the row of a parent menu.
    side: PopoverSide,
    /// The open nested menu and the row that opened it.
    submenu: Option<(usize, Box<ContextMenu>)>,
    /// A pick in a nested menu as `(row, nested row)`, drained by
    /// [`ContextMenu::take_submenu_selection`].
    submenu_selection: Option<(usize, usize)>,
    /// Keyboard focus is in the nested menu.
    submenu_focused: bool,
    platform: PlatformConventions,
    /// The trailing column (shortcuts, chevrons) from the last measure.
    trailing_width: f64,
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
            opened_under: None,
            selection: None,
            bounds: None,
            row_height: ROW_HEIGHT,
            measured_width: MENU_WIDTH,
            scroll_offset: 0.0,
            active_touch: None,
            text_scale: 1.0,
            viewport: None,
            anchor_row: None,
            align: PanelAlign::Leading,
            side: PopoverSide::Below,
            submenu: None,
            submenu_selection: None,
            submenu_focused: false,
            platform: PlatformConventions::default(),
            trailing_width: 0.0,
        }
    }

    /// A menu of `items`, closed.
    pub fn with_items(items: Vec<MenuItem>) -> Self {
        let mut menu = Self::new();
        menu.items = items;
        menu
    }

    /// Replace every row (closes an open nested menu).
    pub fn set_items(&mut self, items: Vec<MenuItem>) {
        self.items = items;
        self.submenu = None;
        if self.hovered.is_some_and(|i| i >= self.items.len()) {
            self.hovered = None;
        }
        self.layout_at_position();
    }

    /// Line the menu up with its anchor's trailing edge (a menu from a
    /// button at the end of a header row) instead of its leading edge.
    pub fn set_align(&mut self, align: PanelAlign) {
        self.align = align;
        self.layout_at_position();
    }

    /// Mark the row (a message, a feed item, a file) this menu acts on, so
    /// it stays visible as the target while the menu is open. Cleared on
    /// close.
    pub fn anchor_row(&mut self, row: Rect) {
        self.anchor_row = Some(row);
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
        self.opened_under = Some(Point::new(f64::from(position[0]), f64::from(position[1])));
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
        self.opened_under = None;
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
        let style = theme.text(TextRole::Body);
        let mut width: f64 = 0.0;
        let mut height: f64 = 0.0;
        let has_leading_gutter = self.has_leading_gutter();
        let mut trailing: f64 = 0.0;
        for item in &self.items {
            if let Some(label) = item.label() {
                let (w, h) = text.measure_style(label, None, &style);
                let check_width = if has_leading_gutter {
                    f64::from(theme.tokens.metrics.icon_sm + theme.tokens.spacing.s2)
                } else {
                    0.0
                };
                // A shortcut sits at the row's end, a gap after the label.
                let keys = item.shortcut().map_or(0.0, |keys| {
                    let (k, _) = text.measure_style(&keys.label(self.platform), None, &style);
                    f64::from(k) + f64::from(theme.tokens.spacing.s6)
                });
                width = width.max(f64::from(w) + check_width + keys);
                height = height.max(f64::from(h));
            }
            if let Some(shortcut) = item.shortcut() {
                let label = shortcut.label(self.platform);
                let (w, _) = text.measure_style(&label, None, &style);
                trailing = trailing.max(f64::from(w));
            }
            if item.submenu_items().is_some() {
                trailing = trailing.max(f64::from(theme.tokens.metrics.icon_sm));
            }
        }
        // The shortcut column stands `s6` clear of the longest label.
        self.trailing_width = if trailing > 0.0 {
            trailing + f64::from(theme.tokens.spacing.s6)
        } else {
            0.0
        };
        // The content's inset on both sides; the row is the menu row height,
        // grown with `s2` of air round the line at large text.
        // However short its labels, a menu stays a menu's width.
        self.measured_width =
            (width + self.trailing_width + 2.0 * label_inset(theme)).max(f64::from(
                theme.tokens.metrics.menu_min_width
                    * crate::tokens::Metrics::text_growth(theme.text_scale),
            ));
        self.row_height = (height + f64::from(theme.tokens.spacing.s2))
            .max(f64::from(theme.tokens.metrics.menu_row_height));
        self.layout_at_position();
        self.ensure_hover_visible();
    }

    /// Height of one row. Action rows take the measured height once a paint
    /// has measured the labels (`ROW_HEIGHT` until then, and never less), in
    /// both the anchored and the point-placed mode — a point-placed menu at
    /// a large text setting otherwise paints two-line-tall glyphs into
    /// 28 px rows.
    fn item_height(&self, index: usize) -> f64 {
        match self.items.get(index) {
            Some(MenuItem::Action { .. }) => self.row_height,
            Some(MenuItem::Separator) if item_shown(&self.items, index) => SEPARATOR_HEIGHT,
            Some(MenuItem::Separator) | None => 0.0,
        }
    }

    /// Width of a point-placed (`open_at`) menu: the fixed reference width,
    /// grown to fit the measured labels so nothing is clipped at a large
    /// text setting. Anchored menus hug their labels instead.
    fn legacy_width(&self) -> f64 {
        self.measured_width.max(MENU_WIDTH)
    }

    fn content_height(&self) -> f64 {
        (0..self.items.len())
            .map(|index| self.item_height(index))
            .sum()
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
        let top =
            body.y0 - self.scroll_offset + (0..index).map(|i| self.item_height(i)).sum::<f64>();
        let rect = Rect::new(body.x0, top, body.x1, top + self.item_height(index)).intersect(body);
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
            let top = (0..index).map(|i| self.item_height(i)).sum::<f64>();
            let height = self.item_height(index);
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
            // Every anchored floating panel keeps the same air from the
            // window's edges (`floating_surface::viewport_margin`, the `s2`
            // step), popovers and menus alike. A window too small for the
            // menu and its air gives up the air first, on that axis only.
            let full = self.content_height() + PADDING * 2.0;
            let margin_x = PADDING
                .min((viewport.width() - self.measured_width) / 2.0)
                .max(0.0);
            let margin_y = PADDING.min((viewport.height() - full) / 2.0).max(0.0);
            let viewport = Rect::new(
                viewport.x0 + margin_x,
                viewport.y0 + margin_y,
                viewport.x1 - margin_x,
                viewport.y1 - margin_y,
            );
            let width = self.measured_width.min(viewport.width().max(0.0));
            // A pointer position opens the menu right at it; a control
            // opens it `s1` off its edge.
            let gap = if anchor.width() > 0.0 && anchor.height() > 0.0 {
                ANCHOR_GAP
            } else {
                0.0
            };
            if matches!(self.side, PopoverSide::Left | PopoverSide::Right) {
                // A nested menu opens beside its row, its first row level
                // with it: the panel starts one inset above the row.
                let height = full.min(viewport.height().max(0.0));
                let row = anchor.inflate(0.0, PADDING);
                self.rect = floating_surface::place_aligned(
                    row,
                    (width, height),
                    self.side,
                    PanelAlign::Leading,
                    0.0,
                    viewport,
                )
                .0;
                self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll());
                return;
            }
            let below = floating_surface::anchored_room(anchor, PopoverSide::Below, gap, viewport);
            let above = floating_surface::anchored_room(anchor, PopoverSide::Above, gap, viewport);
            // Taller than the room on its side, a menu opened from a control
            // takes the side with more room and scrolls its rows, so the
            // control stays in view. A menu opened at the pointer has no
            // control to keep clear: where neither side holds all of it, it
            // covers the point and takes as much of the viewport as it needs,
            // as it does when neither side holds a row.
            let room = if below >= full {
                below
            } else {
                below.max(above)
            };
            let one_row = (self.row_height + PADDING * 2.0).min(full);
            let beside = if gap > 0.0 {
                room >= one_row
            } else {
                room >= full
            };
            let rect = if beside {
                let height = full.min(room).min(viewport.height().max(0.0));
                floating_surface::place_aligned(
                    anchor,
                    (width, height),
                    PopoverSide::Below,
                    self.align,
                    gap,
                    viewport,
                )
                .0
            } else {
                let height = full.min(viewport.height().max(0.0));
                let x = anchor
                    .x0
                    .clamp(viewport.x0, (viewport.x1 - width).max(viewport.x0));
                let y = anchor
                    .y0
                    .clamp(viewport.y0, (viewport.y1 - height).max(viewport.y0));
                Rect::new(x, y, x + width, y + height)
            };
            self.rect = rect;
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
        self.opened_under = None;
        self.active_touch = None;
        self.anchor_row = None;
        self.submenu = None;
        self.submenu_focused = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Every pixel the open menu painted in its last
    /// [`Widget::draw_overlay`], in the coordinates it was drawn in: its
    /// Panel with the floating shadow, the ring on its anchor row and its
    /// open nested menu. Nothing while it is closed.
    pub fn painted_bounds(&self, theme: &Theme) -> Vec<Rect> {
        if !self.open {
            return Vec::new();
        }
        // `floating_surface::paint_rounded` clips the shadow to this reach;
        // two more pixels cover the panel's edge stroke and antialiasing.
        let elevation = &theme.tokens.elevation.floating;
        let reach = f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs() + 2.0;
        let mut bounds = vec![self.rect.inflate(reach, reach)];
        if let Some(row) = self.anchor_row {
            // A 2 px stroke on the row's outline.
            bounds.push(row.inflate(2.0, 2.0));
        }
        if let Some((_, nested)) = &self.submenu {
            bounds.extend(nested.painted_bounds(theme));
        }
        bounds
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
                submenu: Some(_),
                enabled: true,
                ..
            } => self.open_submenu(idx, false),
            MenuItem::Action {
                action,
                enabled: true,
                ..
            } => {
                let cb = action.clone();
                cb();
                self.selection = Some(idx);
                self.close();
            }
            MenuItem::Action { .. } | MenuItem::Separator => {}
        }
    }

    /// Open the nested menu of row `idx` beside it. `keyboard` puts focus
    /// on its first row.
    pub fn open_submenu(&mut self, idx: usize, keyboard: bool) {
        if self.submenu.as_ref().is_some_and(|(row, _)| *row == idx) {
            if keyboard && let Some((_, menu)) = self.submenu.as_mut() {
                menu.focus_first_action();
                self.submenu_focused = true;
            }
            return;
        }
        let Some(items) = self.items.get(idx).and_then(MenuItem::submenu_items) else {
            self.close_submenu();
            return;
        };
        let Some(row) = self.item_rect(idx) else {
            return;
        };
        let viewport = self
            .bounds
            .map(|(_, viewport)| viewport)
            .or(self.viewport)
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        let mut menu = ContextMenu::with_items(items.to_vec());
        menu.side = if self.side == PopoverSide::Left {
            PopoverSide::Left
        } else {
            PopoverSide::Right
        };
        menu.platform = self.platform;
        menu.text_scale = self.text_scale;
        menu.row_height = self.row_height;
        menu.open_anchored(
            Rect::new(self.rect.x0, row.y0, self.rect.x1, row.y1),
            viewport,
        );
        if keyboard {
            menu.focus_first_action();
        }
        self.hovered = Some(idx);
        self.submenu_focused = keyboard;
        self.submenu = Some((idx, Box::new(menu)));
        // Hover opens it without marking the move handled.
        crate::widget::note_resource_presentation_changed();
    }

    /// Close the nested menu, keeping this one open.
    pub fn close_submenu(&mut self) {
        if self.submenu.take().is_some() {
            // Hover closes it without marking the move handled.
            crate::widget::note_resource_presentation_changed();
        }
        self.submenu_focused = false;
    }

    /// Whether a nested menu is open.
    pub fn is_submenu_open(&self) -> bool {
        self.submenu.is_some()
    }

    /// A pick in a nested menu as `(row, nested row)`, exactly once. The
    /// nested row's action has already run.
    pub fn take_submenu_selection(&mut self) -> Option<(usize, usize)> {
        self.submenu_selection.take()
    }

    /// Give `event` to the open nested menu when it is the pointer's or the
    /// keyboard's: the pointer while over it, keys while it has focus.
    /// Returns whether it took the event.
    fn route_to_submenu(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if self.submenu.is_none() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let p = self.last_cursor;
        let over = self
            .submenu
            .as_ref()
            .is_some_and(|(_, menu)| menu.contains([p.x, p.y]));
        let key = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => Some(logical_key),
            _ => None,
        };
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if let Some(key) = key {
            if !self.submenu_focused {
                return false;
            }
            // Left or Escape steps back out to this menu's row.
            if matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::Escape)) {
                self.close_submenu();
                *ctx.handled = true;
                return true;
            }
        } else if !(pointer && over) {
            return false;
        }
        if let Some((_, menu)) = self.submenu.as_mut() {
            menu.handle_event(event, ctx);
        }
        if pointer {
            self.submenu_focused = false;
        }
        self.drain_submenu();
        *ctx.handled |= !matches!(event, WindowEvent::CursorMoved { .. });
        true
    }

    /// Drain a pick the nested menu just made: the whole menu closes.
    fn drain_submenu(&mut self) {
        let Some((row, menu)) = self.submenu.as_mut() else {
            return;
        };
        let row = *row;
        if let Some(pick) = menu.take_selection() {
            self.submenu_selection = Some((row, pick));
            self.close();
        } else if !menu.is_open() {
            self.close_submenu();
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

    /// An unchanged opening pointer was replayed by the host. Keep its
    /// hit-testing position without replacing the menu's keyboard focus.
    pub(crate) fn remember_pointer(&mut self, point: Point) {
        self.last_cursor = point;
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
            crate::tokens::colors::Colors::hairline_on(
                theme.tokens.colors.surfaces.overlay,
                crate::chrome::GLOW_ALPHA.saturating_mul(2),
            ),
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

/// The reference row height at 1× (`metrics.menu_row_height`), used until a
/// paint has measured the labels against the theme.
const ROW_HEIGHT: f64 = crate::tokens::Metrics::BASE.menu_row_height as f64;
const SEPARATOR_HEIGHT: f64 = 9.0;
/// The air between a control and the menu it opened: the `s1` step.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const ANCHOR_GAP: f64 = crate::tokens::Spacing::BASE.s1 as f64;
const MENU_WIDTH: f64 = 220.0;
/// How far, in logical pixels, the pointer may be from where the menu opened
/// and still count as not having moved: the opening position went through
/// `f32`, the pointer's did not.
const STILL_POINTER: f64 = 0.5;
/// The panel's vertical inset around its rows: the `s2` menu inset
/// (`floating_surface::menu_inset`), so the armed row runs concentric with
/// the panel at the top and bottom too.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const PADDING: f64 = crate::tokens::Spacing::BASE.s2 as f64;

/// Distance from the panel's edge to a row's content, on both sides.
fn label_inset(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s3)
}

/// Whether item `index` is painted: every action is, and a separator only
/// between two actions and never next to another separator, so a menu built
/// from optional sections has no leading, trailing or doubled rule.
fn item_shown(items: &[MenuItem], index: usize) -> bool {
    match items.get(index) {
        Some(MenuItem::Action { .. }) => true,
        Some(MenuItem::Separator) => {
            let before = items.get(..index).unwrap_or_default();
            let after = items.get(index.saturating_add(1)..).unwrap_or_default();
            let action_before = before
                .iter()
                .rev()
                .take_while(|item| matches!(item, MenuItem::Action { .. }))
                .count()
                > 0;
            let action_after = after
                .iter()
                .any(|item| matches!(item, MenuItem::Action { .. }));
            action_before && action_after
        }
        None => false,
    }
}

impl Widget for ContextMenu {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
    }

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
        let style = Style {
            size: Size {
                width: Dimension::length(self.legacy_width() as f32),
                height: Dimension::length(height as f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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

    /// A menu paints nothing in place: it is a floating Panel in the
    /// window's top layer ([`Widget::draw_overlay`]), so no later sibling,
    /// pane or chrome covers it.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.set_text_scale(theme.text_scale);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.set_text_scale(theme.text_scale);
        if !self.open {
            return;
        }
        // A menu opened at a point with no viewport of its own flips and
        // shifts inside the window it is presented in.
        if self.bounds.is_none()
            && self.viewport.is_none()
            && let Some(window) = presence.viewport.get()
        {
            self.set_viewport(Some(window));
        }

        // Measure in both modes: an anchored menu hugs its labels, a
        // point-placed one keeps its origin and reference width but grows
        // its rows (and, past the reference width, its width) to the text.
        self.layout_within(text_renderer, theme);
        // The row the menu acts on stays marked while it is open.
        if let Some(row) = self.anchor_row {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &crate::surface::edge_outline(row, f64::from(theme.tokens.radii.item)),
            );
        }
        // Panel role (HIG §9.1): the floating radius, the one floating
        // shadow, and the floating material with its derived edge.
        // One row sits in a panel concentric with it, never a near-pill.
        let single_row = self
            .items
            .iter()
            .filter(|item| matches!(item, MenuItem::Action { .. }))
            .count()
            == 1;
        let panel_radius = if single_row {
            floating_surface::single_row_radius(theme)
        } else {
            floating_surface::radius(theme)
        };
        floating_surface::paint_rounded(scene, transform, self.rect, panel_radius, theme);
        let ground = crate::chrome::panel_ground(theme);

        // One style for measuring (`layout_within`) and painting, in both
        // modes: the point-placed path used to paint the theme's already
        // text-scaled size through the renderer's own multiplier, so its
        // glyphs came out at 4× on a 200 % setting while its rows were
        // measured at 2×.
        let style = theme.text(TextRole::Body);
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
            let row_height = self.item_height(idx);
            match item {
                MenuItem::Action {
                    label,
                    icon,
                    checked,
                    destructive,
                    enabled,
                    shortcut,
                    submenu,
                    ..
                } => {
                    // The row Enter / a click would activate, painted as every
                    // menu paints it: the panel's strong lift
                    // (`floating_surface::armed_row`). A one-step 10 % veneer
                    // measured only ΔL* ≈ 11 on the dark panel and read as
                    // "no highlight"; see `tests/menu_highlight_render.rs`.
                    // The row whose nested menu is open stays armed.
                    let armed = *enabled
                        && (self.hovered == Some(idx)
                            || self.submenu.as_ref().is_some_and(|(row, _)| *row == idx));
                    // Concentric with the panel it is inset in, as in every
                    // menu.
                    let armed_ink = armed.then(|| {
                        let inset = floating_surface::menu_inset(theme);
                        floating_surface::paint_armed_row_in(
                            scene,
                            transform,
                            Rect::new(
                                self.rect.x0 + inset,
                                y,
                                self.rect.x1 - inset,
                                y + row_height,
                            ),
                            panel_radius,
                            inset,
                            theme,
                        )
                    });
                    let colors = &theme.tokens.colors;
                    let ink = if !*enabled {
                        colors
                            .on_surface_at(ground, crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST)
                            .ink_dim
                    } else if let Some(ink) = armed_ink {
                        ink
                    } else if *destructive {
                        colors.ink_on_any(
                            &[ground],
                            colors.semantic.danger,
                            crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
                        )
                    } else {
                        colors.text_primary
                    };
                    let label_x = if has_leading_gutter {
                        let icon_size = f64::from(theme.tokens.metrics.icon_sm);
                        // A state row shows its state, not its icon: the
                        // check is what says a toggle is on.
                        let glyph = match checked {
                            Some(on) => on.then_some(Icon::Check),
                            None => *icon,
                        };
                        if let Some(icon) = glyph {
                            draw_icon(
                                scene,
                                transform,
                                Rect::from_origin_size(
                                    (label_x, y + (row_height - icon_size) / 2.0),
                                    (icon_size, icon_size),
                                ),
                                icon,
                                ink,
                                crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                            );
                        }
                        label_x + icon_size + f64::from(theme.tokens.spacing.s2)
                    } else {
                        label_x
                    };
                    let label = text_renderer.elide_to_width_style(
                        label,
                        (self.rect.x1 - label_x - label_inset(theme) - self.trailing_width).max(1.0)
                            as f32,
                        &style,
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
                    // The trailing column: a nested menu's chevron, or the
                    // shortcut in secondary ink.
                    let trailing_x1 = self.rect.x1 - label_inset(theme);
                    if submenu.is_some() {
                        let size = f64::from(theme.tokens.metrics.icon_sm);
                        draw_icon(
                            scene,
                            transform,
                            Rect::from_origin_size(
                                (trailing_x1 - size, y + (row_height - size) / 2.0),
                                (size, size),
                            ),
                            Icon::ChevronRight,
                            ink,
                            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                        );
                    } else if let Some(shortcut) = shortcut {
                        let text = shortcut.label(self.platform);
                        let (w, _) = text_renderer.measure_style(&text, None, &style);
                        let dim = if armed {
                            ink
                        } else {
                            theme
                                .tokens
                                .colors
                                .on_surface_at(
                                    ground,
                                    crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
                                )
                                .ink_dim
                        };
                        text_renderer.draw_style(
                            scene,
                            &text,
                            dim,
                            transform.pre_translate(Vec2::new(trailing_x1 - f64::from(w), label_y)),
                            None,
                            &style,
                        );
                    }
                    y += row_height;
                }
                MenuItem::Separator if row_height <= 0.0 => {}
                MenuItem::Separator => {
                    let sep_y = y + SEPARATOR_HEIGHT / 2.0;
                    crate::surface::separator(
                        scene,
                        transform,
                        self.rect.x0,
                        self.rect.x1,
                        sep_y,
                        separator_inset,
                        ground,
                    );
                    y += SEPARATOR_HEIGHT;
                }
            }
        }
        scene.pop_layer();
        self.draw_scroll_affordance(scene, transform, theme);
        if let Some((_, menu)) = self.submenu.as_mut() {
            menu.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        if self.route_to_submenu(event, ctx) {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // Hover follows the pointer once it moves; until then the
                // row the menu opened on (or the keyboard moved to) stays.
                if self
                    .opened_under
                    .is_some_and(|at| at.distance(self.last_cursor) <= STILL_POINTER)
                {
                    return;
                }
                self.opened_under = None;
                self.hovered = self
                    .item_at([self.last_cursor.x, self.last_cursor.y])
                    .filter(|index| self.items[*index].is_enabled());
                // Hovering a row with a nested menu opens it; hovering
                // another row closes it.
                if let Some(row) = self.hovered {
                    if self
                        .items
                        .get(row)
                        .and_then(MenuItem::submenu_items)
                        .is_some()
                    {
                        self.open_submenu(row, false);
                    } else if self.submenu.is_some() {
                        self.close_submenu();
                    }
                }
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
                Key::Named(NamedKey::Enter | NamedKey::ArrowRight) => {
                    let opens = self.hovered.filter(|&idx| {
                        self.items
                            .get(idx)
                            .and_then(MenuItem::submenu_items)
                            .is_some()
                    });
                    if let Some(idx) = opens {
                        self.open_submenu(idx, true);
                    } else if let (Key::Named(NamedKey::Enter), Some(idx)) = (key, self.hovered) {
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
                    shortcut,
                    submenu,
                    ..
                } = item
                else {
                    return None;
                };
                action_position = action_position.saturating_add(1);
                let open_here = self.submenu.as_ref().filter(|(row, _)| *row == index);
                let mut node = AccessNode::new(Role::MenuItem)
                    .with_name(label.clone())
                    .with_action_target(format!("context-menu:item:{index}"))
                    .with_bounds(self.item_rect(index).unwrap_or(Rect::ZERO))
                    .with_position(action_position, action_count)
                    .with_state(AccessState {
                        focusable: *enabled,
                        focused: self.open && self.hovered == Some(index) && !self.submenu_focused,
                        hidden: !self.open,
                        checked: *checked,
                        disabled: !*enabled,
                        expanded: submenu.as_ref().map(|_| open_here.is_some()),
                        ..Default::default()
                    });
                if let Some(shortcut) = shortcut {
                    node.description = Some(shortcut.label(self.platform));
                }
                if let Some((_, menu)) = open_here
                    && let Some(nested) = Widget::accessibility(menu.as_ref())
                {
                    node.children.push(nested);
                }
                Some(node)
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
        _state: &mut crate::widget::Edits<'_>,
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

/// A button that opens a menu anchored to its own rect
/// ([`crate::widget::Button::with_menu`]): a header's More button, a
/// toolbar's sort menu. The menu opens under the button, lines up with
/// whichever of its edges keeps it in the window, and flips above near the
/// bottom; Enter, Space or a click opens it, and the button reads as
/// expanded while it is open.
pub struct MenuButton {
    button: crate::widget::Button,
    menu: ContextMenu,
    /// Set by the button's click; the menu opens on the next turn.
    request: std::rc::Rc<std::cell::Cell<bool>>,
    /// Pointer opening leaves selection under the pointer; keyboard and
    /// semantic activation start at the first action.
    request_keyboard: bool,
    /// The window the top layer last painted over.
    window: Option<Rect>,
    align: PanelAlign,
}

impl crate::widget::Button {
    /// Make this button open `items` as a menu anchored to its own rect.
    #[must_use]
    pub fn with_menu(self, items: Vec<MenuItem>) -> MenuButton {
        let request = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = std::rc::Rc::clone(&request);
        MenuButton {
            button: self.on_view(move || flag.set(true)),
            menu: ContextMenu::with_items(items),
            request,
            request_keyboard: true,
            window: None,
            align: PanelAlign::Leading,
        }
    }
}

impl MenuButton {
    /// Line the menu up with the button's trailing edge (a button at the
    /// end of a header row) instead of its leading edge.
    #[must_use]
    pub fn aligned(mut self, align: PanelAlign) -> Self {
        self.align = align;
        self.menu.set_align(align);
        self
    }

    /// Replace the menu's rows (a state row's check, a disabled command).
    pub fn set_items(&mut self, items: Vec<MenuItem>) {
        self.menu.set_items(items);
    }

    /// The menu, for its selection channel
    /// ([`ContextMenu::take_selection`]).
    pub fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    pub fn button(&self) -> &crate::widget::Button {
        &self.button
    }

    pub fn button_mut(&mut self) -> &mut crate::widget::Button {
        &mut self.button
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Open the menu at the button (a stage hook, a shortcut).
    pub fn open(&mut self, keyboard: bool) {
        let anchor = Widget::get_rect(&self.button);
        let viewport = self
            .window
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        self.menu.set_align(self.align);
        self.menu.open_anchored(anchor, viewport);
        if keyboard {
            self.menu.focus_first_action();
        }
    }

    pub fn close(&mut self) {
        self.menu.close();
    }

    fn take_request(&mut self, keyboard: bool) {
        if self.request.replace(false) {
            if self.menu.is_open() {
                self.menu.close();
            } else {
                self.open(keyboard);
            }
            // A later admitted semantic action has no raw pointer event.
            self.request_keyboard = true;
        }
    }
}

impl crate::widget::Decorator for MenuButton {
    type Inner = crate::widget::Button;

    fn inner(&self) -> &crate::widget::Button {
        &self.button
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Button {
        &mut self.button
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        Widget::sync(&mut self.button, state, path);
        // Admitted view actions execute after event dispatch. Consume their
        // one-shot request on the same real sync path as the next frame.
        self.take_request(self.request_keyboard);
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        Widget::set_platform_conventions(&mut self.button, conventions);
        Widget::set_platform_conventions(&mut self.menu, conventions);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.button, taffy, theme, node_map);
        // Focus and keyboard activation reach the menu button, not the
        // button inside it.
        crate::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.menu.is_open() {
            self.menu.handle_event(event, ctx);
            // The press that closed the menu on the button itself must not
            // open it again.
            if !self.menu.is_open() {
                self.request.set(false);
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::Touch(_) => self.request_keyboard = false,
            WindowEvent::KeyboardInput { .. } => self.request_keyboard = true,
            _ => {}
        }
        Widget::handle_event(&mut self.button, event, ctx);
        self.take_request(self.request_keyboard);
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        if matches!(action, crate::a11y::Action::Activate) && Widget::is_enabled(&self.button) {
            if self.menu.is_open() {
                self.menu.close();
            } else {
                self.open(true);
            }
            return true;
        }
        Widget::perform_action(&mut self.button, action, state, root)
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(
            &mut self.button,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        if let Some(window) = presence.viewport.get() {
            self.window = Some(window);
        }
        if self.menu.is_open()
            && let Some(window) = self.window
        {
            self.menu.reanchor(Widget::get_rect(&self.button), window);
        }
        self.menu
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    // A host reaches its menu button in a tree it does not own (a shell
    // header) to refresh the rows or open it from a stage hook.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || Widget::wants_pointer_capture(&self.button)
    }

    fn needs_redraw(&self) -> bool {
        self.menu.is_open() || Widget::needs_redraw(&self.button)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = Widget::accessibility(&self.button)?;
        node.state.expanded = Some(self.menu.is_open());
        if self.menu.is_open()
            && let Some(menu) = Widget::accessibility(&self.menu)
        {
            node.children.push(menu);
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.menu
            .perform_semantic_action(target, action, state, path)
            || Widget::perform_semantic_action(&mut self.button, target, action, state, path)
    }

    fn name(&self) -> &str {
        "MenuButton"
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
            menu.push(MenuItem::action("List", || {}).with_checked(true));
            menu.push(MenuItem::action("Grid", || {}).with_checked(false));
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
    use crate::synthetic_input::mouse_press;
    use crate::synthetic_input::named_key;
    use crate::testing::dispatch;
    use automerge::AutoCommit;
    use std::sync::Arc;

    /// A menu is a floating Panel in the window's top layer: `draw` (the
    /// in-flow pass, where later siblings paint over it) paints nothing, and
    /// `draw_overlay` (run after the app and the window's chrome) paints it.
    #[test]
    fn an_open_menu_paints_only_in_the_top_layer() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        menu.push(MenuItem::action("Rename", || {}));
        menu.open_at([40.0, 40.0]);
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut in_flow = Scene::new();
        menu.draw(&mut in_flow, Affine::IDENTITY, &mut text, &theme, &presence);
        assert!(
            in_flow.encoding().is_empty(),
            "the in-flow pass paints nothing"
        );
        let mut top = Scene::new();
        menu.draw_overlay(&mut top, Affine::IDENTITY, &mut text, &theme, &presence);
        assert!(!top.encoding().is_empty(), "the top layer paints the menu");
    }

    /// Opened from a control, the menu hangs `s1` under it with its
    /// leading edge on the control's; at the window's trailing edge it lines
    /// up with the control's trailing edge, and near the bottom it opens
    /// above. A point-placed menu flips inside the window it is presented
    /// in.
    #[test]
    fn a_menu_opens_at_its_trigger_and_flips_at_the_window_edges() {
        let window = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let menu_with = || {
            let mut menu = ContextMenu::new();
            for label in ["New child note", "Rename", "Duplicate", "Delete"] {
                menu.push(MenuItem::action(label, || {}));
            }
            menu
        };

        let button = Rect::new(300.0, 16.0, 332.0, 48.0);
        let mut menu = menu_with();
        menu.open_anchored(button, window);
        menu.layout_within(&mut text, &theme);
        let rect = menu.get_rect();
        assert!((rect.x0 - button.x0).abs() < 1e-9, "{rect:?}");
        assert!(
            (rect.y0 - (button.y1 + ANCHOR_GAP)).abs() < 1e-9,
            "{rect:?}"
        );

        let more = Rect::new(1236.0, 16.0, 1268.0, 48.0);
        let mut menu = menu_with();
        menu.open_anchored(more, window);
        menu.layout_within(&mut text, &theme);
        let rect = menu.get_rect();
        assert!(
            (rect.x1 - more.x1).abs() < 1e-9,
            "trailing-aligned: {rect:?}"
        );
        assert!(rect.y0 >= more.y1, "under its trigger: {rect:?}");

        let low = Rect::new(600.0, 760.0, 632.0, 792.0);
        let mut menu = menu_with();
        menu.open_anchored(low, window);
        menu.layout_within(&mut text, &theme);
        let rect = menu.get_rect();
        assert!(rect.y1 <= low.y0, "flipped above: {rect:?}");

        let mut menu = menu_with();
        menu.open_at([1270.0, 790.0]);
        let presence = PresenceCtx::default();
        presence.viewport.set(Some(window));
        let mut scene = Scene::new();
        menu.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        let rect = menu.get_rect();
        assert!(
            rect.x0 >= window.x0
                && rect.x1 <= window.x1
                && rect.y0 >= window.y0
                && rect.y1 <= window.y1,
            "a menu at the bottom-trailing corner stays in the window: {rect:?}"
        );
    }

    /// A menu assembled from optional sections paints no leading, trailing
    /// or doubled separator, and its rows are the menu row height.
    #[test]
    fn separators_only_stand_between_actions() {
        let mut menu = ContextMenu::new();
        for item in [
            MenuItem::separator(),
            MenuItem::action("Copy", || {}),
            MenuItem::separator(),
            MenuItem::separator(),
            MenuItem::action("Paste", || {}),
            MenuItem::separator(),
        ] {
            menu.push(item);
        }
        let shown: Vec<bool> = (0..menu.items.len())
            .map(|index| item_shown(&menu.items, index))
            .collect();
        assert_eq!(shown, [false, true, true, false, true, false]);
        assert_eq!(
            menu.content_height(),
            2.0 * ROW_HEIGHT + SEPARATOR_HEIGHT,
            "hidden separators take no room"
        );
        let theme = Theme::dark();
        menu.open_anchored(
            Rect::new(10.0, 10.0, 10.0, 10.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        menu.layout_within(&mut TextRenderer::new(), &theme);
        assert_eq!(
            menu.row_height,
            f64::from(theme.tokens.metrics.menu_row_height)
        );
    }

    #[test]
    fn a_state_row_carries_its_checked_state_and_an_anchor_row_clears_on_close() {
        let item = MenuItem::action("Copy on select", || {}).with_checked(true);
        assert_eq!(item.checked(), Some(true));
        let mut menu = ContextMenu::new();
        menu.push(item);
        menu.anchor_row(Rect::new(0.0, 40.0, 300.0, 80.0));
        menu.open_at([20.0, 60.0]);
        assert!(menu.anchor_row.is_some());
        menu.close();
        assert!(menu.anchor_row.is_none());
    }
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use winit::event::TouchPhase;

    /// A shared counter the rows' callbacks bump.
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

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn touch(phase: TouchPhase, id: u64, point: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(point.x, point.y),
            force: None,
            id,
        })
    }
    fn wheel_lines(y: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, y),
            phase: TouchPhase::Moved,
        }
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
            let handled = dispatch(&mut m, &mouse_press());
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
            let handled = dispatch(&mut m, &mouse_press());
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
            let handled = dispatch(&mut m, &named_key(NamedKey::Escape));
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
            dispatch(&mut m, &named_key(NamedKey::ArrowDown));
            assert_eq!(m.hovered(), Some(0));
            // Down again → must skip the separator (idx 1) and land on idx 2.
            dispatch(&mut m, &named_key(NamedKey::ArrowDown));
            assert_eq!(m.hovered(), Some(2), "ArrowDown skips the separator");
            // Enter selects the hovered row.
            dispatch(&mut m, &named_key(NamedKey::Enter));
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
            assert!(dispatch(&mut menu, &mouse_press()));
            assert_eq!(fired.load(Ordering::SeqCst), 0);
            assert_eq!(menu.take_selection(), None);
            assert!(menu.is_open(), "a rejected activation keeps the menu open");
        }

        fn keyboard_navigation_skips_disabled_rows() {
            let (mut menu, fired) = open_menu();
            menu.set_item_enabled(0, false);

            dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
            assert_eq!(menu.hovered(), Some(2));
            dispatch(&mut menu, &named_key(NamedKey::Enter));

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
                &mut crate::widget::Edits::lend(&mut doc),
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
                &mut crate::widget::Edits::lend(&mut doc),
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
                device_id: winit::event::DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, y),
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(dispatch(&mut menu, &wheel(-100.0)));
            assert_eq!(menu.scroll_offset, menu.max_scroll());
            let last = menu.item_rect(19).unwrap().center();
            assert_eq!(menu.item_at([last.x, last.y]), Some(19));
            dispatch(&mut menu, &cursor_at(last));
            dispatch(&mut menu, &mouse_press());
            assert_eq!(menu.take_selection(), Some(19));
            assert_eq!(menu.take_selection(), None);
            menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
            menu.focus_first_action();
            for _ in 0..19 {
                dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
            }
            assert_eq!(menu.hovered(), Some(19));
            assert!(menu.item_rect(19).is_some());
            dispatch(&mut menu, &named_key(NamedKey::Enter));
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
            m.draw_overlay(
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
            m.draw_overlay(
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
                    (run.font_size - theme.tokens.typography.size(TextRole::Body)).abs() < 0.01,
                    "glyphs painted at {} px; the theme's scaled size is {}",
                    run.font_size,
                    theme.tokens.typography.size(TextRole::Body),
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
            narrow.push(MenuItem::action("i".repeat(40), || {}));
            let mut wide = ContextMenu::new();
            wide.push(MenuItem::action("W".repeat(40), || {}));
            let mut text = TextRenderer::new();
            for menu in [&mut narrow, &mut wide] {
                menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 1000.0, 1000.0));
                menu.layout_within(&mut text, &Theme::dark());
            }
            assert!(wide.get_rect().width() > narrow.get_rect().width());
            // However short its one label, a menu keeps a menu's width.
            let min = f64::from(Theme::dark().tokens.metrics.menu_min_width);
            assert!(narrow.get_rect().width() >= min - 0.5);
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

#[cfg(test)]
mod nested_and_anchored_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    use crate::synthetic_input::mouse_press;
    use crate::synthetic_input::named_key;
    use crate::testing::dispatch;
    fn paint(m: &mut ContextMenu) {
        let presence = PresenceCtx::default();
        let previous = presence.push_viewport(WINDOW);
        let mut scene = Scene::new();
        m.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &presence,
        );
        presence.pop_viewport(previous);
    }

    fn file_menu(fired: &Arc<AtomicU32>) -> ContextMenu {
        let markdown = Arc::clone(fired);
        ContextMenu::with_items(vec![
            MenuItem::action("Open", || {}).with_shortcut(crate::platform::Shortcut::command("O")),
            MenuItem::submenu(
                "New file",
                vec![
                    MenuItem::action("Text file", || {}),
                    MenuItem::action("Markdown file", move || {
                        markdown.fetch_add(1, Ordering::SeqCst);
                    }),
                ],
            ),
            MenuItem::action("Delete", || {})
                .with_shortcut(crate::platform::Shortcut::key("Delete")),
        ])
    }

    #[test]
    fn a_submenu_opens_beside_its_row_by_hover_and_by_keyboard() {
        let fired = Arc::new(AtomicU32::new(0));
        let mut m = file_menu(&fired);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        // Hovering the row opens it beside the row, level with it.
        let row = m.item_rect(1).expect("the submenu row is shown");
        dispatch(&mut m, &cursor_at(row.center()));
        assert!(m.is_submenu_open(), "hover opens the nested menu");
        paint(&mut m);
        let nested = m.submenu.as_ref().map(|(_, menu)| menu.get_rect()).unwrap();
        assert!(
            nested.x0 >= m.get_rect().x1 - 0.5,
            "beside the menu: {nested:?}"
        );
        // A pick in it runs the action, reports the nested pick and closes all.
        let nested_row = m
            .submenu
            .as_ref()
            .and_then(|(_, menu)| menu.item_rect(1))
            .unwrap();
        dispatch(&mut m, &cursor_at(nested_row.center()));
        dispatch(&mut m, &mouse_press());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(m.take_submenu_selection(), Some((1, 1)));
        assert!(!m.is_open());

        // The keyboard: Right opens it with focus, Left steps back out.
        let mut m = file_menu(&fired);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        m.focus_item(1);
        dispatch(&mut m, &named_key(NamedKey::ArrowRight));
        assert!(m.is_submenu_open());
        assert_eq!(
            m.submenu.as_ref().and_then(|(_, menu)| menu.hovered()),
            Some(0)
        );
        dispatch(&mut m, &named_key(NamedKey::ArrowLeft));
        assert!(!m.is_submenu_open());
        assert!(m.is_open(), "Left closes only the nested menu");
        let node = Widget::accessibility(&m).unwrap();
        assert_eq!(node.children[1].state.expanded, Some(false));
        assert_eq!(node.children[0].description.as_deref(), Some("Ctrl+O"));
    }

    #[test]
    fn shortcuts_widen_the_menu_and_a_bare_key_has_no_modifier() {
        let fired = Arc::new(AtomicU32::new(0));
        // Labels long enough that the menu is past its minimum width.
        let label = "Open the most recent document";
        let mut plain = ContextMenu::with_items(vec![MenuItem::action(label, || {})]);
        let mut shortcut = ContextMenu::with_items(vec![
            MenuItem::action(label, || {}).with_shortcut(crate::platform::Shortcut::command("O")),
        ]);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        for menu in [&mut plain, &mut shortcut] {
            menu.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
            menu.layout_within(&mut tr, &theme);
        }
        assert!(
            plain.get_rect().width() > f64::from(theme.tokens.metrics.menu_min_width),
            "fixture: past the minimum"
        );
        assert!(shortcut.get_rect().width() > plain.get_rect().width());
        let mut with = file_menu(&fired);
        with.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        with.layout_within(&mut tr, &theme);
        let conventions = crate::platform::PlatformConventions::default();
        assert_eq!(
            crate::platform::Shortcut::key("Delete").label(conventions),
            "Delete"
        );
    }

    #[test]
    fn a_trailing_menu_hangs_from_its_anchors_trailing_edge_inside_the_margin() {
        let mut m = ContextMenu::with_items(vec![
            MenuItem::action("Rename", || {}),
            MenuItem::action("Duplicate", || {}),
        ]);
        m.set_align(PanelAlign::Trailing);
        let anchor = Rect::new(700.0, 10.0, 740.0, 42.0);
        m.open_anchored(anchor, WINDOW);
        paint(&mut m);
        assert!(
            (m.get_rect().x1 - anchor.x1).abs() < 1e-6,
            "{:?}",
            m.get_rect()
        );
        // Near the window's edge the menu keeps the panel margin.
        let mut edge = ContextMenu::with_items(vec![MenuItem::action("Rename", || {})]);
        edge.open_anchored(Rect::new(790.0, 10.0, 800.0, 42.0), WINDOW);
        paint(&mut edge);
        let margin = crate::widget::floating_surface::viewport_margin(&Theme::dark());
        assert!(
            edge.get_rect().x1 <= WINDOW.x1 - margin + 1e-6,
            "{:?}",
            edge.get_rect()
        );
    }

    #[test]
    fn a_single_row_menu_is_concentric_with_its_row() {
        let theme = Theme::dark();
        let single = crate::widget::floating_surface::single_row_radius(&theme);
        assert!(single < f64::from(theme.tokens.radii.panel));
        assert!(
            (single
                - (f64::from(theme.tokens.radii.item)
                    + crate::widget::floating_surface::menu_inset(&theme)))
            .abs()
                < 1e-9
        );
    }

    #[test]
    fn a_menu_button_opens_its_menu_at_its_own_rect() {
        use crate::headless::Framed;
        let fired = Arc::new(AtomicU32::new(0));
        let f = Arc::clone(&fired);
        let button =
            crate::widget::Button::icon_only("more", crate::icons::Icon::MoreHorizontal, "More")
                .with_menu(vec![MenuItem::action("Print", move || {
                    f.fetch_add(1, Ordering::SeqCst);
                })]);
        let mut framed = Framed::new(crate::State::new(), button, (800, 600), Theme::dark());
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        let audit = Framed::harness(&framed).native_app_audit();
        assert!(
            audit.is_certified(),
            "the menu opener has an admitted view action: {:?}",
            audit.issues
        );
        let rect = framed.button().get_rect();
        Framed::harness_mut(&mut framed).click_at(rect.center().x, rect.center().y);
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert!(framed.is_open(), "a click opens the menu");
        let menu = framed.menu_mut().get_rect();
        assert!(menu.y0 >= rect.y1, "under the button: {menu:?} vs {rect:?}");
        let node = Widget::accessibility(&*framed).unwrap();
        assert_eq!(node.state.expanded, Some(true));
        let print = framed.menu_mut().item_rect(0).expect("Print is laid out");
        Framed::harness_mut(&mut framed).click_at(print.center().x, print.center().y);
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the real Print item runs");
        assert!(!framed.is_open(), "choosing Print closes the menu");
        Framed::harness_mut(&mut framed)
            .perform_named("more", &crate::a11y::Action::Activate)
            .expect("assistive activation invokes the same admitted opener");
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert!(framed.is_open(), "assistive activation opens the same menu");
        assert_eq!(
            framed.menu_mut().hovered(),
            Some(0),
            "assistive opening selects the first command"
        );
        assert!(
            framed
                .menu_mut()
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|item| item.name.as_deref() == Some("Print")),
            "the same Print command is exported after assistive opening"
        );
        let audit = Framed::harness(&framed).native_app_audit();
        assert!(
            audit.is_certified(),
            "open menu retains admitted coverage: {:?}",
            audit.issues
        );
    }
}
