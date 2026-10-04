//! `Banner` — an inline, semantic alert strip. Unlike a [`super::toast::Toast`]
//! (transient, top-right, auto-expiring) a banner is a *persistent* in-flow
//! message the user dismisses or resolves: "You're offline — changes will sync
//! when you reconnect", "Saved", "This file has unsaved changes".
//!
//! Four severities ([`BannerKind`]) map onto the semantic colour ramp so the
//! tint, border, and default leading icon all stay OS-controlled. A banner
//! carries a **title** plus an optional secondary **message**, and can be made
//! dismissible: its close icon button surfaces the click through
//! [`Banner::take_dismissed`], the toolkit's drain channel, so a CRDT-native
//! host can react without a callback.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// The banner's metrics, from the theme.
struct Metrics {
    pad_x: f64,
    pad_y: f64,
    icon: f64,
    dismiss: f64,
    gap: f64,
    min_height: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let t = &theme.tokens;
        Self {
            pad_x: f64::from(t.spacing.s4),
            pad_y: f64::from(t.spacing.s3),
            icon: f64::from(t.metrics.icon_md),
            dismiss: f64::from(t.metrics.bar_control_height),
            gap: f64::from(t.spacing.s3),
            min_height: f64::from(t.metrics.panel_row_height),
        }
    }
}

/// What an immediate-mode [`Banner::paint`] shows: severity, leading icon,
/// title, optional secondary message, an optional action and whether the
/// "×" is offered.
#[derive(Clone, Copy, Debug)]
pub struct BannerContent<'a> {
    pub kind: BannerKind,
    pub icon: Icon,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub dismissible: bool,
    /// The label of the one action that resolves the banner ("Try again",
    /// "Restore"), set inside the box at its trailing edge.
    pub action: Option<&'a str>,
}

/// Where an immediate-mode banner put its controls, for the host's
/// hit-testing: each is `Rect::ZERO` when the banner does not offer it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BannerHits {
    pub dismiss: Rect,
    pub action: Rect,
}

impl<'a> BannerContent<'a> {
    /// A non-dismissible banner with the severity's default icon.
    pub fn new(kind: BannerKind, title: &'a str) -> Self {
        Self {
            kind,
            icon: kind.default_icon(),
            title,
            message: None,
            dismissible: false,
            action: None,
        }
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }

    /// The one action that resolves what the banner reports.
    #[must_use]
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    /// The height this banner takes at `width`: its wrapped title and
    /// message between the banner's insets, never under one panel row.
    pub fn height(&self, width: f64, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let rect = Rect::new(0.0, 0.0, width.max(1.0), 1.0);
        let trailing = self.action.map_or(0.0, |label| {
            action_face(Rect::ZERO, label).intrinsic_width(text, theme) + Metrics::of(theme).gap
        });
        let text_width = banner_text_width(rect, self.dismissible, trailing, theme);
        text_block_height(self.title, self.message, text_width, text, theme)
    }

    #[must_use]
    pub fn with_message(mut self, message: Option<&'a str>) -> Self {
        self.message = message;
        self
    }

    #[must_use]
    pub fn dismissible(mut self, dismissible: bool) -> Self {
        self.dismissible = dismissible;
        self
    }
}

