//! `Toast` + `ToastStack` primitive — transient notifications anchored
//! top-right of the shell, sinking fallible OS operations
//! (trash/rename/move/copy), `ShellError` conversions, and confirmations.
//!
//! - [`ToastKind`] maps to `Theme::tokens.colors.semantic.*` for the accent
//!   bar.
//! - [`Toast`] is constructed via [`ToastStack::push`], not directly.
//! - [`ToastStack`] is a ring-buffered list; default TTL is [`DEFAULT_TTL`]
//!   (3s), default cap is [`DEFAULT_MAX`] (5 toasts).
//!
//! The `Widget` impl draws each active toast as a stacked raised-surface
//! card with a kind-coloured accent bar. It's a pure render of the current
//! state — enter/exit motion is wired by the shell consumer that owns the
//! animation driver — so tests stay deterministic.

use automerge::{AutoCommit, ObjId};
use std::time::Duration;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, ButtonFace, Widget};

/// Default time-to-live for a freshly pushed toast.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3);

/// Default maximum number of simultaneous toasts. When the cap is
/// exceeded by [`ToastStack::push`] the oldest entry is dropped.
pub const DEFAULT_MAX: usize = 5;

/// Severity / category of a toast. Maps to the semantic color ramp in
/// [`crate::tokens::Colors`] for the left accent bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

/// A single toast entry held by a [`ToastStack`]. Entries are produced
/// internally by [`ToastStack::push`]; shell code does not construct
/// `Toast` values directly.
#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub ttl_remaining: Duration,
}

/// Top-right notification stack with a configurable maximum entry count
/// and TTL-based expiry.
///
/// The stack is render-only — input routing (click-to-dismiss, hover
/// pause) is wired by the first shell consumer. The primitive itself
/// exposes timing/capacity API so it stays unit-testable without a live
/// event loop or a real clock.
pub struct ToastStack {
    toasts: Vec<Toast>,
    max: usize,
    default_ttl: Duration,
    rect: Rect,
    /// Whether the pointer is currently over the stack — [`tick`] pauses
    /// expiry while hovered so the user can read/click a toast.
    hovered: bool,
    last_cursor: Point,
}

impl std::fmt::Debug for ToastStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastStack")
            .field("toasts", &self.toasts)
            .field("max", &self.max)
            .field("default_ttl", &self.default_ttl)
            .finish()
    }
}

impl Default for ToastStack {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastStack {
    /// Build an empty toast stack with the [`DEFAULT_MAX`] cap and
    /// [`DEFAULT_TTL`] per-toast lifetime.
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            max: DEFAULT_MAX,
            default_ttl: DEFAULT_TTL,
            rect: Rect::ZERO,
            hovered: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Override the maximum number of simultaneous toasts.
    pub fn with_max(mut self, max: usize) -> Self {
        self.max = max.max(1);
        self
    }

    /// Override the TTL applied to subsequently pushed toasts.
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// Push a new toast. If the stack is at capacity the oldest entry
    /// is dropped to make room (FIFO drop policy).
    pub fn push(&mut self, text: impl Into<String>, kind: ToastKind) {
        let toast = Toast {
            text: text.into(),
            kind,
            ttl_remaining: self.default_ttl,
        };
        self.toasts.push(toast);
        // Drop oldest entries until we are back within the cap.
        while self.toasts.len() > self.max {
            self.toasts.remove(0);
        }
    }

    /// Advance time. Each toast's `ttl_remaining` is saturatingly
    /// decremented by `dt`; entries that reach zero are removed. Expiry is
    /// **paused while the pointer hovers the stack** so a toast can be read
    /// or clicked without vanishing.
    pub fn tick(&mut self, dt: Duration) {
        if self.hovered {
            return;
        }
        for t in &mut self.toasts {
            t.ttl_remaining = t.ttl_remaining.saturating_sub(dt);
        }
        self.toasts.retain(|t| t.ttl_remaining > Duration::ZERO);
    }

