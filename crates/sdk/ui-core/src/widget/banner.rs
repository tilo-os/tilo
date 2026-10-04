//! `Banner` — an inline, semantic alert strip. Unlike a [`super::toast::Toast`]
//! (transient, auto-expiring) a banner is a *persistent* in-flow message the
//! person resolves or dismisses: "You're offline — changes will sync when you
//! reconnect", "This file has unsaved changes", "You're in a private window".
//!
//! A [`BannerKind`] decides the leading icon and its colour; the well, the
//! ink and the buttons come from the theme. A banner carries a **title**, an
//! optional secondary **message**, up to two actions laid out inside its box
//! at the trailing edge (they wrap under the message when the banner is
//! narrow), and an optional dismiss control. The actions and the dismiss
//! control are real [`Button`]s: focusable, named, and published as actions,
//! so an app never places its own buttons beside a banner.
//!
//! [`Banner::paint`] and [`Banner::height`] draw the same banner for a host
//! that paints into a `Scene`, and hand back where its buttons are.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::control_size::ControlSize;
use crate::widget::{Actions, Button, ButtonFace, ButtonRole, TaffyTreeExt, Widget};

/// The banner's metrics, from the theme.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    pad_x: f64,
    pad_y: f64,
    icon: f64,
    dismiss: f64,
    /// Between the icon and the text, and before the trailing controls.
    gap: f64,
    /// Between the message and actions that wrapped under it.
    row_gap: f64,
    /// Between two actions.
    related: f64,
    min_height: f64,
    /// The narrowest the text may get before the actions wrap under it.
    text_basis: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let t = &theme.tokens;
        Self {
            pad_x: f64::from(t.spacing.s4),
            pad_y: f64::from(t.spacing.s3),
            icon: f64::from(t.metrics.icon_md),
            dismiss: ButtonFace::height(ControlSize::Small, theme),
            gap: f64::from(t.spacing.s3),
            row_gap: f64::from(t.spacing.s2),
            related: f64::from(t.spacing.rhythm.related),
            min_height: f64::from(t.metrics.panel_row_height),
            text_basis: f64::from(theme.text(TextRole::Body).line_height_px()) * 12.0,
        }
    }
}

fn title_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Body).strong()
}

fn message_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

/// The ground a banner stands on: a raised well in the content.
fn well(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.raised
}

/// What an immediate-mode [`Banner::paint`] shows: severity, leading icon,
/// title, optional message, up to two actions and whether it offers a
/// dismiss control.
#[derive(Clone, Copy, Debug)]
pub struct BannerContent<'a> {
    pub kind: BannerKind,
    pub icon: Icon,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub dismissible: bool,
    /// The banner's one main action ("Try again", "Restore").
    pub action: Option<&'a str>,
    /// A second action, before the main one ("Discard").
    pub secondary_action: Option<&'a str>,
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
            secondary_action: None,
        }
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
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

    /// The banner's main action, at its trailing edge.
    #[must_use]
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    /// A second action, before the main one.
    #[must_use]
    pub fn secondary_action(mut self, label: &'a str) -> Self {
        self.secondary_action = Some(label);
        self
    }
}

/// Where [`Banner::paint`] put the banner and its controls, for the caller's
/// hit-testing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BannerHit {
    /// The banner's box.
    pub rect: Rect,
    /// The main action's face, when the banner has one.
    pub action: Option<Rect>,
    /// The second action's face, when the banner has one.
    pub secondary_action: Option<Rect>,
    /// The dismiss control's face, when the banner is dismissible.
    pub dismiss: Option<Rect>,
}

/// The node's box in absolute coordinates.
fn layout_rect(taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Rect {
    let l = taffy.layout(node).copied().unwrap_or_default();
    Rect::from_origin_size(
        (
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
        ),
        (f64::from(l.size.width), f64::from(l.size.height)),
    )
}

/// The face of a banner's action: the main one a secondary control, the
/// other a plain one, both small.
fn action_face(rect: Rect, label: &str, primary: bool) -> ButtonFace<'_> {
    ButtonFace::new(rect, label)
        .role(if primary {
            ButtonRole::Secondary
        } else {
            ButtonRole::Plain
        })
        .control_size(ControlSize::Small)
}

