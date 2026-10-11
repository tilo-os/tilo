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
//! The `Widget` impl draws each active toast as a stacked floating card with
//! its severity icon and a dismiss control. The stack advances itself from
//! [`crate::time`]: an app only pushes. Expiry pauses while the pointer is
//! over the stack, and under the capture clock (`--shots`) so a scenario can
//! hold a toast in frame.

use crate::time::Duration;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::ButtonRole;
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

/// Placement for a transient card painted inside an app's area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastAnchor {
    /// Centred, at the bottom of the area.
    BottomCenter,
    /// At the area's bottom trailing corner.
    BottomRight,
    /// Centred, at the top of the area (under a toolbar or a capture HUD).
    TopCenter,
    /// At the area's top trailing corner.
    TopRight,
}

impl ToastAnchor {
    fn is_top(self) -> bool {
        matches!(self, Self::TopCenter | Self::TopRight)
    }

    fn is_centered(self) -> bool {
        matches!(self, Self::TopCenter | Self::BottomCenter)
    }
}

/// Whether a transient shown at `shown` for `ttl` has expired. Under the
/// capture clock (`--shots`) nothing expires, so a scenario that stages a
/// toast keeps it in frame.
#[must_use]
pub fn transient_expired(shown: crate::time::Instant, ttl: Duration) -> bool {
    !crate::time::wall_frozen() && shown.elapsed() > ttl
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
    /// The card whose dismiss control is under the pointer.
    close_hovered: Option<usize>,
    /// When the stack last advanced its toasts.
    advanced_at: Option<crate::time::Instant>,
    /// The number of cards the layout was sized for.
    laid_out: usize,
    /// The theme the stack was laid out in, for its controls' geometry.
    theme: Theme,
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
            .field("close_hovered", &self.close_hovered)
            .field("advanced_at", &self.advanced_at)
            .field("laid_out", &self.laid_out)
            .finish_non_exhaustive()
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
            close_hovered: None,
            advanced_at: None,
            laid_out: 0,
            theme: Theme::dark(),
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
        // Charge elapsed time only to entries that existed during that time.
        // A new notice gets its full lifetime even if the previous frame stalled.
        self.advance(crate::time::now());
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
        let height = stack_height(n);
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

    /// Advance the toasts to `now`: the time since the last advance counts
    /// against each toast's lifetime, except while the pointer is over the
    /// stack and under the capture clock. The widget calls this itself on
    /// every sync; an app only pushes.
    pub fn advance(&mut self, now: crate::time::Instant) {
        let last = self.advanced_at.replace(now);
        if self.toasts.is_empty() || crate::time::wall_frozen() {
            return;
        }
        if let Some(last) = last {
            self.tick(now.saturating_duration_since(last));
        }
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

    fn stack_size(&self) -> (f32, f32) {
        let n = self.toasts.len();
        if n == 0 {
            (0.0, 0.0)
        } else {
            (to_f32(TOAST_WIDTH), to_f32(stack_height(n)))
        }
    }

    /// The dismiss control of the card at `index`, as [`paint_card`] lays
    /// it out.
    fn close_rect(&self, index: usize) -> Rect {
        let y0 = self.rect.y0 + idx_to_f64(index) * (TOAST_HEIGHT + TOAST_GAP);
        let card = Rect::new(
            self.rect.x0,
            y0,
            self.rect.x0 + TOAST_WIDTH,
            y0 + TOAST_HEIGHT,
        );
        dismiss_rect(card, &self.theme)
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

/// The height of a stack of `n` cards.
fn stack_height(n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        (TOAST_HEIGHT * idx_to_f64(n)) + (TOAST_GAP * idx_to_f64(n.saturating_sub(1)))
    }
}

/// The card radius: concentric with the controls inside it, never a pill.
fn card_radius(theme: &Theme) -> f64 {
    let radii = &theme.tokens.radii;
    f64::from(
        (radii.control + theme.tokens.spacing.s2)
            .min(radii.panel)
            .max(radii.detail),
    )
}

/// The card's padding: `(x, y)`.
fn card_pad(theme: &Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.spacing.s3),
        f64::from(theme.tokens.spacing.s2),
    )
}