    /// Whether the pointer is currently over the stack.
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    /// The card index under a logical point, if any (top-to-bottom layout).
    fn card_at(&self, p: Point) -> Option<usize> {
        if p.x < self.rect.x0 || p.x > self.rect.x0 + TOAST_WIDTH {
            return None;
        }
        let rel = p.y - self.rect.y0;
        if rel < 0.0 {
            return None;
        }
        let stride = TOAST_HEIGHT + TOAST_GAP;
        let idx = (rel / stride) as usize;
        // Reject the inter-card gap.
        if rel - idx as f64 * stride > TOAST_HEIGHT {
            return None;
        }
        if idx < self.toasts.len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Dismiss the toast at `idx` (no-op if out of range).
    pub fn dismiss(&mut self, idx: usize) {
        if idx < self.toasts.len() {
            self.toasts.remove(idx);
        }
    }

    /// Number of active (non-expired) toasts.
    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// `true` when no toasts are currently visible.
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Iterate the active toasts in push order (oldest first).
    pub fn iter(&self) -> std::slice::Iter<'_, Toast> {
        self.toasts.iter()
    }

    /// Maximum simultaneous toasts.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Current default TTL applied to subsequent [`Self::push`] calls.
    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }
}

const TOAST_WIDTH: f64 = 320.0;
const TOAST_HEIGHT: f64 = 48.0;
const TOAST_GAP: f64 = 8.0;

fn kind_color(kind: ToastKind, theme: &Theme) -> vello::peniko::Color {
    match kind {
        ToastKind::Info => theme.tokens.colors.semantic.info,
        ToastKind::Success => theme.tokens.colors.semantic.success,
        ToastKind::Warning => theme.tokens.colors.semantic.warning,
        ToastKind::Error => theme.tokens.colors.semantic.danger,
    }
}

// ---------------------------------------------------------------------------
// Shared notification-card renderer
// ---------------------------------------------------------------------------
//
// One immediate-mode painter for a toast/notification card, shared by the
// retained [`ToastStack`] (in-app toasts) and the shell's OS-notification
// stack (`shell::shell::notification::NotificationStack`) — HIG §8: extend
// the widget, don't fork it.

const CARD_ACCENT_BAR_W: f64 = 4.0;
const CARD_PAD_X: f64 = 14.0;
const CARD_PAD_Y: f64 = 8.0;
const CARD_CLOSE_SIZE: f64 = 22.0;

/// Immediate-mode content for one notification / toast card, painted by
/// [`paint_card`]. Built fluently:
/// `ToastCard::new("Slow network", ToastKind::Warning).body("retrying")`.
#[derive(Clone, Copy)]
pub struct ToastCard<'a> {
    /// Primary line — the summary. Semibold, `text_primary`.
    pub title: &'a str,
    /// Optional secondary line (dim). Empty = single-line card.
    pub body: &'a str,
    /// Optional attribution caption above the title (e.g. the sending app's
    /// name), rendered dim + uppercase + semibold. Empty = no caption.
    pub caption: &'a str,
    /// Severity — drives the left accent stripe colour.
    pub kind: ToastKind,
    /// Enter/exit animation opacity in `[0, 1]`; `1.0` for a settled card.
    pub alpha: f32,
    /// Draw a dismiss (`X`) button at the top-right and hand its rect back
    /// via [`ToastCardHit`] so the caller keeps one hit-test on widget-owned
    /// geometry.
    pub closable: bool,
    /// Hover state for the dismiss button, when the caller tracks the cursor.
    pub close_hovered: bool,
    /// Draw an "actionable" chevron at the trailing edge (the card carries
    /// activatable actions).
    pub actionable: bool,
}

impl<'a> ToastCard<'a> {
    pub fn new(title: &'a str, kind: ToastKind) -> Self {
        Self {
            title,
            body: "",
            caption: "",
            kind,
            alpha: 1.0,
            closable: false,
            close_hovered: false,
            actionable: false,
        }
    }
    pub fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = caption;
        self
    }
    pub fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    pub fn close_hovered(mut self, v: bool) -> Self {
        self.close_hovered = v;
        self
    }
    pub fn actionable(mut self, v: bool) -> Self {
        self.actionable = v;
        self
    }
}

