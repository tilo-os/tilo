//! `EmptyState` — the centered "there's nothing here yet" placeholder that
//! almost every list-shaped app hand-rolls (Inbox with no mail, a feed with
//! no articles, a contacts book with no contacts, …). Before this widget the
//! pattern was copy-pasted as a `Column` of a big icon + a heading `Text` +
//! a dim subtitle + an optional call-to-action `Button` across the fleet.
//!
//! `EmptyState` folds that into one theme-driven widget:
//!
//! - an optional hero [`Icon`] inside a soft tinted disc,
//! - a **title** line (the "No messages" headline),
//! - an optional **message** line (the dim one-liner underneath),
//! - an optional **action** pill (e.g. "Compose", "Add contact").
//!
//! The content is centered on both axes inside whatever space the parent
//! gives the widget, so dropping it as the sole child of a content pane
//! "just works". The action is surfaced through the toolkit's drain channel
//! ([`EmptyState::take_action`]) rather than a callback, matching the
//! CRDT-native input model used by [`super::ContextMenu`] /
//! [`super::TriStateControl`].

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{ParagraphAlign, TextRenderer, TextStyle, TextWrapMode};
use crate::theme::Theme;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::{ActionEmitter, Scrollable, TaffyTreeExt, Widget};

/// Diameter of the hero-icon disc.
const DISC_D: f64 = 72.0;
/// Icon glyph size drawn inside the disc.
const ICON_D: f64 = 34.0;

/// Which elements a caller asked for.
#[derive(Clone, Copy, Debug)]
struct Parts {
    icon: bool,
    message: bool,
    action: bool,
}

/// Measured heights the stack is built from.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    title_h: f64,
    msg_h: f64,
    action_w: f64,
    action_h: f64,
    inner_width: f32,
    pad_x: f64,
    gap: f64,
    btn_h: f64,
}

/// Which elements actually fit, and how tall the result is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    icon: bool,
    message: bool,
    total: f64,
}

/// What an empty state shows: optional hero icon, title, optional message
/// and optional action-pill label.
#[derive(Clone, Copy, Debug)]
pub struct EmptyStateContent<'a> {
    pub icon: Option<Icon>,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub action_label: Option<&'a str>,
}

impl<'a> EmptyStateContent<'a> {
    /// Just a title, the minimal case.
    pub fn new(title: &'a str) -> Self {
        Self {
            icon: None,
            title,
            message: None,
            action_label: None,
        }
    }

    fn parts(&self) -> Parts {
        Parts {
            icon: self.icon.is_some(),
            message: self.message.is_some(),
            action: self.action_label.is_some(),
        }
    }
}

/// The typography and spacing an empty-state stack is measured with.
#[derive(Clone, Debug)]
struct StackStyle {
    title: TextStyle,
    message: TextStyle,
    action: TextStyle,
    gap: f64,
    pad_y: f64,
}

impl StackStyle {
    /// Theme styles before renderer scaling, as the retained widget lays out.
    fn layout(theme: &Theme) -> Self {
        let typography = &theme.tokens.typography.scale;
        Self {
            title: TextStyle::new(typography.xl).with_wrap_mode(TextWrapMode::WordOrGlyph),
            message: TextStyle::new(typography.sm).with_wrap_mode(TextWrapMode::WordOrGlyph),
            action: TextStyle::new(typography.sm).with_wrap_mode(TextWrapMode::WordOrGlyph),
            gap: f64::from(theme.tokens.spacing.s4),
            pad_y: f64::from(theme.tokens.spacing.s2),
        }
    }

    /// Centered, renderer-scaled styles for painting.
    fn resolved(text_renderer: &TextRenderer, theme: &Theme) -> Self {
        let layout = Self::layout(theme);
        let centered = |style: TextStyle| {
            text_renderer.renderer_input_style(&style.with_align(ParagraphAlign::Center))
        };
        Self {
            title: centered(layout.title),
            message: centered(layout.message),
            action: centered(layout.action),
            ..layout
        }
    }

    fn renderer_input(&self, text_renderer: &TextRenderer) -> Self {
        Self {
            title: text_renderer.renderer_input_style(&self.title),
            message: text_renderer.renderer_input_style(&self.message),
            action: text_renderer.renderer_input_style(&self.action),
            ..*self
        }
    }

    fn measure(
        &self,
        text_renderer: &mut TextRenderer,
        width: f64,
        content: &EmptyStateContent<'_>,
    ) -> Metrics {
        let gap = self.gap;
        let pad_x = gap;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "window widths are logical pixels well within f32 range"
        )]
        let inner_width = (width - gap * 2.0).max(1.0) as f32;
        let (_, title_h) =
            text_renderer.measure_style(content.title, Some(inner_width), &self.title);
        let (_, msg_h) = content.message.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.message)
        });
        let (action_w, action_h) = content.action_label.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.action)
        });
        Metrics {
            title_h: f64::from(title_h),
            msg_h: f64::from(msg_h),
            action_w: f64::from(action_w),
            action_h: f64::from(action_h),
            inner_width,
            pad_x,
            gap,
            btn_h: if content.action_label.is_some() {
                34.0_f64.max(f64::from(action_h) + self.pad_y * 2.0)
            } else {
                0.0
            },
        }
    }
}