/// One banner geometry, shared by the retained widget's measure and the
/// immediate painter. Coordinates are relative to the banner's top-left.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    height: f64,
    icon: Rect,
    text: Point,
    text_width: f32,
    title_h: f64,
    action: Option<Rect>,
    secondary_action: Option<Rect>,
    dismiss: Option<Rect>,
}

fn geometry(
    content: &BannerContent<'_>,
    width: f64,
    text: &mut TextRenderer,
    theme: &Theme,
) -> Geometry {
    let m = Metrics::of(theme);
    let control_h = ButtonFace::height(ControlSize::Small, theme);
    let trailing = if content.dismissible {
        m.dismiss + m.gap
    } else {
        0.0
    };
    // The body: everything between the padding and the dismiss column.
    let body_x0 = m.pad_x;
    let body_x1 = (width - m.pad_x - trailing).max(body_x0 + 1.0);
    let body_w = body_x1 - body_x0;
    let mut widths = Vec::new();
    if let Some(label) = content.secondary_action {
        widths.push(action_face(Rect::ZERO, label, false).intrinsic_width(text, theme));
    }
    if let Some(label) = content.action {
        widths.push(action_face(Rect::ZERO, label, true).intrinsic_width(text, theme));
    }
    let actions_w = widths.iter().sum::<f64>()
        + m.related * crate::num::f64_of_usize(widths.len().saturating_sub(1));
    let leading = m.icon + m.gap;
    let beside = widths.is_empty() || body_w >= leading + m.text_basis + m.gap + actions_w;
    let text_w = if widths.is_empty() || !beside {
        body_w - leading
    } else {
        body_w - leading - m.gap - actions_w
    }
    .max(1.0);
    let text_width = crate::num::f32_of_f64(text_w);
    let title_h = f64::from(
        text.measure_style(content.title, Some(text_width), &title_style(theme))
            .1,
    );
    let message_h = content.message.map_or(0.0, |message| {
        f64::from(theme.tokens.spacing.s1)
            + f64::from(
                text.measure_style(message, Some(text_width), &message_style(theme))
                    .1,
            )
    });
    let text_h = (title_h + message_h).max(m.icon);
    let (line_h, actions_y) = if widths.is_empty() {
        (text_h, 0.0)
    } else if beside {
        (text_h.max(control_h), 0.0)
    } else {
        (text_h + m.row_gap + control_h, text_h + m.row_gap)
    };
    let natural = line_h + 2.0 * m.pad_y;
    let height = natural.max(m.min_height);
    let top = (height - line_h) / 2.0;
    let text_top = if beside {
        top + (line_h - text_h) / 2.0
    } else {
        top
    };
    let icon = Rect::from_center_size(
        Point::new(body_x0 + m.icon / 2.0, text_top + text_h / 2.0),
        (m.icon, m.icon),
    );
    let text_origin = Point::new(
        body_x0 + leading,
        text_top + (text_h - (title_h + message_h)) / 2.0,
    );
    // Actions are laid out from the trailing edge back: the main action
    // last, so it sits at the edge.
    let actions_top = if beside {
        top + (line_h - control_h) / 2.0
    } else {
        top + actions_y
    };
    let mut x = body_x1;
    let mut rects = Vec::new();
    for w in widths.iter().rev() {
        rects.push(Rect::new(x - w, actions_top, x, actions_top + control_h));
        x -= w + m.related;
    }
    let (action, secondary_action) = match (content.action, content.secondary_action) {
        (Some(_), Some(_)) => (rects.first().copied(), rects.get(1).copied()),
        (Some(_), None) => (rects.first().copied(), None),
        (None, Some(_)) => (None, rects.first().copied()),
        (None, None) => (None, None),
    };
    let dismiss = content.dismissible.then(|| {
        let x1 = width - m.pad_x;
        // The dismiss control centres on the first line of the banner.
        let cy = if beside {
            height / 2.0
        } else {
            text_top + title_h.max(m.icon) / 2.0
        };
        Rect::new(
            x1 - m.dismiss,
            cy - m.dismiss / 2.0,
            x1,
            cy + m.dismiss / 2.0,
        )
    });
    Geometry {
        height,
        icon,
        text: text_origin,
        text_width,
        title_h,
        action,
        secondary_action,
        dismiss,
    }
}

