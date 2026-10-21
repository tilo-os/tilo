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

use crate::num::{f32_of_f64, f64_of_usize, usize_of_f64};
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

/// Toast cards have a fixed height and therefore paint each text field on a
/// single baseline. Collapse all Unicode whitespace before measuring or
/// eliding a field so an app-provided/localized newline cannot turn a card
/// into an overflowing multi-line block. This also makes a body containing
/// only whitespace behave like an omitted body.
fn single_line_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
    !crate::time::wall_frozen() && crate::time::now().saturating_duration_since(shown) > ttl
}

/// How long a [`Notice`] stays up.
pub const NOTICE_LIFETIME: Duration = Duration::from_secs(4);

/// The one transient message an app shows over its content: a confirmation,
/// something under way, or a failure. The app shows it and paints it where
/// it belongs; the notice keeps its own lifetime and its announcement.
#[derive(Clone, Debug, Default)]
pub struct Notice {
    current: Option<(String, ToastKind, crate::time::Instant)>,
}

impl Notice {
    /// Show `text`, replacing any notice already up.
    pub fn show(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.current = Some((text.into(), kind, crate::time::now()));
    }

    /// Take the notice down; whether one was up.
    pub fn dismiss(&mut self) -> bool {
        self.current.take().is_some()
    }

    /// The notice's text and kind, while it is up.
    pub fn get(&self) -> Option<(&str, ToastKind)> {
        self.current
            .as_ref()
            .map(|(text, kind, _)| (text.as_str(), *kind))
    }

    pub fn is_shown(&self) -> bool {
        self.current.is_some()
    }

    /// Take the notice down once its lifetime is over; whether it did.
    pub fn expire(&mut self) -> bool {
        let over = self
            .current
            .as_ref()
            .is_some_and(|(_, _, shown)| transient_expired(*shown, NOTICE_LIFETIME));
        over && self.dismiss()
    }

    /// When the notice expires, for a widget's `next_wake_deadline`. Under
    /// the capture clock nothing expires.
    pub fn deadline(&self) -> Option<crate::time::Instant> {
        if crate::time::wall_frozen() {
            return None;
        }
        self.current
            .as_ref()
            .map(|(_, _, shown)| *shown + NOTICE_LIFETIME)
    }

    /// Paint the notice in `area`, at `anchor`, with [`paint_transient`].
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        anchor: ToastAnchor,
    ) -> ToastCardHit {
        let Some((text, kind)) = self.get() else {
            return ToastCardHit::default();
        };
        paint_transient(
            scene,
            transform,
            text_renderer,
            theme,
            area,
            anchor,
            &ToastCard::new(text, kind),
        )
    }

    /// The notice as a live region: a failure interrupts, anything else
    /// waits its turn.
    pub fn access_node(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        let (text, kind) = self.get()?;
        Some(AccessNode::new(Role::StaticText).with_name(text).with_live(
            if kind == ToastKind::Error {
                Politeness::Assertive
            } else {
                Politeness::Polite
            },
        ))
    }
}

/// How long a transient card has been up for someone to read, which is
/// what its lifetime counts against. The clock runs only while the card is
/// on screen and nobody holds it (the pointer on it, keyboard focus in it):
/// a card that never fit on screen, or one being read, never runs out under
/// the reader. [`ToastStack`] and the desktop's notification banners share
/// it, so a toast and a banner leave by the same rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dwell {
    /// Time on screen, unheld, before the current run.
    spent: Duration,
    /// When the current run began; `None` while the clock is stopped.
    since: Option<crate::time::Instant>,
    /// When the card first came on screen.
    first_shown: Option<crate::time::Instant>,
}