impl Fit {
    /// Shed decorative elements rather than overflow the rect: the hero disc
    /// is the largest element and carries the least information, so it goes
    /// first; the message goes next, only if the title alone still doesn't
    /// fit. The title and the action are never dropped — the first is the
    /// whole point and the second is the only thing the user can do here.
    fn choose(height: f64, parts: Parts, m: Metrics) -> Fit {
        let stack = |icon: bool, message: bool| {
            let mut t = m.title_h;
            if icon {
                t += DISC_D + m.gap;
            }
            if message {
                t += m.gap * 0.5 + m.msg_h;
            }
            if parts.action {
                t += m.gap + m.btn_h;
            }
            t
        };
        let mut icon = parts.icon;
        let mut message = parts.message;
        if icon && stack(true, message) > height {
            icon = false;
        }
        if message && stack(icon, true) > height {
            message = false;
        }
        Fit {
            icon,
            message,
            total: stack(icon, message),
        }
    }
}

/// Shared layout + paint for the empty state, centered inside `rect`. Used by
/// both [`EmptyState`]'s [`Widget::draw`] and the public immediate-mode
/// [`EmptyState::paint`] entry point. Returns the action pill's rect (for
/// hit-testing) when `action_label` is `Some`, else `None`. `hover` is the
/// pointer position for the pill hover state (pass an off-screen point to
/// suppress hover).
#[allow(
    clippy::too_many_lines,
    reason = "one immediate-mode pass must keep adaptive layout, painting, focus treatment and returned hit geometry in lockstep"
)]
fn paint_empty_state(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &EmptyStateContent<'_>,
    hover: Point,
    action_focused: bool,
) -> Option<Rect> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let theme = ctx.theme;
    let transform = ctx.transform;
    let mut style = StackStyle::resolved(ctx.text, theme);
    let mut metrics = style.measure(ctx.text, rect.width(), content);
    let gap = metrics.gap;

    let mut fit = Fit::choose(rect.height(), content.parts(), metrics);
    // On a phone at a high text scale a three-line headline can crowd out
    // the recovery explanation even after the decorative icon is shed.
    // Preserve the action *and* the explanation by compacting only the
    // display size of the heading, while keeping its heading semantics and
    // full accessible name intact. The final fallback still prefers the
    // action if a parent offers too little vertical space.
    if content.message.is_some() && !fit.message {
        let original = style.title.clone();
        for factor in [0.82_f32, 0.72, 0.60] {
            let mut candidate = style.clone();
            candidate.title = original.clone();
            candidate.title.size *= factor;
            let candidate_metrics = candidate.measure(ctx.text, rect.width(), content);
            let candidate_fit = Fit::choose(
                rect.height(),
                Parts {
                    message: true,
                    ..content.parts()
                },
                candidate_metrics,
            );
            if candidate_fit.message {
                style = candidate;
                metrics = candidate_metrics;
                fit = candidate_fit;
                break;
            }
        }
    }
    let icon = if fit.icon { content.icon } else { None };
    let message = if fit.message { content.message } else { None };
    let total = fit.total;

    let cx = rect.center().x;
    let mut y = if total > rect.height() {
        rect.y0
    } else {
        rect.center().y - total / 2.0
    };

    if let Some(icon) = icon {
        let disc_center = Point::new(cx, y + DISC_D / 2.0);
        let disc = Circle::new(disc_center, DISC_D / 2.0);
        ctx.scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.text_primary.with_alpha_factor(0.14),
            None,
            &disc,
        );
        let icon_rect = Rect::from_center_size(disc_center, (ICON_D, ICON_D));
        icon.draw_with_stroke(
            ctx.scene,
            transform,
            icon_rect,
            theme.tokens.colors.intent.inverted,
            1.75,
        );
        y += DISC_D + gap;
    }

    let text_x = cx - f64::from(metrics.inner_width) / 2.0;
    ctx.text(
        content.title,
        &style.title,
        theme.text_primary,
        Point::new(text_x, y),
        Some(metrics.inner_width),
    );
    y += metrics.title_h;

    if let Some(msg) = message {
        y += gap * 0.5;
        ctx.text(
            msg,
            &style.message,
            theme.text_dim,
            Point::new(text_x, y),
            Some(metrics.inner_width),
        );
        y += metrics.msg_h;
    }

    if let Some(label) = content.action_label {
        y += gap;
        let bw = (metrics.action_w + metrics.pad_x * 2.0).min(rect.width());
        let x0 = cx - bw / 2.0;
        let action_rect = Rect::new(x0, y, x0 + bw, y + metrics.btn_h);
        let hovered = action_rect.contains(hover);
        let bg = if hovered {
            theme.tokens.colors.intent.inverted_hover
        } else {
            theme.tokens.colors.intent.inverted
        };
        let pill = RoundedRect::from_rect(action_rect, f64::from(theme.tokens.radii.control));
        ctx.scene.fill(Fill::NonZero, transform, bg, None, &pill);
        // An empty-state action is often the only route out of a first-use
        // screen. Keep the normal, high-visibility focus treatment even
        // though this compact state is painted as one immediate-mode widget.
        if action_focused {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &pill,
            );
        }
        let ty = action_rect.y0 + (metrics.btn_h - metrics.action_h) / 2.0;
        ctx.text(
            label,
            &style.action,
            // Ink chosen against the inverted fill (rest and hover).
            theme.tokens.colors.ink_on_any(
                &[bg, theme.tokens.colors.intent.inverted],
                theme.tokens.colors.intent.on_inverted,
                ON_FILL_MIN_CONTRAST,
            ),
            Point::new(text_x, ty),
            Some(metrics.inner_width),
        );
        (action_rect.y1 > rect.y0 && action_rect.y0 < rect.y1).then_some(action_rect)
    } else {
        None
    }
}

