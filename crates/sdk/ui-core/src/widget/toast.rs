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

use crate::time::Duration;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, ButtonFace, Widget};

/// Default time-to-live for a freshly pushed toast.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3);

/// Default maximum number of simultaneous toasts. When the cap is
/// exceeded by [`ToastStack::push`] the oldest entry is dropped.
pub const DEFAULT_MAX: usize = 5;

/// Widens a small on-screen count (toast count, card index) to `f64` for
/// layout math.
#[allow(
    clippy::cast_precision_loss,
    reason = "a toast count/index is always tiny, far below f64's exact-integer range"
)]
fn idx_to_f64(n: usize) -> f64 {
    n as f64
}

/// Narrows a screen-space `f64` (rect extent or coordinate) to the `f32`
/// vello/text-layout math expects.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout coordinates never approach f32::MAX; this narrowing is exact for every practical UI extent"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Toast cards have a fixed height and therefore paint each text field on a
/// single baseline. Collapse all Unicode whitespace before measuring or
/// eliding a field so an app-provided/localized newline cannot turn a card
/// into an overflowing multi-line block. This also makes a body containing
/// only whitespace behave like an omitted body.
fn single_line_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Floors an already-non-negative screen-space `f64` to a small on-screen
/// index (a card position within the stack).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers check `v >= 0.0` first, and a card index never approaches usize::MAX"
)]
fn f64_to_idx(v: f64) -> usize {
    v as usize
}

/// Severity / category of a toast. Maps to the semantic color ramp in
/// [`crate::tokens::Colors`] for the left accent bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

/// Horizontal placement for a transient card painted inside an app's area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastAnchor {
    /// Centre the card horizontally in the available area.
    BottomCenter,
    /// Align the card with the area's trailing edge.
    BottomRight,
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
/// event loop or a wall clock.
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
            .field("rect", &self.rect)
            .field("hovered", &self.hovered)
            .field("last_cursor", &self.last_cursor)
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
    #[must_use]
    pub fn with_max(mut self, max: usize) -> Self {
        self.max = max.max(1);
        self
    }

    /// Override the TTL applied to subsequently pushed toasts.
    #[must_use]
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

    /// Place this overlay inside `area` without making it a layout child.
    /// Useful for app-level notification stacks that must float above their
    /// content rather than reserve document space.
    pub fn place_bottom_right(&mut self, area: Rect, margin: f64) {
        let n = self.toasts.len();
        let height = (TOAST_HEIGHT * idx_to_f64(n)) + (TOAST_GAP * idx_to_f64(n.saturating_sub(1)));
        let x1 = area.x1 - margin;
        let y1 = area.y1 - margin;
        self.rect = if n == 0 {
            Rect::ZERO
        } else {
            Rect::new(x1 - TOAST_WIDTH, y1 - height, x1, y1)
        };
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
        let idx = f64_to_idx(rel / stride);
        // Reject the inter-card gap.
        if rel - idx_to_f64(idx) * stride > TOAST_HEIGHT {
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

impl<'a> IntoIterator for &'a ToastStack {
    type Item = &'a Toast;
    type IntoIter = std::slice::Iter<'a, Toast>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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
// stack (`tilo_desktop::notifications::NotificationStack`) — HIG §8: extend
// the widget, don't fork it.

const CARD_ACCENT_BAR_W: f64 = 4.0;
const CARD_PAD_X: f64 = 14.0;
const CARD_PAD_Y: f64 = 8.0;
const CARD_CLOSE_SIZE: f64 = 22.0;

/// Immediate-mode content for one notification / toast card, painted by
/// [`paint_card`]. Built fluently:
/// `ToastCard::new("Slow network", ToastKind::Warning).body("retrying")`.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent immediate-mode card affordances are supplied by the caller"
)]
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
    /// Optional left/secondary and right/primary actions. These use the same
    /// immediate-mode [`Button`] painter as ordinary retained controls, so a
    /// notification can ask for a decision without inventing a second button
    /// language. Empty labels omit the action row entirely.
    pub secondary_action: &'a str,
    pub secondary_action_icon: Option<crate::icons::Icon>,
    pub secondary_action_hovered: bool,
    pub primary_action: &'a str,
    pub primary_action_icon: Option<crate::icons::Icon>,
    pub primary_action_hovered: bool,
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
            secondary_action: "",
            secondary_action_icon: None,
            secondary_action_hovered: false,
            primary_action: "",
            primary_action_icon: None,
            primary_action_hovered: false,
        }
    }
    #[must_use]
    pub fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }
    #[must_use]
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = caption;
        self
    }
    #[must_use]
    pub fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
    #[must_use]
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    #[must_use]
    pub fn close_hovered(mut self, v: bool) -> Self {
        self.close_hovered = v;
        self
    }
    #[must_use]
    pub fn actionable(mut self, v: bool) -> Self {
        self.actionable = v;
        self
    }
    /// Add a standard two-choice action row. The secondary action is painted
    /// neutral on the left; the primary action is painted with the theme's
    /// primary face on the right.
    #[must_use]
    pub fn actions(
        mut self,
        secondary: &'a str,
        secondary_icon: crate::icons::Icon,
        secondary_hovered: bool,
        primary: &'a str,
        primary_icon: crate::icons::Icon,
        primary_hovered: bool,
    ) -> Self {
        self.secondary_action = secondary;
        self.secondary_action_icon = Some(secondary_icon);
        self.secondary_action_hovered = secondary_hovered;
        self.primary_action = primary;
        self.primary_action_icon = Some(primary_icon);
        self.primary_action_hovered = primary_hovered;
        self
    }
}