/// Paint the banner's well into `rect`.
fn paint_well(ctx: &mut PaintCtx<'_>, rect: Rect) {
    let theme = ctx.theme;
    let radius = f64::from(theme.tokens.radii.control);
    let well = well(theme);
    ctx.fill_round(rect, radius, well);
    ctx.stroke_round(rect, radius, crate::surface::edge_on(well), 1.0);
}

/// Where the body's icon and text go, in absolute coordinates.
#[derive(Clone, Copy, Debug)]
struct BodyPlace {
    icon: Rect,
    text: Point,
    text_width: f32,
    title_h: f64,
}

/// Paint the icon, title and message of `content` at `place`.
fn paint_body(ctx: &mut PaintCtx<'_>, content: &BannerContent<'_>, place: BodyPlace) {
    let BodyPlace {
        icon: icon_rect,
        text,
        text_width,
        title_h,
    } = place;
    let (kind, icon, title, message) = (content.kind, content.icon, content.title, content.message);
    let theme = ctx.theme;
    let on_well = theme.on_ground(well(theme));
    icon.draw(ctx.scene, ctx.transform, icon_rect, kind.icon_ink(&on_well));
    ctx.text(
        title,
        &title_style(theme),
        on_well.text_primary,
        text,
        Some(text_width),
    );
    if let Some(message) = message {
        let y = text.y + title_h + f64::from(theme.tokens.spacing.s1);
        ctx.text(
            message,
            &message_style(theme),
            on_well.text_dim,
            Point::new(text.x, y),
            Some(text_width),
        );
    }
}

fn paint_banner(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &BannerContent<'_>,
    hover: Point,
) -> BannerHit {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return BannerHit::default();
    }
    let theme = ctx.theme.clone();
    let mut g = geometry(content, rect.width(), ctx.text, &theme);
    // A taller rect than the banner needs centres the content in it.
    let dy = ((rect.height() - g.height) / 2.0).max(0.0);
    let origin = rect.origin().to_vec2() + vello::kurbo::Vec2::new(0.0, dy);
    let shift = |r: Rect| r + origin;
    g.icon = shift(g.icon);
    g.text += origin;
    paint_well(ctx, rect);
    paint_body(
        ctx,
        content,
        BodyPlace {
            icon: g.icon,
            text: g.text,
            text_width: g.text_width,
            title_h: g.title_h,
        },
    );
    let on_well = theme.on_ground(well(&theme));
    let mut hit = BannerHit {
        rect,
        ..BannerHit::default()
    };
    for (label, face_rect, primary) in [
        (content.secondary_action, g.secondary_action, false),
        (content.action, g.action, true),
    ] {
        if let (Some(label), Some(face_rect)) = (label, face_rect) {
            let face_rect = shift(face_rect);
            let face = action_face(face_rect, label, primary);
            let hovered = face.contains(&on_well, hover);
            let face = face.hovered(hovered);
            crate::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, &on_well, &face);
            if primary {
                hit.action = Some(face_rect);
            } else {
                hit.secondary_action = Some(face_rect);
            }
        }
    }
    if let Some(dismiss) = g.dismiss {
        let dismiss = shift(dismiss);
        let face = ButtonFace::icon_only(dismiss, Icon::X, "Dismiss")
            .control_size(ControlSize::Small)
            .hovered(dismiss.contains(hover));
        crate::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, &on_well, &face);
        hit.dismiss = Some(dismiss);
    }
    hit
}