/// A centered empty-state placeholder: hero icon + title + message + action.
pub struct EmptyState {
    icon: Option<Icon>,
    title: String,
    message: Option<String>,
    action_label: Option<String>,
    action_id: Option<String>,
    /// Canonical resource action emitted by every activation path. When set,
    /// this replaces the legacy document-key/drain channels just like
    /// [`super::Button::on_action`].
    action: Option<ActionEmitter<()>>,
    /// Optional CRDT action key, matching [`super::Button`]'s action-path
    /// contract. The drain channel remains available for immediate-mode
    /// parents; a retained shell can instead observe this semantic key.
    action_path: Option<String>,
    /// Drained flag set when the action pill is clicked.
    action_fired: bool,
    rect: Rect,
    action_rect: Rect,
    last_cursor: Point,
    /// The action is immediate-mode, but still participates in the normal
    /// Tab / Enter contract through this owning widget.
    action_focused: bool,
    layout_style: StackStyle,
    scroll_content: bool,
}

impl EmptyState {
    /// Construct with just a title (the minimal case). Chain
    /// [`with_icon`](Self::with_icon) / [`with_message`](Self::with_message) /
    /// [`with_action`](Self::with_action) for the fuller treatment.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            icon: None,
            title: title.into(),
            message: None,
            action_label: None,
            action_id: None,
            action: None,
            action_path: None,
            action_fired: false,
            rect: Rect::ZERO,
            action_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            action_focused: false,
            layout_style: StackStyle::layout(&Theme::default()),
            scroll_content: false,
        }
    }

    /// Add a hero icon rendered inside a soft tinted disc above the title.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Add the dim one-line message under the title.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Add an accent call-to-action pill under the message. The label is a
    /// pre-resolved display string (callers do the `i18n!` lookup).
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self
    }

    /// Stable identity for the optional call-to-action. Supplying it makes the
    /// action automatically available to accessibility, search, and agents.
    #[must_use]
    pub fn with_action_id(mut self, id: impl Into<String>) -> Self {
        self.action_id = Some(id.into());
        self
    }

    /// Route pointer, keyboard and accessibility activation through an
    /// exported resource action. The host revalidates the binding against the
    /// current resource tree before dispatch, so empty-state recovery controls
    /// follow the same policy/history path as ordinary buttons.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.action_path = None;
        self.action_fired = false;
        self
    }

    /// Publish activation at `path` in the app document as well as through
    /// [`Self::take_action`]. This gives an empty state's primary action the
    /// same pointer, keyboard and accessibility route as a normal button.
    #[must_use]
    pub fn with_action_path(mut self, path: impl Into<String>) -> Self {
        self.action = None;
        self.action_path = Some(path.into());
        self
    }

    fn fire_action(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(()) {
                tracing::warn!(%error, empty_state = %self.title, "action-bound empty state refused its arguments");
            }
            return;
        }
        self.action_fired = true;
        if let Some(action_path) = &self.action_path {
            use automerge::transaction::Transactable as _;
            let _ = state.put(path, action_path, true);
        }
    }

    /// Drain the "action pill was clicked" signal (fires once per click). The
    /// host reacts by mutating its own model — e.g. opening a compose sheet.
    pub fn take_action(&mut self) -> bool {
        std::mem::take(&mut self.action_fired)
    }

    /// The resolved bounds of the action pill (empty until laid out + drawn).
    /// Exposed so tests can assert the hit target without rendering to pixels.
    pub fn action_rect(&self) -> Rect {
        self.action_rect
    }

    /// Wrap this retained empty state in the toolkit's vertical viewport.
    /// The child keeps at least the viewport height for ordinary centering and
    /// grows to its measured content height when wrapped text needs more room,
    /// making a below-fold action reachable by wheel, keyboard, or scrollbar.
    #[must_use]
    pub fn into_scrollable(mut self) -> Scrollable {
        self.scroll_content = true;
        Scrollable::new_vertical().with_child(Box::new(self))
    }

    /// Minimum height required to show the title and optional action at
    /// `width`, after wrapping and accessibility scaling. Immediate-mode
    /// callers with a shorter viewport must provide scrolling; [`Self::paint`]
    /// anchors at the top and does not return a wholly offscreen hit target.
    pub fn minimum_essential_height(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        title: &str,
        action_label: Option<&str>,
    ) -> f64 {
        let content = EmptyStateContent {
            action_label,
            ..EmptyStateContent::new(title)
        };
        let metrics =
            StackStyle::resolved(text_renderer, theme).measure(text_renderer, width, &content);
        metrics.title_h + action_label.map_or(0.0, |_| metrics.gap + metrics.btn_h)
    }

    /// Place the empty state inside a rect computed by an immediate-mode
    /// parent.
    ///
    /// Some application shells use Taffy for their root allocation but split
    /// their internal panes with production-specific geometry. This keeps the
    /// empty state a real widget (focus, accessibility and activation included)
    /// without forcing those shells to duplicate its paint and hit-testing.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Immediate-mode paint of an empty state centered inside `rect`, for the
    /// many apps that render straight to a `Scene` instead of building a
    /// widget tree. Returns the action pill's rect when `action_label` is
    /// visible so the caller can hit-test it against its own pointer state
    /// (pass `hover` for the hover tint, or an off-screen point to suppress).
    /// Call [`Self::minimum_essential_height`] first when the supplied pane can
    /// be physically shorter than the title and action; that parent must own
    /// scrolling, and a wholly offscreen action produces no false hit target.
    pub fn paint(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
    ) -> Option<Rect> {
        paint_empty_state(ctx, rect, content, hover, false)
    }

    fn content(&self) -> EmptyStateContent<'_> {
        EmptyStateContent {
            icon: self.icon,
            title: &self.title,
            message: self.message.as_deref(),
            action_label: self.action_label.as_deref(),
        }
    }
}