/// Geometry [`paint_card`] hands back so callers hit-test the card's
/// interactive regions against widget-owned rects (no bespoke maths).
#[derive(Clone, Copy, Debug, Default)]
pub struct ToastCardHit {
    /// The painted card rect. `Rect::ZERO` when the card was not painted.
    pub card_rect: Rect,
    /// The dismiss (`X`) button rect. `Rect::ZERO` when `closable` was unset.
    pub close_rect: Rect,
    /// Left/secondary action rect. `Rect::ZERO` when the card has no actions.
    pub secondary_action_rect: Rect,
    /// Right/primary action rect. `Rect::ZERO` when the card has no actions.
    pub primary_action_rect: Rect,
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
    use crate::color_compat::ColorCompat;
    let mut hit = ToastCardHit::default();
    let a = card.alpha.clamp(0.0, 1.0);
    if rect.width() <= 0.0 || rect.height() <= 0.0 || a <= 0.0 {
        return hit;
    }
    hit.card_rect = rect;
    let radius = f64::from(theme.tokens.radii.control);
    let shape = RoundedRect::from_rect(rect, radius);

    // Drop shadow from the elevation token (not ad-hoc literals), following
    // the card's own radius and fading with the card.
    let mut elev = theme.tokens.elevation.floating;
    elev.color = elev.color.with_alpha_factor(a);
    crate::surface::shadow(scene, transform, rect, radius, &elev);

    // Panel role (HIG §9.1): panel ground + the derived panel edge as a
    // crisp 1 px inset stroke, both fading with the card. The declared
    // `border_standard` on `raised` was invisible on the dark preset.
    scene.fill(
        Fill::NonZero,
        transform,
        crate::chrome::panel_ground(theme).with_alpha(a),
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        crate::chrome::panel_border(theme).with_alpha_factor(a),
        None,
        &crate::surface::edge_outline(rect, radius),
    );

    // Left severity accent stripe — the at-a-glance state form (HIG §7).
    // Inset a token step from the 1 px edge so the two never touch, and
    // `s2` from the top and bottom like the card's own vertical padding.
    let bar_x0 = rect.x0 + 1.0 + f64::from(theme.tokens.spacing.s1);
    let bar_inset_y = f64::from(theme.tokens.spacing.s2);
    let bar = Rect::new(
        bar_x0,
        rect.y0 + bar_inset_y,
        bar_x0 + CARD_ACCENT_BAR_W,
        rect.y1 - bar_inset_y,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        kind_color(card.kind, theme).with_alpha(a),
        None,
        &RoundedRect::from_rect(bar, f64::from(theme.tokens.radii.detail)),
    );