impl Dwell {
    /// A card nobody has seen yet: nothing spent, the clock stopped.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spent: Duration::ZERO,
            since: None,
            first_shown: None,
        }
    }

    /// Time the card has been readable on screen at `now`.
    #[must_use]
    pub fn spent(&self, now: crate::time::Instant) -> Duration {
        self.since.map_or(self.spent, |since| {
            self.spent
                .saturating_add(now.saturating_duration_since(since))
        })
    }

    /// What is left of a `lifetime` at `now`.
    #[must_use]
    pub fn left(&self, lifetime: Duration, now: crate::time::Instant) -> Duration {
        lifetime.saturating_sub(self.spent(now))
    }

    /// Whether a `lifetime` has run out at `now`.
    #[must_use]
    pub fn is_over(&self, lifetime: Duration, now: crate::time::Instant) -> bool {
        self.left(lifetime, now).is_zero()
    }

    /// Say whether the card is `on_screen` and whether someone `held` it
    /// at `now`. The clock runs exactly while it is on screen and unheld.
    pub fn present(&mut self, now: crate::time::Instant, on_screen: bool, held: bool) {
        if on_screen {
            self.first_shown.get_or_insert(now);
        }
        match (on_screen && !held, self.since) {
            (true, None) => self.since = Some(now),
            (false, Some(_)) => {
                self.spent = self.spent(now);
                self.since = None;
            }
            _ => {}
        }
    }

    /// Count `elapsed` against the card, for a host that advances by
    /// frame deltas rather than reading a clock.
    pub fn spend(&mut self, elapsed: Duration) {
        self.spent = self.spent.saturating_add(elapsed);
    }

    /// Leave at least `floor` of a `lifetime` at `now`: a card the person
    /// just let go of gets a moment before it starts to leave, instead of
    /// fading the instant the pointer moves off it.
    pub fn grant(&mut self, lifetime: Duration, now: crate::time::Instant, floor: Duration) {
        let cap = lifetime.saturating_sub(floor);
        if self.spent(now) > cap {
            self.spent = cap;
            if self.since.is_some() {
                self.since = Some(now);
            }
        }
    }

    /// When a `lifetime` runs out while the clock keeps running; `None`
    /// while it is stopped.
    #[must_use]
    pub fn ends_at(&self, lifetime: Duration) -> Option<crate::time::Instant> {
        let since = self.since?;
        since.checked_add(lifetime.saturating_sub(self.spent))
    }

    /// When the card first came on screen; `None` before it has.
    #[must_use]
    pub fn first_shown(&self) -> Option<crate::time::Instant> {
        self.first_shown
    }

    /// Whether the clock is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.since.is_some()
    }
}

/// A single toast entry held by a [`ToastStack`]. Entries are produced
/// internally by [`ToastStack::push`]; shell code does not construct
/// `Toast` values directly.
#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    /// What its one button does ("Undo"), if it has one.
    pub action: Option<String>,
    lifetime: Duration,
    dwell: Dwell,
}

impl Toast {
    /// What is left of this toast's lifetime.
    #[must_use]
    pub fn ttl_remaining(&self) -> Duration {
        self.lifetime.saturating_sub(self.dwell.spent)
    }
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
    /// What the last draw painted: each card's text and kind, the dismiss
    /// control under the pointer, and where. A change needs another draw;
    /// `None` until drawn in the current theme.
    painted: Option<PaintedToasts>,
    /// Each card's button, where the last draw painted it.
    action_rects: Vec<Option<Rect>>,
    /// The card whose button is under the pointer, now and as last drawn.
    action_hovered: Option<usize>,
    action_hovered_painted: Option<usize>,
    /// The text of the notice whose button was pressed, until the owner
    /// takes it.
    activated: Option<String>,
}

/// [`ToastStack::painted`].
type PaintedToasts = (Vec<(String, ToastKind)>, Option<usize>, Rect);

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
            painted: None,
            action_rects: Vec::new(),
            action_hovered: None,
            action_hovered_painted: None,
            activated: None,
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
        self.push_toast(text.into(), kind, None);
    }

    /// Push a notice with one button, `action` ("Undo"). Pressing it takes
    /// the notice down; [`Self::take_activated`] then names the notice.
    pub fn push_with_action(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        action: impl Into<String>,
    ) {
        self.push_toast(text.into(), kind, Some(action.into()));
    }

    /// The text of the notice whose button was pressed since the last call.
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    /// Take down every notice that offers a button: what it would act on
    /// has moved on, so its "Undo" would undo something else.
    pub fn dismiss_actionable(&mut self) {
        self.toasts.retain(|toast| toast.action.is_none());
    }

    fn push_toast(&mut self, text: String, kind: ToastKind, action: Option<String>) {
        // Charge elapsed time only to entries that existed during that time.
        // A new notice gets its full lifetime even if the previous frame stalled.
        self.advance(crate::time::now());
        let toast = Toast {
            text,
            kind,
            action,
            lifetime: self.default_ttl,
            dwell: Dwell::new(),
        };
        self.toasts.push(toast);
        // Drop oldest entries until we are back within the cap.
        while self.toasts.len() > self.max {
            self.toasts.remove(0);
        }
    }

    /// Every pixel the stack's cards paint, their shadows included: what a
    /// host repaints where the stack appears, changes or goes. `None` with
    /// no card showing.
    #[must_use]
    pub fn painted_bounds(&self, theme: &Theme) -> Option<Rect> {
        (!self.toasts.is_empty() && self.rect.area() > 0.0)
            .then(|| crate::widget::floating_surface::painted_bounds(self.rect, theme))
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

    /// Advance time. `dt` counts against each toast's [`Dwell`]; entries
    /// whose lifetime runs out are removed. Expiry is **paused while the
    /// pointer hovers the stack** so a toast can be read or clicked without
    /// vanishing.
    pub fn tick(&mut self, dt: Duration) {
        if self.hovered {
            return;
        }
        for t in &mut self.toasts {
            t.dwell.spend(dt);
        }
        self.toasts.retain(|t| !t.ttl_remaining().is_zero());
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
        let idx = usize_of_f64(rel / stride);
        // Reject the inter-card gap.
        if rel - f64_of_usize(idx) * stride > TOAST_HEIGHT {
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
            (f32_of_f64(TOAST_WIDTH), f32_of_f64(stack_height(n)))
        }
    }

    /// The dismiss control of the card at `index`, as [`paint_card`] lays
    /// it out.
    fn close_rect(&self, index: usize) -> Rect {
        let y0 = self.rect.y0 + f64_of_usize(index) * (TOAST_HEIGHT + TOAST_GAP);
        let card = Rect::new(
            self.rect.x0,
            y0,
            self.rect.x0 + TOAST_WIDTH,
            y0 + TOAST_HEIGHT,
        );
        dismiss_rect(card, &self.theme)
    }

    /// The card whose button is at `point`, as last painted.
    fn action_at(&self, point: Point) -> Option<usize> {
        self.action_rects
            .iter()
            .position(|rect| rect.is_some_and(|rect| rect.contains(point)))
            .filter(|index| *index < self.toasts.len())
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
        (TOAST_HEIGHT * f64_of_usize(n)) + (TOAST_GAP * f64_of_usize(n.saturating_sub(1)))
    }
}