/// Severity of a [`Banner`]. Selects the icon and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    /// A notice where nothing is wrong (a private window, a read-only
    /// document): primary ink, no hue.
    Neutral,
    Info,
    Success,
    Warning,
    Error,
}

impl BannerKind {
    /// The status colour of this severity; primary ink for
    /// [`BannerKind::Neutral`].
    pub fn color(self, theme: &Theme) -> Color {
        let s = &theme.tokens.colors.semantic;
        match self {
            BannerKind::Neutral => theme.text_primary,
            BannerKind::Info => s.info,
            BannerKind::Success => s.success,
            BannerKind::Warning => s.warning,
            BannerKind::Error => s.danger,
        }
    }

    /// The icon ink on `on_well`, the theme resolved for the banner's well.
    fn icon_ink(self, on_well: &Theme) -> Color {
        self.color(on_well)
    }

    /// The default leading icon for this severity (overridable via
    /// [`Banner::with_icon`]).
    pub fn default_icon(self) -> Icon {
        match self {
            BannerKind::Neutral | BannerKind::Info => Icon::Info,
            BannerKind::Warning => Icon::TriangleAlert,
            BannerKind::Success => Icon::Check,
            BannerKind::Error => Icon::Ban,
        }
    }

    fn politeness(self) -> crate::a11y::Politeness {
        match self {
            BannerKind::Error | BannerKind::Warning => crate::a11y::Politeness::Assertive,
            _ => crate::a11y::Politeness::Polite,
        }
    }
}

/// What a state-bound banner shows ([`Banner::from_state`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BannerNotice {
    pub kind: BannerKind,
    pub title: String,
    pub message: Option<String>,
    pub icon: Option<Icon>,
}

impl BannerNotice {
    pub fn new(kind: BannerKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            message: None,
            icon: None,
        }
    }
    pub fn neutral(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Neutral, title)
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
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

// ---------------------------------------------------------------------------
// Retained parts: the body (icon and text) and the row that wraps it with
// the actions.
// ---------------------------------------------------------------------------

/// The icon, title and message: a measured leaf.
struct BannerBody {
    notice: BannerNotice,
    rect: Rect,
    theme: Theme,
    dirty: bool,
    layout_dirty: bool,
}

impl BannerBody {
    fn text_width(&self, width: f64) -> f32 {
        let m = Metrics::of(&self.theme);
        crate::num::f32_of_f64((width - m.icon - m.gap).max(1.0))
    }

    fn heights(&self, text: &mut TextRenderer, width: f64) -> (f64, f64) {
        let tw = self.text_width(width);
        let title_h = f64::from(
            text.measure_style(&self.notice.title, Some(tw), &title_style(&self.theme))
                .1,
        );
        let message_h = self.notice.message.as_ref().map_or(0.0, |message| {
            f64::from(self.theme.tokens.spacing.s1)
                + f64::from(
                    text.measure_style(message, Some(tw), &message_style(&self.theme))
                        .1,
                )
        });
        (title_h, message_h)
    }
}

