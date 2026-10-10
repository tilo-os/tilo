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
pub use crate::widget::floating_surface::PopoverSide;

/// Which edge of its anchor a popover lines up with.
pub use crate::widget::floating_surface::PanelAlign;

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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent open, presentation and invalidation flags"
)]
pub struct Popover<C: Widget = NoContent> {
    content: C,
    options: Option<Options>,
    /// Accessible name of an option menu (e.g. "Refresh interval").
    label: Option<String>,
    open: bool,
    anchor: Rect,
    side: PopoverSide,
    resolved_side: PopoverSide,
    align: PanelAlign,
    /// Bounds the panel stays inside. `None` uses the window the top layer
    /// paints over.
    viewport: Option<Rect>,
    /// The window the top layer last painted over.
    presented: Option<Rect>,
    /// The panel, resolved by [`Self::place`] (also run by layout and paint).
    rect: Rect,
    on_dismiss: Option<Box<dyn FnMut()>>,
    last_cursor: Point,
    layout_theme: Theme,
    /// The content's own tree, when no host lays it out.
    detached: Option<floating_surface::DetachedContent>,
    /// The content's node in the host's tree, once a host has built it: a
    /// field in it then takes focus like any other.
    host_content: Option<NodeId>,
    /// The content's size as the host measured it.
    content_size: (f64, f64),
    /// Where the content's top-left was last placed.
    content_origin: Point,
    /// The host must place the content again.
    stale: bool,
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
            align: PanelAlign::Leading,
            viewport: None,
            presented: None,
            rect: Rect::ZERO,
            on_dismiss: None,
            last_cursor: Point::ZERO,
            layout_theme: Theme::dark(),
            detached: None,
            host_content: None,
            content_size: (0.0, 0.0),
            content_origin: Point::ZERO,
            stale: false,
        }
    }

    /// Line the panel up with the anchor's trailing edge instead of its
    /// leading edge: a popover from a button at the end of a header row.
    #[must_use]
    pub fn aligned(mut self, align: PanelAlign) -> Self {
        self.align = align;
        self
    }

    /// [`Self::aligned`] after construction.
    pub fn set_align(&mut self, align: PanelAlign) {
        self.align = align;
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
        if open && !self.open {
            self.stale = self.host_content.is_some();
        }
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
        let viewport = self.room(theme);
        self.place_in(viewport, tr, theme);
    }

    /// Where the panel may go: the given viewport, else the window the top
    /// layer last painted over (less the edge margin every floating panel
    /// keeps), else anywhere.
    fn room(&self, theme: &Theme) -> Rect {
        self.viewport
            .or_else(|| {
                self.presented
                    .map(|window| floating_surface::panel_room(window, theme))
            })
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6))
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
        } else if self.host_content.is_some() {
            // The host measured the content at its own width.
            self.content_size
        } else {
            // Layout rounds the measured size to whole pixels, so text measured
            // at its max-content width can come back a fraction short and wrap
            // when laid out in exactly that width: give it the next pixel.
            let (w, h) = self.measure_content(tr, theme);
            (w + 1.0, h)
        };
        let (w, h) = (body_w.ceil() + pad * 2.0, body_h.ceil() + pad * 2.0);
        let gap = f64::from(theme.tokens.spacing.s1);
        let (rect, side) = floating_surface::place_aligned(
            self.anchor,
            (w, h),
            self.side,
            self.align,
            gap,
            viewport,
        );
        self.resolved_side = side;
        self.rect = rect;
        if self.options.is_none() && self.host_content.is_none() {
            self.place_content(tr, theme);
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

    /// Where the content's top-left belongs: inside the panel's padding.
    fn content_target(&self, theme: &Theme) -> Point {
        self.rect.inset(-self.padding(theme)).origin()
    }

    /// Lay the content out inside the panel's padding, in its own tree.
    fn place_content(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let inner = self.rect.inset(-self.padding(theme));
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        let available = Size {
            width: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(inner.width())),
            height: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(inner.height())),
        };
        let content =
            floating_surface::DetachedContent::lay_out(&mut self.content, theme, available, tr);
        self.content
            .apply_layout(&content.taffy, content.node, inner.origin());
        self.content_origin = inner.origin();
        self.detached = Some(content);
    }

    fn dismiss(&mut self) {
        self.open = false;
        if let Some(cb) = self.on_dismiss.as_mut() {
            cb();
        }
    }

    /// Forward `event` to the content; returns whether it consumed it. A
    /// hosted popover's content is addressed by its node in the host's
    /// tree, so a field's focus request names a node the window knows.
    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        let before = std::mem::replace(ctx.handled, false);
        if let Some(node) = self.host_content {
            self.content
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        } else if let Some(content) = &self.detached {
            let mut detached_focus = None;
            self.content.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: ctx.state.reborrow(),
                    path: ctx.path,
                    handled: ctx.handled,
                    // A node of the popover's own tree means nothing to the
                    // window; the content keeps its own focus.
                    focus_target: &mut detached_focus,
                    node_id: content.node,
                    taffy: &content.taffy,
                },
            );
        }
        let consumed = *ctx.handled;
        *ctx.handled = before || consumed;
        consumed
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
            // The armed row is the panel's strong lift, as in every menu
            // (HIG §9.3); the applied option carries the check.
            let ink = if idx == options.highlighted {
                crate::widget::floating_surface::paint_armed_row(
                    scene, transform, row, m.pad, theme,
                )
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

/// The body of a prompt popover ([`Popover::prompt`]): a caption, one
/// field and a hint under it. Enter submits what was typed; Escape closes
/// the popover without it.
pub struct PopoverPrompt {
    caption: String,
    hint: String,
    input: crate::widget::TextInput,
    submitted: Option<String>,
    /// How many characters the field is sized for.
    chars: usize,
    rect: Rect,
    theme: Theme,
}

impl PopoverPrompt {
    fn new(caption: &str, placeholder: &str, hint: &str) -> Self {
        let mut input = crate::widget::TextInput::local().with_placeholder(placeholder);
        input.set_focus(true);
        Self {
            caption: caption.to_string(),
            hint: hint.to_string(),
            input,
            submitted: None,
            chars: 24,
            rect: Rect::ZERO,
            theme: Theme::dark(),
        }
    }

    /// The field's text.
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// The field, for its kind, filter or mono face.
    pub fn input_mut(&mut self) -> &mut crate::widget::TextInput {
        &mut self.input
    }

    /// Replace the hint under the field (a range, or why the value is not
    /// accepted).
    pub fn set_hint(&mut self, hint: &str) {
        hint.clone_into(&mut self.hint);
    }

    fn line(theme: &Theme, role: TextRole) -> f64 {
        f64::from(theme.text(role).line_height_px())
    }

    fn gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s2)
    }

    /// The prompt's size: the field is wide enough for `chars` characters of
    /// body text, never narrower than a utility panel's half.
    fn size(&self, theme: &Theme) -> (f64, f64) {
        let em = f64::from(theme.text(TextRole::Body).size) * 0.6;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a prompt's character count is small"
        )]
        let width =
            (em * self.chars as f64).max(f64::from(theme.tokens.metrics.panel_width_utility) / 2.0);
        let mut height = crate::widget::ControlSize::Regular.height(theme);
        if !self.caption.is_empty() {
            height += Self::line(theme, TextRole::Label) + Self::gap(theme);
        }
        if !self.hint.is_empty() {
            height += Self::line(theme, TextRole::Caption) + Self::gap(theme);
        }
        (width, height)
    }

    fn field_rect(&self) -> Rect {
        let theme = &self.theme;
        let top = if self.caption.is_empty() {
            self.rect.y0
        } else {
            self.rect.y0 + Self::line(theme, TextRole::Label) + Self::gap(theme)
        };
        Rect::new(
            self.rect.x0,
            top,
            self.rect.x1,
            top + crate::widget::ControlSize::Regular.height(theme),
        )
    }
}