/// Shared visual paint for a banner filling `rect`. Used by both the retained
/// [`Banner`] widget and the immediate-mode [`Banner::paint`] entry point.
/// Returns the dismiss "×" rect when `dismissible`, else `Rect::ZERO`.
/// `reserve` is trailing room a retained action child takes beside the
/// text.
fn paint_banner(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &BannerContent<'_>,
    hover: Point,
    reserve: f64,
) -> BannerHits {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return BannerHits::default();
    }
    let BannerContent {
        kind,
        icon,
        title,
        message,
        dismissible,
        action,
    } = *content;
    let theme = ctx.theme;
    let transform = ctx.transform;
    let m = Metrics::of(theme);
    let status = kind.color(theme);
    let radius = f64::from(theme.tokens.radii.control);

    // A well in the content: the raised surface under its derived rest
    // edge. The severity is the icon's colour, never a tinted wash.
    let well = theme.tokens.colors.surfaces.raised;
    ctx.fill_round(rect, radius, well);
    ctx.stroke_round(rect, radius, crate::surface::edge_on(well), 1.0);
    let on_well = theme.on_ground(well);

    let icon_rect = Rect::from_center_size(
        Point::new(rect.x0 + m.pad_x + m.icon / 2.0, rect.center().y),
        (m.icon, m.icon),
    );
    icon.draw(ctx.scene, transform, icon_rect, status);

    let text_x = icon_rect.x1 + m.gap;
    let title_style = theme.text(TextRole::Body).strong();
    let message_style = theme.text(TextRole::Caption);
    let action_w = action.map_or(0.0, |label| {
        action_face(Rect::ZERO, label).intrinsic_width(ctx.text, theme)
    });
    let trailing = reserve
        + if action.is_some() {
            action_w + m.gap
        } else {
            0.0
        };
    let text_width = banner_text_width(rect, dismissible, trailing, theme);
    let (_, title_h) = ctx
        .text
        .measure_style(title, Some(text_width), &title_style);
    let message_h = message.map_or(0.0, |msg| {
        ctx.text
            .measure_style(msg, Some(text_width), &message_style)
            .1
    });
    let gap = f64::from(theme.tokens.spacing.s1);
    let total_text_h = f64::from(title_h + message_h) + if message.is_some() { gap } else { 0.0 };
    let mut ty = rect.center().y - total_text_h / 2.0;
    ctx.text(
        title,
        &title_style,
        on_well.text_primary,
        Point::new(text_x, ty),
        Some(text_width),
    );
    ty += f64::from(title_h) + gap;
    if let Some(msg) = message {
        ctx.text(
            msg,
            &message_style,
            on_well.text_dim,
            Point::new(text_x, ty),
            Some(text_width),
        );
    }

    let mut hits = BannerHits::default();
    if dismissible {
        let x1 = rect.x1 - m.pad_x + (m.dismiss - m.icon) / 2.0;
        let dr = Rect::new(
            x1 - m.dismiss,
            rect.center().y - m.dismiss / 2.0,
            x1,
            rect.center().y + m.dismiss / 2.0,
        );
        let face = crate::widget::ButtonFace::icon_only(dr, Icon::X, "Dismiss")
            .control_size(crate::widget::control_size::ControlSize::Small)
            .hovered(dr.contains(hover));
        crate::widget::Button::paint(ctx.scene, transform, ctx.text, &on_well, &face);
        hits.dismiss = dr;
    }
    if let Some(label) = action {
        let x1 = rect.x1 - dismiss_reserve(dismissible, theme) - reserve;
        let h = crate::widget::ButtonFace::height(ACTION_SIZE, theme);
        let ar = Rect::new(
            x1 - action_w,
            rect.center().y - h / 2.0,
            x1,
            rect.center().y + h / 2.0,
        );
        let face = action_face(ar, label).hovered(ar.contains(hover));
        crate::widget::Button::paint(ctx.scene, transform, ctx.text, &on_well, &face);
        hits.action = ar;
    }
    hits
}

/// The size of a banner's action: it sits inside the box, beside the text.
const ACTION_SIZE: crate::widget::control_size::ControlSize =
    crate::widget::control_size::ControlSize::Small;

fn action_face(rect: Rect, label: &str) -> crate::widget::ButtonFace<'_> {
    crate::widget::ButtonFace::new(rect, label)
        .role(crate::widget::ButtonRole::Secondary)
        .control_size(ACTION_SIZE)
}

/// The room at the trailing edge before any action: the inset, plus the
/// "×" when the banner can be dismissed.
fn dismiss_reserve(dismissible: bool, theme: &Theme) -> f64 {
    let m = Metrics::of(theme);
    if dismissible {
        m.pad_x + m.dismiss + m.gap
    } else {
        m.pad_x
    }
}

/// The height of the title and message wrapped at `text_width`, between the
/// banner's insets.
fn text_block_height(
    title: &str,
    message: Option<&str>,
    text_width: f32,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    let title_style = theme.text(TextRole::Body).strong();
    let (_, title_h) = text_renderer.measure_style(title, Some(text_width), &title_style);
    let message_h = message.map_or(0.0, |message| {
        let style = theme.text(TextRole::Caption);
        text_renderer
            .measure_style(message, Some(text_width), &style)
            .1
    });
    let gap = if message.is_some() {
        f64::from(theme.tokens.spacing.s1)
    } else {
        0.0
    };
    let m = Metrics::of(theme);
    (f64::from(title_h + message_h) + gap + m.pad_y * 2.0).max(m.min_height)
}