impl Widget for BannerBody {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let m = Metrics::of(theme);
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: length(crate::num::f32_of_f64(m.icon + m.gap + m.text_basis)),
            min_size: Size {
                width: length(crate::num::f32_of_f64(m.icon + m.gap)),
                height: auto(),
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
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let m = Metrics::of(&self.theme);
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            taffy::AvailableSpace::MinContent => crate::num::f32_of_f64(m.icon + m.gap),
            taffy::AvailableSpace::MaxContent => {
                crate::num::f32_of_f64(m.icon + m.gap + m.text_basis)
            }
        });
        let (title_h, message_h) = self.heights(text, f64::from(width));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64((title_h + message_h).max(m.icon)),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
        self.layout_dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let m = Metrics::of(theme);
        let (title_h, message_h) = self.heights(text, self.rect.width());
        let text_h = title_h + message_h;
        let icon = Rect::from_center_size(
            Point::new(self.rect.x0 + m.icon / 2.0, self.rect.center().y),
            (m.icon, m.icon),
        );
        let origin = Point::new(
            self.rect.x0 + m.icon + m.gap,
            self.rect.center().y - text_h / 2.0,
        );
        let n = &self.notice;
        let content = BannerContent::new(n.kind, &n.title)
            .with_icon(n.icon.unwrap_or_else(|| n.kind.default_icon()))
            .with_message(n.message.as_deref());
        let place = BodyPlace {
            icon,
            text: origin,
            text_width: self.text_width(self.rect.width()),
            title_h,
        };
        paint_body(
            &mut PaintCtx::new(scene, transform, text, theme),
            &content,
            place,
        );
    }

    fn name(&self) -> &'static str {
        "BannerBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.rect == Rect::ZERO
    }
}

/// The body and the actions, in one row that wraps the actions under the
/// body when there is no room beside it.
struct BannerMain {
    body: BannerBody,
    actions: Option<Actions>,
    nodes: Vec<NodeId>,
    rect: Rect,
}