/// The severity icon, the same one a banner of that kind shows.
fn kind_icon(kind: ToastKind) -> crate::icons::Icon {
    match kind {
        ToastKind::Info => crate::widget::BannerKind::Info,
        ToastKind::Success => crate::widget::BannerKind::Success,
        ToastKind::Warning => crate::widget::BannerKind::Warning,
        ToastKind::Error => crate::widget::BannerKind::Error,
    }
    .default_icon()
}

/// Width of a card's leading column after its padding: the severity icon
/// and the gap to the text.
fn leading_column(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_md) + f64::from(theme.tokens.metrics.icon_label_gap)
}

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
    /// name), a dim `Caption` in sentence case. Empty = no caption.
    pub caption: &'a str,
    /// Severity — drives the leading icon and its colour.
    pub kind: ToastKind,
    /// Enter/exit animation opacity in `[0, 1]`; `1.0` for a settled card.
    pub alpha: f32,
    /// Draw a dismiss (`X`) button at the top-right and hand its rect back
    /// via [`ToastCardHit`] so the caller keeps one hit-test on widget-owned
    /// geometry.
    pub closable: bool,
    /// Hover state for the dismiss button, when the caller tracks the cursor.
    pub close_hovered: bool,
    /// Whether the shared pointer controller has armed this control.
    pub close_pressed: bool,
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
    /// Whether the shared pointer controller has armed this control.
    pub secondary_action_pressed: bool,
    pub primary_action: &'a str,
    pub primary_action_icon: Option<crate::icons::Icon>,
    pub primary_action_hovered: bool,
    /// Whether the shared pointer controller has armed this control.
    pub primary_action_pressed: bool,
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
            close_pressed: false,
            actionable: false,
            secondary_action: "",
            secondary_action_icon: None,
            secondary_action_hovered: false,
            secondary_action_pressed: false,
            primary_action: "",
            primary_action_icon: None,
            primary_action_hovered: false,
            primary_action_pressed: false,
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
    /// One action, at the card's trailing edge beside its text ("Undo",
    /// "Open").
    #[must_use]
    pub fn action(mut self, label: &'a str, hovered: bool) -> Self {
        self.secondary_action = "";
        self.secondary_action_icon = None;
        self.primary_action = label;
        self.primary_action_icon = None;
        self.primary_action_hovered = hovered;
        self
    }

    /// Whether the card carries only one action, beside its text.
    fn single_action(&self) -> bool {
        self.secondary_action.is_empty() && !self.primary_action.is_empty()
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

/// Minimum height for a compact card with one elided line per text field.
/// Uses the same normalized text, roles, padding and shared action controls as
/// `paint_card`. A host choosing a fixed-width banner must reserve this height
/// before stacking cards; enlarging type must not paint text over its actions.
pub fn minimum_card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
) -> f64 {
    let (_, pad_y) = card_pad(theme);
    let text_height: f64 = [
        (card.caption, theme.text(TextRole::Caption)),
        (card.title, theme.text(TextRole::Label).strong()),
        (card.body, theme.text(TextRole::Caption)),
    ]
    .into_iter()
    .map(|(text, style)| {
        let line = single_line_text(text);
        if line.is_empty() {
            0.0
        } else {
            f64::from(text_renderer.measure_style(&line, None, &style).1)
        }
    })
    .sum();
    let control_height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let mut height = text_height
        .max(f64::from(theme.tokens.metrics.icon_md))
        .max(if card.closable || card.single_action() {
            control_height
        } else {
            0.0
        })
        + 2.0 * pad_y;
    if !card.secondary_action.is_empty() && !card.primary_action.is_empty() {
        height += control_height + pad_y + f64::from(theme.tokens.spacing.s1);
    }
    height.ceil()
}

