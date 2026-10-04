//! `Popover`: the one anchored floating Panel.
//!
//! A popover opens next to the control that summoned it, on a preferred
//! side, flipping to the opposite side when it would leave the window and
//! sliding along the other axis to stay inside it. It is a floating Panel
//! ([`super::floating_surface`]): panel radius, the blurred material, one
//! soft shadow. It paints in the window's top layer
//! ([`Widget::draw_overlay`]), so it is never clipped by, or drawn under,
//! the pane it was opened from. While open it captures input: Escape or a
//! click outside closes it, and the click does not reach what is behind.
//!
//! Two bodies:
//!
//! - An **option menu** ([`Popover::new`]): pick one of a short list of
//!   options next to the control that owns the value (a refresh cadence, a
//!   unit). The chosen option carries a check; the keyboard and pointer
//!   highlight is the lift of the panel ground, never an accent fill. Full
//!   keyboard navigation, and `Role::Menu` semantics.
//! - **Any content** ([`Popover::with_content`]): a widget laid out at its
//!   own size inside the panel's padding.
//!
//! Hosts that draw by hand drive an option menu through [`Popover::open_at`],
//! [`Popover::item_at`], [`Popover::move_highlight`], [`Popover::select`]
//! and [`Popover::take_selection`], and reach its paint through their own
//! `draw_overlay` (or by returning it from `children_mut`).

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::floating_surface;
use crate::widget::{AppModel, ModelPath, TaffyTreeExt, Widget};

/// Side of the anchor a popover prefers to open on. It flips to the
/// opposite side when the window has no room there; see
/// [`Popover::resolved_side`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PopoverSide {
    Above,
    #[default]
    Below,
    Left,
    Right,
}

impl PopoverSide {
    fn opposite(self) -> Self {
        match self {
            Self::Above => Self::Below,
            Self::Below => Self::Above,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// The body of an option menu: no content widget.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoContent;

impl Widget for NoContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
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
        "NoContent"
    }
    fn get_rect(&self) -> Rect {
        Rect::ZERO
    }
}

/// The options of an option menu and where the highlight and selection
/// are.
#[derive(Clone, Debug, Default)]
struct Options {
    items: Vec<String>,
    /// Row the keyboard / pointer highlight sits on.
    highlighted: usize,
    /// The applied option (drawn with a check), if any.
    selected: Option<usize>,
    /// The row the person just chose, drained by `take_selection`.
    pending: Option<usize>,
}

/// Geometry of an option menu, from the theme.
struct MenuMetrics {
    row: f64,
    pad: f64,
    text_x: f64,
    check: f64,
    min_width: f64,
}

impl MenuMetrics {
    fn of(theme: &Theme) -> Self {
        let t = &theme.tokens;
        let check = f64::from(t.metrics.icon_sm);
        Self {
            // A menu row, the height every menu shares (HIG §9.5).
            row: f64::from(t.metrics.menu_row_height),
            pad: crate::widget::floating_surface::menu_inset(theme),
            // Leading padding, the check column, then the label.
            text_x: f64::from(t.spacing.s3) + check + f64::from(t.spacing.s2),
            check,
            min_width: f64::from(t.metrics.panel_width_utility) / 2.0,
        }
    }
}

/// An anchored floating Panel holding an option menu or any content widget.
/// See the module docs for the contract.
pub struct Popover<C: Widget = NoContent> {
    content: C,
    options: Option<Options>,
    /// Accessible name of an option menu (e.g. "Refresh interval").
    label: Option<String>,
    open: bool,
    anchor: Rect,
    side: PopoverSide,
    resolved_side: PopoverSide,
    /// Bounds the panel stays inside. `None` uses the window the top layer
    /// paints over.
    viewport: Option<Rect>,
    /// The panel, resolved by [`Self::place`] (also run by layout and paint).
    rect: Rect,
    on_dismiss: Option<Box<dyn FnMut()>>,
    last_cursor: Point,
    layout_theme: Theme,
    /// Isolated taffy tree for the content, kept so events reach it.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
}

impl Default for Popover {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Widget> Popover<C> {
    fn with_body(content: C, options: Option<Options>) -> Self {
        Self {
            content,
            options,
            label: None,
            open: false,
            anchor: Rect::ZERO,
            side: PopoverSide::Below,
            resolved_side: PopoverSide::Below,
            viewport: None,
            rect: Rect::ZERO,
            on_dismiss: None,
            last_cursor: Point::ZERO,
            layout_theme: Theme::dark(),
            content_taffy: None,
            content_node: None,
        }
    }