fn banner_text_width(rect: Rect, dismissible: bool, reserve: f64, theme: &Theme) -> f32 {
    let m = Metrics::of(theme);
    let trailing = dismiss_reserve(dismissible, theme) + reserve;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "banner geometry is an ordinary logical-pixel value"
    )]
    {
        (rect.width() - m.pad_x - m.icon - m.gap - trailing).max(1.0) as f32
    }
}

/// Severity of a [`Banner`]. Selects the semantic colour and the default
/// leading icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Success,
    Warning,
    Error,
}

impl BannerKind {
    pub fn color(self, theme: &Theme) -> Color {
        let s = &theme.tokens.colors.semantic;
        match self {
            BannerKind::Info => s.info,
            BannerKind::Success => s.success,
            BannerKind::Warning => s.warning,
            BannerKind::Error => s.danger,
        }
    }

    /// The default leading icon for this severity (overridable via
    /// [`Banner::with_icon`]).
    pub fn default_icon(self) -> Icon {
        match self {
            BannerKind::Info => Icon::Info,
            BannerKind::Warning => Icon::TriangleAlert,
            BannerKind::Success => Icon::Check,
            BannerKind::Error => Icon::Ban,
        }
    }
}

/// The text column of a banner that holds an action: a measured leaf the
/// banner lays out beside its action button. It paints nothing; the banner
/// paints its icon and text into it.
struct BannerBody {
    title: String,
    message: Option<String>,
    theme: Theme,
    rect: Rect,
}