impl Widget for PopoverPrompt {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let (w, h) = self.size(theme);
        let style = Style {
            size: Size {
                width: Dimension::length(crate::num::f32_of_f64(w)),
                height: Dimension::length(crate::num::f32_of_f64(h)),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let field = self.field_rect();
        self.input.apply_layout_external(field);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        if !self.caption.is_empty() {
            tr.draw_style(
                scene,
                &self.caption,
                colors.text_primary,
                transform.pre_translate(Vec2::new(self.rect.x0, self.rect.y0)),
                Some(crate::num::f32_of_f64(self.rect.width())),
                &theme.text(TextRole::Label),
            );
        }
        self.input.draw(scene, transform, tr, theme, presence);
        if !self.hint.is_empty() {
            let y = self.field_rect().y1 + Self::gap(theme);
            tr.draw_style(
                scene,
                &self.hint,
                colors.text_dim,
                transform.pre_translate(Vec2::new(self.rect.x0, y)),
                Some(crate::num::f32_of_f64(self.rect.width())),
                &theme.text(TextRole::Caption),
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            self.submitted = Some(self.input.value().to_string());
            *ctx.handled = true;
            return;
        }
        self.input.handle_event(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.input.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = Widget::accessibility(&self.input)?;
        // The caption names the field; a placeholder is only an example
        // of what goes in it.
        if !self.caption.is_empty() {
            node.name = Some(self.caption.clone());
        }
        if !self.hint.is_empty() {
            node.description = Some(self.hint.clone());
        }
        Some(node)
    }

    fn name(&self) -> &'static str {
        "PopoverPrompt"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl Popover<PopoverPrompt> {
    /// A prompt next to the control that asks for one value ("Go to
    /// offset"): `caption` over one field, `hint` under it. Enter submits
    /// ([`Self::take_submitted`]), Escape or a click outside closes.
    pub fn prompt(caption: &str, placeholder: &str, hint: &str) -> Self {
        Self::with_content(PopoverPrompt::new(caption, placeholder, hint))
    }

    /// Size the field for `chars` characters (default 24).
    #[must_use]
    pub fn with_field_chars(mut self, chars: usize) -> Self {
        self.content.chars = chars.max(4);
        self
    }

    /// Configure the field: its kind, its id, a filter.
    #[must_use]
    pub fn with_field(
        mut self,
        configure: impl FnOnce(crate::widget::TextInput) -> crate::widget::TextInput,
    ) -> Self {
        let input = std::mem::replace(&mut self.content.input, crate::widget::TextInput::local());
        self.content.input = configure(input);
        self
    }

    /// Open at `anchor` with `initial` in the field, selected so typing
    /// replaces it, and the field focused.
    pub fn open_prompt(&mut self, anchor: Rect, initial: &str) {
        self.set_anchor(anchor);
        self.content.input.set_text(initial);
        self.content.input.select_all();
        self.content.input.set_focus(true);
        self.content.submitted = None;
        self.set_open(true);
    }

    /// What was submitted with Enter, once; the popover closes with it.
    pub fn take_submitted(&mut self) -> Option<String> {
        let submitted = self.content.submitted.take();
        if submitted.is_some() {
            self.open = false;
        }
        submitted
    }
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
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
        if self.options.is_some() {
            // A zero-extent leaf out of the flow: the panel is placed against
            // the anchor, not by the parent's layout, and takes no room or
            // gap there. Registered so the host's sweeps reach it.
            let node = taffy.leaf_node(Style {
                position: taffy::Position::Absolute,
                size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        // The content is laid out in the host's tree, out of flow, at its
        // own (max-content) width: its fields focus like any other, and it
        // takes no room from the row the popover is mounted in.
        self.detached = None;
        let content = self.content.build_layout(taffy, theme, node_map);
        self.host_content = Some(content);
        let width = self.presented.map_or(Dimension::auto(), |w| {
            Dimension::length(crate::num::f32_of_f64(w.width()))
        });
        let style = Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::auto(),
                bottom: taffy::LengthPercentageAuto::auto(),
            },
            size: Size {
                width,
                height: Dimension::auto(),
            },
            flex_direction: taffy::FlexDirection::Row,
            align_items: Some(taffy::AlignItems::FlexStart),
            ..Default::default()
        };
        taffy.widget_node(style, &[content], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, _origin: Point) {
        if let Some(content) = taffy
            .children(node)
            .ok()
            .and_then(|c| c.first().copied())
            .filter(|&c| Some(c) == self.host_content)
        {
            let l = taffy.layout(content).copied().unwrap_or_default();
            self.content_size = (f64::from(l.size.width), f64::from(l.size.height));
            let theme = self.layout_theme.clone();
            if self.open {
                crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
                    self.place(tr, &theme);
                });
            }
            let target = self.content_target(&theme);
            self.content.apply_layout(
                taffy,
                content,
                Point::new(
                    target.x - f64::from(l.location.x),
                    target.y - f64::from(l.location.y),
                ),
            );
            self.content_origin = target;
            self.stale = false;
            return;
        }
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
        if let Some(window) = presence.viewport.get() {
            if self
                .presented
                .is_none_or(|w| (w.width() - window.width()).abs() > 0.5)
            {
                self.stale = self.host_content.is_some();
            }
            self.presented = Some(window);
        }
        let viewport = self.room(theme);
        self.place_in(viewport, text_renderer, theme);
        floating_surface::paint(scene, transform, self.rect, theme);
        if self.options.is_some() {
            self.paint_options(scene, transform, text_renderer, theme);
        } else {
            let on_panel = theme.on_ground(crate::chrome::panel_ground(theme));
            // A hosted content follows the panel on the host's next layout
            // pass; until then it is drawn where it belongs.
            let shift = self.content_target(theme) - self.content_origin;
            if shift.hypot() > 0.01 {
                self.stale = true;
            }
            let transform = transform * Affine::translate(shift);
            self.content
                .draw(scene, transform, text_renderer, &on_panel, presence);
            self.content
                .draw_overlay(scene, transform, text_renderer, &on_panel, presence);
        }
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        // The content's max-content width is bounded by the popover node:
        // give it the window's width once the top layer has named it.
        let (Some(window), true) = (self.presented, self.host_content.is_some()) else {
            return false;
        };
        let Ok(mut style) = taffy.style(node).cloned() else {
            return false;
        };
        let width = Dimension::length(crate::num::f32_of_f64(window.width()));
        if style.size.width == width {
            return false;
        }
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    fn is_layout_dirty(&self) -> bool {
        self.stale || (self.open && self.options.is_none() && self.content.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.stale || (self.open && self.options.is_none() && self.content.layout_needs_measure())
    }

    fn needs_animation_frame(&self) -> bool {
        self.open && (self.stale || self.content.needs_animation_frame())
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

/// A button that opens a popover anchored to its own rect
/// ([`crate::widget::Button::with_popover`]): a filter's options, a share
/// sheet's details. A click, Enter or Space opens it and closes it again;
/// the button reads as expanded while it is open.
pub struct PopoverButton<C: Widget + 'static> {
    button: crate::widget::Button,
    popover: Popover<C>,
    request: std::rc::Rc<std::cell::Cell<bool>>,
}

impl crate::widget::Button {
    /// Make this button open `content` in a popover anchored to its own
    /// rect.
    #[must_use]
    pub fn with_popover<C: Widget + 'static>(self, content: C) -> PopoverButton<C> {
        let request = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = std::rc::Rc::clone(&request);
        PopoverButton {
            button: self.on_click(move || flag.set(true)),
            popover: Popover::with_content(content),
            request,
        }
    }
}

impl<C: Widget + 'static> PopoverButton<C> {
    /// Prefer opening on `side` of the button.
    #[must_use]
    pub fn side(mut self, side: PopoverSide) -> Self {
        self.popover = self.popover.side(side);
        self
    }

    /// Line the popover up with the button's trailing edge.
    #[must_use]
    pub fn aligned(mut self, align: PanelAlign) -> Self {
        self.popover.set_align(align);
        self
    }

    pub fn popover(&self) -> &Popover<C> {
        &self.popover
    }

    pub fn button_mut(&mut self) -> &mut crate::widget::Button {
        &mut self.button
    }

    pub fn is_open(&self) -> bool {
        self.popover.is_open()
    }

    /// Open the popover at the button (a stage hook, a shortcut).
    pub fn open(&mut self) {
        self.popover.set_anchor(Widget::get_rect(&self.button));
        self.popover.set_open(true);
    }

    pub fn close(&mut self) {
        self.popover.close();
    }

    /// Answer the button's click (or its keyboard activation): open, or
    /// close an open popover.
    fn take_request(&mut self) {
        if self.request.replace(false) {
            if self.popover.is_open() {
                self.popover.close();
            } else {
                self.open();
            }
        }
    }

    /// The button's and the popover's nodes under this widget's node.
    fn nodes(taffy: &TaffyTree<()>, node: NodeId) -> Option<(NodeId, NodeId)> {
        let children = taffy.children(node).ok()?;
        Some((*children.first()?, *children.get(1)?))
    }
}

impl<C: Widget + 'static> Widget for PopoverButton<C> {
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.button.set_platform_conventions(conventions);
        self.popover.set_platform_conventions(conventions);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.button.sync(state, path);
        self.popover.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let button = self.button.build_layout(taffy, theme, node_map);
        let popover = self.popover.build_layout(taffy, theme, node_map);
        // The popover's node stands out of flow beside the button's, so the
        // pair is as big as the button and the popover's content joins the
        // window's tree (and its focus order) while it is open.
        taffy.widget_node(Style::default(), &[button, popover], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let at = Point::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
        );
        let Some((button, popover)) = Self::nodes(taffy, node) else {
            return;
        };
        self.button.apply_layout(taffy, button, at);
        self.popover.set_anchor(self.button.get_rect());
        self.popover.apply_layout(taffy, popover, at);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A keyboard activation reaches the button through the host.
        self.take_request();
        self.button
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.button
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        self.popover.set_anchor(self.button.get_rect());
        self.popover
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let nodes = Self::nodes(ctx.taffy, ctx.node_id);
        let (button, popover) = nodes.unwrap_or((ctx.node_id, ctx.node_id));
        if self.popover.is_open() {
            self.popover
                .handle_event(event, &mut ctx.reborrow(ctx.path, popover));
            if !self.popover.is_open() {
                // The press that closed it on the button must not reopen it.
                self.request.set(false);
            }
            return;
        }
        self.button
            .handle_event(event, &mut ctx.reborrow(ctx.path, button));
        self.take_request();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.button, &self.popover]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.button);
        visitor(&self.popover);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.button, &mut self.popover]
    }

    fn needs_redraw(&self) -> bool {
        self.request.get() || self.popover.is_open() || self.button.needs_redraw()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.popover.modal_capture()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.popover.is_open() || self.button.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "PopoverButton"
    }

    fn get_rect(&self) -> Rect {
        self.button.get_rect()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        // Presentational: the button and the popover speak for themselves;
        // the button's expanded state follows the popover.
        None
    }
}

#[cfg(test)]
#[path = "popover_tests.rs"]
mod tests;