/// The radius of a floating card: a notification banner, a toast, and the
/// status cards that stand in a column with them. One floating card is one
/// shape everywhere, the floating Panel's (`radii.panel`), so a banner, the
/// palette and a menu never disagree about their corners.
#[must_use]
pub fn card_radius(theme: &Theme) -> f64 {
    crate::widget::floating_surface::radius(theme)
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
/// The side of the card's leading mark, if it has one: a person's avatar
/// is the size of a list row's, a glyph an icon's.
fn leading_side(theme: &Theme, card: &ToastCard<'_>) -> Option<f64> {
    if card.kind == ToastKind::Info && card.person.is_some() {
        return Some(crate::widget::AvatarSize::Regular.side(theme));
    }
    (card.kind != ToastKind::Info || card.icon.is_some())
        .then(|| f64::from(theme.tokens.metrics.icon_md))
}

/// The width the leading mark takes before the text, gap included; none
/// for a card without a mark.
fn leading_column(theme: &Theme, card: &ToastCard<'_>) -> f64 {
    leading_side(theme, card).map_or(0.0, |side| {
        side + f64::from(theme.tokens.metrics.icon_label_gap)
    })
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
    /// What plain information leads with instead of a severity glyph,
    /// which only warnings, errors and successes carry: a glyph naming
    /// what the card is about...
    pub icon: Option<crate::icons::Icon>,
    /// The person the card is from, as `(name, identity)`: their avatar
    /// leads the card, in their presence colour, ahead of any glyph. Plain
    /// information with neither a person nor an icon leads with its text.
    pub person: Option<(&'a str, &'a str)>,
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
            icon: None,
            person: None,
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
    /// Lead plain information with `icon` (see [`Self::icon`]).
    #[must_use]
    pub fn icon(mut self, icon: Option<crate::icons::Icon>) -> Self {
        self.icon = icon;
        self
    }
    /// Lead the card with the avatar of the person it is from.
    #[must_use]
    pub fn person(mut self, name: &'a str, identity: &'a str) -> Self {
        self.person = Some((name, identity));
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
        .max(leading_side(theme, card).unwrap_or(f64::from(theme.tokens.metrics.icon_md)))
        .max(if card.closable || card.single_action() {
            control_height
        } else {
            0.0
        })
        + 2.0 * pad_y;
    if !card.secondary_action.is_empty() && !card.primary_action.is_empty() {
        height += control_height + pad_y + choice_lead(theme);
    }
    height.ceil()
}

/// Height of a card `width` wide whose body may wrap to a second line, as
/// a notification's line of context often needs to. A body longer than
/// that keeps the compact height and is elided to one line.
pub fn card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
    width: f64,
) -> f64 {
    let compact = minimum_card_height(text_renderer, theme, card);
    let body = single_line_text(card.body);
    if body.is_empty() {
        return compact;
    }
    let (pad_x, _) = card_pad(theme);
    let trailing = if card.closable {
        f64::from(theme.tokens.spacing.s2)
            + ButtonFace::height(crate::widget::ControlSize::Small, theme)
            + f64::from(theme.tokens.spacing.s1)
    } else {
        pad_x
    };
    let choices = !card.secondary_action.is_empty() && !card.primary_action.is_empty();
    if card.single_action() && single_action_inline(text_renderer, theme, card, width) {
        return compact;
    }
    if card.single_action() || choices {
        // The actions end the card, so the text has the card's width and
        // wraps there whole rather than eliding: a card that asks the
        // person to choose must say all of what they choose between.
        let control_height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
        let (_, pad_y) = card_pad(theme);
        let text_width =
            f32_of_f64((width - leading_column(theme, card) - 2.0 * pad_x - trailing).max(1.0));
        let block: f64 = [
            (card.caption, theme.text(TextRole::Caption)),
            (card.title, theme.text(TextRole::Label).strong()),
            (card.body, theme.text(TextRole::Caption)),
        ]
        .into_iter()
        .map(|(text, style)| {
            let text = single_line_text(text);
            if text.is_empty() {
                0.0
            } else {
                f64::from(
                    text_renderer
                        .measure_style(&text, Some(text_width), &style)
                        .1,
                )
            }
        })
        .sum();
        let wrapped = block
            .max(leading_side(theme, card).unwrap_or(0.0))
            .max(if card.closable { control_height } else { 0.0 })
            + 2.0 * pad_y
            + choice_lead(theme)
            + choice_height(theme)
            + choice_foot(theme);
        // `compact` already holds a pair of choices; a single action that
        // moved under the text adds its row.
        let least = if choices {
            compact
        } else {
            compact + control_height + pad_y + choice_lead(theme)
        };
        return wrapped.max(least).ceil();
    }
    let text_width = width - leading_column(theme, card) - pad_x - trailing;
    if text_width <= 0.0 {
        return compact;
    }
    let style = theme.text(TextRole::Caption);
    let one = f64::from(text_renderer.measure_style(&body, None, &style).1);
    let wrapped = f64::from(
        text_renderer
            .measure_style(&body, Some(f32_of_f64(text_width)), &style)
            .1,
    );
    if wrapped <= one * 1.5 || wrapped > one * 2.5 {
        return compact;
    }
    (compact + wrapped - one).ceil()
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

    let text_x0 = rect.x0 + leading_column(theme, card);

    // Dismiss (X) button — a ghost `Button` face, top-right.
    let mut text_right = if let Some(close_rect) =
        paint_dismiss_button(scene, transform, text_renderer, theme, rect, card, a)
    {
        hit.close_rect = close_rect;
        close_rect.x0 - f64::from(theme.tokens.spacing.s1)
    } else {
        rect.x1 - pad_x
    };
    let mut action_top = None;
    if card.single_action() {
        if single_action_inline(text_renderer, theme, card, rect.width()) {
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
        } else {
            // Beside the text it would squeeze the text to nothing: it
            // ends the card instead, where a pair of choices would.
            let y1 = rect.y1 - choice_foot(theme);
            let y0 = y1 - choice_height(theme);
            let x1 = if card.closable {
                rect.x1 - pad_x
            } else {
                rect.x1
            };
            hit.primary_action_rect = paint_single_action(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(rect.x0, y0, x1, y1),
                card,
                a,
            );
            action_top = Some(y0 - choice_lead(theme));
        }
    }

    // Text column: optional attribution caption, then the title, then the
    // optional body.
    let action_top = action_top.or(paint_card_actions(
        scene,
        transform,
        text_renderer,
        theme,
        rect,
        card,
        a,
        &mut hit,
    ));
    let block = paint_card_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        Rect::new(text_x0, rect.y0, text_right, action_top.unwrap_or(rect.y1)),
        card,
        a,
    );

    // The severity is its icon's colour, as on a banner (HIG §9.5): the
    // at-a-glance state form, never a stripe crowding the panel's corner.
    // It sits on the title's line, where the eye starts reading, not in the
    // middle of a card that also holds a caption or actions. Colour is kept
    // for states that need attention; plain information stays neutral, so
    // a calm stack of notices never reads as a column of alerts.
    paint_leading(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        rect.x0 + pad_x,
        block,
        card,
        a,
    );

    // Actionable chevron at the trailing-bottom edge, keyed to severity:
    // only for a card the whole of which opens something. One that shows
    // its choices as buttons says it already, and the chevron would sit on
    // the last of them.
    if card.actionable && card.primary_action.is_empty() {
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

/// The card's leading mark at `left`: a severity glyph for anything that
/// needs attention; for plain information whom it is from (their avatar,
/// level with the top of the text, as a message reads), else a glyph for
/// what it is about, on the title's line. A generic "i" or an app's letter
/// on every card would say nothing, so a card with neither leads with text.
fn paint_leading(
    ctx: &mut PaintCtx<'_>,
    left: f64,
    block: TextBlock,
    card: &ToastCard<'_>,
    alpha: f32,
) {
    let theme = ctx.theme;
    let Some(side) = leading_side(theme, card) else {
        return;
    };
    if let (ToastKind::Info, Some((name, identity))) = (card.kind, card.person) {
        // Top-aligned with a block at least as tall as the avatar; centred
        // on a shorter one, so a one-line card is not top-heavy.
        let centre_y = if block.bottom - block.top >= side {
            block.top + side / 2.0
        } else {
            (block.top + block.bottom) / 2.0
        };
        let rect = Rect::from_center_size(Point::new(left + side / 2.0, centre_y), (side, side));
        let faded = alpha < 0.999;
        if faded {
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                ctx.transform,
                &rect,
            );
        }
        crate::widget::AvatarFace::new(name, identity).paint(ctx, rect);
        if faded {
            ctx.scene.pop_layer();
        }
        return;
    }
    let mark = Rect::from_center_size(
        Point::new(left + side / 2.0, block.title_centre),
        (side, side),
    );
    let (icon, color) = match (card.kind, card.icon) {
        (ToastKind::Info, Some(icon)) => (icon, theme.tokens.colors.text_dim),
        (ToastKind::Info, None) => return,
        (kind, _) => (kind_icon(kind), kind_color(kind, theme)),
    };
    crate::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        mark,
        icon,
        color.with_alpha(alpha),
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Where a card's text landed: the centre of the title's first line and
/// the block's vertical extent, for the leading mark to line up with.
#[derive(Clone, Copy, Debug)]
struct TextBlock {
    title_centre: f64,
    top: f64,
    bottom: f64,
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
    let (pad_x, _) = card_pad(theme);
    let left = rect.x0 + pad_x;
    let right = rect.x1 - pad_x;
    let y1 = rect.y1 - choice_foot(theme);
    let y0 = y1 - choice_height(theme);
    let (secondary, primary) = choice_rects(
        Rect::new(left, y0, right, y1),
        choice_gap(theme),
        choice_width(
            text_renderer,
            theme,
            card.secondary_action,
            card.secondary_action_icon,
        ),
        choice_width(
            text_renderer,
            theme,
            card.primary_action,
            card.primary_action_icon,
        ),
    );
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
                Rect::new(secondary.x0, y0, right, y1),
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
    Some(y0 - choice_lead(theme))
}

/// The height of a card's choices: compact controls, so a card reads as
/// its message first.
pub fn choice_height(theme: &Theme) -> f64 {
    ButtonFace::height(crate::widget::ControlSize::Small, theme)
}

/// The step between a card's text and its choices: a full related step,
/// so the choices read as the message's answer rather than part of it.
pub fn choice_lead(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The padding under a card's choices: the card's own vertical padding.
pub fn choice_foot(theme: &Theme) -> f64 {
    card_pad(theme).1
}

/// The space between a card's two choices.
pub fn choice_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.related)
}

/// The width a card's choice asks for: its label (and icon) on a compact
/// face, never narrower than a comfortable target.
pub fn choice_width(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    icon: Option<crate::icons::Icon>,
) -> f64 {
    let mut face =
        ButtonFace::new(Rect::ZERO, label).control_size(crate::widget::ControlSize::Small);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    face.intrinsic_width(text_renderer, theme)
        .max(2.0 * choice_height(theme))
}

/// Where a pair of choices sits on `band` given the widths they ask for:
/// at the trailing edge at their own width, the answer last, as a dialog's
/// do, so the card reads as a sentence that ends in its choices rather than
/// a pair of slabs. Equal halves only when both cannot fit at their width.
/// Returns `(secondary, primary)`.
pub fn choice_rects(band: Rect, gap: f64, secondary: f64, primary: f64) -> (Rect, Rect) {
    if primary + gap + secondary <= band.width() {
        let primary = Rect::new(band.x1 - primary, band.y0, band.x1, band.y1);
        let x1 = primary.x0 - gap;
        (Rect::new(x1 - secondary, band.y0, x1, band.y1), primary)
    } else {
        let mid = (band.x0 + band.x1 - gap) / 2.0;
        (
            Rect::new(band.x0, band.y0, mid, band.y1),
            Rect::new(mid + gap, band.y0, band.x1, band.y1),
        )
    }
}

/// The card's body: the one floating card
/// ([`crate::widget::floating_surface::paint_card`]), fading with the card.
///
/// Anything that stands in a column of cards (a locked-screen stand-in for
/// them, the card for the ones that did not fit) paints this same body, so
/// the column is one material and one shape, and the palette's.
pub fn paint_card_surface(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    alpha: f32,
    theme: &Theme,
) {
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &crate::widget::floating_surface::painted_bounds(rect, theme),
        );
    }
    crate::widget::floating_surface::paint_card(scene, transform, rect, theme);
    if faded {
        scene.pop_layer();
    }
}