    /// A closed popover showing `content` next to `anchor`.
    pub fn with_content(content: C) -> Self {
        Self::with_body(content, None)
    }

    /// Anchor the panel to `anchor` (window coordinates).
    #[must_use]
    pub fn anchored(mut self, anchor: Rect) -> Self {
        self.anchor = anchor;
        self
    }

    /// Prefer opening on `side` of the anchor (default [`PopoverSide::Below`]).
    #[must_use]
    pub fn side(mut self, side: PopoverSide) -> Self {
        self.side = side;
        self.resolved_side = side;
        self
    }

    /// Open or close at construction.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Called when the popover closes itself: Escape, or a click outside.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Keep the panel inside `viewport` rather than the window.
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.viewport = Some(viewport);
        self
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = Some(label.into());
    }

    /// [`Self::with_viewport`] after construction.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = Some(viewport);
    }

    /// Move the anchor (window coordinates).
    pub fn set_anchor(&mut self, anchor: Rect) {
        self.anchor = anchor;
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Close (safe when already closed).
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The resolved panel (valid after layout, [`Self::place`] or paint).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The side the panel opened on: the preferred one unless the window
    /// had no room there.
    pub fn resolved_side(&self) -> PopoverSide {
        self.resolved_side
    }

    pub fn contains(&self, point: Point) -> bool {
        self.open && self.rect.contains(point)
    }

    pub fn content(&self) -> &C {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    fn padding(&self, theme: &Theme) -> f64 {
        if self.options.is_some() {
            MenuMetrics::of(theme).pad
        } else {
            f64::from(theme.tokens.spacing.s4)
        }
    }

    /// Resolve the panel from the anchor, the body's size and the viewport.
    /// Callable without a paint, so a hand-drawn host or a test can hit-test
    /// right after opening.
    pub fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        self.place_in(viewport, tr, theme);
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "the option count of a menu is far below f64's exact-integer range"
    )]
    fn place_in(&mut self, viewport: Rect, tr: &mut TextRenderer, theme: &Theme) {
        let pad = self.padding(theme);
        let (body_w, body_h) = if let Some(options) = &self.options {
            let m = MenuMetrics::of(theme);
            let style = theme.text(TextRole::Body);
            let mut w = m.min_width.max(self.anchor.width() - pad * 2.0);
            for label in &options.items {
                let tw = f64::from(tr.measure_style(label, None, &style).0);
                w = w.max(m.text_x + tw + f64::from(theme.tokens.spacing.s4));
            }
            (w, options.items.len().max(1) as f64 * m.row)
        } else {
            // Layout rounds the measured size to whole pixels, so text measured
            // at its max-content width can come back a fraction short and wrap
            // when laid out in exactly that width: give it the next pixel.
            let (w, h) = self.measure_content(tr, theme);
            (w + 1.0, h)
        };
        let (w, h) = (body_w.ceil() + pad * 2.0, body_h.ceil() + pad * 2.0);
        let gap = f64::from(theme.tokens.spacing.s1);
        let preferred = self.position_on(self.side, w, h, gap);
        let side = if fits(viewport, preferred, self.side) {
            self.side
        } else {
            let flipped = self.side.opposite();
            if fits(viewport, self.position_on(flipped, w, h, gap), flipped) {
                flipped
            } else {
                self.side
            }
        };
        self.resolved_side = side;
        self.rect = clamp_into(viewport, self.position_on(side, w, h, gap));
        if self.options.is_none() {
            self.place_content(tr, theme);
        }
    }

    /// Candidate panel on `side`: leading-aligned with the anchor above or
    /// below it, top-aligned beside it.
    fn position_on(&self, side: PopoverSide, w: f64, h: f64, gap: f64) -> Rect {
        let a = self.anchor;
        match side {
            PopoverSide::Below => Rect::new(a.x0, a.y1 + gap, a.x0 + w, a.y1 + gap + h),
            PopoverSide::Above => Rect::new(a.x0, a.y0 - gap - h, a.x0 + w, a.y0 - gap),
            PopoverSide::Right => Rect::new(a.x1 + gap, a.y0, a.x1 + gap + w, a.y0 + h),
            PopoverSide::Left => Rect::new(a.x0 - gap - w, a.y0, a.x0 - gap, a.y0 + h),
        }
    }