impl Widget for BannerMain {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let m = Metrics::of(theme);
        self.nodes = vec![self.body.build_layout(taffy, theme, node_map)];
        if let Some(actions) = &mut self.actions {
            self.nodes
                .push(actions.build_layout(taffy, theme, node_map));
        }
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_wrap: taffy::FlexWrap::Wrap,
                align_items: Some(taffy::AlignItems::Center),
                justify_content: Some(taffy::JustifyContent::FlexEnd),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0_f32),
                min_size: Size {
                    width: length(0.0_f32),
                    height: auto(),
                },
                gap: Size {
                    width: length(crate::num::f32_of_f64(m.gap)),
                    height: length(crate::num::f32_of_f64(m.row_gap)),
                },
                ..Default::default()
            },
            &self.nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
        let origin = self.rect.origin();
        if let Some(&node) = self.nodes.first() {
            self.body.apply_layout(taffy, node, origin);
        }
        if let (Some(actions), Some(&node)) = (&mut self.actions, self.nodes.get(1)) {
            actions.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.body.draw(scene, transform, text, theme, presence);
        if let Some(actions) = &mut self.actions {
            actions.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if let (Some(actions), Some(&node)) = (&mut self.actions, self.nodes.get(1)) {
            actions.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
    }

    fn update(
        &mut self,
        state: &mut crate::widget::AppModel,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.actions
            .as_mut()
            .is_some_and(|actions| actions.update(state, path))
    }

    fn sync(&mut self, state: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
        if let Some(actions) = &mut self.actions {
            actions.sync(state, path);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.body];
        if let Some(actions) = &self.actions {
            out.push(actions);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.body];
        if let Some(actions) = &mut self.actions {
            out.push(actions);
        }
        out
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.actions
            .as_mut()
            .is_some_and(Widget::take_structure_dirty)
    }

    fn name(&self) -> &'static str {
        "BannerRow"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

// ---------------------------------------------------------------------------
// Banner — retained.
// ---------------------------------------------------------------------------

/// An inline semantic alert with a title, an optional message, up to two
/// actions and an optional dismiss control.
pub struct Banner {
    main: BannerMain,
    dismiss: Option<Button>,
    id: Option<String>,
    /// Absent: the banner collapses, and with it its gap in the parent.
    shown: bool,
    source: Option<LocalBinding<Option<BannerNotice>>>,
    /// Persistent "the person closed this" state; collapses the banner until
    /// the host shows another notice.
    dismissed: bool,
    /// One-shot drain signal for [`take_dismissed`](Banner::take_dismissed).
    dismiss_fired: bool,
    /// Set by the dismiss control's action; read on the next sync.
    dismiss_requested: Rc<Cell<bool>>,
    on_dismiss: Option<Box<dyn FnMut()>>,
    nodes: Vec<NodeId>,
    rect: Rect,
    layout_dirty: bool,
    dirty: bool,
}

impl Banner {
    pub fn new(kind: BannerKind, title: impl Into<String>) -> Self {
        Self::from_notice(Some(BannerNotice::new(kind, title)))
    }

    fn from_notice(notice: Option<BannerNotice>) -> Self {
        let shown = notice.is_some();
        Self {
            main: BannerMain {
                body: BannerBody {
                    notice: notice.unwrap_or_else(|| BannerNotice::neutral("")),
                    rect: Rect::ZERO,
                    theme: Theme::dark(),
                    dirty: true,
                    layout_dirty: true,
                },
                actions: None,
                nodes: Vec::new(),
                rect: Rect::ZERO,
            },
            dismiss: None,
            id: None,
            shown,
            source: None,
            dismissed: false,
            dismiss_fired: false,
            dismiss_requested: Rc::new(Cell::new(false)),
            on_dismiss: None,
            nodes: Vec::new(),
            rect: Rect::ZERO,
            layout_dirty: true,
            dirty: true,
        }
    }

    /// A banner bound to `state`: it shows the notice while there is one and
    /// collapses, with its gap, while the state holds `None`. A new notice
    /// clears an earlier dismissal.
    pub fn from_state(state: &crate::LocalState<Option<BannerNotice>>) -> Self {
        let (notice, binding) = LocalBinding::new(state);
        let mut banner = Self::from_notice(notice);
        banner.source = Some(binding);
        banner
    }

    pub fn neutral(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Neutral, title)
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
        self.main.body.notice.message = Some(message.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.main.body.notice.icon = Some(icon);
        self
    }

    /// Name the banner for tests, scenarios and inspection. Its dismiss
    /// control is `<id>.dismiss`.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        if self.dismiss.is_some() {
            self = self.dismissible();
        }
        self
    }

    /// The banner's main action, inside its box at the trailing edge. The
    /// banner sizes it; the button keeps its role.
    #[must_use]
    pub fn with_action(mut self, button: Button) -> Self {
        let button = button.control_size(ControlSize::Small);
        self.main.actions = Some(match self.main.actions.take() {
            Some(actions) => actions.replace_primary(button),
            None => Actions::new(button),
        });
        self
    }

    /// A second action, before the main one.
    #[must_use]
    pub fn with_secondary_action(mut self, button: Button) -> Self {
        let button = button.control_size(ControlSize::Small);
        self.main.actions = Some(match self.main.actions.take() {
            Some(actions) => actions.secondary(button),
            None => Actions::new(button),
        });
        self
    }

    /// Offer a dismiss control at the trailing edge: a focusable icon button
    /// named "Dismiss".
    #[must_use]
    pub fn dismissible(mut self) -> Self {
        let id = format!("{}.dismiss", self.id.as_deref().unwrap_or("banner"));
        let requested = Rc::clone(&self.dismiss_requested);
        self.dismiss = Some(
            Button::icon_only(&id, Icon::X, "Dismiss")
                .control_size(ControlSize::Small)
                .on_view(move || requested.set(true)),
        );
        self
    }

    /// Run `f` once when the person dismisses the banner. Implies
    /// [`Self::dismissible`].
    #[must_use]
    pub fn on_dismiss(mut self, f: impl FnMut() + 'static) -> Self {
        if self.dismiss.is_none() {
            self = self.dismissible();
        }
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Drain the "the person dismissed this" signal (fires once). Draining
    /// does **not** show the banner again; [`is_dismissed`](Self::is_dismissed)
    /// stays `true`.
    pub fn take_dismissed(&mut self) -> bool {
        std::mem::take(&mut self.dismiss_fired)
    }

    /// Whether this banner has been dismissed (and now collapses).
    pub fn is_dismissed(&self) -> bool {
        self.dismissed
    }

    /// Whether the banner shows: it has a notice and was not dismissed.
    pub fn is_shown(&self) -> bool {
        self.shown && !self.dismissed
    }

    /// The effective leading icon (override or severity default).
    pub fn icon(&self) -> Icon {
        let notice = &self.main.body.notice;
        notice.icon.unwrap_or_else(|| notice.kind.default_icon())
    }

    /// Replace what the banner says. `None` collapses it.
    pub fn set_notice(&mut self, notice: Option<BannerNotice>) {
        match notice {
            Some(notice) => {
                if !self.shown || self.dismissed || self.main.body.notice != notice {
                    self.main.body.notice = notice;
                    self.shown = true;
                    self.dismissed = false;
                    self.changed();
                }
            }
            None => {
                if self.shown {
                    self.shown = false;
                    self.changed();
                }
            }
        }
    }

    fn changed(&mut self) {
        self.layout_dirty = true;
        self.dirty = true;
        self.main.body.layout_dirty = true;
        self.main.body.dirty = true;
    }

    fn dismiss_now(&mut self) {
        if self.dismissed {
            return;
        }
        self.dismissed = true;
        self.dismiss_fired = true;
        if let Some(f) = &mut self.on_dismiss {
            f();
        }
        self.changed();
    }

    /// Immediate-mode paint of a banner filling `rect`, for apps that render
    /// straight to a `Scene`. Size `rect` with [`Self::height`]. Returns where
    /// the banner and its controls are so the caller can hit-test them; pass
    /// `hover` for the pointer's hover state.
    pub fn paint(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &BannerContent<'_>,
        hover: Point,
    ) -> BannerHit {
        paint_banner(ctx, rect, content, hover)
    }

    /// Where [`Self::paint`] would put the banner's controls in `rect`,
    /// without painting: for a host that hit-tests before it paints.
    pub fn layout(
        content: &BannerContent<'_>,
        rect: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> BannerHit {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return BannerHit::default();
        }
        let g = geometry(content, rect.width(), text, theme);
        let dy = ((rect.height() - g.height) / 2.0).max(0.0);
        let origin = rect.origin().to_vec2() + vello::kurbo::Vec2::new(0.0, dy);
        BannerHit {
            rect,
            action: g.action.map(|r| r + origin),
            secondary_action: g.secondary_action.map(|r| r + origin),
            dismiss: g.dismiss.map(|r| r + origin),
        }
    }

    /// The height a banner with `content` takes at `width`: its text, and
    /// its actions beside or under it, never below a panel row.
    pub fn height(
        content: &BannerContent<'_>,
        width: f64,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        geometry(content, width, text, theme).height.ceil()
    }

    fn poll_source(&mut self) {
        if let Some(notice) = self.source.as_mut().and_then(LocalBinding::poll) {
            self.set_notice(notice);
        }
    }

    fn visible(&self) -> bool {
        self.shown && !self.dismissed
    }
}

impl Widget for Banner {
    crate::provides!(dyn crate::undo::UndoPoints);

    fn update(
        &mut self,
        state: &mut crate::widget::AppModel,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let before = self.visible();
        self.poll_source();
        let mut changed = self.main.update(state, path);
        if self.dismiss_requested.take() {
            self.dismiss_now();
        }
        changed |= before != self.visible();
        changed
    }

    fn sync(&mut self, state: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.poll_source();
        if self.dismiss_requested.take() {
            self.dismiss_now();
        }
        self.main.sync(state, path);
        if let Some(dismiss) = &mut self.dismiss {
            dismiss.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut nodes = vec![self.main.build_layout(taffy, theme, node_map)];
        if let Some(dismiss) = &mut self.dismiss {
            nodes.push(dismiss.build_layout(taffy, theme, node_map));
        }
        let node = taffy.parent_node(self.style(theme), &nodes);
        self.nodes = nodes;
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let display = if self.visible() {
            taffy::Display::Flex
        } else {
            taffy::Display::None
        };
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        if current.display == display {
            return false;
        }
        let mut style = current.clone();
        style.display = display;
        taffy.set_style(node, style).is_ok()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = if self.visible() {
            layout_rect(taffy, node, origin)
        } else {
            Rect::ZERO
        };
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
        self.layout_dirty = false;
        let origin = self.rect.origin();
        if let Some(&main) = self.nodes.first() {
            self.main.apply_layout(taffy, main, origin);
        }
        if let (Some(dismiss), Some(&node)) = (&mut self.dismiss, self.nodes.get(1)) {
            dismiss.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.visible() || self.rect.width() <= 0.0 {
            return;
        }
        paint_well(&mut PaintCtx::new(scene, transform, text, theme), self.rect);
        let on_well = theme.on_ground(well(theme));
        self.main.draw(scene, transform, text, &on_well, presence);
        if let Some(dismiss) = &mut self.dismiss {
            dismiss.draw(scene, transform, text, &on_well, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if !self.visible() {
            return;
        }
        if let Some(&main) = self.nodes.first() {
            self.main
                .handle_event(event, &mut ctx.reborrow(ctx.path, main));
            if *ctx.handled {
                return;
            }
        }
        if let (Some(dismiss), Some(&node)) = (&mut self.dismiss, self.nodes.get(1)) {
            dismiss.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.main];
        if let Some(dismiss) = &self.dismiss {
            out.push(dismiss);
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.main];
        if let Some(dismiss) = &mut self.dismiss {
            out.push(dismiss);
        }
        out
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let started = crate::widget::start_in_children(self, op, doc, path)?;
        if self.dismiss_requested.take() {
            self.dismiss_now();
        }
        Ok(started)
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.main.take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.main.body.is_layout_dirty()
            || self
                .main
                .actions
                .as_ref()
                .is_some_and(Widget::is_layout_dirty)
            || self.dismiss.as_ref().is_some_and(Widget::is_layout_dirty)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.main.body.needs_redraw()
            || self.main.actions.as_ref().is_some_and(Widget::needs_redraw)
            || self.dismiss.as_ref().is_some_and(Widget::needs_redraw)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.main.body.mark_drawn();
        if let Some(actions) = &mut self.main.actions {
            actions.mark_drawn();
        }
        if let Some(dismiss) = &mut self.dismiss {
            dismiss.mark_drawn();
        }
    }

    fn name(&self) -> &'static str {
        "Banner"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        if !self.visible() {
            return None;
        }
        // A banner is a live status region: the reader announces it when it
        // appears. Errors and warnings are assertive; the rest polite.
        let notice = &self.main.body.notice;
        let mut node = AccessNode::new(Role::Group)
            .with_name(notice.title.clone())
            .with_live(notice.kind.politeness());
        if let Some(id) = &self.id {
            node = node.with_semantic_key(id.clone());
        }
        if let Some(message) = &notice.message {
            node = node.with_description(message.clone());
        }
        Some(node)
    }
}

impl crate::undo::UndoPoints for Banner {
    fn needs_commit(&self) -> bool {
        self.main
            .actions
            .as_ref()
            .is_some_and(|actions| crate::undo::needs_commit(actions))
    }
    fn on_commit(&mut self) {
        if let Some(actions) = &mut self.main.actions {
            crate::undo::on_commit(actions);
        }
    }
}

impl Banner {
    fn style(&self, theme: &Theme) -> Style {
        let m = Metrics::of(theme);
        let pad_x = crate::num::f32_of_f64(m.pad_x);
        let pad_y = crate::num::f32_of_f64(m.pad_y);
        Style {
            display: if self.visible() {
                taffy::Display::Flex
            } else {
                taffy::Display::None
            },
            align_items: Some(taffy::AlignItems::Center),
            flex_shrink: 0.0,
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: length(crate::num::f32_of_f64(m.min_height)),
            },
            padding: taffy::Rect {
                left: length(pad_x),
                right: length(pad_x),
                top: length(pad_y),
                bottom: length(pad_y),
            },
            gap: Size {
                width: length(crate::num::f32_of_f64(m.gap)),
                height: length(0.0_f32),
            },
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests;
