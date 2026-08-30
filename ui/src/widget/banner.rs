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

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

const PAD_X: f64 = 14.0;
const PAD_Y: f64 = 12.0;
const ICON_D: f64 = 18.0;
const ACCENT_BAR_W: f64 = 4.0;
const DISMISS_D: f64 = 18.0;
const GAP: f64 = 10.0;

/// Shared visual paint for a banner filling `rect`. Used by both the retained
/// [`Banner`] widget and the immediate-mode [`Banner::paint`] entry point.
/// Returns the dismiss "×" rect when `dismissible`, else `Rect::ZERO`.
#[allow(clippy::too_many_arguments)]
fn paint_banner(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    kind: BannerKind,
    icon: Icon,
    title: &str,
    message: Option<&str>,
    dismissible: bool,
    hover: Point,
) -> Rect {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Rect::ZERO;
    }
    let accent = kind.color(theme);
    let radius = f64::from(theme.tokens.radii.md);

    let surf = RoundedRect::from_rect(rect, radius);
    scene.fill(
        Fill::NonZero,
        transform,
        accent.with_alpha_factor(0.13),
        None,
        &surf,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        accent.with_alpha_factor(0.4),
        None,
        &surf,
    );

    let bar = RoundedRect::new(
        rect.x0,
        rect.y0,
        rect.x0 + ACCENT_BAR_W,
        rect.y1,
        (radius, 0.0, 0.0, radius),
    );
    scene.fill(Fill::NonZero, transform, accent, None, &bar);

    let icon_x = rect.x0 + PAD_X;
    let icon_cy = rect.center().y;
    let icon_rect =
        Rect::from_center_size(Point::new(icon_x + ICON_D / 2.0, icon_cy), (ICON_D, ICON_D));
    icon.draw_with_stroke(scene, transform, icon_rect, accent, 1.6);

    let text_x = icon_rect.x1 + GAP;
    let title_size = theme.tokens.typography.scale.base;
    let msg_size = theme.tokens.typography.scale.sm;
    let total_text_h = if message.is_some() {
        f64::from(title_size) + 4.0 + f64::from(msg_size)
    } else {
        f64::from(title_size)
    };
    let mut ty = rect.center().y - total_text_h / 2.0;
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine composition is float matrix multiplication; it cannot overflow or \
                  panic"
    )]
    let title_transform = transform * Affine::translate((text_x, ty));
    text_renderer.draw_text(
        scene,
        title,
        title_size,
        theme.text_primary,
        title_transform,
        None,
    );
    ty += f64::from(title_size) + 4.0;
    if let Some(msg) = message {
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition is float matrix multiplication; it cannot overflow \
                      or panic"
        )]
        let msg_transform = transform * Affine::translate((text_x, ty));
        text_renderer.draw_text(scene, msg, msg_size, theme.text_dim, msg_transform, None);
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
        Icon::X.draw_with_stroke(scene, transform, dr, col, 1.6);
        dr
    } else {
        Rect::ZERO
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
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        kind: BannerKind,
        icon: Icon,
        title: &str,
        message: Option<&str>,
        dismissible: bool,
        hover: Point,
    ) -> Rect {
        paint_banner(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            kind,
            icon,
            title,
            message,
            dismissible,
            hover,
        )
    }

    /// Intrinsic content height for the current title/message, in logical px.
    fn content_height(&self, theme: &Theme) -> f64 {
        let title_h = f64::from(theme.tokens.typography.scale.base);
        let mut h = title_h;
        if self.message.is_some() {
            h += 4.0 + f64::from(theme.tokens.typography.scale.sm);
        }
        (h + PAD_Y * 2.0).max(44.0)
    }
}

impl Widget for Banner {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
        let h = if self.dismissed {
            0.0
        } else {
            self.content_height(theme) as f32
        };
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "content_height is a small, bounded pixel value built from theme typography \
                  constants; f32 precision loss is immaterial for widget layout"
    )]
    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let theme = Theme::dark();
        let h = if self.dismissed {
            0.0
        } else {
            self.content_height(&theme) as f32
        };
        taffy::Size {
            width: 320.0,
            height: h,
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
        if self.dismissed {
            self.dismiss_rect = Rect::ZERO;
            return;
        }
        self.dismiss_rect = paint_banner(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            self.kind,
            self.icon(),
            &self.title,
            self.message.as_deref(),
            self.dismissible,
            self.last_cursor,
        );
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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                *handled = true;
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
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        handled
    }

    #[test]
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

    #[test]
    fn icon_override_wins() {
        let b = Banner::info("Heads up").with_icon(Icon::Bell);
        assert_eq!(b.icon(), Icon::Bell);
        let d = Banner::info("Heads up");
        assert_eq!(d.icon(), Icon::Info, "defaults to the severity icon");
    }

    #[test]
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

    #[test]
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

    #[test]
    fn message_makes_the_banner_taller() {
        let theme = Theme::dark();
        let short = Banner::info("One line").content_height(&theme);
        let tall = Banner::info("One line")
            .with_message("Plus a detail line")
            .content_height(&theme);
        assert!(tall > short, "a message adds a second line of height");
    }

    #[test]
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

    #[test]
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
}