impl Widget for BannerBody {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the banner's text height is a small logical-pixel value"
    )]
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            taffy::AvailableSpace::MinContent => 1.0,
            taffy::AvailableSpace::MaxContent => 320.0,
        });
        let m = Metrics::of(&self.theme);
        let text_width = (f64::from(width) - m.pad_x - m.icon - m.gap).max(1.0) as f32;
        let height = text_block_height(
            &self.title,
            self.message.as_deref(),
            text_width,
            tr,
            &self.theme,
        );
        taffy::Size {
            width,
            height: height as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "BannerBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// An inline semantic alert with a title, optional message, an optional
/// action and an optional dismiss affordance.
pub struct Banner {
    id: Option<String>,
    kind: BannerKind,
    title: String,
    message: Option<String>,
    icon: Option<Icon>,
    dismissible: bool,
    /// Persistent "the user closed this" state — collapses the banner. Stays
    /// set so the widget stops painting until the host removes it from its
    /// model.
    dismissed: bool,
    /// One-shot drain signal for [`take_dismissed`](Banner::take_dismissed),
    /// separate from `dismissed` so reading the signal doesn't un-collapse
    /// the banner.
    dismiss_fired: bool,
    /// The one action that resolves what the banner reports, laid out
    /// inside the box at its trailing edge.
    action: Option<Box<super::Button>>,
    body: BannerBody,
    /// The body and action nodes of the last layout, when there is an action.
    action_nodes: Option<(NodeId, NodeId)>,
    rect: Rect,
    dismiss_rect: Rect,
    last_cursor: Point,
    layout_theme: Theme,
}

impl Banner {
    pub fn new(kind: BannerKind, title: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: title.into(),
            message: None,
            icon: None,
            dismissible: false,
            dismissed: false,
            dismiss_fired: false,
            action: None,
            body: BannerBody {
                title: String::new(),
                message: None,
                theme: Theme::dark(),
                rect: Rect::ZERO,
            },
            action_nodes: None,
            rect: Rect::ZERO,
            dismiss_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            layout_theme: Theme::dark(),
        }
    }

    /// A stable id for tests, inspection and agents: the banner's
    /// accessibility node carries it as its semantic key, so a test can
    /// address an error notice by name.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The one action that resolves what the banner reports ("Try again",
    /// "Restart timer"), inside the box at its trailing edge. It is an
    /// ordinary [`Button`](super::Button), bound like any other; the banner
    /// sets its role and size.
    #[must_use]
    pub fn with_action(mut self, action: super::Button) -> Self {
        self.action = Some(Box::new(
            action
                .role(super::ButtonRole::Secondary)
                .control_size(ACTION_SIZE),
        ));
        self
    }

    pub fn info(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Info, title)
    }
    pub fn success(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Success, title)
    }
    pub fn warning(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Warning, title)
    }
    pub fn error(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Error, title)
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Show a trailing "×" the user can click to dismiss.
    #[must_use]
    pub fn dismissible(mut self) -> Self {
        self.dismissible = true;
        self
    }

    /// Drain the "user clicked ×" signal (fires once). The host removes the
    /// banner from its own model in response. Draining this does **not**
    /// un-collapse the banner — [`is_dismissed`](Self::is_dismissed) stays
    /// `true`.
    pub fn take_dismissed(&mut self) -> bool {
        std::mem::take(&mut self.dismiss_fired)
    }

    /// Whether this banner has been dismissed (and now collapses to nothing).
    pub fn is_dismissed(&self) -> bool {
        self.dismissed
    }

    /// The effective leading icon (override or severity default).
    pub fn icon(&self) -> Icon {
        self.icon.unwrap_or_else(|| self.kind.default_icon())
    }

    /// Immediate-mode paint of a banner filling `rect`, for apps that render
    /// straight to a `Scene`. Returns the dismiss "×" rect (or `Rect::ZERO`
    /// when not dismissible) so the caller can hit-test it against its own
    /// pointer state; pass `hover` for the ×-hover tint.
    pub fn paint(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &BannerContent<'_>,
        hover: Point,
    ) -> BannerHits {
        paint_banner(ctx, rect, content, hover, 0.0)
    }

    /// Intrinsic content height for the current title/message, in logical px.
    fn content_height(&self, theme: &Theme, text_renderer: &mut TextRenderer, width: f32) -> f64 {
        let rect = Rect::new(0.0, 0.0, f64::from(width.max(1.0)), 1.0);
        let text_width = banner_text_width(rect, self.dismissible, 0.0, theme);
        text_block_height(
            &self.title,
            self.message.as_deref(),
            text_width,
            text_renderer,
            theme,
        )
    }

    fn has_action(&self) -> bool {
        self.action.is_some() && !self.dismissed
    }
}