/// Paint a toast / notification card into `rect` and return the geometry a
/// caller needs to route clicks. The card is width/height-agnostic (it lays
/// out relative to `rect`), so a 48px single-line in-app toast and a 64px
/// three-line OS notification share this one renderer.
///
/// Layout: soft drop shadow → the floating material + edge → leading
/// severity icon → (optional) attribution caption / title / body text column →
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
    let mut hit = ToastCardHit::default();
    let a = card.alpha.clamp(0.0, 1.0);
    if rect.width() <= 0.0 || rect.height() <= 0.0 || a <= 0.0 {
        return hit;
    }
    hit.card_rect = rect;
    let (pad_x, pad_y) = card_pad(theme);
    paint_card_surface(scene, transform, rect, a, theme);

    // The severity is its icon's colour, as on a banner (HIG §9.5): the
    // at-a-glance state form, never a stripe crowding the panel's corner.
    let icon = f64::from(theme.tokens.metrics.icon_md);
    crate::icons::draw_icon(
        scene,
        transform,
        Rect::from_center_size(
            vello::kurbo::Point::new(rect.x0 + pad_x + icon / 2.0, rect.center().y),
            (icon, icon),
        ),
        kind_icon(card.kind),
        kind_color(card.kind, theme).with_alpha(a),
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    let text_x0 = rect.x0 + leading_column(theme);

    // Dismiss (X) button — a ghost `Button` face, top-right.
    let mut text_right = rect.x1 - pad_x;
    if let Some(close_rect) =
        paint_dismiss_button(scene, transform, text_renderer, theme, rect, card, a)
    {
        hit.close_rect = close_rect;
        text_right = close_rect.x0 - f64::from(theme.tokens.spacing.s1);
    }
    if card.single_action() {
        let action = paint_single_action(
            scene,
            transform,
            text_renderer,
            theme,
            Rect::new(rect.x0, rect.y0, text_right, rect.y1),
            card,
            a,
        );
        hit.primary_action_rect = action;
        text_right = action.x0 - f64::from(theme.tokens.spacing.s3);
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
        Rect::new(text_x0, rect.y0, text_right, action_top.unwrap_or(rect.y1)),
        card,
        a,
    );

    // Actionable chevron at the trailing-bottom edge, keyed to severity.
    if card.actionable {
        let sz = f64::from(theme.tokens.metrics.icon_sm);
        let ix = rect.x1 - pad_x - sz;
        let iy = rect.y1 - pad_y - sz;
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
    let (pad_x, pad_y) = card_pad(theme);
    let left = rect.x0 + pad_x;
    let right = rect.x1 - pad_x;
    let gap = f64::from(theme.tokens.spacing.rhythm.related);
    let y1 = rect.y1 - pad_y;
    let y0 = y1 - ButtonFace::height(crate::widget::ControlSize::Small, theme);
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
            &RoundedRect::from_rect(
                Rect::new(left, y0, right, y1),
                f64::from(theme.tokens.radii.control),
            ),
        );
    }
    let mut secondary_face = ButtonFace::new(secondary, card.secondary_action)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(card.secondary_action_hovered)
        .pressed(card.secondary_action_pressed);
    if let Some(icon) = card.secondary_action_icon {
        secondary_face = secondary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &secondary_face);
    let mut primary_face = ButtonFace::new(primary, card.primary_action)
        .role(ButtonRole::Primary)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(card.primary_action_hovered)
        .pressed(card.primary_action_pressed);
    if let Some(icon) = card.primary_action_icon {
        primary_face = primary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &primary_face);
    if faded {
        scene.pop_layer();
    }
    Some(y0 - f64::from(theme.tokens.spacing.s1))
}

/// The card's body: the floating material with its edge and shadow, at the
/// card's concentric radius, fading with the card.
fn paint_card_surface(scene: &mut Scene, transform: Affine, rect: Rect, alpha: f32, theme: &Theme) {
    let radius = card_radius(theme);
    let faded = alpha < 0.999;
    if faded {
        let reach = crate::widget::floating_surface::shadow_reach(theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(reach, reach),
        );
    }
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    crate::surface::panel(scene, transform, rect, radius, theme);
    if faded {
        scene.pop_layer();
    }
}