impl Widget for EmptyState {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_style = StackStyle::layout(theme);
        // Auto height contributes wrapped content to a Scrollable's extent;
        // the percentage minimum still fills a normal definite-height pane.
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: percent(0.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => None,
        });
        let measured_width = width.unwrap_or(240.0).max(1.0);
        let metrics = self.layout_style.renderer_input(text_renderer).measure(
            text_renderer,
            f64::from(measured_width),
            &self.content(),
        );
        let essential_height = metrics.title_h
            + self
                .action_label
                .as_ref()
                .map_or(0.0, |_| metrics.gap + metrics.btn_h);
        let intrinsic_height = if self.scroll_content {
            essential_height
        } else {
            Fit::choose(f64::INFINITY, self.content().parts(), metrics)
                .total
                .max(220.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Taffy uses f32 layout sizes; measured text geometry is intentionally rounded to that precision"
        )]
        let height = intrinsic_height as f32;
        taffy::Size {
            width: measured_width,
            height,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        self.action_rect = paint_empty_state(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &self.content(),
            self.last_cursor,
            self.action_focused,
        )
        .unwrap_or(Rect::ZERO);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.is_enabled()
                && self.action_label.is_some()
                && self.action_rect.contains(self.last_cursor) =>
            {
                self.fire_action(ctx.state, ctx.path);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.action_label.is_some() && self.action_rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        self.action_label.is_some() && self.is_enabled()
    }

    fn is_enabled(&self) -> bool {
        self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }

    fn set_focus(&mut self, focused: bool) {
        self.action_focused = focused && self.action_label.is_some();
    }

    fn is_focused(&self) -> bool {
        self.action_focused
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = crate::doc::document(state, path);
        if self.is_enabled()
            && self.action_label.is_some()
            && matches!(action, crate::a11y::Action::Activate)
        {
            self.fire_action(state, path);
            true
        } else {
            false
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        self.action_id.as_deref() == Some(target) && self.perform_action(action, state, path)
    }

    fn name(&self) -> &'static str {
        "EmptyState"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The empty state is a labelled group; the title is its accessible
        // name and the message its description so a screen reader announces
        // both without the caller wiring anything.
        let mut node = AccessNode::new(Role::Group).with_name(self.title.clone());
        if let Some(msg) = &self.message {
            node = node.with_description(msg.clone());
        }
        if let Some(action) = &self.action_label {
            node = node
                .with_state(AccessState {
                    focusable: true,
                    focused: self.action_focused,
                    ..Default::default()
                })
                .with_children(vec![{
                    let mut action_node = AccessNode::new(Role::Button)
                        .with_name(action.clone())
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.action_focused,
                            disabled: !self.is_enabled(),
                            ..Default::default()
                        });
                    if let Some(id) = &self.action_id {
                        action_node = action_node.with_action_target(id.clone());
                    }
                    action_node
                }]);
        }
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        matches!(path, [0]).then_some(self.action_rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if matches!(path, [0]) && self.action_label.is_some() && self.is_enabled() {
            self.action_focused = true;
            true
        } else {
            false
        }
    }
}