    /// The content's own size, from its layout at max-content.
    fn measure_content(&mut self, tr: &mut TextRenderer, theme: &Theme) -> (f64, f64) {
        let available = Size {
            width: taffy::AvailableSpace::MaxContent,
            height: taffy::AvailableSpace::MaxContent,
        };
        floating_surface::DetachedContent::lay_out(&mut self.content, theme, available, tr).size()
    }

    /// Lay the content out inside the panel's padding, in its own tree.
    fn place_content(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let inner = self.rect.inset(-self.padding(theme));
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a popover's body is a bounded screen-pixel size"
        )]
        let available = Size {
            width: taffy::AvailableSpace::Definite(inner.width() as f32),
            height: taffy::AvailableSpace::Definite(inner.height() as f32),
        };
        let content =
            floating_surface::DetachedContent::lay_out(&mut self.content, theme, available, tr);
        self.content
            .apply_layout(&content.taffy, content.node, inner.origin());
        self.content_node = Some(content.node);
        self.content_taffy = Some(content.taffy);
    }

    fn dismiss(&mut self) {
        self.open = false;
        if let Some(cb) = self.on_dismiss.as_mut() {
            cb();
        }
    }

    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return false;
        };
        let mut handled = false;
        self.content.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: &mut handled,
                focus_target: ctx.focus_target,
                node_id: node,
                taffy,
            },
        );
        handled
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "idx is a row of this menu's own short option list"
    )]
    fn row_rect_in(&self, idx: usize, theme: &Theme) -> Rect {
        let m = MenuMetrics::of(theme);
        let y0 = self.rect.y0 + m.pad + idx as f64 * m.row;
        Rect::new(self.rect.x0 + m.pad, y0, self.rect.x1 - m.pad, y0 + m.row)
    }

    fn paint_options(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(options) = &self.options else {
            return;
        };
        let m = MenuMetrics::of(theme);
        let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
        let colors = &on_panel.tokens.colors;
        let style = on_panel.text(TextRole::Body);
        for (idx, label) in options.items.iter().enumerate() {
            let row = self.row_rect_in(idx, theme);
            // The armed row is inverted, as in every menu (HIG §9.3); the
            // applied option carries the check.
            let ink = if idx == options.highlighted {
                crate::widget::floating_surface::paint_armed_row(scene, transform, row, theme)
            } else {
                colors.text_primary
            };
            if options.selected == Some(idx) {
                let x = self.rect.x0 + f64::from(theme.tokens.spacing.s3);
                let cy = row.center().y;
                Self::check_mark(scene, transform, x, cy, m.check, ink);
            }
            let (_, lh) = tr.measure_style(label, None, &style);
            let ty = crate::text_metrics::centered_text_top(row.y0, row.height(), f64::from(lh));
            tr.draw_style(
                scene,
                label,
                ink,
                transform.pre_translate(Vec2::new(self.rect.x0 + m.text_x, ty)),
                None,
                &style,
            );
        }
    }

    fn check_mark(
        scene: &mut Scene,
        transform: Affine,
        x: f64,
        cy: f64,
        size: f64,
        color: vello::peniko::Color,
    ) {
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::new(x, cy - size / 2.0, x + size, cy + size / 2.0),
            crate::icons::Icon::Check,
            color,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

/// True if `candidate` fits inside `viewport` along `side`'s axis.
fn fits(viewport: Rect, candidate: Rect, side: PopoverSide) -> bool {
    match side {
        PopoverSide::Below => candidate.y1 <= viewport.y1,
        PopoverSide::Above => candidate.y0 >= viewport.y0,
        PopoverSide::Right => candidate.x1 <= viewport.x1,
        PopoverSide::Left => candidate.x0 >= viewport.x0,
    }
}

/// Slide `rect` into `viewport` (keeping its size where it fits).
fn clamp_into(viewport: Rect, rect: Rect) -> Rect {
    let dx = if rect.x1 > viewport.x1 {
        viewport.x1 - rect.x1
    } else {
        0.0
    };
    let dy = if rect.y1 > viewport.y1 {
        viewport.y1 - rect.y1
    } else {
        0.0
    };
    let moved = rect.with_origin((rect.x0 + dx, rect.y0 + dy));
    let dx = (viewport.x0 - moved.x0).max(0.0);
    let dy = (viewport.y0 - moved.y0).max(0.0);
    moved.with_origin((moved.x0 + dx, moved.y0 + dy))
}

impl Popover {
    /// A closed option menu with no options yet.
    pub fn new() -> Self {
        Self::with_body(NoContent, Some(Options::default()))
    }

    #[must_use]
    pub fn with_items<I, S>(mut self, items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.set_items(items);
        self
    }

    fn options(&self) -> &Options {
        static EMPTY: Options = Options {
            items: Vec::new(),
            highlighted: 0,
            selected: None,
            pending: None,
        };
        self.options.as_ref().unwrap_or(&EMPTY)
    }

    fn options_mut(&mut self) -> &mut Options {
        self.options.get_or_insert_with(Options::default)
    }

    /// Replace the options. Clamps the highlight into range.
    pub fn set_items<I, S>(&mut self, items: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let options = self.options_mut();
        options.items = items.into_iter().map(Into::into).collect();
        if options.highlighted >= options.items.len() {
            options.highlighted = options.items.len().saturating_sub(1);
        }
    }

    pub fn items(&self) -> &[String] {
        &self.options().items
    }

    /// Open anchored to `anchor`, with `selected` checked and highlighted
    /// (or the first row when nothing is selected).
    pub fn open_at(&mut self, anchor: Rect, selected: Option<usize>) {
        self.anchor = anchor;
        let options = self.options_mut();
        options.selected = selected.filter(|&i| i < options.items.len());
        options.highlighted = options
            .selected
            .unwrap_or(0)
            .min(options.items.len().saturating_sub(1));
        options.pending = None;
        self.open = true;
    }

    pub fn selected(&self) -> Option<usize> {
        self.options().selected
    }

    pub fn highlighted(&self) -> usize {
        self.options().highlighted
    }

    /// Move the highlight by `dir` rows, clamped to the ends.
    pub fn move_highlight(&mut self, dir: i32) {
        let options = self.options_mut();
        if options.items.is_empty() {
            return;
        }
        let max = options.items.len().saturating_sub(1);
        options.highlighted = if dir.is_negative() {
            let step = usize::try_from(dir.unsigned_abs()).unwrap_or(usize::MAX);
            options.highlighted.saturating_sub(step)
        } else {
            let step = usize::try_from(dir).unwrap_or(0);
            options.highlighted.saturating_add(step).min(max)
        };
    }

    pub fn highlight_first(&mut self) {
        self.options_mut().highlighted = 0;
    }

    pub fn highlight_last(&mut self) {
        let options = self.options_mut();
        options.highlighted = options.items.len().saturating_sub(1);
    }

    /// Record `idx` as selected and pending, and close. Out-of-range
    /// indices are ignored.
    pub fn select(&mut self, idx: usize) {
        let options = self.options_mut();
        if idx < options.items.len() {
            options.selected = Some(idx);
            options.highlighted = idx;
            options.pending = Some(idx);
            self.open = false;
        }
    }

    /// Choose the highlighted option (Enter, or a click on the row).
    pub fn select_highlighted(&mut self) {
        let idx = self.highlighted();
        self.select(idx);
    }

    /// The option the person just chose, once.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.options_mut().pending.take()
    }

    /// The option row under `point`, while open.
    pub fn item_at(&self, point: Point) -> Option<usize> {
        if !self.open || !self.rect.contains(point) {
            return None;
        }
        (0..self.items().len()).find(|&i| self.row_rect_in(i, &self.layout_theme).contains(point))
    }

    /// One option row after [`Self::place`], so a hand-drawn host or a test
    /// can synthesize a click without restating the menu's metrics.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        (self.open && index < self.items().len())
            .then(|| self.row_rect_in(index, &self.layout_theme))
    }
}