/// The width of a card's single action face.
fn single_action_width(text_renderer: &mut TextRenderer, theme: &Theme, label: &str) -> f64 {
    ButtonFace::new(Rect::ZERO, label)
        .control_size(crate::widget::ControlSize::Small)
        .intrinsic_width(text_renderer, theme)
}

/// Paint a card's one action at the trailing edge of `row`, centred on the
/// card, and return its rect.
fn paint_single_action(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> Rect {
    let width = single_action_width(text_renderer, theme, card.primary_action);
    let height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let x1 = if card.closable {
        row.x1
    } else {
        row.x1 - card_pad(theme).0
    };
    let rect = Rect::new(
        x1 - width,
        row.center().y - height / 2.0,
        x1,
        row.center().y + height / 2.0,
    );
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &rect);
    }
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(rect, card.primary_action)
            .control_size(crate::widget::ControlSize::Small)
            .hovered(card.primary_action_hovered)
            .pressed(card.primary_action_pressed),
    );
    if faded {
        scene.pop_layer();
    }
    rect
}

/// Paint a transient toast in `area`, at `anchor`.
///
/// This is the immediate-mode counterpart to [`ToastStack`], for apps that
/// already own their notification lifetime (check it with
/// [`transient_expired`]). The card is as wide as its title and body need,
/// up to a readable width, and wraps them there; it holds one action
/// ([`ToastCard::action`]) beside the text. The returned geometry lets
/// callers hit-test the action and the dismiss control.
pub fn paint_transient(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    anchor: ToastAnchor,
    card: &ToastCard<'_>,
) -> ToastCardHit {
    let edge_gap = f64::from(theme.tokens.spacing.s6);
    let available_width = (area.width() - 2.0 * edge_gap).max(0.0);
    let Some(size) = transient_size(text_renderer, theme, available_width, card) else {
        return ToastCardHit::default();
    };
    if area.height() < size.height + edge_gap {
        return ToastCardHit::default();
    }
    let x0 = if anchor.is_centered() {
        area.center().x - size.width / 2.0
    } else {
        area.x1 - edge_gap - size.width
    };
    let y0 = if anchor.is_top() {
        area.y0 + edge_gap
    } else {
        area.y1 - edge_gap - size.height
    };
    paint_card(
        scene,
        transform,
        text_renderer,
        theme,
        Rect::new(x0, y0, x0 + size.width, y0 + size.height),
        card,
    )
}

/// The size a transient card takes inside `available_width`, or `None`
/// when there is no room at all.
fn transient_size(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    available_width: f64,
    card: &ToastCard<'_>,
) -> Option<vello::kurbo::Size> {
    if available_width <= 0.0 {
        return None;
    }
    let (pad_x, pad_y) = card_pad(theme);
    let line = f64::from(theme.text(TextRole::Body).line_height_px());
    let max_width = (line * 22.0).min(available_width);
    let min_width = (line * 8.0).min(available_width);
    let close_column = if card.closable {
        ButtonFace::height(crate::widget::ControlSize::Small, theme)
            + f64::from(theme.tokens.spacing.s1)
    } else {
        0.0
    };
    let action_column = if card.single_action() {
        single_action_width(text_renderer, theme, card.primary_action)
            + f64::from(theme.tokens.spacing.s3)
    } else {
        0.0
    };
    let chrome = 2.0 * pad_x + leading_column(theme) + close_column + action_column;
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);
    let title = single_line_text(card.title);
    let body = single_line_text(card.body);
    let natural = f64::from(text_renderer.measure_style(&title, None, &title_style).0).max(
        if body.is_empty() {
            0.0
        } else {
            f64::from(text_renderer.measure_style(&body, None, &body_style).0)
        },
    );
    let width = (natural + chrome).clamp(min_width, max_width);
    let text_width = to_f32((width - chrome).max(1.0));
    let mut text_h = f64::from(
        text_renderer
            .measure_style(&title, Some(text_width), &title_style)
            .1,
    );
    if !body.is_empty() {
        text_h += f64::from(
            text_renderer
                .measure_style(&body, Some(text_width), &body_style)
                .1,
        );
    }
    let control = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let height = (text_h.max(control) + 2.0 * pad_y)
        .max(f64::from(theme.tokens.metrics.panel_row_height) + 2.0 * pad_y)
        .ceil();
    Some(vello::kurbo::Size::new(width, height))
}

