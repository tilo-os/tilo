//! `Banner` — an inline, semantic alert strip. Unlike a [`super::toast::Toast`]
//! (transient, top-right, auto-expiring) a banner is a *persistent* in-flow
//! message the user dismisses or resolves: "You're offline — changes will sync
//! when you reconnect", "Saved", "This file has unsaved changes".
//!
//! Four severities ([`BannerKind`]) map onto the semantic colour ramp so the
//! tint, border, and default leading icon all stay OS-controlled. A banner
//! carries a **title** plus an optional secondary **message**, and can be made
//! dismissible — the little "×" surfaces its click through
//! [`Banner::take_dismissed`], the toolkit's drain channel, so a CRDT-native
//! host can react without a callback.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

const PAD_X: f64 = 14.0;
const PAD_Y: f64 = 12.0;
const ICON_D: f64 = 18.0;
const ACCENT_BAR_W: f64 = 4.0;
const DISMISS_D: f64 = 18.0;
const GAP: f64 = 10.0;

/// What an immediate-mode [`Banner::paint`] shows: severity, leading icon,
/// title, optional secondary message and whether the "×" is offered.
#[derive(Clone, Copy, Debug)]
pub struct BannerContent<'a> {
    pub kind: BannerKind,
    pub icon: Icon,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub dismissible: bool,
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
}

/// Shared visual paint for a banner filling `rect`. Used by both the retained
/// [`Banner`] widget and the immediate-mode [`Banner::paint`] entry point.
/// Returns the dismiss "×" rect when `dismissible`, else `Rect::ZERO`.
fn paint_banner(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &BannerContent<'_>,
    hover: Point,
) -> Rect {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Rect::ZERO;
    }
    let BannerContent {
        kind,
        icon,
        title,
        message,
        dismissible,
    } = *content;
    let theme = ctx.theme;
    let transform = ctx.transform;
    let accent = kind.color(theme);
    let radius = f64::from(theme.tokens.radii.control);

    ctx.fill_round(rect, radius, accent.with_alpha_factor(0.13));
    ctx.stroke_round(rect, radius, accent.with_alpha_factor(0.4), 1.0);

    let bar = RoundedRect::new(
        rect.x0,
        rect.y0,
        rect.x0 + ACCENT_BAR_W,
        rect.y1,
        (radius, 0.0, 0.0, radius),
    );
    ctx.scene.fill(Fill::NonZero, transform, accent, None, &bar);

    let icon_x = rect.x0 + PAD_X;
    let icon_cy = rect.center().y;
    let icon_rect =
        Rect::from_center_size(Point::new(icon_x + ICON_D / 2.0, icon_cy), (ICON_D, ICON_D));
    icon.draw_with_stroke(ctx.scene, transform, icon_rect, accent, 1.6);

    let text_x = icon_rect.x1 + GAP;
    let title_style = theme.text(TextRole::Body).strong();
    let message_style = theme.text(TextRole::Caption);
    let text_width = banner_text_width(rect, dismissible);
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
        theme.text_primary,
        Point::new(text_x, ty),
        Some(text_width),
    );
    ty += f64::from(title_h) + gap;
    if let Some(msg) = message {
        ctx.text(
            msg,
            &message_style,
            theme.text_dim,
            Point::new(text_x, ty),
            Some(text_width),
        );
    }

    if dismissible {
        let dx = rect.x1 - PAD_X - DISMISS_D;
        let dr = Rect::from_center_size(
            Point::new(dx + DISMISS_D / 2.0, rect.center().y),
            (DISMISS_D, DISMISS_D),
        );
        let hovered = dr.contains(hover);
        let col = if hovered {
            theme.text_primary
        } else {
            theme.text_dim
        };
        Icon::X.draw_with_stroke(ctx.scene, transform, dr, col, 1.6);
        dr
    } else {
        Rect::ZERO
    }
}

fn banner_text_width(rect: Rect, dismissible: bool) -> f32 {
    let trailing = if dismissible {
        PAD_X + DISMISS_D + GAP
    } else {
        PAD_X
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "banner geometry is an ordinary logical-pixel value"
    )]
    {
        (rect.width() - PAD_X - ICON_D - GAP - trailing).max(1.0) as f32
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
            BannerKind::Info | BannerKind::Warning => Icon::Info,
            BannerKind::Success => Icon::Check,
            BannerKind::Error => Icon::Ban,
        }
    }
}

/// An inline semantic alert with a title, optional message, and optional
/// dismiss affordance.
pub struct Banner {
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
    rect: Rect,
    dismiss_rect: Rect,
    last_cursor: Point,
    layout_theme: Theme,
}

impl Banner {
    pub fn new(kind: BannerKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            message: None,
            icon: None,
            dismissible: false,
            dismissed: false,
            dismiss_fired: false,
            rect: Rect::ZERO,
            dismiss_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            layout_theme: Theme::dark(),
        }
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
    ) -> Rect {
        paint_banner(ctx, rect, content, hover)
    }

    /// Intrinsic content height for the current title/message, in logical px.
    fn content_height(&self, theme: &Theme, text_renderer: &mut TextRenderer, width: f32) -> f64 {
        let rect = Rect::new(0.0, 0.0, f64::from(width.max(1.0)), 1.0);
        let text_width = banner_text_width(rect, self.dismissible);
        let title_style = theme.text(TextRole::Body).strong();
        let (_, title_h) = text_renderer.measure_style(&self.title, Some(text_width), &title_style);
        let message_h = self.message.as_ref().map_or(0.0, |message| {
            let style = theme.text(TextRole::Caption);
            text_renderer
                .measure_style(message, Some(text_width), &style)
                .1
        });
        let gap = if self.message.is_some() {
            f64::from(theme.tokens.spacing.s1)
        } else {
            0.0
        };
        (f64::from(title_h + message_h) + gap + PAD_Y * 2.0).max(44.0)
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
        let node = taffy.leaf_node(Style {
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.dismissed {
            self.dismiss_rect = Rect::ZERO;
            return;
        }
        let content = BannerContent::new(self.kind, &self.title)
            .with_icon(self.icon())
            .with_message(self.message.as_deref())
            .dismissible(self.dismissible);
        self.dismiss_rect = paint_banner(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &content,
            self.last_cursor,
        );
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
            let text_width =
                banner_text_width(Rect::new(0.0, 0.0, f64::from(width), height), false);
            let (_, message_height) =
                text.measure_style(message, Some(text_width), &theme.text(TextRole::Caption));

            assert!(
                message_height > theme.tokens.typography.size(TextRole::Caption) * 1.25,
                "the recovery copy must wrap instead of escaping the banner"
            );
            assert!(
                height >= f64::from(message_height) + PAD_Y * 2.0,
                "the banner must reserve the same wrapped height that it paints"
            );
        }

        fn draws_accent_bar_and_text() {
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
        draws_accent_bar_and_text();
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