impl crate::widget::controls::ActionControls for EmptyState {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        if let Some(action) = &self.action {
            return vec![
                action.metadata(
                    self.action_id
                        .clone()
                        .unwrap_or_else(|| format!("empty-state:{}", self.title)),
                ),
            ];
        }
        self.action_path.as_ref().map_or_else(
            || {
                self.action_id
                    .clone()
                    .map(crate::widget::ControlBindingMetadata::semantic)
                    .into_iter()
                    .collect()
            },
            |path| {
                vec![crate::widget::ControlBindingMetadata {
                    control_id: self
                        .action_id
                        .clone()
                        .unwrap_or_else(|| format!("empty-state:{}", self.title)),
                    kind: crate::widget::ControlBindingKind::CollaborativeDocument(path.clone()),
                }]
            },
        )
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn lay_out(w: &mut dyn Widget, vw: f32, vh: f32) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(vw),
                    height: taffy::AvailableSpace::Definite(vh),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn draw(w: &mut dyn Widget) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    fn synth() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    /// Representative numbers: the dark theme's `xl` title and `sm` message at
    /// the spacing scale a docked panel is drawn with.
    fn metrics() -> Metrics {
        Metrics {
            title_h: 24.0,
            msg_h: 18.0,
            action_w: 0.0,
            action_h: 0.0,
            inner_width: 0.0,
            pad_x: 0.0,
            gap: 16.0,
            btn_h: 34.0,
        }
    }

    #[test]
    fn short_and_narrow_panes_shed_decoration_but_keep_the_action() {
        /// A zero-state in a short pane (a docked panel body, a couple hundred
        /// pixels tall) must shed elements rather than push the message through
        /// the bottom edge.
        fn a_short_pane_sheds_the_hero_disc_before_it_overflows() {
            let want = Parts {
                icon: true,
                message: true,
                action: false,
            };
            let m = metrics();

            // Tall pane: everything fits, nothing is dropped.
            let roomy = Fit::choose(400.0, want, m);
            assert!(
                roomy.icon && roomy.message,
                "a full pane keeps the whole stack"
            );
            assert!(roomy.total <= 400.0);

            // Docked panel: the disc alone is 72px plus a 16px gap, so the stack
            // does not fit and the disc is what goes.
            let docked = Fit::choose(120.0, want, m);
            assert!(!docked.icon, "the disc is shed first");
            assert!(docked.message, "the explanation is kept while it fits");
            assert!(
                docked.total <= 120.0,
                "and the result fits: {} in 120",
                docked.total
            );

            // Barely any room: the message goes too, but never the title.
            let cramped = Fit::choose(30.0, want, m);
            assert!(!cramped.icon && !cramped.message);
            assert_eq!(cramped.total, m.title_h, "the title always survives");
        }

        /// The action pill is the only thing the user can do, so it is never shed —
        /// and it still counts toward what fits.
        fn the_action_is_never_shed_and_is_counted() {
            let m = metrics();
            let with_action = Parts {
                icon: true,
                message: true,
                action: true,
            };
            let fit = Fit::choose(120.0, with_action, m);
            assert!(!fit.icon, "the disc gives way to the action");
            assert!(
                fit.total >= m.title_h + m.gap + m.btn_h,
                "the pill is still in the measured stack"
            );
        }

        fn narrow_large_text_recovery_copy_wraps_inside_the_empty_state() {
            let theme = Theme::dark().with_text_scale(2.0);
            let rect = Rect::new(0.0, 0.0, 390.0, 680.0);
            let gap = f64::from(theme.tokens.spacing.s4);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test viewport is an ordinary logical-pixel dimension"
            )]
            let width = (rect.width() - gap * 2.0).max(1.0) as f32;
            let title = "Couldn’t open this page";
            let message = "The site did not respond. Check your connection and try again.";
            let title_style =
                TextStyle::new(theme.tokens.typography.scale.xl).with_align(ParagraphAlign::Center);
            let message_style =
                TextStyle::new(theme.tokens.typography.scale.sm).with_align(ParagraphAlign::Center);
            let mut renderer = TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let (_, title_one_line_height) = renderer.measure_style(title, None, &title_style);
            let (title_width, title_height) =
                renderer.measure_style(title, Some(width), &title_style);
            let (message_width, message_height) =
                renderer.measure_style(message, Some(width), &message_style);

            assert!(title_width <= width);
            assert!(message_width <= width);
            assert!(title_height > title_one_line_height);
            assert!(message_height > 0.0);