impl<C: Widget + 'static> Widget for Popover<C> {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Even when closed, so reopening shows a current frame.
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // A zero-extent leaf: the panel is placed against the anchor, not by
        // the parent's flexbox. Registered so the host's sweeps reach it.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // Resolve the panel so hit-testing works before the first paint, at
        // the active theme and text scale.
        if self.open {
            let theme = self.layout_theme.clone();
            crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.place(tr, &theme));
        }
    }

    /// Nothing: a popover is presented in the window's top layer.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        self.layout_theme = theme.clone();
        let viewport = self
            .viewport
            .or_else(|| presence.viewport.get())
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        self.place_in(viewport, text_renderer, theme);
        floating_surface::paint(scene, transform, self.rect, theme);
        if self.options.is_some() {
            self.paint_options(scene, transform, text_renderer, theme);
        } else {
            let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
            self.content
                .draw(scene, transform, text_renderer, &on_panel, presence);
            self.content
                .draw_overlay(scene, transform, text_renderer, &on_panel, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.options.is_some() {
                    let count = self.options.as_ref().map_or(0, |o| o.items.len());
                    let hit = (0..count).find(|&i| {
                        self.rect.contains(self.last_cursor)
                            && self
                                .row_rect_in(i, &self.layout_theme)
                                .contains(self.last_cursor)
                    });
                    if let (Some(idx), Some(options)) = (hit, self.options.as_mut()) {
                        options.highlighted = idx;
                    }
                } else {
                    self.forward_to_content(event, ctx);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if matches!(key, Key::Named(NamedKey::Escape)) {
                    self.dismiss();
                } else if let Some(options) = self.options.as_mut() {
                    let max = options.items.len().saturating_sub(1);
                    match key {
                        Key::Named(NamedKey::ArrowDown) => {
                            options.highlighted = options.highlighted.saturating_add(1).min(max);
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            options.highlighted = options.highlighted.saturating_sub(1);
                        }
                        Key::Named(NamedKey::Home) => options.highlighted = 0,
                        Key::Named(NamedKey::End) => options.highlighted = max,
                        Key::Named(NamedKey::Enter) if !options.items.is_empty() => {
                            let idx = options.highlighted;
                            options.selected = Some(idx);
                            options.pending = Some(idx);
                            self.open = false;
                        }
                        _ => {}
                    }
                } else {
                    self.forward_to_content(event, ctx);
                }
                // An open popover owns the keyboard: nothing leaks behind it.
                *ctx.handled = true;
            }
            WindowEvent::Ime(_) => {
                if self.options.is_none() {
                    self.forward_to_content(event, ctx);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.rect.contains(self.last_cursor) {
                    // A click outside closes; it does not also hit what is
                    // beneath.
                    self.dismiss();
                } else if let Some(options) = self.options.as_mut() {
                    let theme = &self.layout_theme;
                    let m = MenuMetrics::of(theme);
                    let rel = self.last_cursor.y - self.rect.y0 - m.pad;
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "rel is checked non-negative and bounded by the panel height"
                    )]
                    let idx = (rel >= 0.0).then(|| (rel / m.row).floor() as usize);
                    if let Some(idx) = idx.filter(|&i| i < options.items.len()) {
                        options.selected = Some(idx);
                        options.highlighted = idx;
                        options.pending = Some(idx);
                        self.open = false;
                    }
                } else {
                    self.forward_to_content(event, ctx);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => {
                if self.options.is_none() && self.rect.contains(self.last_cursor) {
                    self.forward_to_content(event, ctx);
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Popover"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// An open popover captures input, so the host's global shortcuts and
    /// click-through stay off while it is up.
    fn modal_capture(&self) -> Option<Rect> {
        self.open.then_some(self.rect)
    }

    fn traps_focus(&self) -> bool {
        self.open
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.open && self.options.is_none() {
            vec![&self.content]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open && self.options.is_none() {
            visitor(&self.content);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open && self.options.is_none() {
            vec![&mut self.content]
        } else {
            Vec::new()
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let options = self.options.as_ref()?;
        let n = options.items.len();
        let children = options
            .items
            .iter()
            .enumerate()
            .map(|(i, label)| {
                AccessNode::new(Role::MenuItem)
                    .with_name(label.clone())
                    .with_position(i.saturating_add(1), n)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.open && i == options.highlighted,
                        selected: options.selected == Some(i),
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_optional_name(self.label.clone())
                .with_state(AccessState {
                    expanded: Some(self.open),
                    hidden: !self.open,
                    ..Default::default()
                })
                .with_children(children),
        )
    }
}

#[cfg(test)]
#[path = "popover_tests.rs"]
mod tests;