/// Where a card's dismiss control sits: a Small icon button inset one step
/// from the top-trailing corner, centred on a single-line card.
fn dismiss_rect(card: Rect, theme: &Theme) -> Rect {
    let side = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let inset = f64::from(theme.tokens.spacing.s2);
    let cx1 = card.x1 - inset;
    let cy0 = (card.y0 + inset).min(card.center().y - side / 2.0);
    Rect::new(cx1 - side, cy0, cx1, cy0 + side)
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
    let close_rect = dismiss_rect(rect, theme);
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &close_rect,
        );
    }
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(close_rect, "")
            .role(ButtonRole::Plain)
            .icon(crate::icons::Icon::X)
            .control_size(crate::widget::control_size::ControlSize::Small)
            .hovered(card.close_hovered)
            .pressed(card.close_pressed)
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
    let (pad_x, pad_y) = card_pad(theme);
    let text_x = column.x0 + pad_x;
    let width_avail = to_f32((column.x1 - text_x).max(0.0));
    // A card sized for its text (`paint_transient`) wraps it; one that is
    // shorter than its wrapped text keeps each field to one elided line.
    if paint_wrapped_card_text(ctx, column, card, alpha, text_x, width_avail) {
        return;
    }
    let text_renderer = &mut *ctx.text;
    let width_avail = to_f32((column.x1 - text_x).max(0.0));
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);

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
    // height is ~1.3× its size, so size-derived heights under-reserved
    // every line and the block sat high in the card. The roles' own
    // leading is the space between lines.
    let line_gap = 0.0;
    let line_h = |text_renderer: &mut TextRenderer, text: &str, style: &TextStyle| {
        f64::from(text_renderer.measure_style(text, None, style).1)
    };
    let cap_h = if has_caption {
        line_h(text_renderer, &caption_text, &caption_style) + line_gap
    } else {
        0.0
    };
    let title_h =
        line_h(text_renderer, &title_text, &title_style) + if has_body { line_gap } else { 0.0 };
    let body_h = if has_body {
        line_h(text_renderer, &body_text, &body_style)
    } else {
        0.0
    };
    let block_h = cap_h + title_h + body_h;
    let text_height = column.height().max(0.0);
    let mut ty = column.y0 + ((text_height - block_h) / 2.0).max(pad_y);

    // `paint_card` deliberately owns a fixed-height notification surface.
    // Wrapping an untrusted summary/body into that surface makes the title
    // paint over the body (and can push the close affordance into the text).
    // Elide each line against the *actual* width left by the close button;
    // long diagnostics belong in the structured log, not in chrome.
    let caption = has_caption
        .then(|| text_renderer.elide_to_width_style(&caption_text, width_avail, &caption_style));
    let title = text_renderer.elide_to_width_style(&title_text, width_avail, &title_style);
    let body =
        has_body.then(|| text_renderer.elide_to_width_style(&body_text, width_avail, &body_style));

    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);

    if has_caption {
        ctx.text(
            caption.as_deref().unwrap_or_default(),
            &caption_style,
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
        ty += cap_h;
    }
    ctx.text(
        &title,
        &title_style,
        primary,
        Point::new(text_x, ty),
        Some(width_avail),
    );
    ty += title_h;
    if has_body {
        ctx.text(
            body.as_deref().unwrap_or_default(),
            &body_style,
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
    }
}

/// Paint the card's text wrapped at `width`, centred in `column`, when it
/// fits there whole. Returns whether it painted.
fn paint_wrapped_card_text(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
    text_x: f64,
    width: f32,
) -> bool {
    let theme = ctx.theme;
    let (_, pad_y) = card_pad(theme);
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);
    let caption = single_line_text(card.caption);
    let title = single_line_text(card.title);
    let body = single_line_text(card.body);
    let mut measure = |text: &str, style: &TextStyle| {
        if text.is_empty() {
            0.0
        } else {
            f64::from(ctx.text.measure_style(text, Some(width), style).1)
        }
    };
    let caption_h = measure(&caption, &caption_style);
    let title_h = measure(&title, &title_style);
    let body_h = measure(&body, &body_style);
    let single = f64::from(ctx.text.measure_style("Ag", None, &title_style).1);
    let block = caption_h + title_h + body_h;
    // Only take over when the text actually wraps and the card holds it.
    if title_h <= single * 1.2 && body_h <= single * 1.2 {
        return false;
    }
    if block > column.height() - 2.0 * pad_y {
        return false;
    }
    let mut y = column.y0 + (column.height() - block) / 2.0;
    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);
    for (text, style, ink, h) in [
        (&caption, &caption_style, dim, caption_h),
        (&title, &title_style, primary, title_h),
        (&body, &body_style, dim, body_h),
    ] {
        if text.is_empty() {
            continue;
        }
        ctx.text(text, style, ink, Point::new(text_x, y), Some(width));
        y += h;
    }
    true
}