            let mut state = EmptyState::new(title)
                .with_message(message)
                .with_action("Try again");
            state.apply_layout_external(rect);
            let mut scene = Scene::new();
            state.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &PresenceCtx::default(),
            );
            let action = state.action_rect();
            assert!(rect.contains(action.center()));
            let action_style = renderer.renderer_input_style(
                &TextStyle::new(theme.tokens.typography.scale.sm)
                    .with_wrap_mode(TextWrapMode::WordOrGlyph)
                    .with_align(ParagraphAlign::Center),
            );
            let (_, action_line_height) =
                renderer.measure_style("Try again", Some(width), &action_style);
            assert!(
                action.height()
                    >= f64::from(action_line_height) + f64::from(theme.tokens.spacing.s2) * 2.0,
                "large-text action reserves its label's full line height"
            );
        }

        fn long_localized_content_wraps_inside_a_narrow_scaled_pane() {
            for width in [240.0, 480.0] {
                for scale in [1.0, 2.0] {
                    let theme = Theme::dark().with_text_scale(scale);
                    let mut tr = crate::font_catalog::fixture_renderer();
                    tr.set_text_scale(scale);
                    let mut scene = Scene::new();
                    let rect = Rect::new(20.0, 30.0, 20.0 + width, 1330.0);
                    let action = EmptyState::paint(
                        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                        rect,
                        &EmptyStateContent {
                            icon: Some(Icon::Inbox),
                            title: "Keine synchronisierten Nachrichten in diesem Arbeitsbereich",
                            message: Some(
                        "Sobald eine Verbindung verfügbar ist, erscheinen neue Nachrichten automatisch.",
                    ),
                            action_label: Some(
                        "Verbindungseinstellungen jetzt vollständig überprüfen und sichere Wiederherstellung vorbereiten",
                    ),
                        },
                        Point::new(-1.0, -1.0),
                    )
                .expect("action remains available");

                    assert!(action.x0 >= rect.x0 && action.x1 <= rect.x1, "{action:?}");
                    assert!(action.height() > 34.0, "wrapped action grows: {action:?}");
                    let encoding = scene.encoding();
                    for run in &encoding.resources.glyph_runs {
                        for glyph in &encoding.resources.glyphs[run.glyphs.clone()] {
                            let x = f64::from(run.transform.translation[0] + glyph.x);
                            assert!(
                                x >= rect.x0 && x <= rect.x1,
                                "{width}px/{scale}x glyph x={x} outside {rect:?}"
                            );
                        }
                    }
                }
            }
        }

        fn tiny_immediate_pane_exposes_required_height_without_offscreen_hit_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = crate::font_catalog::fixture_renderer();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            let rect = Rect::new(10.0, 20.0, 250.0, 45.0);
            let minimum = EmptyState::minimum_essential_height(
                &mut tr,
                &theme,
                rect.width(),
                "No messages are available",
                Some("Review connection settings"),
            );
            assert!(minimum > rect.height());
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: Some(Icon::Inbox),
                    title: "No messages are available",
                    message: Some("Reconnect to load messages."),
                    action_label: Some("Review connection settings"),
                },
                Point::new(-1.0, -1.0),
            );

            assert!(
                action.is_none(),
                "a wholly offscreen action is not exposed as a usable hit target"
            );
        }

        a_short_pane_sheds_the_hero_disc_before_it_overflows();
        the_action_is_never_shed_and_is_counted();
        narrow_large_text_recovery_copy_wraps_inside_the_empty_state();
        long_localized_content_wraps_inside_a_narrow_scaled_pane();
        tiny_immediate_pane_exposes_required_height_without_offscreen_hit_geometry();
    }

    #[test]
    fn exported_action_button_is_placed_where_activation_clicks() {
        let mut es = EmptyState::new("No response yet").with_action("Send request");
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        es.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let update = crate::accesskit_bridge::build_tree_update(&es, &node_map, None, 1.0);
        let (_, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Button)
            .expect("the action is exported as a button");
        let bounds = button.bounds().expect("the action button has bounds");
        let action = es.action_rect();
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (action.x0, action.y0, action.x1, action.y1)
        );
    }

    #[test]
    fn empty_state_action_fires_once_through_pointer_keyboard_and_semantics() {
        fn action_pill_click_fires_once() {
            let mut es = EmptyState::new("No messages")
                .with_icon(Icon::Inbox)
                .with_message("Your inbox is empty")
                .with_action("Compose");
            lay_out(&mut es, 600.0, 400.0);
            // draw resolves the action rect.
            let _ = draw(&mut es);
            let target = es.action_rect().center();
            assert!(target.x > 0.0, "action rect must resolve after draw");
            dispatch(&mut es, &cursor(target));
            assert!(
                dispatch(&mut es, &press()),
                "click on the action pill is consumed"
            );
            assert!(es.take_action(), "the action signal fires");
            assert!(!es.take_action(), "the action signal drains after one read");
        }

        fn click_outside_action_does_not_fire() {
            let mut es = EmptyState::new("Nothing here").with_action("Add");
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            // Top-left corner is well outside the centered pill.
            dispatch(&mut es, &cursor(Point::new(2.0, 2.0)));
            assert!(
                !dispatch(&mut es, &press()),
                "click away from the pill is not consumed"
            );
            assert!(!es.take_action());
        }

        fn action_is_keyboard_reachable_and_exposed_as_a_named_button() {
            let mut es = EmptyState::new("No notes yet")
                .with_message("Create one to get started")
                .with_action("Create note");
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            assert!(Widget::is_focusable(&es));
            Widget::set_focus(&mut es, true);
            let node = Widget::accessibility(&es).expect("empty state is semantic");
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, crate::a11y::Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Create note"));
            assert!(node.children[0].state.focused);
            assert_eq!(
                es.synthetic_accessibility_rect(&[0]),
                Some(es.action_rect())
            );

            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut es,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(es.take_action(), "Enter/Space/AT Activate reaches the CTA");
        }

        fn action_path_is_shared_by_pointer_and_accessibility_activation() {
            use automerge::{ReadDoc as _, Value, transaction::Transactable as _};

            let key = "empty.open";
            let mut es = EmptyState::new("Nothing here")
                .with_action("Open")
                .with_action_path(key);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.leaf_node(Style::default());
            let mut handled = false;
            let mut focus = None;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            for event in [cursor(es.action_rect().center()), press()] {
                es.handle_event(
                    &event,
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }
            assert!(matches!(
                doc.get(&root, key),
                Ok(Some((Value::Scalar(value), _))) if value.as_ref().as_bool() == Some(true)
            ));

            let _ = doc.put(&root, key, false);
            assert!(es.perform_action(&crate::a11y::Action::Activate, &mut doc, &root));
            assert!(matches!(
                doc.get(&root, key),
                Ok(Some((Value::Scalar(value), _))) if value.as_ref().as_bool() == Some(true)
            ));
        }

        fn canonical_action_binding_replaces_the_legacy_document_channel() {
            let resource = tilo_actions::Resource::new("catalog", "catalog", "Catalog")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "refresh",
                        "Refresh catalog",
                        tilo_actions::OperationKind::Effect,
                        "test.refresh",
                        &(),
                    )
                    .unwrap(),
                );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "refresh").unwrap();
            let mut es = EmptyState::new("Unavailable")
                .with_action("Refresh catalog")
                .with_action_id("catalog.empty.refresh")
                .with_action_path("legacy.refresh")
                .on_action(binding);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            let target = es.action_rect().center();
            dispatch(&mut es, &cursor(target));
            assert!(dispatch(&mut es, &press()));
            let invocation = crate::widget::controls::take_control_invocation(&mut es)
                .expect("canonical invocation");
            assert_eq!(
                invocation.address,
                tilo_actions::ActionAddress::new("catalog", "refresh")
            );
            assert!(
                !es.take_action(),
                "action-bound activation cannot also fire the legacy drain channel"
            );
            assert!(matches!(
                crate::widget::controls::control_action_bindings(&es).as_slice(),
                [crate::widget::ControlBindingMetadata {
                    control_id,
                    kind: crate::widget::ControlBindingKind::Action(address),
                }] if control_id == "catalog.empty.refresh"
                    && address == &tilo_actions::ActionAddress::new("catalog", "refresh")
            ));
        }

        fn without_action_paints_no_pill_and_has_no_hit_target() {
            let mut es = EmptyState::new("Empty").with_icon(Icon::Folder);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            assert_eq!(es.action_rect(), Rect::ZERO, "no action ⇒ no pill rect");
            // A press anywhere is ignored when there's no action.
            let center = es.get_rect().center();
            dispatch(&mut es, &cursor(center));
            assert!(!dispatch(&mut es, &press()));
            assert!(!es.take_action());
        }

        action_pill_click_fires_once();
        click_outside_action_does_not_fire();
        action_is_keyboard_reachable_and_exposed_as_a_named_button();
        action_path_is_shared_by_pointer_and_accessibility_activation();
        canonical_action_binding_replaces_the_legacy_document_channel();
        without_action_paints_no_pill_and_has_no_hit_target();
    }

    #[test]
    fn empty_state_paints_scrolls_and_centres_its_essential_content() {
        fn draws_something_visible() {
            let mut es = EmptyState::new("No contacts yet")
                .with_icon(Icon::Inbox)
                .with_message("Add your first contact to get started")
                .with_action("Add contact");
            lay_out(&mut es, 600.0, 400.0);
            let scene = draw(&mut es);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            // disc + icon strokes + title glyphs + message glyphs + pill + label
            // glyphs ⇒ comfortably many COLOR tags.
            assert!(
                fills > 5,
                "a fully-populated empty state paints multiple fills, got {fills}"
            );
        }

        fn immediate_mode_paint_returns_action_rect() {
            // The canvas-app entry point paints into an arbitrary rect and hands
            // back the pill hit target, matching what Widget::draw resolves.
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 500.0, 300.0);
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: Some(Icon::Inbox),
                    title: "No mail",
                    message: Some("You're all caught up"),
                    action_label: Some("Compose"),
                },
                Point::new(-1.0, -1.0),
            );
            let r = action.expect("action label ⇒ a returned pill rect");
            assert!(rect.contains(r.center()), "pill sits inside the paint rect");
            // No action label ⇒ no returned rect.
            let none = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    title: "Empty",
                    message: None,
                    action_label: None,
                },
                Point::new(-1.0, -1.0),
            );
            assert!(none.is_none());
        }

        fn retained_empty_state_has_a_scrollable_route_to_essential_content() {
            use automerge::{ReadDoc as _, Value, transaction::Transactable as _};

            let mut scroll = EmptyState::new("No messages are available")
                .with_icon(Icon::Inbox)
                .with_message("Reconnect to load messages.")
                .with_action("Review connection settings")
                .with_action_path("empty.review")
                .into_scrollable()
                .with_height(50.0);
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let mut tr = crate::font_catalog::fixture_renderer();
            let node = scroll.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(240.0),
                        height: taffy::AvailableSpace::Definite(50.0),
                    },
                    |known, available, id, _, _| {
                        node_map.get(&id).map_or(taffy::Size::ZERO, |&ptr| unsafe {
                            (*ptr).measure(known, available, &mut tr)
                        })
                    },
                )
                .unwrap();
            scroll.apply_layout(&taffy, node, Point::ZERO);

            assert!(scroll.vertical_overflows());
            assert!(scroll.max_offset_y() > 0.0);
            scroll.scroll_by(0.0, scroll.max_offset_y());
            scroll.apply_layout(&taffy, node, Point::ZERO);
            let mut scene = Scene::new();
            scroll.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let action = scroll.children()[0]
                .synthetic_accessibility_rect(&[0])
                .expect("retained action geometry");
            assert!(
                scroll.content_viewport_rect().contains(action.center()),
                "scrolled action is visible: {action:?}"
            );

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut handled = false;
            let mut focus = None;
            for event in [cursor(action.center()), press()] {
                scroll.handle_event(
                    &event,
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }
            assert!(
                handled,
                "visible scrolled action accepts pointer activation"
            );
            assert!(matches!(
                doc.get(&root, "empty.review"),
                Ok(Some((Value::Scalar(value), _))) if value.as_ref().as_bool() == Some(true)
            ));

            let _ = doc.put(&root, "empty.review", false);
            let child = scroll.children_mut().remove(0);
            child.set_focus(true);
            assert!(child.perform_action(&crate::a11y::Action::Activate, &mut doc, &root));
            assert!(matches!(
                doc.get(&root, "empty.review"),
                Ok(Some((Value::Scalar(value), _))) if value.as_ref().as_bool() == Some(true)
            ));
        }

        fn roomy_scrollable_retains_the_icon_and_explanatory_message() {
            let make = |icon| {
                let mut scroll = EmptyState::new("No messages")
                    .with_message("Reconnect to load messages.")
                    .with_action("Review settings");
                if icon {
                    scroll = scroll.with_icon(Icon::Inbox);
                }
                scroll.into_scrollable().with_height(400.0)
            };
            let mut decorated = make(true);
            lay_out(&mut decorated, 480.0, 400.0);
            let decorated_scene = draw(&mut decorated);
            let decorated_encoding = decorated_scene.encoding();
            assert!(
                decorated_encoding.resources.glyph_runs.len() >= 3,
                "title, explanatory message, and action all remain painted"
            );

            let mut plain = make(false);
            lay_out(&mut plain, 480.0, 400.0);
            let plain_scene = draw(&mut plain);
            let decorated_colors = decorated_encoding
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            let plain_colors = plain_scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            assert!(
                decorated_colors > plain_colors,
                "roomy scrollable keeps the icon disc and strokes"
            );
        }

        fn roomy_manual_pane_keeps_the_essential_block_centered() {
            let theme = Theme::dark();
            let mut tr = crate::font_catalog::fixture_renderer();
            let rect = Rect::new(15.0, 25.0, 495.0, 425.0);
            let minimum = EmptyState::minimum_essential_height(
                &mut tr,
                &theme,
                rect.width(),
                "No messages",
                Some("Compose"),
            );
            let mut scene = Scene::new();
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    title: "No messages",
                    message: None,
                    action_label: Some("Compose"),
                },
                Point::new(-1.0, -1.0),
            )
            .expect("roomy pane exposes action");
            let top = action.y1 - minimum;
            assert!(((top - rect.y0) - (rect.y1 - action.y1)).abs() < 0.001);

            let mut retained = EmptyState::new("No messages").with_action("Compose");
            lay_out(&mut retained, 480.0, 400.0);
            assert_eq!(retained.get_rect(), Rect::new(0.0, 0.0, 480.0, 400.0));
            let _ = draw(&mut retained);
            let retained_action = retained.action_rect();
            let retained_top = retained_action.y1 - minimum;
            assert!((retained_top - (400.0 - retained_action.y1)).abs() < 0.001);
        }

        fn accessibility_is_a_named_group() {
            use crate::a11y::Role;
            let es = EmptyState::new("No results").with_message("Try another search");
            let node = Widget::accessibility(&es).expect("empty state exposes a11y");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("No results"));
            assert_eq!(node.description.as_deref(), Some("Try another search"));
        }

        draws_something_visible();
        immediate_mode_paint_returns_action_rect();
        retained_empty_state_has_a_scrollable_route_to_essential_content();
        roomy_scrollable_retains_the_icon_and_explanatory_message();
        roomy_manual_pane_keeps_the_essential_block_centered();
        accessibility_is_a_named_group();
    }
}