/// A card's ground and its edge: the floating card's opaque panel ground
/// and the panel edge.
#[must_use]
pub fn card_ground(theme: &Theme) -> (vello::peniko::Color, vello::peniko::Color) {
    (
        crate::widget::floating_surface::card_ground(theme),
        crate::chrome::panel_border(theme),
    )
}

/// The width of a card's single action face.
/// Whether a card `width` wide keeps its one action beside the text: only
/// while the text keeps room to read, its whole title or some eight lines'
/// height of width. Large text in a narrow column moves the action under
/// the text instead.
fn single_action_inline(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
    width: f64,
) -> bool {
    let (pad_x, _) = card_pad(theme);
    let trailing = if card.closable {
        f64::from(theme.tokens.spacing.s2)
            + ButtonFace::height(crate::widget::ControlSize::Small, theme)
            + f64::from(theme.tokens.spacing.s1)
    } else {
        pad_x
    };
    let room = width
        - leading_column(theme, card)
        - pad_x
        - trailing
        - single_action_width(text_renderer, theme, card.primary_action)
        - f64::from(theme.tokens.spacing.s3);
    let title = theme.text(TextRole::Label).strong();
    let (title_width, _) = text_renderer.measure_style(&single_line_text(card.title), None, &title);
    let (_, line) = text_renderer.measure_style("Ag", None, &theme.text(TextRole::Caption));
    room >= f64::from(title_width) || room >= f64::from(line) * 8.0
}

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
    let chrome = 2.0 * pad_x + leading_column(theme, card) + close_column + action_column;
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
    let text_width = f32_of_f64((width - chrome).max(1.0));
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