impl Widget for ToastStack {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let (w, h) = self.stack_size();
        self.laid_out = self.toasts.len();
        let style = Style {
            size: Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        self.advance(crate::time::now());
    }

    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let before = self.toasts.len();
        self.advance(crate::time::now());
        before != self.toasts.len()
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        if self.hovered || crate::time::wall_frozen() {
            return None;
        }
        let last = self.advanced_at?;
        let soonest = self.toasts.iter().map(|t| t.ttl_remaining).min()?;
        Some(last + soonest)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut crate::text_render::TextRenderer,
    ) -> bool {
        let (w, h) = self.stack_size();
        let Ok(current) = taffy.style(node) else {
            return false;
        };
        let size = Size {
            width: Dimension::length(w),
            height: Dimension::length(h),
        };
        if current.size == size {
            return false;
        }
        let mut style = current.clone();
        style.size = size;
        self.laid_out = self.toasts.len();
        taffy.set_style(node, style).is_ok()
    }

    fn is_layout_dirty(&self) -> bool {
        self.laid_out != self.toasts.len()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        for (index, toast) in self.toasts.iter().enumerate() {
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
                &ToastCard::new(&toast.text, toast.kind)
                    .closable(true)
                    .close_hovered(self.close_hovered == Some(index)),
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
                    // Leaving restarts the clock from now, so the hovered
                    // time does not count against the toasts.
                    self.advanced_at = Some(crate::time::now());
                    self.hovered = now_hover;
                }
                self.close_hovered = self
                    .card_at(self.last_cursor)
                    .filter(|&index| self.close_rect(index).contains(self.last_cursor));
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
    use crate::testing::dispatch;
    use std::collections::HashMap;
    use winit::event::{ElementState, MouseButton};

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
                    pair[1] - pair[0] >= f64::from(theme.tokens.typography.size(TextRole::Caption)),
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
                hit.close_rect.y0 >= rect.y0
                    && hit.close_rect.y0 - rect.y0 <= f64::from(theme.tokens.spacing.s2) + 0.5,
                "close rect sits at the top of the card",
            );
            assert_eq!(
                hit.close_rect.height(),
                f64::from(theme.tokens.metrics.bar_control_height),
                "the close control is a Small icon button"
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
                "surface fill and the severity icon; got {color_fills}"
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
            dispatch(
                &mut s,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (160.0, 80.0).into(),
                },
            );
            dispatch(
                &mut s,
                &WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
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
            dispatch(
                &mut s,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (160.0, 24.0).into(),
                },
            );
            assert!(s.is_hovered());
            // tick longer than the TTL — but hovered, so it must NOT expire.
            s.tick(Duration::from_millis(500));
            assert_eq!(s.len(), 1, "hovered toast does not expire");
            // Move away → expiry resumes.
            dispatch(
                &mut s,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (900.0, 900.0).into(),
                },
            );
            assert!(!s.is_hovered());
            s.tick(Duration::from_millis(500));
            assert_eq!(s.len(), 0, "expires once un-hovered");
        }
    }
}