    // Dismiss (X) button — a ghost `Button` face, top-right.
    let mut text_right = rect.x1 - CARD_PAD_X;
    if let Some(close_rect) =
        paint_dismiss_button(scene, transform, text_renderer, theme, rect, card, a)
    {
        hit.close_rect = close_rect;
        text_right = close_rect.x0 - 6.0;
    }

    // Text column: optional attribution caption, then the title, then the
    // optional body.
    let action_top = paint_card_actions(
        scene,
        transform,
        text_renderer,
        theme,
        rect,
        card,
        a,
        &mut hit,
    );
    paint_card_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        Rect::new(rect.x0, rect.y0, text_right, action_top.unwrap_or(rect.y1)),
        card,
        a,
    );

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

fn paint_card_actions(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
    hit: &mut ToastCardHit,
) -> Option<f64> {
    if card.secondary_action.is_empty() || card.primary_action.is_empty() {
        return None;
    }
    let left = rect.x0 + CARD_PAD_X;
    let right = rect.x1 - CARD_PAD_X;
    let gap = 8.0;
    let y1 = rect.y1 - CARD_PAD_Y;
    let y0 = y1 - 30.0;
    let mid = (left + right - gap) / 2.0;
    let secondary = Rect::new(left, y0, mid, y1);
    let primary = Rect::new(mid + gap, y0, right, y1);
    hit.secondary_action_rect = secondary;
    hit.primary_action_rect = primary;

    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &RoundedRect::from_rect(Rect::new(left, y0, right, y1), 8.0),
        );
    }
    let mut secondary_face = ButtonFace::new(secondary, card.secondary_action)
        .hovered(card.secondary_action_hovered)
        .text_size(12.0);
    if let Some(icon) = card.secondary_action_icon {
        secondary_face = secondary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &secondary_face);
    let mut primary_face = ButtonFace::new(primary, card.primary_action)
        .primary()
        .hovered(card.primary_action_hovered)
        .text_size(12.0);
    if let Some(icon) = card.primary_action_icon {
        primary_face = primary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &primary_face);
    if faded {
        scene.pop_layer();
    }
    Some(y0 - 4.0)
}

/// Paint a single-line transient toast near the bottom of `area`.
///
/// This is the immediate-mode counterpart to [`ToastStack`], for apps that
/// already own their notification lifetime. It standardises card sizing,
/// clipping, and edge spacing while leaving expiry and severity with the
/// caller. The returned geometry lets callers test or align nearby content
/// without repeating the sizing rules.
pub fn paint_transient(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    anchor: ToastAnchor,
    card: &ToastCard<'_>,
) -> ToastCardHit {
    const EDGE_GAP: f64 = 24.0;
    const HEIGHT: f64 = 48.0;
    const MIN_WIDTH: f64 = 160.0;
    let available_width = (area.width() - 2.0 * EDGE_GAP).max(0.0);
    if available_width == 0.0 || area.height() < HEIGHT + EDGE_GAP {
        return ToastCardHit::default();
    }

    let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
    let title_width = f64::from(
        text_renderer
            .measure_weighted(card.title, title_size, None, TextWeight::Semibold)
            .0,
    );
    let natural_width = title_width + 2.0 * CARD_PAD_X + CARD_ACCENT_BAR_W + 2.0;
    let width = natural_width
        .max(MIN_WIDTH.min(available_width))
        .min(available_width);
    let x0 = match anchor {
        ToastAnchor::BottomCenter => area.center().x - width / 2.0,
        ToastAnchor::BottomRight => area.x1 - EDGE_GAP - width,
    };
    let y1 = area.y1 - EDGE_GAP;
    paint_card(
        scene,
        transform,
        text_renderer,
        theme,
        Rect::new(x0, y1 - HEIGHT, x0 + width, y1),
        card,
    )
}

/// Paints the optional dismiss (`X`) button, top-right of `rect`, and
/// returns its rect when the card is closable. Wrapped in an alpha layer
/// during the enter/exit fade because `Button::paint` has no opacity
/// parameter of its own.
fn paint_dismiss_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> Option<Rect> {
    if !card.closable {
        return None;
    }
    let cx1 = rect.x1 - 6.0;
    let cy0 = rect.y0 + 6.0;
    let close_rect = Rect::new(cx1 - CARD_CLOSE_SIZE, cy0, cx1, cy0 + CARD_CLOSE_SIZE);
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
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
    Some(close_rect)
}