/// Geometry [`paint_card`] hands back so callers hit-test the card's
/// interactive regions against widget-owned rects (no bespoke maths).
#[derive(Clone, Copy, Debug, Default)]
pub struct ToastCardHit {
    /// The dismiss (`X`) button rect. `Rect::ZERO` when `closable` was unset.
    pub close_rect: Rect,
}

/// Paint a toast / notification card into `rect` and return the geometry a
/// caller needs to route clicks. The card is width/height-agnostic (it lays
/// out relative to `rect`), so a 48px single-line in-app toast and a 64px
/// three-line OS notification share this one renderer.
///
/// Layout: soft drop shadow → raised surface + border → left severity accent
/// stripe → (optional) attribution caption / title / body text column →
/// (optional) ghost dismiss button → (optional) actionable chevron. Every
/// colour is modulated by `card.alpha` so the caller drives the enter/exit
/// fade without a second painter.
pub fn paint_card(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
) -> ToastCardHit {
    use vello::peniko::{BlendMode, Fill};
    let mut hit = ToastCardHit::default();
    let a = card.alpha.clamp(0.0, 1.0);
    if rect.width() <= 0.0 || rect.height() <= 0.0 || a <= 0.0 {
        return hit;
    }
    let radius = f64::from(theme.tokens.radii.md);
    let shape = RoundedRect::from_rect(rect, radius);

    // Drop shadow from the elevation token (not ad-hoc literals), following
    // the card's own radius and fading with the card.
    use crate::color_compat::ColorCompat;
    let mut elev = theme.tokens.elevation.e3;
    elev.color = elev.color.with_alpha_factor(a);
    crate::surface::shadow(scene, transform, rect, radius, &elev);

    // Raised surface + hairline border.
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised.with_alpha(a),
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard.with_alpha(a),
        None,
        &shape,
    );

    // Left severity accent stripe — the at-a-glance state form (HIG §7).
    let bar = Rect::new(
        rect.x0 + 1.5,
        rect.y0 + 8.0,
        rect.x0 + 1.5 + CARD_ACCENT_BAR_W,
        rect.y1 - 8.0,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        kind_color(card.kind, theme).with_alpha(a),
        None,
        &RoundedRect::from_rect(bar, f64::from(theme.tokens.radii.sm)),
    );

    // Dismiss (X) button — a ghost `Button` face, top-right. Wrapped in an
    // alpha layer during the enter/exit fade because `Button::paint` has no
    // opacity parameter of its own.
    let mut text_right = rect.x1 - CARD_PAD_X;
    if card.closable {
        let cx1 = rect.x1 - 6.0;
        let cy0 = rect.y0 + 6.0;
        let close_rect = Rect::new(cx1 - CARD_CLOSE_SIZE, cy0, cx1, cy0 + CARD_CLOSE_SIZE);
        let faded = a < 0.999;
        if faded {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                a,
                transform,
                &RoundedRect::from_rect(close_rect, 6.0),
            );
        }
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(close_rect, "")
                .ghost()
                .icon(crate::icons::Icon::X)
                .radius(6.0)
                .hovered(card.close_hovered)
                .accessible_name("Dismiss"),
        );
        if faded {
            scene.pop_layer();
        }
        hit.close_rect = close_rect;
        text_right = close_rect.x0 - 6.0;
    }

    // Text column: optional attribution caption, then the title, then the
    // optional body. The block is vertically centred so a single-line card
    // and a three-line card both look balanced.
    let text_x = rect.x0 + CARD_PAD_X;
    let width_avail = (text_right - text_x).max(0.0) as f32;
    let caption_size = theme.tokens.typography.scale.xs;
    let title_size = theme.tokens.typography.scale.sm;
    let body_size = theme.tokens.typography.scale.xs;

    let has_caption = !card.caption.is_empty();
    let has_body = !card.body.is_empty();
    let cap_h = if has_caption {
        f64::from(caption_size) + 4.0
    } else {
        0.0
    };
    let title_h = f64::from(title_size) + if has_body { 4.0 } else { 0.0 };
    let body_h = if has_body { f64::from(body_size) } else { 0.0 };
    let block_h = cap_h + title_h + body_h;
    let mut ty = rect.y0 + ((rect.height() - block_h) / 2.0).max(CARD_PAD_Y);

    let dim = theme.tokens.colors.text_dim.with_alpha(a);
    let primary = theme.tokens.colors.text_primary.with_alpha(a);

    if has_caption {
        text_renderer.draw_text_weighted(
            scene,
            &card.caption.to_uppercase(),
            caption_size,
            dim,
            transform * Affine::translate((text_x, ty)),
            Some(width_avail),
            TextWeight::Semibold,
        );
        ty += cap_h;
    }
    text_renderer.draw_text_weighted(
        scene,
        card.title,
        title_size,
        primary,
        transform * Affine::translate((text_x, ty)),
        Some(width_avail),
        TextWeight::Semibold,
    );
    ty += title_h;
    if has_body {
        text_renderer.draw_text(
            scene,
            card.body,
            body_size,
            dim,
            transform * Affine::translate((text_x, ty)),
            Some(width_avail),
        );
    }

    // Actionable chevron at the trailing-bottom edge, keyed to severity.
    if card.actionable {
        let sz = 14.0;
        let ix = rect.x1 - CARD_PAD_X - sz;
        let iy = rect.y1 - CARD_PAD_Y - sz;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::new(ix, iy, ix + sz, iy + sz),
            crate::icons::Icon::ChevronRight,
            kind_color(card.kind, theme).with_alpha(a),
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    hit
}