/// How much smaller than a small icon the resting dismiss glyph is.
const DISMISS_GLYPH_SCALE: f64 = 0.8;

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
    if card.close_hovered || card.close_pressed {
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
    } else {
        // At rest the close is a small glyph in the faintest ink, well
        // under the title's weight: it is the way out, not the message.
        // The pointer on it brings the full control back.
        let side = f64::from(theme.tokens.metrics.icon_sm) * DISMISS_GLYPH_SCALE;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(close_rect.center(), (side, side)),
            crate::icons::Icon::X,
            theme.tokens.colors.text_tertiary,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
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
fn paint_card_text(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> TextBlock {
    let theme = ctx.theme;
    let (pad_x, pad_y) = card_pad(theme);
    let text_x = column.x0 + pad_x;
    let width_avail = f32_of_f64((column.x1 - text_x).max(0.0));
    // A card sized for its text (`paint_transient`) wraps it; one that is
    // shorter than its wrapped text keeps each field to one elided line.
    if let Some(block) = paint_wrapped_card_text(ctx, column, card, alpha, text_x, width_avail) {
        return block;
    }
    let text_renderer = &mut *ctx.text;
    let width_avail = f32_of_f64((column.x1 - text_x).max(0.0));
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
    let top = ty;

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
    let title_centre = ty + (title_h - line_gap) / 2.0;
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
    TextBlock {
        title_centre,
        top,
        bottom: top + block_h,
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
) -> Option<TextBlock> {
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
        return None;
    }
    if block > column.height() - 2.0 * pad_y {
        return None;
    }
    let mut y = column.y0 + (column.height() - block) / 2.0;
    let extent = TextBlock {
        // The first line of the title, however many it wraps to.
        title_centre: y + caption_h + single.min(title_h) / 2.0,
        top: y,
        bottom: y + block,
    };
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
    Some(extent)
}

impl Widget for ToastStack {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        self.painted = None;
        let (w, h) = self.stack_size();
        self.laid_out = self.toasts.len();
        let style = Style {
            size: Size::from_lengths(w, h),
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
        let soonest = self.toasts.iter().map(Toast::ttl_remaining).min()?;
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

    crate::rect_geometry!();

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
        let mut action_rects = Vec::with_capacity(self.toasts.len());
        for (index, toast) in self.toasts.iter().enumerate() {
            let card_rect = Rect::new(
                self.rect.x0,
                y,
                self.rect.x0 + TOAST_WIDTH,
                y + TOAST_HEIGHT,
            );
            let mut card = ToastCard::new(&toast.text, toast.kind)
                .closable(true)
                .close_hovered(self.close_hovered == Some(index));
            if let Some(action) = &toast.action {
                card = card.action(action, self.action_hovered == Some(index));
            }
            // Route through the shared card renderer so an in-app toast and a
            // desktop notification are pixel-identical (HIG §8).
            let hit = paint_card(scene, transform, text_renderer, theme, card_rect, &card);
            action_rects.push(
                (toast.action.is_some() && hit.primary_action_rect.area() > 0.0)
                    .then_some(hit.primary_action_rect),
            );
            y += TOAST_HEIGHT + TOAST_GAP;
        }
        self.action_rects = action_rects;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
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
                self.action_hovered = self.action_at(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.action_at(self.last_cursor) {
                    self.activated = self.toasts.get(idx).map(|toast| toast.text.clone());
                    self.dismiss(idx);
                    self.hovered = false;
                    *ctx.handled = true;
                } else if let Some(idx) = self.card_at(self.last_cursor) {
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

    /// A stack that shows what it painted (nothing, mostly) needs no frame:
    /// a pointer moving over the window repaints nothing here.
    fn needs_redraw(&self) -> bool {
        self.painted
            .as_ref()
            .is_none_or(|(cards, close_hovered, rect)| {
                *close_hovered != self.close_hovered
                    || self.action_hovered != self.action_hovered_painted
                    || *rect != self.rect
                    || cards.len() != self.toasts.len()
                    || cards
                        .iter()
                        .zip(&self.toasts)
                        .any(|((text, kind), toast)| *text != toast.text || *kind != toast.kind)
            })
    }

    fn mark_drawn(&mut self) {
        self.action_hovered_painted = self.action_hovered;
        self.painted = Some((
            self.toasts
                .iter()
                .map(|toast| (toast.text.clone(), toast.kind))
                .collect(),
            self.close_hovered,
            self.rect,
        ));
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

    /// A card's lifetime counts only time on screen and unheld: off screen
    /// or under the pointer it keeps what it has left, and letting go of it
    /// leaves a moment before it starts to leave.
    #[test]
    fn a_dwell_runs_only_while_shown_and_unheld() {
        let lifetime = Duration::from_secs(5);
        let t0 = crate::time::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut dwell = Dwell::new();
        assert!(!dwell.is_over(lifetime, at(60_000)), "unseen never ends");
        assert_eq!(dwell.first_shown(), None);
        dwell.present(at(1_000), true, false);
        assert_eq!(dwell.first_shown(), Some(at(1_000)));
        assert_eq!(dwell.left(lifetime, at(3_000)), Duration::from_secs(3));
        assert_eq!(dwell.ends_at(lifetime), Some(at(6_000)));
        dwell.present(at(3_000), true, true);
        assert!(!dwell.is_running());
        assert_eq!(dwell.ends_at(lifetime), None);
        assert_eq!(
            dwell.left(lifetime, at(30_000)),
            Duration::from_secs(3),
            "a held card keeps its time"
        );
        dwell.present(at(30_000), false, false);
        assert_eq!(dwell.left(lifetime, at(40_000)), Duration::from_secs(3));
        dwell.present(at(40_000), true, false);
        assert_eq!(dwell.first_shown(), Some(at(1_000)));
        assert!(dwell.is_over(lifetime, at(43_000)));
        dwell.grant(lifetime, at(43_000), Duration::from_secs(1));
        assert_eq!(dwell.left(lifetime, at(43_000)), Duration::from_secs(1));
        assert!(dwell.is_over(lifetime, at(44_000)));
        dwell.spend(Duration::from_secs(9));
        assert!(dwell.is_over(lifetime, at(43_000)));
    }

    /// A stack that shows what it painted asks for no frame, so a pointer
    /// moving over a window with a notice up repaints nothing; a new or
    /// dismissed notice asks for one.
    #[test]
    fn a_painted_stack_needs_a_frame_only_when_its_notices_change() {
        let mut stack = ToastStack::new();
        lay_out(&mut stack);
        assert!(stack.needs_redraw(), "a stack never drawn needs a frame");
        stack.mark_drawn();
        assert!(
            !stack.needs_redraw(),
            "an empty stack it painted needs none"
        );
        stack.push("Saved", ToastKind::Success);
        assert!(stack.needs_redraw(), "a new notice needs a frame");
        stack.mark_drawn();
        assert!(!stack.needs_redraw(), "the notice it painted needs none");
        stack.dismiss(0);
        assert!(stack.needs_redraw(), "a dismissed notice needs a frame");
    }

    /// A notice's one button ("Undo") is reported to its owner once and
    /// takes the notice down; a press elsewhere on the card does not report
    /// it.
    #[test]
    fn a_notice_button_is_reported_once_and_takes_the_notice_down() {
        let mut stack = ToastStack::new();
        stack.push_with_action("Archived", ToastKind::Success, "Undo");
        lay_out(&mut stack);
        let mut scene = Scene::new();
        Widget::draw(
            &mut stack,
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let button = stack
            .action_rects
            .first()
            .copied()
            .flatten()
            .expect("the notice shows its button");
        let at = button.center();
        dispatch(
            &mut stack,
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (at.x, at.y).into(),
            },
        );
        assert!(
            stack.needs_redraw(),
            "the button under the pointer lights up"
        );
        dispatch(&mut stack, &crate::synthetic_input::mouse_press());
        assert_eq!(stack.take_activated().as_deref(), Some("Archived"));
        assert!(stack.is_empty(), "the notice is done with");
        assert_eq!(stack.take_activated(), None, "and reported once");
    }

    fn lay_out(stack: &mut ToastStack) {
        let mut tree = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = stack.build_layout(&mut tree, &theme, &mut nm);
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(f32_of_f64(TOAST_WIDTH)),
                height: taffy::AvailableSpace::Definite(1000.0),
            },
        )
        .unwrap();
        stack.apply_layout(&tree, node, Point::ZERO);
    }

    /// A card's one action sits beside its text while the text keeps room
    /// to read; large text in a narrow column puts it under the text.
    #[test]
    fn a_lone_action_moves_under_text_it_would_squeeze() {
        let mut tr = TextRenderer::new();
        let mut card = ToastCard::new("Launch checklist wasn't saved", ToastKind::Warning)
            .caption("Notes")
            .body("Your changes are still in Notes.")
            .closable(true);
        card.primary_action = "Try again";
        let paint = |tr: &mut TextRenderer, theme: &Theme, width: f64| {
            let rect = Rect::new(0.0, 0.0, width, card_height(tr, theme, &card, width));
            let hit = paint_card(&mut Scene::new(), Affine::IDENTITY, tr, theme, rect, &card);
            (rect, hit.primary_action_rect)
        };

        let (rect, beside) = paint(&mut tr, &Theme::dark(), 440.0);
        assert!(
            beside.y0 < rect.center().y && beside.y1 > rect.center().y,
            "{beside:?} sits beside the text in {rect:?}"
        );

        let large = Theme::light().with_text_scale(2.0);
        let (rect, under) = paint(&mut tr, &large, 320.0);
        assert!(
            under.y0 > rect.center().y && under.y1 <= rect.y1,
            "{under:?} ends the card {rect:?}"
        );
        assert!(rect.height() > minimum_card_height(&mut tr, &large, &card));
        // Every letter of the caption, title, body and button is painted:
        // an elided line would drop most of them.
        let mut scene = Scene::new();
        paint_card(&mut scene, Affine::IDENTITY, &mut tr, &large, rect, &card);
        let glyphs: usize = scene
            .encoding()
            .resources
            .glyph_runs
            .iter()
            .map(|run| run.glyphs.len())
            .sum();
        let letters = [card.caption, card.title, card.body, card.primary_action]
            .concat()
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();
        assert!(
            glyphs >= letters,
            "{glyphs} glyphs for {letters} letters: a line was elided"
        );
    }

    /// A card asking the person to choose ("Not now" / "Keep with passkey")
    /// grows to say its whole question: a body of three lines at the
    /// notification width is not elided to one.
    #[test]
    fn a_choice_card_wraps_its_whole_body() {
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let card = ToastCard::new("Keep your name and work?", ToastKind::Info)
            .caption("Identity")
            .body(
                "A passkey on this device brings you back as yourself, on any browser. \
                 Any time later: Ctrl+K, \u{201c}Keep your name and work\u{201d}.",
            )
            .closable(false)
            .actions(
                "Not now",
                crate::icons::Icon::X,
                false,
                "Keep with passkey",
                crate::icons::Icon::Check,
                false,
            );
        let width = 360.0;
        let rect = Rect::new(0.0, 0.0, width, card_height(&mut tr, &theme, &card, width));
        assert!(rect.height() > minimum_card_height(&mut tr, &theme, &card));
        let mut scene = Scene::new();
        let hit = paint_card(&mut scene, Affine::IDENTITY, &mut tr, &theme, rect, &card);
        assert!(hit.primary_action_rect.y1 <= rect.y1);
        let glyphs: usize = scene
            .encoding()
            .resources
            .glyph_runs
            .iter()
            .map(|run| run.glyphs.len())
            .sum();
        let letters = [
            card.caption,
            card.title,
            card.body,
            card.secondary_action,
            card.primary_action,
        ]
        .concat()
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
        assert!(
            glyphs >= letters,
            "{glyphs} glyphs for {letters} letters: the question was elided"
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
            dispatch(&mut s, &crate::synthetic_input::mouse_press());
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

    #[test]
    fn notices_with_a_button_go_once_what_they_act_on_moves_on() {
        let mut s = ToastStack::new();
        s.push_with_action("Deleted Groceries", ToastKind::Info, "Undo");
        s.push("Imported", ToastKind::Success);
        s.dismiss_actionable();
        assert_eq!(s.len(), 1, "only the notice without a button stays");
        assert!(s.iter().all(|toast| toast.action.is_none()));
    }
}