/// Paints the text column — optional attribution caption, then the title,
/// then the optional body — vertically centred in `column`, after its left
/// pad. `column` runs from the card's top-left to the dismiss button's left
/// edge (or the card's own right pad when there is none) and the top of the
/// action row (or the card's bottom).
fn paint_card_text(ctx: &mut PaintCtx<'_>, column: Rect, card: &ToastCard<'_>, alpha: f32) {
    let theme = ctx.theme;
    let text_renderer = &mut *ctx.text;
    let text_x = column.x0 + CARD_PAD_X;
    let width_avail = to_f32((column.x1 - text_x).max(0.0));
    let caption_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);
    let title_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
    let body_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.xs);

    // A desktop notification may carry arbitrary localized or application
    // text. Keep every field one-line before deriving heights and widths;
    // otherwise `draw_text` interprets an embedded newline as a second line
    // even though this card's geometry is intentionally fixed-height.
    let caption_text = single_line_text(card.caption);
    let title_text = single_line_text(card.title);
    let body_text = single_line_text(card.body);
    let has_caption = !caption_text.is_empty();
    let has_body = !body_text.is_empty();
    // Stack the measured line boxes, not the font sizes: a face's line
    // height is ~1.2–1.3× its size, so size-derived heights under-reserved
    // every line and the block sat high in the card. `spacing.s1` between
    // lines.
    let line_gap = f64::from(theme.tokens.spacing.s1);
    let line_h = |text_renderer: &mut TextRenderer, text: &str, size: f32, weight: TextWeight| {
        f64::from(text_renderer.measure_weighted(text, size, None, weight).1)
    };
    let cap_h = if has_caption {
        line_h(
            text_renderer,
            &caption_text,
            caption_size,
            TextWeight::Semibold,
        ) + line_gap
    } else {
        0.0
    };
    let title_h = line_h(text_renderer, &title_text, title_size, TextWeight::Semibold)
        + if has_body { line_gap } else { 0.0 };
    let body_h = if has_body {
        line_h(text_renderer, &body_text, body_size, TextWeight::Regular)
    } else {
        0.0
    };
    let block_h = cap_h + title_h + body_h;
    let text_height = column.height().max(0.0);
    let mut ty = column.y0 + ((text_height - block_h) / 2.0).max(CARD_PAD_Y);

    // `paint_card` deliberately owns a fixed-height notification surface.
    // Wrapping an untrusted summary/body into that surface makes the title
    // paint over the body (and can push the close affordance into the text).
    // Elide each line against the *actual* width left by the close button;
    // long diagnostics belong in the structured log, not in chrome.
    let caption = has_caption.then(|| {
        text_renderer.elide_to_width_weighted(
            &caption_text.to_uppercase(),
            caption_size,
            width_avail,
            TextWeight::Semibold,
        )
    });
    let title = text_renderer.elide_to_width_weighted(
        &title_text,
        title_size,
        width_avail,
        TextWeight::Semibold,
    );
    let body = has_body.then(|| text_renderer.elide_to_width(&body_text, body_size, width_avail));

    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);

    if has_caption {
        ctx.text(
            caption.as_deref().unwrap_or_default(),
            &TextStyle::new(caption_size).with_weight(TextWeight::Semibold),
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
        ty += cap_h;
    }
    ctx.text(
        &title,
        &TextStyle::new(title_size).with_weight(TextWeight::Semibold),
        primary,
        Point::new(text_x, ty),
        Some(width_avail),
    );
    ty += title_h;
    if has_body {
        ctx.text(
            body.as_deref().unwrap_or_default(),
            &TextStyle::new(body_size),
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
    }
}

impl Widget for ToastStack {
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
            let height =
                (TOAST_HEIGHT * idx_to_f64(n)) + (TOAST_GAP * idx_to_f64(n.saturating_sub(1)));
            (to_f32(TOAST_WIDTH), to_f32(height))
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    *ctx.handled = true;
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
    use automerge::AutoCommit;
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
                width: taffy::AvailableSpace::Definite(to_f32(TOAST_WIDTH)),
                height: taffy::AvailableSpace::Definite(1000.0),
            },
        )
        .unwrap();
        stack.apply_layout(&tree, node, Point::ZERO);
    }

    fn device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn feed(s: &mut ToastStack, ev: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    #[test]
    fn toast_card_paints_text_close_and_actions_inside_the_card() {
        // Case `three_line_text_block_stays_inside_the_card`.
        // Caption, title and body stack on their measured line boxes with an
        // `s1` gap, so in the 64 px card a desktop notification uses every
        // painted glyph line lands inside the card.
        {
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
                &ToastCard::new("Build finished", ToastKind::Success)
                    .caption("Terminal")
                    .body("3 warnings, 0 errors")
                    .closable(true),
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(runs.len() >= 3, "caption, title and body all paint");
            let mut tops: Vec<f64> = Vec::new();
            for run in runs {
                let top = f64::from(run.transform.translation[1]);
                let (_, h) = tr.measure("Ag", run.font_size, None);
                assert!(
                    top >= rect.y0 - 0.01 && top + f64::from(h) <= rect.y1 + 0.01,
                    "a glyph line at y={top} (h={h}) leaves the {rect:?} card",
                );
                tops.push(top);
            }
            tops.sort_by(f64::total_cmp);
            tops.dedup_by(|a, b| (*a - *b).abs() < 0.5);
            assert_eq!(tops.len(), 3, "three distinct baselines: {tops:?}");
            for pair in tops.windows(2) {
                assert!(
                    pair[1] - pair[0] >= f64::from(theme.tokens.typography.scale.xs),
                    "lines do not overlap: {tops:?}"
                );
            }
        }

        // Case `paint_card_returns_a_top_right_close_rect_when_closable`.
        {
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

        // Case `paint_card_actions_use_two_standard_non_overlapping_buttons`.
        {
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let card_rect = Rect::new(20.0, 30.0, 374.0, 142.0);
            let hit = paint_card(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                card_rect,
                &ToastCard::new("No visible video", ToastKind::Warning)
                    .caption("Picture in Picture")
                    .actions(
                        "Float page",
                        crate::icons::Icon::PictureInPicture2,
                        false,
                        "Select area",
                        crate::icons::Icon::MousePointer2,
                        false,
                    ),
            );
            assert!(hit.secondary_action_rect.area() > 0.0);
            assert!(hit.primary_action_rect.area() > 0.0);
            assert!(hit.secondary_action_rect.x1 < hit.primary_action_rect.x0);
            assert!(card_rect.contains(hit.secondary_action_rect.center()));
            assert!(card_rect.contains(hit.primary_action_rect.center()));
        }

        // Case `paint_card_emits_surface_fill_and_title_glyphs`.
        {
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

        // Case `fixed_height_card_collapses_multiline_localized_error_text`.
        {
            assert_eq!(
                single_line_text("  App\nfailed:\u{00a0}profile\tmissing  "),
                "App failed: profile missing"
            );

            let rect = Rect::new(0.0, 0.0, 360.0, 64.0);
            // This is the shape emitted for a launcher failure: caption, title,
            // and long body all share one fixed-height card. Exercise both the
            // normal and accessibility-large text scales so a localized newline
            // cannot introduce an extra baseline or push the close affordance
            // outside the bounded card.
            for theme in [Theme::dark(), Theme::dark().with_text_scale(1.5)] {
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                let hit = paint_card(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                rect,
                &ToastCard::new("Couldn't open\nNotes", ToastKind::Error)
                    .caption("Applications\nlauncher")
                    .body("The app stopped\nbefore opening. This deliberately long localized recovery message must elide safely.")
                    .closable(true),
            );
                assert_eq!(hit.card_rect, rect);
                assert!(hit.close_rect.area() > 0.0);
                assert!(
                    !scene.encoding().resources.glyph_runs.is_empty(),
                    "launcher failure text must render at text scale {}",
                    theme.text_scale
                );
            }

            // The painter's text inputs are normalized before elision; this
            // assertion protects the helper contract independently of glyph
            // backend details.
            assert!(!single_line_text("Couldn't open\nNotes").contains('\n'));
            assert!(!single_line_text("The app stopped\nbefore opening").contains('\n'));
        }
    }

    #[test]
    fn transient_cards_size_safely_anchor_and_fade() {
        // Case `transient_cards_share_safe_sizing_but_keep_the_requested_anchor`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let area = Rect::new(10.0, 20.0, 510.0, 320.0);
            let title =
                "A deliberately long notification that cannot fit in the available app area";

            let centered = paint_transient(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                ToastAnchor::BottomCenter,
                &ToastCard::new(title, ToastKind::Info),
            );
            let trailing = paint_transient(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                ToastAnchor::BottomRight,
                &ToastCard::new(title, ToastKind::Info),
            );

            assert!(centered.card_rect.width() > 320.0);
            assert!(centered.card_rect.width() <= area.width() - 48.0);
            assert_eq!(centered.card_rect.center().x, area.center().x);
            assert_eq!(trailing.card_rect.x1, area.x1 - 24.0);
            assert_eq!(centered.card_rect.y1, area.y1 - 24.0);
        }

        // Case `transient_card_honours_a_caller_inset_and_narrow_available_width`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            // The caller has removed 92 px of bottom player chrome from its area.
            // A narrow content column also cannot accommodate the normal minimum.
            let area = Rect::new(40.0, 10.0, 190.0, 308.0);
            let hit = paint_transient(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                ToastAnchor::BottomCenter,
                &ToastCard::new("This title must clip safely", ToastKind::Info),
            );

            assert_eq!(hit.card_rect.x0, area.x0 + 24.0);
            assert_eq!(hit.card_rect.x1, area.x1 - 24.0);
            assert_eq!(hit.card_rect.y1, area.y1 - 24.0);
            assert!(hit.card_rect.y0 >= area.y0);
        }

        // Case `partial_alpha_keeps_transient_geometry_and_still_paints`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let area = Rect::new(0.0, 0.0, 480.0, 320.0);
            let mut full_scene = Scene::new();
            let full = paint_transient(
                &mut full_scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                ToastAnchor::BottomCenter,
                &ToastCard::new("Backup complete", ToastKind::Success),
            );
            let mut faded_scene = Scene::new();
            let faded = paint_transient(
                &mut faded_scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                ToastAnchor::BottomCenter,
                &ToastCard::new("Backup complete", ToastKind::Success).alpha(0.35),
            );

            assert_eq!(faded.card_rect, full.card_rect);
            assert!(
                !faded_scene.encoding().draw_tags.is_empty(),
                "a partially faded card remains visible"
            );
        }

        // Case `paint_card_is_a_noop_when_fully_faded`.
        {
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
    }

    #[test]
    fn toast_interaction_dismisses_pauses_and_announces() {
        // Case `click_dismisses_the_toast_under_the_cursor`.
        {
            let mut s = ToastStack::new();
            s.push("first", ToastKind::Info);
            s.push("second", ToastKind::Error);
            lay_out(&mut s);
            // Card 1 ("second") spans y ∈ [56, 104); centre ≈ (160, 80).
            feed(
                &mut s,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (160.0, 80.0).into(),
                },
            );
            feed(
                &mut s,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert_eq!(s.len(), 1);
            assert_eq!(s.iter().next().unwrap().text, "first");
        }

        // Case `accessibility_reports_assertive_live_region_with_latest_toast`.
        {
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

        // Case `hover_pauses_expiry`.
        {
            let mut s = ToastStack::new().with_default_ttl(Duration::from_millis(100));
            s.push("stay", ToastKind::Info);
            lay_out(&mut s);
            // Hover over the only card.
            feed(
                &mut s,
                &WindowEvent::CursorMoved {
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
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (900.0, 900.0).into(),
                },
            );
            assert!(!s.is_hovered());
            s.tick(Duration::from_millis(500));
            assert_eq!(s.len(), 0, "expires once un-hovered");
        }
    }
}