impl Widget for ToastStack {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = self.toasts.len();
        let (w, h) = if n == 0 {
            (0.0f32, 0.0f32)
        } else {
            let height = (TOAST_HEIGHT * n as f64) + (TOAST_GAP * (n.saturating_sub(1)) as f64);
            (TOAST_WIDTH as f32, height as f32)
        };
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
        if self.toasts.is_empty() {
            return;
        }

        let mut y = self.rect.y0;
        for toast in &self.toasts {
            let card_rect = Rect::new(
                self.rect.x0,
                y,
                self.rect.x0 + TOAST_WIDTH,
                y + TOAST_HEIGHT,
            );
            // Route through the shared card renderer so an in-app toast and a
            // desktop notification are pixel-identical (HIG §8).
            paint_card(
                scene,
                transform,
                text_renderer,
                theme,
                card_rect,
                &ToastCard::new(&toast.text, toast.kind),
            );
            y += TOAST_HEIGHT + TOAST_GAP;
        }
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
                // Hover-pause: expiry halts while the pointer is over the stack.
                let now_hover = !self.toasts.is_empty() && self.card_at(self.last_cursor).is_some();
                if now_hover != self.hovered {
                    self.hovered = now_hover;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.card_at(self.last_cursor) {
                    self.dismiss(idx);
                    self.hovered = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ToastStack"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        // Assertive live region so a freshly-shown toast announces itself
        // immediately without stealing keyboard focus; the stable `name`
        // is the identity the host diffs across frames.
        let latest = self.toasts.last().map(|t| t.text.clone());
        let mut node = AccessNode::new(Role::Group)
            .with_name("Notifications")
            .with_live(Politeness::Assertive);
        if let Some(text) = latest {
            node = node.with_value(text);
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use winit::event::{DeviceId, ElementState, MouseButton};

    fn lay_out(stack: &mut ToastStack) {
        let mut tree = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = stack.build_layout(&mut tree, &theme, &mut nm);
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(TOAST_WIDTH as f32),
                height: taffy::AvailableSpace::Definite(1000.0),
            },
        )
        .unwrap();
        stack.apply_layout(&tree, node, Point::ZERO);
    }

    fn device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn feed(s: &mut ToastStack, ev: WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            &ev,
            1.0,
            &Default::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
    }

    #[test]
    fn paint_card_returns_a_top_right_close_rect_when_closable() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(100.0, 40.0, 460.0, 104.0);
        let hit = paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("New mail", ToastKind::Info)
                .body("secret subject")
                .caption("Mailer")
                .closable(true)
                .actionable(true),
        );
        assert!(
            hit.close_rect.area() > 0.0,
            "closable card returns a close rect"
        );
        assert!(
            hit.close_rect.x1 <= rect.x1 && hit.close_rect.x1 > rect.center().x,
            "close rect hugs the card's right edge",
        );
        assert!(
            hit.close_rect.y0 >= rect.y0 && hit.close_rect.y1 < rect.center().y,
            "close rect sits at the top of the card",
        );
    }

    #[test]
    fn paint_card_emits_surface_fill_and_title_glyphs() {
        use vello_encoding::DrawTag;
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
        paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("Build finished", ToastKind::Success),
        );
        let enc = scene.encoding();
        let color_fills = enc
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            color_fills >= 2,
            "surface + accent stripe fills; got {color_fills}"
        );
        assert!(
            !enc.resources.glyph_runs.is_empty(),
            "the title must push a glyph run into the scene",
        );
    }

    #[test]
    fn paint_card_is_a_noop_when_fully_faded() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
        let hit = paint_card(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            &ToastCard::new("gone", ToastKind::Info)
                .alpha(0.0)
                .closable(true),
        );
        assert_eq!(
            hit.close_rect,
            Rect::ZERO,
            "faded card exposes no close rect"
        );
        assert!(
            scene.encoding().draw_tags.is_empty(),
            "a fully-faded card emits no draw ops",
        );
    }

    #[test]
    fn click_dismisses_the_toast_under_the_cursor() {
        let mut s = ToastStack::new();
        s.push("first", ToastKind::Info);
        s.push("second", ToastKind::Error);
        lay_out(&mut s);
        // Card 1 ("second") spans y ∈ [56, 104); centre ≈ (160, 80).
        feed(
            &mut s,
            WindowEvent::CursorMoved {
                device_id: device(),
                position: (160.0, 80.0).into(),
            },
        );
        feed(
            &mut s,
            WindowEvent::MouseInput {
                device_id: device(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s.iter().next().unwrap().text, "first");
    }

    #[test]
    fn accessibility_reports_assertive_live_region_with_latest_toast() {
        use crate::a11y::{Politeness, Role};
        // Empty stack: a named, assertive live region with no value yet.
        let s = ToastStack::new();
        let node = Widget::accessibility(&s).expect("toast stack is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Assertive));
        assert_eq!(node.value, None, "an empty stack has no current toast");

        // With a live toast the stack surfaces the most recent text as value,
        // so the host's live-region scan announces it.
        let mut s = ToastStack::new();
        s.push("hello", ToastKind::Info);
        s.push("world", ToastKind::Error);
        let node = Widget::accessibility(&s).unwrap();
        assert_eq!(
            node.value.as_deref(),
            Some("world"),
            "value is the newest toast"
        );
        assert_eq!(node.live, Some(Politeness::Assertive));
    }

    #[test]
    fn hover_pauses_expiry() {
        let mut s = ToastStack::new().with_default_ttl(Duration::from_millis(100));
        s.push("stay", ToastKind::Info);
        lay_out(&mut s);
        // Hover over the only card.
        feed(
            &mut s,
            WindowEvent::CursorMoved {
                device_id: device(),
                position: (160.0, 24.0).into(),
            },
        );
        assert!(s.is_hovered());
        // tick longer than the TTL — but hovered, so it must NOT expire.
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 1, "hovered toast does not expire");
        // Move away → expiry resumes.
        feed(
            &mut s,
            WindowEvent::CursorMoved {
                device_id: device(),
                position: (900.0, 900.0).into(),
            },
        );
        assert!(!s.is_hovered());
        s.tick(Duration::from_millis(500));
        assert_eq!(s.len(), 0, "expires once un-hovered");
    }
}