impl Widget for Banner {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "content_height is a small, bounded pixel value built from theme typography \
                  constants; f32 precision loss is immaterial for widget layout"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        self.action_nodes = None;
        if self.has_action()
            && let Some(action) = self.action.as_mut()
        {
            self.body.title.clone_from(&self.title);
            self.body.message.clone_from(&self.message);
            self.body.theme = theme.clone();
            let body = self.body.build_layout(taffy, theme, node_map);
            let button = action.build_layout(taffy, theme, node_map);
            let m = Metrics::of(theme);
            let node = taffy.parent_node(
                Style {
                    flex_direction: taffy::FlexDirection::Row,
                    align_items: Some(taffy::AlignItems::Center),
                    flex_shrink: 0.0,
                    size: taffy::Size {
                        width: percent(1.0_f32),
                        height: taffy::Dimension::auto(),
                    },
                    min_size: taffy::Size {
                        width: taffy::Dimension::auto(),
                        height: length(m.min_height as f32),
                    },
                    padding: taffy::Rect {
                        left: length(0.0_f32),
                        right: length(dismiss_reserve(self.dismissible, theme) as f32),
                        top: length(0.0_f32),
                        bottom: length(0.0_f32),
                    },
                    gap: taffy::Size {
                        width: length(m.gap as f32),
                        height: length(0.0_f32),
                    },
                    ..Default::default()
                },
                &[body, button],
            );
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            self.action_nodes = Some((body, button));
            return node;
        }
        let node = taffy.leaf_node(Style {
            // A dismissed banner leaves its stack, gap and all.
            display: if self.dismissed {
                taffy::Display::None
            } else {
                taffy::Display::Flex
            },
            flex_shrink: 0.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: if self.dismissed {
                    length(0.0_f32)
                } else {
                    taffy::Dimension::auto()
                },
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
        tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            taffy::AvailableSpace::MinContent => 1.0,
            taffy::AvailableSpace::MaxContent => 320.0,
        });
        let h = if self.dismissed {
            0.0
        } else {
            self.content_height(&self.layout_theme, tr, width) as f32
        };
        taffy::Size { width, height: h }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some((body, button)) = self.action_nodes
            && let Some(action) = self.action.as_mut()
        {
            let inner = Point::new(self.rect.x0, self.rect.y0);
            self.body.apply_layout(taffy, body, inner);
            action.apply_layout(taffy, button, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.dismissed {
            self.dismiss_rect = Rect::ZERO;
            return;
        }
        let content = BannerContent::new(self.kind, &self.title)
            .with_icon(self.icon())
            .with_message(self.message.as_deref())
            .dismissible(self.dismissible);
        let reserve = match (&self.action, self.action_nodes) {
            (Some(action), Some(_)) => action.get_rect().width() + Metrics::of(theme).gap,
            _ => 0.0,
        };
        self.dismiss_rect = paint_banner(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &content,
            self.last_cursor,
            reserve,
        )
        .dismiss;
        if self.action_nodes.is_some()
            && let Some(action) = self.action.as_mut()
        {
            // The action sits on the banner's raised well.
            let on_well = theme.on_ground(theme.tokens.colors.surfaces.raised);
            action.draw(scene, transform, text_renderer, &on_well, presence);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        match (&self.action, self.action_nodes) {
            (Some(action), Some(_)) => vec![&self.body, action.as_ref()],
            _ => Vec::new(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match (&mut self.action, self.action_nodes) {
            (Some(action), Some(_)) => vec![&mut self.body, action.as_mut()],
            _ => Vec::new(),
        }
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some((_, button)) = self.action_nodes
            && let Some(action) = self.action.as_mut()
        {
            let path = ctx.path;
            action.handle_event(event, &mut ctx.reborrow(path, button));
            if *ctx.handled {
                return;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.dismissible
                && !self.dismissed
                && self.dismiss_rect.contains(self.last_cursor) =>
            {
                self.dismissed = true;
                self.dismiss_fired = true;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.dismissible && self.dismiss_rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "Banner"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        if self.dismissed {
            return None;
        }
        // A banner is a live status region — the reader announces it when it
        // appears. Errors/warnings are assertive; info/success are polite.
        let politeness = match self.kind {
            BannerKind::Error | BannerKind::Warning => Politeness::Assertive,
            _ => Politeness::Polite,
        };
        let mut node = AccessNode::new(Role::Group)
            .with_name(self.title.clone())
            .with_live(politeness);
        if let Some(id) = &self.id {
            node = node.with_semantic_key(id.clone());
        }
        if let Some(msg) = &self.message {
            node = node.with_description(msg.clone());
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    #[test]
    fn banner_kind_icon_and_message_drive_paint_and_height() {
        fn retained_measure_uses_the_active_text_scale() {
            let mut banner = Banner::info("A title").with_message("Supporting detail");
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            banner.build_layout(&mut taffy, &Theme::dark().with_text_scale(2.0), &mut map);
            let mut text = TextRenderer::new();
            let measured = banner.measure(
                taffy::Size {
                    width: Some(320.0),
                    height: None,
                },
                taffy::Size::MAX_CONTENT,
                &mut text,
            );
            let default_height = Banner::info("A title")
                .with_message("Supporting detail")
                .content_height(&Theme::dark(), &mut text, 320.0);
            assert!(f64::from(measured.height) > default_height);
        }

        fn kind_maps_to_semantic_color_and_icon() {
            let theme = Theme::dark();
            assert_eq!(
                BannerKind::Error.color(&theme),
                theme.tokens.colors.semantic.danger
            );
            assert_eq!(
                BannerKind::Success.color(&theme),
                theme.tokens.colors.semantic.success
            );
            assert_eq!(
                BannerKind::Warning.color(&theme),
                theme.tokens.colors.semantic.warning
            );
            assert_eq!(
                BannerKind::Info.color(&theme),
                theme.tokens.colors.semantic.info
            );
            assert_eq!(BannerKind::Success.default_icon(), Icon::Check);
        }

        fn icon_override_wins() {
            let b = Banner::info("Heads up").with_icon(Icon::Bell);
            assert_eq!(b.icon(), Icon::Bell);
            let d = Banner::info("Heads up");
            assert_eq!(d.icon(), Icon::Info, "defaults to the severity icon");
        }

        fn message_makes_the_banner_taller() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let short = Banner::info("One line").content_height(&theme, &mut text, 500.0);
            let tall = Banner::info("One line")
                .with_message("Plus a detail line")
                .content_height(&theme, &mut text, 500.0);
            assert!(tall > short, "a message adds a second line of height");
        }

        fn long_message_wraps_and_grows_to_its_painted_height() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let message = "Tilo found your identity metadata, but its private key is unavailable from the desktop keyring. Unlock the keyring and retry.";
            let banner =
                Banner::warning("Your existing identity needs recovery").with_message(message);
            let width = 700.0;
            let height = banner.content_height(&theme, &mut text, width);
            let text_width = banner_text_width(
                Rect::new(0.0, 0.0, f64::from(width), height),
                false,
                0.0,
                &theme,
            );
            let (_, message_height) =
                text.measure_style(message, Some(text_width), &theme.text(TextRole::Caption));

            assert!(
                message_height > theme.tokens.typography.size(TextRole::Caption) * 1.25,
                "the recovery copy must wrap instead of escaping the banner"
            );
            assert!(
                height >= f64::from(message_height) + Metrics::of(&theme).pad_y * 2.0,
                "the banner must reserve the same wrapped height that it paints"
            );
        }

        fn draws_well_icon_and_text() {
            let mut b = Banner::error("Upload failed")
                .with_message("Check your connection")
                .dismissible();
            lay_out(&mut b, 500.0, 200.0);
            let scene = draw(&mut b);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(
                fills > 4,
                "surface + border + bar + icon + text ⇒ many fills, got {fills}"
            );
        }

        retained_measure_uses_the_active_text_scale();
        kind_maps_to_semantic_color_and_icon();
        icon_override_wins();
        message_makes_the_banner_taller();
        long_message_wraps_and_grows_to_its_painted_height();
        draws_well_icon_and_text();
    }

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

    #[test]
    fn banner_dismissal_and_live_region_semantics() {
        fn dismiss_click_fires_once_and_collapses() {
            let mut b = Banner::warning("Offline")
                .with_message("Changes will sync later")
                .dismissible();
            lay_out(&mut b, 500.0, 200.0);
            let _ = draw(&mut b);
            let target = b.dismiss_rect.center();
            assert!(target.x > 0.0, "dismiss rect resolves after draw");
            dispatch(&mut b, &cursor(target));
            assert!(dispatch(&mut b, &press()), "clicking × is consumed");
            assert!(b.take_dismissed(), "dismiss signal fires");
            assert!(!b.take_dismissed(), "dismiss signal drains after one read");
            assert!(b.is_dismissed());
            let scene = draw(&mut b);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert_eq!(fills, 0, "a dismissed banner paints nothing");
        }

        fn non_dismissible_has_no_close_target() {
            let mut b = Banner::success("Saved");
            lay_out(&mut b, 500.0, 200.0);
            let _ = draw(&mut b);
            assert_eq!(b.dismiss_rect, Rect::ZERO, "no × when not dismissible");
            let center = b.get_rect().center();
            dispatch(&mut b, &cursor(center));
            assert!(!dispatch(&mut b, &press()), "a body click is not consumed");
            assert!(!b.take_dismissed());
        }

        fn accessibility_is_live_region_with_severity_politeness() {
            use crate::a11y::{Politeness, Role};
            let err = Banner::error("Boom").with_message("details");
            let node = Widget::accessibility(&err).expect("banner exposes a11y");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Boom"));
            assert_eq!(
                node.live,
                Some(Politeness::Assertive),
                "errors announce assertively"
            );
            let info = Banner::info("FYI");
            assert_eq!(
                Widget::accessibility(&info).unwrap().live,
                Some(Politeness::Polite)
            );
        }

        dismiss_click_fires_once_and_collapses();
        non_dismissible_has_no_close_target();
        accessibility_is_live_region_with_severity_politeness();
    }
}
